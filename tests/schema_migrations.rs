use hrrdarr::db::{Database, Error, MediaTarget};
use libsql::{Connection, params};
use std::path::{Path, PathBuf};

struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("hrrdarr-schema-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn db(&self) -> PathBuf {
        self.0.join("library.db")
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

async fn scalar(conn: &Connection, sql: &str) -> i64 {
    conn.query(sql, ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}

async fn prototype(path: &Path) -> libsql::Database {
    let db = libsql::Builder::new_local(path).build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.execute_batch(include_str!("../migrations/0001_prototype.sql"))
        .await
        .unwrap();
    conn.execute_batch("INSERT INTO series VALUES (7, 'Series', 2024, '/tv/Series', 'poster');
        INSERT INTO episodes VALUES (11, 7, 0, 1, 'Special', '/tv/Series/special.mkv');
        INSERT INTO episodes VALUES (12, 7, 0, 2, 'Special two', '/tv/Series/special.mkv');
        INSERT INTO episodes VALUES (13, 7, 1, 1, 'Missing', NULL);
        INSERT INTO operations VALUES ('kept', 11, '/download/file', 'copy', '/tv/new', 'preview', 'message');").await.unwrap();
    db
}

#[tokio::test]
async fn fresh_relations_enforce_domain_ownership_and_catalog_membership() -> Result<(), Error> {
    let files = Sandbox::new();
    let db = Database::open_local(files.db()).await?;
    assert!(db.migration_backup().is_none());
    let conn = db.connect().await?;
    conn.execute_batch("INSERT INTO series (id,title,path) VALUES (1,'A','/a'), (2,'B','/b');
        INSERT INTO seasons (series_id,number) VALUES (1,0),(2,1);
        INSERT INTO episode_files VALUES (8,1,'/a/pack.mkv');
        INSERT INTO episodes (id,series_id,season,number,title,episode_file_id) VALUES (1,1,0,1,'One',8),(2,1,0,2,'Two',8);
        INSERT INTO movie_metadata (id,tmdb_id,title,year) VALUES (1,10,'Film',2024),(2,20,'Catalog only',2025);
        INSERT INTO movies (id,metadata_id,path) VALUES (1,1,'/movies/Film');
        INSERT INTO movie_files (id,movie_id,path,edition) VALUES (8,1,'/movies/Film/film.mkv','Extended');
        INSERT INTO movie_collections VALUES (1,30,'Collection');
        INSERT INTO movie_collection_members VALUES (1,1),(1,2);
        INSERT INTO operations VALUES ('tv','episode',1,NULL,'source','copy','dest','preview','');
        INSERT INTO operations VALUES ('film','movie',NULL,1,'source','copy','dest','preview','');").await?;
    assert_eq!(
        scalar(
            &conn,
            "SELECT count(*) FROM episodes WHERE episode_file_id=8"
        )
        .await,
        2
    );
    assert_eq!(scalar(&conn,"SELECT count(*) FROM movie_collection_members c LEFT JOIN movies m ON m.metadata_id=c.metadata_id WHERE m.id IS NULL").await, 1);
    assert_eq!(scalar(&conn, "SELECT count(*) FROM operations").await, 2);
    // Same numbers identify distinct entities, and the payload preserves the discriminator.
    assert_ne!(
        serde_json::to_value(MediaTarget::Episode(1))?,
        serde_json::to_value(MediaTarget::Movie(1))?
    );
    for sql in [
        "INSERT INTO seasons VALUES (999,1,1)",
        "INSERT INTO episodes (id,series_id,season,number,title) VALUES (3,1,1,3,'Wrong season')",
        "INSERT INTO episodes (id,series_id,season,number,title,episode_file_id) VALUES (3,2,1,3,'Wrong series file',8)",
        "INSERT INTO episodes (id,series_id,season,number,title) VALUES (3,1,0,1,'Duplicate number')",
        "INSERT INTO movies (id,metadata_id,path) VALUES (2,1,'/duplicate')",
        "INSERT INTO movies (id,metadata_id,path) VALUES (2,999,'/missing')",
        "INSERT INTO movie_files (movie_id,path) VALUES (999,'/orphan')",
        "INSERT INTO movie_files (movie_id,path) VALUES (1,'/second-edition')",
        "INSERT INTO movie_collection_members VALUES (1,999)",
        "INSERT INTO operations VALUES ('bad','movie',1,NULL,'s','copy','d','preview','')",
        "INSERT INTO operations VALUES ('bad','episode',1,1,'s','copy','d','preview','')",
        "INSERT INTO operations VALUES ('bad','movie',NULL,2,'s','copy','d','preview','')",
        "INSERT INTO operations VALUES ('bad','episode',999,NULL,'s','copy','d','preview','')",
        "DELETE FROM episode_files WHERE id=8",
        "DELETE FROM movie_metadata WHERE id=1",
    ] {
        assert!(
            conn.execute(sql, ()).await.is_err(),
            "unexpectedly accepted {sql}"
        );
    }
    let second = db.connect().await?;
    assert_eq!(scalar(&second, "PRAGMA foreign_keys").await, 1);
    assert!(
        second
            .execute("DELETE FROM series WHERE id=1", ())
            .await
            .is_err()
    );
    assert!(
        Database::open_local(files.db()).await.is_err(),
        "second worker must not own the same local DB"
    );
    drop(second);
    drop(conn);
    drop(db);
    let reopened = Database::open_local(files.db()).await?;
    let conn = reopened.connect().await?;
    assert_eq!(
        scalar(&conn, "SELECT count(*) FROM movie_metadata").await,
        2
    );
    assert_eq!(scalar(&conn, "SELECT count(*) FROM movies").await, 1);
    assert_eq!(
        scalar(&conn, "SELECT count(*) FROM movie_files WHERE movie_id=1").await,
        1
    );
    assert_eq!(
        scalar(&conn, "SELECT count(*) FROM movie_collection_members").await,
        2
    );
    assert_eq!(
        scalar(
            &conn,
            "SELECT count(*) FROM operations WHERE media_type='movie' AND movie_id=1"
        )
        .await,
        1
    );
    Ok(())
}

#[tokio::test]
async fn prototype_upgrade_preserves_data_backups_restore_and_rerun_is_noop() -> Result<(), Error> {
    let files = Sandbox::new();
    drop(prototype(&files.db()).await);
    let db = Database::open_local(files.db()).await?;
    let backup = db.migration_backup().unwrap().to_path_buf();
    let conn = db.connect().await?;
    assert_eq!(
        scalar(&conn, "SELECT count(*) FROM schema_migrations").await,
        15 // Latest-schema opens also add scoped remote path mappings.
    );
    assert_eq!(scalar(&conn, "SELECT count(*) FROM episodes").await, 3);
    assert_eq!(scalar(&conn, "SELECT count(*) FROM episode_files").await, 1);
    assert_eq!(scalar(&conn, "SELECT count(*) FROM seasons").await, 2);
    assert_eq!(
        scalar(
            &conn,
            "SELECT count(*) FROM episodes WHERE episode_file_id IS NULL"
        )
        .await,
        1
    );
    let row = conn.query("SELECT s.title,s.year,s.path,s.poster,e.title,f.path,o.source,o.mode,o.destination,o.status,o.message,o.media_type FROM series s JOIN episodes e ON e.series_id=s.id JOIN episode_files f ON f.id=e.episode_file_id JOIN operations o ON o.episode_id=e.id WHERE e.id=11",()).await?.next().await?.unwrap();
    assert_eq!(row.get::<String>(0)?, "Series");
    assert_eq!(row.get::<i64>(1)?, 2024);
    for (column, expected) in [
        (2, "/tv/Series"),
        (3, "poster"),
        (4, "Special"),
        (5, "/tv/Series/special.mkv"),
        (6, "/download/file"),
        (7, "copy"),
        (8, "/tv/new"),
        (9, "preview"),
        (10, "message"),
        (11, "episode"),
    ] {
        assert_eq!(row.get::<String>(column)?, expected);
    }
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(backup.join("manifest.json"))?)?;
    assert_eq!(manifest["schema_version"], 0);
    assert_eq!(manifest["backend"], "local-libsql");
    let bytes = std::fs::read(backup.join("database.db"))?;
    let digest: String = ring::digest::digest(&ring::digest::SHA256, &bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(manifest["sha256"], digest);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&backup)?.permissions().mode() & 0o777,
            0o700
        );
    }
    let restored = files.0.join("restored.db");
    std::fs::copy(backup.join("database.db"), &restored)?;
    let raw = libsql::Builder::new_local(&restored).build().await?;
    assert_eq!(
        scalar(
            &raw.connect()?,
            "SELECT count(*) FROM episodes WHERE file_path='/tv/Series/special.mkv'"
        )
        .await,
        2
    );
    drop(raw);
    let recovered = Database::open_local(&restored).await?;
    assert_eq!(
        scalar(&recovered.connect().await?, "SELECT count(*) FROM episodes").await,
        3
    );
    drop(conn);
    drop(db);
    let reopened = Database::open_local(files.db()).await?;
    assert!(reopened.migration_backup().is_none());
    assert_eq!(
        scalar(
            &reopened.connect().await?,
            "SELECT count(*) FROM operations WHERE id='kept'"
        )
        .await,
        1
    );
    Ok(())
}

#[tokio::test]
async fn invalid_legacy_data_fails_atomically_and_can_be_reconciled() -> Result<(), Error> {
    for invalid in [
        "UPDATE episodes SET series_id=999 WHERE id=11",
        "UPDATE episodes SET number=1 WHERE id=12",
        "UPDATE operations SET episode_id=999",
        "UPDATE episodes SET file_path='1234' WHERE id=11",
        "UPDATE episodes SET file_path='0' WHERE id=11",
        "UPDATE episodes SET file_path='' WHERE id=11",
    ] {
        let files = Sandbox::new();
        let raw = prototype(&files.db()).await;
        let conn = raw.connect()?;
        conn.execute(invalid, ()).await?;
        let before = dump(&conn).await?;
        drop(conn);
        drop(raw);
        let result = Database::open_local(files.db()).await;
        assert!(result.is_err(), "accepted invalid legacy state: {invalid}");
        let raw = libsql::Builder::new_local(files.db()).build().await?;
        let conn = raw.connect()?;
        assert_eq!(
            dump(&conn).await?,
            before,
            "rollback changed prototype: {invalid}"
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT count(*) FROM sqlite_schema WHERE name='schema_migrations'"
            )
            .await,
            0
        );
        // Explicit operator reconciliation, then the same migration can be retried safely.
        conn.execute_batch(
            "UPDATE episodes SET series_id=7, file_path='/tv/Series/special.mkv' WHERE id=11;
            UPDATE episodes SET number=2 WHERE id=12; UPDATE operations SET episode_id=11;",
        )
        .await?;
        drop(conn);
        drop(raw);
        let repaired = Database::open_local(files.db()).await?;
        assert_eq!(
            scalar(&repaired.connect().await?, "SELECT count(*) FROM episodes").await,
            3
        );
    }
    Ok(())
}

