//! Migration 0062: Transmission registry row, replaced scope shape/option triggers and the
//! directory-placement table. Upgrade from 61, late-failure rollback, reopen and constraints.
use super::*;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "hrrdarr-transmission-schema-{}",
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
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,?,'Client',1,1,1,1,'http://127.0.0.1:1/transmission')",params![id.clone(),implementation]).await?;
    Ok(id)
}
async fn scope(
    c: &Connection,
    id: &str,
    media: &str,
    category: Option<&str>,
    state: &str,
) -> Result<u64, libsql::Error> {
    c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state) VALUES(?,'transmission',?,?,1,0,?)",params![id,media,category,state]).await
}
async fn directory(
    c: &Connection,
    id: &str,
    media: &str,
    path: &str,
) -> Result<u64, libsql::Error> {
    c.execute("INSERT INTO provider_scope_directories(provider_id,media_type,implementation,directory) VALUES(?,?,'transmission',?)",params![id,media,path]).await
}
const TABLES: [&str; 6] = [
    "SELECT * FROM providers ORDER BY id",
    "SELECT * FROM provider_scopes ORDER BY provider_id,media_type",
    "SELECT * FROM download_client_tags ORDER BY rowid",
    "SELECT * FROM commands ORDER BY id",
    "SELECT * FROM download_refresh_schedules ORDER BY provider_id,media_type",
    "SELECT * FROM provider_implementations ORDER BY implementation",
];

