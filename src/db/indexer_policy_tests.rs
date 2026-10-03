use super::*;

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
async fn scalar(c: &Connection, sql: &str) -> Result<i64, Error> {
    Ok(c.query(sql, ()).await?.next().await?.unwrap().get(0)?)
}
async fn rows(c: &Connection, sql: &str) -> Result<Vec<Vec<libsql::Value>>, Error> {
    let mut query = c.query(sql, ()).await?;
    let mut result = vec![];
    while let Some(row) = query.next().await? {
        result.push(
            (0..row.column_count())
                .map(|i| row.get_value(i))
                .collect::<Result<_, _>>()?,
        );
    }
    Ok(result)
}
async fn provider(c: &Connection, implementation: &str) -> Result<String, Error> {
    let id = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint)VALUES(?,?,'fixture',1,1,1,1,'http://127.0.0.1:9')",params![id.clone(),implementation]).await?;
    for domain in ["tv", "movies"] {
        if implementation == "qbittorrent" {
            // Existing client writers omit all indexer policy columns.
            c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags)VALUES(?,'qbittorrent',?,?,0,0,'started','default',0,0,0)",params![id.clone(),domain,domain]).await?;
        } else {
            c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year)VALUES(?,?,?,'[5000]','[]',?,?)",params![id.clone(),implementation,domain,(domain=="tv").then_some(0i64),(domain=="movies").then_some(0i64)]).await?;
        }
    }
    Ok(id)
}