async fn dump(conn: &Connection) -> Result<Vec<Vec<libsql::Value>>, Error> {
    let mut result = Vec::new();
    for table in ["series", "episodes", "operations"] {
        let mut rows = conn
            .query(&format!("SELECT * FROM {table} ORDER BY id"), ())
            .await?;
        while let Some(row) = rows.next().await? {
            result.push(
                (0..row.column_count())
                    .map(|i| row.get_value(i))
                    .collect::<Result<Vec<_>, _>>()?,
            );
        }
    }
    Ok(result)
}

#[tokio::test]
async fn unknown_or_modified_history_is_rejected_without_new_backup() -> Result<(), Error> {
    // Version 16 remains unknown after migration 15 adds remote path mappings.
    for sql in [
        "UPDATE schema_migrations SET checksum='tampered' WHERE version=1",
        "UPDATE schema_migrations SET sql=sql || '-- changed' WHERE version=1",
        "UPDATE schema_migrations SET name='different' WHERE version=1",
        "DELETE FROM schema_migrations WHERE version=1",
        "DELETE FROM schema_migrations",
        "INSERT INTO schema_migrations (version,name,checksum,sql) VALUES (16,'future','unknown','unknown')",
    ] {
        let files = Sandbox::new();
        let db = Database::open_local(files.db()).await?;
        db.connect().await?.execute(sql, ()).await?;
        drop(db);
        assert!(
            Database::open_local(files.db()).await.is_err(),
            "accepted {sql}"
        );
        assert_eq!(
            std::fs::read_dir(&files.0)?
                .filter_map(Result::ok)
                .filter(|f| f
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".hrrdarr-migration-"))
                .count(),
            0
        );
    }
    Ok(())
}

