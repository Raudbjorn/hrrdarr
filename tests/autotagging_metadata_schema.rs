//! Pinned-driver column proof and real metadata-foundation migration recovery tests.
use hrrdarr::db::{Database, Error};
use libsql::{Connection, TransactionBehavior};
use std::path::PathBuf;

const REPLACE_RUNTIME: &str = "
ALTER TABLE movie_metadata ADD COLUMN autotag_runtime_next INTEGER
 CHECK(autotag_runtime_next IS NULL OR
 (typeof(autotag_runtime_next)='integer' AND autotag_runtime_next BETWEEN 0 AND 10080));
UPDATE movie_metadata SET autotag_runtime_next=runtime;
ALTER TABLE movie_metadata DROP COLUMN runtime;
ALTER TABLE movie_metadata RENAME COLUMN autotag_runtime_next TO runtime;";

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("hrrdarr-autotag-schema-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
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

// Explicit immutable predecessor: registering45 must not silently turn this into a45 fixture.
async fn predecessor(path: &std::path::Path) -> Result<libsql::Database, Error> {
    let db = libsql::Builder::new_local(path).build().await?;
    let c = db.connect()?;
    c.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY,name TEXT NOT NULL,checksum TEXT NOT NULL,sql TEXT NOT NULL,applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);").await?;
    let migrations = [
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
    ];
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    for (index, (name, sql)) in migrations.iter().enumerate() {
        tx.execute_batch(sql).await?;
        let checksum: String = ring::digest::digest(&ring::digest::SHA256, sql.as_bytes())
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        tx.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
            libsql::params![index as i64 + 1, *name, checksum, *sql],
        )
        .await?;
    }
    tx.commit().await?;
    Ok(db)
}
async fn raw_connection(db: &libsql::Database) -> Result<Connection, Error> {
    let c = db.connect()?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    Ok(c)
}

async fn scalar(c: &Connection, sql: &str) -> Result<i64, Error> {
    Ok(c.query(sql, ()).await?.next().await?.unwrap().get(0)?)
}

// Named projections deliberately avoid assuming runtime retains its old ordinal.
async fn rows(c: &Connection, sql: &str) -> Result<Vec<String>, Error> {
    let mut result = c.query(sql, ()).await?;
    let mut out = Vec::new();
    while let Some(row) = result.next().await? {
        let mut cells = Vec::new();
        for index in 0..row.column_count() {
            cells.push(format!("{:?}", row.get_value(index)?));
        }
        out.push(cells.join("|"));
    }
    Ok(out)
}

async fn witness(c: &Connection) -> Result<Vec<Vec<String>>, Error> {
    let mut out = Vec::new();
    for sql in [
        "SELECT id,tmdb_id,imdb_id,title,year,runtime,status,in_cinemas,digital_release,physical_release,secondary_year,original_language FROM movie_metadata ORDER BY id",
        "SELECT * FROM movies ORDER BY id",
        "SELECT * FROM movie_collections ORDER BY id",
        "SELECT * FROM movie_collection_members ORDER BY collection_id,metadata_id",
        "SELECT * FROM movie_alternative_titles ORDER BY metadata_id,title",
        "SELECT * FROM movie_files ORDER BY id",
        "SELECT * FROM file_metadata ORDER BY id",
        "SELECT * FROM library_settings ORDER BY id",
        "SELECT id,tvdb_id,title,year,path,poster,monitored,original_language FROM series ORDER BY id",
        "SELECT * FROM seasons ORDER BY series_id,number",
        "SELECT * FROM episodes ORDER BY id",
        "SELECT * FROM episode_files ORDER BY id",
        "SELECT application,fingerprint,schema_version,imported_at,episode_metadata_version,history_version,profile_version,blocklist_version,custom_format_version,tag_version,revision_policy_version,delay_profile_version,release_profile_version,cdh_version FROM snapshot_imports ORDER BY application,fingerprint",
        "SELECT * FROM snapshot_mappings ORDER BY application,fingerprint,destination_table,source_id",
        "SELECT * FROM snapshot_records ORDER BY application,fingerprint,source_table,ordinal",
        "SELECT * FROM metadata_refresh_commands ORDER BY id",
        "SELECT * FROM schema_migrations WHERE version<=44 ORDER BY version",
        "SELECT * FROM sqlite_sequence ORDER BY name",
        // The latest runner adds46's three movie-membership invalidation triggers;
        // compare every predecessor object exactly, with those explicit additions
        // covered by removed_metadata_health_tests. Existing45 exclusions stay fixed.
        "SELECT type,name,tbl_name,sql FROM sqlite_schema WHERE tbl_name NOT IN ('movie_metadata','series','snapshot_imports') AND NOT (tbl_name='movies' AND name IN ('removed_metadata_movie_insert','removed_metadata_movie_delete','removed_metadata_movie_update')) ORDER BY type,name",
        "PRAGMA foreign_key_list(movies)",
        "PRAGMA foreign_key_list(movie_collection_members)",
        "PRAGMA foreign_key_list(movie_alternative_titles)",
    ] {
        out.push(rows(c, sql).await?);
    }
    Ok(out)
}

