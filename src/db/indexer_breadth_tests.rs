//! Migration 0090 (indexer breadth): nine tracker implementations, the tracker options side table,
//! widened durable backoff, and the guards that stay deliberately narrow.
use super::*;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "hrrdarr-indexer-breadth-schema-{}",
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
const TRACKERS: [&str; 9] = [
    "nyaa",
    "fanzub",
    "iptorrents",
    "torrentleech",
    "broadcasthenet",
    "filelist",
    "hdbits",
    "passthepopcorn",
    "torrentpotato",
];
const SEARCHABLE: [&str; 7] = [
    "nyaa",
    "fanzub",
    "broadcasthenet",
    "filelist",
    "hdbits",
    "passthepopcorn",
    "torrentpotato",
];
// Main's 78 published entries remain unchanged; tracker breadth is appended.
const BREADTH_MIGRATION: usize = 78;

async fn count(c: &Connection, sql: &str) -> Result<i64, Error> {
    Ok(c.query(sql, ()).await?.next().await?.unwrap().get(0)?)
}
async fn rows(c: &Connection, table: &str) -> Result<Vec<Vec<libsql::Value>>, Error> {
    let mut cursor = c
        .query(&format!("SELECT * FROM {table} ORDER BY rowid"), ())
        .await?;
    let mut values = Vec::new();
    while let Some(row) = cursor.next().await? {
        values.push(
            (0..row.column_count())
                .map(|i| row.get_value(i))
                .collect::<Result<Vec<_>, _>>()?,
        );
    }
    Ok(values)
}
async fn rejects(c: &Connection, sql: &str, args: impl libsql::params::IntoParams) -> bool {
    c.execute(sql, args).await.is_err()
}
async fn tracker(
    c: &Connection,
    implementation: &str,
    scopes: &[(&str, i64)],
) -> Result<String, Error> {
    let id = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,?,'tracker',1,1,1,1,'http://127.0.0.1:9/x')", params![id.clone(), implementation]).await?;
    for (media, search) in scopes {
        c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,enable_rss,enable_automatic_search,enable_interactive_search) VALUES(?,?,?,1,?,?)", params![id.clone(), implementation, *media, *search, *search]).await?;
        c.execute("INSERT INTO tracker_scope_options(provider_id,media_type,implementation,categories,minimum_seeders) VALUES(?,?,?,'[]',NULL)", params![id.clone(), *media, implementation]).await?;
    }
    Ok(id)
}
async fn at_prior(path: &Path) -> Result<(libsql::Database, Connection), Error> {
    let raw = libsql::Builder::new_local(path).build().await?;
    let c = raw.connect()?;
    c.execute("PRAGMA foreign_keys=OFF", ()).await?;
    c.execute(HISTORY_SQL, ()).await?;
    for (i, (name, sql)) in MIGRATIONS.iter().take(BREADTH_MIGRATION).enumerate() {
        c.execute_batch(sql).await?;
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
            params![i as i64 + 1, *name, checksum(sql), *sql],
        )
        .await?;
    }
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    Ok((raw, c))
}

