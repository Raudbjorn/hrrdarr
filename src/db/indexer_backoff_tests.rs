use super::*;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "hrrdarr-indexer-backoff-schema-{}",
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
    let mut out = Vec::new();
    while let Some(row) = cursor.next().await? {
        out.push(
            (0..row.column_count())
                .map(|i| row.get_value(i))
                .collect::<std::result::Result<Vec<_>, _>>()?,
        );
    }
    Ok(out)
}
async fn indexer(c: &Connection, implementation: &str) -> Result<String, Error> {
    let id = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint)VALUES(?,?,'owned fixture',1,1,1,1,'http://127.0.0.1:9')",params![id.clone(),implementation]).await?;
    for media in ["tv", "movies"] {
        c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year)VALUES(?,?,?,'[5000]','[]',?,?)",params![id.clone(),implementation,media,(media=="tv").then_some(0),(media=="movies").then_some(0)]).await?;
    }
    Ok(id)
}
async fn failure(c: &Connection, id: &str, media: &str, revision: i64) -> Result<(), Error> {
    c.execute("INSERT INTO indexer_status(provider_id,media_type,config_revision,failure_count,initial_failure_at,last_failure_at,disabled_until,next_health_at)VALUES(?,?,?,1,100,100,400,400)",params![id,media,revision]).await?;
    Ok(())
}

#[tokio::test]
async fn indexer_backoff_upgrade49_rollback_and_reopen_preserve_existing_state() -> Result<(), Error>
{
    let scratch = Scratch::new();
    let path = scratch.0.join("db");
    let raw = libsql::Builder::new_local(&path).build().await?;
    let c = raw.connect()?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    c.execute(HISTORY_SQL, ()).await?;
    for (i, (name, sql)) in MIGRATIONS.iter().take(49).enumerate() {
        c.execute_batch(sql).await?;
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql)VALUES(?,?,?,?)",
            params![i as i64 + 1, *name, checksum(sql), *sql],
        )
        .await?;
    }
    let id = indexer(&c, "torznab").await?;
    indexer(&c, "newznab").await?;
    super::refresh_tests::provider(&c).await?;
    c.execute_batch("INSERT INTO series(id,title,path)VALUES(1,'TV','/tv'); INSERT INTO seasons VALUES(1,1,1); INSERT INTO episodes(id,series_id,season,number,title)VALUES(1,1,1,1,'Pilot'); INSERT INTO movie_metadata(id,title)VALUES(1,'Movie'); INSERT INTO movies(id,metadata_id,path)VALUES(1,1,'/movies');").await?;
    let tables = [
        "providers",
        "provider_scopes",
        "health_checks",
        "health_lifecycle",
        "schema_migrations",
        "series",
        "episodes",
        "movie_metadata",
        "movies",
    ];
    let mut before = Vec::new();
    for table in tables {
        before.push(rows(&c, table).await?);
    }
    let schema = rows(&c, "sqlite_schema").await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    tx.execute_batch(MIGRATIONS[49].1).await?;
    failure(&tx, &id, "tv", 1).await?;
    assert!(failure(&tx, &id, "movies", 2).await.is_err());
    tx.rollback().await?;
    assert_eq!(rows(&c, "sqlite_schema").await?, schema);
    for (table, expected) in tables.iter().zip(&before) {
        assert_eq!(&rows(&c, table).await?, expected);
    }
    drop(c);
    drop(raw);
    for _ in 0..2 {
        let db = Database::open_local(&path).await?;
        let c = db.connect().await?;
        assert_eq!(version(&c).await?, 72); // Reasoning: latest is migration file 0072 utorrent at position 72, appended after 0071 rtorrent; historical prefixes stay fixed.
        for (table, expected) in tables.iter().zip(&before) {
            let actual = rows(&c, table).await?;
            if matches!(*table, "health_checks" | "schema_migrations") {
                assert_eq!(&actual[..expected.len()], expected);
                assert_eq!(
                    actual.len(),
                    expected.len() + if *table == "health_checks" { 10 } else { 23 } // Reasoning: the 0069 collection snapshot and 0070 deluge migrations add two schema_migrations rows beyond the nineteen previously asserted after the fifteen migrations from 49 through 64 (0060 recycle bin, 0061 upgrade recycle, 0062 transmission registry, 0063 notifications); 0060 adds the two recycle_bin identities and 0054 through 0059 and 0061 through 0063 add none, so Health still gains ten rows.
                );
            } else if *table == "provider_scopes" {
                // 0068 appends the nullable minimum_seeders column; every predecessor value stays exact.
                let mut expected = expected.clone();
                for row in &mut expected {
                    row.push(libsql::Value::Null);
                }
                assert_eq!(actual, expected);
            } else if *table == "series" {
                // 0055 appends unknown runtime; every predecessor value remains exact.
                let mut expected = expected.clone();
                for row in &mut expected {
                    row.push(libsql::Value::Null);
                }
                assert_eq!(actual, expected);
            } else {
                assert_eq!(&actual, expected);
            }
        }
        assert!(rows(&c, "indexer_status").await?.is_empty());
        integrity(&c).await?;
    }
    Ok(())
}

