//! Migration 0059: provider implementation registry, provider table rebuilds and
//! protocol-aware admission. Upgrade, late-failure rollback, reopen and new constraints.
use super::*;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("hrrdarr-registry-schema-{}", uuid::Uuid::new_v4()));
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
async fn indexer(
    c: &Connection,
    implementation: &str,
    client: Option<&str>,
) -> Result<String, Error> {
    let id = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,?,'Indexer',1,1,1,1,'http://127.0.0.1:1/')",params![id.clone(),implementation]).await?;
    for media in ["tv", "movies"] {
        c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year,download_client_id) VALUES(?,?,?,'[5000]','[]',?,?,?)",params![id.clone(),implementation,media,(media=="tv").then_some(0),(media=="movies").then_some(0),client]).await?;
    }
    Ok(id)
}
async fn sabnzbd(c: &Connection) -> Result<String, Error> {
    let id = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'sabnzbd','Usenet',1,1,1,1,'http://127.0.0.1:1/sabnzbd')",[id.clone()]).await?;
    for (media, priority) in [("tv", -100), ("movies", 2)] {
        c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority) VALUES(?,'sabnzbd',?,?,?,-1)",params![id.clone(),media,media,priority]).await?;
    }
    Ok(id)
}
const TABLES: [&str; 8] = [
    "SELECT * FROM providers ORDER BY id",
    "SELECT * FROM provider_scopes ORDER BY provider_id,media_type",
    "SELECT * FROM download_client_tags ORDER BY rowid",
    "SELECT * FROM rss_commands ORDER BY id",
    "SELECT * FROM commands ORDER BY id",
    "SELECT * FROM download_refresh_schedules ORDER BY provider_id,media_type",
    "SELECT * FROM download_processing_policies ORDER BY provider_id,media_type",
    "SELECT * FROM provider_tests ORDER BY rowid",
];

