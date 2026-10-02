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
        47 // Latest adds movie credits47 (was 46: removed metadata health); historical migration prefixes stay fixed.
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
    // Version48 is unallocated after movie credits47 (previously 47 was the probe); insertion must precede unknown-history rejection.
    for sql in [
        "UPDATE schema_migrations SET checksum='tampered' WHERE version=1",
        "UPDATE schema_migrations SET sql=sql || '-- changed' WHERE version=1",
        "UPDATE schema_migrations SET name='different' WHERE version=1",
        "DELETE FROM schema_migrations WHERE version=1",
        "DELETE FROM schema_migrations",
        "INSERT INTO schema_migrations (version,name,checksum,sql) VALUES (48,'future','unknown','unknown')",
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
        47 // Latest adds movie credits47 (was 46: removed metadata health); historical migration prefixes stay fixed.
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
        47 // Latest adds movie credits47 (was 46: removed metadata health); historical migration prefixes stay fixed.
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
        47 // Latest adds movie credits47 (was 46: removed metadata health); historical migration prefixes stay fixed.
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
        47 // Latest adds movie credits47 (was 46: removed metadata health); historical migration prefixes stay fixed.
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
        47 // Latest adds movie credits47 (was 46: removed metadata health); historical migration prefixes stay fixed.
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
        47 // Latest adds movie credits47 (was 46: removed metadata health); historical migration prefixes stay fixed.
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

#[tokio::test]
async fn naming_settings_upgrade_rollback_domain_checks_and_reopen() -> Result<(), Error> {
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
    c.execute_batch("INSERT INTO series(id,title,path) VALUES(1,'Kept','/tv/Kept');")
        .await?;
    let tx = c.transaction().await?;
    tx.execute_batch(include_str!("../migrations/0029_naming_settings.sql"))
        .await?;
    assert!(
        tx.execute(
            "INSERT INTO naming_settings(domain,rename_enabled,replace_illegal_characters,colon_replacement,revision) VALUES('bad',0,1,'smart',1)",
            ()
        )
        .await
        .is_err()
    );
    tx.rollback().await?;
    assert_eq!(
        scalar(&c,"SELECT count(*) FROM sqlite_schema WHERE name IN ('naming_settings','naming_settings_revision_step')").await,
        0
    );
    drop(c);
    drop(raw);
    let db = Database::open_local(files.db()).await?;
    assert!(db.migration_backup().is_some());
    let c = db.connect().await?;
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM schema_migrations").await,
        47 // Latest adds movie credits47 (was 46: removed metadata health); historical migration prefixes stay fixed.
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM series WHERE path='/tv/Kept'").await,
        1
    );
    assert_eq!(scalar(&c, "SELECT count(*) FROM naming_settings").await, 2);
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM naming_settings WHERE domain='tv'").await,
        1
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM naming_settings WHERE domain='movies'"
        )
        .await,
        1
    );
    assert_eq!(
        scalar(&c,"SELECT count(*) FROM naming_settings WHERE rename_enabled=0 AND replace_illegal_characters=1 AND colon_replacement='smart' AND custom_colon_replacement IS NULL AND revision=1 AND standard_episode_format IS NULL AND daily_episode_format IS NULL AND anime_episode_format IS NULL AND series_folder_format IS NULL AND season_folder_format IS NULL AND specials_folder_format IS NULL AND multi_episode_style IS NULL AND standard_movie_format IS NULL AND movie_folder_format IS NULL").await,
        2
    );
    for sql in [
        "UPDATE naming_settings SET revision=2,standard_movie_format='{Movie Title}' WHERE domain='tv'",
        "UPDATE naming_settings SET revision=2,movie_folder_format='{Movie Title}' WHERE domain='tv'",
        "UPDATE naming_settings SET revision=2,standard_episode_format='{Episode Title}' WHERE domain='movies'",
        "UPDATE naming_settings SET revision=2,series_folder_format='{Series Title}' WHERE domain='movies'",
        "UPDATE naming_settings SET revision=2,multi_episode_style=0 WHERE domain='movies'",
        "UPDATE naming_settings SET revision=2,colon_replacement='custom' WHERE domain='tv'",
        "UPDATE naming_settings SET revision=2,custom_colon_replacement=':' WHERE domain='tv'",
        "UPDATE naming_settings SET revision=2,colon_replacement='unsupported' WHERE domain='tv'",
        "UPDATE naming_settings SET revision=2,multi_episode_style=6 WHERE domain='tv'",
        "UPDATE naming_settings SET revision=5 WHERE domain='movies'",
        "UPDATE naming_settings SET revision=1 WHERE domain='movies'",
        "INSERT INTO naming_settings(domain,rename_enabled,replace_illegal_characters,colon_replacement,revision) VALUES('anime',0,1,'smart',1)",
    ] {
        assert!(c.execute(sql, ()).await.is_err(), "accepted {sql}");
    }
    assert_eq!(
        scalar(&c, "SELECT revision FROM naming_settings WHERE domain='tv'").await,
        1
    );
    c.execute("UPDATE naming_settings SET revision=2,colon_replacement='custom',custom_colon_replacement=':' WHERE domain='tv'",()).await?;
    c.execute(
        "UPDATE naming_settings SET revision=2 WHERE domain='movies'",
        (),
    )
    .await?;
    assert_eq!(
        scalar(&c, "SELECT revision FROM naming_settings WHERE domain='tv'").await,
        2
    );
    assert_eq!(
        scalar(&c,"SELECT count(*) FROM naming_settings WHERE domain='tv' AND colon_replacement='custom' AND custom_colon_replacement=':'").await,
        1
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT revision FROM naming_settings WHERE domain='movies'"
        )
        .await,
        2
    );
    drop(c);
    drop(db);
    let db = Database::open_local(files.db()).await?;
    assert!(db.migration_backup().is_none());
    let c = db.connect().await?;
    assert_eq!(
        scalar(&c,"SELECT count(*) FROM naming_settings WHERE colon_replacement='custom' AND custom_colon_replacement=':'").await,
        1
    );
    assert_eq!(scalar(&c, "SELECT count(*) FROM naming_settings").await, 2);
    Ok(())
}

async fn schema_fingerprint(conn: &Connection) -> String {
    conn.query(
        "SELECT group_concat(type || ':' || name || ':' || coalesce(sql,''), char(10)) FROM (SELECT type,name,sql FROM sqlite_schema ORDER BY type,name)",
        (),
    )
    .await
    .unwrap()
    .next()
    .await
    .unwrap()
    .unwrap()
    .get::<Option<String>>(0)
    .unwrap()
    .unwrap_or_default()
}