async fn seed(c: &Connection) -> Result<(), Error> {
    // This proof starts from the accepted complete schema44, not a minimal lookalike.
    assert_eq!(
        scalar(c, "SELECT max(version) FROM schema_migrations").await?,
        44
    );
    assert_eq!(scalar(c, "PRAGMA foreign_keys").await?, 1);
    c.execute_batch("INSERT INTO movie_metadata(id,tmdb_id,imdb_id,title,year,runtime,status,in_cinemas,digital_release,physical_release,secondary_year,original_language) VALUES
 (1,101,'tt101','Film',2020,1,'released','2020-01-01 00:00:00','2020-02-01 00:00:00','2020-03-01 00:00:00',2019,1),
 (2,102,'tt102','Long',2021,10080,NULL,NULL,NULL,NULL,NULL,NULL),
 (3,103,'tt103','Unknown',2022,NULL,NULL,NULL,NULL,NULL,NULL,NULL);
INSERT INTO movies(id,metadata_id,path,monitored) VALUES(1,1,'/fictional/movie',0);
INSERT INTO movie_files(id,movie_id,path,edition) VALUES(1,1,'/fictional/movie/film.mkv','Extended');
INSERT INTO movie_collections(id,tmdb_id,title) VALUES(1,201,'Collection');
INSERT INTO movie_collection_members(collection_id,metadata_id) VALUES(1,1),(1,2);
INSERT INTO movie_alternative_titles(metadata_id,title) VALUES(1,'Alias'),(3,'Unreferenced alias');
INSERT INTO series(id,tvdb_id,title,path) VALUES(1,101,'Series','/fictional/tv');
INSERT INTO seasons(series_id,number) VALUES(1,1);
INSERT INTO episode_files(id,series_id,path) VALUES(1,1,'/fictional/tv/episode.mkv');
INSERT INTO episodes(id,series_id,season,number,title,episode_file_id,runtime) VALUES(1,1,1,1,'Episode',1,45);
INSERT INTO file_metadata(id,media_type,movie_file_id,size) VALUES(1,'movies',1,1234);
INSERT INTO file_metadata(id,media_type,episode_file_id,size) VALUES(2,'tv',1,5678);
INSERT INTO library_settings(id,media_type,movie_id,minimum_availability) VALUES(1,'movies',1,'released');
INSERT INTO library_settings(id,media_type,series_id,season_folder) VALUES(2,'tv',1,0);
INSERT INTO snapshot_imports(application,fingerprint,schema_version) VALUES('sonarr','tv-proof',233),('radarr','movie-proof',242);
INSERT INTO snapshot_mappings(application,fingerprint,destination_table,source_id,destination_id) VALUES
 ('sonarr','tv-proof','series',71,1),('sonarr','tv-proof','episodes',72,1),
 ('radarr','movie-proof','movie_metadata',71,1),('radarr','movie-proof','movies',72,1);
INSERT INTO snapshot_records(application,fingerprint,source_table,ordinal,record_json) VALUES
 ('sonarr','tv-proof','Series',0,'{\"Id\":71}'),('radarr','movie-proof','MovieMetadata',0,'{\"Id\":71}');
INSERT INTO metadata_refresh_commands(id,name,media_type,movie_id,external_id,metadata_id,next_attempt_at,created_at) VALUES('11111111-1111-4111-8111-111111111111','refresh_movie','movies',1,101,1,0,0);").await?;
    Ok(())
}

async fn integrity(c: &Connection) -> Result<(), Error> {
    assert_eq!(scalar(c, "PRAGMA foreign_keys").await?, 1);
    assert!(rows(c, "PRAGMA foreign_key_check").await?.is_empty());
    let mut result = c.query("PRAGMA integrity_check", ()).await?;
    assert_eq!(result.next().await?.unwrap().get::<String>(0)?, "ok");
    Ok(())
}

#[tokio::test]
async fn runtime_column_replacement_preserves_schema44_relationships_and_reopens()
-> Result<(), Error> {
    let scratch = Scratch::new();
    let db = predecessor(&scratch.database()).await?;
    let c = raw_connection(&db).await?;
    seed(&c).await?;
    let before = witness(&c).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    tx.execute_batch(REPLACE_RUNTIME).await?;
    integrity(&tx).await?;
    assert_eq!(witness(&tx).await?, before);
    assert_eq!(scalar(&tx, "SELECT count(*) FROM pragma_table_info('movie_metadata') WHERE name='autotag_runtime_next'").await?, 0);
    // RENAME COLUMN must rewrite the CHECK to the final name; ordinary writes exercise it.
    for literal in ["NULL", "0", "1", "10080"] {
        tx.execute(
            &format!("UPDATE movie_metadata SET runtime={literal} WHERE id=3"),
            (),
        )
        .await?;
    }
    for literal in ["-1", "10081", "0.5", "'invalid'", "x'00'"] {
        assert!(
            tx.execute(
                &format!("UPDATE movie_metadata SET runtime={literal} WHERE id=3"),
                ()
            )
            .await
            .is_err(),
            "accepted runtime {literal}"
        );
    }
    // Affinity-coercible input is allowed; the durable representation must be an integer.
    tx.execute("UPDATE movie_metadata SET runtime='0' WHERE id=3", ())
        .await?;
    assert_eq!(scalar(&tx, "SELECT count(*) FROM movie_metadata WHERE id=3 AND runtime=0 AND typeof(runtime)='integer'").await?, 1);
    for sql in [
        "INSERT INTO movie_metadata(tmdb_id,title) VALUES(101,'Duplicate')",
        "INSERT INTO movie_metadata(imdb_id,title) VALUES('tt101','Duplicate')",
        "INSERT INTO movies(metadata_id,path) VALUES(999,'/fictional/orphan')",
        "INSERT INTO movie_collection_members(collection_id,metadata_id) VALUES(1,999)",
        "INSERT INTO movie_alternative_titles(metadata_id,title) VALUES(999,'Orphan')",
        "DELETE FROM movie_metadata WHERE id=1",
        "DELETE FROM movie_metadata WHERE id=2",
    ] {
        assert!(tx.execute(sql, ()).await.is_err(), "accepted {sql}");
    }
    // The cross-table captured-identity trigger must still resolve the unchanged parent.
    tx.execute("UPDATE metadata_refresh_commands SET status='running',attempts=1,started_at=1 WHERE id='11111111-1111-4111-8111-111111111111'", ()).await?;
    assert!(tx.execute("INSERT INTO metadata_refresh_commands(id,name,media_type,movie_id,external_id,metadata_id,next_attempt_at,created_at) VALUES('22222222-2222-4222-8222-222222222222','refresh_movie','movies',1,999,1,0,0)", ()).await.is_err());
    tx.commit().await?;
    let committed = witness(&c).await?;
    drop(c);
    drop(db);
    // Reopen this isolated column-capability proof without activating the registered45 migration.
    let db = libsql::Builder::new_local(scratch.database())
        .build()
        .await?;
    let c = raw_connection(&db).await?;
    assert_eq!(witness(&c).await?, committed);
    integrity(&c).await?;
    c.execute("DELETE FROM movie_metadata WHERE id=3", ())
        .await?;
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM movie_alternative_titles WHERE metadata_id=3"
        )
        .await?,
        0
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM episodes WHERE id=1 AND episode_file_id=1 AND runtime=45"
        )
        .await?,
        1
    );
    drop(c);
    drop(db);
    Ok(())
}

