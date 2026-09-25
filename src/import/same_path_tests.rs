//! Same-path evidence uses real owned preparation; only upstream receipt facts are seeded.
use super::*;

async fn fixture(
    dir: &Scratch,
    db: &Arc<Database>,
    movie: bool,
    mode: Mode,
    shared: bool,
) -> OwnedImport {
    let mut input = owned_cases::fixture(dir, db, movie, shared).await;
    let c = db.connect().await.unwrap();
    let old = dir.path(if movie {
        "movies/old.mkv"
    } else {
        "tv/old.mkv"
    });
    std::fs::rename(&old, &input.destination).unwrap();
    let table = if movie {
        "movie_files"
    } else {
        "episode_files"
    };
    c.execute(
        &format!("UPDATE {table} SET path=? WHERE id=1"),
        [input.destination.clone()],
    )
    .await
    .unwrap();
    if matches!(mode, Mode::Hardlink) {
        c.execute(
            "UPDATE download_processing_policies SET mode='hardlink',revision=revision+1",
            (),
        )
        .await
        .unwrap();
        c.execute("UPDATE download_processing SET status='blocked',error_code='import_conflict',reasons_json='[\"import_conflict\"]'", ()).await.unwrap();
        c.execute("UPDATE download_processing SET status='queued',policy_revision=2,preflight_attempts=0,error_code=NULL,reasons_json='[]'", ()).await.unwrap();
        c.execute("UPDATE download_processing SET status='checking',preflight_attempts=1,total_preflight_attempts=total_preflight_attempts+1", ()).await.unwrap();
        input.policy_revision = 2;
    }
    input.mode = mode;
    input
}

pub(super) async fn replacements() {
    interrupted_boundaries().await;
    hostile_destination().await;
    for movie in [false, true] {
        for mode in [Mode::Copy, Mode::Hardlink] {
            let dir = Scratch::new();
            let db = dir.database().await;
            let input = fixture(&dir, &db, movie, mode, false).await;
            let source = input.source.clone();
            let destination = input.destination.clone();
            let original = std::fs::metadata(&destination).unwrap();
            let op = prepare_owned(db.clone(), input)
                .await
                .unwrap()
                .id
                .to_string();
            let c = db.connect().await.unwrap();
            c.execute_batch("CREATE TRIGGER same_path_late_failure BEFORE INSERT ON import_history BEGIN SELECT RAISE(ABORT,'injected same-path commit failure'); END;").await.unwrap();
            assert!(execute(db.clone(), &op).await.is_err());
            // A failed DB commit must restore the pathname itself, not merely retain bytes elsewhere.
            assert_eq!(std::fs::read(&destination).unwrap(), b"original-media");
            let restored = std::fs::metadata(&destination).unwrap();
            assert_eq!(
                (restored.dev(), restored.ino()),
                (original.dev(), original.ino())
            );
            let target = if movie {
                MediaTarget::Movie(1)
            } else {
                MediaTarget::Episode(1)
            };
            assert_eq!(owned::ownership(&c, &target).await.unwrap().3, Some(1));
            assert_eq!(count(&db, "import_history").await, 0);
            c.execute_batch("DROP TRIGGER same_path_late_failure;")
                .await
                .unwrap();
            drop(c);
            drop(db);
            let db = dir.database().await;
            assert_eq!(execute(db.clone(), &op).await.unwrap().status, "complete");
            assert_eq!(execute(db.clone(), &op).await.unwrap().status, "complete");
            assert_eq!(count(&db, "import_history").await, 1);
            assert_eq!(std::fs::read(&source).unwrap(), b"new-media-content");
            assert_eq!(std::fs::read(&destination).unwrap(), b"new-media-content");
            let source_meta = std::fs::metadata(&source).unwrap();
            let final_meta = std::fs::metadata(&destination).unwrap();
            assert_eq!(
                (source_meta.dev(), source_meta.ino()) == (final_meta.dev(), final_meta.ino()),
                matches!(mode, Mode::Hardlink)
            );
            let c = db.connect().await.unwrap();
            let facts = owned::facts(&c, &op).await.unwrap().unwrap();
            assert_eq!(
                std::fs::read(facts.old.unwrap().recovery_path().unwrap()).unwrap(),
                b"original-media"
            );
            assert_eq!(facts.retirement_state, "quarantined");
            if movie {
                assert_eq!(owned::ownership(&c, &target).await.unwrap().3, Some(1));
            }
        }
    }
    let dir = Scratch::new();
    let db = dir.database().await;
    let input = fixture(&dir, &db, false, Mode::Copy, true).await;
    let destination = input.destination.clone();
    assert!(prepare_owned(db.clone(), input).await.is_err());
    assert_eq!(std::fs::read(destination).unwrap(), b"original-media");
    assert_eq!(count(&db, "rss_candidate_imports").await, 0);
    drop(db);
    let dir = Scratch::new();
    let db = dir.database().await;
    let mut input = fixture(&dir, &db, false, Mode::Hardlink, false).await;
    std::fs::remove_file(&input.source).unwrap();
    std::fs::hard_link(&input.destination, &input.source).unwrap();
    input.expected_size = 14;
    let destination = input.destination.clone();
    // Distinct path strings do not establish distinct old/new identities for exchange recovery.
    assert!(prepare_owned(db.clone(), input).await.is_err());
    assert_eq!(std::fs::read(destination).unwrap(), b"original-media");
    assert_eq!(count(&db, "rss_candidate_imports").await, 0);
}

