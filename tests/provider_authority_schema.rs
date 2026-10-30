//! Provider authority prerequisite only: real driver migration/rollback and domain invariants.
use hrrdarr::db::{Database, Error};
use libsql::{Connection, TransactionBehavior, params};
use std::path::PathBuf;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "hrrdarr-provider-authority-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn database(&self) -> PathBuf {
        self.0.join("library.db")
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
async fn number(c: &Connection, sql: &str) -> i64 {
    c.query(sql, ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}
async fn rows(c: &Connection, sql: &str) -> Vec<String> {
    let mut q = c.query(sql, ()).await.unwrap();
    let mut out = Vec::new();
    while let Some(r) = q.next().await.unwrap() {
        out.push(
            (0..r.column_count())
                .map(|i| format!("{:?}", r.get_value(i).unwrap()))
                .collect::<Vec<_>>()
                .join("|"),
        );
    }
    out
}
const MIGRATION: &str = include_str!("../migrations/0078_provider_authority.sql");
const OBJECTS: &[&str] = &[
    "provider_scope_tags",
    "provider_scope_tags_tag",
    "provider_tags_limit",
    "provider_tags_identity",
    "provider_tags_insert_invalidates_test",
    "provider_tags_delete_invalidates_test",
    "provider_status",
    "provider_status_insert",
    "provider_status_update",
    "provider_selection_cursors",
    "provider_cursor_insert",
    "provider_cursor_update",
];
async fn predecessor(s: &Scratch) -> Result<libsql::Database, Error> {
    // Replay the real prefix: later registry rebuilds cannot be undone by dropping columns.
    let template = Database::open_local(s.0.join("migration-source.db")).await?;
    let source = template.connect().await?;
    let mut migrations = source.query(
        "SELECT version,name,checksum,sql FROM schema_migrations WHERE version<=77 ORDER BY version", (),
    ).await?;
    let db = libsql::Builder::new_local(s.database()).build().await?;
    let c = db.connect()?;
    c.execute_batch("PRAGMA foreign_keys=OFF; CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY,name TEXT NOT NULL,checksum TEXT NOT NULL,sql TEXT NOT NULL,applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);").await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let mut applied = 0;
    while let Some(row) = migrations.next().await? {
        let version = row.get::<i64>(0)?;
        let name = row.get::<String>(1)?;
        let checksum = row.get::<String>(2)?;
        let sql = row.get::<String>(3)?;
        tx.execute_batch(&sql).await?;
        tx.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
            params![version, name, checksum, sql],
        )
        .await?;
        applied += 1;
    }
    assert_eq!(applied, 77);
    tx.commit().await?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    assert!(
        c.query("PRAGMA foreign_key_check", ())
            .await?
            .next()
            .await?
            .is_none()
    );
    Ok(db)
}

