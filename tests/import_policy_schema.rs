//! Actual55 migration and private snapshot activation; no media effects are exercised.
use hrrdarr::{
    db::{Database, Error},
    snapshots::{self, Application},
};
use libsql::{Connection, Value, params};
use std::path::{Path, PathBuf};
const MIGRATION: &str = include_str!("../migrations/0056_import_policy.sql");
static IMPORT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let p =
            std::env::temp_dir().join(format!("hrrdarr-import-policy-{}", uuid::Uuid::new_v4()));
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
        // Raw historical builders must meet the real private-archive precondition too.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    c.execute_batch("PRAGMA foreign_keys=OFF;CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY,name TEXT NOT NULL,checksum TEXT NOT NULL,sql TEXT NOT NULL,applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);").await?;
    let tx = c.transaction().await?;
    let mut rows=source.query("SELECT version,name,checksum,sql FROM schema_migrations WHERE version<=55 ORDER BY version",()).await?;
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
    assert_eq!(n, 55);
    tx.commit().await?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    Ok(db)
}
async fn source(
    s: &Scratch,
    name: &str,
    app: Application,
    modern: bool,
    config: &str,
) -> Result<Vec<u8>, Error> {
    let p = s.0.join(name);
    let db = libsql::Builder::new_local(&p).build().await?;
    let c = db.connect()?;
    match app {
        Application::Sonarr=>c.execute_batch("CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES(233);CREATE TABLE Series(Id INTEGER,TvdbId INTEGER,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,Seasons TEXT);CREATE TABLE Episodes(Id INTEGER,SeriesId INTEGER,SeasonNumber INTEGER,EpisodeNumber INTEGER,Title TEXT,Monitored INTEGER,EpisodeFileId INTEGER);CREATE TABLE EpisodeFiles(Id INTEGER,SeriesId INTEGER,RelativePath TEXT);").await?,
        Application::Radarr if modern=>c.execute_batch("CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES(242);CREATE TABLE Movies(Id INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER,MovieMetadataId INTEGER);CREATE TABLE MovieMetadata(Id INTEGER,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER);CREATE TABLE MovieFiles(Id INTEGER,MovieId INTEGER,RelativePath TEXT,Edition TEXT);").await?,
        Application::Radarr=>c.execute_batch("CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES(206);CREATE TABLE Movies(Id INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER);CREATE TABLE MovieFiles(Id INTEGER,MovieId INTEGER,RelativePath TEXT,Edition TEXT);").await?,
    };
    // No affinity: hostile numeric/blob values must reach the actual source decoder unchanged.
    c.execute_batch("CREATE TABLE Config(Id INTEGER,Key TEXT,Value);")
        .await?;
    c.execute_batch(config).await?;
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
async fn archive_historical(c: &Connection, bytes: &[u8], source_path: &Path) -> Result<(), Error> {
    let fp = fingerprint(bytes);
    c.execute("INSERT INTO snapshot_imports(application,fingerprint,schema_version,series_runtime_version)VALUES('sonarr',?,233,1)",[fp.clone()]).await?;
    let rawdb = libsql::Builder::new_local(source_path).build().await?;
    let raw = rawdb.connect()?;
    for table in [
        "VersionInfo",
        "Series",
        "Episodes",
        "EpisodeFiles",
        "Config",
    ] {
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
                    _ => return Err("unexpected historical fixture value".into()),
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
async fn schema56_actual55_rollback_backup_reopen_and_constraints() -> Result<(), Error> {
    let s = Scratch::new();
    let fresh = Database::open_local(s.0.join("fresh.db")).await?;
    let fc = fresh.connect().await?;
    assert_eq!(
        scalar(&fc, "SELECT max(version) FROM schema_migrations").await?,
        68 // Reasoning: latest is migration file 0068 torrent_rss_indexer at position 68, appended after 0067 rename_commands; historical prefixes stay fixed.
    );
    let defaults = dump(&fc, "SELECT * FROM import_policies ORDER BY media_type").await?;
    assert_eq!(defaults.len(), 2);
    for (row, domain) in defaults.iter().zip(["movies", "tv"]) {
        assert_eq!(
            row,
            &vec![
                Value::Text(domain.into()),
                Value::Integer(0),
                Value::Integer(100),
                Value::Text("_UNPACK_|_FAILED_".into()),
                Value::Integer(1),
                Value::Integer(0)
            ]
        );
    }
    let path = s.0.join("old.db");
    let old = predecessor(&fc, &path).await?;
    let c = old.connect()?;
    c.execute_batch("INSERT INTO series(id,title,path,runtime)VALUES(1,'TV','/tv',0);INSERT INTO seasons(series_id,number)VALUES(1,1);INSERT INTO episodes(id,series_id,season,number,title)VALUES(1,1,1,1,'Episode');INSERT INTO movie_metadata(id,title)VALUES(1,'Film');INSERT INTO movies(id,metadata_id,path)VALUES(1,1,'/movies');INSERT INTO snapshot_imports(application,fingerprint,schema_version,series_runtime_version)VALUES('sonarr','old',233,1);INSERT INTO snapshot_records VALUES('sonarr','old','Config',0,'{\"private\":[\"text\",\"retained\"]}');").await?;
    let sql = dump(&c, "SELECT type,name,sql FROM sqlite_schema ORDER BY name").await?;
    let raw = dump(&c, "SELECT * FROM snapshot_records ORDER BY rowid").await?;
    let health = dump(&c, "SELECT * FROM health_checks ORDER BY check_key,scope").await?;
    let tx = c.transaction().await?;
    tx.execute_batch(MIGRATION).await?;
    tx.execute(
        "UPDATE import_policies SET minimum_free_space_mib=200 WHERE media_type='tv'",
        (),
    )
    .await?;
    assert!(
        tx.execute("UPDATE import_policies SET minimum_free_space_mib=99", ())
            .await
            .is_err()
    );
    tx.rollback().await?;
    assert_eq!(
        sql,
        dump(&c, "SELECT type,name,sql FROM sqlite_schema ORDER BY name").await?
    );
    assert_eq!(
        raw,
        dump(&c, "SELECT * FROM snapshot_records ORDER BY rowid").await?
    );
    drop(c);
    drop(old);
    let db = Database::open_local(&path).await?;
    let backup = db.migration_backup().ok_or("backup missing")?;
    let copied = s.0.join("backup-copy.db");
    std::fs::copy(backup.join("database.db"), &copied)?;
    let backup_db = libsql::Builder::new_local(&copied).build().await?;
    let bc = backup_db.connect()?;
    assert_eq!(
        sql,
        dump(&bc, "SELECT type,name,sql FROM sqlite_schema ORDER BY name").await?
    );
    assert_eq!(
        raw,
        dump(&bc, "SELECT * FROM snapshot_records ORDER BY rowid").await?
    );
    let c = db.connect().await?;
    assert_eq!(
        defaults,
        dump(&c, "SELECT * FROM import_policies ORDER BY media_type").await?
    );
    // Reasoning: 0060 registers recycle_bin identities after 0056; every predecessor row stays exact.
    assert_eq!(
        health,
        dump(
            &c,
            "SELECT * FROM health_checks WHERE check_key!='recycle_bin' ORDER BY check_key,scope"
        )
        .await?
    );
    assert_eq!(
        scalar(&c, "SELECT import_policy_version FROM snapshot_imports").await?,
        0
    );
    assert_eq!(
        scalar(&c, "SELECT runtime FROM series WHERE id=1").await?,
        0
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM episodes WHERE id=1").await?,
        1
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM movies WHERE id=1").await?,
        1
    );
    for (column, values) in [
        (
            "skip_free_space_check_when_importing",
            vec!["NULL", "-1", "2", "1.5", "X'00'"],
        ),
        (
            "minimum_free_space_mib",
            vec!["NULL", "99", "2147483648", "100.5", "'invalid'", "X'00'"],
        ),
        ("revision", vec!["NULL", "0", "9007199254740992", "1.5"]),
        ("locally_edited", vec!["NULL", "-1", "2", "1.5"]),
        (
            "download_client_working_folders",
            vec!["NULL", "''", "X'61'", "printf('%4097s','x')"],
        ),
    ] {
        for value in values {
            assert!(
                c.execute(&format!("UPDATE import_policies SET {column}={value}"), ())
                    .await
                    .is_err(),
                "{column}={value}"
            );
        }
    }
    assert!(
        c.execute("INSERT INTO import_policies(media_type)VALUES('tv')", ())
            .await
            .is_err()
    );
    assert!(
        c.execute("INSERT INTO import_policies(media_type)VALUES(NULL)", ())
            .await
            .is_err()
    );
    assert!(
        c.execute(
            "INSERT INTO import_policies(media_type)VALUES('episode')",
            ()
        )
        .await
        .is_err()
    );
    for v in ["NULL", "-1", "2", "0.5"] {
        assert!(
            c.execute(
                &format!("UPDATE snapshot_imports SET import_policy_version={v}"),
                ()
            )
            .await
            .is_err()
        );
    }
    let unicode = "é".repeat(2048);
    c.execute("UPDATE import_policies SET download_client_working_folders=?,minimum_free_space_mib=2147483647,revision=9007199254740991 WHERE media_type='tv'",[unicode.clone()]).await?;
    assert!(
        c.execute(
            "UPDATE import_policies SET download_client_working_folders=? WHERE media_type='tv'",
            [format!("{unicode}x")]
        )
        .await
        .is_err()
    );
    let kept = dump(&c, "SELECT * FROM import_policies ORDER BY media_type").await?;
    assert_eq!(
        raw,
        dump(&c, "SELECT * FROM snapshot_records ORDER BY rowid").await?
    );
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
        kept,
        dump(
            &reopened.connect().await?,
            "SELECT * FROM import_policies ORDER BY media_type"
        )
        .await?
    );
    Ok(())
}

#[tokio::test]
async fn snapshots_policy_defaults_supported_shapes_and_whole_policy_rejection() -> Result<(), Error>
{
    let _guard = IMPORT_LOCK.lock().await;
    let s = Scratch::new();
    for (i, (app, modern)) in [
        (Application::Sonarr, false),
        (Application::Radarr, false),
        (Application::Radarr, true),
    ]
    .into_iter()
    .enumerate()
    {
        for (j,config) in ["","INSERT INTO Config VALUES(1,'skipfreespacecheckwhenimporting',NULL),(2,'minimumfreespacewhenimporting',NULL),(3,'downloadclientworkingfolders',NULL);","INSERT INTO Config VALUES(1,'skipfreespacecheckwhenimporting',''),(2,'minimumfreespacewhenimporting',''),(3,'downloadclientworkingfolders','');"].into_iter().enumerate() {
            let bytes=source(&s,&format!("default{i}-{j}.db"),app,modern,config).await?;
            let db=Database::open_local(s.0.join(format!("default-target{i}-{j}.db"))).await?;
            assert!(snapshots::import(&db,app,bytes,false).await?.applied);
            let c=db.connect().await?;
            assert_eq!(scalar(&c,"SELECT import_policy_version FROM snapshot_imports").await?,1);
            assert_eq!(scalar(&c,"SELECT count(*) FROM import_policies WHERE revision=1 AND minimum_free_space_mib=100 AND skip_free_space_check_when_importing=0 AND download_client_working_folders='_UNPACK_|_FAILED_' AND locally_edited=0").await?,2);
        }
    }
    for modern in [false, true] {
        let bytes = source(&s, &format!("movie-supported-{modern}.db"), Application::Radarr, modern,
            "INSERT INTO Config VALUES(1,'skipfreespacecheckwhenimporting','true'),(2,'minimumfreespacewhenimporting','201'),(3,'downloadclientworkingfolders',' lead ||尾|'||char(9)||char(10)||char(0));").await?;
        let target =
            Database::open_local(s.0.join(format!("movie-supported-target-{modern}.db"))).await?;
        let c = target.connect().await?;
        assert!(
            snapshots::import(&target, Application::Radarr, bytes.clone(), false)
                .await?
                .applied
        );
        assert_eq!(
            scalar(&c, "SELECT import_policy_version FROM snapshot_imports").await?,
            1
        );
        // Use bytes at both boundaries: libSQL's generic TEXT getter truncates NUL.
        let expected = " lead ||尾|\t\n\0";
        let source_db =
            libsql::Builder::new_local(s.0.join(format!("movie-supported-{modern}.db")))
                .build()
                .await?;
        let source_c = source_db.connect()?;
        let original: Vec<u8> = source_c
            .query(
                "SELECT CAST(Value AS BLOB) FROM Config WHERE Key='downloadclientworkingfolders'",
                (),
            )
            .await?
            .next()
            .await?
            .ok_or("missing source folders")?
            .get(0)?;
        assert_eq!(original, expected.as_bytes());
        let folders: Vec<u8> = c.query("SELECT CAST(download_client_working_folders AS BLOB) FROM import_policies WHERE media_type='movies'",()).await?.next().await?.ok_or("missing movie policy")?.get(0)?;
        assert_eq!(folders, original);
        assert_eq!(
            hrrdarr::import_policy::read(&c, hrrdarr::api::MediaDomain::Movies)
                .await
                .map_err(|_| "native import policy read failed")?
                .download_client_working_folders,
            expected
        );
        let archived: String = c.query("SELECT record_json FROM snapshot_records WHERE source_table='Config' AND ordinal=2",()).await?.next().await?.ok_or("missing archived folders")?.get(0)?;
        let archived: serde_json::Value = serde_json::from_str(&archived)?;
        assert_eq!(archived["Value"], serde_json::json!(["text", expected]));
        assert_eq!(scalar(&c,"SELECT count(*) FROM import_policies WHERE media_type='movies' AND revision=2 AND minimum_free_space_mib=201 AND skip_free_space_check_when_importing=1 AND locally_edited=0").await?,1);
        assert_eq!(scalar(&c,"SELECT count(*) FROM import_policies WHERE media_type='tv' AND revision=1 AND minimum_free_space_mib=100 AND skip_free_space_check_when_importing=0 AND download_client_working_folders='_UNPACK_|_FAILED_' AND locally_edited=0").await?,1);
        let retained = dump(&c, "SELECT * FROM snapshot_records ORDER BY rowid").await?;
        assert!(
            snapshots::import(&target, Application::Radarr, bytes, false)
                .await?
                .applied
        );
        assert_eq!(
            scalar(
                &c,
                "SELECT revision FROM import_policies WHERE media_type='movies'"
            )
            .await?,
            2
        );
        assert_eq!(
            retained,
            dump(&c, "SELECT * FROM snapshot_records ORDER BY rowid").await?
        );
    }
    let config = "INSERT INTO Config VALUES(1,'SkipFreeSpaceCheckWhenImporting',' True '),(2,'MinimumFreeSpaceWhenImporting','2147483647'),(3,'DownloadClientWorkingFolders',' keep ||尾| '),(4,'PrivateUnknown','PRIVATE_POLICY_SENTINEL');";
    let bytes = source(&s, "supported.db", Application::Sonarr, false, config).await?;
    let db = Database::open_local(s.0.join("supported-target.db")).await?;
    let c = db.connect().await?;
    let before = dump(&c, "SELECT * FROM import_policies ORDER BY media_type").await?;
    assert!(
        !snapshots::import(&db, Application::Sonarr, bytes.clone(), true)
            .await?
            .applied
    );
    assert_eq!(
        before,
        dump(&c, "SELECT * FROM import_policies ORDER BY media_type").await?
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM snapshot_records").await?,
        0
    );
    let report = snapshots::import(&db, Application::Sonarr, bytes.clone(), false).await?;
    assert!(report.applied);
    assert!(!serde_json::to_string(&report)?.contains("PRIVATE_POLICY_SENTINEL"));
    assert_eq!(scalar(&c,"SELECT count(*) FROM snapshot_records WHERE record_json LIKE '%PRIVATE_POLICY_SENTINEL%'").await?,1);
    assert_eq!(scalar(&c,"SELECT count(*) FROM import_policies WHERE media_type='tv' AND revision=2 AND skip_free_space_check_when_importing=1 AND minimum_free_space_mib=2147483647 AND download_client_working_folders=' keep ||尾| ' AND locally_edited=0").await?,1);
    assert_eq!(
        scalar(
            &c,
            "SELECT revision FROM import_policies WHERE media_type='movies'"
        )
        .await?,
        1
    );
    let retained = dump(&c, "SELECT * FROM snapshot_records ORDER BY rowid").await?;
    assert!(
        snapshots::import(&db, Application::Sonarr, bytes, false)
            .await?
            .applied
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT revision FROM import_policies WHERE media_type='tv'"
        )
        .await?,
        2
    );
    assert_eq!(
        retained,
        dump(&c, "SELECT * FROM snapshot_records ORDER BY rowid").await?
    );
    for (i, invalid) in [
        "(2,'minimumfreespacewhenimporting','99')",
        "(2,'minimumfreespacewhenimporting','2147483648')",
        "(2,'minimumfreespacewhenimporting',100)",
        "(2,'minimumfreespacewhenimporting','100.5')",
        "(2,'downloadclientworkingfolders',X'61')",
        "(2,'downloadclientworkingfolders',printf('%4097s','x'))",
        "(2,'skipfreespacecheckwhenimporting','PRIVATE_POLICY_SENTINEL')",
    ]
    .into_iter()
    .enumerate()
    {
        let config = format!(
            "INSERT INTO Config VALUES(1,'minimumfreespacewhenimporting','200'),{invalid};"
        );
        // Avoid duplicate recognized keys except the explicit duplicate test below.
        let config = if invalid.contains("minimumfreespace") {
            format!(
                "INSERT INTO Config VALUES(1,'skipfreespacecheckwhenimporting','true'),{invalid};"
            )
        } else {
            config
        };
        let bytes = source(
            &s,
            &format!("bad{i}.db"),
            Application::Sonarr,
            false,
            &config,
        )
        .await?;
        let target = Database::open_local(s.0.join(format!("bad-target{i}.db"))).await?;
        let report = snapshots::import(&target, Application::Sonarr, bytes, false).await?;
        assert!(report.applied);
        assert!(report.unsupported.iter().any(|u| {
            u.columns
                .iter()
                .any(|v| v.contains("import policy has unsupported"))
        }));
        assert!(!serde_json::to_string(&report)?.contains("PRIVATE_POLICY_SENTINEL"));
        let c = target.connect().await?;
        assert_eq!(scalar(&c,"SELECT count(*) FROM import_policies WHERE revision=1 AND minimum_free_space_mib=100 AND skip_free_space_check_when_importing=0").await?,2);
        assert_eq!(
            scalar(&c, "SELECT import_policy_version FROM snapshot_imports").await?,
            0
        );
        assert_eq!(
            scalar(
                &c,
                "SELECT count(*) FROM snapshot_records WHERE source_table='Config'"
            )
            .await?,
            2
        );
    }
    let duplicate=source(&s,"duplicate.db",Application::Sonarr,false,"INSERT INTO Config VALUES(1,'downloadclientworkingfolders','x'),(2,'DownloadClientWorkingFolders','x');").await?;
    let target = Database::open_local(s.0.join("duplicate-target.db")).await?;
    assert!(
        snapshots::import(&target, Application::Sonarr, duplicate, false)
            .await
            .is_err()
    );
    assert_eq!(
        scalar(
            &target.connect().await?,
            "SELECT count(*) FROM snapshot_imports"
        )
        .await?,
        0
    );
    Ok(())
}

#[tokio::test]
async fn historical_policy_exact_archive_activation_local_edit_and_late_rollback()
-> Result<(), Error> {
    let _guard = IMPORT_LOCK.lock().await;
    let s = Scratch::new();
    let bytes=source(&s,"source.db",Application::Sonarr,false,"INSERT INTO Config VALUES(1,'skipfreespacecheckwhenimporting','true'),(2,'minimumfreespacewhenimporting','200'),(3,'downloadclientworkingfolders','x||y'),(4,'unknown','PRIVATE_POLICY_SENTINEL');").await?;
    let latest = Database::open_local(s.0.join("latest.db")).await?;
    let path = s.0.join("historical.db");
    let old = predecessor(&latest.connect().await?, &path).await?;
    let c = old.connect()?;
    archive_historical(&c, &bytes, &s.0.join("source.db")).await?;
    let retained = dump(
        &c,
        "SELECT * FROM snapshot_records ORDER BY source_table,ordinal",
    )
    .await?;
    drop(c);
    drop(old);
    let db = Database::open_local(&path).await?;
    let c = db.connect().await?;
    c.execute(
        "UPDATE import_policies SET locally_edited=1 WHERE media_type='tv'",
        (),
    )
    .await?;
    let conflict = snapshots::import(&db, Application::Sonarr, bytes.clone(), false).await?;
    assert!(!conflict.applied && conflict.conflicts > 0);
    assert_eq!(
        scalar(&c, "SELECT import_policy_version FROM snapshot_imports").await?,
        0
    );
    // Equal native intent also blocks provenance acquisition, without blocking unrelated import work.
    c.execute("UPDATE import_policies SET skip_free_space_check_when_importing=1,minimum_free_space_mib=200,download_client_working_folders='x||y' WHERE media_type='tv'",()).await?;
    assert!(
        snapshots::import(&db, Application::Sonarr, bytes.clone(), false)
            .await?
            .applied
    );
    assert_eq!(
        scalar(&c, "SELECT import_policy_version FROM snapshot_imports").await?,
        0
    );
    // Restore untouched defaults to exercise eligible historical activation.
    c.execute("UPDATE import_policies SET locally_edited=0,skip_free_space_check_when_importing=0,minimum_free_space_mib=100,download_client_working_folders='_UNPACK_|_FAILED_' WHERE media_type='tv'",()).await?;
    assert!(
        !snapshots::import(&db, Application::Sonarr, bytes.clone(), true)
            .await?
            .applied
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT revision FROM import_policies WHERE media_type='tv'"
        )
        .await?,
        1
    );
    assert_eq!(
        scalar(&c, "SELECT import_policy_version FROM snapshot_imports").await?,
        0
    );
    // This executes after the policy UPDATE; all activation writes share the snapshot transaction.
    c.execute_batch("CREATE TRIGGER fail_policy_marker BEFORE UPDATE OF import_policy_version ON snapshot_imports BEGIN SELECT RAISE(ABORT,'fixture late failure'); END;").await?;
    assert!(
        snapshots::import(&db, Application::Sonarr, bytes.clone(), false)
            .await
            .is_err()
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT revision FROM import_policies WHERE media_type='tv'"
        )
        .await?,
        1
    );
    assert_eq!(
        scalar(&c, "SELECT import_policy_version FROM snapshot_imports").await?,
        0
    );
    assert_eq!(
        retained,
        dump(
            &c,
            "SELECT * FROM snapshot_records ORDER BY source_table,ordinal"
        )
        .await?
    );
    c.execute("DROP TRIGGER fail_policy_marker", ()).await?;
    assert!(
        snapshots::import(&db, Application::Sonarr, bytes.clone(), false)
            .await?
            .applied
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT revision FROM import_policies WHERE media_type='tv'"
        )
        .await?,
        2
    );
    assert_eq!(
        scalar(&c, "SELECT import_policy_version FROM snapshot_imports").await?,
        1
    );
    assert!(
        snapshots::import(&db, Application::Sonarr, bytes.clone(), false)
            .await?
            .applied
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT revision FROM import_policies WHERE media_type='tv'"
        )
        .await?,
        2
    );
    c.execute("UPDATE import_policies SET minimum_free_space_mib=300,locally_edited=1,revision=revision+1 WHERE media_type='tv'",()).await?;
    let conflict = snapshots::import(&db, Application::Sonarr, bytes, false).await?;
    assert!(!conflict.applied && conflict.conflicts > 0);
    assert_eq!(
        scalar(
            &c,
            "SELECT minimum_free_space_mib FROM import_policies WHERE media_type='tv'"
        )
        .await?,
        300
    );
    assert_eq!(
        retained,
        dump(
            &c,
            "SELECT * FROM snapshot_records ORDER BY source_table,ordinal"
        )
        .await?
    );
    Ok(())
}