async fn interrupted_boundaries() {
    for movie in [false, true] {
        for mode in [Mode::Copy, Mode::Hardlink] {
            for boundary in [
                "before_exchange",
                "after_exchange",
                "before_restore",
                "after_restore",
                "after_commit",
            ] {
                let dir = Scratch::new();
                let db = dir.database().await;
                let input = fixture(&dir, &db, movie, mode, false).await;
                let destination = input.destination.clone();
                let op = prepare_owned(db.clone(), input)
                    .await
                    .unwrap()
                    .id
                    .to_string();
                let c = db.connect().await.unwrap();
                let plan = load(&c, &op).await.unwrap().plan.unwrap();
                checkpoint(&c, &op, "staging", None).await.unwrap();
                let stage = plan.create_stage().unwrap();
                checkpoint(&c, &op, "staging", Some(&stage)).await.unwrap();
                let stage = plan.transfer(stage).unwrap();
                checkpoint(&c, &op, "staged", Some(&stage)).await.unwrap();
                let old = owned::facts(&c, &op).await.unwrap().unwrap().old.unwrap();
                c.execute(
                    "INSERT INTO same_path_replacements(operation_id) VALUES(?)",
                    [op.clone()],
                )
                .await
                .unwrap();
                if boundary != "before_exchange" {
                    plan.exchange(&stage, &old).unwrap();
                    assert_eq!(std::fs::read(&destination).unwrap(), b"new-media-content");
                }
                if matches!(
                    boundary,
                    "before_restore" | "after_restore" | "after_commit"
                ) {
                    c.execute(
                        "UPDATE same_path_replacements SET state='installed' WHERE operation_id=?",
                        [op.clone()],
                    )
                    .await
                    .unwrap();
                    checkpoint(&c, &op, "published", Some(&stage))
                        .await
                        .unwrap();
                }
                if matches!(boundary, "before_restore" | "after_restore") {
                    c.execute("UPDATE same_path_replacements SET state='restore_intent' WHERE operation_id=?", [op.clone()]).await.unwrap();
                    if boundary == "after_restore" {
                        plan.restore_exchange(&stage, &old).unwrap();
                        assert_eq!(std::fs::read(&destination).unwrap(), b"original-media");
                    }
                }
                let target = if movie {
                    MediaTarget::Movie(1)
                } else {
                    MediaTarget::Episode(1)
                };
                if boundary == "after_commit" {
                    commit(&c, &op, &target, &plan, &stage).await.unwrap();
                    assert_eq!(count(&db, "import_history").await, 1);
                } else {
                    assert_eq!(count(&db, "import_history").await, 0);
                    assert_eq!(owned::ownership(&c, &target).await.unwrap().3, Some(1));
                }
                // Stop after a real filesystem boundary without recording its next checkpoint.
                drop(c);
                drop(db);
                let db = dir.database().await;
                let reopened = db.connect().await.unwrap();
                let durable_state: String = reopened
                    .query(
                        "SELECT state FROM same_path_replacements WHERE operation_id=?",
                        [op.clone()],
                    )
                    .await
                    .unwrap()
                    .next()
                    .await
                    .unwrap()
                    .unwrap()
                    .get(0)
                    .unwrap();
                assert_eq!(
                    durable_state,
                    match boundary {
                        "before_exchange" | "after_exchange" => "exchange_intent",
                        "before_restore" | "after_restore" => "restore_intent",
                        _ => "installed",
                    }
                );
                assert_eq!(
                    std::fs::read(&destination).unwrap(),
                    if matches!(boundary, "before_exchange" | "after_restore") {
                        b"original-media".as_slice()
                    } else {
                        b"new-media-content".as_slice()
                    }
                );
                if boundary == "after_commit" {
                    let current = owned::ownership(&reopened, &target)
                        .await
                        .unwrap()
                        .3
                        .unwrap();
                    assert_eq!(
                        current == 1,
                        movie,
                        "TV gets a new file identity; movie retains its file identity"
                    );
                    let column = if movie {
                        "movie_file_id"
                    } else {
                        "episode_file_id"
                    };
                    let quality: i64 = reopened
                        .query(
                            &format!("SELECT quality_id FROM file_metadata WHERE {column}=?"),
                            [current],
                        )
                        .await
                        .unwrap()
                        .next()
                        .await
                        .unwrap()
                        .unwrap()
                        .get(0)
                        .unwrap();
                    assert_eq!(quality, 3);
                    let hash: String = reopened
                        .query(
                            "SELECT sha256 FROM import_history WHERE operation_id=?",
                            [op.clone()],
                        )
                        .await
                        .unwrap()
                        .next()
                        .await
                        .unwrap()
                        .unwrap()
                        .get(0)
                        .unwrap();
                    let expected =
                        ring::digest::digest(&ring::digest::SHA256, b"new-media-content")
                            .as_ref()
                            .iter()
                            .map(|b| format!("{b:02x}"))
                            .collect::<String>();
                    assert_eq!(hash, expected);
                    if !movie {
                        let archived: String = reopened
                            .query("SELECT path FROM episode_files WHERE id=1", ())
                            .await
                            .unwrap()
                            .next()
                            .await
                            .unwrap()
                            .unwrap()
                            .get(0)
                            .unwrap();
                        assert_eq!(archived, old.recovery_path().unwrap());
                        let active: i64 = reopened
                            .query(
                                &format!(
                                    "SELECT count(*) FROM episode_files f WHERE f.id=1 AND {}",
                                    crate::media_files::active_file_sql("tv")
                                ),
                                (),
                            )
                            .await
                            .unwrap()
                            .next()
                            .await
                            .unwrap()
                            .unwrap()
                            .get(0)
                            .unwrap();
                        assert_eq!(
                            active, 0,
                            "committed original must already be absent from active file APIs before physical retirement"
                        );
                        assert!(
                            !std::path::Path::new(&archived).exists(),
                            "DB archive path precedes physical retirement; old bytes still reside in owned staging"
                        );
                    }
                } else {
                    assert_eq!(
                        owned::ownership(&reopened, &target).await.unwrap().3,
                        Some(1)
                    );
                }
                drop(reopened);
                assert_eq!(
                    execute(db.clone(), &op).await.unwrap().status,
                    "complete",
                    "{movie} {mode:?} {boundary}"
                );
                assert_eq!(std::fs::read(&destination).unwrap(), b"new-media-content");
                assert_eq!(
                    std::fs::read(old.recovery_path().unwrap()).unwrap(),
                    b"original-media"
                );
                assert_eq!(count(&db, "import_history").await, 1);
            }
        }
    }
}

