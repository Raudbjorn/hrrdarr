use hrrdarr::{
    db::{Database, Error},
    snapshots::{self, Application},
};
use libsql::Connection;
static IMPORT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
use std::path::PathBuf;
struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Self {
        let p =
            std::env::temp_dir().join(format!("hrrdarr-history-snapshot-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Sandbox {
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
async fn fixture(files: &Sandbox, core: &str, changes: &str) -> Vec<u8> {
    let path = files.0.join(format!("source-{}.db", uuid::Uuid::new_v4()));
    let db = libsql::Builder::new_local(&path).build().await.unwrap();
    let c = db.connect().unwrap();
    c.execute_batch(core).await.unwrap();
    c.execute_batch("CREATE TABLE History(Id INTEGER,EpisodeId INTEGER,SeriesId INTEGER,MovieId INTEGER,Date TEXT,EventType INTEGER,SourceTitle TEXT,DownloadId TEXT,Quality TEXT,Languages TEXT,Data TEXT);
 INSERT INTO History VALUES(1,1,7,1,'2026-09-25T03:00:00.500000000+02:00',6,'Release','SABnzbd_nzo_123','{\"quality\":7,\"revision\":{\"version\":1,\"real\":0,\"isRepack\":false}}','[1,2]','{\"DownloadUrl\":\"https://private/SENTINEL_SECRET\"}');
 INSERT INTO History VALUES(2,1,7,1,'2026-09-25 01:00:00',7,NULL,NULL,NULL,'[]','{}');
 INSERT INTO History VALUES(3,1,7,1,'2026-09-25 01:00:01',0,'Unknown',NULL,NULL,NULL,'{}');
 INSERT INTO History VALUES(4,999,7,999,'2026-09-25 01:00:01',1,'Orphan',NULL,NULL,NULL,'{}');
 INSERT INTO History VALUES(5,1,7,1,'2026-09-25 01:00:02',1,'Unknown quality','https://private/?key=SENTINEL_SECRET','{\"quality\":999}','[999]','{}');").await.unwrap();
    c.execute_batch(changes).await.unwrap();
    drop(c);
    drop(db);
    std::fs::read(path).unwrap()
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
#[tokio::test]
async fn source_history_adapters_replay_namespace_and_preserve_private_unknowns()
-> Result<(), Error> {
    let _guard = IMPORT_LOCK.lock().await;
    let files = Sandbox::new();
    let db = Database::open_local(files.0.join("dest.db")).await?;
    let c = db.connect().await?;
    for (core, app, expected_kind) in [
        (TV, Application::Sonarr, "file_renamed"),
        (MOVIE, Application::Radarr, "file_deleted"),
        (OLD, Application::Radarr, "file_deleted"),
    ] {
        let bytes = fixture(&files, core, "").await;
        let before = count(&c, "SELECT count(*) FROM snapshot_imports").await;
        let dry = snapshots::import(&db, app, bytes.clone(), true).await?;
        assert!(!dry.applied);
        assert_eq!(
            count(&c, "SELECT count(*) FROM snapshot_imports").await,
            before
        );
        let report = snapshots::import(&db, app, bytes.clone(), false).await?;
        assert!(report.applied);
        assert_eq!(report.conflicts, 0);
        assert!(
            report
                .unsupported
                .iter()
                .any(|u| u.table == "History" && u.columns.contains(&"Data".into()))
        );
        for field in [
            "EventType.unsupported",
            "target.unresolved",
            "Quality.unsupported",
            "Languages.unsupported",
            "DownloadId",
        ] {
            assert!(
                report
                    .unsupported
                    .iter()
                    .any(|u| u.table == "History" && u.columns.contains(&field.to_string())),
                "missing report {field}"
            );
        }
        assert!(!serde_json::to_string(&report)?.contains("SENTINEL_SECRET"));
        let row=c.query("SELECT event_type,occurred_at,quality_id,quality_revision_json,languages_json FROM snapshot_history_events WHERE fingerprint=? AND source_id=1",[report.fingerprint.clone()]).await?.next().await?.unwrap();
        assert_eq!(row.get::<String>(0)?, expected_kind);
        assert_eq!(row.get::<String>(1)?, "2026-09-25 01:00:00.5");
        assert_eq!(row.get::<i64>(2)?, 7);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&row.get::<String>(3)?)?,
            serde_json::json!({"version":1,"real":0,"is_repack":false})
        );
        assert_eq!(row.get::<String>(4)?, "[1,2]");
        drop(row);
        assert_eq!(c.query("SELECT languages_json FROM snapshot_history_events WHERE fingerprint=? AND source_id=2",[report.fingerprint.clone()]).await?.next().await?.unwrap().get::<String>(0)?,"[]");
        let row=c.query("SELECT quality_id,download_id,languages_json FROM snapshot_history_events WHERE fingerprint=? AND source_id=5",[report.fingerprint.clone()]).await?.next().await?.unwrap();
        for i in 0..3 {
            assert_eq!(row.get_value(i)?, libsql::Value::Null);
        }
        drop(row);
        let again = snapshots::import(&db, app, bytes, false).await?;
        assert!(again.applied);
        assert_eq!(again.mapped, 0);
        assert_eq!(
            c.query(
                "SELECT count(*) FROM snapshot_history_events WHERE fingerprint=?",
                [report.fingerprint.clone()]
            )
            .await?
            .next()
            .await?
            .unwrap()
            .get::<i64>(0)?,
            3
        );
        assert!(c.query("SELECT record_json FROM snapshot_records WHERE fingerprint=? AND source_table='History' AND ordinal=0",[report.fingerprint]).await?.next().await?.unwrap().get::<String>(0)?.contains("SENTINEL_SECRET"));
    }
    assert_eq!(
        count(&c, "SELECT count(*) FROM snapshot_history_events").await,
        9
    );
    assert_eq!(count(&c, "SELECT count(*) FROM import_history").await, 0);
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM snapshot_history_events WHERE source_id=1"
        )
        .await,
        3
    );
    assert!(
        c.execute(
            "UPDATE snapshot_history_events SET source_title='changed'",
            ()
        )
        .await
        .is_err()
    );
    assert!(
        c.execute("DELETE FROM snapshot_history_events", ())
            .await
            .is_err()
    );
    let source = fixture(
        &files,
        TV,
        "CREATE TABLE MoreConfig(Id INTEGER);INSERT INTO MoreConfig VALUES(1);",
    )
    .await;
    let other = snapshots::import(&db, Application::Sonarr, source, false).await?;
    assert!(other.applied);
    assert_eq!(
        count(&c, "SELECT count(*) FROM snapshot_history_events").await,
        12,
        "distinct fingerprints retain distinct provenance; no unsupported source-instance deduplication"
    );
    Ok(())
}
#[tokio::test]
async fn malformed_history_and_late_failure_roll_back_every_destination_write() -> Result<(), Error>
{
    let _guard = IMPORT_LOCK.lock().await;
    let files = Sandbox::new();
    let db = Database::open_local(files.0.join("dest.db")).await?;
    let c = db.connect().await?;
    for sql in [
        "UPDATE History SET Date='invalid' WHERE Id=1",
        "UPDATE History SET Date='2026-09-25T01:00:00.1234567891Z' WHERE Id=1",
        "UPDATE History SET SeriesId=8 WHERE Id=1",
        "UPDATE History SET Quality='malformed' WHERE Id=1",
        "UPDATE History SET Languages='malformed' WHERE Id=1",
        "UPDATE History SET Date='2026-09-25T01:00:60Z' WHERE Id=1",
        "UPDATE History SET Languages='[1,1]' WHERE Id=1",
        "UPDATE History SET Id=1 WHERE Id=2",
    ] {
        let source = fixture(&files, TV, sql).await;
        let error = snapshots::import(&db, Application::Sonarr, source, false)
            .await
            .unwrap_err();
        assert!(!error.to_string().contains("SENTINEL_SECRET"));
        assert_eq!(count(&c, "SELECT count(*) FROM snapshot_imports").await, 0);
        assert_eq!(count(&c, "SELECT count(*) FROM episodes").await, 0);
    }
    let source = fixture(&files, TV, "").await;
    c.execute_batch("CREATE TRIGGER injected_late_history_failure BEFORE INSERT ON snapshot_history_events WHEN NEW.source_id=2 BEGIN SELECT RAISE(ABORT,'fixture');END;").await?;
    assert!(
        snapshots::import(&db, Application::Sonarr, source.clone(), false)
            .await
            .is_err()
    );
    for table in [
        "snapshot_imports",
        "snapshot_records",
        "snapshot_mappings",
        "episodes",
        "snapshot_history_events",
    ] {
        assert_eq!(count(&c, &format!("SELECT count(*) FROM {table}")).await, 0);
    }
    c.execute("DROP TRIGGER injected_late_history_failure", ())
        .await?;
    assert!(
        snapshots::import(&db, Application::Sonarr, source.clone(), false)
            .await?
            .applied
    );
    // A previously activated missing fact is a replay conflict, never silently recreated.
    c.execute_batch("DROP TRIGGER snapshot_history_retained;DELETE FROM snapshot_history_events WHERE source_id=1;").await?;
    let report = snapshots::import(&db, Application::Sonarr, source, false).await?;
    assert!(!report.applied);
    assert_eq!(report.conflicts, 1);
    assert_eq!(
        count(&c, "SELECT count(*) FROM snapshot_history_events").await,
        2
    );
    Ok(())
}