#[tokio::test]
async fn each_migration_ddl_and_writes_rollback_on_failure() -> Result<(), Error> {
    let db = libsql::Builder::new_local(":memory:").build().await?;
    let conn = db.connect()?;
    conn.execute("PRAGMA foreign_keys=ON", ()).await?;
    let tx = conn.transaction().await?;
    tx.execute_batch(include_str!("../migrations/0001_prototype.sql"))
        .await?;
    tx.execute("INSERT INTO series (id,title,path) VALUES (1,'A','/a')", ())
        .await?;
    assert!(
        tx.execute(
            "INSERT INTO series (id,title,path) VALUES (1,'Duplicate','/b')",
            ()
        )
        .await
        .is_err()
    );
    tx.rollback().await?;
    assert_eq!(scalar(&conn, "SELECT count(*) FROM sqlite_schema").await, 0);
    conn.execute_batch(include_str!("../migrations/0001_prototype.sql"))
        .await?;
    conn.execute("INSERT INTO series (id,title,path) VALUES (1,'A','/a')", ())
        .await?;
    let tx = conn.transaction().await?;
    tx.execute_batch(include_str!("../migrations/0002_media_relations.sql"))
        .await?;
    assert!(
        tx.execute("INSERT INTO seasons VALUES (999,1,1)", ())
            .await
            .is_err()
    );
    tx.rollback().await?;
    assert_eq!(
        scalar(&conn, "SELECT count(*) FROM series WHERE id=1").await,
        1
    );
    assert_eq!(
        scalar(
            &conn,
            "SELECT count(*) FROM sqlite_schema WHERE name='movie_metadata'"
        )
        .await,
        0
    );
    conn.execute(
        "INSERT INTO episodes VALUES (1,1,0,1,'Recovered',?1)",
        params!["/a/file.mkv"],
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn import_journal_upgrade_rollback_domain_history_and_reopen() -> Result<(), Error> {
    let files = Sandbox::new();
    let raw = libsql::Builder::new_local(files.db()).build().await?;
    let c = raw.connect()?;
    c.execute_batch("PRAGMA foreign_keys=ON;CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY,name TEXT NOT NULL,checksum TEXT NOT NULL,sql TEXT NOT NULL,applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);").await?;
    // Build the actual previous schema from its immutable migrations, not an approximation.
    for (index, (name, sql)) in [
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
    ]
    .iter()
    .enumerate()
    {
        c.execute_batch(sql).await?;
        let checksum: String = ring::digest::digest(&ring::digest::SHA256, sql.as_bytes())
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
            params![index as i64 + 1, *name, checksum, *sql],
        )
        .await?;
    }
    c.execute_batch("INSERT INTO series(id,title,path) VALUES(1,'TV','/tv'),(2,'Other','/other');
        INSERT INTO seasons VALUES(1,1,1),(2,1,1);
        INSERT INTO episodes(id,series_id,season,number,title) VALUES(1,1,1,1,'Pilot'),(2,2,1,1,'Other');
        INSERT INTO episode_files VALUES(9,2,'/other/file');
        INSERT INTO movie_metadata(id,title) VALUES(1,'Movie');
        INSERT INTO movies(id,metadata_id,path) VALUES(1,1,'/movie');
        INSERT INTO operations VALUES('tv','episode',1,NULL,'/source/tv','copy','/tv/new','preview','');
        INSERT INTO operations VALUES('movie','movie',NULL,1,'/source/movie','move','/movie/new','preview','');
        INSERT INTO snapshot_imports(application,fingerprint,schema_version) VALUES('sonarr','saved',233);
        INSERT INTO snapshot_mappings VALUES('sonarr','saved','episode_files',7,9);").await?;
    let tx = c.transaction().await?;
    tx.execute_batch(include_str!("../migrations/0008_manual_import_journal.sql"))
        .await?;
    assert!(tx.execute("INSERT INTO import_journal(operation_id,plan_json,phase) VALUES('absent','{}','preview')",()).await.is_err());
    tx.rollback().await?;
    assert_eq!(scalar(&c,"SELECT count(*) FROM sqlite_schema WHERE name IN ('import_journal','import_history','import_operation_immutable')").await,0);
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM operations WHERE status='preview'").await,
        2
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM schema_migrations").await,
        7
    );
    drop(c);
    drop(raw);

    let db = Database::open_local(files.db()).await?;
    assert!(db.permits_local_imports());
    assert!(db.migration_backup().is_some());
    let c = db.connect().await?;
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM schema_migrations").await,
        15 // Latest-schema opens also add scoped remote path mappings.
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM operations WHERE status='preview'").await,
        2
    );
    // Initial Stage JSON may be persisted in the same write that completes staging.
    c.execute_batch("INSERT INTO import_journal(operation_id,plan_json,phase) VALUES('tv','{}','preview'),('movie','{}','staging');
        UPDATE import_journal SET stage_json=json_object('directory',json_object('dev',1,'ino',2),'file',json_object('dev',1,'ino',3),'sha256',printf('%064d',0)),phase='staged' WHERE operation_id='tv';").await?;
    // The delivered predicates compare JSON scalar facts, independent of whitespace/key order.
    let reordered = format!(
        r#"{{
        "sha256": "{}", "file": {{ "ino": 3, "dev": 1 }},
        "directory": {{ "ino": 2, "dev": 1 }}
    }}"#,
        "0".repeat(64)
    );
    c.execute(
        "UPDATE import_journal SET stage_json=? WHERE operation_id='tv'",
        params![reordered],
    )
    .await?;
    assert!(c.execute("UPDATE import_journal SET stage_json=json_set(stage_json,'$.file.ino',4) WHERE operation_id='tv'",()).await.is_err());
    for sql in [
        "UPDATE import_journal SET plan_json='{\"changed\":true}' WHERE operation_id='tv'",
        "UPDATE import_journal SET version=2 WHERE operation_id='tv'",
        "UPDATE import_journal SET phase='staging' WHERE operation_id='tv'",
        "UPDATE import_journal SET stage_json='{\"file\":{\"ino\":123}}' WHERE operation_id='tv'",
        "UPDATE import_journal SET stage_json='{\"source_retired\":true}' WHERE operation_id='tv'",
        "UPDATE import_journal SET phase='complete' WHERE operation_id='tv'",
        "UPDATE import_journal SET phase='published' WHERE operation_id='movie'",
        "UPDATE import_journal SET plan_json='[]' WHERE operation_id='tv'",
        "UPDATE import_journal SET stage_json='[]' WHERE operation_id='tv'",
        "UPDATE import_journal SET stage_json=json_object('large',printf('%05000d',1)) WHERE operation_id='tv'",
        "UPDATE operations SET source='/changed' WHERE id='tv'",
        "UPDATE operations SET episode_id=2 WHERE id='tv'",
        "UPDATE operations SET media_type='movie',episode_id=NULL,movie_id=1 WHERE id='tv'",
        "DELETE FROM operations WHERE id='tv'",
    ] {
        assert!(c.execute(sql, ()).await.is_err(), "accepted {sql}");
    }

    // Late failure must roll back file creation, associations, history and phase together.
    let tx = c.transaction().await?;
    tx.execute_batch("INSERT INTO episode_files VALUES(7,1,'/tv/new'); UPDATE episodes SET episode_file_id=7 WHERE id=1;
        INSERT INTO import_history(operation_id,media_type,episode_id,episode_file_id,source,destination,size,sha256)
        VALUES('tv','episode',1,7,'/source/tv','/tv/new',12,printf('%064d',0));
        UPDATE import_journal SET phase='committed' WHERE operation_id='tv';").await?;
    assert!(
        tx.execute("INSERT INTO episode_files VALUES(10,999,'/invalid')", ())
            .await
            .is_err()
    );
    tx.rollback().await?;
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM episode_files WHERE id=7").await,
        0
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM episodes WHERE id=1 AND episode_file_id IS NULL"
        )
        .await,
        1
    );
    assert_eq!(scalar(&c, "SELECT count(*) FROM import_history").await, 0);
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM import_journal WHERE operation_id='tv' AND phase='staged'"
        )
        .await,
        1
    );

    c.execute_batch("INSERT INTO episode_files VALUES(7,1,'/tv/new'); UPDATE episodes SET episode_file_id=7 WHERE id=1;
        INSERT INTO movie_files(id,movie_id,path) VALUES(7,1,'/movie/new');
        UPDATE import_journal SET stage_json='{}',phase='staged' WHERE operation_id='movie';").await?;
    for sql in [
        "INSERT INTO import_history(operation_id,media_type,episode_id,episode_file_id,source,destination,size,sha256) VALUES('tv','episode',1,9,'/source/tv','/tv/new',12,printf('%064d',0))",
        "INSERT INTO import_history(operation_id,media_type,movie_id,movie_file_id,source,destination,size,sha256) VALUES('tv','movie',1,7,'/source/tv','/movie/new',12,printf('%064d',0))",
        "INSERT INTO import_history(operation_id,media_type,episode_id,episode_file_id,source,destination,size,sha256) VALUES('tv','episode',1,7,'/wrong','/tv/new',12,printf('%064d',0))",
        "INSERT INTO import_history(operation_id,media_type,episode_id,episode_file_id,source,destination,size,sha256) VALUES('tv','episode',1,7,'/source/tv','/tv/new',-1,printf('%064d',0))",
    ] {
        assert!(c.execute(sql, ()).await.is_err(), "accepted {sql}");
    }
    let tx = c.transaction().await?;
    tx.execute_batch("INSERT INTO import_history(operation_id,media_type,episode_id,episode_file_id,source,destination,size,sha256) VALUES('tv','episode',1,7,'/source/tv','/tv/new',12,printf('%064d',0));
        INSERT INTO import_history(operation_id,media_type,movie_id,movie_file_id,source,destination,size,sha256) VALUES('movie','movie',1,7,'/source/movie','/movie/new',34,printf('%064d',1));
        UPDATE import_journal SET phase='committed';
        UPDATE import_journal SET stage_json='{\"quarantine\":{\"dev\":1,\"ino\":2},\"source_retired\":false}' WHERE operation_id='movie';
        UPDATE import_journal SET stage_json=json_set(stage_json,'$.source_retired',json('true')) WHERE operation_id='movie';
        UPDATE import_journal SET phase='complete'; UPDATE operations SET status='complete';").await?;
    tx.commit().await?;
    assert!(
        c.execute(
            "UPDATE import_history SET size=99 WHERE operation_id='tv'",
            ()
        )
        .await
        .is_err()
    );
    assert!(
        c.execute("DELETE FROM import_journal WHERE operation_id='tv'", ())
            .await
            .is_err()
    );
    assert!(
        c.execute("DELETE FROM import_history WHERE operation_id='tv'", ())
            .await
            .is_err()
    );
    // Retiring a live file later does not erase/reinterpret historical IDs or snapshot identities.
    c.execute_batch(
        "UPDATE episodes SET episode_file_id=NULL WHERE id=1; DELETE FROM episode_files WHERE id=7;
        DELETE FROM movie_files WHERE id=7;",
    )
    .await?;
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM import_history WHERE episode_file_id=7 OR movie_file_id=7"
        )
        .await,
        2
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT destination_id FROM snapshot_mappings WHERE fingerprint='saved'"
        )
        .await,
        9
    );
    drop(c);
    drop(db);
    let db = Database::open_local(files.db()).await?;
    assert!(db.migration_backup().is_none());
    assert_eq!(
        scalar(&db.connect().await?, "SELECT count(*) FROM import_history").await,
        2
    );
    Ok(())
}

