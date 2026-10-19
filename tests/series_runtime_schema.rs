//! Actual54 upgrades and snapshot replay witnesses for the nullable series runtime.
use hrrdarr::{
    db::{Database, Error},
    snapshots::{self, Application},
};
use libsql::{Connection, Value, params};
use std::path::{Path, PathBuf};
const MIGRATION: &str = include_str!("../migrations/0055_series_runtime.sql");
static IMPORT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let p =
            std::env::temp_dir().join(format!("hrrdarr-series-runtime-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
async fn scalar(c: &Connection, sql: &str) -> Result<i64, Error> {
    Ok(c.query(sql, ())
        .await?
        .next()
        .await?
        .ok_or("missing scalar")?
        .get(0)?)
}
async fn dump(c: &Connection, sql: &str) -> Result<Vec<Vec<Value>>, Error> {
    let mut rows = c.query(sql, ()).await?;
    let mut out = Vec::new();
    while let Some(r) = rows.next().await? {
        out.push(
            (0..r.column_count())
                .map(|i| r.get_value(i))
                .collect::<Result<Vec<_>, _>>()?,
        );
    }
    Ok(out)
}
async fn predecessor(source: &Connection, path: &Path) -> Result<libsql::Database, Error> {
    let db = libsql::Builder::new_local(path).build().await?;
    let c = db.connect()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // The raw predecessor builder uses the process umask. Snapshot replay
        // requires the same owner-only database permissions as a native fresh DB.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    c.execute_batch("PRAGMA foreign_keys=OFF;CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY,name TEXT NOT NULL,checksum TEXT NOT NULL,sql TEXT NOT NULL,applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);").await?;
    let tx = c.transaction().await?;
    let mut rows = source.query("SELECT version,name,checksum,sql FROM schema_migrations WHERE version<=54 ORDER BY version",()).await?;
    let mut n = 0;
    while let Some(r) = rows.next().await? {
        let sql: String = r.get(3)?;
        tx.execute_batch(&sql).await?;
        tx.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql)VALUES(?,?,?,?)",
            params![
                r.get::<i64>(0)?,
                r.get::<String>(1)?,
                r.get::<String>(2)?,
                sql
            ],
        )
        .await?;
        n += 1;
    }
    assert_eq!(n, 54);
    tx.commit().await?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    Ok(db)
}
async fn source(s: &Scratch, name: &str, runtime: Option<&str>) -> Result<Vec<u8>, Error> {
    let p = s.0.join(name);
    let db = libsql::Builder::new_local(&p).build().await?;
    let c = db.connect()?;
    c.execute_batch("CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES(233);CREATE TABLE Series(Id INTEGER,TvdbId INTEGER,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,Seasons TEXT,PrivateSetting TEXT);INSERT INTO Series VALUES(7,777,'TV',2020,'/tv/TV',1,'[]','PRIVATE_RUNTIME_SENTINEL');CREATE TABLE Episodes(Id INTEGER,SeriesId INTEGER,SeasonNumber INTEGER,EpisodeNumber INTEGER,Title TEXT,Monitored INTEGER,EpisodeFileId INTEGER);CREATE TABLE EpisodeFiles(Id INTEGER,SeriesId INTEGER,RelativePath TEXT);").await?;
    if let Some(value) = runtime {
        // No affinity: hostile TEXT/REAL/BLOB source values retain their original types.
        c.execute_batch(&format!(
            "ALTER TABLE Series ADD COLUMN Runtime;UPDATE Series SET Runtime={value};"
        ))
        .await?;
    }
    drop(c);
    drop(db);
    Ok(std::fs::read(p)?)
}
fn fingerprint(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
// Model the actual historical writer's private tagged archive, not an untyped JSON approximation.
async fn seed_historical(c: &Connection, bytes: &[u8], source_path: &Path) -> Result<(), Error> {
    c.execute_batch("INSERT INTO series(id,tvdb_id,title,year,path,monitored)VALUES(70,777,'TV',2020,'/tv/TV',1);").await?;
    let fp = fingerprint(bytes);
    c.execute("INSERT INTO snapshot_imports(application,fingerprint,schema_version,episode_metadata_version)VALUES('sonarr',?,233,1)",[fp.clone()]).await?;
    c.execute(
        "INSERT INTO snapshot_mappings VALUES('sonarr',?,'series',7,70)",
        [fp.clone()],
    )
    .await?;
    let db = libsql::Builder::new_local(source_path).build().await?;
    let raw = db.connect()?;
    for table in ["VersionInfo", "Series", "Episodes", "EpisodeFiles"] {
        let mut columns = raw
            .query(&format!("PRAGMA table_info({table})"), ())
            .await?;
        let mut names = Vec::new();
        while let Some(row) = columns.next().await? {
            names.push(row.get::<String>(1)?);
        }
        let mut rows = raw
            .query(&format!("SELECT * FROM {table} ORDER BY rowid"), ())
            .await?;
        let mut ordinal = 0;
        while let Some(row) = rows.next().await? {
            let mut object = serde_json::Map::new();
            for (i, name) in names.iter().enumerate() {
                let encoded = match row.get_value(i as i32)? {
                    Value::Null => serde_json::json!(["null"]),
                    Value::Integer(v) => serde_json::json!(["integer", v]),
                    Value::Text(v) => serde_json::json!(["text", v]),
                    _ => return Err("unexpected fixture source value".into()),
                };
                object.insert(name.clone(), encoded);
            }
            c.execute(
                "INSERT INTO snapshot_records VALUES('sonarr',?,?,?,?)",
                params![fp.clone(), table, ordinal, serde_json::to_string(&object)?],
            )
            .await?;
            ordinal += 1;
        }
    }
    Ok(())
}

#[tokio::test]
async fn schema55_actual54_backup_rollback_reopen_preserve_both_domains() -> Result<(), Error> {
    let s = Scratch::new();
    let fresh = Database::open_local(s.0.join("fresh.db")).await?;
    let fc = fresh.connect().await?;
    assert_eq!(
        scalar(&fc, "SELECT max(version) FROM schema_migrations").await?,
        76 // Reasoning: latest is migration file 0076 wanted_last_search at position 76, appended after 0075 downloadstation; historical prefixes stay fixed.
    );
    let path = s.0.join("old.db");
    let old = predecessor(&fc, &path).await?;
    let c = old.connect()?;
    c.execute_batch("INSERT INTO series(id,title,path)VALUES(1,'TV','/tv');INSERT INTO seasons(series_id,number)VALUES(1,1);INSERT INTO episodes(id,series_id,season,number,title,runtime)VALUES(1,1,1,1,'Episode',25);INSERT INTO movie_metadata(id,title,runtime)VALUES(1,'Film',100);INSERT INTO movies(id,metadata_id,path)VALUES(1,1,'/movies');INSERT INTO snapshot_imports(application,fingerprint,schema_version)VALUES('sonarr','old',233);INSERT INTO snapshot_records VALUES('sonarr','old','Private',0,'{\"secret\":[\"text\",\"kept\"]}');").await?;
    let schema = dump(&c, "SELECT type,name,sql FROM sqlite_schema ORDER BY name").await?;
    let archives = dump(&c, "SELECT * FROM snapshot_records ORDER BY rowid").await?;
    let tx = c.transaction().await?;
    tx.execute_batch(MIGRATION).await?;
    tx.execute("UPDATE series SET runtime=0", ()).await?;
    assert!(
        tx.execute("UPDATE series SET runtime=2147483648", ())
            .await
            .is_err()
    );
    tx.rollback().await?;
    assert_eq!(
        schema,
        dump(&c, "SELECT type,name,sql FROM sqlite_schema ORDER BY name").await?
    );
    assert_eq!(
        archives,
        dump(&c, "SELECT * FROM snapshot_records ORDER BY rowid").await?
    );
    drop(c);
    drop(old);
    let db = Database::open_local(&path).await?;
    let backup = db.migration_backup().ok_or("missing backup")?.to_path_buf();
    // Keep ownership of the copied backup path while libSQL holds its connection.
    let backup_path = s.0.join("backup-copy.db");
    std::fs::copy(backup.join("database.db"), &backup_path)?;
    let backup_db = libsql::Builder::new_local(&backup_path).build().await?;
    let bc = backup_db.connect()?;
    assert_eq!(
        schema,
        dump(&bc, "SELECT type,name,sql FROM sqlite_schema ORDER BY name").await?
    );
    assert_eq!(
        archives,
        dump(&bc, "SELECT * FROM snapshot_records ORDER BY rowid").await?
    );
    let c = db.connect().await?;
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM series WHERE runtime IS NULL").await?,
        1
    );
    assert_eq!(
        scalar(&c, "SELECT runtime FROM episodes WHERE id=1").await?,
        25
    );
    assert_eq!(
        scalar(&c, "SELECT runtime FROM movie_metadata WHERE id=1").await?,
        100
    );
    assert_eq!(
        scalar(&c, "SELECT series_runtime_version FROM snapshot_imports").await?,
        0
    );
    assert_eq!(
        archives,
        dump(&c, "SELECT * FROM snapshot_records ORDER BY rowid").await?
    );
    for invalid in ["-1", "2147483648", "1.5", "'invalid'", "X'00'"] {
        assert!(
            c.execute(&format!("UPDATE series SET runtime={invalid}"), ())
                .await
                .is_err(),
            "{invalid}"
        );
    }
    for valid in ["NULL", "0", "10081", "2147483647"] {
        c.execute(&format!("UPDATE series SET runtime={valid}"), ())
            .await?;
    }
    for invalid in ["NULL", "-1", "2"] {
        assert!(
            c.execute(
                &format!("UPDATE snapshot_imports SET series_runtime_version={invalid}"),
                ()
            )
            .await
            .is_err()
        );
    }
    assert!(
        c.query("PRAGMA foreign_key_check", ())
            .await?
            .next()
            .await?
            .is_none()
    );
    drop(c);
    drop(db);
    let reopened = Database::open_local(&path).await?;
    assert!(reopened.migration_backup().is_none());
    assert_eq!(
        scalar(&reopened.connect().await?, "SELECT runtime FROM series").await?,
        2147483647
    );
    Ok(())
}

