use super::*;

struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            eprintln!("scratch cleanup failed: {error}");
        }
    }
}
const IDENTITY_SQL: &str = "INSERT INTO series(id,title,path)VALUES(1,'Harbor','/fictional/tv');INSERT INTO seasons(series_id,number)VALUES(1,0),(1,1);INSERT INTO episodes(id,series_id,season,number,title,runtime,absolute_episode_number,air_date)VALUES(1,1,1,1,'Pilot',45,47,'2026-09-24'),(2,1,1,2,'Next',45,48,'2026-09-25');INSERT INTO movie_metadata(id,title,year,runtime,original_language)VALUES(1,'Harbor',2020,100,1);INSERT INTO movies(id,metadata_id,path)VALUES(1,1,'/fictional/movie');INSERT INTO quality_profiles VALUES(1,'tv','HD'),(2,'movies','HD');INSERT INTO quality_profile_items(profile_id,media_type,quality_id,position,allowed)VALUES(1,'tv',3,0,0),(1,'tv',7,1,1),(2,'movies',3,0,0),(2,'movies',7,1,1);INSERT INTO quality_profile_policies(profile_id,media_type,upgrade_allowed,cutoff_quality_id,min_format_score,cutoff_format_score,min_upgrade_format_score,language_id)VALUES(1,'tv',0,7,100,100,100,NULL),(2,'movies',0,7,100,100,100,2);INSERT INTO library_settings(media_type,series_id,quality_profile_id,series_type,use_scene_numbering)VALUES('tv',1,1,'standard',0);INSERT INTO library_settings(media_type,movie_id,quality_profile_id,minimum_availability)VALUES('movies',1,2,'released');";
async fn seeded() -> (Arc<Database>, Scratch) {
    let path = std::env::temp_dir().join(format!("hrrdarr-scan-command-{}", Uuid::new_v4()));
    std::fs::create_dir(&path).unwrap();
    let db = Arc::new(Database::open_local(path.join("db")).await.unwrap());
    db.connect()
        .await
        .unwrap()
        .execute_batch(IDENTITY_SQL)
        .await
        .unwrap();
    (db, Scratch(path))
}
fn input(id: Uuid, path: &std::path::Path, mode: ScanMode) -> DownloadedScanInput {
    DownloadedScanInput {
        id,
        path: path.to_str().unwrap().into(),
        mode,
        priority: CommandPriority::Normal,
        tracked: None,
        supersedes_command_id: None,
    }
}
async fn enqueue(
    db: &Arc<Database>,
    media: MediaDomain,
    request: DownloadedScanInput,
) -> Result<(StatusCode, DownloadedScanCommand)> {
    let (code, Json(command)) = create(
        State(db.clone()),
        media,
        Ok(Query(Empty {})),
        Ok(Json(request)),
    )
    .await?;
    Ok((code, command))
}
async fn actual_claim(db: &Database, id: Uuid) -> Option<ScanClaim> {
    let c = connection(db).await.unwrap();
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    let claimed = claim(&tx, id, now().unwrap()).await.unwrap();
    tx.commit().await.unwrap();
    claimed
}
async fn snapshot(db: &Database, id: Uuid) -> Parent {
    read_parent(&connection(db).await.unwrap(), id)
        .await
        .unwrap()
        .unwrap()
}
async fn child_count(db: &Database, id: Uuid) -> i64 {
    connection(db)
        .await
        .unwrap()
        .query(
            "SELECT count(*) FROM downloaded_scan_children WHERE command_id=?",
            [id.hyphenated().to_string()],
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}
fn wav() -> Vec<u8> {
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&16036u32.to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&8000u32.to_le_bytes());
    wav.extend_from_slice(&16000u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&16000u32.to_le_bytes());
    wav.resize(16044, 0);
    wav
}
/// One acceptable episode file plus a rejected sidecar, in a folder named for the release.
fn release(scratch: &Scratch) -> std::path::PathBuf {
    let folder = scratch.0.join("Harbor.S01E01.1080p.WEB-DL");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join("Harbor.S01E01.1080p.WEB-DL.flv"), wav()).unwrap();
    std::fs::write(folder.join("readme.nfo"), b"text").unwrap();
    folder
}

#[test]
fn request_paths_and_remote_identifiers_follow_the_importer_grammar() {
    for ok in ["/a", "/a/b.mkv", "/a/with\nnewline/and\ttab", "/a/b c/d"] {
        assert!(valid_path(ok), "{ok:?}");
    }
    for bad in [
        "", "/", "relative", "./a", "/a/", "/a//b", "/a/./b", "/a/../b", "/a\0b", "//a",
    ] {
        assert!(!valid_path(bad), "{bad:?}");
    }
    assert!(valid_path(&format!("/{}", "a".repeat(4095))));
    assert!(!valid_path(&format!("/{}", "a".repeat(4096))));
    assert!(valid_remote(&"a".repeat(40)) && valid_remote(&"0".repeat(64)));
    for bad in [
        "",
        "A".repeat(40).as_str(),
        "g".repeat(40).as_str(),
        "a".repeat(41).as_str(),
    ] {
        assert!(!valid_remote(bad));
    }
}

