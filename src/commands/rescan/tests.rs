use super::*;

struct Scratch(std::path::PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("hrrdarr-rescan-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn dir(&self, p: &str) -> String {
        let full = self.0.join(p);
        std::fs::create_dir_all(&full).unwrap();
        full.to_str().unwrap().to_owned()
    }
    fn path(&self, p: &str) -> String {
        self.0.join(p).to_str().unwrap().to_owned()
    }
    async fn database(&self) -> Arc<Database> {
        Arc::new(Database::open_local(self.0.join("db")).await.unwrap())
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn write_file(path: &str, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
}
fn input(target_id: Option<i64>) -> RescanInput {
    RescanInput {
        target_id,
        priority: CommandPriority::Normal,
    }
}
/// Admits a minimal but schema-valid `rss_candidate_imports` row that claims `old_episode_file_id`
/// as the file `operation_id` is about to replace -- walking `rss_candidates` through its full
/// pending->prepared->submitting->observed state machine (0025) since `rss_candidate_admit` only
/// allows inserting at `pending`/`rejected`. `episode.episode_file_id` must already equal
/// `old_episode_file_id` and that file's current path must equal `old_file_json.path`, per
/// `candidate_import_admit` (0026).
async fn admit_pending_replacement(
    c: &Connection,
    series_id: i64,
    episode_id: i64,
    old_episode_file_id: i64,
    old_path: &str,
    operation_id: &str,
    destination: &str,
) {
    let indexer = Uuid::new_v4().to_string();
    let client = Uuid::new_v4().to_string();
    c.execute_batch(&format!(
        "INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES('{indexer}','torznab','Indexer',1,1,1,1,'http://indexer');
         INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search) VALUES('{indexer}','torznab','tv','[5000]','[]',0);
         INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES('{client}','qbittorrent','Client',1,1,1,1,'http://client');
         INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags) VALUES('{client}','qbittorrent','tv','tv',0,1,'started','default',0,0,0);"
    ))
    .await
    .unwrap();
    let command_id = Uuid::new_v4().to_string();
    c.execute(
        "INSERT INTO rss_commands(id,name,media_type,status,next_attempt_at,created_at,indexer_id,indexer_revision,client_id,client_revision) VALUES(?,'rss_sync','tv','queued',0,0,?,1,?,1)",
        libsql::params![command_id.clone(), indexer.clone(), client.clone()],
    )
    .await
    .unwrap();
    let candidate_id = Uuid::new_v4().to_string();
    let fingerprint = "a".repeat(64);
    let payload = vec![0u8; 32];
    c.execute(
        "INSERT INTO rss_candidates(id,command_id,media_type,indexer_id,indexer_revision,client_id,client_revision,fingerprint,title,private_payload,series_id,status,decision_reasons_json,created_at,updated_at) VALUES(?,?,'tv',?,1,?,1,?,'Release',?,?,'pending','[]',0,0)",
        libsql::params![
            candidate_id.clone(),
            command_id,
            indexer,
            client.clone(),
            fingerprint,
            payload,
            series_id
        ],
    )
    .await
    .unwrap();
    c.execute(
        "INSERT INTO rss_candidate_episodes(candidate_id,series_id,episode_id) VALUES(?,?,?)",
        libsql::params![candidate_id.clone(), series_id, episode_id],
    )
    .await
    .unwrap();
    let hash = "b".repeat(40);
    let identity = serde_json::json!({
        "version": 1,
        "target": {"media_type": "episode", "id": episode_id},
        "hashes": [hash.clone()],
        "settings_fingerprint": "c".repeat(64),
        "payload_sha256": "d".repeat(64),
    })
    .to_string();
    c.execute(
        "UPDATE rss_candidates SET status='prepared',submission_identity_json=? WHERE id=?",
        libsql::params![identity, candidate_id.clone()],
    )
    .await
    .unwrap();
    c.execute(
        "INSERT INTO rss_hash_claims(client_id,hash,candidate_id) VALUES(?,?,?)",
        libsql::params![client, hash.clone(), candidate_id.clone()],
    )
    .await
    .unwrap();
    c.execute(
        "UPDATE rss_candidates SET status='submitting',private_payload=NULL WHERE id=?",
        [candidate_id.clone()],
    )
    .await
    .unwrap();
    c.execute(
        "UPDATE rss_candidates SET status='observed',observed_hash=? WHERE id=?",
        libsql::params![hash, candidate_id.clone()],
    )
    .await
    .unwrap();
    c.execute(
        &format!(
            "INSERT INTO operations(id,media_type,episode_id,source,mode,destination,status,message) VALUES(?,'episode',?,'/download/new.mkv','copy','{destination}','preview','fixture')"
        ),
        libsql::params![operation_id, episode_id],
    )
    .await
    .unwrap();
    c.execute(
        "INSERT INTO import_journal(operation_id,plan_json,phase) VALUES(?,'{}','preview')",
        [operation_id],
    )
    .await
    .unwrap();
    let old_file_json = serde_json::json!({
        "version": 1,
        "file_id": old_episode_file_id,
        "path": old_path,
        "metadata": null,
        "parent": {"dev":1,"ino":2,"size":0,"mtime":0,"mtime_ns":0,"ctime":0,"ctime_ns":0},
        "file": {"dev":1,"ino":3,"size":1,"mtime":0,"mtime_ns":0,"ctime":0,"ctime_ns":0},
        "quarantine_name": format!(".hrrdarr-replaced-{operation_id}"),
    })
    .to_string();
    c.execute(
        "INSERT INTO rss_candidate_imports(candidate_id,operation_id,quality_id,revision_json,provenance_json,old_episode_file_id,old_file_json) VALUES(?,?,3,?,'{}',?,?)",
        libsql::params![
            candidate_id,
            operation_id,
            r#"{"version":1,"real":0,"is_repack":false}"#,
            old_episode_file_id,
            old_file_json
        ],
    )
    .await
    .unwrap();
}
async fn actual_claim(db: &Database, id: Uuid) -> RescanCommand {
    let c = connection(db).await.unwrap();
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    let command = claim(&tx, id, now().unwrap()).await.unwrap().unwrap();
    tx.commit().await.unwrap();
    command
}

