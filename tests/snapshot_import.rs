use hrrdarr::{
    db::{Database, Error},
    snapshots::{self, Application},
};
use libsql::{Connection, params};
use std::path::PathBuf;

static IMPORT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Self {
        let p =
            std::env::temp_dir().join(format!("hrrdarr-snapshot-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
async fn count(c: &Connection, sql: &str) -> i64 {
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
async fn fixture(files: &Sandbox, name: &str, sql: &str) -> Vec<u8> {
    let p = files.0.join(name);
    let db = libsql::Builder::new_local(&p).build().await.unwrap();
    let c = db.connect().unwrap();
    c.execute_batch(sql).await.unwrap();
    drop(c);
    drop(db);
    std::fs::read(p).unwrap()
}
// Contract fields from pinned Sonarr233 models and Radarr206/207/242 schema boundary.
// Synthetic fixtures deliberately use colliding IDs and a different MovieMetadataId.
const TV: &str = r#"
CREATE TABLE VersionInfo (Version INTEGER); INSERT INTO VersionInfo VALUES(233);
CREATE TABLE Series (Id INTEGER PRIMARY KEY,TvdbId INTEGER,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,Seasons TEXT,QualityProfileId INTEGER,Tags TEXT);
INSERT INTO Series VALUES(7,777,'TV',2020,'/tv/TV',0,'[{"seasonNumber":0,"monitored":false},{"seasonNumber":1,"monitored":true}]',9,'[4]');
CREATE TABLE Episodes (Id INTEGER PRIMARY KEY,SeriesId INTEGER,SeasonNumber INTEGER,EpisodeNumber INTEGER,Title TEXT,Monitored INTEGER,EpisodeFileId INTEGER);
INSERT INTO Episodes VALUES(1,7,0,1,'Special',0,11),(2,7,0,2,'Pack',1,11),(3,7,1,1,'No file',1,0),(4,7,1,2,'Missing file record',1,99),(5,7,1,3,'Null file',1,NULL);
CREATE TABLE EpisodeFiles (Id INTEGER PRIMARY KEY,SeriesId INTEGER,RelativePath TEXT,Quality TEXT);
INSERT INTO EpisodeFiles VALUES(11,7,'Specials/pack.mkv','{"quality":7}');
CREATE INDEX quality_json ON EpisodeFiles(json_extract(Quality,'$.quality'));
CREATE TABLE Config (Id INTEGER,Key TEXT,Value TEXT);
INSERT INTO Config VALUES(1,'private-setting','SENTINEL_SECRET');
CREATE TABLE DownloadClients (Id INTEGER,Settings TEXT); INSERT INTO DownloadClients VALUES(1,'{"password":"SENTINEL_SECRET"}');
CREATE TABLE UnknownState (Id INTEGER,Opaque BLOB); INSERT INTO UnknownState VALUES(1,X'00FF');
"#;
const MOVIES: &str = r#"
CREATE TABLE VersionInfo (Version INTEGER); INSERT INTO VersionInfo VALUES(242);
CREATE TABLE MovieMetadata (Id INTEGER PRIMARY KEY,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER,CollectionTmdbId INTEGER);
INSERT INTO MovieMetadata VALUES(101,1001,'tt1001','Film',2024,88),(102,1002,NULL,'Catalog only',2025,NULL),(103,1003,'','Missing',2023,NULL),(104,1004,NULL,'No file',2022,NULL);
CREATE TABLE Movies (Id INTEGER PRIMARY KEY,MovieMetadataId INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER,QualityProfileId INTEGER);
INSERT INTO Movies VALUES(1,101,'/movies/Film',0,11,9),(2,103,'/movies/Missing',1,99,9),(3,104,'/movies/No file',1,0,9);
CREATE TABLE MovieFiles (Id INTEGER PRIMARY KEY,MovieId INTEGER,RelativePath TEXT,Edition TEXT);
INSERT INTO MovieFiles VALUES(11,1,'Film.mkv','Extended');
CREATE TABLE Collections (Id INTEGER,TmdbId INTEGER,Title TEXT); INSERT INTO Collections VALUES(1,88,'Collection');
"#;
const OLD_MOVIES: &str = r#"
CREATE TABLE VersionInfo (Version INTEGER); INSERT INTO VersionInfo VALUES(206);
CREATE TABLE Movies (Id INTEGER PRIMARY KEY,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER);
INSERT INTO Movies VALUES(1,9001,'tt9001','Older',2001,'/movies/Older',1,11);
CREATE TABLE MovieFiles (Id INTEGER PRIMARY KEY,MovieId INTEGER,RelativePath TEXT,Edition TEXT);
INSERT INTO MovieFiles VALUES(11,1,'Older.mkv',NULL);
"#;

// Keep these dependent reconciliation scenarios sequential; concurrency is not asserted here.
#[tokio::test]
async fn both_snapshot_adapters_reconcile_without_data_loss_or_secret_disclosure()
-> Result<(), Error> {
    let _guard = IMPORT_LOCK.lock().await;
    let files = Sandbox::new();
    let tv = fixture(&files, "sonarr.db", TV).await;
    let films = fixture(&files, "radarr.db", MOVIES).await;
    let old = fixture(&files, "old.db", OLD_MOVIES).await;
    let db = Database::open_local(files.0.join("library.db")).await?;
    let c = db.connect().await?;
    c.execute_batch("INSERT INTO series(id,tvdb_id,title,path) VALUES(1,123,'Existing','/existing'); INSERT INTO seasons VALUES(1,1,1); INSERT INTO episodes(id,series_id,season,number,title) VALUES(1,1,1,1,'Existing');").await?;
    let preview = snapshots::import(&db, Application::Sonarr, tv.clone(), true).await?;
    assert!(!preview.applied);
    assert!(preview.mapped > 0);
    assert_eq!(preview.missing_file_records, 1);
    assert_eq!(count(&c, "SELECT count(*) FROM series").await, 1);
    assert_eq!(count(&c, "SELECT count(*) FROM snapshot_imports").await, 0);
    let result = snapshots::import(&db, Application::Sonarr, tv.clone(), false).await?;
    assert!(result.applied);
    assert_eq!(result.conflicts, 0);
    assert!(!serde_json::to_string(&result)?.contains("SENTINEL_SECRET"));
    assert!(
        result
            .unsupported
            .iter()
            .any(|u| u.table == "Series" && u.columns.contains(&"QualityProfileId".to_string()))
    );
    assert!(
        result
            .unsupported
            .iter()
            .any(|u| u.table == "DownloadClients")
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM episodes WHERE episode_file_id IS NOT NULL"
        )
        .await,
        2
    );
    assert_eq!(count(&c,"SELECT count(*) FROM episodes e JOIN episode_files f ON f.id=e.episode_file_id WHERE f.path='/tv/TV/Specials/pack.mkv'").await,2);
    assert_eq!(
        count(&c, "SELECT monitored FROM series WHERE tvdb_id=777").await,
        0
    );
    assert_eq!(count(&c,"SELECT s.monitored FROM seasons s JOIN series t ON t.id=s.series_id WHERE t.tvdb_id=777 AND s.number=0").await,0);
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM snapshot_records WHERE record_json LIKE '%SENTINEL_SECRET%'"
        )
        .await,
        2
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM snapshot_records WHERE record_json LIKE '%blob_hex%00ff%'"
        )
        .await,
        1
    );
    let archive_count = count(&c, "SELECT count(*) FROM snapshot_records").await;
    let again = snapshots::import(&db, Application::Sonarr, tv.clone(), false).await?;
    assert!(again.applied);
    assert_eq!(again.mapped, 0);
    assert!(again.duplicates > 0);
    assert_eq!(
        count(&c, "SELECT count(*) FROM snapshot_records").await,
        archive_count
    );
    let film_preview = snapshots::import(&db, Application::Radarr, films.clone(), true).await?;
    assert!(!film_preview.applied);
    assert_eq!(count(&c, "SELECT count(*) FROM movies").await, 0);
    let film_report = snapshots::import(&db, Application::Radarr, films.clone(), false).await?;
    assert!(film_report.applied);
    assert_eq!(film_report.missing_file_records, 1);
    assert_eq!(count(&c, "SELECT count(*) FROM movies").await, 3);
    assert_eq!(count(&c, "SELECT count(*) FROM movie_metadata").await, 4);
    assert_eq!(count(&c,"SELECT count(*) FROM movie_files f JOIN movies m ON m.id=f.movie_id JOIN movie_metadata d ON d.id=m.metadata_id WHERE d.tmdb_id=1001 AND f.path='/movies/Film/Film.mkv' AND f.edition='Extended' AND m.monitored=0").await,1);
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM episodes WHERE id=1 AND title='Existing'"
        )
        .await,
        1
    );
    assert_eq!(count(&c,"SELECT count(*) FROM snapshot_mappings WHERE source_id=1 AND destination_table IN ('episodes','movies')").await,2);
    assert!(
        film_report
            .unsupported
            .iter()
            .any(|u| u.table == "Collections")
    );
    assert_eq!(
        snapshots::import(&db, Application::Radarr, films.clone(), false)
            .await?
            .mapped,
        0
    );
    assert!(
        snapshots::import(&db, Application::Radarr, old.clone(), false)
            .await?
            .applied
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM movie_files WHERE path='/movies/Older/Older.mkv'"
        )
        .await,
        1
    );
    assert_eq!(count(&c, "SELECT count(*) FROM snapshot_imports").await, 3);
    assert_eq!(tv, std::fs::read(files.0.join("sonarr.db"))?);
    assert_eq!(films, std::fs::read(files.0.join("radarr.db"))?);
    assert_eq!(old, std::fs::read(files.0.join("old.db"))?);

    // A changed destination cannot be silently considered an idempotent rerun.
    c.execute(
        "UPDATE episodes SET title='Local edit' WHERE title='Special'",
        (),
    )
    .await?;
    let changed = snapshots::import(&db, Application::Sonarr, tv.clone(), false).await?;
    assert!(!changed.applied);
    assert!(changed.conflicts > 0);
    assert_eq!(
        count(&c, "SELECT count(*) FROM episodes WHERE title='Local edit'").await,
        1
    );

    // Distinct snapshot identity, same domain identity: reconcile rather than overwrite.
    let conflict = fixture(
        &files,
        "conflict.db",
        &TV.replace("'TV',2020", "'Changed',2020"),
    )
    .await;
    let report = snapshots::import(&db, Application::Sonarr, conflict, false).await?;
    assert!(!report.applied);
    assert!(report.conflicts > 0);
    assert_eq!(count(&c, "SELECT count(*) FROM snapshot_imports").await, 3);
    assert_eq!(
        count(&c, "SELECT count(*) FROM series WHERE title='TV'").await,
        1
    );
    assert_eq!(
        count(&c, "SELECT count(*) FROM episodes WHERE title='Existing'").await,
        1
    );

    // Deliberate late destination failure: trigger aborts after earlier inserts; all rollback.
    let isolated = Database::open_local(files.0.join("rollback.db")).await?;
    let ic = isolated.connect().await?;
    ic.execute_batch("CREATE TRIGGER fail_episode BEFORE INSERT ON episodes BEGIN SELECT RAISE(ABORT,'SENTINEL_SECRET'); END;").await?;
    let error = snapshots::import(&isolated, Application::Sonarr, tv.clone(), false)
        .await
        .unwrap_err();
    assert!(!error.to_string().contains("SENTINEL_SECRET"));
    for sql in [
        "SELECT count(*) FROM series",
        "SELECT count(*) FROM episode_files",
        "SELECT count(*) FROM snapshot_imports",
        "SELECT count(*) FROM snapshot_records",
        "SELECT count(*) FROM snapshot_mappings",
    ] {
        assert_eq!(count(&ic, sql).await, 0);
    }
    ic.execute("DROP TRIGGER fail_episode", ()).await?;
    assert!(
        snapshots::import(&isolated, Application::Sonarr, tv.clone(), false)
            .await?
            .applied
    );

    for (index, sql, app) in [
        (
            1,
            TV.replace("VALUES(233)", "VALUES(234)"),
            Application::Sonarr,
        ),
        (
            2,
            TV.replace("Specials/pack.mkv", "../escape.mkv"),
            Application::Sonarr,
        ),
        (
            3,
            TV.replace("Specials/pack.mkv", "/absolute.mkv"),
            Application::Sonarr,
        ),
        (
            4,
            TV.replace("Specials/pack.mkv", "C:\\absolute.mkv"),
            Application::Sonarr,
        ),
        (
            5,
            TV.replace("(11,7,'Specials", "(11,999,'Specials"),
            Application::Sonarr,
        ),
        (
            6,
            MOVIES.replace("(1,101,'/movies", "(1,999,'/movies"),
            Application::Radarr,
        ),
        (
            7,
            MOVIES.replace("(11,1,'Film.mkv'", "(11,2,'Film.mkv'"),
            Application::Radarr,
        ),
        (
            8,
            TV.to_owned() + "CREATE VIEW unexpected AS SELECT * FROM Series;",
            Application::Sonarr,
        ),
        (
            9,
            TV.to_owned()
                + "CREATE TABLE computed(x INTEGER, y BLOB GENERATED ALWAYS AS (zeroblob(1000000000)) VIRTUAL);",
            Application::Sonarr,
        ),
    ] {
        let bytes = fixture(&files, &format!("bad{index}.db"), &sql).await;
        assert!(
            snapshots::import(&db, app, bytes, false).await.is_err(),
            "accepted invalid fixture {index}"
        );
    }
    let mut wal = tv.clone();
    wal[18] = 2;
    wal[19] = 2;
    assert!(
        snapshots::import(&db, Application::Sonarr, wal, false)
            .await
            .unwrap_err()
            .to_string()
            .contains("WAL")
    );
    assert!(
        snapshots::import(&db, Application::Radarr, tv.clone(), false)
            .await
            .is_err()
    );
    assert!(
        snapshots::import(&db, Application::Sonarr, Vec::new(), false)
            .await
            .is_err()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            files.0.join("library.db"),
            std::fs::Permissions::from_mode(0o644),
        )?;
        assert!(
            snapshots::import(&db, Application::Sonarr, tv, false)
                .await
                .unwrap_err()
                .to_string()
                .contains("private local")
        );
        std::fs::set_permissions(
            files.0.join("library.db"),
            std::fs::Permissions::from_mode(0o600),
        )?;
    }
    // CHECK expressions must never execute while reading an untrusted upload.
    // This expression would fail with integer overflow if quick_check evaluated it.
    let check_sql = TV.to_owned()
        + "PRAGMA ignore_check_constraints=ON; CREATE TABLE CheckedState(x INTEGER CHECK(abs(-9223372036854775808)>0)); INSERT INTO CheckedState VALUES(1); CREATE INDEX partial_config ON Config(Id) WHERE Key='private-setting';";
    let checked = fixture(&files, "checks.db", &check_sql).await;
    assert!(
        snapshots::import(&isolated, Application::Sonarr, checked, true)
            .await
            .is_ok()
    );
    // A missing mapped file is a local edit, not permission to recreate it on replay.
    ic.execute("UPDATE episodes SET episode_file_id=NULL", ())
        .await?;
    // Migration0006 restricts file removal while metadata exists; simulate coordinated
    // deletion but retain both mapping records so replay cannot resurrect either row.
    ic.execute("DELETE FROM file_metadata", ()).await?;
    ic.execute("DELETE FROM episode_files", ()).await?;
    let replay = snapshots::import(
        &isolated,
        Application::Sonarr,
        std::fs::read(files.0.join("sonarr.db"))?,
        false,
    )
    .await?;
    assert!(!replay.applied);
    assert!(replay.conflicts > 0);
    assert_eq!(count(&ic, "SELECT count(*) FROM episode_files").await, 0);
    // Appended schema DDL and archival writes are transactional too.
    let memory = libsql::Builder::new_local(":memory:").build().await?;
    let mc = memory.connect()?;
    let tx = mc.transaction().await?;
    tx.execute_batch(include_str!("../migrations/0003_snapshot_imports.sql"))
        .await?;
    tx.execute("INSERT INTO snapshot_imports(application,fingerprint,schema_version) VALUES ('sonarr','x',233)",()).await?;
    assert!(tx.execute("INSERT INTO snapshot_imports(application,fingerprint,schema_version) VALUES ('sonarr','x',233)",()).await.is_err());
    tx.rollback().await?;
    assert_eq!(
        count(
            &mc,
            "SELECT count(*) FROM sqlite_schema WHERE name='snapshot_imports'"
        )
        .await,
        0
    );
    // Parameters preserve archive payloads; no source text is interpolated into SQL.
    assert_eq!(
        c.query(
            "SELECT record_json FROM snapshot_records WHERE source_table=?1 LIMIT 1",
            params!["Config"]
        )
        .await?
        .next()
        .await?
        .unwrap()
        .get::<String>(0)?
        .contains("SENTINEL_SECRET"),
        true
    );
    Ok(())
}

