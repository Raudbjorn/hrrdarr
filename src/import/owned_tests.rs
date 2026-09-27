use super::*;
use crate::import::{OwnedImport, prepare_owned};

pub(super) async fn fixture(
    dir: &Scratch,
    db: &Arc<Database>,
    movie: bool,
    shared: bool,
) -> OwnedImport {
    dir.setup(db).await;
    let c = db.connect().await.unwrap();
    // Any (-1) keeps this fixture language-unrestricted; Original (-2) requires audio matching.
    c.execute_batch("UPDATE episodes SET runtime=45; UPDATE movie_metadata SET year=2020,runtime=100; INSERT INTO quality_profiles VALUES(1,'tv','HD'),(2,'movies','HD'); INSERT INTO quality_profile_items(profile_id,media_type,quality_id,position,allowed)VALUES(1,'tv',1,0,1),(1,'tv',3,1,1),(2,'movies',1,0,1),(2,'movies',3,1,1); INSERT INTO quality_profile_policies(profile_id,media_type,upgrade_allowed,cutoff_quality_id,min_format_score,cutoff_format_score,min_upgrade_format_score,language_id)VALUES(1,'tv',1,3,0,0,1,NULL),(2,'movies',1,3,0,0,1,-1); INSERT INTO library_settings(media_type,series_id,quality_profile_id,series_type,use_scene_numbering)VALUES('tv',1,1,'standard',0); INSERT INTO library_settings(media_type,movie_id,quality_profile_id,minimum_availability)VALUES('movies',1,2,'released'); UPDATE quality_definitions SET min_size=0,max_size=NULL WHERE quality_id=3;").await.unwrap();
    let domain = if movie { "movies" } else { "tv" };
    let basename = if movie {
        "Movie.2020.1080p.WEB-DL.mkv"
    } else {
        "TV.S01E01.1080p.WEB-DL.mkv"
    };
    let source = dir.path(&format!("downloads/{basename}"));
    let destination = dir.path(&format!("{domain}/{basename}"));
    std::fs::write(&source, b"new-media-content").unwrap();
    let oldpath = dir.path(&format!("{domain}/old.mkv"));
    std::fs::write(&oldpath, b"original-media").unwrap();
    if movie {
        c.execute(
            "INSERT INTO movie_files(id,movie_id,path,edition)VALUES(1,1,?,'Old edition')",
            [oldpath],
        )
        .await
        .unwrap();
        c.execute("INSERT INTO file_metadata(media_type,movie_file_id,quality_id,size)VALUES('movies',1,1,14)",()).await.unwrap();
    } else {
        c.execute(
            "INSERT INTO episode_files(id,series_id,path)VALUES(1,1,?)",
            [oldpath],
        )
        .await
        .unwrap();
        c.execute("UPDATE episodes SET episode_file_id=1 WHERE id=1", ())
            .await
            .unwrap();
        c.execute("INSERT INTO file_metadata(media_type,episode_file_id,quality_id,size)VALUES('tv',1,1,14)",()).await.unwrap();
        if shared {
            c.execute("INSERT INTO episodes(id,series_id,season,number,title,episode_file_id)VALUES(2,1,1,2,'Second',1)",()).await.unwrap();
        }
        c.execute(
            "INSERT INTO episode_files(id,series_id,path)VALUES(99,1,?)",
            [dir.path("tv/unassociated.mkv")],
        )
        .await
        .unwrap();
    }
    let client = Uuid::new_v4().to_string();
    let indexer = Uuid::new_v4().to_string();
    let command = Uuid::new_v4().to_string();
    let candidate = Uuid::new_v4().to_string();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint)VALUES(?,'qbittorrent','Client',1,1,1,1,'http://fixture.invalid')",[client.clone()]).await.unwrap();
    c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags)VALUES(?,'qbittorrent',?,?,0,0,'started','default',0,0,0)",params![client.clone(),domain,domain]).await.unwrap();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint)VALUES(?,'torznab','Indexer',1,1,1,1,'http://fixture.invalid')",[indexer.clone()]).await.unwrap();
    c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year)VALUES(?,'torznab',?,'[5000]','[]',?,?)",params![indexer.clone(),domain,(!movie).then_some(0),movie.then_some(0)]).await.unwrap();
    c.execute("INSERT INTO rss_commands(id,name,media_type,indexer_id,indexer_revision,client_id,client_revision,next_attempt_at,created_at)VALUES(?,'rss_sync',?,?,1,?,1,100,100)",params![command.clone(),domain,indexer.clone(),client.clone()]).await.unwrap();
    c.execute("INSERT INTO rss_candidates(id,command_id,media_type,indexer_id,indexer_revision,client_id,client_revision,fingerprint,title,private_payload,series_id,movie_id,status,decision_reasons_json,created_at,updated_at)VALUES(?,?,?,?,1,?,1,?,'Release',?,?,?,'pending','[]',100,100)",params![candidate.clone(),command,domain,indexer,client.clone(),"f".repeat(64),vec![1u8;29],(!movie).then_some(1),movie.then_some(1)]).await.unwrap();
    if !movie {
        c.execute(
            "INSERT INTO rss_candidate_episodes VALUES(?,1,1)",
            [candidate.clone()],
        )
        .await
        .unwrap();
    }
    let identity = serde_json::json!({"version":1,"target":{"media_type":if movie{"movie"}else{"episode"},"id":1},"hashes":["a".repeat(40)],"settings_fingerprint":"b".repeat(64),"payload_sha256":"c".repeat(64)});
    c.execute(
        "UPDATE rss_candidates SET status='prepared',submission_identity_json=? WHERE id=?",
        params![identity.to_string(), candidate.clone()],
    )
    .await
    .unwrap();
    c.execute(
        "INSERT INTO rss_hash_claims VALUES(?,?,?)",
        params![client.clone(), "a".repeat(40), candidate.clone()],
    )
    .await
    .unwrap();
    // Freeze actual provider facts at submission; renamed filenames cannot reconstruct these flags.
    let mut evidence = crate::custom_formats::parsed(
        basename.trim_end_matches(".mkv"),
        &crate::search::parser::parse(basename.trim_end_matches(".mkv"), !movie).unwrap(),
        !movie,
        Some(17),
    );
    evidence.languages = Some(vec![1]);
    evidence.indexer_flags = Some(1);
    evidence.release_group = Some("GROUP".into());
    c.execute(
        "UPDATE rss_candidates SET status='submitting',private_payload=NULL,comparison_facts_json=? WHERE id=?",
        params![serde_json::to_string(&evidence).unwrap(),candidate.clone()],
    )
    .await
    .unwrap();
    c.execute(
        "UPDATE rss_candidates SET status='observed',observed_hash=? WHERE id=?",
        params!["a".repeat(40), candidate.clone()],
    )
    .await
    .unwrap();
    c.execute(
        "INSERT INTO download_processing_policies VALUES(?,?,1,1,1,'copy')",
        params![client, domain],
    )
    .await
    .unwrap();
    c.execute("INSERT INTO download_processing(candidate_id,policy_revision,status,next_attempt_at,created_at,updated_at)VALUES(?,1,'queued',100,100,100)",[candidate.clone()]).await.unwrap();
    c.execute("UPDATE download_processing SET status='checking',preflight_attempts=1,total_preflight_attempts=1 WHERE candidate_id=?",[candidate.clone()]).await.unwrap();
    c.execute("INSERT INTO remote_path_mappings(media_type,host,remote_path,remote_kind,remote_key,local_path)VALUES(?,'fixture.invalid','/downloads','posix','/downloads',?)",params![domain,dir.path("downloads")]).await.unwrap();
    OwnedImport {
        candidate_id: candidate,
        target: if movie {
            MediaTarget::Movie(1)
        } else {
            MediaTarget::Episode(1)
        },
        source,
        destination,
        mode: Mode::Copy,
        expected_size: 17,
        quality_id: 3,
        revision_json: "{\"version\":1,\"real\":0,\"is_repack\":false}".into(),
        edition: None,
        policy_revision: 1,
        mapping_id: c.last_insert_rowid(),
        mapping_revision: 1,
        host: "fixture.invalid".into(),
    }
}

