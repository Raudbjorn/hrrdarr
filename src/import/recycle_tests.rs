//! Upgrade retirement into the recycle bin through the real owned-import producer, journal and
//! restart paths. TV episode 1/series 1 and movie 1 share numeric IDs, and each domain's
//! original is file ID 1, so every assertion names its typed table.
use super::owned_cases::fixture;
use super::*;
use crate::api::MediaDomain;
use std::path::{Path, PathBuf};

async fn configure(db: &Database, media: MediaDomain, bin: &Path) {
    let c = db.connect().await.unwrap();
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    let revision = crate::recycle_bin::read(&tx, media).await.unwrap().revision;
    crate::recycle_bin::set(
        &tx,
        media,
        &crate::recycle_bin::Update {
            revision,
            path: Some(bin.to_str().unwrap().to_owned()),
            cleanup_days: 7,
        },
        crate::commands::now().unwrap(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}
async fn text(c: &Connection, sql: &str) -> Option<String> {
    c.query(sql, ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .map(|r| r.get::<Option<String>>(0).unwrap())
        .unwrap_or(None)
}
async fn scalar(c: &Connection, sql: &str) -> i64 {
    c.query(sql, ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<i64>(0)
        .unwrap()
}
/// Other library tests share the process-wide file-effect permit; retry only its refusal.
async fn execute_owned(db: &Arc<Database>, op: &str) -> Result<Operation> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        match execute(db.clone(), op).await {
            Err(e) if e.code() == "import_busy" && tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await
            }
            other => return other,
        }
    }
}
struct Case {
    dir: Scratch,
    db: Arc<Database>,
    op: String,
    movie: bool,
    oldpath: String,
    destination: String,
}
impl Case {
    async fn new(movie: bool, bin: Option<&Path>) -> Self {
        let dir = Scratch::new();
        let db = dir.database().await;
        let input = fixture(&dir, &db, movie, false).await;
        let destination = input.destination.clone();
        if let Some(bin) = bin {
            std::fs::create_dir_all(bin).unwrap();
            configure(&db, super::media(&input.target), bin).await;
        }
        let op = prepare_owned(db.clone(), input)
            .await
            .unwrap()
            .id
            .to_string();
        let oldpath = dir.path(if movie {
            "movies/old.mkv"
        } else {
            "tv/old.mkv"
        });
        Self {
            dir,
            db,
            op,
            movie,
            oldpath,
            destination,
        }
    }
    fn bin(&self) -> PathBuf {
        self.dir.0.join("bin")
    }
    /// `bin/<series or movie folder name>/<path relative to that folder>`.
    fn recycled(&self, bin: &Path) -> PathBuf {
        bin.join(if self.movie { "movies" } else { "tv" })
            .join("old.mkv")
    }
    fn artifact(&self) -> PathBuf {
        PathBuf::from(&self.oldpath)
            .parent()
            .unwrap()
            .join(format!(".hrrdarr-replaced-{}", self.op))
            .join("original")
    }
    fn history_media(&self) -> &'static str {
        if self.movie { "movie" } else { "episode" }
    }
    /// Process restart: the single local owner is released before the database is reopened.
    async fn reopen(self) -> Self {
        let Self {
            dir,
            db,
            op,
            movie,
            oldpath,
            destination,
        } = self;
        assert_eq!(Arc::strong_count(&db), 1, "no task still owns the database");
        drop(db);
        let db = dir.database().await;
        Self {
            dir,
            db,
            op,
            movie,
            oldpath,
            destination,
        }
    }
    async fn c(&self) -> Connection {
        self.db.connect().await.unwrap()
    }
}

pub(super) async fn upgrades() {
    recycled_upgrade_both_domains().await;
    no_bin_keeps_permanent_retirement().await;
    crash_between_commit_and_recycle_rolls_forward().await;
    crash_after_rename_before_commit_rolls_forward_at_worker_startup().await;
    failed_commit_preserves_original_in_place_and_bin_untouched().await;
    same_path_replacement_recycles_the_exchanged_original().await;
    cross_device_bin_copies_verifies_then_removes_artifact().await;
    missing_or_unwritable_bin_retains_artifact_and_reports().await;
}