#[tokio::test]
async fn busy_capture_deferral_survives_recovery_without_spending_attempts() {
    let (db, scratch) = seeded().await;
    let id = Uuid::new_v4();
    enqueue(
        &db,
        MediaDomain::Tv,
        input(id, &release(&scratch), ScanMode::Auto),
    )
    .await
    .unwrap();
    let first = actual_claim(&db, id).await.unwrap();
    settle(
        &db,
        &first,
        Failure::Busy("scan_capture_busy", CAPTURE_BUSY_DELAY),
    )
    .await
    .unwrap();
    let deferred = snapshot(&db, id).await;
    let c = connection(&db).await.unwrap();
    // Each worker step recovers before claiming: a capacity deferral must keep its delay and
    // attempt count, unlike an interrupted discovery that was actually running.
    for code in ["storage_error", "interrupted"] {
        recover(&c, deferred.next_attempt_at - 1, code)
            .await
            .unwrap();
        let current = snapshot(&db, id).await;
        assert!(matches!(current.status, CommandStatus::Running));
        assert_eq!(current.next_attempt_at, deferred.next_attempt_at);
        assert_eq!((current.attempts, current.run_generation), (1, 1));
    }
    assert!(
        claim(&c, id, deferred.next_attempt_at - 1)
            .await
            .unwrap()
            .is_none()
    );
    let resumed = claim(&c, id, deferred.next_attempt_at)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((resumed.attempts, resumed.run_generation), (1, 2));
    recover(&c, deferred.next_attempt_at + 1, "interrupted")
        .await
        .unwrap();
    assert!(matches!(
        snapshot(&db, id).await.status,
        CommandStatus::RetryWait
    ));

    let retry = claim(&c, id, deferred.next_attempt_at + 1)
        .await
        .unwrap()
        .unwrap();
    settle(
        &db,
        &retry,
        Failure::Busy("scan_capture_busy", CAPTURE_BUSY_DELAY),
    )
    .await
    .unwrap();
    c.execute(
        "UPDATE downloaded_scan_commands SET cancel_requested=1 WHERE id=?",
        [id.to_string()],
    )
    .await
    .unwrap();
    recover(&c, now().unwrap(), "storage_error").await.unwrap();
    assert!(matches!(
        snapshot(&db, id).await.status,
        CommandStatus::Cancelled
    ));
}

#[tokio::test]
async fn claim_counts_attempts_and_generations_and_recovery_spares_published_parents() {
    let (db, scratch) = seeded().await;
    let folder = release(&scratch);
    let id = Uuid::new_v4();
    let (code, created) = enqueue(&db, MediaDomain::Tv, input(id, &folder, ScanMode::Auto))
        .await
        .unwrap();
    assert_eq!(code, StatusCode::ACCEPTED);
    assert!(matches!(created.phase, ScanPhase::Queued));
    // Nothing is claimable twice and a claim consumes exactly one attempt and one generation.
    let first = actual_claim(&db, id).await.unwrap();
    assert!(actual_claim(&db, id).await.is_none());
    let parent = snapshot(&db, id).await;
    assert_eq!((parent.attempts, parent.run_generation), (1, 1));
    assert_eq!(first.run_generation, 1);
    // An interrupted unpublished claim retries within the budget and bumps the generation.
    let c = connection(&db).await.unwrap();
    recover(&c, now().unwrap(), "interrupted").await.unwrap();
    assert!(matches!(
        snapshot(&db, id).await.status,
        CommandStatus::RetryWait
    ));
    let second = actual_claim(&db, id).await.unwrap();
    assert_eq!((second.attempts, second.run_generation), (2, 2));
    // Publish, then recovery must not reset the published running parent.
    let found = discover(&db, &second).await.ok().unwrap();
    publish(&db, &second, found).await.ok().unwrap();
    recover(&c, now().unwrap(), "storage_error").await.unwrap();
    let parent = snapshot(&db, id).await;
    assert!(matches!(parent.status, CommandStatus::Running));
    assert_eq!(parent.revision, 1);
    assert_eq!((parent.attempts, parent.run_generation), (2, 2));
    // The published parent is now claimed as an import continuation: a new scheduling
    // generation that never consumes an attempt.
    let continuation = actual_claim(&db, id).await.unwrap();
    assert!(continuation.continuation);
    assert_eq!((continuation.attempts, continuation.run_generation), (2, 3));
    let parent = snapshot(&db, id).await;
    assert_eq!((parent.attempts, parent.run_generation), (2, 3));
}