#[tokio::test]
async fn indexer_backoff_constraints_revision_fence_scopes_rollback_and_restart()
-> Result<(), Error> {
    let scratch = Scratch::new();
    let path = scratch.0.join("db");
    let db = Database::open_local(&path).await?;
    let c = db.connect().await?;
    let mut seeds = c.query("SELECT scope,check_key,compatibility_type FROM health_checks WHERE check_key IN ('indexer_status','indexer_failures') AND startup=1 AND scheduled=1 AND generation=0 AND pending_reasons=0 AND due_at IS NULL AND observed_generation IS NULL ORDER BY scope,check_key", ()).await?;
    for media in ["movies", "tv"] {
        for (key, compatibility) in [
            ("indexer_failures", "IndexerLongTermStatusCheck"),
            ("indexer_status", "IndexerStatusCheck"),
        ] {
            let row = seeds.next().await?.unwrap();
            assert_eq!(row.get::<String>(0)?, media);
            assert_eq!(row.get::<String>(1)?, key);
            assert_eq!(row.get::<String>(2)?, compatibility);
        }
    }
    assert!(seeds.next().await?.is_none());
    drop(seeds);
    let id = indexer(&c, "torznab").await?;
    let other = indexer(&c, "newznab").await?;
    let client = super::refresh_tests::provider(&c).await?;
    for media in ["tv", "movies"] {
        assert!(failure(&c, &client, media, 1).await.is_err());
        assert!(failure(&c, &id, media, 2).await.is_err());
        failure(&c, &id, media, 1).await?;
        failure(&c, &other, media, 1).await?;
    }
    assert!(
        failure(&c, &uuid::Uuid::new_v4().to_string(), "tv", 1)
            .await
            .is_err()
    );
    for assignment in [
        "failure_count=0",
        "failure_count=-1",
        "failure_count=1.5",
        "failure_count=9007199254740992",
        "failure_count=NULL",
        "failure_count=X'01'",
        "initial_failure_at=-1",
        "initial_failure_at=101",
        "last_failure_at=99",
        "last_failure_at=401",
        "disabled_until=99",
        "disabled_until=9007199254740992",
        "next_health_at=99",
        "next_health_at=401",
        "config_revision=2",
        "media_type='system'",
        "media_type='movies'",
        "provider_id='missing'",
    ] {
        assert!(
            c.execute(
                &format!(
                    "UPDATE indexer_status SET {assignment} WHERE provider_id=? AND media_type='tv'"
                ),
                [id.clone()]
            )
            .await
            .is_err(),
            "accepted {assignment}"
        );
    }
    let before = rows(&c, "indexer_status").await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    tx.execute(
        "UPDATE providers SET revision=revision+1 WHERE id=?",
        [id.clone()],
    )
    .await?;
    assert_eq!(rows(&tx, "indexer_status").await?.len(), 2);
    assert!(failure(&tx, &id, "tv", 1).await.is_err());
    failure(&tx, &id, "tv", 2).await?;
    tx.rollback().await?;
    assert_eq!(rows(&c, "indexer_status").await?, before);
    // Consuming an expiry changes only the timer; history and the other domain survive restart.
    c.execute(
        "UPDATE indexer_status SET next_health_at=NULL WHERE provider_id=? AND media_type='tv'",
        [id.clone()],
    )
    .await?;
    let retained = rows(&c, "indexer_status").await?;
    drop(c);
    drop(db);
    let db = Database::open_local(&path).await?;
    let c = db.connect().await?;
    assert_eq!(rows(&c, "indexer_status").await?, retained);
    c.execute(
        "UPDATE providers SET revision=revision+1 WHERE id=?",
        [id.clone()],
    )
    .await?;
    assert_eq!(rows(&c, "indexer_status").await?.len(), 2);
    assert!(failure(&c, &id, "tv", 1).await.is_err());
    c.execute(
        "DELETE FROM provider_scopes WHERE provider_id=? AND media_type='movies'",
        [id.clone()],
    )
    .await?;
    assert!(failure(&c, &id, "movies", 2).await.is_err());
    failure(&c, &id, "tv", 2).await?;
    c.execute("DELETE FROM providers WHERE id=?", [other])
        .await?;
    assert_eq!(rows(&c, "indexer_status").await?.len(), 1);
    integrity(&c).await?;
    Ok(())
}