#[tokio::test]
async fn runtime_column_replacement_late_failure_rolls_back_ddl_and_data() -> Result<(), Error> {
    let scratch = Scratch::new();
    let db = predecessor(&scratch.database()).await?;
    let c = raw_connection(&db).await?;
    seed(&c).await?;
    let before = witness(&c).await?;
    let schema = rows(
        &c,
        "SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name",
    )
    .await?;
    let columns = rows(&c, "PRAGMA table_info(movie_metadata)").await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    tx.execute_batch(REPLACE_RUNTIME).await?;
    tx.execute("UPDATE movie_metadata SET runtime=0 WHERE id=1", ())
        .await?;
    // Fail after every ALTER, analogous to a rejected migration-history publication.
    assert!(
        tx.execute(
            "INSERT INTO schema_migrations SELECT * FROM schema_migrations WHERE version=44",
            ()
        )
        .await
        .is_err()
    );
    tx.rollback().await?;
    assert_eq!(witness(&c).await?, before);
    assert_eq!(
        rows(
            &c,
            "SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name"
        )
        .await?,
        schema
    );
    assert_eq!(
        rows(&c, "PRAGMA table_info(movie_metadata)").await?,
        columns
    );
    assert!(
        c.execute("UPDATE movie_metadata SET runtime=0 WHERE id=1", ())
            .await
            .is_err()
    );
    drop(c);
    drop(db);
    let db = libsql::Builder::new_local(scratch.database())
        .build()
        .await?;
    let c = raw_connection(&db).await?;
    assert_eq!(witness(&c).await?, before);
    integrity(&c).await?;
    drop(c);
    drop(db);
    Ok(())
}

