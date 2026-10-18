use super::rss_tests::{candidate, command, identity, prepare};
use super::*;

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
#[tokio::test]
async fn candidate_comparison_facts_upgrade_rollback_submission_freeze_and_reopen()
-> Result<(), Error> {
    let files = Scratch(
        std::env::temp_dir().join(format!("hrrdarr-candidate-facts-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&files.0)?;
    let path = files.0.join("db");
    let raw = libsql::Builder::new_local(&path).build().await?;
    let c = raw.connect()?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    c.execute(HISTORY_SQL, ()).await?;
    for (i, (name, sql)) in MIGRATIONS.iter().take(34).enumerate() {
        c.execute_batch(sql).await?;
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
            params![i as i64 + 1, *name, checksum(sql), *sql],
        )
        .await?;
    }
    c.execute_batch("INSERT INTO series(id,tvdb_id,title,path,original_language) VALUES(1,101,'TV','/tv',26);INSERT INTO seasons VALUES(1,1,1);INSERT INTO episodes(id,series_id,season,number,title) VALUES(1,1,1,1,'One');INSERT INTO movie_metadata(id,tmdb_id,title,year) VALUES(1,101,'Film',2020);INSERT INTO movies(id,metadata_id,path) VALUES(1,1,'/movie');").await?;
    let client = super::refresh_tests::provider(&c).await?;
    let indexer = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'torznab','Indexer',1,1,1,1,'http://127.0.0.1:1/')",[indexer.clone()]).await?;
    for media in ["tv", "movies"] {
        c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year) VALUES(?,'torznab',?,'[5000]','[]',?,?)",params![indexer.clone(),media,(media=="tv").then_some(0),(media=="movies").then_some(0)]).await?;
    }
    let tv_command = command(&c, &indexer, &client, "tv").await?;
    let movie_command = command(&c, &indexer, &client, "movies").await?;
    let tv = candidate(&c, &tv_command, "tv", Some(29), true).await?;
    let movie = candidate(&c, &movie_command, "movies", Some(29), true).await?;
    for (media, id, hash) in [
        ("tv", &tv, "a".repeat(40)),
        ("movies", &movie, "b".repeat(40)),
    ] {
        prepare(&c, id, identity(media, &[&hash])).await?;
        c.execute(
            "INSERT INTO rss_hash_claims VALUES(?,?,?)",
            params![client.clone(), hash, id.clone()],
        )
        .await?;
    }
    c.execute(
        "UPDATE rss_candidates SET status='submitting',private_payload=NULL WHERE id=?",
        [movie.clone()],
    )
    .await?;
    let tx = c.transaction().await?;
    tx.execute_batch(MIGRATIONS[34].1).await?;
    assert!(tx.execute_batch(MIGRATIONS[34].1).await.is_err());
    tx.rollback().await?;
    assert_eq!(version(&c).await?, 34);
    assert!(
        c.query("SELECT comparison_facts_json FROM rss_candidates", ())
            .await
            .is_err()
    );
    drop(c);
    drop(raw);
    let db = Database::open_local(&path).await?;
    assert!(db.migration_backup().is_some());
    let c = db.connect().await?;
    assert_eq!(version(&c).await?, 49); // Reasoning: latest migration is now 0049 indexer client binding (was 48: indexer operation policy); historical migration prefixes stay fixed.
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM rss_candidates WHERE comparison_facts_json IS NULL"
        )
        .await?,
        2
    );
    assert_eq!(
        scalar(&c, "SELECT original_language FROM series").await?,
        26
    );
    let facts =
        serde_json::json!({"version":1,"title":"Release","languages":[26],"indexer_flags":8})
            .to_string();
    let extra = uuid::Uuid::new_v4().to_string();
    let insert = "INSERT INTO rss_candidates(id,command_id,media_type,indexer_id,indexer_revision,client_id,client_revision,fingerprint,title,status,decision_reasons_json,created_at,updated_at,comparison_facts_json) SELECT ?,id,media_type,indexer_id,indexer_revision,client_id,client_revision,?,'Rejected','rejected','[]',100,100,? FROM rss_commands WHERE id=?";
    assert!(
        c.execute(
            insert,
            params![
                extra.clone(),
                "c".repeat(64),
                facts.clone(),
                tv_command.clone()
            ]
        )
        .await
        .is_err()
    );
    c.execute(
        insert,
        params![
            extra.clone(),
            "c".repeat(64),
            Option::<String>::None,
            tv_command.clone()
        ],
    )
    .await?;
    c.execute("DELETE FROM rss_candidates WHERE id=?", [extra])
        .await?;
    // Already-submitted legacy receipts cannot invent facts, even while changing status legally.
    assert!(c.execute("UPDATE rss_candidates SET status='reconciling',attempts=1,comparison_facts_json=? WHERE id=?",params![facts.clone(),movie.clone()]).await.is_err());
    assert!(
        c.execute(
            "UPDATE rss_candidates SET comparison_facts_json=? WHERE id=?",
            params![facts.clone(), tv.clone()]
        )
        .await
        .is_err()
    );
    for bad in [
        "{}".to_string(),
        "[]".into(),
        "invalid".into(),
        "{\"version\":2}".into(),
        "{\"version\":1.0}".into(),
        "{\"version\":null}".into(),
        format!("{{\"version\":1,\"title\":\"{}\"}}", "x".repeat(16384)),
    ] {
        assert!(c.execute("UPDATE rss_candidates SET status='submitting',private_payload=NULL,comparison_facts_json=? WHERE id=?",params![bad,tv.clone()]).await.is_err());
    }
    // Private data erasure and factual capture are atomic, including a late rollback.
    let tx = c.transaction().await?;
    tx.execute("UPDATE rss_candidates SET status='submitting',private_payload=NULL,comparison_facts_json=? WHERE id=?",params![facts.clone(),tv.clone()]).await?;
    tx.rollback().await?;
    assert_eq!(scalar(&c,"SELECT count(*) FROM rss_candidates WHERE status='prepared' AND private_payload IS NOT NULL AND comparison_facts_json IS NULL").await?,1);
    c.execute("UPDATE rss_candidates SET status='submitting',private_payload=NULL,comparison_facts_json=? WHERE id=?",params![facts.clone(),tv.clone()]).await?;
    let different = serde_json::json!({"version":1,"title":"Changed"}).to_string();
    for replacement in [Some(different), None] {
        assert!(c.execute("UPDATE rss_candidates SET status='reconciling',attempts=1,comparison_facts_json=? WHERE id=?",params![replacement,tv.clone()]).await.is_err());
    }
    c.execute(
        "UPDATE rss_candidates SET status='observed',observed_hash=? WHERE id=?",
        params!["a".repeat(40), tv.clone()],
    )
    .await?;
    c.execute(
        "UPDATE rss_candidates SET status='reconciling',attempts=1 WHERE id=?",
        [movie.clone()],
    )
    .await?;
    // Detaching completed command history must preserve facts too.
    c.execute(
        "UPDATE rss_candidates SET command_id=NULL WHERE id=?",
        [tv.clone()],
    )
    .await?;
    integrity(&c).await?;
    drop(c);
    drop(db);
    let db = Database::open_local(&path).await?;
    assert!(db.migration_backup().is_none());
    let c = db.connect().await?;
    let row=c.query("SELECT comparison_facts_json,private_payload,status,command_id FROM rss_candidates WHERE id=?",[tv]).await?.next().await?.unwrap();
    assert_eq!(row.get::<String>(0)?, facts);
    assert_eq!(row.get::<Option<Vec<u8>>>(1)?, None);
    assert_eq!(row.get::<String>(2)?, "observed");
    assert_eq!(row.get::<Option<String>>(3)?, None);
    assert_eq!(scalar(&c,"SELECT count(*) FROM rss_candidates WHERE media_type='movies' AND comparison_facts_json IS NULL AND status='reconciling'").await?,1);
    integrity(&c).await?;
    Ok(())
}