async fn recycled_upgrade_both_domains() {
    for movie in [false, true] {
        let mut case = Case::new(movie, None).await;
        // The bin lives beside, not inside, the library folders.
        let bin = case.bin();
        std::fs::create_dir(&bin).unwrap();
        configure(
            &case.db,
            if movie {
                MediaDomain::Movies
            } else {
                MediaDomain::Tv
            },
            &bin,
        )
        .await;
        let original_inode = std::fs::metadata(&case.oldpath).unwrap().ino();
        let done = execute_owned(&case.db, &case.op).await.unwrap();
        assert_eq!(done.status, "complete");
        assert_eq!(
            done.message,
            "Replacement imported; original moved to the recycle bin"
        );
        let recycled = case.recycled(&bin);
        assert_eq!(std::fs::read(&recycled).unwrap(), b"original-media");
        assert_eq!(
            std::fs::metadata(&recycled).unwrap().ino(),
            original_inode,
            "same-device recycling is one rename of the original inode"
        );
        assert!(!Path::new(&case.oldpath).exists());
        assert!(!case.artifact().exists(), "no library copy remains");
        assert_eq!(
            std::fs::read(&case.destination).unwrap(),
            b"new-media-content"
        );
        let c = case.c().await;
        let domain = if movie { "movies" } else { "tv" };
        assert_eq!(
            text(&c, &format!("SELECT phase||':'||transfer||':'||coalesce(error_code,'') FROM upgrade_recycles WHERE media_type='{domain}' AND old_file_id=1")).await,
            Some("complete:rename:".into())
        );
        // Typed history: the movie row cannot select the episode with the same numeric ID.
        let (column, other) = if movie {
            ("movie_id", "episode_id")
        } else {
            ("episode_id", "movie_id")
        };
        assert_eq!(
            scalar(&c, &format!("SELECT count(*) FROM upgrade_recycle_history WHERE media_type='{}' AND {column}=1 AND {other} IS NULL AND file_id=1 AND reason='upgrade' AND path='{}' AND recycle_path='{}'", case.history_media(), case.oldpath, recycled.to_str().unwrap())).await,
            1
        );
        // History API exposes it under the deletion origin with reason `upgrade`.
        let app = crate::history::router(case.db.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let bytes = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!(
                "http://{address}/api/v1/history?media_type={domain}"
            ))
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        let page: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        server.abort();
        let _ = server.await;
        assert_eq!(page["total"], 2, "{page}");
        let event = page["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["origin"] == "native_deletion")
            .unwrap_or_else(|| panic!("{page}"))
            .clone();
        assert_eq!(event["reason"], "upgrade");
        assert_eq!(event["event_type"], "file_deleted");
        assert_eq!(
            event["target"],
            serde_json::json!({"media_type": case.history_media(), "id": 1})
        );
        assert_eq!(
            event["file"],
            serde_json::json!({"media_type": domain, "id": 1})
        );
        assert_eq!(event["recycle_path"], recycled.to_str().unwrap());
        // Rerunning a completed import is a no-op: no second transfer or history row.
        drop(c);
        case = case.reopen().await;
        assert_eq!(
            execute_owned(&case.db, &case.op).await.unwrap().status,
            "complete"
        );
        let c = case.c().await;
        assert_eq!(
            scalar(&c, "SELECT count(*) FROM upgrade_recycle_history").await,
            1
        );
        assert_eq!(std::fs::read(&recycled).unwrap(), b"original-media");
    }
}

async fn no_bin_keeps_permanent_retirement() {
    for movie in [false, true] {
        let case = Case::new(movie, None).await;
        let done = execute_owned(&case.db, &case.op).await.unwrap();
        assert_eq!(done.status, "complete");
        // Reasoning: unchanged pre-0061 behaviour; the original stays in its private artifact.
        assert_eq!(
            done.message,
            "Replacement imported; original bytes retained in private recovery artifact"
        );
        assert_eq!(std::fs::read(case.artifact()).unwrap(), b"original-media");
        assert!(!Path::new(&case.oldpath).exists());
        assert!(!case.bin().exists());
        let c = case.c().await;
        assert_eq!(scalar(&c, "SELECT count(*) FROM upgrade_recycles").await, 0);
        assert_eq!(
            scalar(&c, "SELECT count(*) FROM upgrade_recycle_history").await,
            0
        );
        assert_eq!(
            text(&c, "SELECT retirement_state FROM rss_candidate_imports").await,
            Some("quarantined".into())
        );
    }
}

async fn crash_between_commit_and_recycle_rolls_forward() {
    for movie in [false, true] {
        let mut case = Case::new(movie, None).await;
        let bin = case.bin();
        std::fs::create_dir(&bin).unwrap();
        configure(
            &case.db,
            if movie {
                MediaDomain::Movies
            } else {
                MediaDomain::Tv
            },
            &bin,
        )
        .await;
        let c = case.c().await;
        // The import commits and quarantines the original, then dies before recording intent.
        c.execute_batch("CREATE TRIGGER injected_recycle_intent BEFORE INSERT ON upgrade_recycles BEGIN SELECT RAISE(ABORT,'injected crash before recycle intent'); END;").await.unwrap();
        assert!(execute_owned(&case.db, &case.op).await.is_err());
        assert_eq!(
            status(case.db.clone(), &case.op).await.unwrap().status,
            "committed"
        );
        assert_eq!(std::fs::read(case.artifact()).unwrap(), b"original-media");
        assert!(!case.recycled(&bin).exists());
        c.execute_batch("DROP TRIGGER injected_recycle_intent;")
            .await
            .unwrap();
        drop(c);
        // Restart, then resume the same operation: it rolls forward to the recycled state.
        case = case.reopen().await;
        assert_eq!(
            execute_owned(&case.db, &case.op).await.unwrap().status,
            "complete"
        );
        assert_eq!(
            std::fs::read(case.recycled(&bin)).unwrap(),
            b"original-media"
        );
        assert!(!case.artifact().exists());
        let c = case.c().await;
        assert_eq!(
            text(&c, "SELECT phase FROM upgrade_recycles").await,
            Some("complete".into())
        );
        assert_eq!(
            scalar(&c, "SELECT count(*) FROM upgrade_recycle_history").await,
            1
        );
        assert_eq!(scalar(&c, "SELECT count(*) FROM import_history").await, 1);
    }
}
async fn crash_after_rename_before_commit_rolls_forward_at_worker_startup() {
    for movie in [false, true] {
        let mut case = Case::new(movie, None).await;
        let bin = case.bin();
        std::fs::create_dir(&bin).unwrap();
        configure(
            &case.db,
            if movie {
                MediaDomain::Movies
            } else {
                MediaDomain::Tv
            },
            &bin,
        )
        .await;
        let marker = case.dir.0.file_name().unwrap().to_str().unwrap().to_owned();
        recycle::arm_crash(&marker);
        let crashed = execute_owned(&case.db, &case.op).await;
        recycle::disarm_crash(&marker);
        assert!(crashed.is_err());
        // The rename happened; the journal still says admitted and no history exists.
        assert_eq!(
            std::fs::read(case.recycled(&bin)).unwrap(),
            b"original-media"
        );
        assert!(!case.artifact().exists());
        let c = case.c().await;
        assert_eq!(
            text(&c, "SELECT phase FROM upgrade_recycles").await,
            Some("admitted".into())
        );
        assert_eq!(
            scalar(&c, "SELECT count(*) FROM upgrade_recycle_history").await,
            0
        );
        // The import cannot complete while its recycle is unsettled.
        assert!(
            c.execute(
                "UPDATE import_journal SET phase='complete' WHERE operation_id=?",
                [case.op.clone()]
            )
            .await
            .is_err()
        );
        drop(c);
        // Real worker startup reconciles the journal against disk and rolls forward.
        case = case.reopen().await;
        let (_, refresh) = crate::providers::router_with_refresh(case.db.clone(), None);
        let runtime = crate::commands::start(case.db.clone(), refresh)
            .await
            .unwrap();
        runtime.shutdown().await;
        let c = case.c().await;
        assert_eq!(
            text(&c, "SELECT phase||':'||transfer FROM upgrade_recycles").await,
            Some("complete:rename".into())
        );
        assert_eq!(
            scalar(&c, "SELECT count(*) FROM upgrade_recycle_history").await,
            1
        );
        // The import's own resume then completes without a second transfer or event.
        assert_eq!(
            execute_owned(&case.db, &case.op).await.unwrap().status,
            "complete"
        );
        assert_eq!(
            scalar(&c, "SELECT count(*) FROM upgrade_recycle_history").await,
            1
        );
        assert_eq!(
            std::fs::read(case.recycled(&bin)).unwrap(),
            b"original-media"
        );
    }
}

async fn failed_commit_preserves_original_in_place_and_bin_untouched() {
    for movie in [false, true] {
        let mut case = Case::new(movie, None).await;
        let bin = case.bin();
        std::fs::create_dir(&bin).unwrap();
        configure(
            &case.db,
            if movie {
                MediaDomain::Movies
            } else {
                MediaDomain::Tv
            },
            &bin,
        )
        .await;
        let c = case.c().await;
        c.execute_batch("CREATE TRIGGER injected_upgrade_commit BEFORE INSERT ON import_history BEGIN SELECT RAISE(ABORT,'injected commit failure'); END;").await.unwrap();
        assert!(execute_owned(&case.db, &case.op).await.is_err());
        assert_eq!(std::fs::read(&case.oldpath).unwrap(), b"original-media");
        let association = if movie {
            "SELECT id FROM movie_files WHERE movie_id=1 AND path LIKE '%/movies/old.mkv'"
        } else {
            "SELECT episode_file_id FROM episodes WHERE id=1 AND episode_file_id=1"
        };
        assert_eq!(
            scalar(&c, &format!("SELECT count(*) FROM ({association})")).await,
            1
        );
        assert_eq!(std::fs::read_dir(&bin).unwrap().count(), 0, "bin untouched");
        assert_eq!(scalar(&c, "SELECT count(*) FROM upgrade_recycles").await, 0);
        c.execute_batch("DROP TRIGGER injected_upgrade_commit;")
            .await
            .unwrap();
        drop(c);
        case = case.reopen().await;
        assert_eq!(
            execute_owned(&case.db, &case.op).await.unwrap().status,
            "complete"
        );
        assert_eq!(
            std::fs::read(case.recycled(&bin)).unwrap(),
            b"original-media"
        );
    }
}

async fn same_path_replacement_recycles_the_exchanged_original() {
    for movie in [false, true] {
        let dir = Scratch::new();
        let db = dir.database().await;
        let input = super::same_path_cases::fixture(&dir, &db, movie, Mode::Copy, false).await;
        let destination = input.destination.clone();
        let original_inode = std::fs::metadata(&destination).unwrap().ino();
        let bin = dir.0.join("bin");
        std::fs::create_dir(&bin).unwrap();
        configure(&db, super::media(&input.target), &bin).await;
        let op = prepare_owned(db.clone(), input)
            .await
            .unwrap()
            .id
            .to_string();
        assert_eq!(execute_owned(&db, &op).await.unwrap().status, "complete");
        // The library pathname now holds the new bytes; the exchanged original inode is in the
        // bin under the same name, and its recovery artifact is empty.
        assert_eq!(std::fs::read(&destination).unwrap(), b"new-media-content");
        let name = Path::new(&destination).file_name().unwrap();
        let recycled = bin.join(if movie { "movies" } else { "tv" }).join(name);
        assert_eq!(std::fs::read(&recycled).unwrap(), b"original-media");
        assert_eq!(std::fs::metadata(&recycled).unwrap().ino(), original_inode);
        let artifact = Path::new(&destination)
            .parent()
            .unwrap()
            .join(format!(".hrrdarr-replaced-{op}"))
            .join("original");
        assert!(!artifact.exists());
        let c = db.connect().await.unwrap();
        assert_eq!(
            text(&c, "SELECT phase||':'||transfer FROM upgrade_recycles").await,
            Some("complete:rename".into())
        );
        assert_eq!(
            scalar(&c, "SELECT count(*) FROM upgrade_recycle_history").await,
            1
        );
    }
}

async fn cross_device_bin_copies_verifies_then_removes_artifact() {
    let shm = Path::new("/dev/shm");
    for movie in [false, true] {
        let mut case = Case::new(movie, None).await;
        if !shm.is_dir()
            || std::fs::metadata(shm).unwrap().dev()
                == std::fs::metadata(&case.dir.0).unwrap().dev()
        {
            eprintln!(
                "cross-device scratch unavailable: /dev/shm shares the temp device; not simulated"
            );
            return;
        }
        let root = shm.join(format!("hrrdarr-upgrade-recycle-{}", Uuid::new_v4()));
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        configure(
            &case.db,
            if movie {
                MediaDomain::Movies
            } else {
                MediaDomain::Tv
            },
            &bin,
        )
        .await;
        // Crash after the verified copy is published and checkpointed, before artifact removal.
        let marker = case.dir.0.file_name().unwrap().to_str().unwrap().to_owned();
        recycle::arm_crash(&marker);
        let crashed = execute_owned(&case.db, &case.op).await;
        recycle::disarm_crash(&marker);
        assert!(crashed.is_err());
        let recycled = case.recycled(&bin);
        assert_eq!(std::fs::read(&recycled).unwrap(), b"original-media");
        assert_eq!(
            std::fs::read(case.artifact()).unwrap(),
            b"original-media",
            "the artifact is removed only after the copy is checkpointed and re-observed"
        );
        let c = case.c().await;
        assert_eq!(
            text(&c, "SELECT phase||':'||transfer FROM upgrade_recycles").await,
            Some("copied:copy".into())
        );
        drop(c);
        case = case.reopen().await;
        assert_eq!(
            execute_owned(&case.db, &case.op).await.unwrap().status,
            "complete"
        );
        assert!(!case.artifact().exists());
        assert_ne!(
            std::fs::metadata(&recycled).unwrap().dev(),
            std::fs::metadata(&case.dir.0).unwrap().dev()
        );
        assert_eq!(std::fs::read(&recycled).unwrap(), b"original-media");
        let c = case.c().await;
        assert_eq!(
            text(
                &c,
                "SELECT phase||':'||transfer||':'||coalesce(error_code,'') FROM upgrade_recycles"
            )
            .await,
            Some("complete:copy:".into())
        );
        let leftovers = std::fs::read_dir(recycled.parent().unwrap())
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_str()
                    .unwrap()
                    .starts_with(crate::recycle_bin::fsops::PARTIAL_PREFIX)
            })
            .count();
        assert_eq!(leftovers, 0, "no private partial copy remains");
    }
}