#[tokio::test]
async fn foundation_real_runner_upgrade_backup_reopen_preserves_predecessor() -> Result<(), Error> {
    let scratch = Scratch::new();
    let old = predecessor(&scratch.database()).await?;
    let c = raw_connection(&old).await?;
    seed(&c).await?;
    let before = witness(&c).await?;
    drop(c);
    drop(old);
    let db = Database::open_local(scratch.database()).await?;
    let backup = db
        .migration_backup()
        .expect("populated predecessor backup")
        .to_path_buf();
    let c = db.connect().await?;
    assert_eq!(
        scalar(&c, "SELECT max(version) FROM schema_migrations").await?,
        46 // Latest adds removed-health46; predecessor44 and injected45 failure remain fixed.
    );
    assert_eq!(witness(&c).await?, before);
    assert_eq!(scalar(&c, "SELECT count(*) FROM series WHERE network IS NULL AND original_country IS NULL AND status IS NULL AND genres_json IS NULL").await?, 1);
    assert_eq!(scalar(&c, "SELECT count(*) FROM movie_metadata WHERE studio IS NULL AND genres_json IS NULL AND keywords_json IS NULL").await?, 3);
    assert_eq!(
        scalar(
            &c,
            "SELECT sum(autotag_metadata_version) FROM snapshot_imports"
        )
        .await?,
        0
    );
    c.execute("UPDATE movie_metadata SET runtime=0 WHERE id=1", ())
        .await?;
    integrity(&c).await?;
    let saved = witness(&c).await?;
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(backup.join("manifest.json"))?)?;
    assert_eq!(manifest["schema_version"], 44);
    let bytes = std::fs::read(backup.join("database.db"))?;
    let hash: String = ring::digest::digest(&ring::digest::SHA256, &bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(manifest["sha256"], hash);
    let copy = libsql::Builder::new_local(backup.join("database.db"))
        .build()
        .await?;
    let bc = raw_connection(&copy).await?;
    assert_eq!(witness(&bc).await?, before);
    assert!(
        bc.execute("UPDATE movie_metadata SET runtime=0 WHERE id=1", ())
            .await
            .is_err()
    );
    drop(bc);
    drop(copy);
    drop(c);
    drop(db);
    let reopened = Database::open_local(scratch.database()).await?;
    assert!(reopened.migration_backup().is_none());
    let c = reopened.connect().await?;
    assert_eq!(witness(&c).await?, saved);
    integrity(&c).await?;
    drop(c);
    drop(reopened);
    Ok(())
}

