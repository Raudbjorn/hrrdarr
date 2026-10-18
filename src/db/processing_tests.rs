use super::rss_tests::{candidate, command, identity, prepare};
use super::*;
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
pub(super) async fn observed(
    c: &Connection,
    command: &str,
    client: &str,
    domain: &str,
    hash: &str,
) -> Result<String, Error> {
    let id = candidate(c, command, domain, Some(29), true).await?;
    prepare(c, &id, identity(domain, &[hash])).await?;
    c.execute(
        "INSERT INTO rss_hash_claims VALUES(?,?,?)",
        params![client, hash, id.clone()],
    )
    .await?;
    c.execute(
        "UPDATE rss_candidates SET status='submitting',private_payload=NULL WHERE id=?",
        [id.clone()],
    )
    .await?;
    c.execute(
        "UPDATE rss_candidates SET status='observed',observed_hash=? WHERE id=?",
        params![hash, id.clone()],
    )
    .await?;
    Ok(id)
}
async fn preview(c: &Connection, domain: &str) -> Result<String, Error> {
    let id = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO operations(id,media_type,episode_id,movie_id,source,mode,destination,status,message) VALUES(?,?,?,?,?,'copy',?,'preview','fixture')",params![id.clone(),if domain=="tv"{"episode"}else{"movie"},(domain=="tv").then_some(1),(domain=="movies").then_some(1),format!("/download/{domain}"),format!("/{domain}/new.mkv")]).await?;
    c.execute(
        "INSERT INTO import_journal(operation_id,plan_json,phase) VALUES(?,'{}','preview')",
        [id.clone()],
    )
    .await?;
    Ok(id)
}
fn old_file(op: &str, domain: &str) -> String {
    serde_json::json!({"version":1,"file_id":1,"path":format!("/{domain}/old.mkv"),"metadata":null,"parent":{"dev":1,"ino":2,"size":0,"mtime":0,"mtime_ns":0,"ctime":0,"ctime_ns":0},"file":{"dev":1,"ino":3,"size":1,"mtime":0,"mtime_ns":0,"ctime":0,"ctime_ns":0},"quarantine_name":format!(".hrrdarr-replaced-{op}")}).to_string()
}
pub(super) async fn link(
    c: &Connection,
    candidate: &str,
    op: &str,
    domain: &str,
) -> Result<(), Error> {
    c.execute("INSERT INTO rss_candidate_imports(candidate_id,operation_id,quality_id,revision_json,provenance_json,old_episode_file_id,old_movie_file_id,old_file_json) VALUES(?,?,1,?, '{}',?,?,?)",params![candidate,op,r#"{"version":1,"real":0,"is_repack":false}"#,(domain=="tv").then_some(1),(domain=="movies").then_some(1),old_file(op,domain)]).await?;
    Ok(())
}
#[tokio::test]
async fn processing_schema25_upgrade_rollback_retirement_and_retry_fences() -> Result<(), Error> {
    let scratch = Scratch(std::env::temp_dir().join(format!(
        "hrrdarr-processing-schema-{}",
        uuid::Uuid::new_v4()
    )));
    std::fs::create_dir(&scratch.0)?;
    let path = scratch.0.join("db");
    let raw = libsql::Builder::new_local(&path).build().await?;
    let c = raw.connect()?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    c.execute(HISTORY_SQL, ()).await?;
    for (i, (name, sql)) in MIGRATIONS.iter().take(25).enumerate() {
        c.execute_batch(sql).await?;
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
            params![i as i64 + 1, *name, checksum(sql), *sql],
        )
        .await?;
    }
    c.execute_batch("INSERT INTO series(id,tvdb_id,title,path) VALUES(1,101,'TV','/tv'); INSERT INTO seasons VALUES(1,1,1);INSERT INTO episode_files(id,series_id,path) VALUES(1,1,'/tv/old.mkv'); INSERT INTO episodes(id,series_id,season,number,title,episode_file_id) VALUES(1,1,1,1,'One',1); INSERT INTO movie_metadata(id,tmdb_id,title) VALUES(1,101,'Movie'); INSERT INTO movies(id,metadata_id,path) VALUES(1,1,'/movies');INSERT INTO movie_files(id,movie_id,path) VALUES(1,1,'/movies/old.mkv');").await?;
    let client = super::refresh_tests::provider(&c).await?;
    let oldcommand = super::refresh_tests::enqueue(&c, &client, "tv").await?;
    c.execute_batch("INSERT INTO operations(id,media_type,episode_id,source,mode,destination,status,message) VALUES('prior-import','episode',1,'/prior','copy','/tv/old.mkv','preview','fixture');INSERT INTO import_journal(operation_id,plan_json,phase) VALUES('prior-import','{}','preview');INSERT INTO import_history(operation_id,media_type,episode_id,episode_file_id,source,destination,size,sha256) VALUES('prior-import','episode',1,1,'/prior','/tv/old.mkv',1,printf('%064d',0));UPDATE import_journal SET phase='complete',stage_json='{}' WHERE operation_id='prior-import';").await?;

    let indexer = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'torznab','Indexer',1,1,1,1,'http://127.0.0.1:1/')",[indexer.clone()]).await?;
    for domain in ["tv", "movies"] {
        c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year) VALUES(?,'torznab',?,'[5000]','[]',?,?)",params![indexer.clone(),domain,(domain=="tv").then_some(0),(domain=="movies").then_some(0)]).await?;
    }
    let tvcommand = command(&c, &indexer, &client, "tv").await?;
    let moviecommand = command(&c, &indexer, &client, "movies").await?;
    let tv = observed(&c, &tvcommand, &client, "tv", &"a".repeat(40)).await?;
    let movie = observed(&c, &moviecommand, &client, "movies", &"b".repeat(40)).await?;
    let tx = c.transaction().await?;
    tx.execute_batch(MIGRATIONS[25].1).await?;
    assert!(tx.execute_batch(MIGRATIONS[25].1).await.is_err());
    tx.rollback().await?;
    assert_eq!(version(&c).await?, 25);
    assert!(
        c.query("SELECT * FROM rss_candidate_imports", ())
            .await
            .is_err()
    );
    assert_eq!(scalar(&c, "SELECT count(*) FROM rss_hash_claims").await?, 2);
    drop(c);
    drop(raw);
    let db = Database::open_local(&path).await?;
    let c = db.connect().await?;
    assert_eq!(version(&c).await?, 49); // Reasoning: latest migration is now 0049 indexer client binding (was 48: indexer operation policy); historical migration prefixes stay fixed.
    assert!(db.migration_backup().is_some());
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM download_processing_policies").await?,
        0
    );
    assert_eq!(
        c.query("SELECT id FROM commands", ())
            .await?
            .next()
            .await?
            .unwrap()
            .get::<String>(0)?,
        oldcommand
    );
    for (domain, id) in [("tv", &tv), ("movies", &movie)] {
        assert!(c.execute("INSERT INTO download_processing(candidate_id,policy_revision,status,next_attempt_at,created_at,updated_at) VALUES(?,1,'queued',100,100,100)",[id.clone()]).await.is_err());
        c.execute(
            "INSERT INTO download_processing_policies(provider_id,media_type,provider_revision,revision,enabled,mode) VALUES(?,?,1,1,1,'copy')",
            params![client.clone(), domain],
        )
        .await?;
        c.execute("INSERT INTO download_processing(candidate_id,policy_revision,status,next_attempt_at,created_at,updated_at) VALUES(?,1,'queued',100,100,100)",[id.clone()]).await?;
        assert!(
            c.execute(
                "UPDATE download_processing SET status='checking' WHERE candidate_id=?",
                [id.clone()]
            )
            .await
            .is_err()
        );
        c.execute("UPDATE download_processing SET status='checking',preflight_attempts=1,total_preflight_attempts=1 WHERE candidate_id=?",[id.clone()]).await?;
    }
    c.execute("UPDATE download_processing SET status='blocked',error_code='source_unavailable' WHERE candidate_id=?",[tv.clone()]).await?;
    assert!(c.execute("UPDATE download_processing SET status='queued',preflight_attempts=0,total_preflight_attempts=0 WHERE candidate_id=?",[tv.clone()]).await.is_err());
    c.execute("UPDATE download_processing SET status='queued',preflight_attempts=0,error_code=NULL WHERE candidate_id=?",[tv.clone()]).await?;
    c.execute("UPDATE download_processing SET status='checking',preflight_attempts=1,total_preflight_attempts=2 WHERE candidate_id=?",[tv.clone()]).await?;

    // Unlinked/rejected processing has no root ownership.
    c.execute("UPDATE series SET path='/tv-temp' WHERE id=1", ())
        .await?;
    c.execute("UPDATE series SET path='/tv' WHERE id=1", ())
        .await?;
    c.execute("UPDATE movies SET path='/movies-temp' WHERE id=1", ())
        .await?;
    c.execute("UPDATE movies SET path='/movies' WHERE id=1", ())
        .await?;
    assert!(
        c.execute(
            "UPDATE download_processing SET resume_requested=1 WHERE candidate_id=?",
            [tv.clone()]
        )
        .await
        .is_err()
    );
    assert!(c.execute("UPDATE download_processing SET status='importing',resume_requested=1 WHERE candidate_id=?",[tv.clone()]).await.is_err());
    let tvop = preview(&c, "tv").await?;
    let movieop = preview(&c, "movies").await?;
    assert!(link(&c, &tv, &movieop, "movies").await.is_err());
    let tx = c.transaction().await?;
    link(&tx, &tv, &tvop, "tv").await?;
    tx.rollback().await?;
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM rss_candidate_imports").await?,
        0
    );
    // Capture itself cannot accept an already ambiguous cross-domain path claim.
    let tx = c.transaction().await?;
    tx.execute("UPDATE movie_files SET path='/tv/old.mkv' WHERE id=1", ())
        .await?;
    assert!(link(&tx, &tv, &tvop, "tv").await.is_err());
    tx.rollback().await?;
    link(&c, &tv, &tvop, "tv").await?;
    link(&c, &movie, &movieop, "movies").await?;
    for sql in [
        "INSERT INTO episode_files(series_id,path) VALUES(1,'/movies/old.mkv')",
        "INSERT INTO movie_files(movie_id,path) VALUES(1,'/tv/old.mkv')",
        "UPDATE episode_files SET path='/movies/old.mkv' WHERE id=1",
        "UPDATE movie_files SET path='/tv/old.mkv' WHERE id=1",
    ] {
        let error = c.execute(sql, ()).await.unwrap_err();
        // Migration 28 recreates the old-row guard; either overlapping ownership
        // constraint may fire first, while all four mutations must remain denied.
        let message = error.to_string();
        assert!(
            message.contains("file path is owned by unfinished download import")
                || message.contains("replaced episode path requires retirement checkpoint"),
            "{error}"
        );
    }
    c.execute("UPDATE episode_files SET path=path WHERE id=1", ())
        .await?;
    c.execute("UPDATE movie_files SET path=path WHERE id=1", ())
        .await?;

    assert!(
        c.execute("UPDATE series SET path='/tv-other' WHERE id=1", ())
            .await
            .is_err()
    );
    assert!(
        c.execute("UPDATE movies SET path='/movies-other' WHERE id=1", ())
            .await
            .is_err()
    );

    for (domain, cmd) in [("tv", &tvcommand), ("movies", &moviecommand)] {
        let next = candidate(&c, cmd, domain, Some(29), true).await?;
        assert!(
            prepare(&c, &next, identity(domain, &[&"d".repeat(40)]))
                .await
                .is_err()
        );
        c.execute(
            "UPDATE rss_candidates SET status='cancelled',private_payload=NULL WHERE id=?",
            [next.clone()],
        )
        .await?;
        c.execute("DELETE FROM rss_candidates WHERE id=?", [next])
            .await?;
    }

    assert!(c.execute("INSERT INTO episodes(id,series_id,season,number,title,episode_file_id) VALUES(2,1,1,2,'Two',1)",()).await.is_err());
    for id in [&tv, &movie] {
        c.execute(
            "UPDATE download_processing SET status='importing' WHERE candidate_id=?",
            [id.clone()],
        )
        .await?;
        for sql in [
            "UPDATE download_processing SET status='cancelled' WHERE candidate_id=?",
            "UPDATE download_processing SET status='imported' WHERE candidate_id=?",
            "DELETE FROM rss_candidate_imports WHERE candidate_id=?",
            "UPDATE rss_candidate_imports SET quality_id=2 WHERE candidate_id=?",
            "UPDATE rss_candidate_imports SET retirement_state='shared_retained' WHERE candidate_id=?",
        ] {
            assert!(c.execute(sql, [id.clone()]).await.is_err());
        }
    }
    assert!(
        c.execute(
            "UPDATE download_processing SET resume_requested=2 WHERE candidate_id=?",
            [tv.clone()]
        )
        .await
        .is_err()
    );
    c.execute(
        "UPDATE download_processing SET resume_requested=1 WHERE candidate_id=?",
        [tv.clone()],
    )
    .await?;
    c.execute(
        "UPDATE import_journal SET error_code='source_unavailable' WHERE operation_id=?",
        [tvop.clone()],
    )
    .await?;
    let tx = c.transaction().await?;
    tx.execute(
        "UPDATE download_processing SET resume_requested=0 WHERE candidate_id=?",
        [tv.clone()],
    )
    .await?;
    tx.execute(
        "UPDATE import_journal SET error_code=NULL WHERE operation_id=?",
        [tvop.clone()],
    )
    .await?;
    tx.rollback().await?;
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM download_processing WHERE resume_requested=1"
        )
        .await?,
        1
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM import_journal WHERE error_code='source_unavailable'"
        )
        .await?,
        1
    );
    let tx = c.transaction().await?;
    tx.execute(
        "UPDATE download_processing SET resume_requested=0 WHERE candidate_id=?",
        [tv.clone()],
    )
    .await?;
    tx.execute(
        "UPDATE import_journal SET error_code=NULL WHERE operation_id=?",
        [tvop.clone()],
    )
    .await?;
    tx.commit().await?;
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM download_processing WHERE resume_requested=1"
        )
        .await?,
        0
    );
    // Provider edits stop new work but cannot strand an already-linked import recovery.
    c.execute(
        "UPDATE providers SET revision=2 WHERE id=?",
        [client.clone()],
    )
    .await?;
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM download_processing_policies WHERE enabled=0 AND revision=2"
        )
        .await?,
        2
    );
    c.execute("UPDATE download_processing SET status='importing',error_code='interrupted' WHERE candidate_id=?",[tv.clone()]).await?;
    for (domain, id, op) in [("tv", &tv, &tvop), ("movies", &movie, &movieop)] {
        let tx = c.transaction().await?;
        if domain == "tv" {
            tx.execute_batch("INSERT INTO episode_files(id,series_id,path) VALUES(2,1,'/tv/new.mkv');UPDATE episodes SET episode_file_id=2 WHERE id=1;").await?;
        } else {
            tx.execute(
                "UPDATE movie_files SET path='/movies/new.mkv' WHERE id=1",
                (),
            )
            .await?;
        }
        tx.execute("INSERT INTO import_history(operation_id,media_type,episode_id,movie_id,episode_file_id,movie_file_id,source,destination,size,sha256) VALUES(?,?,?,?,?,?,?,?,1,?)",params![op.clone(),if domain=="tv"{"episode"}else{"movie"},(domain=="tv").then_some(1),(domain=="movies").then_some(1),(domain=="tv").then_some(2),(domain=="movies").then_some(1),format!("/download/{domain}"),format!("/{domain}/new.mkv"),"c".repeat(64)]).await?;
        tx.execute(
            "UPDATE import_journal SET phase='committed',stage_json='{}' WHERE operation_id=?",
            [op.clone()],
        )
        .await?;
        tx.commit().await?;
        assert!(
            c.execute(
                "UPDATE import_journal SET phase='complete' WHERE operation_id=?",
                [op.clone()]
            )
            .await
            .is_err()
        );
        if domain == "tv" {
            c.execute(
                "UPDATE episode_files SET path='/tv/.hrrdarr-private/original' WHERE id=1",
                (),
            )
            .await?;
        }
        c.execute("UPDATE rss_candidate_imports SET retirement_state='quarantined',retirement_json=? WHERE candidate_id=?",params![r#"{"directory":{"dev":1,"ino":5,"size":0,"mtime":0,"mtime_ns":0,"ctime":0,"ctime_ns":0}}"#,id.clone()]).await?;
        assert!(
            c.execute(
                "UPDATE rss_candidate_imports SET retirement_state='pending' WHERE candidate_id=?",
                [id.clone()]
            )
            .await
            .is_err()
        );
        c.execute(
            "UPDATE import_journal SET phase='complete' WHERE operation_id=?",
            [op.clone()],
        )
        .await?;
        c.execute(
            "UPDATE download_processing SET resume_requested=1 WHERE candidate_id=?",
            [id.clone()],
        )
        .await?;
        assert!(c.execute("UPDATE download_processing SET status='imported',error_code=NULL WHERE candidate_id=?",[id.clone()]).await.is_err());
        c.execute(
            "UPDATE download_processing SET status='imported',resume_requested=0,error_code=NULL WHERE candidate_id=?",
            [id.clone()],
        )
        .await?;
    }
    c.execute("UPDATE series SET path='/tv-other' WHERE id=1", ())
        .await?;
    c.execute("UPDATE movies SET path='/movies-other' WHERE id=1", ())
        .await?;
    // Both old active paths become available only after complete retirement/history.
    let tx = c.transaction().await?;
    tx.execute(
        "INSERT INTO episode_files(series_id,path) VALUES(1,'/movies/old.mkv')",
        (),
    )
    .await?;
    tx.execute(
        "INSERT INTO movie_metadata(id,tmdb_id,title) VALUES(2,202,'Other')",
        (),
    )
    .await?;
    tx.execute(
        "INSERT INTO movies(id,metadata_id,path) VALUES(2,2,'/other')",
        (),
    )
    .await?;
    tx.execute(
        "INSERT INTO movie_files(movie_id,path) VALUES(2,'/tv/old.mkv')",
        (),
    )
    .await?;
    tx.rollback().await?;
    // Completed imports release only typed target ownership, never submission/hash authority.
    for (domain, cmd) in [("tv", &tvcommand), ("movies", &moviecommand)] {
        let next = candidate(&c, cmd, domain, Some(29), true).await?;
        prepare(&c, &next, identity(domain, &[&"d".repeat(40)])).await?;
    }
    assert_eq!(scalar(&c, "SELECT count(*) FROM rss_hash_claims").await?, 2);
    assert!(c.execute("DELETE FROM rss_hash_claims", ()).await.is_err());
    assert!(
        c.execute("DELETE FROM download_processing", ())
            .await
            .is_err()
    );
    drop(c);
    drop(db);
    let db = Database::open_local(&path).await?;
    let c = db.connect().await?;
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM download_processing WHERE status='imported'"
        )
        .await?,
        2
    );
    assert_eq!(scalar(&c, "SELECT count(*) FROM import_history").await?, 3);
    integrity(&c).await?;
    Ok(())
}