#[tokio::test]
async fn registry_upgrade58_preserves_provider_rows_rolls_back_and_enforces_protocols()
-> Result<(), Error> {
    let scratch = Scratch::new();
    let path = scratch.0.join("db");
    let raw = libsql::Builder::new_local(&path).build().await?;
    let c = raw.connect()?;
    // Like the runner, rebuild migrations (0052, 0059) execute with foreign keys off.
    c.execute("PRAGMA foreign_keys=OFF", ()).await?;
    c.execute(HISTORY_SQL, ()).await?;
    for (i, (name, sql)) in MIGRATIONS.iter().take(58).enumerate() {
        c.execute_batch(sql).await?;
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql)VALUES(?,?,?,?)",
            params![i as i64 + 1, *name, checksum(sql), *sql],
        )
        .await?;
    }
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    // Schema 58 cannot store SABnzbd: the old implementation CHECK rejects it.
    assert!(sabnzbd(&c).await.is_err());
    c.execute_batch("INSERT INTO series(id,tvdb_id,title,path) VALUES(1,101,'TV','/tv'); INSERT INTO seasons VALUES(1,1,1); INSERT INTO episodes(id,series_id,season,number,title) VALUES(1,1,1,1,'One'); INSERT INTO movie_metadata(id,tmdb_id,title,year) VALUES(1,101,'Movie',2020); INSERT INTO movies(id,metadata_id,path) VALUES(1,1,'/movies');").await?;
    let client = super::refresh_tests::provider(&c).await?;
    let torznab = indexer(&c, "torznab", Some(&client)).await?;
    let newznab = indexer(&c, "newznab", None).await?;
    for media in ["tv", "movies"] {
        super::rss_tests::command(&c, &torznab, &client, media).await?;
        super::refresh_tests::enqueue(&c, &client, media).await?;
        super::refresh_tests::schedule(&c, &client, media).await?;
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

    // Late failure after every rebuild and trigger replacement leaves schema 58 intact.
    c.execute("PRAGMA foreign_keys=OFF", ()).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    tx.execute_batch(MIGRATIONS[58].1).await?;
    // The registry immutability trigger exists inside the migration transaction.
    assert!(tx.execute("UPDATE provider_implementations SET protocol='usenet' WHERE implementation='qbittorrent'",()).await.is_err());
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

    // The real runner applies 0059 and later; every predecessor row is preserved byte for byte.
    let db = Database::open_local(&path).await?;
    let c = db.connect().await?;
    assert_eq!(
        rows(&c, "SELECT max(version) FROM schema_migrations").await?,
        vec![vec![libsql::Value::Integer(69)]] // Reasoning: the runner applies through latest 0072 (0072 flood registry, 0071 aria2 registry, 0070 utorrent registry, 0069 rtorrent registry, 0068 deluge registry, 0064 NZBGet registry, 0063 notifications; recycle bin 0060, upgrade recycle 0061 and the 0062 transmission registry row follow the 0059 registry).
    );
    for (sql, expected) in TABLES.iter().zip(&before) {
        assert_eq!(&rows(&c, sql).await?, expected, "{sql}");
    }
    assert!(rows(&c, "PRAGMA foreign_key_check").await?.is_empty());
    assert_eq!(
        rows(&c, "PRAGMA integrity_check").await?,
        vec![vec![libsql::Value::Text("ok".into())]]
    );
    assert_eq!(
        rows(&c, "SELECT implementation,kind,protocol FROM provider_implementations ORDER BY implementation").await?,
        // Reasoning: the runner continues through 0062, which appends the transmission row; the
        // four 0059 rows are unchanged.
        [("aria2", "download_client", "torrent"), ("deluge", "download_client", "torrent"), ("flood", "download_client", "torrent"), ("newznab", "indexer", "usenet"), ("nzbget", "download_client", "usenet"), ("qbittorrent", "download_client", "torrent"), ("rtorrent", "download_client", "torrent"), ("sabnzbd", "download_client", "usenet"), ("torznab", "indexer", "torrent"), ("transmission", "download_client", "torrent"), ("utorrent", "download_client", "torrent")]
            .map(|(a, b, p)| vec![libsql::Value::Text(a.into()), libsql::Value::Text(b.into()), libsql::Value::Text(p.into())])
            .to_vec()
    );

    // Registry rows are write-once and referenced implementations cannot disappear.
    assert!(
        c.execute(
            "UPDATE provider_implementations SET protocol='torrent' WHERE implementation='sabnzbd'",
            ()
        )
        .await
        .is_err()
    );
    assert!(
        c.execute(
            "DELETE FROM provider_implementations WHERE implementation='qbittorrent'",
            ()
        )
        .await
        .is_err()
    );
    // Reasoning: 0062 registers transmission, so the unregistered-implementation probe now uses an
    // implementation no migration registers (with an otherwise valid row).
    assert!(c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES('aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaab','tribler','X',1,1,1,1,'http://x/')", ()).await.is_err());

    // SABnzbd scopes: category required, no qBittorrent-only columns, usenet priority range.
    let sab = sabnzbd(&c).await?;
    for (column, value) in [
        ("imported_category", "'done'"),
        ("initial_state", "'started'"),
        ("recent_priority", "3"),
        ("older_priority", "-3"),
        ("category", "NULL"),
        ("categories", "'[1]'"),
    ] {
        assert!(c.execute(&format!("UPDATE provider_scopes SET {column}={value} WHERE provider_id=? AND media_type='tv'"), [sab.clone()]).await.is_err(), "{column}={value}");
    }
    // qBittorrent priorities stay exactly 0/1.
    assert!(
        c.execute(
            "UPDATE provider_scopes SET recent_priority=-1 WHERE provider_id=? AND media_type='tv'",
            [client.clone()]
        )
        .await
        .is_err()
    );
    // A shared client cannot reuse a category across domains, case-insensitively.
    assert!(
        c.execute(
            "UPDATE provider_scopes SET category='TV' WHERE provider_id=? AND media_type='movies'",
            [sab.clone()]
        )
        .await
        .is_err()
    );

    // Explicit admissions require matching protocols; this is independent of the API checks.
    assert!(
        super::rss_tests::command(&c, &newznab, &client, "tv")
            .await
            .is_err(),
        "usenet indexer with torrent client"
    );
    assert!(
        super::rss_tests::command(&c, &torznab, &sab, "tv")
            .await
            .is_err(),
        "torrent indexer with usenet client"
    );
    super::rss_tests::command(&c, &newznab, &sab, "movies").await?;
    // Generalized client triggers admit SABnzbd refresh commands and schedules.
    super::refresh_tests::enqueue(&c, &sab, "tv").await?;
    super::refresh_tests::schedule(&c, &sab, "tv").await?;
    Ok(())
}
