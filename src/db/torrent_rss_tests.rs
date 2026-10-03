//! Migration 0050 (generic torrent RSS feed implementation): widened storage guards on fresh and
//! upgraded databases, data preservation, rollback, and the guards that stay deliberately narrow.
use super::*;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "hrrdarr-torrent-rss-schema-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
async fn rows(c: &Connection, table: &str) -> Result<Vec<Vec<libsql::Value>>, Error> {
    let mut cursor = c
        .query(&format!("SELECT * FROM {table} ORDER BY rowid"), ())
        .await?;
    let mut result = Vec::new();
    while let Some(row) = cursor.next().await? {
        result.push(
            (0..row.column_count())
                .map(|i| row.get_value(i))
                .collect::<std::result::Result<Vec<_>, _>>()?,
        );
    }
    Ok(result)
}
async fn count(c: &Connection, sql: &str) -> Result<i64, Error> {
    Ok(c.query(sql, ()).await?.next().await?.unwrap().get(0)?)
}
async fn text(c: &Connection, sql: &str) -> Result<String, Error> {
    Ok(c.query(sql, ()).await?.next().await?.unwrap().get(0)?)
}
async fn feed(c: &Connection, scopes: &[&str]) -> Result<String, Error> {
    let id = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'torrentrss','Feed',1,1,1,1,'http://127.0.0.1:9/feed')",[id.clone()]).await?;
    for media in scopes {
        c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,enable_rss,enable_automatic_search,enable_interactive_search,minimum_seeders) VALUES(?,'torrentrss',?,1,0,0,5)",params![id.clone(),*media]).await?;
    }
    Ok(id)
}
async fn indexer(c: &Connection, implementation: &str) -> Result<String, Error> {
    let id = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,?,'owned fixture',1,1,1,1,'http://127.0.0.1:9')",params![id.clone(),implementation]).await?;
    for media in ["tv", "movies"] {
        c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year) VALUES(?,?,?,'[5000]','[]',?,?)",params![id.clone(),implementation,media,(media=="tv").then_some(0),(media=="movies").then_some(0)]).await?;
    }
    Ok(id)
}
async fn at_49(path: &Path) -> Result<(libsql::Database, Connection), Error> {
    let raw = libsql::Builder::new_local(path).build().await?;
    let c = raw.connect()?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    c.execute(HISTORY_SQL, ()).await?;
    for (i, (name, sql)) in MIGRATIONS.iter().take(49).enumerate() {
        c.execute_batch(sql).await?;
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
            params![i as i64 + 1, *name, checksum(sql), *sql],
        )
        .await?;
    }
    Ok((raw, c))
}
async fn rejects(c: &Connection, sql: &str, args: impl libsql::params::IntoParams) -> bool {
    c.execute(sql, args).await.is_err()
}
async fn schema_text(c: &Connection) -> Result<Vec<Vec<libsql::Value>>, Error> {
    rows(c, "sqlite_schema").await
}