#[tokio::test]
async fn provider_configuration_upgrade_constraints_and_atomic_replacement() -> Result<(), Error> {
    let files = Sandbox::new();
    let raw = libsql::Builder::new_local(files.db()).build().await?;
    let c = raw.connect()?;
    c.execute_batch("PRAGMA foreign_keys=ON;CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY,name TEXT NOT NULL,checksum TEXT NOT NULL,sql TEXT NOT NULL,applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);").await?;
    for (index, (name, sql)) in [
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
    ]
    .iter()
    .enumerate()
    {
        c.execute_batch(sql).await?;
        let checksum: String = ring::digest::digest(&ring::digest::SHA256, sql.as_bytes())
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
            params![index as i64 + 1, *name, checksum, *sql],
        )
        .await?;
    }
    c.execute_batch("INSERT INTO series(id,title,path) VALUES(1,'TV','/tv');
        INSERT INTO seasons VALUES(1,1,1); INSERT INTO episode_files VALUES(7,1,'/tv/file');
        INSERT INTO episodes(id,series_id,season,number,title,episode_file_id) VALUES(1,1,1,1,'Episode',7);
        INSERT INTO operations VALUES('kept','episode',1,NULL,'/source','copy','/tv/file','complete','');
        INSERT INTO import_journal(operation_id,plan_json,stage_json,phase) VALUES('kept','{}','{}','staging');
        INSERT INTO import_history(operation_id,media_type,episode_id,episode_file_id,source,destination,size,sha256) VALUES('kept','episode',1,7,'/source','/tv/file',10,printf('%064d',0));
        UPDATE import_journal SET phase='complete' WHERE operation_id='kept';").await?;
    let tx = c.transaction().await?;
    tx.execute_batch(include_str!(
        "../migrations/0009_provider_configuration.sql"
    ))
    .await?;
    assert!(tx.execute("INSERT INTO providers VALUES('bad','unused-family','Bad',1,1,1,1,'https://example.test',NULL)",()).await.is_err());
    tx.rollback().await?;
    assert_eq!(scalar(&c,"SELECT count(*) FROM sqlite_schema WHERE name IN ('providers','provider_scopes','provider_revision_step')").await,0);
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM import_history WHERE operation_id='kept'"
        )
        .await,
        1
    );
    drop(c);
    drop(raw);
    let db = Database::open_local(files.db()).await?;
    assert!(db.migration_backup().is_some());
    let c = db.connect().await?;
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM schema_migrations").await,
        15 // Latest-schema opens also add scoped remote path mappings.
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM import_journal WHERE operation_id='kept' AND phase='complete'"
        )
        .await,
        1
    );
    assert_eq!(
        scalar(&c, "SELECT episode_file_id FROM episodes WHERE id=1").await,
        7
    );
    // Latest-schema writes supply applicable false options; old-schema upgrade fixtures retain their old shape.
    // These envelopes test SQL byte/type bounds only; authenticated encryption belongs to the service tests.
    c.execute_batch("INSERT INTO providers VALUES('00000000-0000-0000-0000-000000000001','torznab','Indexer',1,1,1,1,'https://indexer.test',zeroblob(29));
        INSERT INTO providers VALUES('00000000-0000-0000-0000-000000000002','qbittorrent','Client',1,50,1,1,'https://client.test',NULL);
        INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year) VALUES
        ('00000000-0000-0000-0000-000000000001','torznab','tv','[5000,5030]','[5070]',0,NULL),
        ('00000000-0000-0000-0000-000000000001','torznab','movies','[2000]','[]',NULL,0);
        INSERT INTO provider_scopes(provider_id,implementation,media_type,category,imported_category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags) VALUES
        ('00000000-0000-0000-0000-000000000002','qbittorrent','tv','tv','tv-imported',1,0,'started','default',0,0,0),
        ('00000000-0000-0000-0000-000000000002','qbittorrent','movies','movies',NULL,0,0,'started','default',0,0,0);").await?;
    for sql in [
        "UPDATE providers SET name='Unversioned' WHERE implementation='torznab'",
        "UPDATE providers SET revision=revision+2 WHERE implementation='torznab'",
        "UPDATE providers SET revision=revision+1,implementation='newznab' WHERE implementation='torznab'",
        "UPDATE providers SET revision=revision+1,credentials='plaintext' WHERE implementation='torznab'",
        "UPDATE providers SET revision=revision+1,credentials=zeroblob(28) WHERE implementation='torznab'",
        "UPDATE providers SET revision=revision+1,credentials=zeroblob(16385) WHERE implementation='torznab'",
        "UPDATE providers SET revision=revision+1,priority=101 WHERE implementation='torznab'",
        "UPDATE providers SET revision=revision+1,settings_version=2 WHERE implementation='torznab'",
        "UPDATE provider_scopes SET implementation='newznab' WHERE implementation='torznab'",
        "UPDATE provider_scopes SET categories='[1,1]' WHERE implementation='torznab'",
        "UPDATE provider_scopes SET categories='[1.5]' WHERE implementation='torznab'",
        "UPDATE provider_scopes SET categories='[\"1\"]' WHERE implementation='torznab'",
        "UPDATE provider_scopes SET categories='[0]' WHERE implementation='torznab'",
        // Pinned Newznab Category.Id is a signed 32-bit integer, not a JS-sized integer.
        "UPDATE provider_scopes SET categories='[2147483648]' WHERE implementation='torznab'",
        "UPDATE provider_scopes SET categories='[null]' WHERE implementation='torznab'",
        "UPDATE provider_scopes SET categories='{}' WHERE implementation='torznab'",
        "UPDATE provider_scopes SET anime_categories='[5070]' WHERE media_type='movies' AND implementation='torznab'",
        "UPDATE provider_scopes SET category='tv' WHERE media_type='movies' AND implementation='qbittorrent'",
        // Pinned qBittorrent priority is the closed enum Last=0 / First=1.
        "UPDATE provider_scopes SET recent_priority=2 WHERE implementation='qbittorrent'",
        "UPDATE provider_scopes SET older_priority=-1 WHERE implementation='qbittorrent'",
        "UPDATE provider_scopes SET categories='[5000]' WHERE implementation='qbittorrent'",
    ] {
        assert!(c.execute(sql, ()).await.is_err(), "accepted {sql}");
    }
    // The positive signed-32-bit boundary is accepted, then restored for the rollback fixture.
    c.execute("UPDATE provider_scopes SET categories='[2147483647]' WHERE implementation='torznab' AND media_type='tv'",()).await?;
    // Structural category validation accepts formatting changes while preserving real values.
    c.execute("UPDATE provider_scopes SET categories='[ 5000 , 5030 ]' WHERE implementation='torznab' AND media_type='tv'",()).await?;
    let tx = c.transaction().await?;
    tx.execute("UPDATE providers SET revision=2,name='Changed',credentials=zeroblob(64) WHERE implementation='torznab' AND revision=1",()).await?;
    tx.execute(
        "DELETE FROM provider_scopes WHERE implementation='torznab'",
        (),
    )
    .await?;
    tx.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year) VALUES('00000000-0000-0000-0000-000000000001','torznab','tv','[5000]','[]',0,NULL)",()).await?;
    assert!(tx.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year) VALUES('00000000-0000-0000-0000-000000000001','torznab','movies','[]','[]',NULL,0)",()).await.is_err());
    tx.rollback().await?;
    assert_eq!(
        scalar(
            &c,
            "SELECT revision FROM providers WHERE implementation='torznab'"
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT length(credentials) FROM providers WHERE implementation='torznab'"
        )
        .await,
        29
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM providers WHERE name='Indexer'").await,
        1
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM provider_scopes WHERE implementation='torznab'"
        )
        .await,
        2
    );
    assert_eq!(scalar(&c,"SELECT json_array_length(categories) FROM provider_scopes WHERE implementation='torznab' AND media_type='tv'").await,2);
    c.execute("UPDATE providers SET revision=2,credentials=NULL WHERE implementation='torznab' AND revision=1",()).await?;
    assert_eq!(c.execute("UPDATE providers SET revision=2,name='Stale' WHERE implementation='torznab' AND revision=1",()).await?,0);
    c.execute(
        "DELETE FROM providers WHERE implementation='qbittorrent'",
        (),
    )
    .await?;
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM provider_scopes WHERE implementation='qbittorrent'"
        )
        .await,
        0
    );
    drop(c);
    drop(db);
    let db = Database::open_local(files.db()).await?;
    assert!(db.migration_backup().is_none());
    let c = db.connect().await?;
    assert_eq!(
        scalar(
            &c,
            "SELECT revision FROM providers WHERE implementation='torznab' AND credentials IS NULL"
        )
        .await,
        2
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM import_history WHERE operation_id='kept'"
        )
        .await,
        1
    );
    Ok(())
}

