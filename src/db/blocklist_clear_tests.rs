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
    let mut result = vec![];
    while let Some(row) = rows.next().await? {
        result.push(
            (0..row.column_count())
                .map(|i| row.get_value(i))
                .collect::<Result<Vec<_>, _>>()?,
        );
    }
    Ok(result)
}
async fn clear(c: &Connection, domain: &str) -> Result<String, libsql::Error> {
    let id = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO blocklist_clear_commands(id,name,media_type,next_attempt_at,created_at) VALUES(?,'clear_blocklist',?,100,100)",params![id.clone(),domain]).await?;
    Ok(id)
}
async fn metadata(c: &Connection) -> Result<String, libsql::Error> {
    let id = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO metadata_refresh_commands(id,name,media_type,series_id,external_id,next_attempt_at,created_at) VALUES(?,'refresh_series','tv',1,101,100,100)",[id.clone()]).await?;
    Ok(id)
}
const TOTAL: &str = "SELECT (SELECT count(*) FROM commands)+(SELECT count(*) FROM metadata_refresh_commands)+(SELECT count(*) FROM blocklist_clear_commands)";
#[tokio::test]
async fn blocklist_clear_schema22_upgrade_rollback_reopen_scopes_and_shared_capacity()
-> Result<(), Error> {
    let files = Scratch(
        std::env::temp_dir().join(format!("hrrdarr-clear-schema-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&files.0)?;
    let path = files.0.join("db");
    let raw = libsql::Builder::new_local(&path).build().await?;
    let c = raw.connect()?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    c.execute(HISTORY_SQL, ()).await?;
    for (i, (name, sql)) in MIGRATIONS.iter().take(22).enumerate() {
        c.execute_batch(sql).await?;
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
            params![i as i64 + 1, *name, checksum(sql), *sql],
        )
        .await?;
    }
    c.execute_batch("INSERT INTO series(id,tvdb_id,title,path) VALUES(1,101,'TV','/tv');INSERT INTO seasons VALUES(1,1,1);INSERT INTO episodes(id,series_id,season,number,title) VALUES(1,1,1,1,'One');INSERT INTO movie_metadata(id,tmdb_id,title) VALUES(7,101,'Movie');INSERT INTO movies(id,metadata_id,path) VALUES(1,7,'/movie');").await?;
    let provider = provider(&c).await?;
    let download = enqueue(&c, &provider, "tv").await?;
    succeed(&c, &download, 1).await?;
    snapshot(
        &c,
        &provider,
        "tv",
        &download,
        r#"[{"domain":"tv","hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}]"#,
    )
    .await?;
    schedule(&c, &provider, "tv").await?;
    let metadata_id = metadata(&c).await?;
    for (app, domain) in [("sonarr", "tv"), ("radarr", "movies")] {
        let fingerprint = if app == "sonarr" {
            "a".repeat(64)
        } else {
            "b".repeat(64)
        };
        c.execute("INSERT INTO snapshot_imports(application,fingerprint,schema_version,blocklist_version) VALUES(?,?,?,1)",params![app,fingerprint.clone(),if app=="sonarr"{233}else{242}]).await?;
        c.execute(
            "INSERT INTO snapshot_records VALUES(?,?,'Blocklist',0,'{\"private\":\"retained\"}')",
            params![app, fingerprint.clone()],
        )
        .await?;
        c.execute("INSERT INTO snapshot_blocklist(application,fingerprint,source_id,facts_digest) VALUES(?,?,1,?)",params![app,fingerprint.clone(),"c".repeat(64)]).await?;
        c.execute("INSERT INTO blocklist_entries(application,fingerprint,source_id,media_type,series_id,movie_id,occurred_at,source_title) VALUES(?,?,1,?,?,?,'2026-01-01 00:00:00','Release')",params![app,fingerprint.clone(),domain,(domain=="tv").then_some(1),(domain=="movies").then_some(1)]).await?;
        if domain == "tv" {
            c.execute(
                "INSERT INTO blocklist_episodes VALUES(?,?,1,1,1)",
                params![app, fingerprint.clone()],
            )
            .await?;
            c.execute("INSERT INTO snapshot_history_events(application,fingerprint,source_id,media_type,episode_id,occurred_at,event_type,source_event_type) VALUES(?,?,1,'episode',1,'2026-01-01 00:00:00','grabbed',1)",params![app,fingerprint]).await?;
        }
    }
    let tables = [
        "commands",
        "metadata_refresh_commands",
        "download_refresh_schedules",
        "download_refresh_snapshots",
        "snapshot_imports",
        "snapshot_records",
        "snapshot_history_events",
        "snapshot_blocklist",
        "blocklist_entries",
        "blocklist_episodes",
    ];
    let mut before = vec![];
    for table in tables {
        before.push(rows(&c, table).await?);
    }
    let tx = c.transaction().await?;
    tx.execute_batch(MIGRATIONS[22].1).await?;
    clear(&tx, "tv").await?;
    assert!(tx.execute_batch(MIGRATIONS[22].1).await.is_err());
    tx.rollback().await?;
    assert_eq!(version(&c).await?, 22);
    assert!(
        c.query("SELECT * FROM blocklist_clear_commands", ())
            .await
            .is_err()
    );
    for (i, table) in tables.iter().enumerate() {
        assert_eq!(rows(&c, table).await?, before[i]);
    }
    drop(c);
    drop(raw);
    let db = Database::open_local(&path).await?;
    assert!(db.migration_backup().is_some());
    let c = db.connect().await?;
    assert_eq!(version(&c).await?, 45); // Latest includes AutoTagging metadata45; historical migration prefixes stay unchanged.
    for (i, table) in tables.iter().enumerate() {
        let mut expected = before[i].clone();
        if *table == "snapshot_imports" {
            // Migrations36 through41 and45 append seven inactive metadata markers; retain every prior field and assert the new AutoTagging marker is0.
            for row in &mut expected {
                row.extend([
                    libsql::Value::Integer(0),
                    libsql::Value::Integer(0),
                    libsql::Value::Integer(0),
                    libsql::Value::Integer(0),
                    libsql::Value::Integer(0),
                    libsql::Value::Integer(0),
                    libsql::Value::Integer(0),
                ]);
            }
        }
        if *table == "download_refresh_schedules" {
            //0041 appends explicit intent and preserves the old effective bit as requested intent.
            for row in &mut expected {
                let enabled = row[3].clone();
                row.extend([libsql::Value::Text("explicit".into()), enabled]);
            }
        }
        assert_eq!(rows(&c, table).await?, expected);
    }
    assert!(clear(&c, "all").await.is_err());
    let tv = clear(&c, "tv").await?;
    let movie = clear(&c, "movies").await?;
    assert!(clear(&c, "tv").await.is_err());
    assert!(c.execute("UPDATE blocklist_clear_commands SET media_type='movies',status='cancelled',completed_at=100 WHERE id=?",[tv.clone()]).await.is_err());
    assert!(
        c.execute(
            "UPDATE blocklist_clear_commands SET status='running',started_at=100 WHERE id=?",
            [tv.clone()]
        )
        .await
        .is_err()
    );
    assert!(
        c.execute(
            "DELETE FROM blocklist_clear_commands WHERE id=?",
            [tv.clone()]
        )
        .await
        .is_err()
    );
    c.execute(
        "UPDATE blocklist_clear_commands SET status='running',attempts=1,started_at=100 WHERE id=?",
        [tv.clone()],
    )
    .await?;
    for sql in [
        "UPDATE blocklist_clear_commands SET records_removed=1 WHERE id=?",
        "UPDATE blocklist_clear_commands SET status='succeeded',completed_at=100,records_removed=-1 WHERE id=?",
        "UPDATE blocklist_clear_commands SET status='succeeded',completed_at=100,records_removed=9007199254740992 WHERE id=?",
        "UPDATE blocklist_clear_commands SET priority=1 WHERE id=?",
        "UPDATE blocklist_clear_commands SET created_at=101 WHERE id=?",
    ] {
        assert!(c.execute(sql, [tv.clone()]).await.is_err());
    }
    // Clear and completion are one transaction: rollback restores entries, links, tombstones and status.
    let tx = c.transaction().await?;
    assert_eq!(
        tx.execute("DELETE FROM blocklist_entries WHERE media_type='tv'", ())
            .await?,
        1
    );
    tx.execute("UPDATE blocklist_clear_commands SET status='succeeded',records_removed=1,completed_at=100 WHERE id=?",[tv.clone()]).await?;
    tx.rollback().await?;
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM blocklist_entries").await?,
        2
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM snapshot_blocklist WHERE removed_at IS NOT NULL"
        )
        .await?,
        0
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM blocklist_episodes").await?,
        1
    );
    let tx = c.transaction().await?;
    tx.execute("DELETE FROM blocklist_entries WHERE media_type='tv'", ())
        .await?;
    tx.execute("UPDATE blocklist_clear_commands SET status='succeeded',records_removed=1,completed_at=100 WHERE id=?",[tv.clone()]).await?;
    tx.commit().await?;
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM blocklist_entries WHERE media_type='movies'"
        )
        .await?,
        1
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM snapshot_blocklist WHERE removed_at IS NOT NULL"
        )
        .await?,
        1
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM snapshot_history_events").await?,
        1
    );
    for sql in [
        "UPDATE blocklist_clear_commands SET records_removed=2 WHERE id=?",
        "UPDATE blocklist_clear_commands SET status='queued',attempts=0,started_at=NULL,completed_at=NULL,records_removed=0 WHERE id=?",
    ] {
        assert!(c.execute(sql, [tv.clone()]).await.is_err());
    }
    c.execute(
        "UPDATE blocklist_clear_commands SET status='cancelled',completed_at=100 WHERE id=?",
        [movie],
    )
    .await?;
    let retry = clear(&c, "tv").await?;
    for attempt in 1..=3 {
        c.execute("UPDATE blocklist_clear_commands SET status='running',attempts=?,started_at=100,error_code=NULL WHERE id=?",params![attempt,retry.clone()]).await?;
        let result=c.execute("UPDATE blocklist_clear_commands SET status='retry_wait',error_code='interrupted' WHERE id=?",[retry.clone()]).await;
        if attempt < 3 {
            result?;
        } else {
            assert!(result.is_err());
        }
    }
    assert!(c.execute("UPDATE blocklist_clear_commands SET status='failed',error_code='private error',completed_at=100 WHERE id=?",[retry.clone()]).await.is_err());
    c.execute("UPDATE blocklist_clear_commands SET status='failed',error_code='storage_error',completed_at=100 WHERE id=?",[retry]).await?;
    c.execute(
        "UPDATE metadata_refresh_commands SET status='cancelled',completed_at=100 WHERE id=?",
        [metadata_id],
    )
    .await?;
    let mut total = scalar(&c, TOTAL).await?;
    while total < 1024 {
        match total % 3 {
            0 => {
                let id = clear(&c, "tv").await?;
                c.execute("UPDATE blocklist_clear_commands SET status='cancelled',completed_at=100 WHERE id=?",[id]).await?;
            }
            1 => {
                let id = metadata(&c).await?;
                c.execute("UPDATE metadata_refresh_commands SET status='cancelled',completed_at=100 WHERE id=?",[id]).await?;
            }
            _ => {
                let id = enqueue(&c, &provider, "movies").await?;
                c.execute(
                    "UPDATE commands SET status='cancelled',completed_at=100 WHERE id=?",
                    [id],
                )
                .await?;
            }
        }
        total += 1;
    }
    // Every one of those 1024 historical rows is terminal (migration 0033: active rows only count
    // toward the shared pool), so the pool is not actually full yet -- pad it with genuinely active
    // search_commands rows (no per-target uniqueness, unlike the tables above) to prove admission
    // is really rejected once every pool table combined reaches 1024.
    let search_indexer = super::refresh_tests::search_indexer(&c, "tv").await?;
    let active = scalar(
        &c,
        &format!("SELECT {}", super::refresh_tests::POOL_ACTIVE_SQL),
    )
    .await?;
    let padding = super::refresh_tests::pad_search_commands(
        &c,
        &search_indexer,
        &provider,
        "tv",
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
        clear(&c, "tv")
            .await
            .unwrap_err()
            .to_string()
            .contains("command capacity reached")
    );
    assert!(
        metadata(&c)
            .await
            .unwrap_err()
            .to_string()
            .contains("command capacity reached")
    );
    assert!(
        enqueue(&c, &provider, "movies")
            .await
            .unwrap_err()
            .to_string()
            .contains("command capacity reached")
    );
    assert_eq!(scalar(&c, TOTAL).await?, 1024);
    c.execute("DELETE FROM blocklist_clear_commands WHERE id=?", [tv])
        .await?;
    // The DELETE above only removed an already-terminal row, so it did not free the pool (migration
    // 0033): the pool is still exactly full.
    assert!(
        clear(&c, "movies")
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
    let slot = clear(&c, "movies").await?;
    assert!(
        c.execute(
            "DELETE FROM blocklist_clear_commands WHERE id=?",
            [slot.clone()]
        )
        .await
        .is_err()
    );
    c.execute(
        "UPDATE blocklist_clear_commands SET status='cancelled',completed_at=100 WHERE id=?",
        [slot],
    )
    .await?;
    integrity(&c).await?;
    drop(c);
    drop(db);
    let db = Database::open_local(&path).await?;
    assert!(db.migration_backup().is_none());
    let c = db.connect().await?;
    assert_eq!(scalar(&c, TOTAL).await?, 1024);
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM snapshot_blocklist WHERE removed_at IS NOT NULL"
        )
        .await?,
        1
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM blocklist_entries WHERE media_type='movies'"
        )
        .await?,
        1
    );
    Ok(())
}
