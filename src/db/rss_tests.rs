use super::*;
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
pub(super) async fn command(
    c: &Connection,
    indexer: &str,
    client: &str,
    domain: &str,
) -> Result<String, libsql::Error> {
    let id = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO rss_commands(id,name,media_type,indexer_id,indexer_revision,client_id,client_revision,next_attempt_at,created_at) VALUES(?,'rss_sync',?,?,1,?,1,100,100)",params![id.clone(),domain,indexer,client]).await?;
    Ok(id)
}
pub(super) async fn candidate(
    c: &Connection,
    command: &str,
    domain: &str,
    bytes: Option<usize>,
    target: bool,
) -> Result<String, libsql::Error> {
    let id = uuid::Uuid::new_v4().to_string();
    let hash = format!("{}{}", id.replace('-', ""), id.replace('-', ""));
    c.execute("INSERT INTO rss_candidates(id,command_id,media_type,indexer_id,indexer_revision,client_id,client_revision,fingerprint,title,private_payload,series_id,movie_id,status,decision_reasons_json,created_at,updated_at) SELECT ?,id,media_type,indexer_id,indexer_revision,client_id,client_revision,?,'Release',?,?,?,?, '[]',100,100 FROM rss_commands WHERE id=?",params![id.clone(),hash,bytes.map(|n|vec![1u8;n]),(target&&domain=="tv").then_some(1),(target&&domain=="movies").then_some(1),if bytes.is_some(){"pending"}else{"rejected"},command]).await?;
    if target && domain == "tv" {
        c.execute(
            "INSERT INTO rss_candidate_episodes VALUES(?,1,1)",
            [id.clone()],
        )
        .await?;
    }
    Ok(id)
}
pub(super) fn identity(domain: &str, hashes: &[&str]) -> String {
    serde_json::json!({"version":1,"target":{"media_type":if domain=="tv"{"episode"}else{"movie"},"id":1},"hashes":hashes,"settings_fingerprint":"a".repeat(64),"payload_sha256":"b".repeat(64)}).to_string()
}
pub(super) async fn prepare(
    c: &Connection,
    id: &str,
    identity: String,
) -> Result<u64, libsql::Error> {
    c.execute(
        "UPDATE rss_candidates SET status='prepared',submission_identity_json=? WHERE id=?",
        params![identity, id],
    )
    .await
}
#[tokio::test]
async fn rss_schema23_upgrade_rollback_intent_ownership_and_caps() -> Result<(), Error> {
    let files =
        Scratch(std::env::temp_dir().join(format!("hrrdarr-rss-schema-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&files.0)?;
    let path = files.0.join("db");
    let raw = libsql::Builder::new_local(&path).build().await?;
    let c = raw.connect()?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    c.execute(HISTORY_SQL, ()).await?;
    for (i, (name, sql)) in MIGRATIONS.iter().take(23).enumerate() {
        c.execute_batch(sql).await?;
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
            params![i as i64 + 1, *name, checksum(sql), *sql],
        )
        .await?;
    }
    c.execute_batch("INSERT INTO series(id,tvdb_id,title,path) VALUES(1,101,'TV','/tv');INSERT INTO seasons VALUES(1,1,1);INSERT INTO episodes(id,series_id,season,number,title) VALUES(1,1,1,1,'One');INSERT INTO movie_metadata(id,tmdb_id,title,year) VALUES(1,101,'Film',2020);INSERT INTO movies(id,metadata_id,path) VALUES(1,1,'/movie');").await?;
    let client = super::refresh_tests::provider(&c).await?;
    let old = super::refresh_tests::enqueue(&c, &client, "tv").await?;
    let clear = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO blocklist_clear_commands(id,name,media_type,next_attempt_at,created_at) VALUES(?,'clear_blocklist','tv',100,100)",[clear.clone()]).await?;
    let tx = c.transaction().await?;
    tx.execute_batch(MIGRATIONS[23].1).await?;
    tx.execute_batch(MIGRATIONS[24].1).await?;
    assert!(tx.execute_batch(MIGRATIONS[24].1).await.is_err());
    tx.rollback().await?;
    assert_eq!(version(&c).await?, 23);
    assert!(
        c.query("SELECT runtime FROM movie_metadata", ())
            .await
            .is_err()
    );
    drop(c);
    drop(raw);
    let db = Database::open_local(&path).await?;
    assert!(db.migration_backup().is_some());
    let c = db.connect().await?;
    assert_eq!(version(&c).await?, 26); // Latest open adds durable download import ownership; predecessor stays fixed.
    assert_eq!(
        c.query("SELECT id FROM commands", ())
            .await?
            .next()
            .await?
            .unwrap()
            .get::<String>(0)?,
        old
    );
    assert_eq!(
        c.query("SELECT id FROM blocklist_clear_commands", ())
            .await?
            .next()
            .await?
            .unwrap()
            .get::<String>(0)?,
        clear
    );
    assert_eq!(scalar(&c,"SELECT count(*) FROM movie_metadata WHERE runtime IS NULL AND status IS NULL AND in_cinemas IS NULL AND digital_release IS NULL AND original_language IS NULL").await?,1);
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM release_delay_policies").await?,
        0
    );
    assert!(
        c.execute("INSERT INTO release_delay_policies VALUES('tv',0,0,1)", ())
            .await
            .is_err()
    );
    c.execute(
        "INSERT INTO release_delay_policies VALUES('movies',10080,0,-365)",
        (),
    )
    .await?;
    assert!(
        c.execute(
            "UPDATE release_delay_policies SET torrent_delay_minutes=10081",
            ()
        )
        .await
        .is_err()
    );
    for n in 0..64 {
        c.execute(
            "INSERT INTO movie_alternative_titles VALUES(1,?)",
            [format!("Alias{n}")],
        )
        .await?;
    }
    assert!(
        c.execute(
            "INSERT INTO movie_alternative_titles VALUES(1,'Overflow')",
            ()
        )
        .await
        .is_err()
    );
    let indexer = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'torznab','Indexer',1,1,1,1,'http://127.0.0.1:1/')",[indexer.clone()]).await?;
    for domain in ["tv", "movies"] {
        c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year) VALUES(?,'torznab',?,'[5000]','[]',?,?)",params![indexer.clone(),domain,(domain=="tv").then_some(0),(domain=="movies").then_some(0)]).await?;
    }
    let tv = command(&c, &indexer, &client, "tv").await?;
    let movie = command(&c, &indexer, &client, "movies").await?;
    assert!(command(&c, &indexer, &client, "tv").await.is_err());
    let tv_candidate = candidate(&c, &tv, "tv", Some(29), true).await?;
    let movie_candidate = candidate(&c, &movie, "movies", Some(29), true).await?;
    assert!(
        prepare(&c, &tv_candidate, identity("movies", &[&"a".repeat(40)]))
            .await
            .is_err()
    );
    let mut leaked: serde_json::Value = serde_json::from_str(&identity("tv", &[&"a".repeat(40)]))?;
    leaked["secret_url"] = serde_json::json!("private");
    assert!(
        prepare(&c, &tv_candidate, leaked.to_string())
            .await
            .is_err()
    );
    let h1 = "a".repeat(40);
    let h2 = "b".repeat(64);
    prepare(&c, &tv_candidate, identity("tv", &[&h1, &h2])).await?;
    assert!(
        c.execute(
            "DELETE FROM rss_candidate_episodes WHERE candidate_id=?",
            [tv_candidate.clone()]
        )
        .await
        .is_err()
    );
    let tx = c.transaction().await?;
    tx.execute(
        "INSERT INTO rss_hash_claims VALUES(?,?,?)",
        params![client.clone(), h1.clone(), tv_candidate.clone()],
    )
    .await?;
    assert!(
        tx.execute(
            "UPDATE rss_candidates SET status='submitting',private_payload=NULL WHERE id=?",
            [tv_candidate.clone()]
        )
        .await
        .is_err()
    );
    tx.execute(
        "INSERT INTO rss_hash_claims VALUES(?,?,?)",
        params![client.clone(), h2.clone(), tv_candidate.clone()],
    )
    .await?;
    tx.execute(
        "UPDATE rss_candidates SET status='submitting',private_payload=NULL WHERE id=?",
        [tv_candidate.clone()],
    )
    .await?;
    tx.rollback().await?;
    assert_eq!(scalar(&c, "SELECT count(*) FROM rss_hash_claims").await?, 0);
    let tx = c.transaction().await?;
    for hash in [&h1, &h2] {
        tx.execute(
            "INSERT INTO rss_hash_claims VALUES(?,?,?)",
            params![client.clone(), hash.clone(), tv_candidate.clone()],
        )
        .await?;
    }
    tx.execute(
        "UPDATE rss_candidates SET status='submitting',private_payload=NULL WHERE id=?",
        [tv_candidate.clone()],
    )
    .await?;
    tx.commit().await?;
    for sql in [
        "UPDATE rss_candidates SET status='prepared',private_payload=zeroblob(29) WHERE id=?",
        "UPDATE rss_candidates SET status='pending',private_payload=zeroblob(29) WHERE id=?",
        "DELETE FROM rss_candidates WHERE id=?",
    ] {
        assert!(c.execute(sql, [tv_candidate.clone()]).await.is_err());
    }
    for attempt in 1..=3 {
        c.execute("UPDATE rss_candidates SET status='reconciling',attempts=?,error_code='submission_unknown' WHERE id=?",params![attempt,tv_candidate.clone()]).await?;
    }
    assert!(
        c.execute(
            "UPDATE rss_candidates SET status='reconciling',attempts=4 WHERE id=?",
            [tv_candidate.clone()]
        )
        .await
        .is_err()
    );
    assert!(
        c.execute(
            "UPDATE rss_candidates SET status='observed',observed_hash=? WHERE id=?",
            params!["b".repeat(40), tv_candidate.clone()]
        )
        .await
        .is_err()
    );
    c.execute("UPDATE rss_candidates SET status='needs_attention',error_code='presence_unconfirmed' WHERE id=?",[tv_candidate.clone()]).await?;
    let mh = "c".repeat(40);
    prepare(&c, &movie_candidate, identity("movies", &[&mh])).await?;
    let tx = c.transaction().await?;
    tx.execute(
        "INSERT INTO rss_hash_claims VALUES(?,?,?)",
        params![client.clone(), mh, movie_candidate.clone()],
    )
    .await?;
    tx.execute(
        "UPDATE rss_candidates SET status='submitting',private_payload=NULL WHERE id=?",
        [movie_candidate.clone()],
    )
    .await?;
    tx.commit().await?;
    // The confirmed remote handle may differ from every prepared identity hash.
    let remote_hash = "b".repeat(40);
    for invalid in [
        None,
        Some("B".repeat(40)),
        Some("b".repeat(39)),
        Some("g".repeat(64)),
    ] {
        assert!(
            c.execute(
                "UPDATE rss_candidates SET status='observed',observed_hash=? WHERE id=?",
                params![invalid, movie_candidate.clone()]
            )
            .await
            .is_err()
        );
    }
    c.execute(
        "UPDATE rss_candidates SET status='observed',observed_hash=? WHERE id=?",
        params![remote_hash.clone(), movie_candidate.clone()],
    )
    .await?;
    assert_eq!(
        c.query(
            "SELECT observed_hash FROM rss_candidates WHERE id=?",
            [movie_candidate.clone()]
        )
        .await?
        .next()
        .await?
        .unwrap()
        .get::<String>(0)?,
        remote_hash
    );
    assert!(
        c.execute(
            "UPDATE rss_candidates SET observed_hash=? WHERE id=?",
            params!["d".repeat(40), movie_candidate.clone()]
        )
        .await
        .is_err()
    );
    assert!(
        c.execute(
            "UPDATE rss_candidates SET observed_hash=? WHERE id=?",
            params![remote_hash.clone(), tv_candidate.clone()]
        )
        .await
        .is_err()
    );
    // A different typed target cannot acquire the same client's confirmed remote ID.
    let tx = c.transaction().await?;
    tx.execute_batch("INSERT INTO movie_metadata(id,tmdb_id,title) VALUES(2,202,'Other'); INSERT INTO movies(id,metadata_id,path) VALUES(2,2,'/other');").await?;
    let other = candidate(&tx, &movie, "movies", Some(29), false).await?;
    tx.execute(
        "UPDATE rss_candidates SET movie_id=2 WHERE id=?",
        [other.clone()],
    )
    .await?;
    let mut other_identity: serde_json::Value =
        serde_json::from_str(&identity("movies", &[&"d".repeat(64)]))?;
    other_identity["target"]["id"] = 2.into();
    prepare(&tx, &other, other_identity.to_string()).await?;
    tx.execute(
        "INSERT INTO rss_hash_claims VALUES(?,?,?)",
        params![client.clone(), "d".repeat(64), other.clone()],
    )
    .await?;
    tx.execute(
        "UPDATE rss_candidates SET status='submitting',private_payload=NULL WHERE id=?",
        [other.clone()],
    )
    .await?;
    assert!(
        tx.execute(
            "UPDATE rss_candidates SET status='observed',observed_hash=? WHERE id=?",
            params![remote_hash, other.clone()]
        )
        .await
        .is_err()
    );
    tx.execute(
        "UPDATE rss_candidates SET status='observed',observed_hash=? WHERE id=?",
        params!["e".repeat(64), other],
    )
    .await?;
    tx.rollback().await?;
    for (domain, cmd) in [("tv", &tv), ("movies", &movie)] {
        let other = candidate(&c, cmd, domain, Some(29), true).await?;
        assert!(
            prepare(&c, &other, identity(domain, &[&"d".repeat(40)]))
                .await
                .is_err()
        );
        c.execute("UPDATE rss_candidates SET status='rejected',private_payload=NULL,error_code='target_conflict' WHERE id=?",[other.clone()]).await?;
        c.execute("DELETE FROM rss_candidates WHERE id=?", [other])
            .await?;
        let cancelled = candidate(&c, cmd, domain, Some(29), true).await?;
        c.execute(
            "UPDATE rss_candidates SET status='cancelled',private_payload=NULL WHERE id=?",
            [cancelled.clone()],
        )
        .await?;
        c.execute("DELETE FROM rss_candidates WHERE id=?", [cancelled])
            .await?;
    }
    assert!(c.execute("DELETE FROM rss_hash_claims", ()).await.is_err());
    assert!(
        c.execute(
            "DELETE FROM rss_candidates WHERE id=?",
            [movie_candidate.clone()]
        )
        .await
        .is_err()
    );
    // Ownership spans clients, even when a second candidate has a different hash.
    let tx = c.transaction().await?;
    let other_client = super::refresh_tests::provider(&tx).await?;
    let other_command = command(&tx, &indexer, &other_client, "tv").await?;
    let cross_client = candidate(&tx, &other_command, "tv", Some(29), true).await?;
    assert!(
        prepare(&tx, &cross_client, identity("tv", &[&"e".repeat(40)]))
            .await
            .is_err()
    );
    tx.rollback().await?;
    // Aggregate encrypted payload budget cannot be exceeded or restored after release.
    let tx = c.transaction().await?;
    let mut held = vec![];
    for _ in 0..255 {
        held.push(candidate(&tx, &tv, "tv", Some(65565), false).await?);
    }
    assert!(candidate(&tx, &tv, "tv", Some(65565), false).await.is_err());
    tx.execute(
        "UPDATE rss_candidates SET status='cancelled',private_payload=NULL WHERE id=?",
        [held.pop().unwrap()],
    )
    .await?;
    candidate(&tx, &tv, "tv", Some(65565), false).await?;
    tx.rollback().await?;
    let tx = c.transaction().await?;
    for _ in 0..1022 {
        candidate(&tx, &tv, "tv", None, false).await?;
    }
    assert!(candidate(&tx, &tv, "tv", None, false).await.is_err());
    tx.rollback().await?;
    // All four concrete command APIs share one race-safe storage admission budget.
    let tx = c.transaction().await?;
    tx.execute(
        "UPDATE rss_commands SET status='cancelled',completed_at=100",
        (),
    )
    .await?;
    for _ in 0..1020 {
        let id = command(&tx, &indexer, &client, "tv").await?;
        tx.execute(
            "UPDATE rss_commands SET status='cancelled',completed_at=100 WHERE id=?",
            [id],
        )
        .await?;
    }
    let total = "SELECT (SELECT count(*) FROM commands)+(SELECT count(*) FROM metadata_refresh_commands)+(SELECT count(*) FROM blocklist_clear_commands)+(SELECT count(*) FROM rss_commands)";
    assert_eq!(scalar(&tx, total).await?, 1024);
    assert!(
        command(&tx, &indexer, &client, "tv")
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
    assert!(tx.execute("INSERT INTO metadata_refresh_commands(id,name,media_type,series_id,external_id,next_attempt_at,created_at) VALUES(?,'refresh_series','tv',1,101,100,100)",[uuid::Uuid::new_v4().to_string()]).await.unwrap_err().to_string().contains("command capacity reached"));
    assert!(tx.execute("INSERT INTO blocklist_clear_commands(id,name,media_type,next_attempt_at,created_at) VALUES(?,'clear_blocklist','movies',100,100)",[uuid::Uuid::new_v4().to_string()]).await.unwrap_err().to_string().contains("command capacity reached"));
    tx.execute("DELETE FROM rss_commands WHERE id=?", [tv.clone()])
        .await?;
    command(&tx, &indexer, &client, "tv").await?;
    tx.rollback().await?;
    let schedule = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO rss_schedules(id,media_type,indexer_id,indexer_revision,client_id,client_revision,interval_seconds,enabled,next_run_at,created_at) VALUES(?,'tv',?,1,?,1,60,1,100,100)",params![schedule,indexer.clone(),client.clone()]).await?;
    c.execute("UPDATE providers SET revision=2 WHERE id=?", [indexer])
        .await?;
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM rss_schedules WHERE enabled=0 AND error_code='provider_changed'"
        )
        .await?,
        1
    );
    for cmd in [&tv, &movie] {
        c.execute(
            "UPDATE rss_commands SET status='running',attempts=1,started_at=100 WHERE id=?",
            [cmd.clone()],
        )
        .await?;
        c.execute(
            "UPDATE rss_commands SET fetch_complete=1 WHERE id=?",
            [cmd.clone()],
        )
        .await?;
        assert!(
            c.execute(
                "UPDATE rss_commands SET fetch_complete=0 WHERE id=?",
                [cmd.clone()]
            )
            .await
            .is_err()
        );
        c.execute(
            "UPDATE rss_commands SET status='succeeded',completed_at=100 WHERE id=?",
            [cmd.clone()],
        )
        .await?;
        c.execute("DELETE FROM rss_commands WHERE id=?", [cmd.clone()])
            .await?;
    }
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM rss_candidates WHERE command_id IS NULL"
        )
        .await?,
        2
    );
    assert_eq!(scalar(&c, "SELECT count(*) FROM rss_hash_claims").await?, 3);
    integrity(&c).await?;
    drop(c);
    drop(db);
    let db = Database::open_local(&path).await?;
    assert!(db.migration_backup().is_none());
    let c = db.connect().await?;
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM rss_candidates WHERE status='observed'"
        )
        .await?,
        1
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM rss_candidates WHERE status='needs_attention'"
        )
        .await?,
        1
    );
    Ok(())
}
