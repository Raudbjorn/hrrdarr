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
const MIGRATION: &str = include_str!("../migrations/0048_provider_authority.sql");
const PREDECESSORS: &[(&str, &str)] = &[
    (
        "prototype",
        include_str!("../migrations/0001_prototype.sql"),
    ),
    (
        "media_relations",
        include_str!("../migrations/0002_media_relations.sql"),
    ),
    (
        "snapshot_imports",
        include_str!("../migrations/0003_snapshot_imports.sql"),
    ),
    (
        "quality_definitions",
        include_str!("../migrations/0004_quality_definitions.sql"),
    ),
    (
        "episode_metadata",
        include_str!("../migrations/0005_episode_metadata.sql"),
    ),
    (
        "file_metadata",
        include_str!("../migrations/0006_file_metadata.sql"),
    ),
    (
        "library_settings",
        include_str!("../migrations/0007_library_settings.sql"),
    ),
    (
        "manual_import_journal",
        include_str!("../migrations/0008_manual_import_journal.sql"),
    ),
    (
        "provider_configuration",
        include_str!("../migrations/0009_provider_configuration.sql"),
    ),
    (
        "provider_test_results",
        include_str!("../migrations/0010_provider_test_results.sql"),
    ),
    (
        "indexer_scope_options",
        include_str!("../migrations/0011_indexer_scope_options.sql"),
    ),
    (
        "qbittorrent_options",
        include_str!("../migrations/0012_qbittorrent_options.sql"),
    ),
    (
        "snapshot_provider_mappings",
        include_str!("../migrations/0013_snapshot_provider_mappings.sql"),
    ),
    (
        "root_folders",
        include_str!("../migrations/0014_root_folders.sql"),
    ),
    (
        "remote_path_mappings",
        include_str!("../migrations/0015_remote_path_mappings.sql"),
    ),
    (
        "download_refresh",
        include_str!("../migrations/0016_download_refresh.sql"),
    ),
    (
        "import_history_order",
        include_str!("../migrations/0017_import_history_order.sql"),
    ),
    (
        "snapshot_history",
        include_str!("../migrations/0018_snapshot_history.sql"),
    ),
    (
        "profile_policy",
        include_str!("../migrations/0019_profile_policy.sql"),
    ),
    (
        "snapshot_profiles",
        include_str!("../migrations/0020_snapshot_profiles.sql"),
    ),
    (
        "metadata_refresh_commands",
        include_str!("../migrations/0021_metadata_refresh_commands.sql"),
    ),
    (
        "snapshot_blocklist",
        include_str!("../migrations/0022_snapshot_blocklist.sql"),
    ),
    (
        "blocklist_clear_commands",
        include_str!("../migrations/0023_blocklist_clear_commands.sql"),
    ),
    (
        "release_catalog_policy",
        include_str!("../migrations/0024_release_catalog_policy.sql"),
    ),
    (
        "rss_grab_journal",
        include_str!("../migrations/0025_rss_grab_journal.sql"),
    ),
    (
        "download_processing",
        include_str!("../migrations/0026_download_processing.sql"),
    ),
    (
        "targeted_search",
        include_str!("../migrations/0027_targeted_search.sql"),
    ),
    (
        "same_path_replacements",
        include_str!("../migrations/0028_same_path_replacements.sql"),
    ),
    (
        "naming_settings",
        include_str!("../migrations/0029_naming_settings.sql"),
    ),
    (
        "manual_import_commands",
        include_str!("../migrations/0030_manual_import_commands.sql"),
    ),
    (
        "quality_reset_commands",
        include_str!("../migrations/0031_quality_reset_commands.sql"),
    ),
    (
        "rescan_commands",
        include_str!("../migrations/0032_rescan_commands.sql"),
    ),
    (
        "command_capacity_active_only",
        include_str!("../migrations/0033_command_capacity_active_only.sql"),
    ),
    (
        "custom_formats",
        include_str!("../migrations/0034_custom_formats.sql"),
    ),
    (
        "candidate_comparison_facts",
        include_str!("../migrations/0035_candidate_comparison_facts.sql"),
    ),
    (
        "snapshot_custom_formats",
        include_str!("../migrations/0036_snapshot_custom_formats.sql"),
    ),
    ("tags", include_str!("../migrations/0037_tags.sql")),
    (
        "revision_policy",
        include_str!("../migrations/0038_revision_policy.sql"),
    ),
    (
        "delay_profiles",
        include_str!("../migrations/0039_delay_profiles.sql"),
    ),
    (
        "release_profiles",
        include_str!("../migrations/0040_release_profiles.sql"),
    ),
    (
        "completed_download_handling",
        include_str!("../migrations/0041_completed_download_handling.sql"),
    ),
    (
        "health_checks",
        include_str!("../migrations/0042_health_checks.sql"),
    ),
    (
        "health_communication",
        include_str!("../migrations/0043_health_communication.sql"),
    ),
    (
        "health_download_client_roots",
        include_str!("../migrations/0044_health_download_client_roots.sql"),
    ),
    (
        "autotagging_metadata",
        include_str!("../migrations/0045_autotagging_metadata.sql"),
    ),
    (
        "removed_metadata_health",
        include_str!("../migrations/0046_removed_metadata_health.sql"),
    ),
    (
        "collections",
        include_str!("../migrations/0047_collections.sql"),
    ),
];
const OBJECTS: &[&str] = &[
    "provider_indexer_modes",
    "provider_modes_owner_insert",
    "provider_modes_owner_update",
    "provider_modes_required",
    "provider_scope_initialize_modes",
    "provider_modes_invalidate_test",
    "provider_scope_authority_identity",
    "provider_scope_tags",
    "provider_scope_tags_tag",
    "provider_tags_limit",
    "provider_tags_identity",
    "provider_tags_insert_invalidates_test",
    "provider_tags_delete_invalidates_test",
    "provider_scope_client_overrides",
    "provider_client_override_usage",
    "provider_override_owner",
    "provider_override_identity",
    "provider_override_insert_invalidates_test",
    "provider_override_delete_invalidates_test",
    "provider_status",
    "provider_status_insert",
    "provider_status_update",
    "provider_selection_cursors",
    "provider_cursor_insert",
    "provider_cursor_update",
];
async fn predecessor(s: &Scratch) -> Result<libsql::Database, Error> {
    let db = libsql::Builder::new_local(s.database()).build().await?;
    let c = db.connect()?;
    c.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY,name TEXT NOT NULL,checksum TEXT NOT NULL,sql TEXT NOT NULL,applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);").await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    for (i, (name, sql)) in PREDECESSORS.iter().enumerate() {
        tx.execute_batch(sql).await?;
        let checksum: String = ring::digest::digest(&ring::digest::SHA256, sql.as_bytes())
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        tx.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
            params![i as i64 + 1, *name, checksum, *sql],
        )
        .await?;
    }
    tx.commit().await?;
    Ok(db)
}