async fn hostile_destination() {
    for movie in [false, true] {
        for mutation in ["file", "symlink", "parent", "stage"] {
            let dir = Scratch::new();
            let db = dir.database().await;
            let input = fixture(&dir, &db, movie, Mode::Copy, false).await;
            let destination = input.destination.clone();
            let op = prepare_owned(db.clone(), input)
                .await
                .unwrap()
                .id
                .to_string();
            let c = db.connect().await.unwrap();
            let plan = load(&c, &op).await.unwrap().plan.unwrap();
            checkpoint(&c, &op, "staging", None).await.unwrap();
            let stage = plan.create_stage().unwrap();
            checkpoint(&c, &op, "staging", Some(&stage)).await.unwrap();
            let stage = plan.transfer(stage).unwrap();
            checkpoint(&c, &op, "staged", Some(&stage)).await.unwrap();
            let old = owned::facts(&c, &op).await.unwrap().unwrap().old.unwrap();
            c.execute(
                "INSERT INTO same_path_replacements(operation_id)VALUES(?)",
                [op.clone()],
            )
            .await
            .unwrap();
            plan.exchange(&stage, &old).unwrap();
            let plan_json = serde_json::to_value(&plan).unwrap();
            let parent = std::path::Path::new(&destination).parent().unwrap();
            let stage_path = parent.join(plan_json["stage_name"].as_str().unwrap());
            let original_path = stage_path.join("original");
            let displaced = dir.path("displaced-new");
            let original_witness;
            match mutation {
                "parent" => {
                    let moved = dir.0.join("moved-parent");
                    std::fs::rename(parent, &moved).unwrap();
                    std::fs::create_dir(parent).unwrap();
                    std::fs::write(&destination, b"unrelated-inode").unwrap();
                    original_witness = moved
                        .join(plan_json["stage_name"].as_str().unwrap())
                        .join("original");
                }
                "stage" => {
                    let moved = dir.0.join("moved-stage");
                    std::fs::rename(&stage_path, &moved).unwrap();
                    std::fs::create_dir(&stage_path).unwrap();
                    original_witness = moved.join("original");
                }
                _ => {
                    std::fs::rename(&destination, &displaced).unwrap();
                    if mutation == "symlink" {
                        let unrelated = dir.path("unrelated");
                        std::fs::write(&unrelated, b"unrelated-inode").unwrap();
                        std::os::unix::fs::symlink(&unrelated, &destination).unwrap();
                    } else {
                        std::fs::write(&destination, b"unrelated-inode").unwrap();
                    }
                    original_witness = original_path;
                }
            }
            assert!(plan.restore_exchange(&stage, &old).is_err(), "{mutation}");
            assert!(plan.exchange(&stage, &old).is_err(), "{mutation}");
            assert_eq!(
                std::fs::read(&destination).unwrap(),
                if mutation == "stage" {
                    b"new-media-content".as_slice()
                } else {
                    b"unrelated-inode".as_slice()
                }
            );
            assert_eq!(std::fs::read(original_witness).unwrap(), b"original-media");
            // No ambiguous filesystem orientation may become a committed library/history fact.
            assert_eq!(count(&db, "import_history").await, 0);
        }
    }
}
