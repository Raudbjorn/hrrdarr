use hrrdarr::{
    db::{Database, Error},
    snapshots::{self, Application},
};
use libsql::{Connection, params};
use std::path::PathBuf;
struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Self {
        let p =
            std::env::temp_dir().join(format!("hrrdarr-library-settings-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
fn digest(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
async fn scalar(c: &Connection, sql: &str) -> i64 {
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
async fn source(s: &Sandbox, name: &str, sql: &str) -> Vec<u8> {
    let p = s.0.join(name);
    let d = libsql::Builder::new_local(&p).build().await.unwrap();
    let c = d.connect().unwrap();
    c.execute_batch(sql).await.unwrap();
    drop(c);
    drop(d);
    std::fs::read(p).unwrap()
}
const TV: &str = r#"
CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES(233);
CREATE TABLE Series(Id INTEGER,TvdbId INTEGER,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,Seasons TEXT,SeriesType INTEGER,SeasonFolder INTEGER,UseSceneNumbering INTEGER,MonitorNewItems INTEGER,Added TEXT,QualityProfileId INTEGER,PrivateSetting TEXT);
INSERT INTO Series VALUES(7,777,'TV',2020,'/tv/TV',1,'[]',2,0,1,1,'2020-02-29 12:30:00',9,'PRIVATE_LIBRARY_SENTINEL');
CREATE TABLE Episodes(Id INTEGER,SeriesId INTEGER,SeasonNumber INTEGER,EpisodeNumber INTEGER,Title TEXT,Monitored INTEGER,EpisodeFileId INTEGER);
CREATE TABLE EpisodeFiles(Id INTEGER,SeriesId INTEGER,RelativePath TEXT);
"#;
const MOVIES: &str = r#"
CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES(242);
CREATE TABLE MovieMetadata(Id INTEGER,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER);INSERT INTO MovieMetadata VALUES(101,1001,'tt1001','Film',2024),(102,1002,NULL,'Catalog',2025);
CREATE TABLE Movies(Id INTEGER,MovieMetadataId INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER,MinimumAvailability INTEGER,Added TEXT,QualityProfileId INTEGER);
INSERT INTO Movies VALUES(1,101,'/movies/Film',1,0,2,'2024-02-29T12:30:00+02:00',9);
CREATE TABLE MovieFiles(Id INTEGER,MovieId INTEGER,RelativePath TEXT,Edition TEXT);
"#;
#[tokio::test]
async fn schema6_upgrade_activates_only_exact_library_settings_and_preserves_private_sources()
-> Result<(), Error> {
    let s = Sandbox::new();
    let tv = source(&s, "sonarr.db", TV).await;
    let movies = source(&s, "radarr.db", MOVIES).await;
    let path = s.0.join("library.db");
    let old = libsql::Builder::new_local(&path).build().await?;
    let c = old.connect()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    c.execute_batch("PRAGMA foreign_keys=ON;CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY,name TEXT NOT NULL,checksum TEXT NOT NULL,sql TEXT NOT NULL,applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);").await?;
    for (i, (name, sql)) in [
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
    ]
    .iter()
    .enumerate()
    {
        c.execute_batch(sql).await?;
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql)VALUES(?,?,?,?)",
            params![(i + 1) as i64, *name, digest(sql.as_bytes()), *sql],
        )
        .await?;
    }
    c.execute_batch("INSERT INTO series(id,tvdb_id,title,year,path,monitored)VALUES(10,777,'TV',2020,'/tv/TV',1);INSERT INTO movie_metadata(id,tmdb_id,imdb_id,title,year)VALUES(10,1001,'tt1001','Film',2024),(11,1002,NULL,'Catalog',2025);INSERT INTO movies VALUES(10,10,'/movies/Film',1);INSERT INTO quality_profiles VALUES(9,'movies','Movie profile'),(10,'tv','TV profile');").await?;
    for (app, bytes, version, mappings) in [
        ("sonarr", &tv, 233, vec![("series", 7, 10)]),
        (
            "radarr",
            &movies,
            242,
            vec![
                ("movie_metadata", 101, 10),
                ("movie_metadata", 102, 11),
                ("movies", 1, 10),
            ],
        ),
    ] {
        c.execute("INSERT INTO snapshot_imports(application,fingerprint,schema_version,episode_metadata_version)VALUES(?,?,?,1)",params![app,digest(bytes),version]).await?;
        for (table, source, target) in mappings {
            c.execute(
                "INSERT INTO snapshot_mappings VALUES(?,?,?,?,?)",
                params![app, digest(bytes), table, source, target],
            )
            .await?;
        }
        c.execute(
            "INSERT INTO snapshot_records VALUES(?,?,'OldPrivateArchive',0,?)",
            params![
                app,
                digest(bytes),
                "{\"private\":\"PRIVATE_LIBRARY_SENTINEL\"}"
            ],
        )
        .await?;
    }
    let tx = c.transaction().await?;
    tx.execute_batch(include_str!("../migrations/0007_library_settings.sql"))
        .await?;
    assert!(
        tx.execute(
            "INSERT INTO library_settings(media_type,series_id,movie_id)VALUES('tv',10,10)",
            ()
        )
        .await
        .is_err()
    );
    tx.rollback().await?;
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM sqlite_schema WHERE name='library_settings'"
        )
        .await,
        0
    );
    assert_eq!(scalar(&c, "SELECT count(*) FROM series").await, 1);
    drop(c);
    drop(old);
    let db = Database::open_local(&path).await?;
    assert!(db.migration_backup().is_some());
    let c = db.connect().await?;
    assert_eq!(scalar(&c, "SELECT count(*) FROM library_settings").await, 0);
    c.execute("UPDATE series SET monitored=0 WHERE id=10", ())
        .await?;
    let edited = snapshots::import(&db, Application::Sonarr, tv.clone(), false).await?;
    assert!(!edited.applied);
    assert!(edited.conflicts > 0);
    assert_eq!(scalar(&c, "SELECT count(*) FROM library_settings").await, 0);
    c.execute("UPDATE series SET monitored=1 WHERE id=10", ())
        .await?;
    for (app, bytes, count) in [
        (Application::Sonarr, tv.clone(), 0),
        (Application::Radarr, movies.clone(), 1),
    ] {
        let dry = snapshots::import(&db, app, bytes.clone(), true).await?;
        assert!(!dry.applied);
        assert_eq!(dry.mapped, 1);
        assert_eq!(
            scalar(&c, "SELECT count(*) FROM library_settings").await,
            count
        );
        let applied = snapshots::import(&db, app, bytes.clone(), false).await?;
        assert!(applied.applied);
        assert_eq!(applied.mapped, 1);
        assert!(
            applied
                .unsupported
                .iter()
                .any(|u| u.columns.iter().any(|c| c == "QualityProfileId"))
        );
        assert!(!serde_json::to_string(&applied)?.contains("PRIVATE_LIBRARY_SENTINEL"));
        assert_eq!(snapshots::import(&db, app, bytes, false).await?.mapped, 0);
    }
    let row=c.query("SELECT series_type,season_folder,use_scene_numbering,monitor_new_items,added,quality_profile_id FROM library_settings WHERE media_type='tv'",()).await?.next().await?.unwrap();
    assert_eq!(row.get::<String>(0)?, "anime");
    assert_eq!(row.get::<i64>(1)?, 0);
    assert_eq!(row.get::<i64>(2)?, 1);
    assert_eq!(row.get::<String>(3)?, "none");
    assert_eq!(row.get::<String>(4)?, "2020-02-29T12:30:00Z");
    assert!(row.get::<Option<i64>>(5)?.is_none());
    drop(row);
    let row=c.query("SELECT minimum_availability,added,quality_profile_id FROM library_settings WHERE media_type='movies'",()).await?.next().await?.unwrap();
    assert_eq!(row.get::<String>(0)?, "in_cinemas");
    assert_eq!(row.get::<String>(1)?, "2024-02-29T10:30:00Z");
    assert!(row.get::<Option<i64>>(2)?.is_none());
    drop(row);
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM movie_metadata WHERE id NOT IN(SELECT metadata_id FROM movies)"
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM snapshot_imports WHERE episode_metadata_version=1"
        )
        .await,
        2
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM snapshot_records WHERE source_table='OldPrivateArchive'"
        )
        .await,
        2
    );
    let archive = c
        .query(
            "SELECT record_json FROM snapshot_records WHERE source_table='Series'",
            (),
        )
        .await?
        .next()
        .await?
        .unwrap()
        .get::<String>(0)?;
    assert!(archive.contains("PRIVATE_LIBRARY_SENTINEL"));
    for sql in [
        "UPDATE library_settings SET quality_profile_id=9 WHERE media_type='tv'",
        "UPDATE library_settings SET quality_profile_id=10 WHERE media_type='movies'",
        "UPDATE library_settings SET movie_id=10 WHERE media_type='tv'",
        "DELETE FROM series WHERE id=10",
    ] {
        assert!(c.execute(sql, ()).await.is_err(), "{sql}");
    }
    c.execute(
        "UPDATE library_settings SET series_type=NULL WHERE media_type='tv'",
        (),
    )
    .await?;
    assert!(
        !snapshots::import(&db, Application::Sonarr, tv.clone(), false)
            .await?
            .applied
    );
    c.execute("DELETE FROM library_settings WHERE media_type='movies'", ())
        .await?;
    assert!(
        !snapshots::import(&db, Application::Radarr, movies.clone(), false)
            .await?
            .applied
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM library_settings WHERE media_type='movies'"
        )
        .await,
        0
    );
    assert_eq!(std::fs::read(s.0.join("sonarr.db"))?, tv);
    assert_eq!(std::fs::read(s.0.join("radarr.db"))?, movies);
    drop(c);
    drop(db);
    let reopened = Database::open_local(&path).await?;
    assert_eq!(
        scalar(
            &reopened.connect().await?,
            "SELECT count(*) FROM library_settings WHERE series_type IS NULL"
        )
        .await,
        1
    );
    drop(reopened);
    // Both source readers report unknown enum values without activating a guessed policy.
    for (app, sql) in [
        (
            Application::Sonarr,
            format!("{TV} UPDATE Series SET SeriesType=999,MonitorNewItems=999;"),
        ),
        (
            Application::Radarr,
            format!("{MOVIES} UPDATE Movies SET MinimumAvailability=-1;"),
        ),
    ] {
        let name = if matches!(app, Application::Sonarr) {
            "tv"
        } else {
            "movies"
        };
        let bytes = source(&s, &format!("unknown-{name}.db"), &sql).await;
        let target = Database::open_local(s.0.join(format!("unknown-target-{name}.db"))).await?;
        let report = snapshots::import(&target, app, bytes, false).await?;
        assert!(report.applied);
        assert!(report.unsupported.iter().any(|r| {
            r.columns
                .iter()
                .any(|c| matches!(c.as_str(), "SeriesType" | "MinimumAvailability"))
        }));
    }
    // Late settings failure restores newly inserted core and archive/mapping rows.
    let failed = Database::open_local(s.0.join("failed.db")).await?;
    let c = failed.connect().await?;
    c.execute_batch("CREATE TRIGGER reject_settings BEFORE INSERT ON library_settings BEGIN SELECT RAISE(ABORT,'PRIVATE_LIBRARY_SENTINEL'); END;").await?;
    for (app, bytes) in [(Application::Sonarr, tv), (Application::Radarr, movies)] {
        let error = snapshots::import(&failed, app, bytes, false)
            .await
            .unwrap_err();
        assert!(!error.to_string().contains("PRIVATE_LIBRARY_SENTINEL"));
        for table in [
            "series",
            "movies",
            "movie_metadata",
            "library_settings",
            "snapshot_imports",
        ] {
            assert_eq!(
                scalar(&c, &format!("SELECT count(*) FROM {table}")).await,
                0
            );
        }
    }
    Ok(())
}