#[tokio::test]
async fn provider_test_results_upgrade_revision_invalidation_and_reopen() -> Result<(), Error> {
    let files = Sandbox::new();
    let raw = libsql::Builder::new_local(files.db()).build().await?;
    let c = raw.connect()?;
    c.execute_batch("PRAGMA foreign_keys=ON;CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY,name TEXT NOT NULL,checksum TEXT NOT NULL,sql TEXT NOT NULL,applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);").await?;
    for (index, (name, sql)) in [
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
    ]
    .iter()
    .enumerate()
    {
        c.execute_batch(sql).await?;
        let checksum: String = ring::digest::digest(&ring::digest::SHA256, sql.as_bytes())
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
            params![index as i64 + 1, *name, checksum, *sql],
        )
        .await?;
    }
    c.execute_batch("INSERT INTO providers VALUES('00000000-0000-0000-0000-000000000001','torznab','Kept',1,1,1,1,'https://example.test/api',zeroblob(29));
        INSERT INTO providers VALUES('00000000-0000-0000-0000-000000000002','qbittorrent','Untested client',1,1,1,1,'https://example.test/client',NULL);
        INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories) VALUES('00000000-0000-0000-0000-000000000001','torznab','tv','[5000]','[]');
        INSERT INTO series(id,title,path) VALUES(1,'Kept TV','/tv');").await?;
    let tx = c.transaction().await?;
    tx.execute_batch(include_str!("../migrations/0010_provider_test_results.sql"))
        .await?;
    assert!(
        tx.execute(
            "INSERT INTO provider_tests VALUES('absent',1,1,'success',NULL)",
            ()
        )
        .await
        .is_err()
    );
    tx.rollback().await?;
    assert_eq!(scalar(&c,"SELECT count(*) FROM sqlite_schema WHERE name IN ('provider_tests','provider_test_insert_owner','provider_configuration_invalidates_test')").await,0);
    assert_eq!(scalar(&c,"SELECT count(*) FROM providers WHERE name='Kept' AND revision=1 AND length(credentials)=29").await,1);
    drop(c);
    drop(raw);
    let db = Database::open_local(files.db()).await?;
    assert!(db.migration_backup().is_some());
    let c = db.connect().await?;
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM schema_migrations").await,
        15 // Latest-schema opens also add scoped remote path mappings.
    );
    assert_eq!(scalar(&c, "SELECT count(*) FROM provider_scopes").await, 1);
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM series WHERE title='Kept TV'").await,
        1
    );
    for sql in [
        "INSERT INTO provider_tests VALUES('00000000-0000-0000-0000-000000000001',2,1,'success',NULL)",
        "INSERT INTO provider_tests VALUES('00000000-0000-0000-0000-000000000099',1,1,'success',NULL)", // qBittorrent is now supported; nonexistent ownership remains invalid.
        "INSERT INTO provider_tests VALUES('00000000-0000-0000-0000-000000000001',1,-1,'success',NULL)",
        "INSERT INTO provider_tests VALUES('00000000-0000-0000-0000-000000000001',1,1.5,'success',NULL)",
        "INSERT INTO provider_tests VALUES('00000000-0000-0000-0000-000000000001',1,1,'success','timeout')",
        "INSERT INTO provider_tests VALUES('00000000-0000-0000-0000-000000000001',1,1,'failure',NULL)",
        "INSERT INTO provider_tests VALUES('00000000-0000-0000-0000-000000000001',1,1,'failure','SENTINEL_UPSTREAM_SECRET')",
    ] {
        assert!(c.execute(sql, ()).await.is_err(), "accepted {sql}");
    }
    c.execute("INSERT INTO provider_tests VALUES('00000000-0000-0000-0000-000000000001',1,1720000000,'success',NULL)",()).await?;
    drop(c);
    drop(db);
    let db = Database::open_local(files.db()).await?;
    assert!(db.migration_backup().is_none());
    let c = db.connect().await?;
    assert_eq!(
        scalar(
            &c,
            "SELECT tested_at FROM provider_tests WHERE status='success'"
        )
        .await,
        1720000000
    );
    assert!(
        c.execute("UPDATE provider_tests SET config_revision=2", ())
            .await
            .is_err()
    );
    assert!(
        c.execute(
            "UPDATE provider_tests SET provider_id='00000000-0000-0000-0000-000000000002'",
            ()
        )
        .await
        .is_err()
    );
    // An aborted config transaction restores both the old revision and its valid observation.
    let tx = c.transaction().await?;
    tx.execute(
        "UPDATE providers SET revision=2,name='Uncommitted' WHERE implementation='torznab'",
        (),
    )
    .await?;
    assert_eq!(scalar(&tx, "SELECT count(*) FROM provider_tests").await, 0);
    assert!(
        tx.execute(
            "UPDATE providers SET revision=3,priority=101 WHERE implementation='torznab'",
            ()
        )
        .await
        .is_err()
    );
    tx.rollback().await?;
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM provider_tests WHERE config_revision=1 AND status='success'"
        )
        .await,
        1
    );
    c.execute(
        "UPDATE providers SET revision=2,name='Changed' WHERE implementation='torznab'",
        (),
    )
    .await?;
    assert_eq!(scalar(&c, "SELECT count(*) FROM provider_tests").await, 0);
    // This is the service's post-network CAS pattern: a stale completion writes zero rows.
    let cas = "INSERT INTO provider_tests(provider_id,config_revision,tested_at,status,error_code) SELECT id,revision,1720000001,'failure','timeout' FROM providers WHERE id=? AND revision=? ON CONFLICT(provider_id) DO UPDATE SET config_revision=excluded.config_revision,tested_at=excluded.tested_at,status=excluded.status,error_code=excluded.error_code";
    assert_eq!(
        c.execute(cas, params!["00000000-0000-0000-0000-000000000001", 1])
            .await?,
        0
    );
    assert_eq!(scalar(&c, "SELECT count(*) FROM provider_tests").await, 0);
    assert_eq!(
        c.execute(cas, params!["00000000-0000-0000-0000-000000000001", 2])
            .await?,
        1
    );
    assert_eq!(
        c.execute(cas, params!["00000000-0000-0000-0000-000000000001", 2])
            .await?,
        1
    );
    assert_eq!(scalar(&c,"SELECT count(*) FROM provider_tests WHERE config_revision=2 AND status='failure' AND error_code='timeout'").await,1);
    assert!(
        c.execute("UPDATE provider_tests SET config_revision=1", ())
            .await
            .is_err()
    );
    // Scope maintenance invalidates existing observations too; runtime replacements also bump revision.
    c.execute(
        "UPDATE provider_scopes SET categories='[5030]' WHERE implementation='torznab'",
        (),
    )
    .await?;
    assert_eq!(scalar(&c, "SELECT count(*) FROM provider_tests").await, 0);
    c.execute(cas, params!["00000000-0000-0000-0000-000000000001", 2])
        .await?;
    c.execute("DELETE FROM providers WHERE implementation='torznab'", ())
        .await?;
    assert_eq!(scalar(&c, "SELECT count(*) FROM provider_tests").await, 0);
    assert_eq!(scalar(&c, "SELECT count(*) FROM provider_scopes").await, 0);
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM providers WHERE implementation='qbittorrent'"
        )
        .await,
        1
    );
    Ok(())
}