#[tokio::test]
async fn foundation_real_runner_late_failure_restores_schema_and_backup() -> Result<(), Error> {
    let scratch = Scratch::new();
    let old = predecessor(&scratch.database()).await?;
    let c = raw_connection(&old).await?;
    seed(&c).await?;
    c.execute_batch("CREATE TRIGGER proof_fail_history BEFORE INSERT ON schema_migrations WHEN NEW.version=45 BEGIN SELECT RAISE(ABORT,'proof late publication failure'); END;").await?;
    let before = witness(&c).await?;
    let schema = rows(
        &c,
        "SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name",
    )
    .await?;
    let columns = rows(&c, "PRAGMA table_info(movie_metadata)").await?;
    drop(c);
    drop(old);
    let error = match Database::open_local(scratch.database()).await {
        Ok(_) => panic!("late history failure unexpectedly migrated"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("proof late publication failure"), "{error}");
    assert!(
        error.contains("rolled back") && error.contains("recovery backup"),
        "{error}"
    );
    let old = libsql::Builder::new_local(scratch.database())
        .build()
        .await?;
    let c = raw_connection(&old).await?;
    assert_eq!(witness(&c).await?, before);
    assert_eq!(
        rows(
            &c,
            "SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name"
        )
        .await?,
        schema
    );
    assert_eq!(
        rows(&c, "PRAGMA table_info(movie_metadata)").await?,
        columns
    );
    assert!(
        c.execute("UPDATE movie_metadata SET runtime=0 WHERE id=1", ())
            .await
            .is_err()
    );
    integrity(&c).await?;
    let backups: Vec<_> = std::fs::read_dir(&scratch.0)?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".hrrdarr-migration-")
        })
        .collect();
    assert_eq!(backups.len(), 1);
    let copy = libsql::Builder::new_local(backups[0].path().join("database.db"))
        .build()
        .await?;
    let bc = raw_connection(&copy).await?;
    assert_eq!(witness(&bc).await?, before);
    integrity(&bc).await?;
    drop(bc);
    drop(copy);
    // Recovery remains possible after the injected fault is removed; no table repair required.
    c.execute("DROP TRIGGER proof_fail_history", ()).await?;
    drop(c);
    drop(old);
    let db = Database::open_local(scratch.database()).await?;
    let c = db.connect().await?;
    assert_eq!(
        scalar(&c, "SELECT max(version) FROM schema_migrations").await?,
        46 // Latest adds removed-health46; predecessor44 and injected45 failure remain fixed.
    );
    integrity(&c).await?;
    drop(c);
    drop(db);
    Ok(())
}

