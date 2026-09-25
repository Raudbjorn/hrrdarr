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
            std::env::temp_dir().join(format!("hrrdarr-episode-metadata-{}", uuid::Uuid::new_v4()));
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
const SOURCE: &str = r#"
CREATE TABLE VersionInfo(Version INTEGER); INSERT INTO VersionInfo VALUES(233);
CREATE TABLE Series(Id INTEGER,TvdbId INTEGER,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,Seasons TEXT);
INSERT INTO Series VALUES(7,777,'Test',2024,'/tv/Test',1,'[{"seasonNumber":1,"monitored":true}]');
CREATE TABLE EpisodeFiles(Id INTEGER,SeriesId INTEGER,RelativePath TEXT);
INSERT INTO EpisodeFiles VALUES(11,7,'Season 1/Leap.mkv');
CREATE TABLE Episodes(Id INTEGER,SeriesId INTEGER,SeasonNumber INTEGER,EpisodeNumber INTEGER,Title TEXT,Monitored INTEGER,EpisodeFileId INTEGER,
 TvdbId INTEGER,AirDate TEXT,AirDateUtc TEXT,LastSearchTime TEXT,Runtime INTEGER,FinaleType TEXT,Overview TEXT,
 AbsoluteEpisodeNumber INTEGER,SceneAbsoluteEpisodeNumber INTEGER,SceneEpisodeNumber INTEGER,SceneSeasonNumber INTEGER,UnverifiedSceneNumbering INTEGER,Images TEXT);
INSERT INTO Episodes VALUES(1,7,1,1,'Leap',1,11,999,'2024-02-29','2024-02-29T20:30:00+02:00','2024-03-01 01:02:03.123',45,'season','Episode overview',10,20,2,3,1,
 '[{"coverType":4,"url":"/covers/episode.jpg","remoteUrl":"https://example.invalid/cover.jpg","apiKey":"SENTINEL_PRIVATE_COVER"}]');
"#;
async fn source(files: &Sandbox, name: &str, sql: &str) -> Vec<u8> {
    let path = files.0.join(name);
    let d = libsql::Builder::new_local(&path).build().await.unwrap();
    d.connect().unwrap().execute_batch(sql).await.unwrap();
    drop(d);
    std::fs::read(path).unwrap()
}