#[tokio::test]
async fn registry_rows_and_storage_guards_on_a_fresh_database() -> Result<(), Error> {
    let scratch = Scratch::new();
    let db = Database::open_local(scratch.0.join("db")).await?;
    let c = db.connect().await?;
    assert_eq!(MIGRATIONS[BREADTH_MIGRATION].0, "indexer_breadth");
    for kind in TRACKERS {
        let row = c
            .query(
                "SELECT kind,protocol FROM provider_implementations WHERE implementation=?",
                [kind],
            )
            .await?
            .next()
            .await?
            .unwrap();
        assert_eq!(row.get::<String>(0)?, "indexer", "{kind}");
        assert_eq!(
            row.get::<String>(1)?,
            if kind == "fanzub" {
                "usenet"
            } else {
                "torrent"
            },
            "{kind}"
        );
    }
    // Searchable trackers take free search flags; the RSS-only ones are forced to zero.
    for kind in SEARCHABLE {
        let id = tracker(&c, kind, &[("tv", 1), ("movies", 0)]).await?;
        assert!(rejects(&c, "UPDATE provider_scopes SET categories='[5000]' WHERE provider_id=? AND media_type='tv'", [id.clone()]).await, "{kind}: no native categories");
        assert!(
            rejects(
                &c,
                "UPDATE provider_scopes SET category='tv' WHERE provider_id=? AND media_type='tv'",
                [id.clone()]
            )
            .await,
            "{kind}: no client columns"
        );
        assert!(rejects(&c, "UPDATE provider_scopes SET anime_standard_format_search=0 WHERE provider_id=? AND media_type='tv'", [id.clone()]).await, "{kind}: no native options");
        assert!(rejects(&c, "UPDATE provider_scopes SET initial_state='started' WHERE provider_id=? AND media_type='tv'", [id.clone()]).await, "{kind}: no client options");
        assert!(rejects(&c, "UPDATE provider_scopes SET download_client_id='abcdefab-1234-5678-9abc-def012345678' WHERE provider_id=? AND media_type='tv'", [id.clone()]).await, "{kind}: bindings stay native only");
        c.execute("UPDATE provider_scopes SET enable_automatic_search=0,enable_interactive_search=1 WHERE provider_id=? AND media_type='tv'", [id]).await?;
    }
    for kind in ["iptorrents", "torrentleech"] {
        let id = tracker(&c, kind, &[("tv", 0)]).await?;
        assert!(rejects(&c, "UPDATE provider_scopes SET enable_interactive_search=1 WHERE provider_id=? AND media_type='tv'", [id.clone()]).await, "{kind}: RSS only");
        assert!(rejects(&c, "UPDATE provider_scopes SET enable_automatic_search=1 WHERE provider_id=? AND media_type='tv'", [id]).await, "{kind}: RSS only");
        assert!(rejects(&c, "INSERT INTO provider_scopes(provider_id,implementation,media_type,enable_rss,enable_automatic_search,enable_interactive_search) VALUES(?,?,'movies',1,1,0)", params![uuid::Uuid::new_v4().to_string(), kind]).await);
    }
    // The generic feed keeps its zero search flags and its own minimum_seeders column.
    let feed = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'torrentrss','Feed',1,1,1,1,'http://127.0.0.1:9/feed')", [feed.clone()]).await?;
    c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,enable_rss,enable_automatic_search,enable_interactive_search,minimum_seeders) VALUES(?,'torrentrss','tv',1,0,0,5)", [feed.clone()]).await?;
    assert!(
        rejects(
            &c,
            "UPDATE provider_scopes SET enable_interactive_search=1 WHERE provider_id=?",
            [feed.clone()]
        )
        .await
    );
    // A tracker cannot use the feed-only column.
    let id = tracker(&c, "nyaa", &[("tv", 1)]).await?;
    assert!(
        rejects(
            &c,
            "UPDATE provider_scopes SET minimum_seeders=3 WHERE provider_id=?",
            [id.clone()]
        )
        .await
    );

    // Options side table: bounded, owned by a matching scope, immutable identity, cascading.
    assert!(
        rejects(
            &c,
            "UPDATE tracker_scope_options SET minimum_seeders=1000001 WHERE provider_id=?",
            [id.clone()]
        )
        .await
    );
    assert!(
        rejects(
            &c,
            "UPDATE tracker_scope_options SET minimum_seeders=-1 WHERE provider_id=?",
            [id.clone()]
        )
        .await
    );
    assert!(
        rejects(
            &c,
            "UPDATE tracker_scope_options SET categories='{}' WHERE provider_id=?",
            [id.clone()]
        )
        .await
    );
    assert!(
        rejects(
            &c,
            "UPDATE tracker_scope_options SET categories='not json' WHERE provider_id=?",
            [id.clone()]
        )
        .await
    );
    assert!(
        rejects(
            &c,
            "UPDATE tracker_scope_options SET media_type='movies' WHERE provider_id=?",
            [id.clone()]
        )
        .await
    );
    assert!(rejects(&c, "INSERT INTO tracker_scope_options(provider_id,media_type,implementation,categories) VALUES(?,'movies','nyaa','[]')", [id.clone()]).await, "no scope, no options");
    assert!(rejects(&c, "INSERT INTO tracker_scope_options(provider_id,media_type,implementation,categories) VALUES(?,'tv','hdbits','[]')", [feed.clone()]).await);
    assert!(rejects(&c, "INSERT INTO tracker_scope_options(provider_id,media_type,implementation,categories) VALUES(?,'tv','torrentrss','[]')", [feed]).await, "feeds have no side-table row");
    c.execute("UPDATE tracker_scope_options SET minimum_seeders=1000000,categories='[2,7]' WHERE provider_id=?", [id.clone()]).await?;
    c.execute(
        "DELETE FROM provider_scopes WHERE provider_id=?",
        [id.clone()],
    )
    .await?;
    assert_eq!(
        count(
            &c,
            &format!("SELECT count(*) FROM tracker_scope_options WHERE provider_id='{id}'")
        )
        .await?,
        0
    );

    // Durable backoff accepts trackers and rejects clients.
    let owner = tracker(&c, "broadcasthenet", &[("tv", 1)]).await?;
    c.execute("INSERT INTO indexer_status(provider_id,media_type,config_revision,failure_count,initial_failure_at,last_failure_at,disabled_until) VALUES(?,'tv',1,1,10,10,70)", [owner]).await?;
    let client = super::refresh_tests::provider(&c).await?;
    assert!(rejects(&c, "INSERT INTO indexer_status(provider_id,media_type,config_revision,failure_count,initial_failure_at,last_failure_at,disabled_until) VALUES(?,'tv',1,1,10,10,70)", [client]).await);
    Ok(())
}