#[tokio::test]
async fn foundation_fresh_scalar_array_and_activation_constraints() -> Result<(), Error> {
    let scratch = Scratch::new();
    let db = Database::open_local(scratch.database()).await?;
    assert!(db.migration_backup().is_none());
    let c = db.connect().await?;
    c.execute_batch("INSERT INTO series(id,title,path) VALUES(1,'TV','/fictional/tv'); INSERT INTO movie_metadata(id,title,runtime) VALUES(1,'Movie',0); INSERT INTO snapshot_imports(application,fingerprint,schema_version) VALUES('sonarr','fresh',233);").await?;
    for (table, column) in [("series", "network"), ("movie_metadata", "studio")] {
        for value in [" Network ".to_owned(), "é".repeat(512)] {
            c.execute(
                &format!("UPDATE {table} SET {column}=? WHERE id=1"),
                [value],
            )
            .await?;
        }
        for value in [
            "".to_owned(),
            " \t".to_owned(),
            "\u{a0}\u{2003}".to_owned(),
            "a\0b".to_owned(),
            "a\u{85}b".to_owned(),
            "a\u{9f}b".to_owned(),
            "a\u{7f}b".to_owned(),
            "é".repeat(513),
        ] {
            assert!(
                c.execute(
                    &format!("UPDATE {table} SET {column}=? WHERE id=1"),
                    [value.clone()]
                )
                .await
                .is_err(),
                "accepted {table}.{column} {value:?}"
            );
        }
        c.execute(&format!("UPDATE {table} SET {column}=NULL WHERE id=1"), ())
            .await?;
    }
    for value in ["ISL", "USA"] {
        c.execute("UPDATE series SET original_country=? WHERE id=1", [value])
            .await?;
    }
    for value in ["is", "isl", "ÍSL", "ISL ", "123", "US\0"] {
        assert!(
            c.execute("UPDATE series SET original_country=? WHERE id=1", [value])
                .await
                .is_err()
        );
    }
    for value in ["deleted", "continuing", "ended", "upcoming"] {
        c.execute("UPDATE series SET status=? WHERE id=1", [value])
            .await?;
    }
    assert!(
        c.execute("UPDATE series SET status='released' WHERE id=1", ())
            .await
            .is_err()
    );
    for (table, column) in [
        ("series", "genres_json"),
        ("movie_metadata", "genres_json"),
        ("movie_metadata", "keywords_json"),
    ] {
        for value in [
            "[]".to_owned(),
            serde_json::to_string(&vec!["é".repeat(128); 128])?,
        ] {
            c.execute(
                &format!("UPDATE {table} SET {column}=? WHERE id=1"),
                [value],
            )
            .await?;
        }
        let invalid = vec![
            "{".to_owned(),
            "{}".to_owned(),
            "null".to_owned(),
            "[1]".to_owned(),
            "[null]".to_owned(),
            "[true]".to_owned(),
            "[{}]".to_owned(),
            "[[]]".to_owned(),
            serde_json::to_string(&vec!["a"; 129])?,
            serde_json::to_string(&vec!["é".repeat(129)])?,
            serde_json::to_string(&vec![""])?,
            serde_json::to_string(&vec!["\u{a0}\u{2003}"])?,
            serde_json::to_string(&vec!["a\0b"])?,
            serde_json::to_string(&vec!["a\u{80}b"])?,
            serde_json::to_string(&vec!["a\u{9f}b"])?,
            format!("[{}\"a\"]", " ".repeat(65536)),
        ];
        for value in invalid {
            assert!(
                c.execute(
                    &format!("UPDATE {table} SET {column}=? WHERE id=1"),
                    [value.clone()]
                )
                .await
                .is_err(),
                "accepted {table}.{column} {value:?}"
            );
            let insert = if table == "series" {
                format!(
                    "INSERT INTO series(id,title,path,{column}) VALUES(2,'Invalid','/fictional/invalid',?)"
                )
            } else {
                format!("INSERT INTO movie_metadata(id,title,{column}) VALUES(2,'Invalid',?)")
            };
            assert!(
                c.execute(&insert, [value]).await.is_err(),
                "insert bypassed {table}.{column}"
            );
        }
        c.execute(&format!("UPDATE {table} SET {column}=NULL WHERE id=1"), ())
            .await?;
        assert_eq!(
            scalar(
                &c,
                &format!("SELECT {column} IS NULL FROM {table} WHERE id=1")
            )
            .await?,
            1
        );
        c.execute(&format!("UPDATE {table} SET {column}='[]' WHERE id=1"), ())
            .await?;
        assert_eq!(
            scalar(&c, &format!("SELECT {column}='[]' FROM {table} WHERE id=1")).await?,
            1
        );
    }
    for value in ["NULL", "-1", "2", "0.5", "'bad'"] {
        assert!(
            c.execute(
                &format!("UPDATE snapshot_imports SET autotag_metadata_version={value}"),
                ()
            )
            .await
            .is_err()
        );
    }
    c.execute("UPDATE snapshot_imports SET autotag_metadata_version=1", ())
        .await?;
    integrity(&c).await?;
    drop(c);
    drop(db);
    Ok(())
}