#[tokio::test]
async fn indexer_scope_options_upgrade_rollback_constraints_and_reopen() -> Result<(), Error> {
    let files = Sandbox::new();
    let raw = libsql::Builder::new_local(files.db()).build().await?;
    let c = raw.connect()?;
    c.execute_batch("PRAGMA foreign_keys=ON;CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY,name TEXT NOT NULL,checksum TEXT NOT NULL,sql TEXT NOT NULL,applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);").await?;
    for (index, (name, sql)) in [
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
    ]
    .iter()
    .enumerate()
    {
        c.execute_batch(sql).await?;
        let checksum: String = ring::digest::digest(&ring::digest::SHA256, sql.as_bytes())
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
            params![index as i64 + 1, *name, checksum, *sql],
        )
        .await?;
    }

    // Actual schema 10 has no option columns. Preserve both protocols/domains and client scopes.
    for (id, implementation) in [(1, "torznab"), (2, "newznab"), (3, "qbittorrent")] {
        let id = format!("00000000-0000-0000-0000-{id:012}");
        c.execute(
            "INSERT INTO providers VALUES(?,?,?,1,1,1,1,'https://example.test/api',zeroblob(29))",
            params![id.clone(), implementation, implementation],
        )
        .await?;
        for domain in ["tv", "movies"] {
            if implementation == "qbittorrent" {
                c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority) VALUES(?,?,?,?,0,0)",params![id.clone(),implementation,domain,domain]).await?;
            } else {
                c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories) VALUES(?,?,?,'[2000]','[]')",params![id.clone(),implementation,domain]).await?;
            }
        }
        if implementation != "qbittorrent" {
            c.execute(
                "INSERT INTO provider_tests VALUES(?,1,1720000000,'success',NULL)",
                [id],
            )
            .await?;
        }
    }
    let tx = c.transaction().await?;
    tx.execute_batch(include_str!("../migrations/0011_indexer_scope_options.sql"))
        .await?;
    assert!(
        tx.execute(
            "UPDATE provider_scopes SET remove_year=2 WHERE media_type='movies'",
            ()
        )
        .await
        .is_err()
    );
    tx.rollback().await?;
    assert_eq!(scalar(&c,"SELECT count(*) FROM pragma_table_info('provider_scopes') WHERE name IN ('remove_year','anime_standard_format_search')").await,0);
    assert_eq!(scalar(&c, "SELECT count(*) FROM provider_tests").await, 2);
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM schema_migrations").await,
        10
    );
    assert_eq!(scalar(&c,"SELECT count(*) FROM sqlite_schema WHERE type='trigger' AND name='provider_scope_update_invalidates_test'").await,1);
    // Rollback restores the old trigger's behavior, not merely its name.
    let tx = c.transaction().await?;
    tx.execute(
        "UPDATE provider_scopes SET categories='[2001]' WHERE implementation='torznab'",
        (),
    )
    .await?;
    assert_eq!(scalar(&tx, "SELECT count(*) FROM provider_tests").await, 1);
    tx.rollback().await?;
    drop(c);
    drop(raw);
    let db = Database::open_local(files.db()).await?;
    assert!(db.migration_backup().is_some());
    let c = db.connect().await?;
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM schema_migrations").await,
        15 // Latest-schema opens also add scoped remote path mappings.
    );
    assert_eq!(scalar(&c,"SELECT count(*) FROM providers WHERE revision=1 AND credentials=zeroblob(29) AND endpoint='https://example.test/api' AND name=implementation").await,3);
    assert_eq!(scalar(&c, "SELECT count(*) FROM provider_scopes").await, 6);
    assert_eq!(scalar(&c,"SELECT count(*) FROM provider_scopes WHERE implementation IN ('torznab','newznab') AND categories='[2000]' AND anime_categories='[]' AND ((media_type='tv' AND anime_standard_format_search=0 AND remove_year IS NULL) OR (media_type='movies' AND remove_year=0 AND anime_standard_format_search IS NULL))").await,4);
    assert_eq!(scalar(&c,"SELECT count(*) FROM provider_scopes WHERE implementation='qbittorrent' AND category=media_type AND anime_standard_format_search IS NULL AND remove_year IS NULL").await,2);
    assert_eq!(scalar(&c,"SELECT count(*) FROM provider_tests WHERE config_revision=1 AND tested_at=1720000000 AND status='success' AND error_code IS NULL").await,2);
    for (column, domain) in [
        ("anime_standard_format_search", "tv"),
        ("remove_year", "movies"),
    ] {
        for value in ["NULL", "2", "-1", "0.5", "'true'", "x'00'"] {
            let sql = format!(
                "UPDATE provider_scopes SET {column}={value} WHERE implementation='torznab' AND media_type='{domain}'"
            );
            assert!(c.execute(&sql, ()).await.is_err(), "accepted {sql}");
            let (anime, year) = if domain == "tv" {
                (value, "NULL")
            } else {
                ("NULL", value)
            };
            let sql = format!(
                "INSERT OR REPLACE INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year) VALUES('00000000-0000-0000-0000-000000000001','torznab','{domain}','[2000]','[]',{anime},{year})"
            );
            assert!(c.execute(&sql, ()).await.is_err(), "accepted {sql}");
        }
        let opposite = if domain == "tv" { "movies" } else { "tv" };
        assert!(
            c.execute(
                &format!("UPDATE provider_scopes SET {column}=0 WHERE media_type='{opposite}'"),
                ()
            )
            .await
            .is_err()
        );
        assert!(
            c.execute(
                &format!(
                    "UPDATE provider_scopes SET {column}=0 WHERE implementation='qbittorrent'"
                ),
                ()
            )
            .await
            .is_err()
        );
    }
    for sql in [
        "INSERT OR REPLACE INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year) VALUES('00000000-0000-0000-0000-000000000001','torznab','tv','[2000]','[]',0,0)",
        "INSERT OR REPLACE INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year) VALUES('00000000-0000-0000-0000-000000000001','torznab','movies','[2000]','[]',0,0)",
        "INSERT OR REPLACE INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,anime_standard_format_search) VALUES('00000000-0000-0000-0000-000000000003','qbittorrent','tv','tv',0,0,0)",
        "INSERT OR REPLACE INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,remove_year) VALUES('00000000-0000-0000-0000-000000000003','qbittorrent','movies','movies',0,0,0)",
    ] {
        assert!(c.execute(sql, ()).await.is_err(), "accepted {sql}");
    }
    assert_eq!(scalar(&c, "SELECT count(*) FROM provider_tests").await, 2);
    c.execute("UPDATE provider_scopes SET anime_standard_format_search=1 WHERE implementation='torznab' AND media_type='tv'",()).await?;
    c.execute("UPDATE provider_scopes SET remove_year=1 WHERE implementation='newznab' AND media_type='movies'",()).await?;
    assert_eq!(scalar(&c, "SELECT count(*) FROM provider_tests").await, 0);
    drop(c);
    drop(db);
    let db = Database::open_local(files.db()).await?;
    assert!(db.migration_backup().is_none());
    let c = db.connect().await?;
    assert_eq!(scalar(&c,"SELECT count(*) FROM provider_scopes WHERE anime_standard_format_search=1 OR remove_year=1").await,2);
    assert_eq!(scalar(&c, "SELECT count(*) FROM provider_tests").await, 0);
    Ok(())
}