const INDEXER: &str = "00000000-0000-0000-0000-000000000001";
const CLIENT: &str = "00000000-0000-0000-0000-000000000002";
const NEWS: &str = "00000000-0000-0000-0000-000000000003";
const RSS: &str = "00000000-0000-0000-0000-000000000004";
async fn client_scope(c: &Connection, domain: &str) -> Result<(), Error> {
    c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags) VALUES(?,'qbittorrent',?,?,0,0,'started','default',0,0,0)",params![CLIENT,domain,format!("owned-{domain}")]).await?;
    Ok(())
}
async fn seed(c: &Connection) -> Result<(), Error> {
    for (id, implementation) in [
        (INDEXER, "torznab"),
        (CLIENT, "qbittorrent"),
        (NEWS, "newznab"),
        (RSS, "torrentrss"),
    ] {
        c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,?,?,1,1,1,1,'http://127.0.0.1:1/')",params![id,implementation,implementation]).await?;
    }
    for (id, implementation, domain) in [
        (INDEXER, "torznab", "tv"),
        (INDEXER, "torznab", "movies"),
        (NEWS, "newznab", "movies"),
    ] {
        c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year) VALUES(?,?,?,'[5000]','[]',?,?)",params![id,implementation,domain,(domain=="tv").then_some(0),(domain=="movies").then_some(0)]).await?;
    }
    client_scope(c, "tv").await?;
    client_scope(c, "movies").await?;
    for id in [INDEXER, CLIENT, NEWS] {
        c.execute("INSERT INTO provider_tests(provider_id,config_revision,tested_at,status) VALUES(?,1,100,'success')",[id]).await?;
    }
    c.execute_batch("INSERT INTO series(id,title,path) VALUES(1,'TV','/tv/one');
     INSERT INTO movie_metadata(id,tmdb_id,title) VALUES(1,101,'Movie');
     INSERT INTO movies(id,metadata_id,path) VALUES(1,1,'/movies/one');
     INSERT INTO snapshot_imports(application,fingerprint,schema_version) VALUES('sonarr','tv',233),('radarr','movie',242);
     INSERT INTO tags(id,media_type,label) VALUES(1,'tv','tv'),(2,'movies','movies');").await?;
    Ok(())
}
async fn witness(c: &Connection) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for sql in [
        "SELECT * FROM providers ORDER BY id",
        "SELECT * FROM provider_scopes ORDER BY provider_id,media_type",
        "SELECT * FROM provider_tests ORDER BY provider_id",
        "SELECT * FROM series ORDER BY id",
        "SELECT * FROM movie_metadata ORDER BY id",
        "SELECT * FROM movies ORDER BY id",
        "SELECT * FROM tags ORDER BY id",
        "SELECT application,fingerprint,collection_version FROM snapshot_imports ORDER BY application",
    ] {
        out.push(rows(c, sql).await);
    }
    out
}
async fn objects(c: &Connection) {
    for name in OBJECTS {
        assert!(
            c.query("SELECT 1 FROM sqlite_schema WHERE name=?", [*name])
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .is_some(),
            "missing {name}"
        );
    }
    assert_eq!(number(c,"SELECT count(*) FROM pragma_table_info('snapshot_imports') WHERE name='provider_authority_version'").await,1);
}
#[tokio::test]
async fn upgrade77_preserves_test_evidence_domains_and_reopens() -> Result<(), Error> {
    let s = Scratch::new();
    let raw = predecessor(&s).await?;
    let c = raw.connect()?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    seed(&c).await?;
    let mut before = witness(&c).await;
    // 0079 appends a NULL movie last-search timestamp without changing predecessor fields.
    const MOVIE_ROWS: usize = 5;
    for row in &mut before[MOVIE_ROWS] {
        row.push_str("|Null");
    }
    drop(c);
    drop(raw);
    let db = Database::open_local(s.database()).await?;
    assert!(db.migration_backup().is_some());
    let c = db.connect().await?;
    assert_eq!(witness(&c).await, before);
    objects(&c).await;
    // Reasoning: operation modes are main's 0048 provider_scopes columns, not 0051 rows;
    // 0051 backfills no tag authority.
    assert_eq!(
        number(&c, "SELECT count(*) FROM provider_scope_tags").await,
        0
    );
    assert_eq!(number(&c, "SELECT count(*) FROM provider_status").await, 0);
    assert_eq!(
        number(
            &c,
            "SELECT count(*) FROM snapshot_imports WHERE provider_authority_version=0"
        )
        .await,
        2
    );
    assert_eq!(
        number(&c, "SELECT max(version) FROM schema_migrations").await,
        81 // Reasoning: Wanted, library-root health and Backup follow main provider authority at 78.
    );
    assert!(rows(&c, "PRAGMA foreign_key_check").await.is_empty());
    drop(c);
    drop(db);
    let db = Database::open_local(s.database()).await?;
    let c = db.connect().await?;
    assert_eq!(witness(&c).await, before);
    objects(&c).await;
    Ok(())
}
#[tokio::test]
async fn migration_late_failure_restores77_schema_and_evidence() -> Result<(), Error> {
    let s = Scratch::new();
    let raw = predecessor(&s).await?;
    let c = raw.connect()?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    seed(&c).await?;
    let before = witness(&c).await;
    let schema = rows(
        &c,
        "SELECT type,name,sql FROM sqlite_schema ORDER BY type,name",
    )
    .await;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    tx.execute_batch(MIGRATION).await?;
    assert_eq!(number(&tx, "SELECT count(*) FROM provider_tests").await, 3);
    assert!(
        tx.execute(
            "INSERT INTO schema_migrations SELECT * FROM schema_migrations WHERE version=77",
            ()
        )
        .await
        .is_err()
    );
    tx.rollback().await?;
    assert_eq!(witness(&c).await, before);
    assert_eq!(
        rows(
            &c,
            "SELECT type,name,sql FROM sqlite_schema ORDER BY type,name"
        )
        .await,
        schema
    );
    drop(c);
    drop(raw);
    let raw = libsql::Builder::new_local(s.database()).build().await?;
    let c = raw.connect()?;
    assert_eq!(
        number(&c, "SELECT max(version) FROM schema_migrations").await,
        77 // Reasoning: provider authority follows collections authority 0077.
    );
    assert_eq!(witness(&c).await, before);
    Ok(())
}
#[tokio::test]
async fn tag_authority_is_domain_scoped_and_bounded() -> Result<(), Error> {
    let s = Scratch::new();
    let db = Database::open_local(s.database()).await?;
    let c = db.connect().await?;
    seed(&c).await?;
    objects(&c).await;
    // Reasoning: indexer modes moved to main's 0048 columns, so this test now covers
    // only what 0051 owns: tags are domain-scoped and bounded per domain.
    assert!(
        c.execute(
            "INSERT INTO provider_scope_tags(provider_id,media_type,tag_id) VALUES(?,'movies',1)",
            [INDEXER]
        )
        .await
        .is_err()
    );
    c.execute(
        "INSERT INTO provider_scope_tags(provider_id,media_type,tag_id) VALUES(?,'movies',2)",
        [CLIENT],
    )
    .await?;
    assert!(c.execute("DELETE FROM tags WHERE id=2", ()).await.is_err());
    c.execute_batch("WITH RECURSIVE n(x) AS(VALUES(100) UNION ALL SELECT x+1 FROM n WHERE x<164) INSERT INTO tags(id,media_type,label) SELECT x,'movies','tag-'||x FROM n;").await?;
    assert!(c.execute("INSERT INTO provider_scope_tags(provider_id,media_type,tag_id) SELECT ?,'movies',id FROM tags WHERE id BETWEEN 100 AND 164",[INDEXER]).await.is_err());
    assert_eq!(number(&c,"SELECT count(*) FROM provider_scope_tags WHERE provider_id='00000000-0000-0000-0000-000000000001'").await,0);
    c.execute("INSERT INTO provider_scope_tags(provider_id,media_type,tag_id) SELECT ?,'movies',id FROM tags WHERE id BETWEEN 100 AND 163",[INDEXER]).await?;
    assert_eq!(number(&c,"SELECT count(*) FROM provider_scope_tags WHERE provider_id='00000000-0000-0000-0000-000000000001'").await,64);
    c.execute("DELETE FROM providers WHERE id=?", [INDEXER])
        .await?;
    // Reasoning: owner deletion cascades its tags; the client's tag survives.
    assert_eq!(
        number(&c, "SELECT count(*) FROM provider_scope_tags").await,
        1
    );
    Ok(())
}
#[tokio::test]
async fn cursor_lifetime_outlives_client_without_authority() -> Result<(), Error> {
    let s = Scratch::new();
    let db = Database::open_local(s.database()).await?;
    let c = db.connect().await?;
    seed(&c).await?;
    for domain in ["tv", "movies"] {
        c.execute("INSERT INTO provider_selection_cursors(media_type,protocol,last_client_id,revision) VALUES(?,'torrent',?,1)",params![domain,CLIENT]).await?;
    }
    assert!(c.execute("INSERT INTO provider_selection_cursors(media_type,protocol,last_client_id,revision) VALUES('movies','usenet',?,1)",[CLIENT]).await.is_err());
    assert!(c.execute("UPDATE provider_selection_cursors SET last_client_id=?,revision=revision+1 WHERE media_type='movies'",[INDEXER]).await.is_err());
    assert!(
        c.execute(
            "UPDATE provider_selection_cursors SET revision=3 WHERE media_type='movies'",
            ()
        )
        .await
        .is_err()
    );
    c.execute(
        "UPDATE provider_selection_cursors SET revision=2 WHERE media_type='movies'",
        (),
    )
    .await?;
    c.execute("DELETE FROM providers WHERE id=?", [CLIENT])
        .await?;
    // Historical cursor survives client removal; it cannot authorize a new selection.
    assert_eq!(
        number(&c, "SELECT count(*) FROM provider_selection_cursors").await,
        2
    );
    assert!(
        c.execute(
            "UPDATE provider_selection_cursors SET revision=revision+1 WHERE media_type='movies'",
            ()
        )
        .await
        .is_err()
    );
    assert_eq!(number(&c, "SELECT count(*) FROM movies").await, 1);
    assert_eq!(number(&c, "SELECT count(*) FROM series").await, 1);
    Ok(())
}
#[tokio::test]
async fn status_revision_class_dates_and_restart_do_not_reset_on_configuration_edit()
-> Result<(), Error> {
    let s = Scratch::new();
    let db = Database::open_local(s.database()).await?;
    let c = db.connect().await?;
    seed(&c).await?;
    // Every indexer implementation, torrent RSS included, owns one status row.
    for (id, level) in [(INDEXER, 9), (RSS, 9)] {
        c.execute("INSERT INTO provider_status(provider_id,escalation_level,status_version,last_observed_config_revision,initial_failure,most_recent_failure,disabled_until) VALUES(?,?,1,1,'2026-10-01T00:00:00Z','2026-10-01T00:01:00Z','2026-10-01T01:00:00Z')",params![id,level]).await?;
    }
    c.execute(
        "UPDATE provider_status SET escalation_level=4,status_version=2 WHERE provider_id=?",
        [RSS],
    )
    .await?;
    c.execute(
        "UPDATE provider_status SET escalation_level=9,status_version=3 WHERE provider_id=?",
        [RSS],
    )
    .await?;
    // Download-client backoff belongs to 0051 download_client_status, not this table.
    assert!(c.execute("INSERT INTO provider_status(provider_id,escalation_level,status_version,last_observed_config_revision) VALUES(?,0,1,1)",[CLIENT]).await.is_err());
    for date in [
        "2026-02-30T00:00:00Z",
        "2026-10-01T24:00:00Z",
        "2026-10-01T00:00:00+00:00",
        "0000-01-01T00:00:00Z",
        "bad\0date",
    ] {
        assert!(
            c.execute(
                "UPDATE provider_status SET disabled_until=?,status_version=2 WHERE provider_id=?",
                params![date, INDEXER]
            )
            .await
            .is_err()
        );
    }
    let old = rows(&c, "SELECT * FROM provider_status ORDER BY provider_id").await;
    c.execute(
        "UPDATE providers SET name='Edited',revision=2 WHERE id=?",
        [INDEXER],
    )
    .await?;
    assert_eq!(
        rows(&c, "SELECT * FROM provider_status ORDER BY provider_id").await,
        old
    );
    assert!(c.execute("UPDATE provider_status SET escalation_level=8,disabled_until=NULL,status_version=2 WHERE provider_id=?",[INDEXER]).await.is_err());
    c.execute("UPDATE provider_status SET escalation_level=8,disabled_until=NULL,status_version=2,last_observed_config_revision=2 WHERE provider_id=?",[INDEXER]).await?;
    // A wall-clock rollback does not erase the actual most recent observation.
    c.execute("UPDATE provider_status SET most_recent_failure='2026-09-30T23:59:00Z',status_version=3 WHERE provider_id=?",[INDEXER]).await?;
    let saved = rows(&c, "SELECT * FROM provider_status ORDER BY provider_id").await;
    drop(c);
    drop(db);
    let db = Database::open_local(s.database()).await?;
    let c = db.connect().await?;
    assert_eq!(
        rows(&c, "SELECT * FROM provider_status ORDER BY provider_id").await,
        saved
    );
    assert_eq!(
        number(
            &c,
            "SELECT count(*) FROM provider_status WHERE disabled_until>'2026-10-01T01:00:00Z'"
        )
        .await,
        0
    );
    assert_eq!(
        number(
            &c,
            "SELECT count(*) FROM provider_status WHERE disabled_until>'2026-10-01T00:59:59Z'"
        )
        .await,
        1
    );
    c.execute("DELETE FROM providers WHERE id=?", [RSS]).await?;
    assert_eq!(number(&c, "SELECT count(*) FROM provider_status").await, 1);
    Ok(())
}

