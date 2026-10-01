//! Collection prerequisite only: real driver migration/rollback and domain invariants.
use hrrdarr::db::{Database, Error};
use libsql::{Connection, TransactionBehavior, params};
use std::path::PathBuf;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("hrrdarr-collections-{}", uuid::Uuid::new_v4()));
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
const MIGRATION: &str = include_str!("../migrations/0047_collections.sql");
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
];
const OBJECTS: &[&str] = &[
    "movie_collection_settings",
    "movie_collection_tags",
    "movie_collection_tags_tag",
    "movie_collection_intents",
    "movie_import_exclusions",
    "collections_identity_insert",
    "collections_identity_update",
    "collections_generated_id",
    "collections_metadata_revision",
    "collections_images_insert",
    "collections_images_update",
    "collection_settings_root_insert",
    "collection_settings_root_update",
    "collection_root_domain_update",
    "collection_settings_revision",
    "collection_tags_insert",
    "collection_tags_update",
    "collection_tags_revision_insert",
    "collection_tags_revision_delete",
    "collection_members_insert",
    "collection_members_update",
    "movie_collection_intents_revision",
    "movie_collection_intents_retain",
    "movie_import_exclusions_revision",
    "movie_import_exclusions_retain",
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
async fn seed(c: &Connection) -> Result<(), Error> {
    c.execute_batch("INSERT INTO series(id,tvdb_id,title,path) VALUES(1,100,'TV','/tv/one');
     INSERT INTO seasons(series_id,number) VALUES(1,1);
     INSERT INTO episode_files(id,series_id,path) VALUES(1,1,'/tv/one/file.mkv');
     INSERT INTO episodes(id,series_id,season,number,title,episode_file_id) VALUES(1,1,1,1,'Pilot',1);
     INSERT INTO movie_metadata(id,tmdb_id,title) VALUES(1,101,'Movie'),(2,102,'Catalog');
     INSERT INTO movies(id,metadata_id,path) VALUES(1,1,'/movies/one');
     INSERT INTO movie_files(id,movie_id,path) VALUES(1,1,'/movies/one/file.mkv');
     INSERT INTO movie_collections(id,tmdb_id,title) VALUES(1,201,'Collection'),(2,NULL,'Legacy');
     INSERT INTO movie_collection_members(collection_id,metadata_id) VALUES(1,1),(1,2),(2,2);").await?;
    Ok(())
}
async fn preserved(c: &Connection) -> Vec<Vec<String>> {
    let mut v = Vec::new();
    for sql in [
        "SELECT * FROM series ORDER BY id",
        "SELECT * FROM seasons ORDER BY series_id,number",
        "SELECT * FROM episodes ORDER BY id",
        "SELECT * FROM episode_files ORDER BY id",
        "SELECT * FROM movie_metadata ORDER BY id",
        "SELECT * FROM movies ORDER BY id",
        "SELECT * FROM movie_files ORDER BY id",
        "SELECT id,tmdb_id,title FROM movie_collections ORDER BY id",
        "SELECT collection_id,metadata_id FROM movie_collection_members ORDER BY collection_id,metadata_id",
        "PRAGMA foreign_key_list(movie_collection_members)",
    ] {
        v.push(rows(c, sql).await);
    }
    v
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
    assert_eq!(number(c,"SELECT count(*) FROM pragma_table_info('movie_collections') WHERE name IN('sort_title','overview','images_json','metadata_revision','graph_complete','added','last_info_sync')").await,7);
    assert_eq!(number(c,"SELECT count(*) FROM pragma_table_info('snapshot_imports') WHERE name='collection_version'").await,1);
}
#[tokio::test]
async fn upgrade46_preserves_both_domains_unknowns_and_multiple_memberships() -> Result<(), Error> {
    let s = Scratch::new();
    let raw = predecessor(&s).await?;
    let c = raw.connect()?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    seed(&c).await?;
    let before = preserved(&c).await;
    drop(c);
    drop(raw);
    let db = Database::open_local(s.database()).await?;
    assert!(db.migration_backup().is_some());
    let c = db.connect().await?;
    objects(&c).await;
    assert_eq!(preserved(&c).await, before);
    assert_eq!(number(&c,"SELECT count(*) FROM movie_collections WHERE metadata_revision=0 AND graph_complete=0 AND images_json IS NULL AND last_info_sync IS NULL AND added IS NULL").await,2);
    assert_eq!(number(&c,"SELECT count(*) FROM movie_collection_members WHERE origin='legacy' AND metadata_revision=0").await,3);
    assert_eq!(
        number(&c, "SELECT count(*) FROM movie_collection_settings").await,
        0
    );
    assert_eq!(
        number(&c, "SELECT max(version) FROM schema_migrations").await,
        48 // Latest runner adds provider authority48; fixed46/47 migration fixtures remain unchanged.
    );
    assert!(rows(&c, "PRAGMA foreign_key_check").await.is_empty());
    drop(c);
    drop(db);
    let db = Database::open_local(s.database()).await?;
    let c = db.connect().await?;
    assert_eq!(preserved(&c).await, before);
    objects(&c).await;
    Ok(())
}
#[tokio::test]
async fn late_failure_rolls_back_all47_ddl_and_facts() -> Result<(), Error> {
    let s = Scratch::new();
    let db = predecessor(&s).await?;
    let c = db.connect()?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    seed(&c).await?;
    let before = preserved(&c).await;
    let schema = rows(
        &c,
        "SELECT type,name,sql FROM sqlite_schema ORDER BY type,name",
    )
    .await;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    tx.execute_batch(MIGRATION).await?;
    tx.execute(
        "INSERT INTO movie_import_exclusions(tmdb_id,local_edit) VALUES(102,1)",
        (),
    )
    .await?;
    assert!(
        tx.execute(
            "INSERT INTO schema_migrations SELECT * FROM schema_migrations WHERE version=46",
            ()
        )
        .await
        .is_err()
    );
    tx.rollback().await?;
    assert_eq!(preserved(&c).await, before);
    assert_eq!(
        rows(
            &c,
            "SELECT type,name,sql FROM sqlite_schema ORDER BY type,name"
        )
        .await,
        schema
    );
    drop(c);
    drop(db);
    let db = libsql::Builder::new_local(s.database()).build().await?;
    let c = db.connect()?;
    assert_eq!(
        number(&c, "SELECT max(version) FROM schema_migrations").await,
        46
    );
    assert_eq!(preserved(&c).await, before);
    Ok(())
}
#[tokio::test]
async fn fresh_defaults_domains_revisions_and_local_tombstones() -> Result<(), Error> {
    let s = Scratch::new();
    let db = Database::open_local(s.database()).await?;
    let c = db.connect().await?;
    objects(&c).await;
    seed(&c).await?;
    c.execute_batch(
        "INSERT INTO root_folders(id,media_type,path) VALUES(1,'tv','/tv'),(2,'movies','/movies');
      INSERT INTO quality_profiles(id,media_type,name) VALUES(1,'tv','TV'),(2,'movies','Movies');
      INSERT INTO tags(id,media_type,label) VALUES(1,'tv','tv'),(2,'movies','movies');
      INSERT INTO movie_collection_settings(collection_id) VALUES(1);",
    )
    .await?;
    for sql in [
        "UPDATE movie_collection_settings SET monitored=1,settings_revision=2 WHERE collection_id=1",
        "UPDATE movie_collection_settings SET root_folder_id=1,settings_revision=2 WHERE collection_id=1",
        "UPDATE movie_collection_settings SET quality_profile_id=1,settings_revision=2 WHERE collection_id=1",
        "INSERT INTO movie_collection_tags(collection_id,tag_id) VALUES(1,1)",
        "UPDATE movie_collection_settings SET search_on_add=1 WHERE collection_id=1",
        "UPDATE movie_collections SET title='Changed' WHERE id=1",
    ] {
        assert!(c.execute(sql, ()).await.is_err(), "accepted {sql}");
    }
    c.execute("UPDATE movie_collection_settings SET monitored=1,root_folder_id=2,quality_profile_id=2,minimum_availability='released',search_on_add=0,local_edit=1,settings_revision=2 WHERE collection_id=1",()).await?;
    c.execute(
        "INSERT INTO movie_collection_tags(collection_id,tag_id) VALUES(1,2)",
        (),
    )
    .await?;
    assert_eq!(
        number(
            &c,
            "SELECT settings_revision FROM movie_collection_settings WHERE collection_id=1"
        )
        .await,
        3
    );
    for sql in [
        "DELETE FROM tags WHERE id=2",
        "DELETE FROM root_folders WHERE id=2",
        "DELETE FROM quality_profiles WHERE id=2",
        "UPDATE root_folders SET media_type='tv' WHERE id=2",
        "UPDATE movie_collection_settings SET local_edit=0,settings_revision=4 WHERE collection_id=1",
    ] {
        assert!(c.execute(sql, ()).await.is_err(), "accepted {sql}");
    }
    c.execute(
        "DELETE FROM movie_collection_tags WHERE collection_id=1",
        (),
    )
    .await?;
    assert_eq!(
        number(
            &c,
            "SELECT local_edit FROM movie_collection_settings WHERE collection_id=1"
        )
        .await,
        1
    );
    c.execute_batch("INSERT INTO movie_collection_intents(tmdb_id,local_edit,removed,removal_reason) VALUES(201,1,1,'user');
      INSERT INTO movie_import_exclusions(tmdb_id,local_edit) VALUES(102,1);
      UPDATE movie_import_exclusions SET excluded=0,revision=2 WHERE tmdb_id=102;").await?;
    for sql in [
        "DELETE FROM movie_collection_intents",
        "DELETE FROM movie_import_exclusions",
        "UPDATE movie_import_exclusions SET excluded=1 WHERE tmdb_id=102",
        "UPDATE movie_import_exclusions SET local_edit=0,revision=3 WHERE tmdb_id=102",
    ] {
        assert!(c.execute(sql, ()).await.is_err(), "accepted {sql}");
    }
    // Collection teardown is explicit and cannot remove a movie, file or exclusion.
    c.execute_batch("DELETE FROM movie_collection_settings WHERE collection_id=1;DELETE FROM movie_collection_members WHERE collection_id=1;DELETE FROM movie_collections WHERE id=1;").await?;
    assert_eq!(number(&c, "SELECT count(*) FROM movie_files").await, 1);
    assert_eq!(number(&c, "SELECT count(*) FROM episode_files").await, 1);
    assert_eq!(
        number(
            &c,
            "SELECT count(*) FROM movie_import_exclusions WHERE excluded=0"
        )
        .await,
        1
    );
    Ok(())
}
#[tokio::test]
async fn fact_shapes_dates_graph_fences_and_capacity_rollback() -> Result<(), Error> {
    let s = Scratch::new();
    let db = Database::open_local(s.database()).await?;
    let c = db.connect().await?;
    seed(&c).await?;
    for images in [
        r#"[null]"#,
        r#"[{"cover_type":"poster","wrong":"x"}]"#,
        r#"[{"source_url":"x","other":"y"}]"#,
        r#"[{"cover_type":"poster","source_url":null}]"#,
        r#"[{"cover_type":"a","cover_type":"b"}]"#,
        r#"[{"cover_type":"poster","source_url":"x","extra":1}]"#,
    ] {
        assert!(
            c.execute(
                "UPDATE movie_collections SET images_json=?,metadata_revision=1 WHERE id=1",
                [images]
            )
            .await
            .is_err()
        );
    }
    for date in [
        "2025-02-30T00:00:00Z",
        "2025-01-01T00:00:00+00:00",
        "2025-01-01T24:00:00Z",
        "0000-01-01T00:00:00Z",
        "invalid",
    ] {
        assert!(
            c.execute(
                "UPDATE movie_collections SET last_info_sync=?,metadata_revision=1 WHERE id=1",
                [date]
            )
            .await
            .is_err(),
            "accepted {date}"
        );
    }
    c.execute("UPDATE movie_collections SET images_json='[{\"cover_type\":\"unknown\",\"source_url\":\"private:fact\"}]',last_info_sync='2024-02-29T00:00:00Z',graph_complete=1,metadata_revision=1 WHERE id=1",()).await?;
    assert!(
        c.execute(
            "UPDATE movie_collection_members SET origin='graph' WHERE collection_id=1",
            ()
        )
        .await
        .is_err()
    );
    c.execute("UPDATE movie_collection_members SET origin='graph',metadata_revision=1 WHERE collection_id=1",()).await?;
    let before = preserved(&c).await;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    tx.execute(
        "DELETE FROM movie_collection_members WHERE collection_id=1",
        (),
    )
    .await?;
    tx.execute(
        "UPDATE movie_collections SET metadata_revision=2 WHERE id=1",
        (),
    )
    .await?;
    tx.execute_batch("WITH RECURSIVE n(x) AS(VALUES(1000) UNION ALL SELECT x+1 FROM n WHERE x<2000) INSERT INTO movie_metadata(id,tmdb_id,title) SELECT x,x,'Member' FROM n;").await?;
    assert!(tx.execute("INSERT INTO movie_collection_members(collection_id,metadata_id,origin,metadata_revision) SELECT 1,id,'graph',2 FROM movie_metadata WHERE id BETWEEN 1000 AND 2000",()).await.is_err());
    tx.rollback().await?;
    assert_eq!(preserved(&c).await, before);
    c.execute("INSERT INTO movie_collections(id,tmdb_id,title,metadata_revision) VALUES(3,203,'Exhausted',9007199254740991)",()).await?;
    assert!(c.execute("UPDATE movie_collections SET title='Changed',metadata_revision=metadata_revision+1 WHERE id=3",()).await.is_err());
    c.execute("INSERT INTO movie_collection_settings(collection_id,settings_revision) VALUES(3,9007199254740991)",()).await?;
    c.execute(
        "INSERT INTO tags(id,media_type,label) VALUES(3,'movies','overflow')",
        (),
    )
    .await?;
    assert!(
        c.execute(
            "INSERT INTO movie_collection_tags(collection_id,tag_id) VALUES(3,3)",
            ()
        )
        .await
        .is_err()
    );
    assert_eq!(
        number(
            &c,
            "SELECT count(*) FROM movie_collection_tags WHERE collection_id=3"
        )
        .await,
        0
    );
    Ok(())
}

#[tokio::test]
async fn tag_and_artwork_limits_fail_atomically() -> Result<(), Error> {
    let s = Scratch::new();
    let db = Database::open_local(s.database()).await?;
    let c = db.connect().await?;
    seed(&c).await?;
    c.execute(
        "INSERT INTO movie_collection_settings(collection_id) VALUES(1)",
        (),
    )
    .await?;
    c.execute_batch("WITH RECURSIVE n(x) AS(VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<201) INSERT INTO tags(id,media_type,label) SELECT x,'movies','tag-'||x FROM n;").await?;
    assert!(c.execute("INSERT INTO movie_collection_tags(collection_id,tag_id) SELECT 1,id FROM tags ORDER BY id", ()).await.is_err());
    assert_eq!(
        number(&c, "SELECT count(*) FROM movie_collection_tags").await,
        0
    );
    assert_eq!(
        number(
            &c,
            "SELECT settings_revision FROM movie_collection_settings WHERE collection_id=1"
        )
        .await,
        1
    );
    c.execute("INSERT INTO movie_collection_tags(collection_id,tag_id) SELECT 1,id FROM tags WHERE id<=200 ORDER BY id", ()).await?;
    assert_eq!(
        number(&c, "SELECT count(*) FROM movie_collection_tags").await,
        200
    );
    let too_many = serde_json::to_string(&vec![
        serde_json::json!({"cover_type":"poster","source_url":"private"});
        33
    ])?;
    let too_large = serde_json::to_string(&vec![
        serde_json::json!({"cover_type":"p".repeat(64),"source_url":"u".repeat(2048)});
        32
    ])?;
    assert!(too_large.len() > 65536);
    for images in [too_many, too_large] {
        assert!(
            c.execute(
                "UPDATE movie_collections SET images_json=?,metadata_revision=1 WHERE id=1",
                [images]
            )
            .await
            .is_err()
        );
    }
    assert_eq!(
        number(
            &c,
            "SELECT metadata_revision FROM movie_collections WHERE id=1"
        )
        .await,
        0
    );
    Ok(())
}