/// Exercises every guard the migration touched or deliberately left alone.
async fn assert_feed_contract(c: &Connection) -> Result<(), Error> {
    assert!(rejects(c, "INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES('11111111-1111-4111-8111-111111111111','bogus','x',1,1,1,1,'http://127.0.0.1:9')", ()).await, "unknown implementations stay rejected");
    let both = feed(c, &["tv", "movies"]).await?;
    // Stored shape: no categories, no client columns, search flags forced to zero.
    let bad = [
        "UPDATE provider_scopes SET enable_automatic_search=1 WHERE provider_id=?1 AND media_type='tv'",
        "UPDATE provider_scopes SET enable_interactive_search=1 WHERE provider_id=?1 AND media_type='tv'",
        "UPDATE provider_scopes SET categories='[5000]' WHERE provider_id=?1 AND media_type='tv'",
        "UPDATE provider_scopes SET anime_categories='[]' WHERE provider_id=?1 AND media_type='tv'",
        "UPDATE provider_scopes SET category='tv' WHERE provider_id=?1 AND media_type='tv'",
        "UPDATE provider_scopes SET recent_priority=0 WHERE provider_id=?1 AND media_type='tv'",
        "UPDATE provider_scopes SET anime_standard_format_search=0 WHERE provider_id=?1 AND media_type='tv'",
        "UPDATE provider_scopes SET remove_year=0 WHERE provider_id=?1 AND media_type='movies'",
        "UPDATE provider_scopes SET initial_state='started' WHERE provider_id=?1 AND media_type='tv'",
        "UPDATE provider_scopes SET add_tags=0 WHERE provider_id=?1 AND media_type='tv'",
        "UPDATE provider_scopes SET download_client_id='abcdefab-1234-5678-9abc-def012345678' WHERE provider_id=?1 AND media_type='tv'",
        "UPDATE provider_scopes SET minimum_seeders=-1 WHERE provider_id=?1 AND media_type='tv'",
        "UPDATE provider_scopes SET minimum_seeders=1000001 WHERE provider_id=?1 AND media_type='tv'",
        "UPDATE provider_scopes SET minimum_seeders='abc' WHERE provider_id=?1 AND media_type='tv'",
        "UPDATE provider_scopes SET minimum_seeders=1.5 WHERE provider_id=?1 AND media_type='tv'",
        "UPDATE provider_scopes SET enable_rss=2 WHERE provider_id=?1 AND media_type='tv'",
    ];
    for sql in bad {
        assert!(rejects(c, sql, [both.clone()]).await, "accepted {sql}");
    }
    // Both bounds are inclusive and RSS can be switched off without touching anything else.
    c.execute(
        "UPDATE provider_scopes SET minimum_seeders=0 WHERE provider_id=? AND media_type='tv'",
        [both.clone()],
    )
    .await?;
    c.execute("UPDATE provider_scopes SET minimum_seeders=1000000,enable_rss=0 WHERE provider_id=? AND media_type='movies'", [both.clone()]).await?;
    c.execute(
        "UPDATE provider_scopes SET minimum_seeders=NULL WHERE provider_id=?",
        [both.clone()],
    )
    .await?;
    // The minimum is a feed-only column: native indexers and clients can never carry one.
    let torznab = indexer(c, "torznab").await?;
    let client = super::refresh_tests::provider(c).await?;
    for id in [&torznab, &client] {
        assert!(
            rejects(
                c,
                "UPDATE provider_scopes SET minimum_seeders=1 WHERE provider_id=?",
                [id.clone()]
            )
            .await
        );
    }
    // Native shapes still reject feed-style rows (a torznab scope still needs categories).
    assert!(rejects(c, "INSERT INTO provider_scopes(provider_id,implementation,media_type,enable_automatic_search,enable_interactive_search) SELECT ?,'torznab','tv',0,0", [uuid::Uuid::new_v4().to_string()]).await);
    // A feed cannot masquerade as another implementation in its scope row.
    assert!(
        rejects(
            c,
            "UPDATE provider_scopes SET implementation='torznab' WHERE provider_id=?",
            [both.clone()]
        )
        .await
    );
    // Revision-bound test results accept feeds; the last-test record is invalidated by edits.
    c.execute("INSERT INTO provider_tests(provider_id,config_revision,tested_at,status,error_code) VALUES(?,1,100,'success',NULL)", [both.clone()]).await?;
    assert!(
        rejects(
            c,
            "UPDATE provider_tests SET config_revision=2 WHERE provider_id=?",
            [both.clone()]
        )
        .await
    );
    c.execute(
        "UPDATE providers SET revision=2,name='Feed2' WHERE id=?",
        [both.clone()],
    )
    .await?;
    assert_eq!(
        count(
            c,
            &format!("SELECT count(*) FROM provider_tests WHERE provider_id='{both}'")
        )
        .await?,
        0
    );
    c.execute("INSERT INTO provider_tests(provider_id,config_revision,tested_at,status,error_code) VALUES(?,2,101,'failure','invalid_response')", [both.clone()]).await?;
    // RSS schedules and commands admit a feed paired with a client; searching commands do not.
    c.execute("INSERT INTO rss_schedules(id,media_type,indexer_id,indexer_revision,client_id,client_revision,interval_seconds,enabled,next_run_at,created_at) VALUES(?,'tv',?,2,?,1,600,1,0,0)",params![uuid::Uuid::new_v4().to_string(),both.clone(),client.clone()]).await?;
    c.execute("INSERT INTO rss_schedules(id,media_type,indexer_id,indexer_revision,client_id,client_revision,interval_seconds,enabled,next_run_at,created_at) VALUES(?,'movies',?,2,?,1,600,1,0,0)",params![uuid::Uuid::new_v4().to_string(),both.clone(),client.clone()]).await?;
    assert!(rejects(c, "INSERT INTO rss_schedules(id,media_type,indexer_id,indexer_revision,client_id,client_revision,interval_seconds,enabled,next_run_at,created_at) VALUES(?,'tv',?,1,?,1,600,1,0,0)",params![uuid::Uuid::new_v4().to_string(),both.clone(),client.clone()]).await, "stale revision");
    assert!(rejects(c, "INSERT INTO rss_schedules(id,media_type,indexer_id,indexer_revision,client_id,client_revision,interval_seconds,enabled,next_run_at,created_at) VALUES(?,'tv',?,1,?,1,600,1,0,0)",params![uuid::Uuid::new_v4().to_string(),client.clone(),both.clone()]).await, "a feed is never a download client");
    let tv = super::rss_tests::command(c, &both, &client, "tv").await;
    assert!(tv.is_err(), "revision 1 is stale for the edited feed");
    let id = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO rss_commands(id,name,media_type,indexer_id,indexer_revision,client_id,client_revision,next_attempt_at,created_at) VALUES(?,'rss_sync','tv',?,2,?,1,100,100)",params![id,both.clone(),client.clone()]).await?;
    // Deleting the provider cascades its scopes, tests and schedules (unchanged FK behaviour).
    c.execute(
        "UPDATE rss_commands SET status='cancelled',completed_at=100",
        (),
    )
    .await?;
    c.execute("DELETE FROM rss_commands", ()).await?;
    c.execute("DELETE FROM providers WHERE id=?", [both.clone()])
        .await?;
    for table in ["provider_scopes", "provider_tests"] {
        assert_eq!(
            count(
                c,
                &format!("SELECT count(*) FROM {table} WHERE provider_id='{both}'")
            )
            .await?,
            0
        );
    }
    assert_eq!(
        count(
            c,
            &format!("SELECT count(*) FROM rss_schedules WHERE indexer_id='{both}'")
        )
        .await?,
        0
    );
    // The search admission guard is deliberately unchanged: feeds cannot search.
    assert!(
        !text(
            c,
            "SELECT sql FROM sqlite_schema WHERE name='search_commands_admit'"
        )
        .await?
        .contains("torrentrss")
    );
    // Every other widened trigger names the new implementation.
    for name in [
        "provider_test_insert_owner",
        "provider_test_update_owner",
        "provider_scope_options_insert",
        "provider_scope_options_update",
        "provider_client_options_insert",
        "provider_client_options_update",
        "rss_commands_admit",
        "rss_schedules_admit",
        "rss_schedules_update",
    ] {
        assert!(
            text(
                c,
                &format!("SELECT sql FROM sqlite_schema WHERE name='{name}'")
            )
            .await?
            .contains("torrentrss"),
            "{name}"
        );
    }
    assert_eq!(text(c, "PRAGMA integrity_check").await?, "ok");
    assert_eq!(
        count(c, "SELECT count(*) FROM pragma_foreign_key_check").await?,
        0
    );
    Ok(())
}