async fn gate_state(c: &Connection) -> Vec<Vec<Vec<libsql::Value>>> {
    let mut state = Vec::new();
    for table in [
        "series",
        "episodes",
        "episode_files",
        "seasons",
        "operations",
        "movies",
        "movie_metadata",
        "movie_files",
        "library_settings",
        "file_metadata",
        "snapshot_imports",
        "snapshot_mappings",
        "snapshot_records",
    ] {
        let mut rows = c
            .query(&format!("SELECT * FROM {table} ORDER BY rowid"), ())
            .await
            .unwrap();
        let mut values = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            values.push(
                (0..row.column_count())
                    .map(|i| row.get_value(i).unwrap())
                    .collect(),
            );
        }
        state.push(values);
    }
    state
}

async fn original_tv(c: &Connection) -> Vec<libsql::Value> {
    let row = c.query("SELECT s.id,s.title,s.year,s.path,s.poster,e.id,e.series_id,e.season,e.number,e.title,f.path,o.id,o.episode_id,o.source,o.mode,o.destination,o.status,o.message,o.media_type,o.movie_id FROM series s JOIN episodes e ON e.series_id=s.id JOIN episode_files f ON f.id=e.episode_file_id JOIN operations o ON o.episode_id=e.id WHERE o.id='original'", ()).await.unwrap().next().await.unwrap().unwrap();
    let mut values: Vec<_> = (0..row.column_count())
        .map(|i| row.get_value(i).unwrap())
        .collect();
    drop(row);
    let mut rows = c.query("SELECT e.id,e.series_id,e.season,e.number,e.title,f.path FROM episodes e LEFT JOIN episode_files f ON f.id=e.episode_file_id WHERE e.series_id=7 ORDER BY e.id", ()).await.unwrap();
    while let Some(row) = rows.next().await.unwrap() {
        values.extend((0..row.column_count()).map(|i| row.get_value(i).unwrap()));
    }
    values
}

