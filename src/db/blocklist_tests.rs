use super::*;
use crate::snapshots::{self, Application};
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
const TV:&str="CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES(233);
CREATE TABLE Series(Id INTEGER,TvdbId INTEGER,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,Seasons TEXT);INSERT INTO Series VALUES(7,777,'TV',2020,'/tv/TV',1,'[{\"seasonNumber\":1,\"monitored\":true}]');
CREATE TABLE Episodes(Id INTEGER,SeriesId INTEGER,SeasonNumber INTEGER,EpisodeNumber INTEGER,Title TEXT,Monitored INTEGER,EpisodeFileId INTEGER);INSERT INTO Episodes VALUES(1,7,1,1,'One',1,0);
CREATE TABLE EpisodeFiles(Id INTEGER,SeriesId INTEGER,RelativePath TEXT);";
const MOVIE:&str="CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES(242);
CREATE TABLE MovieMetadata(Id INTEGER,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER);INSERT INTO MovieMetadata VALUES(101,1001,NULL,'Film',2020);
CREATE TABLE Movies(Id INTEGER,MovieMetadataId INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER);INSERT INTO Movies VALUES(1,101,'/movies/Film',1,0);
CREATE TABLE MovieFiles(Id INTEGER,MovieId INTEGER,RelativePath TEXT,Edition TEXT);";
const OLD:&str="CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES(206);
CREATE TABLE Movies(Id INTEGER,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER);INSERT INTO Movies VALUES(1,2001,NULL,'Old',2000,'/movies/Old',1,0);
CREATE TABLE MovieFiles(Id INTEGER,MovieId INTEGER,RelativePath TEXT,Edition TEXT);";

