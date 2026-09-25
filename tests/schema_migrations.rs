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
        2
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
    for sql in [
        "UPDATE schema_migrations SET checksum='tampered' WHERE version=1",
        "UPDATE schema_migrations SET sql=sql || '-- changed' WHERE version=1",
        "UPDATE schema_migrations SET name='different' WHERE version=1",
        "DELETE FROM schema_migrations WHERE version=1",
        "DELETE FROM schema_migrations",
        "INSERT INTO schema_migrations (version,name,checksum,sql) VALUES (3,'future','unknown','unknown')",
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