#[tokio::test]
async fn qbittorrent_options_upgrade_ownership_rollback_and_reopen() -> Result<(), Error> {
    let files = Sandbox::new();
    let raw = libsql::Builder::new_local(files.db()).build().await?;
    let c = raw.connect()?;
    c.execute_batch("PRAGMA foreign_keys=ON;CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY,name TEXT NOT NULL,checksum TEXT NOT NULL,sql TEXT NOT NULL,applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);").await?;
    for (index, (name, sql)) in [
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
    ]
    .iter()
    .enumerate()
    {
        c.execute_batch(sql).await?;
        let checksum: String = ring::digest::digest(&ring::digest::SHA256, sql.as_bytes())
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
            params![index as i64 + 1, *name, checksum, *sql],
        )
        .await?;
    }

    // Actual schema 10 has no option columns. Preserve both protocols/domains and client scopes.
    for (id, implementation) in [(1, "torznab"), (2, "newznab"), (3, "qbittorrent")] {
        let id = format!("00000000-0000-0000-0000-{id:012}");
        c.execute(
            "INSERT INTO providers VALUES(?,?,?,1,1,1,1,'https://example.test/api',zeroblob(29))",
            params![id.clone(), implementation, implementation],
        )
        .await?;
        for domain in ["tv", "movies"] {
            if implementation == "qbittorrent" {
                c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority) VALUES(?,?,?,?,0,0)",params![id.clone(),implementation,domain,domain]).await?;
            } else {
                c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories) VALUES(?,?,?,'[2000]','[]')",params![id.clone(),implementation,domain]).await?;
            }
        }
        if implementation != "qbittorrent" {
            c.execute(
                "INSERT INTO provider_tests VALUES(?,1,1720000000,'success',NULL)",
                [id],
            )
            .await?;
        }
    }

    let sql = include_str!("../migrations/0011_indexer_scope_options.sql");
    c.execute_batch(sql).await?;
    let checksum: String = ring::digest::digest(&ring::digest::SHA256, sql.as_bytes())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    c.execute("INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(11,'indexer_scope_options',?,?)",params![checksum,sql]).await?;
    // Schema 11 permits cross-domain imported-category collisions; upgrade must reject intact.
    c.execute("UPDATE provider_scopes SET imported_category='tv/child' WHERE implementation='qbittorrent' AND media_type='movies'",()).await?;
    let tx = c.transaction().await?;
    assert!(
        tx.execute_batch(include_str!("../migrations/0012_qbittorrent_options.sql"))
            .await
            .is_err()
    );
    tx.rollback().await?;
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM pragma_table_info('provider_scopes') WHERE name='initial_state'"
        )
        .await,
        0
    );
    assert_eq!(scalar(&c, "SELECT count(*) FROM provider_tests").await, 2);
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM provider_scopes WHERE imported_category='tv/child'"
        )
        .await,
        1
    );
    assert!(c.execute("INSERT INTO provider_tests VALUES('00000000-0000-0000-0000-000000000003',1,1,'success',NULL)",()).await.is_err());
    let tx = c.transaction().await?;
    tx.execute(
        "UPDATE provider_scopes SET categories='[2001]' WHERE implementation='torznab'",
        (),
    )
    .await?;
    assert_eq!(scalar(&tx, "SELECT count(*) FROM provider_tests").await, 1);
    tx.rollback().await?;
    c.execute("UPDATE provider_scopes SET imported_category='movies/imported' WHERE implementation='qbittorrent' AND media_type='movies'",()).await?;
    drop(c);
    drop(raw);
    let db = Database::open_local(files.db()).await?;
    let c = db.connect().await?;
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM schema_migrations").await,
        15 // Latest-schema opens also add scoped remote path mappings.
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM providers WHERE revision=1 AND credentials=zeroblob(29)"
        )
        .await,
        3
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM provider_tests WHERE tested_at=1720000000"
        )
        .await,
        2
    );
    assert_eq!(scalar(&c,"SELECT count(*) FROM provider_scopes WHERE implementation='qbittorrent' AND initial_state='started' AND content_layout='default' AND sequential_order=0 AND first_last_first=0 AND add_tags=0").await,2);
    assert_eq!(scalar(&c,"SELECT count(*) FROM provider_scopes WHERE implementation!='qbittorrent' AND initial_state IS NULL AND content_layout IS NULL AND sequential_order IS NULL AND first_last_first IS NULL AND add_tags IS NULL").await,4);
    for assignment in [
        "initial_state=NULL",
        "initial_state='paused'",
        "initial_state=1",
        "content_layout=NULL",
        "content_layout='invalid'",
        "sequential_order=NULL",
        "sequential_order=0.5",
        "first_last_first=2",
        "add_tags='yes'",
        "add_tags=1",
    ] {
        assert!(c.execute(&format!("UPDATE provider_scopes SET {assignment} WHERE implementation='qbittorrent' AND media_type='movies'"),()).await.is_err(),"accepted {assignment}");
    }
    for assignment in [
        "initial_state='started'",
        "content_layout='default'",
        "sequential_order=0",
        "first_last_first=0",
        "add_tags=0",
    ] {
        assert!(
            c.execute(
                &format!("UPDATE provider_scopes SET {assignment} WHERE implementation='torznab'"),
                ()
            )
            .await
            .is_err()
        );
    }
    for value in ["tv", "tv/child", "movies/imported/child"] {
        let domain = if value.starts_with("tv") {
            "movies"
        } else {
            "tv"
        };
        assert!(c.execute("UPDATE provider_scopes SET imported_category=? WHERE implementation='qbittorrent' AND media_type=?",params![value,domain]).await.is_err());
    }
    // Slash boundary is literal: sibling prefixes and SQL wildcard characters remain valid.
    c.execute("UPDATE provider_scopes SET category='tv2',imported_category='tv%_done' WHERE implementation='qbittorrent' AND media_type='movies'",()).await?;
    c.execute("UPDATE provider_scopes SET initial_state='forced',content_layout='subfolder',sequential_order=1,first_last_first=1,add_tags=1 WHERE implementation='qbittorrent' AND media_type='tv'",()).await?;
    c.execute("INSERT INTO provider_tests VALUES('00000000-0000-0000-0000-000000000003',1,1720000001,'success',NULL)",()).await?;
    assert!(c.execute("UPDATE provider_tests SET config_revision=2 WHERE provider_id='00000000-0000-0000-0000-000000000003'",()).await.is_err());
    assert!(c.execute("UPDATE provider_tests SET provider_id='00000000-0000-0000-0000-000000000003' WHERE provider_id='00000000-0000-0000-0000-000000000001'",()).await.is_err());
    drop(c);
    drop(db);
    let db = Database::open_local(files.db()).await?;
    assert!(db.migration_backup().is_none());
    let c = db.connect().await?;
    assert_eq!(scalar(&c, "SELECT count(*) FROM provider_tests").await, 3);
    c.execute("UPDATE provider_scopes SET initial_state='stopped' WHERE implementation='qbittorrent' AND media_type='tv'",()).await?;
    assert_eq!(scalar(&c, "SELECT count(*) FROM provider_tests").await, 2);
    // Invalid INSERT follows the same constraints; rollback restores the removed scope.
    let tx = c.transaction().await?;
    tx.execute(
        "DELETE FROM provider_scopes WHERE implementation='qbittorrent' AND media_type='movies'",
        (),
    )
    .await?;
    for (state, cat) in [("NULL", "'movies'"), ("'started'", "'tv/child'")] {
        assert!(tx.execute(&format!("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags) VALUES('00000000-0000-0000-0000-000000000003','qbittorrent','movies',{cat},0,0,{state},'default',0,0,0)"),()).await.is_err());
    }
    tx.rollback().await?;
    Ok(())
}

#[tokio::test]
async fn provider_snapshot_mapping_upgrade_rollback_and_reopen() -> Result<(), Error> {
    let files = Sandbox::new();
    let raw = libsql::Builder::new_local(files.db()).build().await?;
    let c = raw.connect()?;
    c.execute_batch("PRAGMA foreign_keys=ON;CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY,name TEXT NOT NULL,checksum TEXT NOT NULL,sql TEXT NOT NULL,applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);").await?;
    for (index, (name, sql)) in [
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
    ]
    .iter()
    .enumerate()
    {
        c.execute_batch(sql).await?;
        let checksum: String = ring::digest::digest(&ring::digest::SHA256, sql.as_bytes())
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
            params![index as i64 + 1, *name, checksum, *sql],
        )
        .await?;
    }
    c.execute_batch("INSERT INTO snapshot_imports(application,fingerprint,schema_version) VALUES('sonarr','abc',233);INSERT INTO providers VALUES('00000000-0000-0000-0000-000000000001','torznab','Kept',0,1,1,1,'https://example.test/api',zeroblob(29));INSERT INTO provider_tests VALUES('00000000-0000-0000-0000-000000000001',1,1,'success',NULL);").await?;
    let tx = c.transaction().await?;
    tx.execute_batch(include_str!(
        "../migrations/0013_snapshot_provider_mappings.sql"
    ))
    .await?;
    assert!(
        tx.execute(
            "INSERT INTO snapshot_provider_mappings VALUES('sonarr','abc','Indexers',1,'bad-id',1)",
            ()
        )
        .await
        .is_err()
    );
    tx.rollback().await?;
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM sqlite_schema WHERE name='snapshot_provider_mappings'"
        )
        .await,
        0
    );
    drop(c);
    drop(raw);
    let db = Database::open_local(files.db()).await?;
    assert!(db.migration_backup().is_some());
    let c = db.connect().await?;
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM schema_migrations").await,
        15 // Latest-schema opens also add scoped remote path mappings.
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM providers WHERE name='Kept' AND credentials=zeroblob(29)"
        )
        .await,
        1
    );
    assert_eq!(scalar(&c, "SELECT count(*) FROM provider_tests").await, 1);
    for sql in [
        "INSERT INTO snapshot_provider_mappings VALUES('radarr','abc','Indexers',1,'00000000-0000-0000-0000-000000000001',1)",
        "INSERT INTO snapshot_provider_mappings VALUES('sonarr','abc','Other',1,'00000000-0000-0000-0000-000000000001',1)",
        "INSERT INTO snapshot_provider_mappings VALUES('sonarr','abc','Indexers',0,'00000000-0000-0000-0000-000000000001',1)",
        "INSERT INTO snapshot_provider_mappings VALUES('sonarr','abc','Indexers',1,'00000000-0000-0000-0000-000000000001',0)",
    ] {
        assert!(c.execute(sql, ()).await.is_err());
    }
    c.execute_batch("INSERT INTO snapshot_provider_mappings VALUES('sonarr','abc','Indexers',1,'00000000-0000-0000-0000-000000000001',1);INSERT INTO snapshot_provider_mappings VALUES('sonarr','abc','DownloadClients',1,'00000000-0000-0000-0000-000000000001',1);DELETE FROM providers;").await?;
    // Intentional absent provider FK preserves deletion and lets replay detect dangling mappings.
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM snapshot_provider_mappings").await,
        2
    );
    drop(c);
    drop(db);
    let db = Database::open_local(files.db()).await?;
    assert!(db.migration_backup().is_none());
    let c = db.connect().await?;
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM snapshot_provider_mappings").await,
        2
    );
    Ok(())
}