#[tokio::test]
async fn cancel_of_a_parked_parent_settles_every_ready_child_without_any_operation() {
    let (db, scratch) = seeded().await;
    let folder = release(&scratch);
    let id = Uuid::new_v4();
    enqueue(&db, MediaDomain::Tv, input(id, &folder, ScanMode::Auto))
        .await
        .unwrap();
    // Publication only: no continuation step runs, so the parent stays parked.
    run(&db, actual_claim(&db, id).await.unwrap())
        .await
        .unwrap();
    assert!(matches!(
        snapshot(&db, id).await.status,
        CommandStatus::Running
    ));
    for _ in 0..2 {
        let Json(cancelled) = cancel(
            State(db.clone()),
            MediaDomain::Tv,
            Path(id.to_string()),
            Ok(Query(Empty {})),
        )
        .await
        .unwrap();
        assert!(matches!(cancelled.status, CommandStatus::Cancelled));
        assert!(matches!(cancelled.phase, ScanPhase::Settled));
        assert!(cancelled.cancel_requested);
        assert_eq!((cancelled.counts.ready, cancelled.counts.cancelled), (0, 1));
    }
    // A cancelled parent is never claimed again, and nothing was prepared or transferred.
    assert!(actual_claim(&db, id).await.is_none());
    let operations: i64 = connection(&db)
        .await
        .unwrap()
        .query("SELECT count(*) FROM scan_import_facts", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(operations, 0);
    assert!(folder.join("Harbor.S01E01.1080p.WEB-DL.flv").exists());
}

#[tokio::test]
async fn exhausted_unpublished_attempts_fail_and_cancel_wins_over_recovery() {
    let (db, scratch) = seeded().await;
    let folder = release(&scratch);
    let id = Uuid::new_v4();
    enqueue(&db, MediaDomain::Tv, input(id, &folder, ScanMode::Copy))
        .await
        .unwrap();
    let c = connection(&db).await.unwrap();
    for attempt in 1..=3 {
        actual_claim(&db, id).await.unwrap();
        assert_eq!(snapshot(&db, id).await.attempts, attempt);
        recover(&c, now().unwrap(), "interrupted").await.unwrap();
    }
    let parent = snapshot(&db, id).await;
    assert!(matches!(parent.status, CommandStatus::Failed));
    assert_eq!(parent.error_code.as_deref(), Some("interrupted"));
    assert_eq!(child_count(&db, id).await, 0);
    // A recorded cancel request is honored by recovery instead of retrying.
    let other = Uuid::new_v4();
    enqueue(&db, MediaDomain::Tv, input(other, &folder, ScanMode::Copy))
        .await
        .unwrap();
    actual_claim(&db, other).await.unwrap();
    let _ = cancel(
        State(db.clone()),
        MediaDomain::Tv,
        Path(other.to_string()),
        Ok(Query(Empty {})),
    )
    .await
    .unwrap();
    assert!(snapshot(&db, other).await.cancel_requested);
    recover(&c, now().unwrap(), "interrupted").await.unwrap();
    assert!(matches!(
        snapshot(&db, other).await.status,
        CommandStatus::Cancelled
    ));
}

#[tokio::test]
async fn publication_from_a_superseded_generation_is_rejected_without_writes() {
    let (db, scratch) = seeded().await;
    let folder = release(&scratch);
    let id = Uuid::new_v4();
    enqueue(&db, MediaDomain::Tv, input(id, &folder, ScanMode::Auto))
        .await
        .unwrap();
    let old = actual_claim(&db, id).await.unwrap();
    let stale = discover(&db, &old).await.ok().unwrap();
    // The worker is interrupted and a replacement claim supersedes the first generation.
    let c = connection(&db).await.unwrap();
    recover(&c, now().unwrap(), "interrupted").await.unwrap();
    let current = actual_claim(&db, id).await.unwrap();
    assert_eq!(current.run_generation, old.run_generation + 1);
    assert!(matches!(
        publish(&db, &old, stale).await,
        Err(Failure::Stale)
    ));
    assert_eq!(child_count(&db, id).await, 0);
    assert_eq!(snapshot(&db, id).await.revision, 0);
    // The stale settlement is also inert: the current claim remains running.
    settle(&db, &old, Failure::Retry("scan_storage_error"))
        .await
        .unwrap();
    assert!(matches!(
        snapshot(&db, id).await.status,
        CommandStatus::Running
    ));
    // The current generation publishes normally.
    let fresh = discover(&db, &current).await.ok().unwrap();
    let published = publish(&db, &current, fresh).await.ok().unwrap();
    assert_eq!((published.children, published.selected), (2, 1));
}

#[tokio::test]
async fn cancel_requested_before_publication_fences_it_and_settles_cancelled() {
    let (db, scratch) = seeded().await;
    let folder = release(&scratch);
    let id = Uuid::new_v4();
    enqueue(&db, MediaDomain::Tv, input(id, &folder, ScanMode::Auto))
        .await
        .unwrap();
    let claimed = actual_claim(&db, id).await.unwrap();
    let found = discover(&db, &claimed).await.ok().unwrap();
    // Cancel arrives while the complete inventory is only in memory.
    let Json(cancelled) = cancel(
        State(db.clone()),
        MediaDomain::Tv,
        Path(id.to_string()),
        Ok(Query(Empty {})),
    )
    .await
    .unwrap();
    assert!(cancelled.cancel_requested && matches!(cancelled.status, CommandStatus::Running));
    assert!(matches!(
        publish(&db, &claimed, found).await,
        Err(Failure::Cancelled)
    ));
    assert_eq!(child_count(&db, id).await, 0);
    settle(&db, &claimed, Failure::Cancelled).await.unwrap();
    let parent = snapshot(&db, id).await;
    assert!(matches!(parent.status, CommandStatus::Cancelled));
    assert_eq!(
        (parent.revision, parent.error_code, parent.outcome),
        (0, None, None)
    );
}

#[tokio::test]
async fn cancel_before_claim_never_runs_discovery() {
    let (db, scratch) = seeded().await;
    let folder = release(&scratch);
    let id = Uuid::new_v4();
    enqueue(&db, MediaDomain::Movies, input(id, &folder, ScanMode::Auto))
        .await
        .unwrap();
    for _ in 0..2 {
        let Json(done) = cancel(
            State(db.clone()),
            MediaDomain::Movies,
            Path(id.to_string()),
            Ok(Query(Empty {})),
        )
        .await
        .unwrap();
        assert!(matches!(done.status, CommandStatus::Cancelled) && done.cancel_requested);
        assert_eq!((done.attempts, done.run_generation), (0, 0));
    }
    assert!(actual_claim(&db, id).await.is_none());
    // The other domain cannot observe or cancel it.
    assert!(matches!(
        cancel(
            State(db.clone()),
            MediaDomain::Tv,
            Path(id.to_string()),
            Ok(Query(Empty {}))
        )
        .await,
        Err(Error(StatusCode::NOT_FOUND, "downloaded_scan_not_found"))
    ));
}

#[tokio::test]
async fn a_claimed_cancel_requested_retry_is_cancelled_instead_of_run() {
    let (db, scratch) = seeded().await;
    let folder = release(&scratch);
    let id = Uuid::new_v4();
    enqueue(&db, MediaDomain::Tv, input(id, &folder, ScanMode::Auto))
        .await
        .unwrap();
    let c = connection(&db).await.unwrap();
    // A cancel request recorded on a queued row by a path other than the API still wins.
    c.execute(
        "UPDATE downloaded_scan_commands SET cancel_requested=1 WHERE id=?",
        [id.to_string()],
    )
    .await
    .unwrap();
    assert!(actual_claim(&db, id).await.is_none());
    let parent = snapshot(&db, id).await;
    assert!(matches!(parent.status, CommandStatus::Cancelled));
    assert_eq!(parent.attempts, 0);
}

#[tokio::test]
async fn run_publishes_selected_children_and_leaves_the_parent_awaiting_import() {
    let (db, scratch) = seeded().await;
    let folder = release(&scratch);
    let id = Uuid::new_v4();
    enqueue(&db, MediaDomain::Tv, input(id, &folder, ScanMode::Auto))
        .await
        .unwrap();
    run(&db, actual_claim(&db, id).await.unwrap())
        .await
        .unwrap();
    let parent = snapshot(&db, id).await;
    assert!(matches!(parent.status, CommandStatus::Running));
    assert_eq!((parent.revision, parent.attempts), (1, 1));
    // Unpublished new files never join the immutable inventory.
    std::fs::write(folder.join("Harbor.S01E02.1080p.WEB-DL.flv"), wav()).unwrap();
    let c = connection(&db).await.unwrap();
    recover(&c, now().unwrap(), "interrupted").await.unwrap();
    assert_eq!(child_count(&db, id).await, 2);
    let Json(page) = children(
        State(db.clone()),
        MediaDomain::Tv,
        Path(id.to_string()),
        Ok(Query(DownloadedScanChildQuery {
            disposition: Some(ScanDisposition::Ready),
            limit: 25,
            offset: 0,
        })),
    )
    .await
    .unwrap();
    assert_eq!(page.total, 1);
    let ready = &page.items[0];
    assert_eq!(
        (ready.series_id, ready.movie_id, ready.member_count),
        (Some(1), None, 1)
    );
    assert!(matches!(ready.resolved_mode, Some(ScanResolvedMode::Move)));
    assert_eq!(
        ready.destination.as_deref(),
        Some("/fictional/tv/Harbor.S01E01.1080p.WEB-DL.flv")
    );
    // Nothing was transferred.
    assert!(folder.join("Harbor.S01E01.1080p.WEB-DL.flv").exists());
}

#[tokio::test]
async fn retryable_failures_back_off_within_three_attempts_then_fail_terminally() {
    let (db, scratch) = seeded().await;
    let folder = release(&scratch);
    let id = Uuid::new_v4();
    enqueue(&db, MediaDomain::Tv, input(id, &folder, ScanMode::Auto))
        .await
        .unwrap();
    for attempt in 1..=3 {
        let claimed = actual_claim(&db, id).await.unwrap();
        assert_eq!(claimed.attempts, attempt);
        let before = now().unwrap();
        settle(&db, &claimed, Failure::Retry("scan_source_changed"))
            .await
            .unwrap();
        let after = now().unwrap();
        let parent = snapshot(&db, id).await;
        assert_eq!(parent.error_code.as_deref(), Some("scan_source_changed"));
        if attempt < 3 {
            assert!(matches!(parent.status, CommandStatus::RetryWait));
            // Reasoning: the delay is 2^attempt plus a 0..=2 s jitter, added to the clock read
            // inside `settle`. Comparing against a wall-clock read taken after more asynchronous
            // work (the former `> now()`) fails whenever the host stalls for the delay; bounding
            // the stored time by reads taken before and after `settle` keeps the backoff contract
            // (at least 2^attempt seconds ahead, at most jitter beyond the latest read) without
            // depending on how long the test itself took.
            let delay = 1i64 << attempt;
            assert!(
                (before + delay..=after + delay + 2).contains(&parent.next_attempt_at),
                "attempt {attempt}: next_attempt_at {} outside [{}, {}]",
                parent.next_attempt_at,
                before + delay,
                after + delay + 2
            );
            assert!(parent.completed_at.is_none());
        } else {
            assert!(matches!(parent.status, CommandStatus::Failed));
            assert!(parent.completed_at.is_some());
        }
    }
    assert_eq!(child_count(&db, id).await, 0);
    // A terminal parent cannot be claimed, cancelled into another state, or re-run.
    assert!(actual_claim(&db, id).await.is_none());
    let Json(done) = cancel(
        State(db.clone()),
        MediaDomain::Tv,
        Path(id.to_string()),
        Ok(Query(Empty {})),
    )
    .await
    .unwrap();
    assert!(matches!(done.status, CommandStatus::Failed) && !done.cancel_requested);
}

#[tokio::test]
async fn nothing_to_import_settles_succeeded_unsuccessful_and_ready_children_cannot_regress() {
    let (db, scratch) = seeded().await;
    let folder = scratch.0.join("Harbor.S01E01.1080p.WEB-DL");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join("readme.nfo"), b"text").unwrap();
    let id = Uuid::new_v4();
    enqueue(&db, MediaDomain::Tv, input(id, &folder, ScanMode::Auto))
        .await
        .unwrap();
    run(&db, actual_claim(&db, id).await.unwrap())
        .await
        .unwrap();
    let parent = snapshot(&db, id).await;
    assert!(matches!(parent.status, CommandStatus::Succeeded));
    assert_eq!(parent.outcome.as_deref(), Some("unsuccessful"));
    assert_eq!((parent.revision, parent.attempts), (1, 1));
    assert_eq!(child_count(&db, id).await, 1);
    // The terminal proof is immutable at the storage layer.
    let c = connection(&db).await.unwrap();
    assert!(
        c.execute(
            "UPDATE downloaded_scan_commands SET status='running' WHERE id=?",
            [id.to_string()]
        )
        .await
        .is_err()
    );
}