async fn source(path: &Path, core: &str) -> Result<Vec<u8>, Error> {
    let db = libsql::Builder::new_local(path).build().await?;
    let c = db.connect()?;
    c.execute_batch(core).await?;
    c.execute_batch("CREATE TABLE Blocklist(Id INTEGER,SeriesId INTEGER,EpisodeIds TEXT,MovieId INTEGER,Date TEXT,SourceTitle TEXT);INSERT INTO Blocklist VALUES(1,7,'[1]',1,'2026-01-01 00:00:00','Release');").await?;
    drop(c);
    drop(db);
    Ok(std::fs::read(path)?)
}
#[tokio::test]
async fn blocklist_schema21_backfill_rollback_reopen_and_complete_target_constraints()
-> Result<(), Error> {
    let _guard = crate::snapshots::IMPORT_TEST_LOCK.lock().await;
    let files = Scratch(
        std::env::temp_dir().join(format!("hrrdarr-blocklist-schema-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&files.0)?;
    let sources = [
        (Application::Sonarr, source(&files.0.join("tv"), TV).await?),
        (
            Application::Radarr,
            source(&files.0.join("old"), OLD).await?,
        ),
        (
            Application::Radarr,
            source(&files.0.join("new"), MOVIE).await?,
        ),
    ];
    let path = files.0.join("dest");
    let raw = libsql::Builder::new_local(&path).build().await?;
    let c = raw.connect()?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    c.execute(HISTORY_SQL, ()).await?;
    for (i, (name, sql)) in MIGRATIONS.iter().take(21).enumerate() {
        c.execute_batch(sql).await?;
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
            params![i as i64 + 1, *name, checksum(sql), *sql],
        )
        .await?;
    }
    // Real predecessor21 core/archive, with no History or profile tables in these source fixtures.
    let tx = c.transaction().await?;
    for (app, bytes) in &sources {
        snapshots::write_core_snapshot_fixture(&tx, *app, bytes.clone()).await?;
    }
    tx.execute(
        "UPDATE snapshot_imports SET history_version=1,profile_version=1",
        (),
    )
    .await?;
    tx.commit().await?;
    let provider = super::refresh_tests::provider(&c).await?;
    let command = super::refresh_tests::enqueue(&c, &provider, "tv").await?;
    let tx = c.transaction().await?;
    tx.execute_batch(MIGRATIONS[21].1).await?;
    assert!(tx.execute_batch(MIGRATIONS[21].1).await.is_err());
    tx.rollback().await?;
    assert_eq!(version(&c).await?, 21);
    assert!(
        c.query("SELECT blocklist_version FROM snapshot_imports", ())
            .await
            .is_err()
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM snapshot_records WHERE source_table='Blocklist'"
        )
        .await?,
        3
    );
    drop(c);
    drop(raw);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    let db = Database::open_local(&path).await?;
    assert!(db.migration_backup().is_some());
    let c = db.connect().await?;
    assert_eq!(version(&c).await?, 34); // Latest open adds custom formats; fixed predecessor migrations remain unchanged.
    assert_eq!(
        c.query("SELECT id FROM commands", ())
            .await?
            .next()
            .await?
            .unwrap()
            .get::<String>(0)?,
        command
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM blocklist_entries").await?,
        0
    );
    for (app, bytes) in &sources {
        let r = snapshots::import(&db, *app, bytes.clone(), false).await?;
        assert!(r.applied);
        assert_eq!(r.conflicts, 0);
        assert!(
            snapshots::import(&db, *app, bytes.clone(), false)
                .await?
                .applied
        );
    }
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM blocklist_entries").await?,
        3
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM snapshot_imports WHERE blocklist_version=1"
        )
        .await?,
        3
    );
    assert!(
        c.execute("DELETE FROM blocklist_episodes", ())
            .await
            .is_err()
    );
    let tx = c.transaction().await?;
    tx.execute_batch("INSERT INTO series(id,title,path) VALUES(999,'Other','/other');INSERT INTO seasons VALUES(999,1,1);INSERT INTO episodes(id,series_id,season,number,title) VALUES(999,999,1,1,'Other');").await?;
    assert!(tx.execute("INSERT INTO blocklist_episodes SELECT application,fingerprint,source_id,series_id,999 FROM blocklist_entries WHERE media_type='tv'",()).await.is_err());
    tx.rollback().await?;
    for (sort, direction, index) in [
        (
            crate::blocklist::BlocklistSort::Date,
            crate::blocklist::BlocklistSortDirection::Desc,
            "blocklist_order",
        ),
        (
            crate::blocklist::BlocklistSort::SourceTitle,
            crate::blocklist::BlocklistSortDirection::Asc,
            "blocklist_title_order",
        ),
    ] {
        let sql = crate::blocklist::page_sql("", &sort, &direction);
        let mut rows = c
            .query(&format!("EXPLAIN QUERY PLAN {sql}"), [100, 0])
            .await?;
        let mut plan = vec![];
        while let Some(row) = rows.next().await? {
            plan.push(row.get::<String>(3)?);
        }
        assert!(plan.iter().any(|line| line.contains(index)), "{plan:?}");
        assert!(
            plan.iter()
                .any(|line| line.contains("SEARCH e USING INDEX")),
            "{plan:?}"
        );
    }
    // Missing activated linkage is corruption, not permission to narrow/reconstruct the source set.
    let guard_sql = c
        .query(
            "SELECT sql FROM sqlite_schema WHERE name='blocklist_episode_retained'",
            (),
        )
        .await?
        .next()
        .await?
        .unwrap()
        .get::<String>(0)?;
    c.execute("DROP TRIGGER blocklist_episode_retained", ())
        .await?;
    c.execute("DELETE FROM blocklist_episodes", ()).await?;
    let r = snapshots::import(&db, sources[0].0, sources[0].1.clone(), false).await?;
    assert!(!r.applied);
    assert!(r.conflicts > 0);
    // Restore linkage only for the isolated fixture, then exercise target cascade and durable tombstone.
    c.execute("INSERT INTO blocklist_episodes SELECT application,fingerprint,source_id,series_id,1 FROM blocklist_entries WHERE media_type='tv'",()).await?;
    c.execute_batch(&guard_sql).await?;
    c.execute("DELETE FROM blocklist_entries WHERE media_type='tv'", ())
        .await?;
    assert!(
        snapshots::import(&db, sources[0].0, sources[0].1.clone(), false)
            .await?
            .applied
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM snapshot_blocklist WHERE removed_at IS NOT NULL"
        )
        .await?,
        1
    );
    assert!(
        c.execute(
            "UPDATE snapshot_blocklist SET removed_at=NULL WHERE removed_at IS NOT NULL",
            ()
        )
        .await
        .is_err()
    );
    drop(c);
    drop(db);
    let db = Database::open_local(&path).await?;
    assert!(db.migration_backup().is_none());
    let c = db.connect().await?;
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM blocklist_entries").await?,
        2
    );
    integrity(&c).await?;
    Ok(())
}