#[tokio::test]
async fn fresh_database_accepts_the_feed_implementation_and_keeps_native_guards()
-> Result<(), Error> {
    let scratch = Scratch::new();
    let db = Database::open_local(scratch.0.join("db")).await?;
    let c = db.connect().await?;
    assert_eq!(
        count(&c, "SELECT max(version) FROM schema_migrations").await?,
        50
    ); // Reasoning: 0050 is the torrent RSS indexer migration.
    assert_eq!(
        text(&c, "SELECT name FROM schema_migrations WHERE version=50").await?,
        "torrent_rss_indexer"
    );
    assert_feed_contract(&c).await?;
    // A pre-existing guard kept as is: download_client_id stays native-indexer only.
    let feed = feed(&c, &["tv"]).await?;
    assert!(
        rejects(
            &c,
            "UPDATE provider_scopes SET download_client_id=? WHERE provider_id=?",
            params![uuid::Uuid::new_v4().to_string(), feed]
        )
        .await
    );
    Ok(())
}

#[tokio::test]
async fn upgrade_from_49_preserves_populated_rows_children_and_foreign_keys() -> Result<(), Error> {
    let scratch = Scratch::new();
    let path = scratch.0.join("db");
    let (raw, c) = at_49(&path).await?;
    let torznab = indexer(&c, "torznab").await?;
    let newznab = indexer(&c, "newznab").await?;
    let client = super::refresh_tests::provider(&c).await?;
    c.execute("INSERT INTO provider_tests(provider_id,config_revision,tested_at,status,error_code) VALUES(?,1,100,'success',NULL)", [torznab.clone()]).await?;
    c.execute("INSERT INTO provider_tests(provider_id,config_revision,tested_at,status,error_code) VALUES(?,1,101,'failure','timeout')", [client.clone()]).await?;
    c.execute("INSERT INTO rss_schedules(id,media_type,indexer_id,indexer_revision,client_id,client_revision,interval_seconds,enabled,next_run_at,created_at) VALUES(?,'tv',?,1,?,1,600,1,0,0)",params![uuid::Uuid::new_v4().to_string(),torznab.clone(),client.clone()]).await?;
    c.execute("INSERT INTO rss_schedules(id,media_type,indexer_id,indexer_revision,client_id,client_revision,interval_seconds,enabled,next_run_at,created_at) VALUES(?,'movies',?,1,?,1,900,0,5,1)",params![uuid::Uuid::new_v4().to_string(),newznab.clone(),client.clone()]).await?;
    assert!(rejects(&c, "INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES('11111111-1111-4111-8111-111111111111','torrentrss','x',1,1,1,1,'http://127.0.0.1:9')", ()).await, "49 has no feed implementation");
    let tables = [
        "providers",
        "provider_scopes",
        "provider_tests",
        "rss_schedules",
        "schema_migrations",
    ];
    let mut before = Vec::new();
    for table in tables {
        before.push(rows(&c, table).await?);
    }
    assert_eq!(count(&c, "SELECT count(*) FROM provider_scopes").await?, 6);
    drop(c);
    drop(raw);
    for round in 0..2 {
        let db = Database::open_local(&path).await?;
        if round == 0 {
            assert!(
                db.migration_backup().is_some(),
                "upgrade takes a pre-migration backup"
            );
        }
        let c = db.connect().await?;
        assert_eq!(
            count(&c, "SELECT max(version) FROM schema_migrations").await?,
            50
        ); // Reasoning: 0050 is the torrent RSS indexer migration.
        assert_eq!(rows(&c, "providers").await?, before[0]);
        // Only the appended nullable minimum_seeders column differs; every old value stays exact.
        let mut expected = before[1].clone();
        for row in &mut expected {
            row.push(libsql::Value::Null);
        }
        assert_eq!(rows(&c, "provider_scopes").await?, expected);
        assert_eq!(rows(&c, "provider_tests").await?, before[2]);
        assert_eq!(rows(&c, "rss_schedules").await?, before[3]);
        let history = rows(&c, "schema_migrations").await?;
        assert_eq!(&history[..49], before[4].as_slice());
        assert_eq!(history.len(), 50);
        assert_eq!(text(&c, "PRAGMA integrity_check").await?, "ok");
        assert_eq!(
            count(&c, "SELECT count(*) FROM pragma_foreign_key_check").await?,
            0
        );
        if round == 0 {
            // Old children still enforce the old guards after the in-place CHECK widening.
            assert!(rejects(&c, "INSERT INTO provider_scopes(provider_id,implementation,media_type) VALUES(?,'newznab','tv')", [uuid::Uuid::new_v4().to_string()]).await);
        }
    }
    // The contract run mutates the data, so it follows the two read-only reopen rounds.
    let db = Database::open_local(&path).await?;
    assert_feed_contract(&db.connect().await?).await?;
    Ok(())
}