#[tokio::test]
async fn retired_snapshot_episode_file_cannot_be_reactivated_by_exact_reupload() -> Result<(), Error>
{
    let _guard = crate::snapshots::IMPORT_TEST_LOCK.lock().await;
    let scratch = Scratch(
        std::env::temp_dir().join(format!("hrrdarr-retired-snapshot-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&scratch.0)?;
    let sourcepath = scratch.0.join("source");
    let source = libsql::Builder::new_local(&sourcepath).build().await?;
    let sc = source.connect()?;
    sc.execute_batch("CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES(233);CREATE TABLE Series(Id INTEGER,TvdbId INTEGER,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,Seasons TEXT);INSERT INTO Series VALUES(1,101,'TV',2020,'/tv',1,'[{\"seasonNumber\":1,\"monitored\":true}]');CREATE TABLE Episodes(Id INTEGER,SeriesId INTEGER,SeasonNumber INTEGER,EpisodeNumber INTEGER,Title TEXT,Monitored INTEGER,EpisodeFileId INTEGER);INSERT INTO Episodes VALUES(1,1,1,1,'One',1,1);CREATE TABLE EpisodeFiles(Id INTEGER,SeriesId INTEGER,RelativePath TEXT);INSERT INTO EpisodeFiles VALUES(1,1,'old.mkv');").await?;
    drop(sc);
    drop(source);
    let bytes = std::fs::read(sourcepath)?;
    let db = Database::open_local(scratch.0.join("library")).await?;
    assert!(
        crate::snapshots::import(
            &db,
            crate::snapshots::Application::Sonarr,
            bytes.clone(),
            false
        )
        .await?
        .applied
    );
    assert_eq!(
        crate::snapshots::import(
            &db,
            crate::snapshots::Application::Sonarr,
            bytes.clone(),
            false
        )
        .await?
        .conflicts,
        0
    );
    let c = db.connect().await?;
    let client = super::refresh_tests::provider(&c).await?;
    let indexer = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'torznab','Indexer',1,1,1,1,'http://127.0.0.1:1/')",[indexer.clone()]).await?;
    c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search) VALUES(?,'torznab','tv','[5000]','[]',0)",[indexer.clone()]).await?;
    let cmd = command(&c, &indexer, &client, "tv").await?;
    let receipt = observed(&c, &cmd, &client, "tv", &"a".repeat(40)).await?;
    let op = preview(&c, "tv").await?;
    link(&c, &receipt, &op, "tv").await?;
    c.execute_batch("INSERT INTO episode_files(id,series_id,path) VALUES(2,1,'/tv/new.mkv');UPDATE episodes SET episode_file_id=2 WHERE id=1;").await?;
    c.execute("INSERT INTO import_history(operation_id,media_type,episode_id,episode_file_id,source,destination,size,sha256) VALUES(?,'episode',1,2,'/download/tv','/tv/new.mkv',1,?)",params![op.clone(),"b".repeat(64)]).await?;
    c.execute(
        "UPDATE import_journal SET phase='committed',stage_json='{}' WHERE operation_id=?",
        [op.clone()],
    )
    .await?;
    c.execute(
        "UPDATE episode_files SET path='/tv/.hrrdarr-private/original' WHERE id=1",
        (),
    )
    .await?;
    c.execute("UPDATE rss_candidate_imports SET retirement_state='quarantined',retirement_json='{\"directory\":{\"dev\":1,\"ino\":5}}' WHERE candidate_id=?",[receipt]).await?;
    c.execute(
        "UPDATE import_journal SET phase='complete' WHERE operation_id=?",
        [op],
    )
    .await?;
    let report = crate::snapshots::import(
        &db,
        crate::snapshots::Application::Sonarr,
        bytes.clone(),
        false,
    )
    .await?;
    assert!(!report.applied);
    assert!(report.conflicts > 0);
    let dry =
        crate::snapshots::import(&db, crate::snapshots::Application::Sonarr, bytes, true).await?;
    assert!(!dry.applied);
    assert!(dry.conflicts > 0);
    assert_eq!(
        scalar(&c, "SELECT episode_file_id FROM episodes WHERE id=1").await?,
        2
    );
    assert_eq!(scalar(&c, "SELECT count(*) FROM episode_files").await?, 2);
    assert_eq!(
        c.query("SELECT path FROM episode_files WHERE id=1", ())
            .await?
            .next()
            .await?
            .unwrap()
            .get::<String>(0)?,
        "/tv/.hrrdarr-private/original"
    );
    Ok(())
}