async fn run_scan(db: &Arc<Database>, media: MediaDomain, path: &std::path::Path) -> Uuid {
    let id = Uuid::new_v4();
    enqueue(db, media, input(id, path, ScanMode::Auto))
        .await
        .unwrap();
    run(db, actual_claim(db, id).await.unwrap()).await.unwrap();
    id
}
async fn dump(db: &Database, sql: &str, id: Uuid) -> Vec<Vec<Option<i64>>> {
    let c = connection(db).await.unwrap();
    let mut rows = c.query(sql, [id.hyphenated().to_string()]).await.unwrap();
    let mut out = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        out.push(
            (0..r.column_count())
                .map(|i| r.get::<Option<i64>>(i).unwrap())
                .collect(),
        );
    }
    out
}

#[tokio::test]
async fn upgrade_candidates_freeze_every_member_original_and_the_movie_original() {
    let (db, scratch) = seeded().await;
    db.connect().await.unwrap().execute_batch("INSERT INTO episode_files(id,series_id,path)VALUES(1,1,'/fictional/tv/old.mkv');UPDATE episodes SET episode_file_id=1 WHERE id IN(1,2);INSERT INTO file_metadata(media_type,episode_file_id,quality_id,revision_json)VALUES('tv',1,3,'{\"version\":1,\"real\":0,\"is_repack\":false}');INSERT INTO movie_files(id,movie_id,path)VALUES(1,1,'/fictional/movie/old.mkv');INSERT INTO file_metadata(media_type,movie_file_id,quality_id,revision_json)VALUES('movies',1,3,'{\"version\":1,\"real\":0,\"is_repack\":false}');").await.unwrap();
    let tv = scratch.0.join("Harbor.S01E01-E02.1080p.WEB-DL");
    std::fs::create_dir(&tv).unwrap();
    std::fs::write(tv.join("Harbor.S01E01-E02.1080p.WEB-DL.flv"), wav()).unwrap();
    let id = run_scan(&db, MediaDomain::Tv, &tv).await;
    // One physical file owns both members; both frozen originals reference the shared old file.
    let members = dump(&db, "SELECT episode_id,expected_old_episode_file_id FROM downloaded_scan_child_episodes WHERE command_id=? ORDER BY episode_id", id).await;
    assert_eq!(
        members,
        vec![vec![Some(1), Some(1)], vec![Some(2), Some(1)]]
    );
    let Json(page) = children(
        State(db.clone()),
        MediaDomain::Tv,
        Path(id.to_string()),
        Ok(Query(DownloadedScanChildQuery {
            disposition: Some(ScanDisposition::Ready),
            limit: 25,
            offset: 0,
        })),
    )
    .await
    .unwrap();
    assert_eq!(
        (
            page.total,
            page.items[0].member_count,
            page.items[0].anchor_episode_id
        ),
        (1, 2, Some(1))
    );
    let movie = scratch.0.join("Harbor.2020.1080p.WEB-DL");
    std::fs::create_dir(&movie).unwrap();
    std::fs::write(movie.join("Harbor.2020.1080p.WEB-DL.flv"), wav()).unwrap();
    let id = run_scan(&db, MediaDomain::Movies, &movie).await;
    let old = dump(&db, "SELECT expected_old_movie_file_id,movie_id,series_id FROM downloaded_scan_children WHERE command_id=? AND selected=1", id).await;
    assert_eq!(old, vec![vec![Some(1), Some(1), None]]);
}

