use super::rss_tests::{candidate, command, identity, prepare};
use super::*;
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
async fn search(
    c: &Connection,
    indexer: &str,
    client: &str,
    media: &str,
    mode: &str,
) -> Result<String, Error> {
    let id = uuid::Uuid::new_v4().to_string();
    let captured = if media == "tv" {
        serde_json::json!({"media_type":"tv","series_id":1,"episode_id":1,"tvdb_id":101,"title":"TV","season":1,"number":1,"series_type":"standard","use_scene_numbering":false})
    } else {
        serde_json::json!({"media_type":"movies","movie_id":1,"metadata_id":1,"tmdb_id":101,"imdb_id":null,"title":"Movie","year":2020})
    };
    c.execute("INSERT INTO search_commands(id,mode,media_type,requested_episode_id,requested_movie_id,captured_target_json,indexer_id,indexer_revision,client_id,client_revision,next_attempt_at,created_at) VALUES(?,?,?,?,?,?,?,1,?,1,100,100)",params![id.clone(),mode,media,(media=="tv").then_some(1),(media=="movies").then_some(1),captured.to_string(),indexer,client]).await?;
    Ok(id)
}
async fn start(c: &Connection, id: &str) -> Result<(), Error> {
    c.execute(
        "UPDATE search_commands SET status='running',attempts=1,started_at=100 WHERE id=?",
        [id],
    )
    .await?;
    Ok(())
}
async fn offer(
    c: &Connection,
    cmd: &str,
    ordinal: i64,
    bytes: usize,
    age: i64,
) -> Result<String, Error> {
    let id = uuid::Uuid::new_v4().to_string();
    let fp = id.replace('-', "").repeat(2);
    c.execute("INSERT INTO search_results(id,command_id,ordinal,fingerprint,title,metadata_json,decision_json,private_payload,created_at,expires_at) VALUES(?,?,?,?,'Release','{}','{}',?,unixepoch()-?,unixepoch()-?+1800)",params![id.clone(),cmd,ordinal,fp,vec![1u8;bytes],age,age]).await?;
    Ok(id)
}
async fn select(c: &Connection, result: &str) -> Result<String, Error> {
    select_target(c, result, None).await
}
async fn select_target(
    c: &Connection,
    result: &str,
    wrong_movie: Option<i64>,
) -> Result<String, Error> {
    let id = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO rss_candidates(id,search_result_id,media_type,indexer_id,indexer_revision,client_id,client_revision,fingerprint,title,private_payload,series_id,movie_id,status,decision_reasons_json,created_at,updated_at) SELECT ?,o.id,c.media_type,c.indexer_id,c.indexer_revision,c.client_id,c.client_revision,o.fingerprint,o.title,o.private_payload,CASE WHEN c.media_type='tv' THEN 1 END,coalesce(?,c.requested_movie_id),'pending','[]',unixepoch(),unixepoch() FROM search_results o JOIN search_commands c ON c.id=o.command_id WHERE o.id=?",params![id.clone(),wrong_movie,result]).await?;
    Ok(id)
}
#[tokio::test]
async fn search_schema26_upgrade_rollback_transfer_origin_and_retention() -> Result<(), Error> {
    let scratch = Scratch(
        std::env::temp_dir().join(format!("hrrdarr-search-schema-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&scratch.0)?;
    let path = scratch.0.join("db");
    let raw = libsql::Builder::new_local(&path).build().await?;
    let c = raw.connect()?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    c.execute(HISTORY_SQL, ()).await?;
    for (i, (name, sql)) in MIGRATIONS.iter().take(26).enumerate() {
        c.execute_batch(sql).await?;
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
            params![i as i64 + 1, *name, checksum(sql), *sql],
        )
        .await?;
    }
    c.execute_batch("INSERT INTO series(id,tvdb_id,title,path) VALUES(1,101,'TV','/tv');INSERT INTO seasons VALUES(1,1,1);INSERT INTO episodes(id,series_id,season,number,title) VALUES(1,1,1,1,'One'),(2,1,1,2,'Two');INSERT INTO movie_metadata(id,tmdb_id,title,year) VALUES(1,101,'Movie',2020);INSERT INTO movies(id,metadata_id,path) VALUES(1,1,'/movie');").await?;
    c.execute_batch("INSERT INTO movie_metadata(id,tmdb_id,title) VALUES(2,202,'Other');INSERT INTO movies(id,metadata_id,path) VALUES(2,2,'/other');INSERT INTO episode_files(id,series_id,path) VALUES(1,1,'/tv/original');UPDATE episodes SET episode_file_id=1 WHERE id=1;INSERT INTO operations(id,media_type,episode_id,source,mode,destination,status,message) VALUES('prior-import','episode',1,'/prior','copy','/tv/original','preview','fixture');INSERT INTO import_journal(operation_id,plan_json,phase) VALUES('prior-import','{}','preview');INSERT INTO import_history(operation_id,media_type,episode_id,episode_file_id,source,destination,size,sha256) VALUES('prior-import','episode',1,1,'/prior','/tv/original',1,printf('%064d',0));UPDATE import_journal SET phase='complete',stage_json='{}' WHERE operation_id='prior-import';").await?;
    let client = super::refresh_tests::provider(&c).await?;
    let oldcommand = super::refresh_tests::enqueue(&c, &client, "tv").await?;
    let indexer = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'torznab','Indexer',1,1,1,1,'http://127.0.0.1:1/')",[indexer.clone()]).await?;
    for media in ["tv", "movies"] {
        c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year) VALUES(?,'torznab',?,'[5000]','[]',?,?)",params![indexer.clone(),media,(media=="tv").then_some(0),(media=="movies").then_some(0)]).await?;
    }
    let rss = command(&c, &indexer, &client, "tv").await?;
    let legacy = candidate(&c, &rss, "tv", Some(29), true).await?;
    // A prior uncertain receipt retains its original claims and never regains submission authority.
    prepare(&c, &legacy, identity("tv", &[&"a".repeat(40)])).await?;
    c.execute(
        "INSERT INTO rss_hash_claims VALUES(?,?,?)",
        params![client.clone(), "a".repeat(40), legacy.clone()],
    )
    .await?;
    c.execute(
        "UPDATE rss_candidates SET status='submitting',private_payload=NULL WHERE id=?",
        [legacy.clone()],
    )
    .await?;
    c.execute("UPDATE rss_candidates SET status='needs_attention',error_code='submission_unknown' WHERE id=?",[legacy.clone()]).await?;
    let tx = c.transaction().await?;
    tx.execute_batch(MIGRATIONS[26].1).await?;
    assert!(tx.execute_batch(MIGRATIONS[26].1).await.is_err());
    tx.rollback().await?;
    assert_eq!(version(&c).await?, 26);
    assert!(c.query("SELECT * FROM search_commands", ()).await.is_err());
    assert!(
        c.query("SELECT search_result_id FROM rss_candidates", ())
            .await
            .is_err()
    );
    drop(c);
    drop(raw);
    let db = Database::open_local(&path).await?;
    assert!(db.migration_backup().is_some());
    let c = db.connect().await?;
    assert_eq!(version(&c).await?, 36); // Latest open adds snapshot CF activation; historical migration starts remain unchanged.
    assert_eq!(
        c.query("SELECT id FROM commands", ())
            .await?
            .next()
            .await?
            .unwrap()
            .get::<String>(0)?,
        oldcommand
    );
    assert_eq!(scalar(&c,"SELECT count(*) FROM rss_candidates WHERE search_result_id IS NULL AND status='needs_attention'").await?,1);
    assert_eq!(scalar(&c, "SELECT count(*) FROM rss_hash_claims").await?, 1);
    let tv = search(&c, &indexer, &client, "tv", "interactive").await?;
    let movie = search(&c, &indexer, &client, "movies", "automatic").await?;
    for cmd in [&tv, &movie] {
        assert!(
            c.execute(
                "UPDATE search_commands SET decision_context='rss' WHERE id=?",
                [cmd.clone()]
            )
            .await
            .is_err()
        );
        assert!(offer(&c, cmd, 0, 29, 0).await.is_err());
        start(&c, cmd).await?;
    }
    let tvresult = offer(&c, &tv, 0, 29, 0).await?;
    let movieresult = offer(&c, &movie, 0, 29, 0).await?;
    let expired = offer(&c, &tv, 1, 29, 1801).await?;
    let other = offer(&c, &tv, 2, 29, 0).await?;
    assert!(select(&c, &tvresult).await.is_err());
    c.execute(
        "UPDATE search_commands SET fetch_complete=1,fetched=3 WHERE id=?",
        [tv.clone()],
    )
    .await?;
    c.execute(
        "UPDATE search_commands SET status='succeeded',completed_at=101 WHERE id=?",
        [tv.clone()],
    )
    .await?;
    c.execute(
        "UPDATE search_commands SET fetch_complete=1,fetched=1 WHERE id=?",
        [movie.clone()],
    )
    .await?;
    assert!(select(&c, &expired).await.is_err());
    assert!(
        c.execute(
            "UPDATE search_results SET selected_candidate_id=? WHERE id=?",
            params![legacy.clone(), tvresult.clone()]
        )
        .await
        .is_err()
    );
    assert!(
        c.execute(
            "UPDATE search_results SET private_payload=NULL WHERE id=?",
            [tvresult.clone()]
        )
        .await
        .is_err()
    );
    c.execute(
        "UPDATE search_results SET private_payload=NULL WHERE id=?",
        [expired.clone()],
    )
    .await?;
    // Late transfer failure rolls candidate and offer changes back together.
    c.execute_batch("CREATE TRIGGER injected_selection BEFORE UPDATE OF selected_candidate_id ON search_results BEGIN SELECT RAISE(ABORT,'selection failure');END;").await?;
    assert!(select(&c, &tvresult).await.is_err());
    c.execute_batch("DROP TRIGGER injected_selection;").await?;
    assert_eq!(scalar(&c, "SELECT count(*) FROM rss_candidates").await?, 1);
    assert!(select_target(&c, &movieresult, Some(2)).await.is_err());
    let legacy_pending = candidate(&c, &rss, "tv", Some(29), false).await?;
    assert!(
        c.execute(
            "UPDATE rss_candidates SET search_result_id=? WHERE id=?",
            params![tvresult.clone(), legacy_pending.clone()]
        )
        .await
        .is_err()
    );
    c.execute(
        "UPDATE rss_candidates SET status='cancelled',private_payload=NULL WHERE id=?",
        [legacy_pending.clone()],
    )
    .await?;
    c.execute("DELETE FROM rss_candidates WHERE id=?", [legacy_pending])
        .await?;
    let tvcandidate = select(&c, &tvresult).await?;
    let moviecandidate = select(&c, &movieresult).await?;
    assert_eq!(scalar(&c,"SELECT count(*) FROM search_results WHERE selected_candidate_id IS NOT NULL AND private_payload IS NULL").await?,2);
    assert!(select(&c, &tvresult).await.is_err());
    assert!(select(&c, &other).await.is_err());
    // Expired unselected siblings can be reclaimed without erasing selection provenance.
    c.execute("DELETE FROM search_results WHERE id=?", [expired])
        .await?;
    assert_eq!(
        scalar(
            &c,
            "SELECT fetched FROM search_commands WHERE media_type='tv'"
        )
        .await?,
        3
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM search_results WHERE selected_candidate_id IS NOT NULL"
        )
        .await?,
        2
    );
    assert!(
        c.execute(
            "INSERT INTO rss_candidate_episodes VALUES(?,1,2)",
            [tvcandidate.clone()]
        )
        .await
        .is_err()
    );
    c.execute(
        "INSERT INTO rss_candidate_episodes VALUES(?,1,1)",
        [tvcandidate.clone()],
    )
    .await?;
    assert!(
        c.execute(
            "UPDATE rss_candidates SET search_result_id=NULL WHERE id=?",
            [tvcandidate.clone()]
        )
        .await
        .is_err()
    );
    assert!(
        c.execute(
            "UPDATE rss_candidates SET series_id=NULL WHERE id=?",
            [tvcandidate.clone()]
        )
        .await
        .is_err()
    );
    assert!(
        c.execute(
            "UPDATE rss_candidates SET movie_id=NULL WHERE id=?",
            [moviecandidate.clone()]
        )
        .await
        .is_err()
    );
    assert!(
        c.execute(
            "UPDATE search_results SET selected_candidate_id=NULL WHERE id=?",
            [tvresult.clone()]
        )
        .await
        .is_err()
    );
    assert!(
        c.execute(
            "UPDATE search_results SET private_payload=zeroblob(29) WHERE id=?",
            [tvresult.clone()]
        )
        .await
        .is_err()
    );
    assert!(
        c.execute("DELETE FROM search_results WHERE id=?", [tvresult.clone()])
            .await
            .is_err()
    );
    assert!(
        c.execute("DELETE FROM search_commands WHERE id=?", [tv.clone()])
            .await
            .is_err()
    );
    // Target and hash claims are shared with RSS, irrespective of UserSearch exception.
    assert!(
        prepare(&c, &tvcandidate, identity("tv", &[&"b".repeat(40)]))
            .await
            .is_err()
    );
    prepare(&c, &moviecandidate, identity("movies", &[&"a".repeat(40)])).await?;
    assert!(
        c.execute(
            "INSERT INTO rss_hash_claims VALUES(?,?,?)",
            params![client.clone(), "a".repeat(40), moviecandidate.clone()]
        )
        .await
        .is_err()
    );
    let cleanup = search(&c, &indexer, &client, "movies", "interactive").await?;
    start(&c, &cleanup).await?;
    offer(&c, &cleanup, 0, 29, 1801).await?;
    c.execute(
        "UPDATE search_commands SET status='cancelled',completed_at=101 WHERE id=?",
        [cleanup.clone()],
    )
    .await?;
    c.execute(
        "DELETE FROM search_results WHERE command_id=?",
        [cleanup.clone()],
    )
    .await?;
    c.execute("DELETE FROM search_commands WHERE id=?", [cleanup])
        .await?;
    // Offers and candidates have independent row caps, but share one encrypted-byte budget.
    let tx = c.transaction().await?;
    let bulk = search(&tx, &indexer, &client, "movies", "interactive").await?;
    start(&tx, &bulk).await?;
    let bytes_sql = "SELECT (SELECT coalesce(sum(length(private_payload)),0) FROM rss_candidates)+(SELECT coalesce(sum(length(private_payload)),0) FROM search_results)";
    let existing = scalar(&tx, bytes_sql).await?;
    let mut first = String::new();
    for n in 0..255 {
        let id = offer(&tx, &bulk, n, 65565, 0).await?;
        if n == 0 {
            first = id;
        }
    }
    let remainder = 16_777_216 - existing - 255 * 65_565;
    offer(&tx, &bulk, 255, remainder as usize, 0).await?;
    assert_eq!(scalar(&tx, bytes_sql).await?, 16_777_216);
    let error = offer(&tx, &bulk, 256, 29, 0).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("release payload capacity reached")
    );
    let error = candidate(&tx, &rss, "tv", Some(29), false)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("release payload capacity reached")
    );
    tx.execute(
        "UPDATE search_commands SET fetch_complete=1,fetched=256 WHERE id=?",
        [bulk.clone()],
    )
    .await?;
    tx.execute(
        "UPDATE search_commands SET status='succeeded',completed_at=101 WHERE id=?",
        [bulk],
    )
    .await?;
    select(&tx, &first).await?;
    assert_eq!(
        scalar(&tx, bytes_sql).await?,
        16_777_216,
        "selection transfers authority without duplicating retained ciphertext"
    );
    tx.rollback().await?;
    let tx = c.transaction().await?;
    let bulk = search(&tx, &indexer, &client, "movies", "interactive").await?;
    start(&tx, &bulk).await?;
    let second = search(&tx, &indexer, &client, "tv", "interactive").await?;
    start(&tx, &second).await?;
    let existing = scalar(&tx, "SELECT count(*) FROM search_results").await?;
    for n in 0..1024 - existing {
        offer(&tx, if n < 1000 { &bulk } else { &second }, n % 1000, 29, 0).await?;
    }
    assert_eq!(
        scalar(&tx, "SELECT count(*) FROM search_results").await?,
        1024
    );
    let error = offer(&tx, &second, 999, 29, 0).await.unwrap_err();
    assert!(error.to_string().contains("search result capacity reached"));
    tx.rollback().await?;
    // Admission for every delivered command kind sees the same 1024-row capacity. Only active
    // (queued/running/retry_wait) rows occupy the pool (migration 0033), so `existing` must be
    // computed the same way -- terminal rows left over from earlier in this test must not be
    // mistaken for occupied slots, or this loop under-pads and the capacity below is never hit.
    let tx = c.transaction().await?;
    let existing = scalar(
        &tx,
        &format!("SELECT {}", super::refresh_tests::POOL_ACTIVE_SQL),
    )
    .await?;
    for _ in 0..1024 - existing {
        search(&tx, &indexer, &client, "tv", "interactive").await?;
    }
    assert_eq!(
        scalar(
            &tx,
            &format!("SELECT {}", super::refresh_tests::POOL_ACTIVE_SQL),
        )
        .await?,
        1024
    );
    assert!(
        search(&tx, &indexer, &client, "tv", "interactive")
            .await
            .unwrap_err()
            .to_string()
            .contains("command capacity reached")
    );
    assert!(
        super::refresh_tests::enqueue(&tx, &client, "movies")
            .await
            .unwrap_err()
            .to_string()
            .contains("command capacity reached")
    );
    assert!(
        command(&tx, &indexer, &client, "movies")
            .await
            .unwrap_err()
            .to_string()
            .contains("command capacity reached")
    );
    for sql in [
        "INSERT INTO blocklist_clear_commands(id,name,media_type,next_attempt_at,created_at) VALUES(?,'clear_blocklist','tv',100,100)",
        "INSERT INTO metadata_refresh_commands(id,name,media_type,series_id,external_id,next_attempt_at,created_at) VALUES(?,'refresh_series','tv',1,101,100,100)",
    ] {
        assert!(
            tx.execute(sql, [uuid::Uuid::new_v4().to_string()])
                .await
                .unwrap_err()
                .to_string()
                .contains("command capacity reached")
        );
    }
    tx.rollback().await?;
    drop(c);
    drop(db);
    let db = Database::open_local(&path).await?;
    let c = db.connect().await?;
    assert_eq!(
        c.query(
            "SELECT selected_candidate_id FROM search_results WHERE id=?",
            [tvresult]
        )
        .await?
        .next()
        .await?
        .unwrap()
        .get::<String>(0)?,
        tvcandidate
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM search_commands WHERE decision_context='user_search'"
        )
        .await?,
        2
    );
    assert_eq!(scalar(&c,"SELECT count(*) FROM import_history WHERE operation_id='prior-import' AND episode_id=1 AND destination='/tv/original'").await?,1);
    integrity(&c).await?;
    Ok(())
}