pub(super) async fn replacements() {
    custom_format_naming_changes_reject_before_journaling().await;
    // Keep execute() regressions within the shared serial recovery test: its permit is global.
    changed_original_title_or_format_policy_prevents_owned_replacement().await;
    legacy_owned_journals_revalidate_current_policy_before_recovery().await;
    // Real owned producer and journal transitions, not fabricated replacement sidecars.
    for (movie, shared) in [(false, false), (true, false), (false, true)] {
        let dir = Scratch::new();
        let db = dir.database().await;
        let input = fixture(&dir, &db, movie, shared).await;
        let candidate = input.candidate_id.clone();
        let source = input.source.clone();
        let destination = input.destination.clone();
        let repeat = OwnedImport {
            candidate_id: input.candidate_id.clone(),
            target: if movie {
                MediaTarget::Movie(1)
            } else {
                MediaTarget::Episode(1)
            },
            source: input.source.clone(),
            destination: input.destination.clone(),
            mode: input.mode,
            expected_size: input.expected_size,
            quality_id: input.quality_id,
            revision_json: input.revision_json.clone(),
            edition: None,
            policy_revision: input.policy_revision,
            mapping_id: input.mapping_id,
            mapping_revision: input.mapping_revision,
            host: input.host.clone(),
        };
        let op = prepare_owned(db.clone(), input)
            .await
            .unwrap()
            .id
            .to_string();
        assert_eq!(
            prepare_owned(db.clone(), repeat)
                .await
                .unwrap()
                .id
                .to_string(),
            op
        );
        let c = db.connect().await.unwrap();
        let (plan, stage) = stage_and_publish(&db, &op, true).await;
        // DB failure after successful publication preserves original associations and bytes.
        c.execute_batch("CREATE TRIGGER injected_owned_history BEFORE INSERT ON import_history BEGIN SELECT RAISE(ABORT,'injected failure');END;").await.unwrap();
        let target = if movie {
            MediaTarget::Movie(1)
        } else {
            MediaTarget::Episode(1)
        };
        assert!(commit(&c, &op, &target, &plan, &stage).await.is_err());
        let oldpath = dir.path(if movie {
            "movies/old.mkv"
        } else {
            "tv/old.mkv"
        });
        assert_eq!(std::fs::read(&oldpath).unwrap(), b"original-media");
        assert_eq!(owned::ownership(&c, &target).await.unwrap().3, Some(1));
        assert_eq!(count(&db, "import_history").await, 0);
        c.execute_batch("DROP TRIGGER injected_owned_history;")
            .await
            .unwrap();
        drop(c);
        drop(db);
        let db = dir.database().await;
        let c = db.connect().await.unwrap();
        commit(&c, &op, &target, &plan, &stage).await.unwrap();
        let facts = owned::facts(&c, &op).await.unwrap().unwrap();
        let old = facts.old.unwrap();
        if !shared {
            let checkpoint = old.prepare_retirement().unwrap();
            c.execute(
                "UPDATE rss_candidate_imports SET retirement_json=? WHERE operation_id=?",
                params![json(&checkpoint).unwrap(), op.clone()],
            )
            .await
            .unwrap();
            old.retire(&checkpoint).unwrap();
            // Interrupt before checkpoint, then create unrelated bytes at original pathname.
            std::fs::write(&oldpath, b"reappeared-unrelated").unwrap();
        }
        drop(c);
        drop(db);
        let db = dir.database().await;
        if shared {
            let held = acquire(&db, "other-operation").unwrap();
            assert_eq!(
                start_owned(db.clone(), &op).await.unwrap(),
                StartOwned::Busy
            );
            drop(held);
            assert_eq!(
                start_owned(db.clone(), &op).await.unwrap(),
                StartOwned::Started
            );
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    let current = status(db.clone(), &op).await.unwrap();
                    assert!(current.error_code.is_none());
                    if current.status == "complete" {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            while EXECUTING.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        } else {
            assert_eq!(execute(db.clone(), &op).await.unwrap().status, "complete");
        }
        assert_eq!(std::fs::read(&source).unwrap(), b"new-media-content");
        assert_eq!(std::fs::read(&destination).unwrap(), b"new-media-content");
        assert_eq!(
            std::fs::read(&oldpath).unwrap(),
            if shared {
                b"original-media".as_slice()
            } else {
                b"reappeared-unrelated".as_slice()
            }
        );
        if !shared {
            assert_eq!(
                std::fs::read(old.recovery_path().unwrap()).unwrap(),
                b"original-media"
            );
        }
        let c = db.connect().await.unwrap();
        let state: String = c
            .query(
                "SELECT retirement_state FROM rss_candidate_imports WHERE candidate_id=?",
                [candidate],
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
            state,
            if shared {
                "shared_retained"
            } else {
                "quarantined"
            }
        );
        assert_eq!(count(&db, "import_history").await, 1);
        assert_eq!(execute(db.clone(), &op).await.unwrap().status, "complete");
        assert_eq!(count(&db, "import_history").await, 1);
        let table = if movie {
            "movie_file_id"
        } else {
            "episode_file_id"
        };
        let fid = owned::ownership(&c, &target).await.unwrap().3.unwrap();
        let quality: i64 = c
            .query(
                &format!("SELECT quality_id FROM file_metadata WHERE {table}=?"),
                [fid],
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
        // Import/restart retains the immutable receipt facts alongside the new quality.
        let factual=c.query(&format!("SELECT original_release_title,languages_json,indexer_flags FROM file_metadata WHERE {table}=?"),[fid]).await.unwrap().next().await.unwrap().unwrap();
        assert!(factual.get::<String>(0).unwrap().contains("1080p.WEB-DL"));
        assert_eq!(factual.get::<String>(1).unwrap(), "[1]");
        assert_eq!(factual.get::<i64>(2).unwrap(), 1);

        if movie {
            assert_eq!(fid, 1);
        } else {
            // Mounted file API must hide only explicitly retired TV records.
            let app =
                crate::media_files::router(db.clone()).merge(crate::library::router(db.clone()));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let http = reqwest::Client::new();
            let oldurl = format!("http://{address}/api/v1/tv/files/1");
            assert_eq!(
                http.get(&oldurl).send().await.unwrap().status().as_u16(),
                if shared { 200 } else { 404 }
            );
            assert_eq!(
                http.get(format!("http://{address}/api/v1/tv/files/99"))
                    .send()
                    .await
                    .unwrap()
                    .status()
                    .as_u16(),
                200
            );
            let bytes = http
                .get(format!("http://{address}/api/v1/tv/files?series_id=1"))
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["total"], if shared { 3 } else { 2 });
            if !shared {
                assert_eq!(
                    http.put(&oldurl)
                        .header("content-type", "application/json")
                        .body("{\"quality\":{\"quality_id\":3}}")
                        .send()
                        .await
                        .unwrap()
                        .status()
                        .as_u16(),
                    404
                );
            }
            let bytes = http
                .get(format!("http://{address}/api/v1/tv/series/1"))
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap();
            let series: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                series["statistics"]["file_count"],
                if shared { 3 } else { 2 }
            );
            server.abort();
            let _ = server.await;
        }
        drop(c);
        drop(db);
    }
    let dir = Scratch::new();
    let db = dir.database().await;
    let input = fixture(&dir, &db, true, false).await;
    let op = prepare_owned(db.clone(), input)
        .await
        .unwrap()
        .id
        .to_string();
    let (plan, stage) = stage_and_publish(&db, &op, true).await;
    let c = db.connect().await.unwrap();
    commit(&c, &op, &MediaTarget::Movie(1), &plan, &stage)
        .await
        .unwrap();
    std::fs::write(dir.path("movies/old.mkv"), b"changed-original-preserved").unwrap();
    assert_eq!(
        execute(db.clone(), &op).await.unwrap_err().code(),
        "identity_changed"
    );
    assert_eq!(
        start_owned(db.clone(), &op).await.unwrap_err().code(),
        "resume_required"
    );
    c.execute("UPDATE download_processing SET resume_requested=1 WHERE candidate_id=(SELECT candidate_id FROM rss_candidate_imports WHERE operation_id=?)",[op.clone()]).await.unwrap();
    let held = acquire(&db, "other-operation").unwrap();
    assert_eq!(
        start_owned(db.clone(), &op).await.unwrap(),
        StartOwned::Busy
    );
    drop(held);
    c.execute_batch("CREATE TRIGGER injected_start_commit BEFORE UPDATE ON download_processing WHEN NEW.resume_requested=0 BEGIN SELECT RAISE(ABORT,'injected start checkpoint failure');END;").await.unwrap();
    assert!(start_owned(db.clone(), &op).await.is_err());
    assert!(!EXECUTING.load(Ordering::Acquire));
    let flag: i64 = c
        .query("SELECT resume_requested FROM download_processing", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(flag, 1);
    c.execute_batch("DROP TRIGGER injected_start_commit;")
        .await
        .unwrap();
    assert_eq!(
        start_owned(db.clone(), &op).await.unwrap(),
        StartOwned::Started
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while EXECUTING.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        start_owned(db.clone(), &op).await.unwrap_err().code(),
        "resume_required"
    );
    assert_eq!(
        std::fs::read(dir.path("movies/old.mkv")).unwrap(),
        b"changed-original-preserved"
    );
    assert_eq!(status(db.clone(), &op).await.unwrap().status, "committed");
    assert_eq!(count(&db, "import_history").await, 1);
    // A failed error-status write must not grant the worker unlimited automatic retries.
    c.execute_batch("CREATE TRIGGER injected_error_persist BEFORE UPDATE OF error_code ON import_journal WHEN NEW.error_code IS NOT NULL BEGIN SELECT RAISE(ABORT,'owned failure persistence unavailable');END;").await.unwrap();
    c.execute("UPDATE download_processing SET resume_requested=1", ())
        .await
        .unwrap();
    assert_eq!(
        execute(db.clone(), &op).await.unwrap_err().code(),
        "identity_changed"
    );
    assert!(status(db.clone(), &op).await.unwrap().error_code.is_none());
    let flag: i64 = c
        .query("SELECT resume_requested FROM download_processing", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(
        flag, 0,
        "Manual owned execution consumes pending retry authority too"
    );
    assert_eq!(
        start_owned(db.clone(), &op).await.unwrap_err().code(),
        "resume_required"
    );
    c.execute_batch("DROP TRIGGER injected_error_persist;")
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(dir.path("movies/old.mkv")).unwrap(),
        b"changed-original-preserved"
    );
    drop(c);
    drop(db);
    let dir = Scratch::new();
    let db = dir.database().await;
    let input = fixture(&dir, &db, true, false).await;
    let op = prepare_owned(db.clone(), input)
        .await
        .unwrap()
        .id
        .to_string();
    let (plan, stage) = stage_and_publish(&db, &op, true).await;
    let c = db.connect().await.unwrap();
    commit(&c, &op, &MediaTarget::Movie(1), &plan, &stage)
        .await
        .unwrap();
    c.execute_batch("CREATE TRIGGER injected_retirement_checkpoint BEFORE UPDATE OF retirement_json ON rss_candidate_imports BEGIN SELECT RAISE(ABORT,'retirement checkpoint unavailable');END;").await.unwrap();
    assert!(execute(db.clone(), &op).await.is_err());
    assert_eq!(
        std::fs::read(dir.path("movies/old.mkv")).unwrap(),
        b"original-media"
    );
    c.execute_batch("DROP TRIGGER injected_retirement_checkpoint;")
        .await
        .unwrap();
    assert_eq!(
        execute(db.clone(), &op).await.unwrap_err().code(),
        "unowned_artifact"
    );
    assert_eq!(
        std::fs::read(dir.path("movies/old.mkv")).unwrap(),
        b"original-media"
    );
}

async fn changed_original_title_or_format_policy_prevents_owned_replacement() {
    for movie in [false, true] {
        let dir = Scratch::new();
        let db = dir.database().await;
        let input = fixture(&dir, &db, movie, false).await;
        let oldpath = dir.path(if movie {
            "movies/old.mkv"
        } else {
            "tv/old.mkv"
        });
        let operation = prepare_owned(db.clone(), input)
            .await
            .unwrap()
            .id
            .to_string();
        let c = db.connect().await.unwrap();
        let domain = if movie { "movies" } else { "tv" };
        // Original title is part of the replacement identity snapshot, not decorative metadata.
        c.execute("UPDATE file_metadata SET original_release_title='changed-after-preview' WHERE media_type=?",[domain]).await.unwrap();
        // Require the identity rejection; an unrelated import_busy error proves nothing here.
        assert_eq!(
            execute(db.clone(), &operation).await.unwrap_err().code(),
            "target_changed"
        );
        assert_eq!(std::fs::read(&oldpath).unwrap(), b"original-media");
        assert_eq!(count(&db, "import_history").await, 0);
    }
    for movie in [false, true] {
        let dir = Scratch::new();
        let db = dir.database().await;
        let input = fixture(&dir, &db, movie, false).await;
        let oldpath = dir.path(if movie {
            "movies/old.mkv"
        } else {
            "tv/old.mkv"
        });
        let operation = prepare_owned(db.clone(), input)
            .await
            .unwrap()
            .id
            .to_string();
        let c = db.connect().await.unwrap();
        // Frozen facts never freeze policy: a changed minimum is checked before placement.
        c.execute(
            "UPDATE quality_profile_policies SET min_format_score=1 WHERE media_type=?",
            [if movie { "movies" } else { "tv" }],
        )
        .await
        .unwrap();
        // Require policy rejection rather than any error from the execution boundary.
        assert_eq!(
            execute(db.clone(), &operation).await.unwrap_err().code(),
            "preflight_changed"
        );
        assert_eq!(std::fs::read(&oldpath).unwrap(), b"original-media");
        assert_eq!(count(&db, "import_history").await, 0);
    }
}

async fn legacy_owned_journals_revalidate_current_policy_before_recovery() {
    for movie in [false, true] {
        for published in [false, true] {
            for blocked in [false, true] {
                let dir = Scratch::new();
                let db = dir.database().await;
                let input = fixture(&dir, &db, movie, false).await;
                let candidate = input.candidate_id.clone();
                let destination = input.destination.clone();
                let operation = prepare_owned(db.clone(), input)
                    .await
                    .unwrap()
                    .id
                    .to_string();
                if published {
                    stage_and_publish(&db, &operation, true).await;
                }
                let c = db.connect().await.unwrap();
                legacy_provenance(&c, &operation, &candidate, false).await;
                assert!(
                    owned::facts(&c, &operation)
                        .await
                        .unwrap()
                        .unwrap()
                        .evidence
                        .is_none()
                );
                // A reachable native policy, whose required provider flag is unavailable
                // in the old receipt. This also exercises prewarm/cached-only commit scoring.
                let media = if movie { "movies" } else { "tv" };
                let specs = serde_json::json!([{"name":"Freeleech","required":false,"negate":false,"condition":{"kind":"indexer_flag","value":1}}]).to_string();
                c.execute("INSERT INTO custom_formats(media_type,name,include_when_renaming,specifications_json)VALUES(?,'Legacy flag',0,?)",params![media,specs]).await.unwrap();
                c.execute("INSERT INTO quality_profile_format_scores(profile_id,format_id,media_type,score)VALUES(?,?,?,10)",params![if movie {2}else{1},c.last_insert_rowid(),media]).await.unwrap();
                if blocked {
                    c.execute(
                        "UPDATE quality_profile_policies SET min_format_score=1 WHERE media_type=?",
                        [if movie { "movies" } else { "tv" }],
                    )
                    .await
                    .unwrap();
                }
                drop(c);
                drop(db);
                let db = dir.database().await;
                if published && !blocked {
                    let c = db.connect().await.unwrap();
                    let record = load(&c, &operation).await.unwrap();
                    let plan = record.plan.unwrap();
                    let stage = record.stage.unwrap();
                    owned::before(&c, &operation, &record.target).await.unwrap();
                    commit(&c, &operation, &record.target, &plan, &stage)
                        .await
                        .unwrap();
                    // The committed association is authoritative: pending retirement must
                    // finish even when policy becomes restrictive after the commit.
                    c.execute(
                        "UPDATE quality_profile_policies SET min_format_score=1 WHERE media_type=?",
                        [if movie { "movies" } else { "tv" }],
                    )
                    .await
                    .unwrap();
                }
                let outcome = execute(db.clone(), &operation).await;
                let old = dir.path(if movie {
                    "movies/old.mkv"
                } else {
                    "tv/old.mkv"
                });
                if blocked {
                    assert_eq!(outcome.unwrap_err().code(), "preflight_changed");
                    assert_eq!(std::fs::read(&old).unwrap(), b"original-media");
                    assert_eq!(count(&db, "import_history").await, 0);
                    if !published {
                        assert!(!std::path::Path::new(&destination).exists());
                    }
                } else {
                    assert_eq!(outcome.unwrap().status, "complete");
                    assert_eq!(std::fs::read(&destination).unwrap(), b"new-media-content");
                    assert_eq!(count(&db, "import_history").await, 1);
                    let c = db.connect().await.unwrap();
                    let sql = if movie {
                        "SELECT original_release_title,languages_json,indexer_flags FROM file_metadata WHERE movie_file_id=(SELECT id FROM movie_files WHERE movie_id=1)"
                    } else {
                        "SELECT original_release_title,languages_json,indexer_flags FROM file_metadata WHERE episode_file_id=(SELECT episode_file_id FROM episodes WHERE id=1)"
                    };
                    let row = c
                        .query(sql, ())
                        .await
                        .unwrap()
                        .next()
                        .await
                        .unwrap()
                        .unwrap();
                    assert_eq!(row.get::<Option<String>>(0).unwrap(), None);
                    assert_eq!(row.get::<Option<String>>(1).unwrap(), None);
                    assert_eq!(row.get::<Option<i64>>(2).unwrap(), None);
                }
            }
        }
        let dir = Scratch::new();
        let db = dir.database().await;
        let input = fixture(&dir, &db, movie, false).await;
        let candidate = input.candidate_id.clone();
        let operation = prepare_owned(db.clone(), input)
            .await
            .unwrap()
            .id
            .to_string();
        let c = db.connect().await.unwrap();
        legacy_provenance(&c, &operation, &candidate, true).await;
        assert!(
            execute(db.clone(), &operation).await.is_err(),
            "Invalid immutable expected size must fail closed"
        );
        assert_eq!(count(&db, "import_history").await, 0);
        assert_eq!(
            std::fs::read(dir.path(if movie {
                "movies/old.mkv"
            } else {
                "tv/old.mkv"
            }))
            .unwrap(),
            b"original-media"
        );
    }
}

async fn legacy_provenance(c: &Connection, operation: &str, candidate: &str, invalid_size: bool) {
    // Fixture-only historical shape: old journals had expected_size but no comparison facts.
    // Temporarily relax immutable guards in this scratch transaction, restore the exact
    // trigger definitions, and only then invoke application recovery. No migration changes.
    let tx = c.transaction().await.unwrap();
    let mut triggers = Vec::new();
    for name in [
        "candidate_import_immutable",
        "rss_comparison_facts_update",
        "rss_candidate_transition",
    ] {
        let sql: String = tx
            .query(
                "SELECT sql FROM sqlite_master WHERE type='trigger' AND name=?",
                [name],
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        tx.execute_batch(&format!("DROP TRIGGER {name}"))
            .await
            .unwrap();
        triggers.push(sql);
    }
    tx.execute("UPDATE rss_candidate_imports SET provenance_json=json_remove(provenance_json,'$.comparison_facts') WHERE operation_id=?",[operation]).await.unwrap();
    if invalid_size {
        tx.execute("UPDATE rss_candidate_imports SET provenance_json=json_set(provenance_json,'$.expected_size',0) WHERE operation_id=?",[operation]).await.unwrap();
    }
    tx.execute(
        "UPDATE rss_candidates SET comparison_facts_json=NULL WHERE id=?",
        [candidate],
    )
    .await
    .unwrap();
    for sql in triggers {
        tx.execute_batch(&sql).await.unwrap();
    }
    tx.commit().await.unwrap();
    assert!(
        c.execute(
            "UPDATE rss_candidate_imports SET provenance_json='{}' WHERE operation_id=?",
            [operation]
        )
        .await
        .is_err(),
        "Historical fixture must restore immutability"
    );
}

// Kept in the existing serial import test, which owns the process-wide execution permit.
async fn custom_format_naming_changes_reject_before_journaling() {
    for movie in [false, true] {
        for change in ["name", "include", "definition", "illegal"] {
            let dir = Scratch::new();
            let db = dir.database().await;
            let mut input = fixture(&dir, &db, movie, false).await;
            let c = db.connect().await.unwrap();
            let media = if movie { "movies" } else { "tv" };
            let column = if movie {
                "standard_movie_format"
            } else {
                "standard_episode_format"
            };
            c.execute(&format!("UPDATE naming_settings SET revision=revision+1,rename_enabled=1,replace_illegal_characters=0,{column}='CF-{{Custom Formats}}' WHERE domain=?"), [media]).await.unwrap();
            let specs = serde_json::json!([{"name":"Title","negate":false,"required":false,"condition":{"kind":"release_title","pattern":"WEB-DL"}}]).to_string();
            c.execute("INSERT INTO custom_formats(media_type,name,include_when_renaming,specifications_json)VALUES(?,'Before',1,?)",params![media,specs]).await.unwrap();
            let accepted = crate::search::downloaded::evaluate_receipt(
                &c,
                &input.target,
                &input.source,
                input.expected_size,
                &input.candidate_id,
            )
            .await
            .unwrap()
            .accepted
            .unwrap();
            input.destination = crate::naming::destination::resolve_owned_destination(
                &c,
                &input.target,
                &accepted.root,
                &accepted.basename,
                accepted.quality_id,
                accepted.edition.as_deref(),
                &accepted.evidence,
            )
            .await
            .unwrap();
            assert!(input.destination.ends_with("CF-Before.mkv"));
            match change {
                "name" => {
                    c.execute("UPDATE custom_formats SET name='After'", ())
                        .await
                        .unwrap();
                }
                "include" => {
                    c.execute("UPDATE custom_formats SET include_when_renaming=0", ())
                        .await
                        .unwrap();
                }
                "definition" => {
                    c.execute("UPDATE custom_formats SET specifications_json=replace(specifications_json,'WEB-DL','impossible')",()).await.unwrap();
                }
                _ => {
                    c.execute("UPDATE custom_formats SET name='private/secret'", ())
                        .await
                        .unwrap();
                }
            }
            // Exact-input miss inside the writer must fail, never run regex work there.
            let tx = c
                .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
                .await
                .unwrap();
            let outcome = crate::naming::destination::resolve_owned_destination(
                &tx,
                &input.target,
                &accepted.root,
                &accepted.basename,
                accepted.quality_id,
                accepted.edition.as_deref(),
                &accepted.evidence,
            )
            .await;
            assert!(matches!(
                outcome,
                Err(crate::naming::destination::DestinationError::Render(_))
            ));
            tx.rollback().await.unwrap();
            let source = input.source.clone();
            let error = prepare_owned(db.clone(), input).await.unwrap_err();
            assert_eq!(error.code(), "preflight_changed", "{change}");
            assert!(!format!("{error:?}").contains("private/secret"));
            assert_eq!(
                std::fs::read(dir.path(&format!("{media}/old.mkv"))).unwrap(),
                b"original-media"
            );
            assert_eq!(std::fs::read(source).unwrap(), b"new-media-content");
            assert_eq!(count(&db, "operations").await, 0);
        }
    }
}