#[tokio::test]
async fn hardlink_aliases_and_multi_owner_folders_never_publish_partial_authority() {
    let (db, scratch) = seeded().await;
    let tv = scratch.0.join("Harbor.S01.1080p.WEB-DL");
    std::fs::create_dir(&tv).unwrap();
    std::fs::write(tv.join("Harbor.S01E01.1080p.WEB-DL.flv"), wav()).unwrap();
    // Same inode under a name for a different episode: one physical file, one selection.
    std::fs::hard_link(
        tv.join("Harbor.S01E01.1080p.WEB-DL.flv"),
        tv.join("Harbor.S01E02.1080p.WEB-DL.flv"),
    )
    .unwrap();
    let id = run_scan(&db, MediaDomain::Tv, &tv).await;
    let Json(page) = children(
        State(db.clone()),
        MediaDomain::Tv,
        Path(id.to_string()),
        Ok(Query(DownloadedScanChildQuery {
            disposition: None,
            limit: 25,
            offset: 0,
        })),
    )
    .await
    .unwrap();
    let ready: Vec<_> = page.items.iter().filter(|c| c.selected).collect();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].relative_path, "Harbor.S01E01.1080p.WEB-DL.flv");
    let alias = page
        .items
        .iter()
        .find(|c| c.relative_path == "Harbor.S01E02.1080p.WEB-DL.flv")
        .unwrap();
    assert_eq!(alias.reasons, vec!["duplicate_source_identity".to_string()]);
    // Two library movies in one named folder are ambiguous: fail visibly, publish nothing.
    db.connect().await.unwrap().execute_batch("INSERT INTO movie_metadata(id,title,year,runtime,original_language)VALUES(2,'Orbit',2021,100,1);INSERT INTO movies(id,metadata_id,path)VALUES(2,2,'/fictional/orbit');INSERT INTO library_settings(media_type,movie_id,quality_profile_id,minimum_availability)VALUES('movies',2,2,'released');").await.unwrap();
    let movies = scratch.0.join("1080p.WEB-DL");
    std::fs::create_dir(&movies).unwrap();
    std::fs::write(movies.join("Harbor.2020.1080p.WEB-DL.flv"), wav()).unwrap();
    std::fs::write(movies.join("Orbit.2021.1080p.WEB-DL.flv"), wav()).unwrap();
    let id = Uuid::new_v4();
    enqueue(&db, MediaDomain::Movies, input(id, &movies, ScanMode::Auto))
        .await
        .unwrap();
    run(&db, actual_claim(&db, id).await.unwrap())
        .await
        .unwrap();
    let parent = snapshot(&db, id).await;
    assert!(matches!(parent.status, CommandStatus::Failed));
    assert_eq!(
        (
            parent.error_code.as_deref(),
            parent.revision,
            parent.attempts
        ),
        (Some("scan_multiple_owners"), 0, 1)
    );
    assert_eq!(child_count(&db, id).await, 0);
}