#[tokio::test]
async fn manual_import_commands_upgrade_rollback_ownership_and_reopen() -> Result<(), Error> {
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
    c.execute_batch("INSERT INTO series(id,title,path) VALUES(1,'Kept','/tv/Kept');INSERT INTO seasons(series_id,number) VALUES(1,1);INSERT INTO episodes(id,series_id,season,number,title) VALUES(1,1,1,1,'One'),(2,1,1,2,'Two');").await?;

    // Seed one real, fully observed RSS-owned import so the disjointness guard has live data to reject.
    let indexer = uuid::Uuid::new_v4().to_string();
    let client = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'torznab','Indexer',1,1,1,1,'http://127.0.0.1:1/')",[indexer.clone()]).await?;
    c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year) VALUES(?,'torznab','tv','[5000]','[]',0,NULL)",[indexer.clone()]).await?;
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'qbittorrent','Client',1,1,1,1,'http://fixture.invalid')",[client.clone()]).await?;
    c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags) VALUES(?,'qbittorrent','tv','tv',0,0,'started','default',0,0,0)",[client.clone()]).await?;
    let rss_command = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO rss_commands(id,name,media_type,indexer_id,indexer_revision,client_id,client_revision,next_attempt_at,created_at) VALUES(?,'rss_sync','tv',?,1,?,1,100,100)",params![rss_command.clone(),indexer.clone(),client.clone()]).await?;
    let candidate = uuid::Uuid::new_v4().to_string();
    let hash = "a".repeat(40);
    c.execute("INSERT INTO rss_candidates(id,command_id,media_type,indexer_id,indexer_revision,client_id,client_revision,fingerprint,title,private_payload,series_id,movie_id,status,decision_reasons_json,created_at,updated_at) SELECT ?,id,media_type,indexer_id,indexer_revision,client_id,client_revision,?,'Release',?,1,NULL,'pending','[]',100,100 FROM rss_commands WHERE id=?",params![candidate.clone(),"c".repeat(64),vec![1u8;29],rss_command.clone()]).await?;
    c.execute(
        "INSERT INTO rss_candidate_episodes VALUES(?,1,1)",
        [candidate.clone()],
    )
    .await?;
    let identity = serde_json::json!({"version":1,"target":{"media_type":"episode","id":1},"hashes":[hash],"settings_fingerprint":"a".repeat(64),"payload_sha256":"b".repeat(64)}).to_string();
    c.execute(
        "UPDATE rss_candidates SET status='prepared',submission_identity_json=? WHERE id=?",
        params![identity, candidate.clone()],
    )
    .await?;
    c.execute(
        "INSERT INTO rss_hash_claims VALUES(?,?,?)",
        params![client.clone(), hash.clone(), candidate.clone()],
    )
    .await?;
    c.execute(
        "UPDATE rss_candidates SET status='submitting',private_payload=NULL WHERE id=?",
        [candidate.clone()],
    )
    .await?;
    c.execute(
        "UPDATE rss_candidates SET status='observed',observed_hash=? WHERE id=?",
        params![hash, candidate.clone()],
    )
    .await?;
    let owned_op = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO operations(id,media_type,episode_id,source,mode,destination,status,message) VALUES(?,'episode',1,'/download/tv','copy','/tv/Kept/one.mkv','preview','fixture')",[owned_op.clone()]).await?;
    c.execute(
        "INSERT INTO import_journal(operation_id,plan_json,phase) VALUES(?,'{}','preview')",
        [owned_op.clone()],
    )
    .await?;
    c.execute("INSERT INTO rss_candidate_imports(candidate_id,operation_id,quality_id,revision_json,provenance_json) VALUES(?,?,1,'{\"version\":1,\"real\":0,\"is_repack\":false}','{}')",params![candidate.clone(),owned_op.clone()]).await?;

    // A second, fully observed candidate on episode 2, kept unlinked so it can later prove the
    // reverse direction: it must still be rejected once episode 2's operation is manually claimed.
    let candidate2 = uuid::Uuid::new_v4().to_string();
    let hash2 = "e".repeat(40);
    c.execute("INSERT INTO rss_candidates(id,command_id,media_type,indexer_id,indexer_revision,client_id,client_revision,fingerprint,title,private_payload,series_id,movie_id,status,decision_reasons_json,created_at,updated_at) SELECT ?,id,media_type,indexer_id,indexer_revision,client_id,client_revision,?,'Release',?,1,NULL,'pending','[]',100,100 FROM rss_commands WHERE id=?",params![candidate2.clone(),"d".repeat(64),vec![1u8;29],rss_command.clone()]).await?;
    c.execute(
        "INSERT INTO rss_candidate_episodes VALUES(?,1,2)",
        [candidate2.clone()],
    )
    .await?;
    let identity2 = serde_json::json!({"version":1,"target":{"media_type":"episode","id":2},"hashes":[hash2],"settings_fingerprint":"a".repeat(64),"payload_sha256":"b".repeat(64)}).to_string();
    c.execute(
        "UPDATE rss_candidates SET status='prepared',submission_identity_json=? WHERE id=?",
        params![identity2, candidate2.clone()],
    )
    .await?;
    c.execute(
        "INSERT INTO rss_hash_claims VALUES(?,?,?)",
        params![client.clone(), hash2.clone(), candidate2.clone()],
    )
    .await?;
    c.execute(
        "UPDATE rss_candidates SET status='submitting',private_payload=NULL WHERE id=?",
        [candidate2.clone()],
    )
    .await?;
    c.execute(
        "UPDATE rss_candidates SET status='observed',observed_hash=? WHERE id=?",
        params![hash2, candidate2.clone()],
    )
    .await?;

    // A second, independent manually-previewed operation with no RSS ownership at all.
    let manual_op = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO operations(id,media_type,episode_id,source,mode,destination,status,message) VALUES(?,'episode',2,'/download/manual','copy','/tv/Kept/two.mkv','preview','fixture')",[manual_op.clone()]).await?;
    c.execute(
        "INSERT INTO import_journal(operation_id,plan_json,phase) VALUES(?,'{}','preview')",
        [manual_op.clone()],
    )
    .await?;
    let other_manual_op = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO operations(id,media_type,episode_id,source,mode,destination,status,message) VALUES(?,'episode',2,'/download/manual-2','copy','/tv/Kept/two-again.mkv','preview','fixture')",[other_manual_op.clone()]).await?;
    c.execute(
        "INSERT INTO import_journal(operation_id,plan_json,phase) VALUES(?,'{}','preview')",
        [other_manual_op.clone()],
    )
    .await?;
    let dangling_op = uuid::Uuid::new_v4().to_string();

    let before = schema_fingerprint(&c).await;
    let tx = c
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    tx.execute_batch(include_str!(
        "../migrations/0030_manual_import_commands.sql"
    ))
    .await?;
    // Even before rollback, the fresh DDL already rejects an operation the RSS pipeline owns.
    assert!(
        tx.execute(
            "INSERT INTO manual_import_commands(id,batch_id,operation_id,next_attempt_at,created_at) VALUES(?,?,?,100,100)",
            params![uuid::Uuid::new_v4().to_string(), uuid::Uuid::new_v4().to_string(), owned_op.clone()]
        )
        .await
        .is_err()
    );
    tx.rollback().await?;
    assert_eq!(
        schema_fingerprint(&c).await,
        before,
        "rollback of migration 30 must leave the prior schema (including the five DROP/CREATE-recreated sibling admit triggers) byte-identical"
    );
    drop(c);
    drop(raw);

    let db = Database::open_local(files.db()).await?;
    assert!(db.migration_backup().is_some());
    let c = db.connect().await?;
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM schema_migrations").await,
        47 // Latest adds movie credits47 (was 46: removed metadata health); historical migration prefixes stay fixed.
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM series WHERE path='/tv/Kept'").await,
        1
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM rss_candidate_imports").await,
        1
    );

    // The FK to operations(id) is structurally declared with ON DELETE RESTRICT...
    let mut fk = c
        .query("PRAGMA foreign_key_list(manual_import_commands)", ())
        .await?;
    let fk = fk.next().await?.unwrap();
    assert_eq!(fk.get::<String>(2)?, "operations"); // table
    assert_eq!(fk.get::<String>(3)?, "operation_id"); // from
    assert_eq!(fk.get::<String>(4)?, "id"); // to
    assert_eq!(fk.get::<String>(6)?, "RESTRICT"); // on_delete
    // ...but a dangling operation_id is actually rejected by the journal-existence admission
    // check first (an operation without a journal can never have been previewed); the FK itself
    // is unreachable on INSERT because that check subsumes it, which is why it is asserted above
    // structurally rather than behaviorally here.
    let message = c
        .execute(
            "INSERT INTO manual_import_commands(id,batch_id,operation_id,next_attempt_at,created_at) VALUES(?,?,?,100,100)",
            params![uuid::Uuid::new_v4().to_string(), uuid::Uuid::new_v4().to_string(), dangling_op.clone()]
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("operation has no import journal"),
        "{message}"
    );

    // An operation already claimed by the automated RSS/search-grab pipeline cannot be submitted here.
    let message = c
        .execute(
            "INSERT INTO manual_import_commands(id,batch_id,operation_id,next_attempt_at,created_at) VALUES(?,?,?,100,100)",
            params![uuid::Uuid::new_v4().to_string(), uuid::Uuid::new_v4().to_string(), owned_op.clone()]
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("operation is owned by automated download import"),
        "{message}"
    );

    // A batch groups independently claimable rows: two operations submitted together share one batch_id.
    let batch = uuid::Uuid::new_v4().to_string();
    let first = uuid::Uuid::new_v4().to_string();
    c.execute(
        "INSERT INTO manual_import_commands(id,batch_id,operation_id,next_attempt_at,created_at) VALUES(?,?,?,100,100)",
        params![first.clone(), batch.clone(), manual_op.clone()],
    )
    .await?;
    // Nor can the reverse: once manual_op is claimed above, a fresh RSS receipt for the same
    // operation (candidate2, fully observed on episode 2, otherwise eligible on its own) is
    // still rejected because manual_import_commands already owns it.
    let message = c
        .execute(
            "INSERT INTO rss_candidate_imports(candidate_id,operation_id,quality_id,revision_json,provenance_json) VALUES(?,?,1,'{\"version\":1,\"real\":0,\"is_repack\":false}','{}')",
            params![candidate2.clone(), manual_op.clone()],
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("operation is owned by manual import command"),
        "{message}"
    );
    let second = uuid::Uuid::new_v4().to_string();
    c.execute(
        "INSERT INTO manual_import_commands(id,batch_id,operation_id,next_attempt_at,created_at) VALUES(?,?,?,100,100)",
        params![second.clone(), batch.clone(), other_manual_op.clone()],
    )
    .await?;
    assert_eq!(
        scalar(
            &c,
            &format!("SELECT count(*) FROM manual_import_commands WHERE batch_id='{batch}'")
        )
        .await,
        2
    );

    // One active command per operation: a second non-terminal row for the same operation is rejected...
    let message = c
        .execute(
            "INSERT INTO manual_import_commands(id,batch_id,operation_id,next_attempt_at,created_at) VALUES(?,?,?,100,100)",
            params![uuid::Uuid::new_v4().to_string(), batch.clone(), manual_op.clone()]
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(message.contains("UNIQUE constraint failed"), "{message}");
    // Identity is immutable once queued.
    let message = c
        .execute(
            "UPDATE manual_import_commands SET operation_id=? WHERE id=?",
            params![other_manual_op.clone(), first.clone()],
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("command identity is immutable"),
        "{message}"
    );
    // ...queued -> running -> failed retains the row as terminal (attempts/started_at/completed_at/error_code rules enforced).
    c.execute(
        "UPDATE manual_import_commands SET status='running',attempts=1,started_at=100 WHERE id=?",
        [first.clone()],
    )
    .await?;
    c.execute(
        "UPDATE manual_import_commands SET status='failed',completed_at=101,error_code='storage_error' WHERE id=?",
        [first.clone()],
    )
    .await?;
    // A queued -> running -> succeeded row is terminal too.
    c.execute(
        "UPDATE manual_import_commands SET status='running',attempts=1,started_at=100 WHERE id=?",
        [second.clone()],
    )
    .await?;
    c.execute(
        "UPDATE manual_import_commands SET status='succeeded',completed_at=101 WHERE id=?",
        [second.clone()],
    )
    .await?;
    // A queued -> cancelled row (direct, no attempt) is terminal as well.
    let third = uuid::Uuid::new_v4().to_string();
    c.execute(
        "INSERT INTO manual_import_commands(id,batch_id,operation_id,next_attempt_at,created_at) VALUES(?,?,?,100,100)",
        params![third.clone(), batch.clone(), other_manual_op.clone()],
    )
    .await?;
    let message = c
        .execute(
            "UPDATE manual_import_commands SET status='cancelled' WHERE id=?",
            [third.clone()],
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("CHECK constraint failed"),
        "cancelling must still record completed_at: {message}"
    );
    c.execute(
        "UPDATE manual_import_commands SET status='cancelled',completed_at=101 WHERE id=?",
        [third.clone()],
    )
    .await?;
    // ...now that every prior row for manual_op is terminal, a fresh submission is admitted again.
    let fourth = uuid::Uuid::new_v4().to_string();
    c.execute(
        "INSERT INTO manual_import_commands(id,batch_id,operation_id,next_attempt_at,created_at) VALUES(?,?,?,100,100)",
        params![fourth.clone(), batch.clone(), manual_op.clone()],
    )
    .await?;

    // Active rows cannot be deleted; terminal rows can.
    let message = c
        .execute(
            "DELETE FROM manual_import_commands WHERE id=?",
            [fourth.clone()],
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("active commands cannot be deleted"),
        "{message}"
    );
    c.execute(
        "DELETE FROM manual_import_commands WHERE id=?",
        [first.clone()],
    )
    .await?;
    assert_eq!(
        scalar(
            &c,
            &format!("SELECT count(*) FROM manual_import_commands WHERE id='{first}'")
        )
        .await,
        0
    );
    Ok(())
}

