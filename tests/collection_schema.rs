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
const MIGRATION: &str = include_str!("../migrations/0077_collection_authority.sql");
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
    // Replay the real prefix: later registry rebuilds cannot be undone by dropping columns.
    let template = Database::open_local(s.0.join("migration-source.db")).await?;
    let source = template.connect().await?;
    let mut migrations = source.query(
        "SELECT version,name,checksum,sql FROM schema_migrations WHERE version<=76 ORDER BY version", (),
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
    assert_eq!(applied, 76);
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
async fn upgrade76_preserves_both_domains_unknowns_and_multiple_memberships() -> Result<(), Error> {
    let s = Scratch::new();
    let raw = predecessor(&s).await?;
    let c = raw.connect()?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    seed(&c).await?;
    c.execute_batch("INSERT INTO quality_profiles(id,media_type,name)VALUES(50,'movies','Retained profile');
        INSERT INTO root_folders(id,media_type,path)VALUES(50,'movies','/collection-root');
        INSERT INTO tags(id,media_type,label)VALUES(50,'movies','retained');
        UPDATE movie_collections SET monitored=1,search_on_add=1,quality_profile_id=50,root_folder_id=50,minimum_availability='released' WHERE id=1;
        INSERT INTO movie_collection_tags(collection_id,tag_id)VALUES(1,50);
        INSERT INTO root_folders(id,media_type,path)VALUES(51,'movies','/removed-root');
        INSERT INTO movie_collections(id,tmdb_id,title)VALUES(3,203,'Orphaned policy');
        UPDATE movie_collections SET monitored=1,quality_profile_id=50,root_folder_id=51,minimum_availability='released' WHERE id=3;
        DELETE FROM root_folders WHERE id=51;").await?;
    let old_policy = rows(&c,"SELECT id,monitored,search_on_add,quality_profile_id,root_folder_id,minimum_availability FROM movie_collections WHERE id=1").await;
    let old_tags = rows(
        &c,
        "SELECT * FROM movie_collection_tags ORDER BY collection_id,tag_id",
    )
    .await;
    let before = preserved(&c).await;
    drop(c);
    drop(raw);
    let db = Database::open_local(s.database()).await?;
    assert!(db.migration_backup().is_some());
    let c = db.connect().await?;
    // The old SET NULL root FK permits monitored-but-incomplete policy; preserve it disabled.
    assert_eq!(number(&c,"SELECT count(*) FROM movie_collection_settings WHERE collection_id=3 AND monitored=0 AND root_folder_id IS NULL AND quality_profile_id=50 AND minimum_availability='released' AND local_edit=1").await,1);
    objects(&c).await;
    assert_eq!(preserved(&c).await, before);
    assert_eq!(number(&c,"SELECT count(*) FROM movie_collections WHERE metadata_revision=0 AND graph_complete=0 AND images_json IS NULL AND last_info_sync IS NULL AND added IS NULL").await,3);
    assert_eq!(number(&c,"SELECT count(*) FROM movie_collection_members WHERE origin='legacy' AND metadata_revision=0").await,3);
    assert_eq!(
        number(&c, "SELECT count(*) FROM movie_collection_settings").await,
        2 // Reasoning: populated and orphaned policies migrate; bare catalog id2 stays policy-free.
    );
    assert_eq!(
        number(&c, "SELECT max(version) FROM schema_migrations").await,
        78 // Reasoning: the merged runner appends collections and provider authority after the original 76 migrations.
    );
    assert_eq!(rows(&c,"SELECT collection_id,monitored,search_on_add,quality_profile_id,root_folder_id,minimum_availability FROM movie_collection_settings WHERE collection_id=1").await, old_policy);
    assert_eq!(
        rows(
            &c,
            "SELECT * FROM movie_collection_tags ORDER BY collection_id,tag_id"
        )
        .await,
        old_tags
    );
    assert_eq!(number(&c,"SELECT count(*) FROM movie_collection_settings WHERE collection_id=1 AND local_edit=1 AND snapshot_owned=0").await,1);
    assert_eq!(number(&c,"SELECT count(*) FROM movie_collections WHERE monitored=0 AND search_on_add=0 AND root_folder_id IS NULL AND quality_profile_id IS NULL AND minimum_availability IS NULL").await,3);
    assert!(
        c.execute("DELETE FROM quality_profiles WHERE id=50", ())
            .await
            .is_err()
    );
    assert!(
        c.execute("DELETE FROM root_folders WHERE id=50", ())
            .await
            .is_err()
    );
    assert!(c.execute("DELETE FROM tags WHERE id=50", ()).await.is_err());
    assert!(rows(&c, "PRAGMA foreign_key_check").await.is_empty());
    drop(c);
    drop(db);
    let db = Database::open_local(s.database()).await?;
    let c = db.connect().await?;
    assert_eq!(preserved(&c).await, before);
    assert_eq!(rows(&c,"SELECT collection_id,monitored,search_on_add,quality_profile_id,root_folder_id,minimum_availability FROM movie_collection_settings WHERE collection_id=1").await, old_policy);
    assert_eq!(
        rows(
            &c,
            "SELECT * FROM movie_collection_tags ORDER BY collection_id,tag_id"
        )
        .await,
        old_tags
    );
    assert_eq!(number(&c,"SELECT count(*) FROM movie_collection_settings WHERE collection_id=1 AND local_edit=1 AND snapshot_owned=0").await,1);
    assert_eq!(number(&c,"SELECT count(*) FROM movie_collections WHERE monitored=0 AND search_on_add=0 AND root_folder_id IS NULL AND quality_profile_id IS NULL AND minimum_availability IS NULL").await,3);
    assert!(
        c.execute("DELETE FROM quality_profiles WHERE id=50", ())
            .await
            .is_err()
    );
    assert!(
        c.execute("DELETE FROM root_folders WHERE id=50", ())
            .await
            .is_err()
    );
    assert!(c.execute("DELETE FROM tags WHERE id=50", ()).await.is_err());
    // The old SET NULL root FK permits monitored-but-incomplete policy; preserve it disabled.
    assert_eq!(number(&c,"SELECT count(*) FROM movie_collection_settings WHERE collection_id=3 AND monitored=0 AND root_folder_id IS NULL AND quality_profile_id=50 AND minimum_availability='released' AND local_edit=1").await,1);
    objects(&c).await;
    Ok(())
}
#[tokio::test]
async fn late_failure_rolls_back_all_collections_ddl_and_facts() -> Result<(), Error> {
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
            "INSERT INTO schema_migrations SELECT * FROM schema_migrations WHERE version=76",
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
        76 // Reasoning: collections authority now follows the preserved branch history through 0076.
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