#[tokio::test]
async fn cancel_observed_during_discovery_stops_before_any_publication() {
    let (db, scratch) = seeded().await;
    let folder = release(&scratch);
    let id = Uuid::new_v4();
    enqueue(&db, MediaDomain::Tv, input(id, &folder, ScanMode::Auto))
        .await
        .unwrap();
    let claimed = actual_claim(&db, id).await.unwrap();
    let _ = cancel(
        State(db.clone()),
        MediaDomain::Tv,
        Path(id.to_string()),
        Ok(Query(Empty {})),
    )
    .await
    .unwrap();
    assert!(matches!(
        discover(&db, &claimed).await,
        Err(Failure::Cancelled)
    ));
    run(&db, claimed).await.unwrap();
    assert!(matches!(
        snapshot(&db, id).await.status,
        CommandStatus::Cancelled
    ));
    assert_eq!(child_count(&db, id).await, 0);
}

#[tokio::test]
async fn completed_history_for_an_exact_source_path_is_historical_coverage_not_a_new_import() {
    let (db, scratch) = seeded().await;
    let movie = scratch.0.join("Harbor.2020.1080p.WEB-DL");
    std::fs::create_dir(&movie).unwrap();
    let source = movie.join("Harbor.2020.1080p.WEB-DL.flv");
    std::fs::write(&source, wav()).unwrap();
    let source = source.to_str().unwrap().to_owned();
    let destination = "/fictional/movie/Harbor.2020.1080p.WEB-DL.flv";
    let c = db.connect().await.unwrap();
    c.execute_batch(&format!("INSERT INTO movie_files(id,movie_id,path) VALUES(2,1,'{destination}');INSERT INTO operations VALUES('done','movie',NULL,1,'{source}','copy','{destination}','complete','kept');INSERT INTO import_journal(operation_id,plan_json,stage_json,phase) VALUES('done','{{}}','{{}}','staging');INSERT INTO import_history(operation_id,media_type,movie_id,movie_file_id,source,destination,size,sha256) VALUES('done','movie',1,2,'{source}','{destination}',1,'{}');UPDATE import_journal SET phase='complete' WHERE operation_id='done';", "a".repeat(64))).await.unwrap();
    drop(c);
    let id = run_scan(&db, MediaDomain::Movies, &movie).await;
    let parent = snapshot(&db, id).await;
    assert!(matches!(parent.status, CommandStatus::Succeeded));
    assert_eq!(parent.outcome.as_deref(), Some("unsuccessful"));
    let Json(page) = children(
        State(db.clone()),
        MediaDomain::Movies,
        Path(id.to_string()),
        Ok(Query(DownloadedScanChildQuery {
            disposition: Some(ScanDisposition::AlreadyImported),
            limit: 25,
            offset: 0,
        })),
    )
    .await
    .unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].reasons, vec!["already_imported".to_string()]);
    assert!(!page.items[0].selected);
}