#[tokio::test]
async fn failed_or_unapplied_widening_rolls_the_whole_migration_back() -> Result<(), Error> {
    let scratch = Scratch::new();
    let path = scratch.0.join("db");
    let (raw, c) = at_49(&path).await?;
    let torznab = indexer(&c, "torznab").await?;
    let before_schema = schema_text(&c).await?;
    let before_providers = rows(&c, "providers").await?;
    // A schema whose CHECK text differs from the shipped history: the guard table must abort the
    // migration instead of leaving a half-widened database.
    c.execute_batch(
        "PRAGMA writable_schema=ON;
         UPDATE sqlite_schema SET sql=replace(sql,'''newznab'',''qbittorrent''))','''newznab'',''qbittorrent'' ))') WHERE name='providers';
         PRAGMA writable_schema=RESET;",
    )
    .await?;
    let tampered_schema = schema_text(&c).await?;
    assert_ne!(tampered_schema, before_schema);
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let error = tx.execute_batch(MIGRATIONS[49].1).await.unwrap_err();
    assert!(
        error.to_string().contains("CHECK constraint failed"),
        "{error}"
    );
    tx.rollback().await?;
    assert_eq!(
        schema_text(&c).await?,
        tampered_schema,
        "DDL, trigger and column changes roll back together"
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM pragma_table_info('provider_scopes') WHERE name='minimum_seeders'"
        )
        .await?,
        0
    );
    assert_eq!(rows(&c, "providers").await?, before_providers);
    assert_eq!(
        count(
            &c,
            &format!("SELECT count(*) FROM provider_scopes WHERE provider_id='{torznab}'")
        )
        .await?,
        2
    );
    // The real runner refuses the tampered database too and leaves it at 49 with a recovery backup.
    drop(c);
    drop(raw);
    assert!(Database::open_local(&path).await.is_err());
    let raw = libsql::Builder::new_local(&path).build().await?;
    let c = raw.connect()?;
    assert_eq!(
        count(&c, "SELECT max(version) FROM schema_migrations").await?,
        49
    );
    // Re-applying 0050 to an already migrated database also fails atomically (duplicate column).
    let fresh = Scratch::new();
    let db = Database::open_local(fresh.0.join("db")).await?;
    let c = db.connect().await?;
    let schema = schema_text(&c).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    assert!(tx.execute_batch(MIGRATIONS[49].1).await.is_err());
    tx.rollback().await?;
    assert_eq!(schema_text(&c).await?, schema);
    Ok(())
}