#[tokio::test]
async fn upgrade_from_the_previous_head_preserves_data_and_applies_the_breadth_schema()
-> Result<(), Error> {
    let scratch = Scratch::new();
    let path = scratch.0.join("db");
    let (raw, c) = at_prior(&path).await?;
    let native = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'torznab','Native',1,2,3,1,'http://127.0.0.1:9/api')", [native.clone()]).await?;
    c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search) VALUES(?,'torznab','tv','[5000]','[]',0)", [native.clone()]).await?;
    let feed = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'torrentrss','Feed',1,1,1,1,'http://127.0.0.1:9/feed')", [feed.clone()]).await?;
    c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,enable_rss,enable_automatic_search,enable_interactive_search,minimum_seeders) VALUES(?,'torrentrss','movies',1,0,0,7)", [feed.clone()]).await?;
    c.execute_batch("INSERT INTO tags(id,media_type,label) VALUES(1,'tv','kept');
        INSERT INTO movie_collections(id,tmdb_id,title) VALUES(1,123,'Kept collection');
        INSERT INTO movie_collection_settings(collection_id,search_on_add,local_edit) VALUES(1,1,1);
        INSERT INTO movie_collection_intents(tmdb_id,local_edit,removed,removal_reason) VALUES(456,1,1,'user');").await?;
    c.execute(
        "INSERT INTO provider_scope_tags(provider_id,media_type,tag_id) VALUES(?,'tv',1)",
        [native.clone()],
    )
    .await?;
    c.execute("INSERT INTO provider_status(provider_id,escalation_level,status_version,last_observed_config_revision,initial_failure,most_recent_failure,disabled_until) VALUES(?,1,1,3,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z','2026-01-01T00:05:00Z')", [native.clone()]).await?;
    let preserved = [
        "schema_migrations",
        "providers",
        "provider_scopes",
        "provider_scope_tags",
        "provider_status",
        "movie_collections",
        "movie_collection_settings",
        "movie_collection_intents",
    ];
    let mut before = Vec::new();
    for table in preserved {
        before.push(rows(&c, table).await?);
    }
    assert_eq!(
        count(&c, "SELECT count(*) FROM schema_migrations").await?,
        BREADTH_MIGRATION as i64
    );
    assert!(
        c.query(
            "SELECT 1 FROM sqlite_schema WHERE name='tracker_scope_options'",
            ()
        )
        .await?
        .next()
        .await?
        .is_none()
    );
    drop(c);
    drop(raw);
    let db = Database::open_local(&path).await?;
    assert!(db.migration_backup().is_some());
    let c = db.connect().await?;
    for (table, expected) in preserved.iter().zip(&before) {
        let actual = rows(&c, table).await?;
        if *table == "schema_migrations" {
            assert_eq!(&actual[..BREADTH_MIGRATION], expected);
        } else {
            assert_eq!(&actual, expected, "changed main authority in {table}");
        }
    }
    integrity(&c).await?;
    assert_eq!(
        count(&c, "SELECT count(*) FROM schema_migrations").await?,
        MIGRATIONS.len() as i64 // Reasoning: opening the 78-entry prior head applies 79 (breadth) and every later entry, so the history is the whole registry.
    );
    assert_eq!(
        c.query("SELECT name FROM schema_migrations WHERE version=79", ())
            .await?
            .next()
            .await?
            .unwrap()
            .get::<String>(0)?,
        "indexer_breadth"
    );
    let row = c
        .query(
            "SELECT name,priority,revision FROM providers WHERE id=?",
            [native.clone()],
        )
        .await?
        .next()
        .await?
        .unwrap();
    assert_eq!(
        (
            row.get::<String>(0)?,
            row.get::<i64>(1)?,
            row.get::<i64>(2)?
        ),
        ("Native".to_string(), 2, 3)
    );
    assert_eq!(count(&c, &format!("SELECT count(*) FROM provider_scopes WHERE provider_id='{native}' AND categories='[5000]'")).await?, 1);
    assert_eq!(
        count(
            &c,
            &format!("SELECT minimum_seeders FROM provider_scopes WHERE provider_id='{feed}'")
        )
        .await?,
        7
    );
    assert_eq!(
        count(&c, "SELECT count(*) FROM tracker_scope_options").await?,
        0
    );
    let id = tracker(&c, "hdbits", &[("tv", 1), ("movies", 1)]).await?;
    assert_eq!(
        count(
            &c,
            &format!("SELECT count(*) FROM tracker_scope_options WHERE provider_id='{id}'")
        )
        .await?,
        2
    );
    // Reopening is a no-op.
    drop(c);
    drop(db);
    let reopened = Database::open_local(&path).await?;
    assert!(reopened.migration_backup().is_none());
    assert_eq!(
        count(
            &reopened.connect().await?,
            "SELECT count(*) FROM schema_migrations"
        )
        .await?,
        MIGRATIONS.len() as i64 // Reasoning: opening the 78-entry prior head applies 79 (breadth) and every later entry, so the history is the whole registry.
    );
    Ok(())
}