#[tokio::test]
async fn a_known_tracked_receipt_is_captured_exactly_once_and_never_degrades_to_a_fallback() {
    let (db, scratch) = seeded().await;
    let c = db.connect().await.unwrap();
    let (indexer, client) = (Uuid::new_v4().to_string(), Uuid::new_v4().to_string());
    for (id, implementation) in [(&indexer, "torznab"), (&client, "qbittorrent")] {
        c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,?,?,1,1,1,1,'http://fixture.invalid')", params![id.clone(), implementation, implementation]).await.unwrap();
    }
    c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year) VALUES(?,'torznab','tv','[5000]','[]',0,NULL)", [indexer.clone()]).await.unwrap();
    c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags) VALUES(?,'qbittorrent','tv','tv',0,0,'started','default',0,0,0)", [client.clone()]).await.unwrap();
    let (command, candidate, hash) = (
        Uuid::new_v4().to_string(),
        Uuid::new_v4().to_string(),
        "a".repeat(40),
    );
    c.execute("INSERT INTO rss_commands(id,name,media_type,indexer_id,indexer_revision,client_id,client_revision,next_attempt_at,created_at) VALUES(?,'rss_sync','tv',?,1,?,1,100,100)", params![command.clone(), indexer.clone(), client.clone()]).await.unwrap();
    c.execute("INSERT INTO rss_candidates(id,command_id,media_type,indexer_id,indexer_revision,client_id,client_revision,fingerprint,title,private_payload,series_id,status,decision_reasons_json,created_at,updated_at) SELECT ?,id,media_type,indexer_id,indexer_revision,client_id,client_revision,?,'Harbor.S01E01.1080p.WEB-DL',?,1,'pending','[]',100,100 FROM rss_commands WHERE id=?", params![candidate.clone(), "a".repeat(64), vec![1u8; 29], command]).await.unwrap();
    c.execute(
        "INSERT INTO rss_candidate_episodes VALUES(?,1,1)",
        [candidate.clone()],
    )
    .await
    .unwrap();
    let identity = serde_json::json!({"version":1,"target":{"media_type":"episode","id":1},"hashes":[hash],"settings_fingerprint":"c".repeat(64),"payload_sha256":"d".repeat(64)}).to_string();
    c.execute(
        "UPDATE rss_candidates SET status='prepared',submission_identity_json=? WHERE id=?",
        params![identity, candidate.clone()],
    )
    .await
    .unwrap();
    c.execute(
        "INSERT INTO rss_hash_claims VALUES(?,?,?)",
        params![client.clone(), hash.clone(), candidate.clone()],
    )
    .await
    .unwrap();
    c.execute(
        "UPDATE rss_candidates SET status='submitting',private_payload=NULL WHERE id=?",
        [candidate.clone()],
    )
    .await
    .unwrap();
    c.execute(
        "UPDATE rss_candidates SET status='observed',observed_hash=? WHERE id=?",
        params![hash.clone(), candidate.clone()],
    )
    .await
    .unwrap();
    drop(c);
    let folder = release(&scratch);
    let tracked = |remote: &str| {
        let mut request = input(Uuid::new_v4(), &folder, ScanMode::Auto);
        request.tracked = Some(ScanTracked {
            provider_id: Uuid::parse_str(&client).unwrap(),
            remote_id: remote.into(),
        });
        request
    };
    // Known receipt: captured and reserved in the publication transaction (no processing row
    // is fabricated), never degraded into an ordinary fallback scan.
    let request = tracked(&hash);
    let id = request.id;
    enqueue(&db, MediaDomain::Tv, request).await.unwrap();
    run(&db, actual_claim(&db, id).await.unwrap())
        .await
        .unwrap();
    let parent = snapshot(&db, id).await;
    assert!(matches!(parent.status, CommandStatus::Running));
    assert_eq!(
        (
            parent.revision,
            parent.attempts,
            parent.candidate.as_deref()
        ),
        (1, 1, Some(candidate.as_str()))
    );
    assert_eq!(parent.coverage_state, "pending");
    assert_eq!(child_count(&db, id).await, 2);
    let c = db.connect().await.unwrap();
    let absent: i64 = c
        .query("SELECT count(*) FROM download_processing", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(absent, 0, "absent processing stays absent");
    drop(c);
    // A second tracked scan of the same receipt cannot take it over.
    let request = tracked(&hash);
    let second = request.id;
    enqueue(&db, MediaDomain::Tv, request).await.unwrap();
    run(&db, actual_claim(&db, second).await.unwrap())
        .await
        .unwrap();
    let parent = snapshot(&db, second).await;
    assert!(matches!(parent.status, CommandStatus::Failed));
    assert_eq!(
        parent.error_code.as_deref(),
        Some("scan_tracked_receipt_reserved")
    );
    // The same hash in the other domain is not this receipt (the provider has no movie scope).
    assert!(matches!(
        enqueue(&db, MediaDomain::Movies, tracked(&hash)).await,
        Err(Error(
            StatusCode::CONFLICT,
            "downloaded_scan_provider_unknown"
        ))
    ));
    // Unknown remote falls back visibly to ordinary identification. (Episode 1 belongs to the
    // observed receipt above, so the fallback scan uses an unrelated episode.)
    let other = scratch.0.join("Harbor.S01E02.1080p.WEB-DL");
    std::fs::create_dir(&other).unwrap();
    std::fs::write(other.join("Harbor.S01E02.1080p.WEB-DL.flv"), wav()).unwrap();
    let mut request = tracked(&"b".repeat(40));
    request.path = other.to_str().unwrap().into();
    let id = request.id;
    enqueue(&db, MediaDomain::Tv, request).await.unwrap();
    run(&db, actual_claim(&db, id).await.unwrap())
        .await
        .unwrap();
    let parent = snapshot(&db, id).await;
    assert!(matches!(parent.status, CommandStatus::Running));
    assert_eq!(parent.revision, 1);
}