#[tokio::test]
async fn exhausted_status_and_cursor_sequences_fail_without_partial_change() -> Result<(), Error> {
    let s = Scratch::new();
    let db = Database::open_local(s.database()).await?;
    let c = db.connect().await?;
    seed(&c).await?;
    c.execute("INSERT INTO provider_status(provider_id,escalation_level,status_version,last_observed_config_revision) VALUES(?,0,1,1)",[INDEXER]).await?;
    c.execute("INSERT INTO provider_selection_cursors(media_type,protocol,last_client_id,revision) VALUES('movies','torrent',?,1)",[CLIENT]).await?;
    // Owned fixture reaches the valid terminal sequence without billions of writes.
    // Restore the exact production guards before exercising either transition.
    for (trigger, update) in [
        (
            "provider_status_update",
            "UPDATE provider_status SET status_version=9007199254740991",
        ),
        (
            "provider_cursor_update",
            "UPDATE provider_selection_cursors SET revision=9007199254740991",
        ),
    ] {
        let sql: String = c
            .query(
                "SELECT sql FROM sqlite_schema WHERE type='trigger' AND name=?",
                [trigger],
            )
            .await?
            .next()
            .await?
            .unwrap()
            .get(0)?;
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        tx.execute(&format!("DROP TRIGGER {trigger}"), ()).await?;
        tx.execute(update, ()).await?;
        tx.execute_batch(&sql).await?;
        tx.commit().await?;
    }
    let status = rows(&c, "SELECT * FROM provider_status").await;
    let cursor = rows(&c, "SELECT * FROM provider_selection_cursors").await;
    assert!(
        c.execute(
            "UPDATE provider_status SET status_version=status_version+1",
            ()
        )
        .await
        .is_err()
    );
    assert!(
        c.execute(
            "UPDATE provider_selection_cursors SET revision=revision+1",
            ()
        )
        .await
        .is_err()
    );
    assert_eq!(rows(&c, "SELECT * FROM provider_status").await, status);
    assert_eq!(
        rows(&c, "SELECT * FROM provider_selection_cursors").await,
        cursor
    );
    assert!(rows(&c, "PRAGMA foreign_key_check").await.is_empty());
    Ok(())
}