#[tokio::test]
async fn configured_roots_upgrade_rollback_domains_and_reopen() -> Result<(), Error> {
    let files = Sandbox::new();
    let raw = libsql::Builder::new_local(files.db()).build().await?;
    let c = raw.connect()?;
    c.execute_batch("PRAGMA foreign_keys=ON;CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY,name TEXT NOT NULL,checksum TEXT NOT NULL,sql TEXT NOT NULL,applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);").await?;
    for (index, (name, sql)) in [
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
    ]
    .iter()
    .enumerate()
    {
        c.execute_batch(sql).await?;
        let checksum: String = ring::digest::digest(&ring::digest::SHA256, sql.as_bytes())
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
            params![index as i64 + 1, *name, checksum, *sql],
        )
        .await?;
    }
    c.execute_batch("INSERT INTO series(id,title,path) VALUES(1,'Kept','/tv/Kept');INSERT INTO movie_metadata(id,title) VALUES(1,'Movie');INSERT INTO movies(id,metadata_id,path) VALUES(1,1,'/movies/Movie');").await?;
    let tx = c.transaction().await?;
    tx.execute_batch(include_str!("../migrations/0014_root_folders.sql"))
        .await?;
    assert!(
        tx.execute(
            "INSERT INTO root_folders(media_type,path) VALUES('other','/media')",
            ()
        )
        .await
        .is_err()
    );
    tx.rollback().await?;
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM sqlite_schema WHERE name='root_folders'"
        )
        .await,
        0
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM schema_migrations").await,
        13
    );
    drop(c);
    drop(raw);
    let db = Database::open_local(files.db()).await?;
    assert!(db.migration_backup().is_some());
    let c = db.connect().await?;
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM series WHERE id=1 AND path='/tv/Kept'"
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM movies WHERE id=1 AND path='/movies/Movie'"
        )
        .await,
        1
    );
    for path in [
        "/",
        "relative",
        "/media/",
        "/media//tv",
        "/media/../tv",
        "/media/./tv",
        "/media\\tv",
        "/media\ntv",
    ] {
        assert!(
            c.execute(
                "INSERT INTO root_folders(media_type,path) VALUES('tv',?)",
                [path]
            )
            .await
            .is_err(),
            "accepted {path}"
        );
    }
    c.execute_batch(
        "INSERT INTO root_folders(media_type,path) VALUES('tv','/media'),('movies','/media');",
    )
    .await?;
    assert!(
        c.execute(
            "INSERT INTO root_folders(media_type,path) VALUES('tv','/media')",
            ()
        )
        .await
        .is_err()
    );
    let old = scalar(&c, "SELECT id FROM root_folders WHERE media_type='tv'").await;
    c.execute("DELETE FROM root_folders WHERE media_type='tv'", ())
        .await?;
    c.execute(
        "INSERT INTO root_folders(media_type,path) VALUES('tv','/media')",
        (),
    )
    .await?;
    assert!(scalar(&c, "SELECT id FROM root_folders WHERE media_type='tv'").await > old);
    drop(c);
    drop(db);
    let db = Database::open_local(files.db()).await?;
    assert!(db.migration_backup().is_none());
    let c = db.connect().await?;
    assert_eq!(scalar(&c, "SELECT count(*) FROM root_folders").await, 2);
    Ok(())
}

#[tokio::test]
async fn remote_mappings_upgrade_rollback_constraints_and_reopen() -> Result<(), Error> {
    let files = Sandbox::new();
    let raw = libsql::Builder::new_local(files.db()).build().await?;
    let c = raw.connect()?;
    c.execute_batch("PRAGMA foreign_keys=ON;CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY,name TEXT NOT NULL,checksum TEXT NOT NULL,sql TEXT NOT NULL,applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);").await?;
    for (index, (name, sql)) in [
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
    ]
    .iter()
    .enumerate()
    {
        c.execute_batch(sql).await?;
        let checksum: String = ring::digest::digest(&ring::digest::SHA256, sql.as_bytes())
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
            params![index as i64 + 1, *name, checksum, *sql],
        )
        .await?;
    }
    c.execute_batch("INSERT INTO series(id,title,path) VALUES(1,'Kept','/tv/Kept');INSERT INTO root_folders(media_type,path) VALUES('tv','/media');").await?;
    let tx = c.transaction().await?;
    tx.execute_batch(include_str!("../migrations/0015_remote_path_mappings.sql"))
        .await?;
    tx.rollback().await?;
    assert_eq!(scalar(&c,"SELECT count(*) FROM sqlite_schema WHERE name IN ('remote_path_mappings','remote_mapping_revision')").await,0);
    assert_eq!(
        scalar(&c, "SELECT max(version) FROM schema_migrations").await,
        14
    );
    drop(c);
    drop(raw);
    let db = Database::open_local(files.db()).await?;
    assert!(db.migration_backup().is_some());
    let c = db.connect().await?;
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM series WHERE path='/tv/Kept'").await,
        1
    );
    assert_eq!(scalar(&c, "SELECT count(*) FROM root_folders").await, 1);
    for media in ["tv", "movies"] {
        c.execute("INSERT INTO remote_path_mappings(media_type,host,remote_path,remote_kind,remote_key,local_path) VALUES(?,'client','/remote','posix','/remote','/local')",[media]).await?;
    }
    for change in [
        "media_type='other'",
        "host='Client'",
        "remote_kind='other'",
        "remote_key='/wrong'",
        "local_path='/'",
        "local_path='/local/../other'",
        "revision=0",
        "revision=1",
        "id=30",
    ] {
        assert!(
            c.execute(
                &format!(
                    "UPDATE remote_path_mappings SET {change}{} WHERE media_type='tv'",
                    if change.starts_with("revision=") {
                        ""
                    } else {
                        ",revision=revision+1"
                    }
                ),
                ()
            )
            .await
            .is_err(),
            "{change}"
        );
    }
    assert!(c.execute("INSERT INTO remote_path_mappings(media_type,host,remote_path,remote_kind,remote_key,local_path) VALUES('tv','client','/remote','posix','/remote','/other')",()).await.is_err());
    c.execute(
        "UPDATE remote_path_mappings SET revision=2,local_path='/updated' WHERE media_type='tv'",
        (),
    )
    .await?;
    let old = scalar(
        &c,
        "SELECT id FROM remote_path_mappings WHERE media_type='tv'",
    )
    .await;
    c.execute("DELETE FROM remote_path_mappings WHERE media_type='tv'", ())
        .await?;
    c.execute("INSERT INTO remote_path_mappings(media_type,host,remote_path,remote_kind,remote_key,local_path) VALUES('tv','client','/remote','posix','/remote','/local')",()).await?;
    assert!(
        scalar(
            &c,
            "SELECT id FROM remote_path_mappings WHERE media_type='tv'"
        )
        .await
            > old
    );
    drop(c);
    drop(db);
    let db = Database::open_local(files.db()).await?;
    assert!(db.migration_backup().is_none());
    assert_eq!(
        scalar(
            &db.connect().await?,
            "SELECT count(*) FROM remote_path_mappings"
        )
        .await,
        2
    );
    Ok(())
}