async fn missing_or_unwritable_bin_retains_artifact_and_reports() {
    use std::os::unix::fs::PermissionsExt;
    for (movie, unwritable) in [(false, false), (true, false), (false, true), (true, true)] {
        let case = Case::new(movie, None).await;
        let bin = case.bin();
        std::fs::create_dir(&bin).unwrap();
        let media = if movie {
            MediaDomain::Movies
        } else {
            MediaDomain::Tv
        };
        configure(&case.db, media, &bin).await;
        if unwritable {
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o555)).unwrap();
            if std::fs::create_dir(bin.join("probe")).is_ok() {
                std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
                eprintln!("privileged runner can write mode 0555; unwritable bin not established");
                continue;
            }
        } else {
            // The configured bin disappeared (for example an unmounted share).
            std::fs::remove_dir(&bin).unwrap();
        }
        let scope = if movie { "movies" } else { "tv" };
        let health = format!(
            "SELECT generation FROM health_checks WHERE check_key='recycle_bin' AND scope='{scope}'"
        );
        let before = scalar(&case.c().await, &health).await;
        let done = execute_owned(&case.db, &case.op).await.unwrap();
        if unwritable {
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(
            done.status, "complete",
            "a bin failure never blocks the import"
        );
        assert_eq!(
            done.message,
            "Replacement imported; recycling the original failed, bytes retained in private recovery artifact"
        );
        assert_eq!(
            std::fs::read(case.artifact()).unwrap(),
            b"original-media",
            "no data loss: the artifact keeps the replaced bytes"
        );
        let c = case.c().await;
        assert_eq!(
            text(&c, "SELECT phase||':'||error_code FROM upgrade_recycles").await,
            Some(if unwritable {
                "retained:recycle_bin_unwritable".into()
            } else {
                "retained:recycle_bin_unavailable".into()
            })
        );
        assert_eq!(
            scalar(&c, "SELECT count(*) FROM upgrade_recycle_history").await,
            0
        );
        // The domain's recycle-bin health check is marked for re-evaluation.
        assert!(scalar(&c, &health).await > before);
        if !unwritable {
            assert!(
                !bin.exists(),
                "a missing bin is never created by retirement"
            );
        }
    }
}