#[tokio::test]
async fn snapshot_raw_cells_preserve_types_and_reject_invalid_or_hidden_oversize_text()
-> Result<(), Error> {
    let _guard = IMPORT_LOCK.lock().await;
    let s = Scratch::new();
    let bytes = source(&s,"typed-source.db",Application::Sonarr,false,
        "CREATE TABLE ArchiveProbe(NullValue,IntegerValue,RealValue,TextValue,BlobValue,EmptyText,EmptyBlob);INSERT INTO ArchiveProbe VALUES(NULL,-9223372036854775808,1.25,'PRIVATE_NUL_SENTINEL'||char(0)||'tail',X'0080FF','',X'');").await?;
    let db = Database::open_local(s.0.join("typed-target.db")).await?;
    let report = snapshots::import(&db, Application::Sonarr, bytes.clone(), false).await?;
    assert!(report.applied);
    assert!(!serde_json::to_string(&report)?.contains("PRIVATE_NUL_SENTINEL"));
    let c = db.connect().await?;
    let raw: String = c
        .query(
            "SELECT record_json FROM snapshot_records WHERE source_table='ArchiveProbe'",
            (),
        )
        .await?
        .next()
        .await?
        .ok_or("missing typed archive")?
        .get(0)?;
    let decoded: serde_json::Value = serde_json::from_str(&raw)?;
    assert_eq!(
        decoded,
        serde_json::json!({
            "NullValue":["null"], "IntegerValue":["integer",i64::MIN],
            "RealValue":["real",1.25], "TextValue":["text","PRIVATE_NUL_SENTINEL\0tail"],
            "BlobValue":["blob_hex","0080ff"], "EmptyText":["text",""], "EmptyBlob":["blob_hex",""]
        })
    );
    let retained = dump(&c, "SELECT * FROM snapshot_records ORDER BY rowid").await?;
    assert!(
        snapshots::import(&db, Application::Sonarr, bytes, false)
            .await?
            .applied
    );
    assert_eq!(
        retained,
        dump(&c, "SELECT * FROM snapshot_records ORDER BY rowid").await?
    );

    for (i, (value, expected_error)) in [
        ("CAST(X'610080' AS TEXT)", "invalid UTF-8 source text"),
        (
            "char(0)||printf('%1048576s','x')",
            "snapshot exceeds cell limit",
        ),
        ("zeroblob(1048577)", "snapshot exceeds cell limit"),
        ("1e999", "unsupported non-finite source value"),
    ]
    .into_iter()
    .enumerate()
    {
        let bytes = source(
            &s,
            &format!("invalid-cell-{i}.db"),
            Application::Sonarr,
            false,
            &format!("CREATE TABLE ArchiveProbe(Value);INSERT INTO ArchiveProbe VALUES({value});"),
        )
        .await?;
        let target = Database::open_local(s.0.join(format!("invalid-cell-target-{i}.db"))).await?;
        let error = snapshots::import(&target, Application::Sonarr, bytes, false)
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), expected_error);
        let c = target.connect().await?;
        assert_eq!(
            scalar(&c, "SELECT count(*) FROM snapshot_records").await?,
            0
        );
        assert_eq!(
            scalar(&c, "SELECT count(*) FROM snapshot_imports").await?,
            0
        );
        assert_eq!(
            scalar(
                &c,
                "SELECT count(*) FROM import_policies WHERE revision=1 AND locally_edited=0"
            )
            .await?,
            2
        );
    }
    // The original inclusive cell limit still accepts a complete one-MiB text value.
    let bytes=source(&s,"cell-boundary.db",Application::Sonarr,false,
        "CREATE TABLE ArchiveProbe(Value);INSERT INTO ArchiveProbe VALUES(char(0)||printf('%1048575s','x'));").await?;
    let target = Database::open_local(s.0.join("cell-boundary-target.db")).await?;
    assert!(
        snapshots::import(&target, Application::Sonarr, bytes, false)
            .await?
            .applied
    );
    let raw: String = target
        .connect()
        .await?
        .query(
            "SELECT record_json FROM snapshot_records WHERE source_table='ArchiveProbe'",
            (),
        )
        .await?
        .next()
        .await?
        .ok_or("missing boundary archive")?
        .get(0)?;
    let decoded: serde_json::Value = serde_json::from_str(&raw)?;
    let value = decoded["Value"][1]
        .as_str()
        .ok_or("missing boundary text")?;
    assert_eq!(value.len(), 1024 * 1024);
    assert_eq!(value.as_bytes()[0], 0);
    assert_eq!(value.as_bytes().last(), Some(&b'x'));
    Ok(())
}
