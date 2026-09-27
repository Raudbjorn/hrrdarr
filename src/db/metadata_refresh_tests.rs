use super::refresh_tests::{enqueue, provider, schedule, snapshot, succeed};
use super::*;
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
async fn rows(c: &Connection, table: &str) -> Result<Vec<Vec<libsql::Value>>, Error> {
    let mut rows = c
        .query(&format!("SELECT * FROM {table} ORDER BY rowid"), ())
        .await?;
    let mut result = Vec::new();
    while let Some(row) = rows.next().await? {
        result.push(
            (0..row.column_count())
                .map(|i| row.get_value(i))
                .collect::<Result<Vec<_>, _>>()?,
        );
    }
    Ok(result)
}
async fn metadata(c: &Connection, tv: bool) -> Result<String, libsql::Error> {
    let id = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO metadata_refresh_commands(id,name,media_type,series_id,movie_id,external_id,metadata_id,next_attempt_at,created_at) VALUES(?,?,?,?,?,101,?,100,100)",params![id.clone(),if tv{"refresh_series"}else{"refresh_movie"},if tv{"tv"}else{"movies"},tv.then_some(1),(!tv).then_some(1),(!tv).then_some(7)]).await?;
    Ok(id)
}
#[tokio::test]
async fn metadata_commands_preserve_download_state_upgrade_rollback_and_shared_capacity()
-> Result<(), Error> {
    let files = Scratch(
        std::env::temp_dir().join(format!("hrrdarr-metadata-schema-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&files.0)?;
    let path = files.0.join("db");
    let raw = libsql::Builder::new_local(&path).build().await?;
    let c = raw.connect()?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    c.execute(HISTORY_SQL, ()).await?;
    for (i, (name, sql)) in MIGRATIONS.iter().take(20).enumerate() {
        c.execute_batch(sql).await?;
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
            params![i as i64 + 1, *name, checksum(sql), *sql],
        )
        .await?;
    }
    let provider = provider(&c).await?;
    let command = enqueue(&c, &provider, "tv").await?;
    succeed(&c, &command, 1).await?;
    snapshot(
        &c,
        &provider,
        "tv",
        &command,
        r#"[{"domain":"tv","hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}]"#,
    )
    .await?;
    schedule(&c, &provider, "tv").await?;
    enqueue(&c, &provider, "movies").await?;
    c.execute_batch("INSERT INTO series(id,tvdb_id,title,path) VALUES(1,101,'TV','/tv');INSERT INTO movie_metadata(id,tmdb_id,title) VALUES(7,101,'Movie');INSERT INTO movies(id,metadata_id,path) VALUES(1,7,'/movie');").await?;
    let mut before = Vec::new();
    for table in [
        "commands",
        "download_refresh_schedules",
        "download_refresh_snapshots",
    ] {
        before.push(rows(&c, table).await?);
    }
    let tx = c.transaction().await?;
    tx.execute_batch(MIGRATIONS[20].1).await?;
    metadata(&tx, true).await?;
    assert!(tx.execute_batch(MIGRATIONS[20].1).await.is_err());
    tx.rollback().await?;
    assert_eq!(version(&c).await?, 20);
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM sqlite_schema WHERE name='metadata_refresh_commands'"
        )
        .await?,
        0
    );
    for (i, table) in [
        "commands",
        "download_refresh_schedules",
        "download_refresh_snapshots",
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(rows(&c, table).await?, before[i]);
    }
    drop(c);
    drop(raw);
    let db = Database::open_local(&path).await?;
    assert!(db.migration_backup().is_some());
    let c = db.connect().await?;
    assert_eq!(version(&c).await?, 38); // Latest open adds revision preference; historical migration starts remain unchanged.
    for (i, table) in [
        "commands",
        "download_refresh_schedules",
        "download_refresh_snapshots",
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(rows(&c, table).await?, before[i]);
    }
    let tv = metadata(&c, true).await?;
    let movie = metadata(&c, false).await?;
    assert!(metadata(&c, true).await.is_err());
    assert!(
        c.execute(
            "UPDATE metadata_refresh_commands SET movie_id=1 WHERE id=?",
            [tv.clone()]
        )
        .await
        .is_err()
    );
    assert!(
        c.execute(
            "DELETE FROM metadata_refresh_commands WHERE id=?",
            [tv.clone()]
        )
        .await
        .is_err()
    );
    c.execute("UPDATE metadata_refresh_commands SET status='running',attempts=1,started_at=100 WHERE id=?",[tv.clone()]).await?;
    c.execute("UPDATE series SET tvdb_id=202 WHERE id=1", ())
        .await?;
    assert!(c.execute("UPDATE metadata_refresh_commands SET status='succeeded',completed_at=101,records_updated=1 WHERE id=?",[tv.clone()]).await.is_err());
    c.execute("UPDATE metadata_refresh_commands SET status='failed',completed_at=101,error_code='target_changed' WHERE id=?",[tv.clone()]).await?;
    c.execute(
        "UPDATE metadata_refresh_commands SET status='cancelled',completed_at=101 WHERE id=?",
        [movie],
    )
    .await?;
    c.execute("DELETE FROM series WHERE id=1", ()).await?;
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM metadata_refresh_commands WHERE series_id=1"
        )
        .await?,
        1
    );
    c.execute(
        "INSERT INTO series(id,tvdb_id,title,path) VALUES(1,101,'TV','/tv')",
        (),
    )
    .await?;
    // Both concrete command kinds contribute to one retained-history cap.
    for i in 0..1020 {
        if i % 2 == 0 {
            let id = metadata(&c, true).await?;
            c.execute("UPDATE metadata_refresh_commands SET status='cancelled',completed_at=100 WHERE id=?",[id]).await?;
        } else {
            let id = enqueue(&c, &provider, "tv").await?;
            c.execute(
                "UPDATE commands SET status='cancelled',completed_at=100 WHERE id=?",
                [id],
            )
            .await?;
        }
    }
    assert_eq!(scalar(&c,"SELECT (SELECT count(*) FROM commands)+(SELECT count(*) FROM metadata_refresh_commands)").await?,1024);
    // Every terminal row above is invisible to the shared pool (migration 0033: active rows only
    // count), so it is not actually full yet -- pad it with genuinely active search_commands rows
    // (no per-target uniqueness) against the existing movie fixture to prove admission is really
    // rejected once every pool table combined reaches 1024.
    let search_indexer = super::refresh_tests::search_indexer(&c, "movies").await?;
    let active = scalar(
        &c,
        &format!("SELECT {}", super::refresh_tests::POOL_ACTIVE_SQL),
    )
    .await?;
    let padding = super::refresh_tests::pad_search_commands(
        &c,
        &search_indexer,
        &provider,
        "movies",
        1024 - active,
    )
    .await?;
    assert_eq!(
        scalar(
            &c,
            &format!("SELECT {}", super::refresh_tests::POOL_ACTIVE_SQL)
        )
        .await?,
        1024
    );
    assert!(
        metadata(&c, true)
            .await
            .unwrap_err()
            .to_string()
            .contains("command capacity reached")
    );
    assert!(
        enqueue(&c, &provider, "tv")
            .await
            .unwrap_err()
            .to_string()
            .contains("command capacity reached")
    );
    c.execute("DELETE FROM metadata_refresh_commands WHERE id=?", [tv])
        .await?;
    // The DELETE above only removed an already-terminal row, so it did not free the pool (migration
    // 0033): the pool is still exactly full.
    assert!(
        metadata(&c, true)
            .await
            .unwrap_err()
            .to_string()
            .contains("command capacity reached")
    );
    // Cancelling one active padding row instead proves capacity really does free up again.
    c.execute(
        "UPDATE search_commands SET status='cancelled',completed_at=100 WHERE id=?",
        [padding[0].clone()],
    )
    .await?;
    let id = metadata(&c, true).await?;
    c.execute(
        "UPDATE metadata_refresh_commands SET status='cancelled',completed_at=100 WHERE id=?",
        [id],
    )
    .await?;
    integrity(&c).await?;
    drop(c);
    drop(db);
    let db = Database::open_local(&path).await?;
    assert!(db.migration_backup().is_none());
    let c = db.connect().await?;
    assert_eq!(rows(&c, "download_refresh_snapshots").await?, before[2]);
    assert_eq!(rows(&c, "download_refresh_schedules").await?, before[1]);
    assert_eq!(scalar(&c,"SELECT (SELECT count(*) FROM commands)+(SELECT count(*) FROM metadata_refresh_commands)").await?,1024);
    integrity(&c).await?;
    Ok(())
}
