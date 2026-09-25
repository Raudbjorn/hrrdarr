#[path = "owned_tests.rs"]
mod owned_cases;
#[path = "same_path_tests.rs"]
mod same_path_cases;
use super::*;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
struct Scratch(std::path::PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("hrrdarr-import-recovery-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        for p in ["tv", "movies", "downloads"] {
            std::fs::create_dir(path.join(p)).unwrap();
        }
        Self(path)
    }
    fn path(&self, p: &str) -> String {
        self.0.join(p).to_str().unwrap().to_owned()
    }
    async fn database(&self) -> Arc<Database> {
        Arc::new(Database::open_local(self.0.join("db")).await.unwrap())
    }
    async fn setup(&self, db: &Database) {
        db.connect().await.unwrap().execute_batch(&format!("INSERT INTO series(id,title,path)VALUES(1,'TV','{}');INSERT INTO seasons(series_id,number)VALUES(1,1);INSERT INTO episodes(id,series_id,season,number,title)VALUES(1,1,1,1,'Episode');INSERT INTO movie_metadata(id,title)VALUES(1,'Movie');INSERT INTO movies(id,metadata_id,path)VALUES(1,1,'{}');",self.path("tv"),self.path("movies"))).await.unwrap();
    }
    async fn preview(&self, db: Arc<Database>, movie: bool, mode: Mode) -> String {
        std::fs::write(self.path("downloads/media"), b"verified-media-content").unwrap();
        super::preview(
            db,
            ImportInput::Typed(ManualImportRequest {
                target: if movie {
                    MediaTarget::Movie(1)
                } else {
                    MediaTarget::Episode(1)
                },
                source: self.path("downloads/media"),
                mode,
                destination: self.path(if movie {
                    "movies/media.mkv"
                } else {
                    "tv/media.mkv"
                }),
            }),
        )
        .await
        .unwrap()
        .id
        .to_string()
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(
            self.0.join("downloads"),
            std::fs::Permissions::from_mode(0o700),
        );
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
async fn stage_and_publish(db: &Database, id: &str, record_publish: bool) -> (Plan, Stage) {
    let c = db.connect().await.unwrap();
    let rec = load(&c, id).await.unwrap();
    let plan = rec.plan.unwrap();
    checkpoint(&c, id, "staging", None).await.unwrap();
    let stage = plan.create_stage().unwrap();
    checkpoint(&c, id, "staging", Some(&stage)).await.unwrap();
    let stage = plan.transfer(stage).unwrap();
    checkpoint(&c, id, "staged", Some(&stage)).await.unwrap();
    plan.publish(&stage).unwrap();
    if record_publish {
        checkpoint(&c, id, "published", Some(&stage)).await.unwrap();
    }
    (plan, stage)
}
async fn count(db: &Database, table: &str) -> i64 {
    db.connect()
        .await
        .unwrap()
        .query(&format!("SELECT count(*) FROM {table}"), ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}

#[tokio::test]
async fn restart_recovers_publication_commit_and_cleanup_without_losing_sources() {
    owned_cases::replacements().await;
    same_path_cases::replacements().await;
    // These use the real phase helpers and reopen the owned database between interruption and
    // execute. Boundary setup stops before the next journal write; it does not mock recovery.
    for movie in [false, true] {
        let dir = Scratch::new();
        let db = dir.database().await;
        dir.setup(&db).await;
        let id = dir.preview(db.clone(), movie, Mode::Copy).await;
        let (_, stage) = stage_and_publish(&db, &id, false).await;
        let dest = dir.path(if movie {
            "movies/media.mkv"
        } else {
            "tv/media.mkv"
        });
        assert_eq!(count(&db, "import_history").await, 0);
        drop(db);
        let db = dir.database().await;
        assert_eq!(execute(db.clone(), &id).await.unwrap().status, "complete");
        assert_eq!(count(&db, "import_history").await, 1);
        assert_eq!(std::fs::read(&dest).unwrap(), b"verified-media-content");
        assert!(std::path::Path::new(&dir.path("downloads/media")).exists());
        assert_eq!(execute(db.clone(), &id).await.unwrap().status, "complete");
        assert_eq!(count(&db, "import_history").await, 1);
        assert_eq!(stage.size(), 22);
        drop(db);
    }
    // An interrupted copy can restart only after its private stage identity was recorded.
    let dir = Scratch::new();
    let db = dir.database().await;
    dir.setup(&db).await;
    let id = dir.preview(db.clone(), false, Mode::Copy).await;
    let c = db.connect().await.unwrap();
    let plan = load(&c, &id).await.unwrap().plan.unwrap();
    checkpoint(&c, &id, "staging", None).await.unwrap();
    let stage = plan.create_stage().unwrap();
    checkpoint(&c, &id, "staging", Some(&stage)).await.unwrap();
    std::fs::write(
        dir.path(&format!("tv/.hrrdarr-import-{id}/media")),
        b"partial",
    )
    .unwrap();
    drop(c);
    drop(db);
    let db = dir.database().await;
    assert_eq!(execute(db.clone(), &id).await.unwrap().status, "complete");
    assert_eq!(
        std::fs::read(dir.path("tv/media.mkv")).unwrap(),
        b"verified-media-content"
    );
    drop(db);
    // Transaction failure after physical publication retains both files and no partial DB rows.
    let dir = Scratch::new();
    let db = dir.database().await;
    dir.setup(&db).await;
    let id = dir.preview(db.clone(), false, Mode::Move).await;
    db.connect().await.unwrap().execute_batch("CREATE TRIGGER injected_import_failure BEFORE INSERT ON import_history BEGIN SELECT RAISE(ABORT,'scratch failure');END;").await.unwrap();
    assert!(execute(db.clone(), &id).await.is_err());
    assert_eq!(status(db.clone(), &id).await.unwrap().status, "published");
    assert_eq!(count(&db, "episode_files").await, 0);
    assert_eq!(count(&db, "file_metadata").await, 0);
    assert!(std::path::Path::new(&dir.path("downloads/media")).exists());
    db.connect()
        .await
        .unwrap()
        .execute_batch("DROP TRIGGER injected_import_failure")
        .await
        .unwrap();
    drop(db);
    let db = dir.database().await;
    assert_eq!(execute(db.clone(), &id).await.unwrap().status, "complete");
    assert!(!std::path::Path::new(&dir.path("downloads/media")).exists());
    assert_eq!(count(&db, "import_history").await, 1);
    drop(db);
    // A changed source after DB commit is retained. Restoring the exact inode/content permits retry.
    let dir = Scratch::new();
    let db = dir.database().await;
    dir.setup(&db).await;
    let id = dir.preview(db.clone(), true, Mode::Move).await;
    let (plan, stage) = stage_and_publish(&db, &id, true).await;
    commit(
        &db.connect().await.unwrap(),
        &id,
        &MediaTarget::Movie(1),
        &plan,
        &stage,
    )
    .await
    .unwrap();
    std::fs::rename(dir.path("downloads/media"), dir.path("downloads/original")).unwrap();
    std::fs::write(dir.path("downloads/media"), b"unrelated replacement").unwrap();
    drop(db);
    let db = dir.database().await;
    assert!(execute(db.clone(), &id).await.is_err());
    assert_eq!(
        std::fs::read(dir.path("downloads/media")).unwrap(),
        b"unrelated replacement"
    );
    assert_eq!(status(db.clone(), &id).await.unwrap().status, "committed");
    std::fs::remove_file(dir.path("downloads/media")).unwrap();
    std::fs::rename(dir.path("downloads/original"), dir.path("downloads/media")).unwrap();
    assert_eq!(execute(db.clone(), &id).await.unwrap().status, "complete");
    drop(db);
    // Quarantine rename completed, but source_retired was not checkpointed. Recovery finds the
    // verified quarantined inode, not the unrelated new file at the old source pathname.
    let dir = Scratch::new();
    let db = dir.database().await;
    dir.setup(&db).await;
    let id = dir.preview(db.clone(), true, Mode::Move).await;
    let (plan, stage) = stage_and_publish(&db, &id, true).await;
    let c = db.connect().await.unwrap();
    commit(&c, &id, &MediaTarget::Movie(1), &plan, &stage)
        .await
        .unwrap();
    let stage = plan.prepare_cleanup(stage).unwrap();
    checkpoint(&c, &id, "committed", Some(&stage))
        .await
        .unwrap();
    let _uncheckpointed = plan.retire_source(stage).unwrap();
    std::fs::write(dir.path("downloads/media"), b"unrelated recreated source").unwrap();
    drop(c);
    drop(db);
    let db = dir.database().await;
    assert_eq!(execute(db.clone(), &id).await.unwrap().status, "complete");
    assert_eq!(
        std::fs::read(dir.path("downloads/media")).unwrap(),
        b"unrelated recreated source"
    );
    assert_eq!(count(&db, "import_history").await, 1);
    drop(db);
    // Source unlink and stage cleanup happened, but the completion checkpoint did not.
    let dir = Scratch::new();
    let db = dir.database().await;
    dir.setup(&db).await;
    let id = dir.preview(db.clone(), false, Mode::Move).await;
    let (plan, stage) = stage_and_publish(&db, &id, true).await;
    let c = db.connect().await.unwrap();
    commit(&c, &id, &MediaTarget::Episode(1), &plan, &stage)
        .await
        .unwrap();
    let stage = plan.prepare_cleanup(stage).unwrap();
    checkpoint(&c, &id, "committed", Some(&stage))
        .await
        .unwrap();
    let stage = plan.retire_source(stage).unwrap();
    checkpoint(&c, &id, "committed", Some(&stage))
        .await
        .unwrap();
    plan.cleanup(&stage).unwrap();
    std::fs::write(dir.path("downloads/media"), b"new download at old path").unwrap();
    drop(c);
    drop(db);
    let db = dir.database().await;
    assert_eq!(execute(db.clone(), &id).await.unwrap().status, "complete");
    assert_eq!(
        std::fs::read(dir.path("downloads/media")).unwrap(),
        b"new download at old path"
    );
    assert_eq!(count(&db, "import_history").await, 1);
    drop(db);
    // Aborting the async waiter does not release the database owner or execution permit while
    // its blocking filesystem job is alive (also the runtime-shutdown ownership boundary).
    let dir = Scratch::new();
    let db = dir.database().await;
    assert!(
        EXECUTING
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    );
    let lease = Arc::new(Permit {
        _db: Some(db.clone()),
    });
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let task = tokio::spawn(async move {
        leased(&lease, move || {
            ready_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(())
        })
        .await
    });
    tokio::task::yield_now().await;
    ready_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    drop(db);
    assert!(Database::open_local(dir.0.join("db")).await.is_err());
    assert!(EXECUTING.load(Ordering::Acquire));
    release_tx.send(()).unwrap();
    let started = std::time::Instant::now();
    while EXECUTING.load(Ordering::Acquire) {
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        tokio::task::yield_now().await;
    }
    drop(dir.database().await);
}

#[tokio::test]
async fn filesystem_identity_checks_reject_swaps_and_retain_unrecorded_staging() {
    // No execute() calls in this test, so it can run alongside the single-worker recovery test.
    let dir = Scratch::new();
    let db = dir.database().await;
    dir.setup(&db).await;
    let id = dir.preview(db.clone(), false, Mode::Copy).await;
    let c = db.connect().await.unwrap();
    let plan = load(&c, &id).await.unwrap().plan.unwrap();
    std::fs::rename(dir.path("tv"), dir.path("old-tv")).unwrap();
    std::fs::create_dir(dir.path("tv")).unwrap();
    assert!(plan.create_stage().is_err());
    assert_eq!(
        std::fs::read(dir.path("downloads/media")).unwrap(),
        b"verified-media-content"
    );
    std::fs::remove_dir(dir.path("tv")).unwrap();
    std::fs::rename(dir.path("old-tv"), dir.path("tv")).unwrap();
    let stage = plan.create_stage().unwrap();
    assert!(plan.create_stage().is_err());
    let stage = plan.transfer(stage).unwrap();
    std::fs::write(dir.path("tv/media.mkv"), b"unrelated destination").unwrap();
    assert!(plan.publish(&stage).is_err());
    assert_eq!(
        std::fs::read(dir.path("tv/media.mkv")).unwrap(),
        b"unrelated destination"
    );
    drop(c);
    drop(db);
    // Real same-device hardlinks retain inode identity and expose subsequent source writes.
    let dir = Scratch::new();
    let db = dir.database().await;
    dir.setup(&db).await;
    let id = dir.preview(db.clone(), false, Mode::Hardlink).await;
    let c = db.connect().await.unwrap();
    let plan = load(&c, &id).await.unwrap().plan.unwrap();
    let stage = plan.create_stage().unwrap();
    let stage = plan.transfer(stage).unwrap();
    plan.publish(&stage).unwrap();
    assert_eq!(
        std::fs::metadata(dir.path("downloads/media"))
            .unwrap()
            .ino(),
        std::fs::metadata(dir.path("tv/media.mkv")).unwrap().ino()
    );
    std::fs::write(dir.path("downloads/media"), b"concurrent writer").unwrap();
    assert!(plan.verify_destination(&stage).is_err());
    drop(c);
    drop(db);
}