#[tokio::test]
async fn tv_adopt_in_place_happy_path() {
    let scratch = Scratch::new();
    let series_path = scratch.dir("tv/Show");
    let db = scratch.database().await;
    let c = connection(&db).await.unwrap();
    c.execute_batch(&format!(
        "INSERT INTO series(id,title,path) VALUES(1,'Show','{series_path}');
         INSERT INTO seasons(series_id,number) VALUES(1,1);
         INSERT INTO episodes(id,series_id,season,number,title) VALUES(1,1,1,1,'Pilot');"
    ))
    .await
    .unwrap();
    write_file(
        &format!("{series_path}/Show.S01E01.1080p.WEB-DL.mkv"),
        b"episode-bytes",
    );
    let created = create(
        State(db.clone()),
        MediaDomain::Tv,
        Ok(Query(Empty {})),
        Ok(Json(input(Some(1)))),
    )
    .await
    .unwrap()
    .1
    .0;
    assert_eq!(created.commands.len(), 1);
    let claimed = actual_claim(&db, created.commands[0].id).await;
    run(&db, claimed).await.unwrap();
    let done = read(&c, created.commands[0].id).await.unwrap();
    assert!(matches!(done.status, RescanStatus::Succeeded));
    assert_eq!(done.files_adopted, Some(1));
    assert_eq!(done.files_removed, Some(0));
    let row = c
        .query(
            "SELECT f.path,m.quality_id FROM episodes e JOIN episode_files f ON f.id=e.episode_file_id JOIN file_metadata m ON m.episode_file_id=f.id WHERE e.id=1",
            (),
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap();
    let path: String = row.get(0).unwrap();
    assert_eq!(path, format!("{series_path}/Show.S01E01.1080p.WEB-DL.mkv"));
    assert_eq!(row.get::<i64>(1).unwrap(), 3); // WEBDL-1080p
}

#[tokio::test]
async fn movies_adopt_in_place_happy_path() {
    let scratch = Scratch::new();
    let movie_path = scratch.dir("movies/Movie (2020)");
    let db = scratch.database().await;
    let c = connection(&db).await.unwrap();
    c.execute_batch(&format!(
        "INSERT INTO movie_metadata(id,title,year) VALUES(1,'Movie',2020);
         INSERT INTO movies(id,metadata_id,path) VALUES(1,1,'{movie_path}');"
    ))
    .await
    .unwrap();
    write_file(
        &format!("{movie_path}/Movie.2020.1080p.WEB-DL.mkv"),
        b"movie-bytes",
    );
    let created = create(
        State(db.clone()),
        MediaDomain::Movies,
        Ok(Query(Empty {})),
        Ok(Json(input(Some(1)))),
    )
    .await
    .unwrap()
    .1
    .0;
    let claimed = actual_claim(&db, created.commands[0].id).await;
    run(&db, claimed).await.unwrap();
    let done = read(&c, created.commands[0].id).await.unwrap();
    assert!(matches!(done.status, RescanStatus::Succeeded));
    assert_eq!(done.files_adopted, Some(1));
    let row = c
        .query(
            "SELECT f.path,m.quality_id FROM movie_files f JOIN file_metadata m ON m.movie_file_id=f.id WHERE f.movie_id=1",
            (),
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap();
    let path: String = row.get(0).unwrap();
    assert_eq!(path, format!("{movie_path}/Movie.2020.1080p.WEB-DL.mkv"));
    assert_eq!(row.get::<i64>(1).unwrap(), 3);
}

#[tokio::test]
async fn root_missing_and_root_empty_skip_with_zero_db_changes() {
    let scratch = Scratch::new();
    let db = scratch.database().await;
    let c = connection(&db).await.unwrap();
    // Root missing: neither the series folder nor its parent exist on disk.
    c.execute_batch(&format!(
        "INSERT INTO series(id,title,path) VALUES(1,'Gone','{}');
         INSERT INTO seasons(series_id,number) VALUES(1,1);
         INSERT INTO episodes(id,series_id,season,number,title) VALUES(1,1,1,1,'Pilot');",
        scratch.path("unmounted/Show")
    ))
    .await
    .unwrap();
    let created = create(
        State(db.clone()),
        MediaDomain::Tv,
        Ok(Query(Empty {})),
        Ok(Json(input(Some(1)))),
    )
    .await
    .unwrap()
    .1
    .0;
    let claimed = actual_claim(&db, created.commands[0].id).await;
    run(&db, claimed).await.unwrap();
    let done = read(&c, created.commands[0].id).await.unwrap();
    assert!(matches!(done.status, RescanStatus::Skipped));
    assert_eq!(done.skip_reason.as_deref(), Some("root_missing"));
    assert_eq!(done.files_adopted, None);
    assert_eq!(done.files_removed, None);
    let episode_files: i64 = c
        .query("SELECT count(*) FROM episode_files", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(episode_files, 0);

    // Root empty: the parent directory exists but has nothing in it, series folder still missing.
    let empty_root = scratch.dir("emptyroot");
    c.execute_batch(&format!(
        "INSERT INTO series(id,title,path) VALUES(2,'Gone2','{empty_root}/Show');
         INSERT INTO seasons(series_id,number) VALUES(2,1);
         INSERT INTO episodes(id,series_id,season,number,title) VALUES(2,2,1,1,'Pilot');"
    ))
    .await
    .unwrap();
    let created = create(
        State(db.clone()),
        MediaDomain::Tv,
        Ok(Query(Empty {})),
        Ok(Json(input(Some(2)))),
    )
    .await
    .unwrap()
    .1
    .0;
    let claimed = actual_claim(&db, created.commands[0].id).await;
    run(&db, claimed).await.unwrap();
    let done = read(&c, created.commands[0].id).await.unwrap();
    assert!(matches!(done.status, RescanStatus::Skipped));
    assert_eq!(done.skip_reason.as_deref(), Some("root_empty"));
    let episode_files: i64 = c
        .query("SELECT count(*) FROM episode_files", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(episode_files, 0);
}

#[tokio::test]
async fn missing_subfolder_with_root_present_is_a_real_deletion() {
    let scratch = Scratch::new();
    let root = scratch.dir("tv");
    // Root is non-empty (holds an unrelated file) but this series' own subfolder is missing.
    write_file(&format!("{root}/keepalive.txt"), b"x");
    let db = scratch.database().await;
    let c = connection(&db).await.unwrap();
    c.execute_batch(&format!(
        "INSERT INTO series(id,title,path) VALUES(1,'Gone','{root}/Show');
         INSERT INTO seasons(series_id,number) VALUES(1,1);
         INSERT INTO episodes(id,series_id,season,number,title) VALUES(1,1,1,1,'Pilot');
         INSERT INTO episode_files(id,series_id,path) VALUES(1,1,'{root}/Show/old.mkv');
         UPDATE episodes SET episode_file_id=1 WHERE id=1;
         INSERT INTO file_metadata(media_type,episode_file_id,size,date_added) VALUES('tv',1,10,'2026-01-01T00:00:00Z');"
    ))
    .await
    .unwrap();
    let created = create(
        State(db.clone()),
        MediaDomain::Tv,
        Ok(Query(Empty {})),
        Ok(Json(input(Some(1)))),
    )
    .await
    .unwrap()
    .1
    .0;
    let claimed = actual_claim(&db, created.commands[0].id).await;
    run(&db, claimed).await.unwrap();
    let done = read(&c, created.commands[0].id).await.unwrap();
    assert!(matches!(done.status, RescanStatus::Succeeded));
    assert_eq!(done.files_adopted, Some(0));
    assert_eq!(done.files_removed, Some(1));
    let file_id: Option<i64> = c
        .query("SELECT episode_file_id FROM episodes WHERE id=1", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(file_id, None);
    let remaining: i64 = c
        .query("SELECT count(*) FROM episode_files", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(remaining, 0);
}

// Movies-domain counterpart of `root_missing_and_root_empty_skip_with_zero_db_changes`: the
// missing-mount failsafe is the single most safety-critical behavior in this command, so it
// needs both-domain evidence, not just the TV case.
#[tokio::test]
async fn root_missing_and_root_empty_skip_with_zero_db_changes_for_movies() {
    let scratch = Scratch::new();
    let db = scratch.database().await;
    let c = connection(&db).await.unwrap();
    // Root missing: neither the movie folder nor its parent exist on disk.
    c.execute_batch(&format!(
        "INSERT INTO movie_metadata(id,title,year) VALUES(1,'Gone',2020);
         INSERT INTO movies(id,metadata_id,path) VALUES(1,1,'{}');",
        scratch.path("unmounted/Movie (2020)")
    ))
    .await
    .unwrap();
    let created = create(
        State(db.clone()),
        MediaDomain::Movies,
        Ok(Query(Empty {})),
        Ok(Json(input(Some(1)))),
    )
    .await
    .unwrap()
    .1
    .0;
    let claimed = actual_claim(&db, created.commands[0].id).await;
    run(&db, claimed).await.unwrap();
    let done = read(&c, created.commands[0].id).await.unwrap();
    assert!(matches!(done.status, RescanStatus::Skipped));
    assert_eq!(done.skip_reason.as_deref(), Some("root_missing"));
    assert_eq!(done.files_adopted, None);
    assert_eq!(done.files_removed, None);
    let movie_files: i64 = c
        .query("SELECT count(*) FROM movie_files", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(movie_files, 0);

    // Root empty: the parent directory exists but has nothing in it, movie folder still missing.
    let empty_root = scratch.dir("emptymoviesroot");
    c.execute_batch(&format!(
        "INSERT INTO movie_metadata(id,title,year) VALUES(2,'Gone2',2021);
         INSERT INTO movies(id,metadata_id,path) VALUES(2,2,'{empty_root}/Movie2 (2021)');"
    ))
    .await
    .unwrap();
    let created = create(
        State(db.clone()),
        MediaDomain::Movies,
        Ok(Query(Empty {})),
        Ok(Json(input(Some(2)))),
    )
    .await
    .unwrap()
    .1
    .0;
    let claimed = actual_claim(&db, created.commands[0].id).await;
    run(&db, claimed).await.unwrap();
    let done = read(&c, created.commands[0].id).await.unwrap();
    assert!(matches!(done.status, RescanStatus::Skipped));
    assert_eq!(done.skip_reason.as_deref(), Some("root_empty"));
    let movie_files: i64 = c
        .query("SELECT count(*) FROM movie_files", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(movie_files, 0);
}

// Movies-domain counterpart of `missing_subfolder_with_root_present_is_a_real_deletion`.
#[tokio::test]
async fn missing_subfolder_with_root_present_is_a_real_deletion_for_movies() {
    let scratch = Scratch::new();
    let root = scratch.dir("movies");
    // Root is non-empty (holds an unrelated file) but this movie's own subfolder is missing.
    write_file(&format!("{root}/keepalive.txt"), b"x");
    let db = scratch.database().await;
    let c = connection(&db).await.unwrap();
    c.execute_batch(&format!(
        "INSERT INTO movie_metadata(id,title,year) VALUES(1,'Gone',2020);
         INSERT INTO movies(id,metadata_id,path) VALUES(1,1,'{root}/Movie (2020)');
         INSERT INTO movie_files(id,movie_id,path) VALUES(1,1,'{root}/Movie (2020)/old.mkv');
         INSERT INTO file_metadata(media_type,movie_file_id,size,date_added) VALUES('movies',1,10,'2026-01-01T00:00:00Z');"
    ))
    .await
    .unwrap();
    let created = create(
        State(db.clone()),
        MediaDomain::Movies,
        Ok(Query(Empty {})),
        Ok(Json(input(Some(1)))),
    )
    .await
    .unwrap()
    .1
    .0;
    let claimed = actual_claim(&db, created.commands[0].id).await;
    run(&db, claimed).await.unwrap();
    let done = read(&c, created.commands[0].id).await.unwrap();
    assert!(matches!(done.status, RescanStatus::Succeeded));
    assert_eq!(done.files_adopted, Some(0));
    assert_eq!(done.files_removed, Some(1));
    let remaining: i64 = c
        .query("SELECT count(*) FROM movie_files", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(remaining, 0);
}

#[tokio::test]
async fn whole_library_fan_out_rejects_the_whole_batch_over_capacity() {
    let scratch = Scratch::new();
    let db = scratch.database().await;
    let c = connection(&db).await.unwrap();
    for id in 1..=3i64 {
        c.execute(
            "INSERT INTO series(id,title,path) VALUES(?,?,?)",
            libsql::params![
                id,
                format!("Show{id}"),
                scratch.path(&format!("tv/Show{id}"))
            ],
        )
        .await
        .unwrap();
    }
    // Fill the shared 1024-row command pool to leave fewer free slots than the fan-out needs.
    // rescan_commands only allows one active row per target (unlike quality_reset_commands,
    // which allows only one active row per *media_type* at all), so each filler needs its own
    // dummy series.
    for id in 1000..2022i64 {
        c.execute(
            "INSERT INTO series(id,title,path) VALUES(?,?,?)",
            libsql::params![id, format!("Filler{id}"), format!("/filler/{id}")],
        )
        .await
        .unwrap();
        c.execute(
            "INSERT INTO rescan_commands(id,media_type,series_id,priority,status,attempts,next_attempt_at,created_at) VALUES(?,'tv',?,0,'queued',0,0,0)",
            libsql::params![Uuid::new_v4().to_string(), id],
        )
        .await
        .unwrap();
    }
    let result = create(
        State(db.clone()),
        MediaDomain::Tv,
        Ok(Query(Empty {})),
        Ok(Json(input(None))),
    )
    .await;
    assert!(matches!(
        result,
        Err(Error(StatusCode::TOO_MANY_REQUESTS, "command_history_full"))
    ));
    // Only the 1022 pre-existing fillers remain; none of the 3 fan-out targets were inserted.
    let rescans: i64 = c
        .query("SELECT count(*) FROM rescan_commands", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(rescans, 1022);
}

#[tokio::test]
async fn cancel_permitted_before_start_conflicts_once_running() {
    let scratch = Scratch::new();
    let db = scratch.database().await;
    let c = connection(&db).await.unwrap();
    c.execute_batch(&format!(
        "INSERT INTO series(id,title,path) VALUES(1,'Show','{}');",
        scratch.path("tv/Show")
    ))
    .await
    .unwrap();
    let created = create(
        State(db.clone()),
        MediaDomain::Tv,
        Ok(Query(Empty {})),
        Ok(Json(input(Some(1)))),
    )
    .await
    .unwrap()
    .1
    .0;
    let id = created.commands[0].id;
    let cancelled = cancel(
        State(db.clone()),
        MediaDomain::Tv,
        Path(id.to_string()),
        Ok(Query(Empty {})),
    )
    .await
    .unwrap()
    .0;
    assert!(matches!(cancelled.status, RescanStatus::Cancelled));

    let created = create(
        State(db.clone()),
        MediaDomain::Tv,
        Ok(Query(Empty {})),
        Ok(Json(input(Some(1)))),
    )
    .await
    .unwrap()
    .1
    .0;
    let id = created.commands[0].id;
    actual_claim(&db, id).await;
    assert!(matches!(
        cancel(
            State(db.clone()),
            MediaDomain::Tv,
            Path(id.to_string()),
            Ok(Query(Empty {}))
        )
        .await,
        Err(Error(StatusCode::CONFLICT, "command_conflict"))
    ));
}

#[tokio::test]
async fn claim_settles_target_changed_when_series_deleted_first() {
    let scratch = Scratch::new();
    let db = scratch.database().await;
    let c = connection(&db).await.unwrap();
    c.execute_batch(&format!(
        "INSERT INTO series(id,title,path) VALUES(1,'Show','{}');",
        scratch.path("tv/Show")
    ))
    .await
    .unwrap();
    let created = create(
        State(db.clone()),
        MediaDomain::Tv,
        Ok(Query(Empty {})),
        Ok(Json(input(Some(1)))),
    )
    .await
    .unwrap()
    .1
    .0;
    let id = created.commands[0].id;
    c.execute("DELETE FROM series WHERE id=1", ())
        .await
        .unwrap();
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    let claimed = claim(&tx, id, now().unwrap()).await.unwrap();
    tx.commit().await.unwrap();
    assert!(claimed.is_none());
    let done = read(&c, id).await.unwrap();
    assert!(matches!(done.status, RescanStatus::Failed));
    assert_eq!(done.error_code.as_deref(), Some("target_changed"));
}

#[tokio::test]
async fn does_not_displace_an_episode_that_already_has_a_different_present_file() {
    let scratch = Scratch::new();
    let series_path = scratch.dir("tv/Show");
    let db = scratch.database().await;
    let c = connection(&db).await.unwrap();
    write_file(&format!("{series_path}/original.mkv"), b"original");
    write_file(
        &format!("{series_path}/Show.S01E01.1080p.WEB-DL.mkv"),
        b"candidate",
    );
    c.execute_batch(&format!(
        "INSERT INTO series(id,title,path) VALUES(1,'Show','{series_path}');
         INSERT INTO seasons(series_id,number) VALUES(1,1);
         INSERT INTO episodes(id,series_id,season,number,title) VALUES(1,1,1,1,'Pilot');
         INSERT INTO episode_files(id,series_id,path) VALUES(1,1,'{series_path}/original.mkv');
         UPDATE episodes SET episode_file_id=1 WHERE id=1;
         INSERT INTO file_metadata(media_type,episode_file_id,size,date_added) VALUES('tv',1,10,'2026-01-01T00:00:00Z');"
    ))
    .await
    .unwrap();
    let created = create(
        State(db.clone()),
        MediaDomain::Tv,
        Ok(Query(Empty {})),
        Ok(Json(input(Some(1)))),
    )
    .await
    .unwrap()
    .1
    .0;
    let claimed = actual_claim(&db, created.commands[0].id).await;
    run(&db, claimed).await.unwrap();
    let done = read(&c, created.commands[0].id).await.unwrap();
    assert!(matches!(done.status, RescanStatus::Succeeded));
    // Neither file was touched: the original stays associated, the candidate stays unmatched.
    assert_eq!(done.files_adopted, Some(0));
    assert_eq!(done.files_removed, Some(0));
    let path: String = c
        .query(
            "SELECT f.path FROM episodes e JOIN episode_files f ON f.id=e.episode_file_id WHERE e.id=1",
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
    assert_eq!(path, format!("{series_path}/original.mkv"));
}

#[tokio::test]
async fn cross_series_path_collision_is_left_unmatched() {
    let scratch = Scratch::new();
    let series_a_path = scratch.dir("tv/ShowA");
    let series_b_path = scratch.dir("tv/ShowB");
    let db = scratch.database().await;
    let c = connection(&db).await.unwrap();
    // A file physically under series B's folder, but its DB row is (erroneously) owned by A.
    let collided = format!("{series_b_path}/Show.S01E01.1080p.WEB-DL.mkv");
    write_file(&collided, b"bytes");
    c.execute_batch(&format!(
        "INSERT INTO series(id,title,path) VALUES(1,'ShowA','{series_a_path}'),(2,'ShowB','{series_b_path}');
         INSERT INTO seasons(series_id,number) VALUES(1,1),(2,1);
         INSERT INTO episodes(id,series_id,season,number,title) VALUES(1,1,1,1,'Pilot A'),(2,2,1,1,'Pilot B');
         INSERT INTO episode_files(id,series_id,path) VALUES(1,1,'{collided}');
         UPDATE episodes SET episode_file_id=1 WHERE id=1;
         INSERT INTO file_metadata(media_type,episode_file_id,size,date_added) VALUES('tv',1,5,'2026-01-01T00:00:00Z');"
    ))
    .await
    .unwrap();
    let created = create(
        State(db.clone()),
        MediaDomain::Tv,
        Ok(Query(Empty {})),
        Ok(Json(input(Some(2)))),
    )
    .await
    .unwrap()
    .1
    .0;
    let claimed = actual_claim(&db, created.commands[0].id).await;
    run(&db, claimed).await.unwrap();
    let done = read(&c, created.commands[0].id).await.unwrap();
    assert!(matches!(done.status, RescanStatus::Succeeded));
    assert_eq!(done.files_adopted, Some(0));
    assert_eq!(done.files_removed, Some(0));
    let owner: i64 = c
        .query("SELECT series_id FROM episode_files WHERE id=1", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(owner, 1); // still owned by series A; series B never stole it
    let b_file: Option<i64> = c
        .query("SELECT episode_file_id FROM episodes WHERE id=2", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(b_file, None);
}

#[tokio::test]
async fn walk_fails_closed_on_depth_cap_leaving_existing_association_untouched() {
    let scratch = Scratch::new();
    let series_path = scratch.dir("tv/Show");
    // Build a chain deeper than MAX_WALK_DEPTH so the walk must abort rather than return a
    // partial (and therefore misleading) file list.
    let mut deep = series_path.clone();
    for i in 0..(MAX_WALK_DEPTH + 4) {
        deep = format!("{deep}/d{i}");
    }
    std::fs::create_dir_all(&deep).unwrap();
    let db = scratch.database().await;
    let c = connection(&db).await.unwrap();
    c.execute_batch(&format!(
        "INSERT INTO series(id,title,path) VALUES(1,'Show','{series_path}');
         INSERT INTO seasons(series_id,number) VALUES(1,1);
         INSERT INTO episodes(id,series_id,season,number,title) VALUES(1,1,1,1,'Pilot');
         INSERT INTO episode_files(id,series_id,path) VALUES(1,1,'{series_path}/kept.mkv');
         UPDATE episodes SET episode_file_id=1 WHERE id=1;
         INSERT INTO file_metadata(media_type,episode_file_id,size,date_added) VALUES('tv',1,5,'2026-01-01T00:00:00Z');"
    ))
    .await
    .unwrap();
    write_file(&format!("{series_path}/kept.mkv"), b"bytes");
    let created = create(
        State(db.clone()),
        MediaDomain::Tv,
        Ok(Query(Empty {})),
        Ok(Json(input(Some(1)))),
    )
    .await
    .unwrap()
    .1
    .0;
    let claimed = actual_claim(&db, created.commands[0].id).await;
    run(&db, claimed).await.unwrap();
    let done = read(&c, created.commands[0].id).await.unwrap();
    assert!(matches!(
        done.status,
        RescanStatus::Failed | RescanStatus::RetryWait
    ));
    assert_eq!(done.error_code.as_deref(), Some("storage_error"));
    // Zero DB changes: cleanup never ran against an incomplete file list.
    let file_id: Option<i64> = c
        .query("SELECT episode_file_id FROM episodes WHERE id=1", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(file_id, Some(1));
    let count: i64 = c
        .query("SELECT count(*) FROM episode_files", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(count, 1);
}

// A file mid an owned-download replacement (admitted, not yet transferred) must survive a
// rescan untouched on both sides of the interaction: cleanup must not delete the "old" file
// record FK-protected by `rss_candidate_imports.old_episode_file_id`, and the walk must not
// wander into its `.hrrdarr-replaced-<operation_id>` quarantine directory and re-adopt whatever
// sits there as a stray new file.
#[tokio::test]
async fn pending_replacement_file_survives_cleanup_and_is_never_readopted() {
    let scratch = Scratch::new();
    let series_path = scratch.dir("tv/Show");
    let db = scratch.database().await;
    let c = connection(&db).await.unwrap();
    let old_path = format!("{series_path}/original.mkv");
    c.execute_batch(&format!(
        "INSERT INTO series(id,title,path) VALUES(1,'Show','{series_path}');
         INSERT INTO seasons(series_id,number) VALUES(1,1);
         INSERT INTO episodes(id,series_id,season,number,title) VALUES(1,1,1,1,'Pilot');
         INSERT INTO episode_files(id,series_id,path) VALUES(1,1,'{old_path}');
         UPDATE episodes SET episode_file_id=1 WHERE id=1;
         INSERT INTO file_metadata(media_type,episode_file_id,size,date_added) VALUES('tv',1,5,'2026-01-01T00:00:00Z');"
    ))
    .await
    .unwrap();
    admit_pending_replacement(
        &c,
        1,
        1,
        1,
        &old_path,
        "op-1",
        &format!("{series_path}/new.mkv"),
    )
    .await;
    // The quarantined bytes physically exist under a hidden directory the walk must never enter;
    // the "original" file at `old_path` was never actually moved there in this fixture (the real
    // transfer never ran), so it's simply absent from disk -- exactly what would make cleanup
    // consider it "vanished" if the FK didn't stop it.
    std::fs::create_dir_all(format!("{series_path}/.hrrdarr-replaced-op-1")).unwrap();
    write_file(
        &format!("{series_path}/.hrrdarr-replaced-op-1/original.mkv"),
        b"quarantined",
    );
    let created = create(
        State(db.clone()),
        MediaDomain::Tv,
        Ok(Query(Empty {})),
        Ok(Json(input(Some(1)))),
    )
    .await
    .unwrap()
    .1
    .0;
    let claimed = actual_claim(&db, created.commands[0].id).await;
    run(&db, claimed).await.unwrap();
    let done = read(&c, created.commands[0].id).await.unwrap();
    assert!(matches!(done.status, RescanStatus::Succeeded));
    assert_eq!(done.files_adopted, Some(0));
    assert_eq!(done.files_removed, Some(0));
    let (file_id, path): (Option<i64>, String) = {
        let row = c
            .query(
                "SELECT e.episode_file_id,f.path FROM episodes e JOIN episode_files f ON f.id=e.episode_file_id WHERE e.id=1",
                (),
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap();
        (row.get(0).unwrap(), row.get(1).unwrap())
    };
    assert_eq!(file_id, Some(1));
    assert_eq!(path, old_path);
    let total_files: i64 = c
        .query("SELECT count(*) FROM episode_files", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(
        total_files, 1,
        "the quarantined file must not be adopted as a new one"
    );
}