#[tokio::test]
async fn quality_reset_commands_upgrade_rollback_capacity_and_reopen() -> Result<(), Error> {
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

    // Capture the six sibling admit triggers exactly as migration 0030 left them, so the DROP/CREATE
    // bodies added by 0031 can be checked for byte-for-byte fidelity, not just presence of the new term.
    let sibling_admit_triggers = [
        "commands_admit",
        "metadata_refresh_admit",
        "blocklist_clear_admit",
        "rss_commands_admit",
        "search_commands_admit",
        "manual_import_commands_admit",
    ];
    let mut sibling_sql_before = std::collections::BTreeMap::new();
    for name in sibling_admit_triggers {
        let sql: String = c
            .query(
                "SELECT sql FROM sqlite_schema WHERE type='trigger' AND name=?",
                [name],
            )
            .await?
            .next()
            .await?
            .unwrap()
            .get(0)?;
        sibling_sql_before.insert(name, sql);
    }

    // Move one movie quality definition away from its defaults so reset has something real to prove.
    c.execute("UPDATE quality_definitions SET min_size=999,max_size=999,preferred_size=999,title='Custom' WHERE media_type='movies' AND quality_id=(SELECT quality_id FROM quality_definitions WHERE media_type='movies' LIMIT 1)",()).await?;

    let before = schema_fingerprint(&c).await;
    let tx = c
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    tx.execute_batch(include_str!(
        "../migrations/0031_quality_reset_commands.sql"
    ))
    .await?;
    // Even before rollback, the fresh DDL already rejects a command that isn't freshly enqueued.
    assert!(
        tx.execute(
            "INSERT INTO quality_reset_commands(id,media_type,reset_titles,next_attempt_at,created_at,status) VALUES(?,?,0,100,100,'running')",
            params![uuid::Uuid::new_v4().to_string(), "movies"]
        )
        .await
        .is_err()
    );
    tx.rollback().await?;
    assert_eq!(
        schema_fingerprint(&c).await,
        before,
        "rollback of migration 31 must leave the prior schema (including the six DROP/CREATE-recreated sibling admit triggers) byte-identical"
    );
    drop(c);
    drop(raw);

    let db = Database::open_local(files.db()).await?;
    assert!(db.migration_backup().is_some());
    let c = db.connect().await?;
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM schema_migrations").await,
        47 // Latest adds movie credits47 (was 46: removed metadata health); historical migration prefixes stay fixed.
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM quality_definitions WHERE media_type='movies' AND title='Custom'"
        )
        .await,
        1
    );

    // A real upgrade DROP/CREATEs exactly the six known sibling admit triggers, and each recreated
    // body is byte-for-byte the migration-30 body with only the new capacity term inserted before
    // the limit; a `contains` check alone would miss drift elsewhere in the copied body. This test
    // opens the database through the real, current migration runner, so later migrations that also
    // DROP/CREATE these same six triggers (each appending one more chained capacity term, then
    // migration 33 making every term active-only) must be reflected here too, in the order they
    // were applied.
    let all_pool_tables = [
        "commands",
        "metadata_refresh_commands",
        "blocklist_clear_commands",
        "rss_commands",
        "search_commands",
        "manual_import_commands",
        "quality_reset_commands",
        "rescan_commands",
    ];
    for name in sibling_admit_triggers {
        let sql: String = c
            .query(
                "SELECT sql FROM sqlite_schema WHERE type='trigger' AND name=?",
                [name],
            )
            .await?
            .next()
            .await?
            .unwrap()
            .get(0)?;
        let mut expected = sibling_sql_before[name]
            .replacen(
                ">=1024",
                "+(SELECT count(*) FROM quality_reset_commands)>=1024",
                1,
            )
            .replacen(">=1024", "+(SELECT count(*) FROM rescan_commands)>=1024", 1);
        for table in all_pool_tables {
            expected = expected.replacen(
                &format!("(SELECT count(*) FROM {table})"),
                &format!(
                    "(SELECT count(*) FROM {table} WHERE status IN ('queued','running','retry_wait'))"
                ),
                1,
            );
        }
        // Reopening also applies migration42: append only its exact health capacity term;
        // byte equality still detects changes to every historical admission predicate.
        assert_eq!(expected.matches(">=1024").count(), 1);
        expected = expected.replacen(
            ">=1024",
            "+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))>=1024",
            1,
        );
        assert_eq!(sql, expected, "{name} drifted from a faithful DROP/CREATE");
    }

    // One active reset per media domain: a second 'movies' submission is rejected while the first is queued...
    let id = uuid::Uuid::new_v4().to_string();
    c.execute(
        "INSERT INTO quality_reset_commands(id,media_type,reset_titles,next_attempt_at,created_at) VALUES(?,?,1,100,100)",
        params![id.clone(), "movies"],
    )
    .await?;
    let message = c
        .execute(
            "INSERT INTO quality_reset_commands(id,media_type,reset_titles,next_attempt_at,created_at) VALUES(?,?,0,100,100)",
            params![uuid::Uuid::new_v4().to_string(), "movies"],
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(message.contains("UNIQUE constraint failed"), "{message}");
    // ...but a different media domain is independently admitted.
    let other_domain = uuid::Uuid::new_v4().to_string();
    c.execute(
        "INSERT INTO quality_reset_commands(id,media_type,reset_titles,next_attempt_at,created_at) VALUES(?,?,0,100,100)",
        params![other_domain.clone(), "tv"],
    )
    .await?;

    // queued -> running -> succeeded requires the outcome-projection column to be populated...
    c.execute(
        "UPDATE quality_reset_commands SET status='running',attempts=1,started_at=100 WHERE id=?",
        [id.clone()],
    )
    .await?;
    let message = c
        .execute(
            "UPDATE quality_reset_commands SET status='succeeded',completed_at=101 WHERE id=?",
            [id.clone()],
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("CHECK constraint failed"),
        "succeeding without a recorded outcome must fail: {message}"
    );
    c.execute(
        "UPDATE quality_reset_commands SET status='succeeded',completed_at=101,definitions_reset=1 WHERE id=?",
        [id.clone()],
    )
    .await?;

    // Identity, including reset_titles, is immutable once queued.
    let message = c
        .execute(
            "UPDATE quality_reset_commands SET reset_titles=1 WHERE id=?",
            [other_domain.clone()],
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("command identity is immutable"),
        "{message}"
    );

    // ...queued -> running -> failed is terminal too, and requires an error_code but no outcome count.
    c.execute(
        "UPDATE quality_reset_commands SET status='running',attempts=1,started_at=100 WHERE id=?",
        [other_domain.clone()],
    )
    .await?;
    c.execute(
        "UPDATE quality_reset_commands SET status='failed',completed_at=101,error_code='storage_error' WHERE id=?",
        [other_domain.clone()],
    )
    .await?;

    // Now that every prior row is terminal, a fresh submission for either domain is admitted again.
    let fresh = uuid::Uuid::new_v4().to_string();
    c.execute(
        "INSERT INTO quality_reset_commands(id,media_type,reset_titles,next_attempt_at,created_at) VALUES(?,?,0,100,100)",
        params![fresh.clone(), "movies"],
    )
    .await?;

    // Active rows cannot be deleted; terminal rows can.
    let message = c
        .execute(
            "DELETE FROM quality_reset_commands WHERE id=?",
            [fresh.clone()],
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("active commands cannot be deleted"),
        "{message}"
    );
    c.execute(
        "DELETE FROM quality_reset_commands WHERE id=?",
        [id.clone()],
    )
    .await?;
    assert_eq!(
        scalar(
            &c,
            &format!("SELECT count(*) FROM quality_reset_commands WHERE id='{id}'")
        )
        .await,
        0
    );
    Ok(())
}