#[tokio::test]
async fn transmission_upgrade61_preserves_rows_rolls_back_and_enforces_placement()
-> Result<(), Error> {
    let scratch = Scratch::new();
    let path = scratch.0.join("db");
    let raw = libsql::Builder::new_local(&path).build().await?;
    let c = raw.connect()?;
    // Like the runner, rebuild migrations execute with foreign keys off.
    c.execute("PRAGMA foreign_keys=OFF", ()).await?;
    c.execute(HISTORY_SQL, ()).await?;
    for (i, (name, sql)) in MIGRATIONS.iter().take(61).enumerate() {
        c.execute_batch(sql).await?;
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql)VALUES(?,?,?,?)",
            params![i as i64 + 1, *name, checksum(sql), *sql],
        )
        .await?;
    }
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    // Schema 61 cannot store Transmission: the registry has no such implementation.
    assert!(provider(&c, "transmission").await.is_err());
    let qbit = super::refresh_tests::provider(&c).await?;
    for media in ["tv", "movies"] {
        super::refresh_tests::enqueue(&c, &qbit, media).await?;
        super::refresh_tests::schedule(&c, &qbit, media).await?;
    }
    let mut before = vec![];
    for sql in TABLES {
        before.push(rows(&c, sql).await?);
    }
    let schema = rows(
        &c,
        "SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY name",
    )
    .await?;

    // A late failure after every trigger replacement leaves schema 61 intact.
    c.execute("PRAGMA foreign_keys=OFF", ()).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    tx.execute_batch(MIGRATIONS[61].1).await?;
    assert_eq!(MIGRATIONS[61].0, "transmission");
    tx.rollback().await?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    assert_eq!(
        rows(
            &c,
            "SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY name"
        )
        .await?,
        schema
    );
    for (sql, expected) in TABLES.iter().zip(&before) {
        assert_eq!(&rows(&c, sql).await?, expected, "{sql}");
    }
    drop(c);
    drop(raw);

    // The real runner applies 0062; existing provider rows are untouched and checks pass.
    let db = Database::open_local(&path).await?;
    let c = db.connect().await?;
    assert_eq!(version(&c).await?, 75); // Reasoning: latest is migration file 0075 provider_scope_union at position 75, appended after 0074 downloadstation; historical prefixes stay fixed.
    for (index, (sql, expected)) in TABLES[..5].iter().zip(&before).enumerate() {
        let mut expected = expected.clone();
        if index == 1 {
            // 0068 appends the nullable minimum_seeders column to provider_scopes.
            for row in &mut expected {
                row.push(libsql::Value::Null);
            }
        }
        assert_eq!(rows(&c, sql).await?, expected, "{sql}");
    }
    // Reasoning: the runner continues through the later nzbget migration, so the registry holds
    // all appended rows, ordered by implementation name.
    let mut registry = before[5].clone();
    for (name, kind, protocol) in [
        ("aria2", "download_client", "torrent"),
        ("deluge", "download_client", "torrent"),
        ("downloadstation", "download_client", "torrent"),
        ("flood", "download_client", "torrent"),
        ("nzbget", "download_client", "usenet"),
        ("rtorrent", "download_client", "torrent"),
        ("torrentrss", "indexer", "torrent"),
        ("transmission", "download_client", "torrent"),
        ("utorrent", "download_client", "torrent"),
    ] {
        registry.push(vec![
            libsql::Value::Text(name.into()),
            libsql::Value::Text(kind.into()),
            libsql::Value::Text(protocol.into()),
        ]);
    }
    registry.sort_by_key(|row| format!("{:?}", row[0]));
    assert_eq!(rows(&c, TABLES[5]).await?, registry);
    assert!(rows(&c, "PRAGMA foreign_key_check").await?.is_empty());
    assert_eq!(
        rows(&c, "PRAGMA integrity_check").await?,
        vec![vec![libsql::Value::Text("ok".into())]]
    );
    assert!(
        c.execute(
            "UPDATE provider_implementations SET protocol='usenet' WHERE implementation='transmission'",
            ()
        )
        .await
        .is_err(),
        "the registry row is write-once"
    );

    // Category placement (TV) and directory placement (movies) on one shared client.
    let shared = provider(&c, "transmission").await?;
    scope(&c, &shared, "tv", Some("tv-sonarr"), "started").await?;
    scope(&c, &shared, "movies", None, "stopped").await?;
    directory(&c, &shared, "movies", "/srv/movies").await?;
    // The domain label is unique per client, case-insensitively.
    let labels = provider(&c, "transmission").await?;
    scope(&c, &labels, "tv", Some("media"), "started").await?;
    assert!(
        scope(&c, &labels, "movies", Some("MEDIA"), "started")
            .await
            .is_err()
    );
    // Scope shape: qBittorrent-only and indexer columns, priorities and add-paused are closed.
    for (column, value) in [
        ("imported_category", "'done'"),
        ("recent_priority", "2"),
        ("older_priority", "-1"),
        ("initial_state", "'forced'"),
        ("content_layout", "'default'"),
        ("sequential_order", "0"),
        ("add_tags", "0"),
        ("categories", "'[1]'"),
        ("category", "'tv/sub'"),
        ("category", "'..'"),
        ("category", "'tv,hd'"),
    ] {
        assert!(c.execute(&format!("UPDATE provider_scopes SET {column}={value} WHERE provider_id=? AND media_type='tv'"), [shared.clone()]).await.is_err(), "{column}={value}");
    }
    // A scope cannot hold both a category and a directory, in either order.
    assert!(
        c.execute(
            "UPDATE provider_scopes SET category='movies' WHERE provider_id=? AND media_type='movies'",
            [shared.clone()]
        )
        .await
        .is_err()
    );
    assert!(directory(&c, &shared, "tv", "/srv/tv").await.is_err());
    // Directory rows are immutable, absolute, and never nest across domains.
    assert!(
        c.execute(
            "UPDATE provider_scope_directories SET directory='/srv/other' WHERE provider_id=?",
            [shared.clone()]
        )
        .await
        .is_err()
    );
    let nested = provider(&c, "transmission").await?;
    scope(&c, &nested, "tv", None, "started").await?;
    scope(&c, &nested, "movies", None, "started").await?;
    assert!(directory(&c, &nested, "tv", "relative/tv").await.is_err());
    directory(&c, &nested, "tv", "/srv/media").await?;
    for conflicting in ["/srv/media", "/srv/media/movies", "/SRV/MEDIA", "/srv"] {
        assert!(
            directory(&c, &nested, "movies", conflicting).await.is_err(),
            "{conflicting}"
        );
    }
    directory(&c, &nested, "movies", "/srv/media-movies").await?;
    // Deleting the scope (or provider) removes its directory placement.
    c.execute(
        "DELETE FROM provider_scopes WHERE provider_id=? AND media_type='movies'",
        [nested.clone()],
    )
    .await?;
    assert_eq!(
        rows(
            &c,
            &format!(
                "SELECT count(*) FROM provider_scope_directories WHERE provider_id='{nested}'"
            )
        )
        .await?,
        vec![vec![libsql::Value::Integer(1)]]
    );
    c.execute("DELETE FROM providers WHERE id=?", [nested.clone()])
        .await?;
    assert_eq!(
        rows(
            &c,
            &format!(
                "SELECT count(*) FROM provider_scope_directories WHERE provider_id='{nested}'"
            )
        )
        .await?,
        vec![vec![libsql::Value::Integer(0)]]
    );

    // Protocol-aware admission: a torrent indexer may target Transmission, a usenet one may not.
    let torznab = uuid::Uuid::new_v4().to_string();
    let newznab = uuid::Uuid::new_v4().to_string();
    for (id, implementation) in [(&torznab, "torznab"), (&newznab, "newznab")] {
        c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,?,'Indexer',1,1,1,1,'http://127.0.0.1:1/')",params![id.clone(),implementation]).await?;
        for media in ["tv", "movies"] {
            c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year) VALUES(?,?,?,'[5000]','[]',?,?)",params![id.clone(),implementation,media,(media=="tv").then_some(0),(media=="movies").then_some(0)]).await?;
        }
    }
    super::rss_tests::command(&c, &torznab, &shared, "tv").await?;
    super::rss_tests::command(&c, &torznab, &shared, "movies").await?;
    assert!(
        super::rss_tests::command(&c, &newznab, &shared, "tv")
            .await
            .is_err(),
        "usenet indexer with a Transmission client"
    );
    // Generalized client triggers admit Transmission refresh commands and schedules per domain.
    for media in ["tv", "movies"] {
        super::refresh_tests::enqueue(&c, &shared, media).await?;
        super::refresh_tests::schedule(&c, &shared, media).await?;
    }
    drop(c);
    drop(db);
    let db = Database::open_local(&path).await?;
    assert_eq!(version(&db.connect().await?).await?, 75); // Reasoning: latest is migration file 0075 provider_scope_union at position 75, appended after 0074 downloadstation; historical prefixes stay fixed.
    Ok(())
}