#[tokio::test]
async fn episode_metadata_backfills_only_exact_old_snapshot_without_overwriting_local_edits()
-> Result<(), Error> {
    let files = Sandbox::new();
    let bytes = source(&files, "source.db", SOURCE).await;
    let fingerprint = digest(&bytes);
    // Materialize actual schema4 and its applied migration history, not a fake version label
    // on the newest schema. These rows represent the previous importer's core-only writes.
    let path = files.0.join("library.db");
    let old = libsql::Builder::new_local(&path).build().await?;
    let c = old.connect()?;
    // Previous application databases are owner-only; the raw fixture builder uses the process umask.
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
    ]
    .iter()
    .enumerate()
    {
        c.execute_batch(sql).await?;
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql)VALUES(?1,?2,?3,?4)",
            params![(i + 1) as i64, *name, digest(sql.as_bytes()), *sql],
        )
        .await?;
    }
    c.execute_batch("INSERT INTO series(id,tvdb_id,title,year,path,monitored)VALUES(10,777,'Test',2024,'/tv/Test',1);
        INSERT INTO seasons VALUES(10,1,1); INSERT INTO episode_files VALUES(30,10,'/tv/Test/Season 1/Leap.mkv');
        INSERT INTO episodes(id,series_id,season,number,title,episode_file_id,monitored)VALUES(20,10,1,1,'Leap',30,1);").await?;
    c.execute("INSERT INTO snapshot_imports(application,fingerprint,schema_version)VALUES('sonarr',?1,233)",params![fingerprint.clone()]).await?;
    for (table, source_id, destination_id) in [
        ("series", 7, 10),
        ("episode_files", 11, 30),
        ("episodes", 1, 20),
    ] {
        c.execute(
            "INSERT INTO snapshot_mappings VALUES('sonarr',?1,?2,?3,?4)",
            params![fingerprint.clone(), table, source_id, destination_id],
        )
        .await?;
    }
    let archive=serde_json::json!({"Id":["integer",1],"Overview":["text","Episode overview"],"Images":["text","SENTINEL_PRIVATE_COVER"]}).to_string();
    c.execute(
        "INSERT INTO snapshot_records VALUES('sonarr',?1,'Episodes',0,?2)",
        params![fingerprint.clone(), archive.clone()],
    )
    .await?;
    drop(c);
    drop(old);
    let db = Database::open_local(&path).await?;
    assert!(db.migration_backup().is_some());
    let c = db.connect().await?;
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM episodes WHERE tvdb_id IS NULL AND overview IS NULL"
        )
        .await,
        1
    );
    assert_eq!(
        scalar(&c, "SELECT episode_metadata_version FROM snapshot_imports").await,
        0
    );
    let preview = snapshots::import(&db, Application::Sonarr, bytes.clone(), true).await?;
    assert!(!preview.applied);
    assert_eq!(preview.metadata_backfilled, 1);
    assert_eq!(
        scalar(&c, "SELECT episode_metadata_version FROM snapshot_imports").await,
        0
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM episodes WHERE overview IS NULL").await,
        1
    );
    c.execute("UPDATE episodes SET title='Local title'", ())
        .await?;
    let conflict = snapshots::import(&db, Application::Sonarr, bytes.clone(), false).await?;
    assert!(!conflict.applied);
    assert!(conflict.conflicts > 0);
    c.execute(
        "UPDATE episodes SET title='Leap',overview='Local metadata'",
        (),
    )
    .await?;
    assert!(
        !snapshots::import(&db, Application::Sonarr, bytes.clone(), false)
            .await?
            .applied
    );
    assert_eq!(
        scalar(&c, "SELECT episode_metadata_version FROM snapshot_imports").await,
        0
    );
    c.execute("UPDATE episodes SET overview=NULL", ()).await?;
    let applied = snapshots::import(&db, Application::Sonarr, bytes.clone(), false).await?;
    assert!(applied.applied);
    assert_eq!(applied.metadata_backfilled, 1);
    assert!(applied.unsupported.iter().any(|u|u.table=="Episodes" && u.columns.iter().any(|c|c.contains("unknown cover"))));
    assert!(!serde_json::to_string(&applied)?.contains("SENTINEL_PRIVATE_COVER"));
    assert_eq!(
        scalar(&c, "SELECT episode_metadata_version FROM snapshot_imports").await,
        1
    );
    let row=c.query("SELECT tvdb_id,air_date,air_date_utc,last_search_time,runtime,finale_type,overview,absolute_episode_number,scene_absolute_episode_number,scene_episode_number,scene_season_number,unverified_scene_numbering,images_json FROM episodes WHERE id=20",()).await?.next().await?.unwrap();
    assert_eq!(row.get::<i64>(0)?, 999);
    assert_eq!(row.get::<String>(1)?, "2024-02-29");
    assert_eq!(row.get::<String>(2)?, "2024-02-29T18:30:00Z");
    assert_eq!(row.get::<String>(3)?, "2024-03-01T01:02:03.123Z");
    assert_eq!(row.get::<i64>(4)?, 45);
    assert_eq!(row.get::<String>(5)?, "season");
    assert_eq!(row.get::<String>(6)?, "Episode overview");
    for (col, expected) in [(7, 10), (8, 20), (9, 2), (10, 3), (11, 1)] {
        assert_eq!(row.get::<i64>(col)?, expected);
    }
    let images = row.get::<String>(12)?;
    assert!(!images.contains("SENTINEL_PRIVATE_COVER"));
    assert!(!images.contains("apiKey"));
    assert!(images.contains("screenshot"));
    // libSQL Row retains its statement; release this observer before a second connection writes.
    drop(row);
    assert_eq!(
        c.query(
            "SELECT record_json FROM snapshot_records WHERE source_table='Episodes'",
            ()
        )
        .await?
        .next()
        .await?
        .unwrap()
        .get::<String>(0)?,
        archive
    );
    assert_eq!(
        snapshots::import(&db, Application::Sonarr, bytes.clone(), false)
            .await?
            .metadata_backfilled,
        0
    );
    c.execute("UPDATE episodes SET overview=NULL", ()).await?;
    let replay = snapshots::import(&db, Application::Sonarr, bytes.clone(), false).await?;
    assert!(!replay.applied);
    assert!(replay.conflicts > 0);
    assert_eq!(replay.metadata_backfilled, 0);
    c.execute("UPDATE episodes SET overview='Episode overview'", ())
        .await?;
    for (i, invalid) in [
        SOURCE.replace("2024-02-29','", "2024-02-30','"),
        SOURCE.replace("20:30:00+02:00", "25:30:00+02:00"),
        SOURCE.replace("45,'season'", "-1,'season'"),
        SOURCE.replace("\"coverType\":4", "\"coverType\":99"),
    ]
    .into_iter()
    .enumerate()
    {
        let invalid = source(&files, &format!("bad-{i}.db"), &invalid).await;
        assert!(
            snapshots::import(&db, Application::Sonarr, invalid, false)
                .await
                .is_err()
        );
    }
    assert_eq!(std::fs::read(files.0.join("source.db"))?, bytes);
    drop(c);
    drop(db);
    let reopened = Database::open_local(path).await?;
    assert_eq!(
        scalar(
            &reopened.connect().await?,
            "SELECT tvdb_id FROM episodes WHERE id=20"
        )
        .await,
        999
    );
    // All schema5 columns/index and replay marker roll back as one DDL transaction.
    let raw = libsql::Builder::new_local(":memory:").build().await?;
    let c = raw.connect()?;
    for sql in [
        include_str!("../migrations/0001_prototype.sql"),
        include_str!("../migrations/0002_media_relations.sql"),
        include_str!("../migrations/0003_snapshot_imports.sql"),
    ] {
        c.execute_batch(sql).await?;
    }
    let tx = c.transaction().await?;
    tx.execute_batch(include_str!("../migrations/0005_episode_metadata.sql"))
        .await?;
    assert!(
        tx.execute(
            "INSERT INTO episodes(series_id,season,number,title,runtime)VALUES(1,1,1,'bad',-1)",
            ()
        )
        .await
        .is_err()
    );
    tx.rollback().await?;
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM pragma_table_info('episodes') WHERE name='overview'"
        )
        .await,
        0
    );
    assert_eq!(scalar(&c,"SELECT count(*) FROM pragma_table_info('snapshot_imports') WHERE name='episode_metadata_version'").await,0);
    Ok(())
}