#[tokio::test]
async fn snapshot_runtime_fresh_absent_null_zero_bounds_and_local_conflicts() -> Result<(), Error> {
    let _guard = IMPORT_LOCK.lock().await;
    let s = Scratch::new();
    for (i, runtime) in [None, Some("NULL"), Some("0"), Some("2147483647")]
        .into_iter()
        .enumerate()
    {
        let bytes = source(&s, &format!("source{i}.db"), runtime).await?;
        let db = Database::open_local(s.0.join(format!("target{i}.db"))).await?;
        let c = db.connect().await?;
        let dry = snapshots::import(&db, Application::Sonarr, bytes.clone(), true).await?;
        assert!(!dry.applied);
        assert_eq!(scalar(&c, "SELECT count(*) FROM series").await?, 0);
        let report = snapshots::import(&db, Application::Sonarr, bytes.clone(), false).await?;
        assert!(report.applied);
        assert_eq!(report.metadata_backfilled, 0);
        assert!(
            !report
                .unsupported
                .iter()
                .any(|u| u.table == "Series" && u.columns.iter().any(|c| c == "Runtime"))
        );
        assert!(
            report
                .unsupported
                .iter()
                .any(|u| u.columns.iter().any(|c| c == "PrivateSetting"))
        );
        assert!(!serde_json::to_string(&report)?.contains("PRIVATE_RUNTIME_SENTINEL"));
        let expected = match runtime {
            Some("0") => Some(0i64),
            Some("2147483647") => Some(2147483647),
            _ => None,
        };
        assert_eq!(
            c.query("SELECT runtime FROM series", ())
                .await?
                .next()
                .await?
                .unwrap()
                .get::<Option<i64>>(0)?,
            expected
        );
        assert_eq!(
            scalar(&c, "SELECT series_runtime_version FROM snapshot_imports").await?,
            1
        );
        let archives = dump(&c, "SELECT * FROM snapshot_records ORDER BY rowid").await?;
        let replay = snapshots::import(&db, Application::Sonarr, bytes.clone(), false).await?;
        assert!(replay.applied);
        assert_eq!(replay.mapped, 0);
        assert_eq!(replay.metadata_backfilled, 0);
        c.execute("UPDATE series SET runtime=42", ()).await?;
        let conflict = snapshots::import(&db, Application::Sonarr, bytes, false).await?;
        assert!(!conflict.applied);
        assert!(conflict.conflicts > 0);
        assert_eq!(scalar(&c, "SELECT runtime FROM series").await?, 42);
        assert_eq!(
            archives,
            dump(&c, "SELECT * FROM snapshot_records ORDER BY rowid").await?
        );
    }
    // A fresh fingerprint must not gain historical-backfill permission merely
    // because profile preparation creates the import receipt before the core writer.
    let fresh_bytes = source(&s, "fresh-conflict-source.db", Some("0")).await?;
    let local = Database::open_local(s.0.join("fresh-conflict-target.db")).await?;
    let lc = local.connect().await?;
    lc.execute_batch(
        "INSERT INTO series(tvdb_id,title,year,path,monitored)VALUES(777,'TV',2020,'/tv/TV',1);",
    )
    .await?;
    let conflict = snapshots::import(&local, Application::Sonarr, fresh_bytes, false).await?;
    assert!(!conflict.applied);
    assert!(conflict.conflicts > 0);
    assert_eq!(
        scalar(&lc, "SELECT count(*) FROM series WHERE runtime IS NULL").await?,
        1
    );
    assert_eq!(
        scalar(&lc, "SELECT count(*) FROM snapshot_imports").await?,
        0
    );
    let db = Database::open_local(s.0.join("invalid.db")).await?;
    for (i, value) in ["-1", "2147483648", "1.5", "'45'", "X'00'"]
        .into_iter()
        .enumerate()
    {
        let bytes = source(&s, &format!("invalid{i}.db"), Some(value)).await?;
        assert!(
            snapshots::import(&db, Application::Sonarr, bytes, false)
                .await
                .is_err(),
            "{value}"
        );
        assert_eq!(
            scalar(
                &db.connect().await?,
                "SELECT count(*) FROM snapshot_imports"
            )
            .await?,
            0
        );
    }
    Ok(())
}