#[tokio::test]
async fn rescan_commands_upgrade_rollback_mutual_exclusion_and_reopen() -> Result<(), Error> {
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

    // Seed data that must survive the upgrade untouched, and that the mutual-exclusion checks
    // target. Series 2 (with its own episode, for the TV branch of the download_processing
    // guards) and series 3 (bare, for the 'skipped' status check) give those checks a target that
    // isn't already occupied by series 1's own narrative. Movie 2 plays the same role for the
    // movie branch of the import_journal guards.
    c.execute_batch(
        "INSERT INTO series(id,title,path) VALUES(1,'Kept','/tv/Kept');
        INSERT INTO seasons(series_id,number) VALUES(1,1);
        INSERT INTO episodes(id,series_id,season,number,title) VALUES(1,1,1,1,'Pilot');
        INSERT INTO series(id,title,path) VALUES(2,'Other','/tv/Other');
        INSERT INTO seasons(series_id,number) VALUES(2,1);
        INSERT INTO episodes(id,series_id,season,number,title) VALUES(2,2,1,1,'Other Pilot');
        INSERT INTO series(id,title,path) VALUES(3,'Unmounted','/tv/Unmounted');
        INSERT INTO movie_metadata(id,title) VALUES(1,'Movie');
        INSERT INTO movies(id,metadata_id,path) VALUES(1,1,'/movies/Movie');
        INSERT INTO movie_metadata(id,title) VALUES(2,'Movie Two');
        INSERT INTO movies(id,metadata_id,path) VALUES(2,2,'/movies/MovieTwo');",
    )
    .await?;

    // Capture the seven sibling admit triggers exactly as migration 0031 left them, so the
    // DROP/CREATE bodies added by 0032 can be checked for byte-for-byte fidelity.
    let sibling_admit_triggers = [
        "commands_admit",
        "metadata_refresh_admit",
        "blocklist_clear_admit",
        "rss_commands_admit",
        "search_commands_admit",
        "manual_import_commands_admit",
        "quality_reset_admit",
    ];
    let mut sibling_sql_before = std::collections::BTreeMap::new();
    for name in sibling_admit_triggers {
        let sql: String = c
            .query(
                "SELECT sql FROM sqlite_schema WHERE type='trigger' AND name=?",
                [name],
            )
            .await?
            .next()
            .await?
            .unwrap()
            .get(0)?;
        sibling_sql_before.insert(name, sql);
    }

    let before = schema_fingerprint(&c).await;
    let tx = c
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    tx.execute_batch(include_str!("../migrations/0032_rescan_commands.sql"))
        .await?;
    // Even before rollback, the fresh DDL already rejects a command that isn't freshly enqueued.
    assert!(
        tx.execute(
            "INSERT INTO rescan_commands(id,media_type,series_id,next_attempt_at,created_at,status) VALUES(?,?,?,100,100,'running')",
            params![uuid::Uuid::new_v4().to_string(), "tv", 1]
        )
        .await
        .is_err()
    );
    tx.rollback().await?;
    assert_eq!(
        schema_fingerprint(&c).await,
        before,
        "rollback of migration 32 must leave the prior schema (including the seven DROP/CREATE-recreated sibling admit triggers) byte-identical"
    );
    drop(c);
    drop(raw);

    let db = Database::open_local(files.db()).await?;
    assert!(db.migration_backup().is_some());
    let c = db.connect().await?;
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM schema_migrations").await,
        47 // Latest adds movie credits47 (was 46: removed metadata health); historical migration prefixes stay fixed.
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM series WHERE path='/tv/Kept'").await,
        1
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM movies WHERE path='/movies/Movie'").await,
        1
    );

    // A real upgrade DROP/CREATEs exactly the seven known sibling admit triggers, and each
    // recreated body is byte-for-byte the migration-31 body with the new capacity term inserted
    // before the limit (migration 32), then every unconditional `count(*)` term made active-only
    // (migration 33, applied by this same reopen); a `contains` check alone would miss drift
    // elsewhere in the body.
    let all_pool_tables = [
        "commands",
        "metadata_refresh_commands",
        "blocklist_clear_commands",
        "rss_commands",
        "search_commands",
        "manual_import_commands",
        "quality_reset_commands",
        "rescan_commands",
    ];
    for name in sibling_admit_triggers {
        let sql: String = c
            .query(
                "SELECT sql FROM sqlite_schema WHERE type='trigger' AND name=?",
                [name],
            )
            .await?
            .next()
            .await?
            .unwrap()
            .get(0)?;
        let mut expected = sibling_sql_before[name].replacen(
            ">=1024",
            "+(SELECT count(*) FROM rescan_commands)>=1024",
            1,
        );
        for table in all_pool_tables {
            expected = expected.replacen(
                &format!("(SELECT count(*) FROM {table})"),
                &format!(
                    "(SELECT count(*) FROM {table} WHERE status IN ('queued','running','retry_wait'))"
                ),
                1,
            );
        }
        // Reopening also applies migration42: append only its exact health capacity term;
        // byte equality still detects changes to every historical admission predicate.
        assert_eq!(expected.matches(">=1024").count(), 1);
        expected = expected.replacen(
            ">=1024",
            "+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))>=1024",
            1,
        );
        assert_eq!(sql, expected, "{name} drifted from a faithful DROP/CREATE");
    }
    // The new eighth pool member carries the same full eight-way capacity sum from the start
    // (migration 32), then the same active-only rewrite as every sibling (migration 33).
    // The current reopen also appends health in migration42, so verify both final terms.
    let rescan_admit_sql: String = c
        .query(
            "SELECT sql FROM sqlite_schema WHERE type='trigger' AND name='rescan_admit'",
            (),
        )
        .await?
        .next()
        .await?
        .unwrap()
        .get(0)?;
    assert!(
        rescan_admit_sql.contains(
            "+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))>=1024"
        ),
        "{rescan_admit_sql}"
    );

    // An unknown series/movie id is rejected outright.
    let message = c
        .execute(
            "INSERT INTO rescan_commands(id,media_type,series_id,next_attempt_at,created_at) VALUES(?,?,?,100,100)",
            params![uuid::Uuid::new_v4().to_string(), "tv", 999],
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(message.contains("invalid rescan target"), "{message}");

    // A 'preview' import (no filesystem work yet, and may never be executed) does not block a TV
    // rescan of series 1 by itself...
    let owned_op = uuid::Uuid::new_v4().to_string();
    c.execute(
        "INSERT INTO operations(id,media_type,episode_id,source,mode,destination,status,message) VALUES(?,'episode',1,'/download/tv','copy','/tv/Kept/pilot.mkv','preview','fixture')",
        [owned_op.clone()],
    )
    .await?;
    c.execute(
        "INSERT INTO import_journal(operation_id,plan_json,phase) VALUES(?,'{}','preview')",
        [owned_op.clone()],
    )
    .await?;
    let rescan_tv = uuid::Uuid::new_v4().to_string();
    c.execute(
        "INSERT INTO rescan_commands(id,media_type,series_id,next_attempt_at,created_at) VALUES(?,?,?,100,100)",
        params![rescan_tv.clone(), "tv", 1],
    )
    .await?;

    // ...and the reverse guard's INSERT path (not just its UPDATE-out-of-preview path) catches a
    // second episode-1 operation whose journal is created directly at 'staging', the same way
    // manual imports are sometimes seeded (skipping 'preview' entirely).
    let second_tv_op = uuid::Uuid::new_v4().to_string();
    c.execute(
        "INSERT INTO operations(id,media_type,episode_id,source,mode,destination,status,message) VALUES(?,'episode',1,'/download/tv2','copy','/tv/Kept/pilot2.mkv','preview','fixture')",
        [second_tv_op.clone()],
    )
    .await?;
    let message = c
        .execute(
            "INSERT INTO import_journal(operation_id,plan_json,phase) VALUES(?,'{}','staging')",
            [second_tv_op.clone()],
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("import target has an active rescan"),
        "{message}"
    );

    // ...cross-domain isolation: an active TV rescan of series 1 never blocks an unrelated movie.
    let cross_movie = uuid::Uuid::new_v4().to_string();
    c.execute(
        "INSERT INTO rescan_commands(id,media_type,movie_id,next_attempt_at,created_at) VALUES(?,?,?,100,100)",
        params![cross_movie.clone(), "movies", 1],
    )
    .await?;
    c.execute(
        "UPDATE rescan_commands SET status='cancelled',completed_at=100 WHERE id=?",
        [cross_movie.clone()],
    )
    .await?;

    // ...but the reverse guard now blocks that same preview from starting real transfer work,
    // because a rescan is queued against its target.
    let message = c
        .execute(
            "UPDATE import_journal SET stage_json='{}',phase='staging' WHERE operation_id=?",
            [owned_op.clone()],
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("import target has an active rescan"),
        "{message}"
    );

    // Completing the rescan (succeeded requires both outcome-projection columns) releases the target...
    c.execute(
        "UPDATE rescan_commands SET status='running',attempts=1,started_at=100 WHERE id=?",
        [rescan_tv.clone()],
    )
    .await?;
    let message = c
        .execute(
            "UPDATE rescan_commands SET status='succeeded',completed_at=101 WHERE id=?",
            [rescan_tv.clone()],
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("CHECK constraint failed"),
        "succeeding without a recorded outcome must fail: {message}"
    );
    c.execute(
        "UPDATE rescan_commands SET status='succeeded',completed_at=101,files_adopted=2,files_removed=1 WHERE id=?",
        [rescan_tv.clone()],
    )
    .await?;

    // ...so the same preview can now start real transfer work...
    c.execute(
        "UPDATE import_journal SET stage_json='{}',phase='staging' WHERE operation_id=?",
        [owned_op.clone()],
    )
    .await?;

    // ...and, symmetrically, a fresh TV rescan of series 1 is now rejected by the forward check
    // while that transfer is in flight (not yet 'complete', no longer merely 'preview').
    let message = c
        .execute(
            "INSERT INTO rescan_commands(id,media_type,series_id,next_attempt_at,created_at) VALUES(?,?,?,100,100)",
            params![uuid::Uuid::new_v4().to_string(), "tv", 1],
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("rescan target has an in-flight import"),
        "{message}"
    );

    // ...but once that import reaches 'complete', the same series is admitted again.
    c.execute(
        "INSERT INTO episode_files VALUES(1,1,'/tv/Kept/pilot.mkv')",
        (),
    )
    .await?;
    c.execute("UPDATE episodes SET episode_file_id=1 WHERE id=1", ())
        .await?;
    c.execute(
        "UPDATE import_journal SET phase='staged' WHERE operation_id=?",
        [owned_op.clone()],
    )
    .await?;
    c.execute(
        "UPDATE import_journal SET phase='published' WHERE operation_id=?",
        [owned_op.clone()],
    )
    .await?;
    c.execute(
        "INSERT INTO import_history(operation_id,media_type,episode_id,episode_file_id,source,destination,size,sha256) VALUES(?,'episode',1,1,'/download/tv','/tv/Kept/pilot.mkv',10,printf('%064d',0))",
        [owned_op.clone()],
    )
    .await?;
    c.execute(
        "UPDATE import_journal SET phase='committed' WHERE operation_id=?",
        [owned_op.clone()],
    )
    .await?;
    c.execute(
        "UPDATE import_journal SET phase='complete' WHERE operation_id=?",
        [owned_op.clone()],
    )
    .await?;
    let rescan_tv2 = uuid::Uuid::new_v4().to_string();
    c.execute(
        "INSERT INTO rescan_commands(id,media_type,series_id,next_attempt_at,created_at) VALUES(?,?,?,100,100)",
        params![rescan_tv2.clone(), "tv", 1],
    )
    .await?;

    // One active rescan per target: a second queued row for the same series is rejected while
    // the first is still active.
    let message = c
        .execute(
            "INSERT INTO rescan_commands(id,media_type,series_id,next_attempt_at,created_at) VALUES(?,?,?,100,100)",
            params![uuid::Uuid::new_v4().to_string(), "tv", 1],
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(message.contains("UNIQUE constraint failed"), "{message}");

    // A failsafe skip (e.g. an unmounted root folder, per upstream DiskScanService) is a distinct
    // terminal outcome from 'succeeded': it requires a skip_reason instead of file-outcome counts,
    // and it must never silently look like a real scan happened.
    let rescan_skip = uuid::Uuid::new_v4().to_string();
    c.execute(
        "INSERT INTO rescan_commands(id,media_type,series_id,next_attempt_at,created_at) VALUES(?,?,?,100,100)",
        params![rescan_skip.clone(), "tv", 3],
    )
    .await?;
    c.execute(
        "UPDATE rescan_commands SET status='running',attempts=1,started_at=100 WHERE id=?",
        [rescan_skip.clone()],
    )
    .await?;
    let message = c
        .execute(
            "UPDATE rescan_commands SET status='skipped',completed_at=101 WHERE id=?",
            [rescan_skip.clone()],
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("CHECK constraint failed"),
        "skipping without a recorded reason must fail: {message}"
    );
    c.execute(
        "UPDATE rescan_commands SET status='skipped',completed_at=101,skip_reason='root_missing' WHERE id=?",
        [rescan_skip.clone()],
    )
    .await?;

    // A live, non-terminal download_processing row for a movie candidate blocks a movie rescan
    // of that movie...
    let indexer = uuid::Uuid::new_v4().to_string();
    let client = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'torznab','Indexer',1,1,1,1,'http://127.0.0.1:1/')",[indexer.clone()]).await?;
    c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year) VALUES(?,'torznab','movies','[2000]','[]',NULL,0)",[indexer.clone()]).await?;
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'qbittorrent','Client',1,1,1,1,'http://fixture.invalid')",[client.clone()]).await?;
    c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags) VALUES(?,'qbittorrent','movies','movies',0,0,'started','default',0,0,0)",[client.clone()]).await?;
    c.execute(
        "INSERT INTO download_processing_policies(provider_id,media_type,provider_revision,revision,enabled,mode) VALUES(?,'movies',1,1,1,'copy')",
        [client.clone()],
    )
    .await?;
    let rss_command = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO rss_commands(id,name,media_type,indexer_id,indexer_revision,client_id,client_revision,next_attempt_at,created_at) VALUES(?,'rss_sync','movies',?,1,?,1,100,100)",params![rss_command.clone(),indexer.clone(),client.clone()]).await?;
    let candidate = uuid::Uuid::new_v4().to_string();
    let hash = "a".repeat(40);
    c.execute("INSERT INTO rss_candidates(id,command_id,media_type,indexer_id,indexer_revision,client_id,client_revision,fingerprint,title,private_payload,series_id,movie_id,status,decision_reasons_json,created_at,updated_at) SELECT ?,id,media_type,indexer_id,indexer_revision,client_id,client_revision,?,'Release',?,NULL,NULL,'pending','[]',100,100 FROM rss_commands WHERE id=?",params![candidate.clone(),"c".repeat(64),vec![1u8;29],rss_command.clone()]).await?;
    let identity = serde_json::json!({"version":1,"target":{"media_type":"movie","id":1},"hashes":[hash],"settings_fingerprint":"a".repeat(64),"payload_sha256":"b".repeat(64)}).to_string();
    c.execute(
        "UPDATE rss_candidates SET status='prepared',submission_identity_json=?,movie_id=1 WHERE id=?",
        params![identity, candidate.clone()],
    )
    .await?;
    c.execute(
        "INSERT INTO rss_hash_claims VALUES(?,?,?)",
        params![client.clone(), hash.clone(), candidate.clone()],
    )
    .await?;
    c.execute(
        "UPDATE rss_candidates SET status='submitting',private_payload=NULL WHERE id=?",
        [candidate.clone()],
    )
    .await?;
    c.execute(
        "UPDATE rss_candidates SET status='observed',observed_hash=? WHERE id=?",
        params![hash, candidate.clone()],
    )
    .await?;
    c.execute(
        "INSERT INTO download_processing(candidate_id,policy_revision,status,next_attempt_at,created_at,updated_at) VALUES(?,1,'queued',100,100,100)",
        [candidate.clone()],
    )
    .await?;

    // 'queued' has not started preflight/transfer work, so it does not block a movie rescan by itself...
    let rescan_movie = uuid::Uuid::new_v4().to_string();
    c.execute(
        "INSERT INTO rescan_commands(id,media_type,movie_id,next_attempt_at,created_at) VALUES(?,?,?,100,100)",
        params![rescan_movie.clone(), "movies", 1],
    )
    .await?;

    // One active rescan per target: a second queued row for the same movie is rejected while the
    // first is still active.
    let message = c
        .execute(
            "INSERT INTO rescan_commands(id,media_type,movie_id,next_attempt_at,created_at) VALUES(?,?,?,100,100)",
            params![uuid::Uuid::new_v4().to_string(), "movies", 1],
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(message.contains("UNIQUE constraint failed"), "{message}");

    // Cross-domain isolation: an active movie rescan never blocks an unrelated TV series.
    let cross_tv = uuid::Uuid::new_v4().to_string();
    c.execute(
        "INSERT INTO rescan_commands(id,media_type,series_id,next_attempt_at,created_at) VALUES(?,?,?,100,100)",
        params![cross_tv.clone(), "tv", 2],
    )
    .await?;
    c.execute(
        "UPDATE rescan_commands SET status='cancelled',completed_at=100 WHERE id=?",
        [cross_tv.clone()],
    )
    .await?;

    // ...but the reverse guard now blocks that download from entering 'checking' (the gate to
    // real preflight/transfer work), because a rescan is active against its target.
    let message = c
        .execute(
            "UPDATE download_processing SET status='checking',preflight_attempts=1,total_preflight_attempts=1 WHERE candidate_id=?",
            [candidate.clone()],
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("download processing target has an active rescan"),
        "{message}"
    );

    // Identity, including movie_id, is immutable once queued.
    let message = c
        .execute(
            "UPDATE rescan_commands SET movie_id=NULL,media_type='tv',series_id=1 WHERE id=?",
            [rescan_movie.clone()],
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("command identity is immutable"),
        "{message}"
    );

    // queued -> running -> failed is terminal too, and requires an error_code but no outcome
    // counts; it also releases the target, both for the uniqueness index and for the reverse guard.
    c.execute(
        "UPDATE rescan_commands SET status='running',attempts=1,started_at=100 WHERE id=?",
        [rescan_movie.clone()],
    )
    .await?;
    c.execute(
        "UPDATE rescan_commands SET status='failed',completed_at=101,error_code='storage_error' WHERE id=?",
        [rescan_movie.clone()],
    )
    .await?;

    // ...so the download can now enter 'checking'...
    c.execute(
        "UPDATE download_processing SET status='checking',preflight_attempts=1,total_preflight_attempts=1 WHERE candidate_id=?",
        [candidate.clone()],
    )
    .await?;

    // ...and, symmetrically, the forward check on rescan_admit now rejects a fresh movie-1 rescan
    // while that download is actively checking/importing.
    let message = c
        .execute(
            "INSERT INTO rescan_commands(id,media_type,movie_id,next_attempt_at,created_at) VALUES(?,?,?,100,100)",
            params![uuid::Uuid::new_v4().to_string(), "movies", 1],
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("rescan target has in-flight download processing"),
        "{message}"
    );

    // Once that download reaches 'imported' (with its import complete), the same movie is
    // admitted again.
    let movie_op = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO operations(id,media_type,movie_id,source,mode,destination,status,message) VALUES(?,'movie',1,'/download/movie','copy','/movies/Movie/film.mkv','preview','fixture')",[movie_op.clone()]).await?;
    c.execute(
        "INSERT INTO import_journal(operation_id,plan_json,phase) VALUES(?,'{}','preview')",
        [movie_op.clone()],
    )
    .await?;
    c.execute("INSERT INTO rss_candidate_imports(candidate_id,operation_id,quality_id,revision_json,provenance_json) VALUES(?,?,1,'{\"version\":1,\"real\":0,\"is_repack\":false}','{}')",params![candidate.clone(),movie_op.clone()]).await?;
    c.execute(
        "UPDATE download_processing SET status='importing' WHERE candidate_id=?",
        [candidate.clone()],
    )
    .await?;
    c.execute(
        "UPDATE import_journal SET stage_json='{}',phase='staging' WHERE operation_id=?",
        [movie_op.clone()],
    )
    .await?;
    c.execute(
        "UPDATE import_journal SET phase='staged' WHERE operation_id=?",
        [movie_op.clone()],
    )
    .await?;
    c.execute(
        "UPDATE import_journal SET phase='published' WHERE operation_id=?",
        [movie_op.clone()],
    )
    .await?;
    c.execute(
        "INSERT INTO movie_files(id,movie_id,path) VALUES(1,1,'/movies/Movie/film.mkv')",
        (),
    )
    .await?;
    c.execute(
        "INSERT INTO import_history(operation_id,media_type,movie_id,movie_file_id,source,destination,size,sha256) VALUES(?,'movie',1,1,'/download/movie','/movies/Movie/film.mkv',10,printf('%064d',0))",
        [movie_op.clone()],
    )
    .await?;
    c.execute(
        "UPDATE import_journal SET phase='committed' WHERE operation_id=?",
        [movie_op.clone()],
    )
    .await?;
    c.execute(
        "UPDATE import_journal SET phase='complete' WHERE operation_id=?",
        [movie_op.clone()],
    )
    .await?;
    c.execute(
        "UPDATE download_processing SET status='imported' WHERE candidate_id=?",
        [candidate.clone()],
    )
    .await?;
    let rescan_movie2 = uuid::Uuid::new_v4().to_string();
    c.execute(
        "INSERT INTO rescan_commands(id,media_type,movie_id,next_attempt_at,created_at) VALUES(?,?,?,100,100)",
        params![rescan_movie2.clone(), "movies", 1],
    )
    .await?;

    // The movie branch of the import_journal guards, on movie 2 (independent of movie 1's own
    // narrative, and never routed through download_processing at all).
    let rescan_movie2_branch = uuid::Uuid::new_v4().to_string();
    c.execute(
        "INSERT INTO rescan_commands(id,media_type,movie_id,next_attempt_at,created_at) VALUES(?,?,?,100,100)",
        params![rescan_movie2_branch.clone(), "movies", 2],
    )
    .await?;
    let movie2_op = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO operations(id,media_type,movie_id,source,mode,destination,status,message) VALUES(?,'movie',2,'/download/movie2','copy','/movies/MovieTwo/film.mkv','preview','fixture')",[movie2_op.clone()]).await?;
    // A direct insert at 'staging' (the same shape used for movie 1 earlier in this test) is
    // rejected by rescan_blocks_import_insert's movie branch...
    let message = c
        .execute(
            "INSERT INTO import_journal(operation_id,plan_json,phase) VALUES(?,'{}','staging')",
            [movie2_op.clone()],
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("import target has an active rescan"),
        "{message}"
    );
    // ...but once the rescan is terminal, the same insert succeeds...
    c.execute(
        "UPDATE rescan_commands SET status='cancelled',completed_at=100 WHERE id=?",
        [rescan_movie2_branch.clone()],
    )
    .await?;
    c.execute(
        "INSERT INTO import_journal(operation_id,plan_json,phase) VALUES(?,'{}','staging')",
        [movie2_op.clone()],
    )
    .await?;
    // ...and now rescan_admit's own forward check (movie branch of the import_journal predicate)
    // rejects a fresh movie-2 rescan while that import sits mid-transfer.
    let message = c
        .execute(
            "INSERT INTO rescan_commands(id,media_type,movie_id,next_attempt_at,created_at) VALUES(?,?,?,100,100)",
            params![uuid::Uuid::new_v4().to_string(), "movies", 2],
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("rescan target has an in-flight import"),
        "{message}"
    );

    // The TV branch of the download_processing guards, on series 2/episode 2 (independent of both
    // the TV import_journal scenario on series 1 and the movie download_processing scenario above).
    let tv_indexer = uuid::Uuid::new_v4().to_string();
    let tv_client = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'torznab','TV Indexer',1,1,1,1,'http://127.0.0.1:2/')",[tv_indexer.clone()]).await?;
    c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year) VALUES(?,'torznab','tv','[5000]','[]',0,NULL)",[tv_indexer.clone()]).await?;
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'qbittorrent','TV Client',1,1,1,1,'http://fixture-tv.invalid')",[tv_client.clone()]).await?;
    c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags) VALUES(?,'qbittorrent','tv','tv',0,0,'started','default',0,0,0)",[tv_client.clone()]).await?;
    c.execute(
        "INSERT INTO download_processing_policies(provider_id,media_type,provider_revision,revision,enabled,mode) VALUES(?,'tv',1,1,1,'copy')",
        [tv_client.clone()],
    )
    .await?;
    let tv_rss_command = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO rss_commands(id,name,media_type,indexer_id,indexer_revision,client_id,client_revision,next_attempt_at,created_at) VALUES(?,'rss_sync','tv',?,1,?,1,100,100)",params![tv_rss_command.clone(),tv_indexer.clone(),tv_client.clone()]).await?;
    let tv_candidate = uuid::Uuid::new_v4().to_string();
    let tv_hash = "f".repeat(40);
    c.execute("INSERT INTO rss_candidates(id,command_id,media_type,indexer_id,indexer_revision,client_id,client_revision,fingerprint,title,private_payload,series_id,movie_id,status,decision_reasons_json,created_at,updated_at) SELECT ?,id,media_type,indexer_id,indexer_revision,client_id,client_revision,?,'Release',?,2,NULL,'pending','[]',100,100 FROM rss_commands WHERE id=?",params![tv_candidate.clone(),"e".repeat(64),vec![1u8;29],tv_rss_command.clone()]).await?;
    c.execute(
        "INSERT INTO rss_candidate_episodes VALUES(?,2,2)",
        [tv_candidate.clone()],
    )
    .await?;
    let tv_identity = serde_json::json!({"version":1,"target":{"media_type":"episode","id":2},"hashes":[tv_hash],"settings_fingerprint":"a".repeat(64),"payload_sha256":"b".repeat(64)}).to_string();
    c.execute(
        "UPDATE rss_candidates SET status='prepared',submission_identity_json=? WHERE id=?",
        params![tv_identity, tv_candidate.clone()],
    )
    .await?;
    c.execute(
        "INSERT INTO rss_hash_claims VALUES(?,?,?)",
        params![tv_client.clone(), tv_hash.clone(), tv_candidate.clone()],
    )
    .await?;
    c.execute(
        "UPDATE rss_candidates SET status='submitting',private_payload=NULL WHERE id=?",
        [tv_candidate.clone()],
    )
    .await?;
    c.execute(
        "UPDATE rss_candidates SET status='observed',observed_hash=? WHERE id=?",
        params![tv_hash, tv_candidate.clone()],
    )
    .await?;
    c.execute(
        "INSERT INTO download_processing(candidate_id,policy_revision,status,next_attempt_at,created_at,updated_at) VALUES(?,1,'queued',100,100,100)",
        [tv_candidate.clone()],
    )
    .await?;
    let rescan_series2 = uuid::Uuid::new_v4().to_string();
    c.execute(
        "INSERT INTO rescan_commands(id,media_type,series_id,next_attempt_at,created_at) VALUES(?,?,?,100,100)",
        params![rescan_series2.clone(), "tv", 2],
    )
    .await?;
    // A queued TV rescan of series 2 blocks that download from entering 'checking' (reverse
    // guard, TV branch)...
    let message = c
        .execute(
            "UPDATE download_processing SET status='checking',preflight_attempts=1,total_preflight_attempts=1 WHERE candidate_id=?",
            [tv_candidate.clone()],
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("download processing target has an active rescan"),
        "{message}"
    );
    // ...and, once the rescan is cancelled and the download reaches 'checking' on its own, the
    // forward check (TV branch) rejects a fresh series-2 rescan while it is active.
    c.execute(
        "UPDATE rescan_commands SET status='cancelled',completed_at=100 WHERE id=?",
        [rescan_series2.clone()],
    )
    .await?;
    c.execute(
        "UPDATE download_processing SET status='checking',preflight_attempts=1,total_preflight_attempts=1 WHERE candidate_id=?",
        [tv_candidate.clone()],
    )
    .await?;
    let message = c
        .execute(
            "INSERT INTO rescan_commands(id,media_type,series_id,next_attempt_at,created_at) VALUES(?,?,?,100,100)",
            params![uuid::Uuid::new_v4().to_string(), "tv", 2],
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("rescan target has in-flight download processing"),
        "{message}"
    );

    // Active rows cannot be deleted, but terminal rows can (also proving the pool re-admits once
    // every prior row for series 1, respectively movie 1, is terminal).
    let message = c
        .execute(
            "DELETE FROM rescan_commands WHERE id=?",
            [rescan_tv2.clone()],
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("active commands cannot be deleted"),
        "{message}"
    );
    c.execute(
        "UPDATE rescan_commands SET status='running',attempts=1,started_at=100 WHERE id=?",
        [rescan_tv2.clone()],
    )
    .await?;
    c.execute(
        "UPDATE rescan_commands SET status='succeeded',completed_at=101,files_adopted=0,files_removed=0 WHERE id=?",
        [rescan_tv2.clone()],
    )
    .await?;
    c.execute(
        "DELETE FROM rescan_commands WHERE id=?",
        [rescan_tv2.clone()],
    )
    .await?;
    assert_eq!(
        scalar(
            &c,
            &format!("SELECT count(*) FROM rescan_commands WHERE id='{rescan_tv2}'")
        )
        .await,
        0
    );
    Ok(())
}

