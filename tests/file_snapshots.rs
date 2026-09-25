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
            std::env::temp_dir().join(format!("hrrdarr-file-snapshot-{}", uuid::Uuid::new_v4()));
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
CREATE TABLE Series(Id INTEGER,TvdbId INTEGER,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,Seasons TEXT);
INSERT INTO Series VALUES(7,777,'TV',2020,'/tv/TV',1,'[{"seasonNumber":1,"monitored":true}]');
CREATE TABLE Episodes(Id INTEGER,SeriesId INTEGER,SeasonNumber INTEGER,EpisodeNumber INTEGER,Title TEXT,Monitored INTEGER,EpisodeFileId INTEGER);
INSERT INTO Episodes VALUES(1,7,1,1,'Pilot',1,11);
CREATE TABLE EpisodeFiles(Id INTEGER,SeriesId INTEGER,RelativePath TEXT,Quality TEXT,Languages TEXT,Size INTEGER,DateAdded TEXT,SeasonNumber INTEGER,ReleaseGroup TEXT,IndexerFlags INTEGER,ReleaseType INTEGER,SceneName TEXT,MediaInfo TEXT);
INSERT INTO EpisodeFiles VALUES(11,7,'pilot.mkv','{"quality":7,"revision":{"version":2,"real":1,"isRepack":true,"secret":"SENTINEL_FILE_SECRET"},"secret":"SENTINEL_FILE_SECRET"}','[1,null]',1234,'2024-02-29 12:30:00',1,'GROUP',4,1,'SENTINEL_FILE_SECRET','{"schemaRevision":14,"videoBitDepth":10,"videoBitrate":12000000,"videoFps":23.97649,"width":1920,"height":1080,"runTime":"1.01:02:03.1234567","videoHdrFormat":1,"scanType":"Progressive","videoFormat":"HEVC","videoCodecID":"V_MPEGH/ISO/HEVC","rawStreamData":"SENTINEL_MEDIA_PRIVATE","audioStreams":[{"language":"eng","format":"E-AC-3","codecId":"A_EAC3","bitrate":640000,"channels":6,"channelPositions":"3/2/0.1","apiKey":"SENTINEL_MEDIA_PRIVATE"},{"language":"isl","bitrate":128000,"channels":2}],"subtitleStreams":[{"language":"eng","format":"UTF-8","forced":false,"hearingImpaired":true,"title":"SENTINEL_MEDIA_PRIVATE"}]}');
"#;
const MOVIES: &str = r#"
CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES(242);
CREATE TABLE MovieMetadata(Id INTEGER,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER);INSERT INTO MovieMetadata VALUES(101,1001,'tt1001','Film',2024);
CREATE TABLE Movies(Id INTEGER,MovieMetadataId INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER);INSERT INTO Movies VALUES(1,101,'/movies/Film',1,11);
CREATE TABLE MovieFiles(Id INTEGER,MovieId INTEGER,RelativePath TEXT,Edition TEXT,Quality TEXT,Languages TEXT,Size INTEGER,DateAdded TEXT,OriginalFilePath TEXT,ReleaseGroup TEXT,IndexerFlags INTEGER,MediaInfo TEXT);
INSERT INTO MovieFiles VALUES(11,1,'film.mkv','Extended','{"quality":31,"revision":{"version":1,"real":0,"isRepack":false}}','[1,57]',5678,'2024-02-29T12:30:00+02:00','/download/film.mkv','MOVIE',4095,'{"schemaRevision":14,"width":3840,"height":2160,"runTime":"02:03:04.0000001","videoFps":24,"videoHdrFormat":1,"audioBitrate":0,"audioChannels":8,"audioChannelPositions":"3/4/0.1","audioStreamCount":2,"audioLanguages":["eng","fra"],"subtitles":["fra"],"audioFormat":"DTS","audioCodecID":"A_DTS","rawFrameData":"SENTINEL_MEDIA_PRIVATE"}');
"#;
#[tokio::test]
async fn schema5_file_metadata_activation_is_private_atomic_and_idempotent() -> Result<(), Error> {
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
    ]
    .iter()
    .enumerate()
    {
        c.execute_batch(sql).await?;
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql)VALUES(?,?,?,?)",
            params![(index + 1) as i64, *name, digest(sql.as_bytes()), *sql],
        )
        .await?;
    }
    c.execute_batch("INSERT INTO series(id,tvdb_id,title,year,path,monitored)VALUES(10,777,'TV',2020,'/tv/TV',1);INSERT INTO seasons VALUES(10,1,1);INSERT INTO episode_files VALUES(30,10,'/tv/TV/pilot.mkv');INSERT INTO episodes(id,series_id,season,number,title,episode_file_id,monitored)VALUES(20,10,1,1,'Pilot',30,1);INSERT INTO movie_metadata(id,tmdb_id,imdb_id,title,year)VALUES(10,1001,'tt1001','Film',2024);INSERT INTO movies VALUES(20,10,'/movies/Film',1);INSERT INTO movie_files VALUES(30,20,'/movies/Film/film.mkv','Extended');").await?;
    for (app, bytes, version, mappings) in [
        (
            "sonarr",
            &tv,
            233,
            vec![
                ("series", 7, 10),
                ("episode_files", 11, 30),
                ("episodes", 1, 20),
            ],
        ),
        (
            "radarr",
            &movies,
            242,
            vec![
                ("movie_metadata", 101, 10),
                ("movies", 1, 20),
                ("movie_files", 11, 30),
            ],
        ),
    ] {
        c.execute("INSERT INTO snapshot_imports(application,fingerprint,schema_version,episode_metadata_version)VALUES(?,?,?,1)",params![app,digest(bytes),version]).await?;
        for (table, source, dest) in mappings {
            c.execute(
                "INSERT INTO snapshot_mappings VALUES(?,?,?,?,?)",
                params![app, digest(bytes), table, source, dest],
            )
            .await?;
        }
        c.execute(
            "INSERT INTO snapshot_records VALUES(?,?,'PrivateOldArchive',0,?)",
            params![app, digest(bytes), "{\"old\":\"SENTINEL_FILE_SECRET\"}"],
        )
        .await?;
    }
    // Apply the real appended DDL, fail a typed-target write, and roll both back.
    let tx = c.transaction().await?;
    tx.execute_batch(include_str!("../migrations/0006_file_metadata.sql"))
        .await?;
    assert!(
        tx.execute(
            "INSERT INTO file_metadata(media_type,episode_file_id,movie_file_id)VALUES('tv',30,30)",
            ()
        )
        .await
        .is_err()
    );
    tx.rollback().await?;
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM sqlite_schema WHERE name='file_metadata'"
        )
        .await,
        0
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM episode_files WHERE id=30 AND path='/tv/TV/pilot.mkv'"
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM movie_files WHERE id=30 AND edition='Extended'"
        )
        .await,
        1
    );
    drop(c);
    drop(old);
    let db = Database::open_local(&path).await?;
    assert!(db.migration_backup().is_some());
    let c = db.connect().await?;
    assert_eq!(scalar(&c, "SELECT count(*) FROM file_metadata").await, 0);
    // An old core file edit is not permission to activate newly available metadata.
    c.execute(
        "UPDATE episode_files SET path='/tv/TV/local.mkv' WHERE id=30",
        (),
    )
    .await?;
    let conflict = snapshots::import(&db, Application::Sonarr, tv.clone(), false).await?;
    assert!(!conflict.applied);
    assert!(conflict.conflicts > 0);
    assert_eq!(scalar(&c, "SELECT count(*) FROM file_metadata").await, 0);
    c.execute(
        "UPDATE episode_files SET path='/tv/TV/pilot.mkv' WHERE id=30",
        (),
    )
    .await?;
    for (app, bytes) in [
        (Application::Sonarr, tv.clone()),
        (Application::Radarr, movies.clone()),
    ] {
        let dry = snapshots::import(&db, app, bytes.clone(), true).await?;
        assert!(!dry.applied);
        assert_eq!(dry.mapped, 1);
        assert_eq!(
            scalar(&c, "SELECT count(*) FROM file_metadata").await,
            if matches!(app, Application::Sonarr) {
                0
            } else {
                1
            }
        );
        let result = snapshots::import(&db, app, bytes.clone(), false).await?;
        assert!(result.applied);
        assert_eq!(result.mapped, 1);
        assert!(!serde_json::to_string(&result)?.contains("SENTINEL_FILE_SECRET"));
        let again = snapshots::import(&db, app, bytes, false).await?;
        assert!(again.applied);
        assert_eq!(again.mapped, 0);
    }
    assert_eq!(scalar(&c, "SELECT count(*) FROM file_metadata").await, 2);
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM snapshot_imports WHERE episode_metadata_version=1"
        )
        .await,
        2
    );
    let row=c.query("SELECT quality_id,revision_json,languages_json,size,date_added,season_number FROM file_metadata WHERE media_type='tv'",()).await?.next().await?.unwrap();
    assert_eq!(row.get::<i64>(0)?, 7);
    assert!(!row.get::<String>(1)?.contains("SENTINEL"));
    assert_eq!(row.get::<String>(2)?, "[1,0]");
    assert_eq!(row.get::<i64>(3)?, 1234);
    assert_eq!(row.get::<String>(4)?, "2024-02-29T12:30:00Z");
    assert_eq!(row.get::<i64>(5)?, 1);
    drop(row);
    assert_eq!(
        scalar(
            &c,
            "SELECT quality_id FROM file_metadata WHERE media_type='movies'"
        )
        .await,
        31
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM snapshot_records WHERE source_table='PrivateOldArchive'"
        )
        .await,
        2
    );
    for (media, expected_hdr, expected_runtime, expected_languages, expected_bitrate) in [
        ("tv", "HDR", "25:02:03", "eng/isl", 640000),
        ("movies", "PQ", "2:03:04", "eng/fra", 0),
    ] {
        let value = c
            .query(
                "SELECT media_info_json FROM file_metadata WHERE media_type=?",
                [media],
            )
            .await?
            .next()
            .await?
            .unwrap()
            .get::<String>(0)?;
        assert!(!value.contains("SENTINEL"));
        let info: serde_json::Value = serde_json::from_str(&value)?;
        assert_eq!(info["video_dynamic_range_type"], expected_hdr);
        assert_eq!(info["run_time"], expected_runtime);
        assert_eq!(info["audio_languages"], expected_languages);
        assert_eq!(info["audio_bitrate"], expected_bitrate);
        assert!(info["audio_codec"].is_null());
        assert!(info["video_codec"].is_null());
        if media == "tv" {
            assert_eq!(info["runtime_ticks"], 901231234567_i64);
            assert_eq!(info["video_fps"], 23.976);
            assert_eq!(info["audio_streams"][0]["channels"], 6);
        }
    }
    // Media facts use strict replay equality as well, including a deliberate local clear.
    let media_before = c
        .query(
            "SELECT media_info_json FROM file_metadata WHERE media_type='tv'",
            (),
        )
        .await?
        .next()
        .await?
        .unwrap()
        .get::<String>(0)?;
    c.execute(
        "UPDATE file_metadata SET media_info_json=NULL WHERE media_type='tv'",
        (),
    )
    .await?;
    assert!(
        !snapshots::import(&db, Application::Sonarr, tv.clone(), false)
            .await?
            .applied
    );
    c.execute(
        "UPDATE file_metadata SET media_info_json=? WHERE media_type='tv'",
        [media_before],
    )
    .await?;
    let archive = c
        .query(
            "SELECT record_json FROM snapshot_records WHERE source_table='EpisodeFiles'",
            (),
        )
        .await?
        .next()
        .await?
        .unwrap()
        .get::<String>(0)?;
    assert!(archive.contains("SENTINEL_FILE_SECRET"));
    // Domain scope, required target, catalog and deletion restrictions are enforced by actual libSQL.
    for sql in [
        "UPDATE file_metadata SET media_type='movies' WHERE episode_file_id=30",
        "UPDATE file_metadata SET movie_file_id=30 WHERE episode_file_id=30",
        "UPDATE file_metadata SET quality_id=31 WHERE media_type='tv'",
        "DELETE FROM episode_files WHERE id=30",
    ] {
        assert!(c.execute(sql, ()).await.is_err(), "{sql}");
    }
    c.execute(
        "UPDATE file_metadata SET release_group=NULL WHERE media_type='tv'",
        (),
    )
    .await?;
    let edited = snapshots::import(&db, Application::Sonarr, tv.clone(), false).await?;
    assert!(!edited.applied);
    assert!(edited.conflicts > 0);
    c.execute("DELETE FROM file_metadata WHERE media_type='movies'", ())
        .await?;
    let deleted = snapshots::import(&db, Application::Radarr, movies.clone(), false).await?;
    assert!(!deleted.applied);
    assert!(deleted.conflicts > 0);
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM file_metadata WHERE media_type='movies'"
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
            "SELECT count(*) FROM file_metadata WHERE release_group IS NULL"
        )
        .await,
        1
    );
    drop(reopened);
    // A late metadata constraint failure rolls back earlier parent/file/archive writes too.
    let failure = Database::open_local(s.0.join("failure.db")).await?;
    let failure_conn = failure.connect().await?;
    failure_conn.execute_batch("CREATE TRIGGER reject_metadata BEFORE INSERT ON file_metadata BEGIN SELECT RAISE(ABORT,'SENTINEL_FILE_SECRET'); END;").await?;
    for (app, bytes) in [(Application::Sonarr, tv), (Application::Radarr, movies)] {
        let error = snapshots::import(&failure, app, bytes, false)
            .await
            .unwrap_err();
        assert!(!error.to_string().contains("SENTINEL"));
        for table in [
            "series",
            "movies",
            "episode_files",
            "movie_files",
            "file_metadata",
            "snapshot_imports",
        ] {
            assert_eq!(
                scalar(&failure_conn, &format!("SELECT count(*) FROM {table}")).await,
                0
            );
        }
    }
    // Unknown versioned enum values remain private and are reported, never converted to a known enum.
    let unknown = source(
        &s,
        "unknown.db",
        &TV.replace("\"quality\":7", "\"quality\":999")
            .replace("'[1,null]'", "'[-2,999]'")
            .replace("\"videoHdrFormat\":1", "\"videoHdrFormat\":999")
            .replace(",'GROUP',4,1,", ",'GROUP',9999,9,"),
    )
    .await;
    let fresh = Database::open_local(s.0.join("unknown-target.db")).await?;
    let result = snapshots::import(&fresh, Application::Sonarr, unknown, false).await?;
    assert!(result.applied);
    for field in ["Quality", "Languages", "IndexerFlags", "ReleaseType"] {
        assert!(
            result
                .unsupported
                .iter()
                .any(|r| r.columns.iter().any(|c| c == field)),
            "{field}"
        );
    }
    assert_eq!(scalar(&fresh.connect().await?,"SELECT count(*) FROM file_metadata WHERE quality_id IS NULL AND languages_json IS NULL AND indexer_flags IS NULL AND release_type IS NULL").await,1);
    // Malformed media facts fail without inserting core rows or exposing source values.
    for (index, media_json) in [
        serde_json::json!({"videoBitrate":-1}),
        serde_json::json!({"videoFps":"NaN"}),
        serde_json::json!({"runTime":"-01:00:00"}),
        serde_json::json!({"audioStreams":[{"channels":-1}]}),
        serde_json::json!({"subtitleStreams":[{"forced":"yes"}]}),
        serde_json::json!({"audioStreams":vec![serde_json::json!({});65]}),
        serde_json::json!({"scanType":"x".repeat(1025)}),
        serde_json::json!({"rawStreamData":"SENTINEL_MEDIA_PRIVATE".repeat(4000)}),
    ]
    .iter()
    .enumerate()
    {
        let sql = format!(
            "{TV} UPDATE EpisodeFiles SET MediaInfo='{}';",
            media_json.to_string().replace('\'', "''")
        );
        let bytes = source(&s, &format!("invalid-media-{index}.db"), &sql).await;
        let target = Database::open_local(s.0.join(format!("invalid-target-{index}.db"))).await?;
        let error = snapshots::import(&target, Application::Sonarr, bytes, false)
            .await
            .unwrap_err();
        assert!(!error.to_string().contains("SENTINEL"));
        assert_eq!(
            scalar(
                &target.connect().await?,
                "SELECT count(*) FROM episode_files"
            )
            .await,
            0
        );
    }
    for (index, media_json) in [
        serde_json::json!({"audioLanguages":[null]}),
        serde_json::json!({"audioChannels":-1}),
        serde_json::json!({"audioStreamCount":1.5}),
        serde_json::json!({"subtitles":vec!["eng";65]}),
    ]
    .iter()
    .enumerate()
    {
        let sql = format!(
            "{MOVIES} UPDATE MovieFiles SET MediaInfo='{}';",
            media_json.to_string().replace('\'', "''")
        );
        let bytes = source(&s, &format!("invalid-movie-media-{index}.db"), &sql).await;
        let target =
            Database::open_local(s.0.join(format!("invalid-movie-target-{index}.db"))).await?;
        assert!(
            snapshots::import(&target, Application::Radarr, bytes, false)
                .await
                .is_err()
        );
        assert_eq!(
            scalar(&target.connect().await?, "SELECT count(*) FROM movie_files").await,
            0
        );
    }
    Ok(())
}