#[tokio::test]
async fn indexer_policy_upgrade43_rollback_defaults_false_reopen_and_registry() -> Result<(), Error>
{
    let scratch = Scratch(
        std::env::temp_dir().join(format!("hrrdarr-indexer-policy-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&scratch.0)?;
    let path = scratch.0.join("db");
    let raw = libsql::Builder::new_local(&path).build().await?;
    let c = raw.connect()?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    c.execute(HISTORY_SQL, ()).await?;
    for (i, (name, sql)) in MIGRATIONS.iter().take(47).enumerate() {
        c.execute_batch(sql).await?;
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql)VALUES(?,?,?,?)",
            params![i as i64 + 1, *name, checksum(sql), *sql],
        )
        .await?;
    }
    let torznab = provider(&c, "torznab").await?;
    let newznab = provider(&c, "newznab").await?;
    let client = provider(&c, "qbittorrent").await?;
    c.execute(
        "INSERT INTO provider_tests VALUES(?,1,123,'success',NULL)",
        [torznab.clone()],
    )
    .await?;
    c.execute_batch("INSERT INTO series(id,title,path)VALUES(1,'TV','/tv');INSERT INTO seasons(series_id,number)VALUES(1,1);INSERT INTO episode_files VALUES(1,1,'/tv/pilot.mkv');INSERT INTO episodes(id,series_id,season,number,title,episode_file_id)VALUES(1,1,1,1,'Pilot',1);INSERT INTO movie_metadata(id,tmdb_id,title,year)VALUES(1,1,'Movie',2020);INSERT INTO movies(id,metadata_id,path)VALUES(1,1,'/movies');INSERT INTO movie_files(id,movie_id,path,edition)VALUES(1,1,'/movies/movie.mkv','');").await?;
    let tables = [
        "providers",
        "provider_tests",
        "health_checks",
        "health_lifecycle",
        "schema_migrations",
        "series",
        "episodes",
        "episode_files",
        "movies",
        "movie_metadata",
        "movie_files",
    ];
    let mut before = vec![];
    for table in tables {
        before.push(rows(&c, &format!("SELECT * FROM {table} ORDER BY rowid")).await?);
    }
    let scopes = rows(
        &c,
        "SELECT * FROM provider_scopes ORDER BY provider_id,media_type",
    )
    .await?;
    let schema = rows(&c, "SELECT * FROM sqlite_schema ORDER BY name").await?;
    let tx = c.transaction().await?;
    tx.execute_batch(MIGRATIONS[47].1).await?;
    assert!(
        tx.execute("UPDATE provider_scopes SET enable_rss=2", ())
            .await
            .is_err()
    );
    tx.rollback().await?;
    assert_eq!(version(&c).await?, 47); // Reasoning: predecessor is now schema47 (movie credits); policy migration is 0048.
    assert_eq!(
        rows(&c, "SELECT * FROM sqlite_schema ORDER BY name").await?,
        schema
    );
    assert_eq!(
        rows(
            &c,
            "SELECT * FROM provider_scopes ORDER BY provider_id,media_type"
        )
        .await?,
        scopes
    );
    for (table, expected) in tables.iter().zip(&before) {
        assert_eq!(
            &rows(&c, &format!("SELECT * FROM {table} ORDER BY rowid")).await?,
            expected
        );
    }
    drop(c);
    drop(raw);
    let db = Database::open_local(&path).await?;
    let c = db.connect().await?;
    assert_eq!(version(&c).await?, 48);
    // ADD COLUMN supplies logical defaults without firing scope mutation/invalidation triggers.
    for (table, expected) in tables.iter().zip(&before) {
        if !matches!(*table, "health_checks" | "schema_migrations") {
            assert_eq!(
                &rows(&c, &format!("SELECT * FROM {table} ORDER BY rowid")).await?,
                expected
            );
        }
    }
    for (actual, expected) in rows(
        &c,
        "SELECT * FROM provider_scopes ORDER BY provider_id,media_type",
    )
    .await?
    .iter()
    .zip(&scopes)
    {
        assert_eq!(&actual[..expected.len()], expected);
        assert_eq!(
            &actual[expected.len()..],
            &vec![libsql::Value::Integer(1); 3]
        );
    }
    assert_eq!(scalar(&c, "SELECT count(*) FROM health_checks").await?, 12); // Reasoning: schema47 predecessor already holds 8 health_checks rows (download-root and removed-metadata checks added since schema43); policy adds the 4 indexer checks.
    // Reasoning: the preserved predecessor prefix is all 8 schema47 health rows (was 4 at schema43).
    assert_eq!(
        rows(&c, "SELECT * FROM health_checks ORDER BY rowid LIMIT 8").await?,
        before[2]
    );
    assert_eq!(scalar(&c,"SELECT count(*) FROM health_checks WHERE scope IN ('tv','movies') AND ((check_key='indexer_search' AND compatibility_type='IndexerSearchCheck') OR (check_key='indexer_rss' AND compatibility_type='IndexerRssCheck')) AND startup=1 AND scheduled=1 AND generation=0 AND pending_reasons=0 AND due_at IS NULL AND observed_generation IS NULL AND observed_epoch IS NULL AND checked_at IS NULL AND last_error IS NULL AND severity IS NULL AND reason IS NULL AND message IS NULL AND wiki_url IS NULL").await?,4);
    provider(&c, "torznab").await?;
    provider(&c, "qbittorrent").await?;
    assert_eq!(scalar(&c,"SELECT count(*) FROM provider_scopes WHERE enable_rss=1 AND enable_automatic_search=1 AND enable_interactive_search=1").await?,10);
    for flag in [
        "enable_rss",
        "enable_automatic_search",
        "enable_interactive_search",
    ] {
        for invalid in ["NULL", "-1", "2", "0.5", "'true'", "X'00'"] {
            assert!(
                c.execute(&format!("UPDATE provider_scopes SET {flag}={invalid}"), ())
                    .await
                    .is_err(),
                "accepted {flag}={invalid}"
            );
            // Ordinary INSERT must reject NULL; REPLACE would substitute the NOT NULL default.
            // Remove this scope transactionally so uniqueness cannot hide a missing flag guard.
            let tx = c.transaction().await?;
            tx.execute(
                "DELETE FROM provider_scopes WHERE provider_id=? AND media_type='tv'",
                [torznab.clone()],
            )
            .await?;
            assert!(tx.execute(&format!("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,{flag})VALUES(?,'torznab','tv','[5000]','[]',0,{invalid})"),[torznab.clone()]).await.is_err(), "INSERT accepted {flag}={invalid}");
            tx.execute(&format!("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,{flag})VALUES(?,'torznab','tv','[5000]','[]',0,0)"),[torznab.clone()]).await?;
            tx.rollback().await?;
        }
    }
    for (id, domain, flags) in [
        (&torznab, "tv", [0, 1, 0]),
        (&torznab, "movies", [1, 0, 1]),
        (&newznab, "tv", [1, 0, 0]),
        (&newznab, "movies", [0, 1, 1]),
    ] {
        c.execute("UPDATE provider_scopes SET enable_rss=?,enable_automatic_search=?,enable_interactive_search=? WHERE provider_id=? AND media_type=?",params![flags[0],flags[1],flags[2],id.clone(),domain]).await?;
    }
    let policy_sql = "SELECT provider_id,media_type,enable_rss,enable_automatic_search,enable_interactive_search FROM provider_scopes ORDER BY provider_id,media_type";
    let policies = rows(&c, policy_sql).await?;
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM provider_tests").await?,
        0,
        "existing scope invalidation trigger remains active"
    );
    assert!(
        c.execute("UPDATE providers SET enabled=0", ())
            .await
            .is_err(),
        "revision guard remains active"
    );
    for enabled in [0, 1] {
        c.execute(
            "UPDATE providers SET enabled=?,revision=revision+1",
            [enabled],
        )
        .await?;
        assert_eq!(rows(&c, policy_sql).await?, policies);
    }
    assert!(
        c.execute(
            "UPDATE provider_scopes SET implementation='qbittorrent' WHERE provider_id=?",
            [torznab.clone()]
        )
        .await
        .is_err()
    );
    assert!(
        c.execute(
            "UPDATE provider_scopes SET category='tv' WHERE provider_id=? AND media_type='movies'",
            [client.clone()]
        )
        .await
        .is_err()
    );
    for sql in [
        "UPDATE health_checks SET generation=generation+2",
        "UPDATE health_checks SET scope='system' WHERE check_key='indexer_rss'",
        "UPDATE health_checks SET severity=1 WHERE check_key='indexer_rss'",
        "INSERT INTO health_checks(scope,check_key,startup,scheduled,compatibility_type)VALUES('tv','indexer_rss',1,1,'IndexerRssCheck')",
    ] {
        assert!(c.execute(sql, ()).await.is_err(), "accepted {sql}");
    }
    let tx = c.transaction().await?;
    for i in 0..116
    /* Reasoning: 12 registry rows exist at schema48 (was 8), so 116 fixtures fill the 128 cap */
    {
        tx.execute("INSERT INTO health_checks(scope,check_key,startup,scheduled,compatibility_type)VALUES('system',?,0,0,'fixture')",[format!("fixture_{i}")]).await?;
    }
    assert!(tx.execute("INSERT INTO health_checks(scope,check_key,startup,scheduled,compatibility_type)VALUES('system','overflow',0,0,'fixture')",()).await.is_err());
    tx.rollback().await?;
    integrity(&c).await?;
    drop(c);
    drop(db);
    let db = Database::open_local(&path).await?;
    let c = db.connect().await?;
    assert_eq!(rows(&c, policy_sql).await?, policies);
    assert_eq!(scalar(&c, "SELECT count(*) FROM health_checks").await?, 12); // Reasoning: 12 health rows at schema48 (was 8).
    c.execute("DELETE FROM providers WHERE id=?", [torznab.clone()])
        .await?;
    assert_eq!(
        c.query(
            "SELECT count(*) FROM provider_scopes WHERE provider_id=?",
            [torznab]
        )
        .await?
        .next()
        .await?
        .unwrap()
        .get::<i64>(0)?,
        0
    );
    integrity(&c).await?;
    // The normal fresh runner traverses all migrations, not just the predecessor upgrade.
    let fresh = Database::open_local(scratch.0.join("fresh")).await?;
    assert_eq!(version(&fresh.connect().await?).await?, 48);
    assert_eq!(
        scalar(
            &fresh.connect().await?,
            "SELECT count(*) FROM health_checks"
        )
        .await?,
        12 // Reasoning: fresh schema48 registry has 12 rows (was 8).
    );
    Ok(())
}