fn normalize_whitespace(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

// Standing guard: every shared-pool admit trigger must count every pool table. The pool
// membership is *derived* from the triggers actually found (by their shared
// 'command capacity reached' abort message), not hardcoded, so a future migration that adds another
// pool table but forgets to update every sibling body fails this test loudly instead of silently
// under-counting capacity in production. The fixed comparison against today's known nine tables
// additionally catches the opposite failure: a table silently and consistently dropped from every
// trigger body at once (which the cross-reference check alone would not detect, since a shrunken
// but internally consistent set would still pass it).
#[tokio::test]
async fn pool_admit_triggers_cross_reference_every_capacity_table() -> Result<(), Error> {
    let files = Sandbox::new();
    let db = Database::open_local(files.db()).await?;
    let c = db.connect().await?;
    let mut rows = c
        .query(
            "SELECT name,tbl_name,sql FROM sqlite_schema WHERE type='trigger' AND name LIKE '%_admit' AND sql LIKE '%command capacity reached%'",
            (),
        )
        .await?;
    let mut triggers = std::collections::BTreeMap::new();
    let mut pool_tables = std::collections::BTreeSet::new();
    while let Some(row) = rows.next().await? {
        let name: String = row.get(0)?;
        let table: String = row.get(1)?;
        let sql: String = row.get(2)?;
        pool_tables.insert(table.clone());
        triggers.insert(name, (table, sql));
    }
    let known_pool_tables: std::collections::BTreeSet<String> = [
        "commands",
        "metadata_refresh_commands",
        "blocklist_clear_commands",
        "rss_commands",
        "search_commands",
        "manual_import_commands",
        "quality_reset_commands",
        "rescan_commands",
        "health_commands", // Migration0042 joins the active pool; terminal history remains excluded.
    ]
    .into_iter()
    .map(String::from)
    .collect();
    assert_eq!(
        pool_tables, known_pool_tables,
        "the shared command-capacity pool must have exactly these nine member tables; update \
         both this list and every admit trigger body together if that ever changes"
    );
    assert_eq!(
        triggers.len(),
        pool_tables.len(),
        "expected exactly one capacity-admit trigger per pool table"
    );
    for (name, (table, sql)) in &triggers {
        let body = normalize_whitespace(sql);
        for other in &pool_tables {
            let clause = normalize_whitespace(&format!(
                "FROM {other} WHERE status IN ('queued','running','retry_wait')"
            ));
            assert!(
                body.contains(&clause),
                "{name} (on {table}) must count active rows in {other} toward the shared pool, \
                 found: {sql}"
            );
        }
    }
    Ok(())
}

// Direct migration test for 0033 (the active-only command-capacity pool). Follows the upgrade/
// rollback/reopen pattern of migration 0032's own test (rescan_commands_upgrade_rollback_mutual_
// exclusion_and_reopen above): a fresh application and rollback must leave the schema
// byte-identical, and a real upgrade must preserve prior data through migration 33 and later. It then
// proves the actual behavior change: terminal rows that would have exhausted the old unconditional
// pool no longer occupy it, while active rows still enforce the exact 1024-row boundary.
#[tokio::test]
async fn command_capacity_migration33_active_only_upgrade_rollback_and_boundary()
-> Result<(), Error> {
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

    // Reproduce the actual bug on the real, unmodified v32 schema: a provider and 1024 terminal
    // (cancelled) `commands` rows -- ordinary historical churn, nothing concurrently active -- are
    // already enough to exhaust the old unconditional pool outright.
    let provider_id = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'qbittorrent','Client',1,1,1,1,'http://fixture.invalid')",[provider_id.clone()]).await?;
    c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags) VALUES(?,'qbittorrent','tv','tv',0,0,'started','default',0,0,0)",[provider_id.clone()]).await?;
    for _ in 0..1024 {
        let id = uuid::Uuid::new_v4().to_string();
        c.execute(
            "INSERT INTO commands(id,provider_id,media_type,provider_revision,next_attempt_at,created_at) VALUES(?,?,'tv',1,100,100)",
            params![id.clone(), provider_id.clone()],
        )
        .await?;
        c.execute(
            "UPDATE commands SET status='cancelled',completed_at=101 WHERE id=?",
            [id],
        )
        .await?;
    }
    assert_eq!(scalar(&c, "SELECT count(*) FROM commands").await, 1024);
    let message = c
        .execute(
            "INSERT INTO commands(id,provider_id,media_type,provider_revision,next_attempt_at,created_at) VALUES(?,?,'tv',1,100,100)",
            params![uuid::Uuid::new_v4().to_string(), provider_id.clone()],
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("command capacity reached"),
        "the pre-migration bug (unconditional count(*)) must reproduce on real v32: {message}"
    );

    let pool_tables = [
        "commands",
        "metadata_refresh_commands",
        "blocklist_clear_commands",
        "rss_commands",
        "search_commands",
        "manual_import_commands",
        "quality_reset_commands",
        "rescan_commands",
    ];
    let admit_triggers = [
        "commands_admit",
        "metadata_refresh_admit",
        "blocklist_clear_admit",
        "rss_commands_admit",
        "search_commands_admit",
        "manual_import_commands_admit",
        "quality_reset_admit",
        "rescan_admit",
    ];
    let mut sibling_sql_before = std::collections::BTreeMap::new();
    for name in admit_triggers {
        let sql: String = c
            .query(
                "SELECT sql FROM sqlite_schema WHERE type='trigger' AND name=?",
                [name],
            )
            .await?
            .next()
            .await?
            .unwrap()
            .get(0)?;
        sibling_sql_before.insert(name, sql);
    }
    // schema_fingerprint covers schema only (sqlite_schema type/name/sql), not row data, so the
    // 1024 seeded rows above do not affect it either way.
    let before = schema_fingerprint(&c).await;

    let tx = c
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    tx.execute_batch(include_str!(
        "../migrations/0033_command_capacity_active_only.sql"
    ))
    .await?;
    tx.rollback().await?;
    assert_eq!(
        schema_fingerprint(&c).await,
        before,
        "rollback of migration 33 must leave the prior schema (including all eight DROP/CREATE-\
         recreated admit triggers) byte-identical"
    );
    drop(c);
    drop(raw);

    let db = Database::open_local(files.db()).await?;
    assert!(db.migration_backup().is_some());
    let c = db.connect().await?;
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM schema_migrations").await,
        47 // Latest adds movie credits47 (was 46: removed metadata health); historical migration prefixes stay fixed.
    );
    // A real upgrade preserves prior data: all 1024 seeded rows (and the provider they reference)
    // survive untouched.
    assert_eq!(scalar(&c, "SELECT count(*) FROM commands").await, 1024);
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM commands WHERE status='cancelled'").await,
        1024
    );
    assert_eq!(
        scalar(
            &c,
            &format!("SELECT count(*) FROM providers WHERE id='{provider_id}'")
        )
        .await,
        1
    );

    // Every sibling trigger is DROP/CREATE-recreated with each unconditional `count(*)` term
    // replaced by an active-only (queued/running/retry_wait) term for every pool table, and
    // nothing else changes.
    for name in admit_triggers {
        let sql: String = c
            .query(
                "SELECT sql FROM sqlite_schema WHERE type='trigger' AND name=?",
                [name],
            )
            .await?
            .next()
            .await?
            .unwrap()
            .get(0)?;
        let mut expected = sibling_sql_before[name].clone();
        for table in pool_tables {
            expected = expected.replacen(
                &format!("(SELECT count(*) FROM {table})"),
                &format!(
                    "(SELECT count(*) FROM {table} WHERE status IN ('queued','running','retry_wait'))"
                ),
                1,
            );
        }
        // Reopening also applies migration42: append only its exact health capacity term;
        // byte equality still detects changes to every historical admission predicate.
        assert_eq!(expected.matches(">=1024").count(), 1);
        expected = expected.replacen(
            ">=1024",
            "+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))>=1024",
            1,
        );
        assert_eq!(sql, expected, "{name} drifted from a faithful DROP/CREATE");
    }

    // Direct proof of the bug fix, on the very data that reproduced the bug above: the exact same
    // insert that was rejected under the pre-upgrade (v32) trigger now succeeds after the real
    // upgrade, because none of those 1024 historical rows are active.
    let fresh = uuid::Uuid::new_v4().to_string();
    c.execute(
        "INSERT INTO commands(id,provider_id,media_type,provider_revision,next_attempt_at,created_at) VALUES(?,?,'tv',1,100,100)",
        params![fresh.clone(), provider_id.clone()],
    )
    .await?;
    c.execute(
        "UPDATE commands SET status='cancelled',completed_at=101 WHERE id=?",
        [fresh],
    )
    .await?;

    // Separately, active rows still enforce the exact 1024-row boundary: seed 1023 active rows
    // (search_commands has no per-target uniqueness, so one indexer/episode pair is enough), admit
    // the 1024th in a different pool table, reject the 1025th outright, then free exactly one slot.
    c.execute_batch("INSERT INTO series(id,tvdb_id,title,path) VALUES(1,101,'TV','/tv');INSERT INTO seasons VALUES(1,1,1);INSERT INTO episodes(id,series_id,season,number,title) VALUES(1,1,1,1,'One');").await?;
    let search_indexer = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'torznab','Indexer',1,1,1,1,'http://127.0.0.1:1/')",[search_indexer.clone()]).await?;
    c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year) VALUES(?,'torznab','tv','[5000]','[]',0,NULL)",[search_indexer.clone()]).await?;
    let captured = serde_json::json!({"media_type":"tv","series_id":1,"episode_id":1,"tvdb_id":101,"title":"Boundary","season":1,"number":1,"series_type":"standard","use_scene_numbering":false});
    for _ in 0..1023 {
        let id = uuid::Uuid::new_v4().to_string();
        c.execute(
            "INSERT INTO search_commands(id,mode,media_type,requested_episode_id,captured_target_json,indexer_id,indexer_revision,client_id,client_revision,next_attempt_at,created_at) VALUES(?,'automatic','tv',1,?,?,1,?,1,9007199254740000,100)",
            params![id, captured.to_string(), search_indexer.clone(), provider_id.clone()],
        )
        .await?;
    }
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM search_commands WHERE status='queued'"
        )
        .await,
        1023
    );
    let boundary = uuid::Uuid::new_v4().to_string();
    c.execute(
        "INSERT INTO commands(id,provider_id,media_type,provider_revision,next_attempt_at,created_at) VALUES(?,?,'tv',1,100,100)",
        params![boundary.clone(), provider_id.clone()],
    )
    .await?;
    let message = c
        .execute(
            "INSERT INTO blocklist_clear_commands(id,name,media_type,next_attempt_at,created_at) VALUES(?,'clear_blocklist','tv',100,100)",
            [uuid::Uuid::new_v4().to_string()],
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(message.contains("command capacity reached"), "{message}");
    c.execute(
        "UPDATE commands SET status='cancelled',completed_at=101 WHERE id=?",
        [boundary],
    )
    .await?;
    c.execute(
        "INSERT INTO blocklist_clear_commands(id,name,media_type,next_attempt_at,created_at) VALUES(?,'clear_blocklist','tv',100,100)",
        [uuid::Uuid::new_v4().to_string()],
    )
    .await?;
    Ok(())
}