#[tokio::test]
async fn slice0_fresh_isolated_and_combined_imports_follow_actual_prototype_upgrade()
-> Result<(), Error> {
    let _guard = IMPORT_LOCK.lock().await;
    let files = Sandbox::new();
    // Radarr242 uses QualityProfileId; ProfileId belongs to the older206 contract.
    let sources = [
        (
            Application::Sonarr,
            "gate-tv.db",
            fixture(&files, "gate-tv.db", TV).await,
        ),
        (
            Application::Radarr,
            "gate-206.db",
            fixture(&files, "gate-206.db", OLD_MOVIES).await,
        ),
        (
            Application::Radarr,
            "gate-242.db",
            fixture(&files, "gate-242.db", MOVIES).await,
        ),
    ];
    for (name, selected, upgraded) in [
        ("tv-only", vec![0], false),
        ("movie206-only", vec![1], false),
        ("movie242-only", vec![2], false),
        ("combined", vec![0, 1, 2], false),
        ("upgraded-combined", vec![0, 1, 2], true),
    ] {
        let path = files.0.join(format!("{name}.db"));
        if upgraded {
            let raw = libsql::Builder::new_local(&path).build().await?;
            let c = raw.connect()?;
            c.execute_batch(include_str!("../migrations/0001_prototype.sql"))
                .await?;
            c.execute_batch("INSERT INTO series VALUES(7,'Original TV',1999,'/original','original-poster');INSERT INTO episodes VALUES(1,7,0,1,'Original episode','/original/pack.mkv'),(2,7,0,2,'Original second','/original/pack.mkv'),(3,7,1,1,'Original absent',NULL);INSERT INTO operations VALUES('original',1,'/source/original','move','/original/future.mkv','preview','Original message');").await?;
            drop(c);
            drop(raw);
        }
        // Existing prototype files must meet the archive's private-state permission gate.
        #[cfg(unix)]
        if upgraded {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        let db = Database::open_local(&path).await?;
        assert_eq!(db.migration_backup().is_some(), upgraded);
        let c = db.connect().await?;
        let original = if upgraded {
            let expected: Vec<libsql::Value> = vec![
                7.into(),
                "Original TV".into(),
                1999.into(),
                "/original".into(),
                "original-poster".into(),
                1.into(),
                7.into(),
                0.into(),
                1.into(),
                "Original episode".into(),
                "/original/pack.mkv".into(),
                "original".into(),
                1.into(),
                "/source/original".into(),
                "move".into(),
                "/original/future.mkv".into(),
                "preview".into(),
                "Original message".into(),
                "episode".into(),
                libsql::Value::Null,
                // All original episodes, including shared-file and absent-file records.
                1.into(),
                7.into(),
                0.into(),
                1.into(),
                "Original episode".into(),
                "/original/pack.mkv".into(),
                2.into(),
                7.into(),
                0.into(),
                2.into(),
                "Original second".into(),
                "/original/pack.mkv".into(),
                3.into(),
                7.into(),
                1.into(),
                1.into(),
                "Original absent".into(),
                libsql::Value::Null,
            ];
            assert_eq!(original_tv(&c).await, expected);
            Some(expected)
        } else {
            None
        };
        for index in &selected {
            let (app, _, bytes) = &sources[*index];
            let before = gate_state(&c).await;
            let dry = snapshots::import(&db, *app, bytes.clone(), true).await?;
            assert!(!dry.applied && dry.conflicts == 0);
            assert_eq!(gate_state(&c).await, before, "dryrun changed {name}");
            // Abort after preceding core inserts, preserving both earlier imports and prototype facts.
            let target = if *index == 0 { "episodes" } else { "movies" };
            c.execute_batch(&format!("CREATE TRIGGER gate_failure BEFORE INSERT ON {target} BEGIN SELECT RAISE(ABORT,'PRIVATE_FAILURE'); END;")).await?;
            let failure = snapshots::import(&db, *app, bytes.clone(), false)
                .await
                .unwrap_err();
            assert!(!failure.to_string().contains("PRIVATE_FAILURE"));
            assert_eq!(gate_state(&c).await, before, "failed import changed {name}");
            c.execute("DROP TRIGGER gate_failure", ()).await?;
            let report = snapshots::import(&db, *app, bytes.clone(), false).await?;
            assert!(report.applied && report.conflicts == 0);
            assert_eq!(report.missing_file_records, if *index == 1 { 0 } else { 1 });
            assert!(!serde_json::to_string(&report)?.contains("SENTINEL_SECRET"));
            if *index == 0 {
                assert!(
                    report
                        .unsupported
                        .iter()
                        .any(|u| u.table == "DownloadClients")
                );
                assert_eq!(count(&c,"SELECT count(*) FROM snapshot_records WHERE source_table='Config' AND instr(record_json,'SENTINEL_SECRET')>0").await,1);
                assert_eq!(count(&c,"SELECT count(*) FROM episodes e JOIN episode_files f ON f.id=e.episode_file_id WHERE f.path='/tv/TV/Specials/pack.mkv'").await,2);
                assert_eq!(count(&c,"SELECT count(*) FROM episodes e JOIN series s ON s.id=e.series_id WHERE s.tvdb_id=777 AND e.episode_file_id IS NULL").await,3);
            } else if *index == 2 {
                assert!(report.unsupported.iter().any(|u| u.table == "Collections"));
                assert_eq!(count(&c,"SELECT count(*) FROM movie_files WHERE path='/movies/Film/Film.mkv' AND edition='Extended'").await,1);
            }
            let applied = gate_state(&c).await;
            let replay = snapshots::import(&db, *app, bytes.clone(), false).await?;
            assert!(replay.applied && replay.mapped == 0 && replay.conflicts == 0);
            assert_eq!(gate_state(&c).await, applied, "replay changed {name}");
            if let Some(expected) = &original {
                assert_eq!(&original_tv(&c).await, expected);
            }
        }
        if selected.len() == 1 {
            assert_eq!(
                count(
                    &c,
                    if selected[0] == 0 {
                        "SELECT count(*) FROM movies"
                    } else {
                        "SELECT count(*) FROM episodes"
                    }
                )
                .await,
                0
            );
        } else {
            // Numeric identity1 is simultaneously a movie and episode; no unqualified target is used.
            assert_eq!(
                count(&c, "SELECT count(*) FROM episodes WHERE id=1").await,
                1
            );
            assert_eq!(count(&c, "SELECT count(*) FROM movies WHERE id=1").await, 1);
            assert_eq!(count(&c,"SELECT count(*) FROM snapshot_mappings WHERE source_id=1 AND destination_table IN ('episodes','movies')").await,3);
            assert_eq!(count(&c, "SELECT count(*) FROM movies").await, 4);
        }
        let before_reopen = gate_state(&c).await;
        drop(c);
        drop(db);
        let reopened = Database::open_local(&path).await?;
        assert!(reopened.migration_backup().is_none());
        let c = reopened.connect().await?;
        assert_eq!(gate_state(&c).await, before_reopen);
        if let Some(expected) = &original {
            assert_eq!(&original_tv(&c).await, expected);
        }
        for (_, filename, bytes) in &sources {
            assert_eq!(&std::fs::read(files.0.join(filename))?, bytes);
        }
    }
    Ok(())
}