const INDEXER: &str = "00000000-0000-0000-0000-000000000001";
const CLIENT: &str = "00000000-0000-0000-0000-000000000002";
const NEWS: &str = "00000000-0000-0000-0000-000000000003";
async fn client_scope(c: &Connection, domain: &str) -> Result<(), Error> {
    c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags) VALUES(?,'qbittorrent',?,?,0,0,'started','default',0,0,0)",params![CLIENT,domain,format!("owned-{domain}")]).await?;
    Ok(())
}
async fn seed(c: &Connection) -> Result<(), Error> {
    for (id, implementation) in [
        (INDEXER, "torznab"),
        (CLIENT, "qbittorrent"),
        (NEWS, "newznab"),
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
async fn upgrade47_preserves_test_evidence_domains_and_reopens() -> Result<(), Error> {
    let s = Scratch::new();
    let raw = predecessor(&s).await?;
    let c = raw.connect()?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    seed(&c).await?;
    let before = witness(&c).await;
    drop(c);
    drop(raw);
    let db = Database::open_local(s.database()).await?;
    assert!(db.migration_backup().is_some());
    let c = db.connect().await?;
    assert_eq!(witness(&c).await, before);
    objects(&c).await;
    assert_eq!(number(&c,"SELECT count(*) FROM provider_indexer_modes WHERE enable_rss=1 AND enable_automatic_search=1 AND enable_interactive_search=1").await,3);
    assert_eq!(number(&c,"SELECT count(*) FROM provider_indexer_modes m JOIN providers p ON p.id=m.provider_id WHERE p.implementation='qbittorrent'").await,0);
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
        48
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
async fn migration_late_failure_restores47_schema_and_evidence() -> Result<(), Error> {
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
            "INSERT INTO schema_migrations SELECT * FROM schema_migrations WHERE version=47",
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
        47
    );
    assert_eq!(witness(&c).await, before);
    Ok(())
}
#[tokio::test]
async fn modes_and_tag_authority_are_domain_scoped_and_bounded() -> Result<(), Error> {
    let s = Scratch::new();
    let db = Database::open_local(s.database()).await?;
    let c = db.connect().await?;
    seed(&c).await?;
    objects(&c).await;
    assert!(
        c.execute(
            "INSERT INTO provider_indexer_modes(provider_id,media_type) VALUES(?,'movies')",
            [CLIENT]
        )
        .await
        .is_err()
    );
    assert!(
        c.execute(
            "DELETE FROM provider_indexer_modes WHERE provider_id=? AND media_type='tv'",
            [INDEXER]
        )
        .await
        .is_err()
    );
    assert!(
        c.execute(
            "UPDATE provider_indexer_modes SET enable_rss=2 WHERE provider_id=?",
            [INDEXER]
        )
        .await
        .is_err()
    );
    c.execute("UPDATE provider_indexer_modes SET enable_automatic_search=0 WHERE provider_id=? AND media_type='movies'",[INDEXER]).await?;
    assert_eq!(number(&c, "SELECT count(*) FROM provider_tests").await, 2);
    let chosen=number(&c,&format!("SELECT enable_automatic_search FROM provider_indexer_modes WHERE provider_id='{INDEXER}' AND media_type='movies'")).await;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    tx.execute(
        "UPDATE providers SET revision=revision+1,name='Renamed' WHERE id=?",
        [INDEXER],
    )
    .await?;
    tx.execute(
        "DELETE FROM provider_scopes WHERE provider_id=? AND media_type='movies'",
        [INDEXER],
    )
    .await?;
    tx.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,remove_year) VALUES(?,'torznab','movies','[5000]','[]',0)",[INDEXER]).await?;
    // Producer contract: capture old false before scope reconstruction and restore it.
    tx.execute("UPDATE provider_indexer_modes SET enable_automatic_search=? WHERE provider_id=? AND media_type='movies'",params![chosen,INDEXER]).await?;
    tx.commit().await?;
    assert_eq!(number(&c,"SELECT enable_automatic_search FROM provider_indexer_modes WHERE media_type='movies' AND provider_id='00000000-0000-0000-0000-000000000001'").await,0);
    assert_eq!(
        number(
            &c,
            "SELECT enable_automatic_search FROM provider_indexer_modes WHERE media_type='tv'"
        )
        .await,
        1
    );
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
    assert_eq!(
        number(&c, "SELECT count(*) FROM provider_indexer_modes").await,
        1
    );
    Ok(())
}
#[tokio::test]
async fn override_deferred_scope_replacement_and_cursor_lifetime() -> Result<(), Error> {
    let s = Scratch::new();
    let db = Database::open_local(s.database()).await?;
    let c = db.connect().await?;
    seed(&c).await?;
    for (owner, target) in [(CLIENT, INDEXER), (NEWS, CLIENT), (INDEXER, NEWS)] {
        assert!(c.execute("INSERT INTO provider_scope_client_overrides(provider_id,media_type,client_id) VALUES(?,'movies',?)",params![owner,target]).await.is_err());
    }
    c.execute("INSERT INTO provider_scope_client_overrides(provider_id,media_type,client_id) VALUES(?,'movies',?)",params![INDEXER,CLIENT]).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    tx.execute(
        "DELETE FROM provider_scopes WHERE provider_id=? AND media_type='movies'",
        [CLIENT],
    )
    .await?;
    client_scope(&tx, "movies").await?;
    tx.commit().await?;
    assert!(rows(&c, "PRAGMA foreign_key_check").await.is_empty());
    // Failed deferred commit remains an open transaction; explicitly roll it back.
    c.execute("BEGIN IMMEDIATE", ()).await?;
    c.execute("DELETE FROM providers WHERE id=?", [CLIENT])
        .await?;
    assert!(c.execute("COMMIT", ()).await.is_err());
    c.execute("ROLLBACK", ()).await?;
    assert_eq!(number(&c, "SELECT count(*) FROM providers").await, 3);
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
    c.execute("DELETE FROM provider_scope_client_overrides", ())
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
    for (id, level) in [(INDEXER, 9), (CLIENT, 5)] {
        c.execute("INSERT INTO provider_status(provider_id,escalation_level,status_version,last_observed_config_revision,initial_failure,most_recent_failure,disabled_until) VALUES(?,?,1,1,'2026-10-01T00:00:00Z','2026-10-01T00:01:00Z','2026-10-01T01:00:00Z')",params![id,level]).await?;
    }
    assert!(
        c.execute(
            "UPDATE provider_status SET escalation_level=6,status_version=2 WHERE provider_id=?",
            [CLIENT]
        )
        .await
        .is_err()
    );
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
    c.execute("DELETE FROM providers WHERE id=?", [CLIENT])
        .await?;
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