#[tokio::test]
async fn snapshot_runtime_actual54_exact_archive_replay_is_once_only_and_atomic()
-> Result<(), Error> {
    let _guard = IMPORT_LOCK.lock().await;
    let s = Scratch::new();
    let source_path = s.0.join("source.db");
    let bytes = source(&s, "source.db", Some("0")).await?;
    let current = Database::open_local(s.0.join("current.db")).await?;
    let path = s.0.join("historical.db");
    let old = predecessor(&current.connect().await?, &path).await?;
    let c = old.connect()?;
    seed_historical(&c, &bytes, &source_path).await?;
    let archive = dump(
        &c,
        "SELECT * FROM snapshot_records ORDER BY source_table,ordinal",
    )
    .await?;
    let mapping = dump(&c, "SELECT * FROM snapshot_mappings ORDER BY rowid").await?;
    drop(c);
    drop(old);
    let db = Database::open_local(&path).await?;
    let c = db.connect().await?;
    // An edited preexisting value must not be overwritten during first-time activation.
    c.execute("UPDATE series SET runtime=11", ()).await?;
    let edited = snapshots::import(&db, Application::Sonarr, bytes.clone(), false).await?;
    assert!(!edited.applied);
    assert!(edited.conflicts > 0);
    assert_eq!(scalar(&c, "SELECT runtime FROM series").await?, 11);
    assert_eq!(
        scalar(&c, "SELECT series_runtime_version FROM snapshot_imports").await?,
        0
    );
    c.execute("UPDATE series SET runtime=NULL", ()).await?;
    // An absent provenance link cannot turn a matching local series into a backfill target.
    c.execute("DELETE FROM snapshot_mappings", ()).await?;
    let unlinked = snapshots::import(&db, Application::Sonarr, bytes.clone(), false).await?;
    assert!(!unlinked.applied);
    assert!(unlinked.conflicts > 0);
    c.execute(
        "INSERT INTO snapshot_mappings VALUES('sonarr',?,'series',7,70)",
        [fingerprint(&bytes)],
    )
    .await?;
    let dry = snapshots::import(&db, Application::Sonarr, bytes.clone(), true).await?;
    assert!(!dry.applied);
    assert_eq!(dry.metadata_backfilled, 1);
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM series WHERE runtime IS NULL").await?,
        1
    );
    assert_eq!(
        scalar(&c, "SELECT series_runtime_version FROM snapshot_imports").await?,
        0
    );
    // Fail after the runtime UPDATE, activation flag and archive loop: the outer transaction owns all of them.
    c.execute_batch("CREATE TRIGGER fail_runtime_finish BEFORE UPDATE OF profile_version ON snapshot_imports BEGIN SELECT RAISE(ABORT,'fixture late failure'); END;").await?;
    assert!(
        snapshots::import(&db, Application::Sonarr, bytes.clone(), false)
            .await
            .is_err()
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM series WHERE runtime IS NULL").await?,
        1
    );
    assert_eq!(
        scalar(&c, "SELECT series_runtime_version FROM snapshot_imports").await?,
        0
    );
    assert_eq!(
        archive,
        dump(
            &c,
            "SELECT * FROM snapshot_records ORDER BY source_table,ordinal"
        )
        .await?
    );
    c.execute("DROP TRIGGER fail_runtime_finish", ()).await?;
    let applied = snapshots::import(&db, Application::Sonarr, bytes.clone(), false).await?;
    assert!(applied.applied);
    assert_eq!(applied.metadata_backfilled, 1);
    assert_eq!(
        scalar(&c, "SELECT runtime FROM series WHERE id=70").await?,
        0
    );
    assert_eq!(
        scalar(&c, "SELECT series_runtime_version FROM snapshot_imports").await?,
        1
    );
    assert_eq!(
        archive,
        dump(
            &c,
            "SELECT * FROM snapshot_records ORDER BY source_table,ordinal"
        )
        .await?
    );
    assert_eq!(
        mapping,
        dump(
            &c,
            "SELECT * FROM snapshot_mappings WHERE destination_table='series' ORDER BY rowid"
        )
        .await?
    );
    let repeat = snapshots::import(&db, Application::Sonarr, bytes.clone(), false).await?;
    assert!(repeat.applied);
    assert_eq!(repeat.metadata_backfilled, 0);
    // A later deliberate clear is an edit, not permission to backfill a second time.
    c.execute("UPDATE series SET runtime=NULL", ()).await?;
    let cleared = snapshots::import(&db, Application::Sonarr, bytes, false).await?;
    assert!(!cleared.applied);
    assert!(cleared.conflicts > 0);
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM series WHERE runtime IS NULL").await?,
        1
    );
    Ok(())
}
