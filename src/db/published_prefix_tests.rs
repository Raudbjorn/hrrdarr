//! Current-main upgrades and fail-closed protection of the incompatible published history.
use super::*;
use std::path::PathBuf;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("hrrdarr-prefix-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
async fn record(c: &Connection, version: usize, name: &str, sql: &str) -> Result<(), Error> {
    c.execute_batch(sql).await?;
    c.execute(
        "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
        params![version as i64, name, checksum(sql), sql],
    )
    .await?;
    Ok(())
}
async fn history(c: &Connection) -> Result<String, Error> {
    Ok(c.query("SELECT json_group_array(json_array(version,name,checksum,sql)) FROM (SELECT * FROM schema_migrations ORDER BY version)", ()).await?.next().await?.unwrap().get(0)?)
}
#[tokio::test]
async fn current_main_prefix_upgrades_without_rewriting_history_or_intents() -> Result<(), Error> {
    let files = Scratch::new();
    let path = files.0.join("db");
    let raw = libsql::Builder::new_local(&path).build().await?;
    let c = raw.connect()?;
    // The real runner suspends foreign keys while rebuilding referenced tables.
    c.execute("PRAGMA foreign_keys=OFF", ()).await?;
    c.execute(HISTORY_SQL, ()).await?;
    for (i, (name, sql)) in MIGRATIONS.iter().take(78).enumerate() {
        record(&c, i + 1, name, sql).await?;
    }
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    c.execute("INSERT INTO movie_collection_intents(tmdb_id,revision,local_edit,removed,removal_reason) VALUES(42,7,1,1,'user')",()).await?;
    let before = history(&c).await?;
    drop(c);
    drop(raw);
    let db = Database::open_local(&path).await?;
    let c = db.connect().await?;
    assert_eq!(version(&c).await?, 81); // Main's 78 migrations plus Wanted, library health and Backup.
    let recorded: serde_json::Value = serde_json::from_str(&history(&c).await?)?;
    assert_eq!(
        &recorded.as_array().unwrap()[..78],
        serde_json::from_str::<Vec<serde_json::Value>>(&before)?.as_slice()
    );
    assert_eq!(scalar(&c,"SELECT count(*) FROM movie_collection_intents WHERE tmdb_id=42 AND revision=7 AND local_edit=1 AND removed=1 AND removal_reason='user'").await?,1);
    assert!(
        c.execute("DELETE FROM movie_collection_intents WHERE tmdb_id=42", ())
            .await
            .is_err()
    );
    drop(c);
    drop(db);
    let reopened = Database::open_local(&path).await?;
    assert_eq!(version(&reopened.connect().await?).await?, 81); // Reopening preserves the complete history.
    Ok(())
}
#[tokio::test]
async fn incompatible_published_history_is_rejected_without_changing_intents_or_history()
-> Result<(), Error> {
    let files = Scratch::new();
    let path = files.0.join("db");
    let raw = libsql::Builder::new_local(&path).build().await?;
    let c = raw.connect()?;
    // The real runner suspends foreign keys while rebuilding referenced tables.
    c.execute("PRAGMA foreign_keys=OFF", ()).await?;
    c.execute(HISTORY_SQL, ()).await?;
    for (i, (name, sql)) in MIGRATIONS.iter().take(49).enumerate() {
        record(&c, i + 1, name, sql).await?;
    }
    let old = [
        (
            "torrent_rss_indexer",
            include_str!("../../tests/fixtures/published-main-53/0050_torrent_rss_indexer.sql"),
        ),
        (
            "download_client_backoff",
            include_str!("../../tests/fixtures/published-main-53/0051_download_client_backoff.sql"),
        ),
        (
            "collections",
            include_str!("../../tests/fixtures/published-main-53/0052_collections.sql"),
        ),
        (
            "provider_authority",
            include_str!("../../tests/fixtures/published-main-53/0053_provider_authority.sql"),
        ),
    ];
    for (i, (name, sql)) in old.iter().enumerate() {
        record(&c, i + 50, name, sql).await?;
    }
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    c.execute("INSERT INTO movie_collection_intents(tmdb_id,revision,local_edit,removed,removal_reason) VALUES(42,7,1,1,'user')",()).await?;
    let before = history(&c).await?;
    drop(c);
    drop(raw);
    let error = match Database::open_local(&path).await {
        Ok(_) => panic!("incompatible history must not be silently rewritten"),
        Err(e) => e.to_string(),
    };
    assert!(
        error.contains("migration history mismatch at version 50"),
        "{error}"
    );
    let raw = libsql::Builder::new_local(&path).build().await?;
    let c = raw.connect()?;
    assert_eq!(history(&c).await?, before);
    assert_eq!(scalar(&c,"SELECT count(*) FROM movie_collection_intents WHERE tmdb_id=42 AND revision=7 AND local_edit=1 AND removed=1 AND removal_reason='user'").await?,1);
    assert!(
        c.execute("DELETE FROM movie_collection_intents WHERE tmdb_id=42", ())
            .await
            .is_err()
    );
    Ok(())
}
