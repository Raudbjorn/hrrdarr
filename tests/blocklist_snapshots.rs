use hrrdarr::{
    db::{Database, Error},
    snapshots::{self, Application},
};
use libsql::Connection;
struct Sandbox(std::path::PathBuf);
impl Sandbox {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("hrrdarr-blocklist-{}", uuid::Uuid::new_v4()));
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
    let path = files.0.join(format!("{}.db", uuid::Uuid::new_v4()));
    let db = libsql::Builder::new_local(&path).build().await.unwrap();
    let c = db.connect().unwrap();
    c.execute_batch(core).await.unwrap();
    c.execute_batch("CREATE TABLE Blocklist(Id INTEGER,SeriesId INTEGER,EpisodeIds TEXT,MovieId INTEGER,Date TEXT,PublishedDate TEXT,SourceTitle TEXT,Size INTEGER,Protocol INTEGER,Quality TEXT,Languages TEXT,Message TEXT,Indexer TEXT,Source TEXT,UnknownColumn TEXT);INSERT INTO Blocklist VALUES(1,7,'[1]',1,'2026-09-25T03:00:00.500000000+02:00',NULL,'Release',123,2,'{\"quality\":7,\"revision\":{\"version\":1,\"real\":0,\"isRepack\":false}}','[1,null]','PRIVATE_SECRET','PRIVATE_SECRET','PRIVATE_SECRET','PRIVATE_SECRET');").await.unwrap();
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
async fn both_domain_blocklist_snapshot_management_preserves_provenance_and_complete_targets()
-> Result<(), Error> {
    let files = Sandbox::new();
    let path = files.0.join("dest");
    let db = Database::open_local(&path).await?;
    let c = db.connect().await?;
    let mut sources = vec![];
    for (core, app) in [
        (TV, Application::Sonarr),
        (MOVIE, Application::Radarr),
        (OLD, Application::Radarr),
    ] {
        let bytes=fixture(&files,core,if matches!(app,Application::Sonarr){"INSERT INTO Episodes VALUES(2,7,1,2,'Two',1,0);UPDATE Blocklist SET EpisodeIds='[2,1]';"}else{""}).await;
        let before = count(&c, "SELECT count(*) FROM snapshot_imports").await;
        let dry = snapshots::import(&db, app, bytes.clone(), true).await?;
        assert!(!dry.applied);
        assert_eq!(
            count(&c, "SELECT count(*) FROM snapshot_imports").await,
            before
        );
        let r = snapshots::import(&db, app, bytes.clone(), false).await?;
        assert!(r.applied);
        assert_eq!(r.conflicts, 0);
        assert!(!serde_json::to_string(&r)?.contains("PRIVATE_SECRET"));
        assert!(
            r.unsupported
                .iter()
                .any(|u| u.table == "Blocklist" && u.columns.contains(&"Message".into()))
        );
        assert!(
            snapshots::import(&db, app, bytes.clone(), false)
                .await?
                .applied
        );
        sources.push((app, bytes));
    }
    assert_eq!(count(&c, "SELECT count(*) FROM blocklist_entries").await, 3);
    assert_eq!(
        count(&c, "SELECT count(*) FROM blocklist_episodes").await,
        2
    );
    assert_eq!(count(&c,"SELECT count(*) FROM blocklist_entries WHERE occurred_at='2026-09-25 01:00:00.5' AND quality_id=7 AND size=123").await,3);
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM blocklist_entries WHERE series_id=1 OR movie_id=1"
        )
        .await,
        2
    );
    assert!(
        c.execute("DELETE FROM blocklist_episodes", ())
            .await
            .is_err()
    );
    assert!(
        c.execute("DELETE FROM episodes WHERE id=1", ())
            .await
            .is_err()
    );
    assert!(
        c.execute("UPDATE blocklist_entries SET source_title='changed'", ())
            .await
            .is_err()
    );
    assert!(
        c.execute("UPDATE blocklist_episodes SET series_id=999", ())
            .await
            .is_err()
    );
    c.execute("DELETE FROM blocklist_entries WHERE media_type='tv'", ())
        .await?;
    assert_eq!(
        count(&c, "SELECT count(*) FROM blocklist_episodes").await,
        0
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM snapshot_blocklist WHERE removed_at IS NOT NULL"
        )
        .await,
        1
    );
    assert!(
        snapshots::import(&db, sources[0].0, sources[0].1.clone(), false)
            .await?
            .applied
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM blocklist_entries WHERE media_type='tv'"
        )
        .await,
        0
    );
    // A different snapshot fingerprint is a new provenance, even if source IDs are unchanged.
    let fresh=fixture(&files,TV,"INSERT INTO Episodes VALUES(2,7,1,2,'Two',1,0);UPDATE Blocklist SET EpisodeIds='[2,1]',Message='OTHER_PRIVATE';").await;
    assert!(
        snapshots::import(&db, Application::Sonarr, fresh, false)
            .await?
            .applied
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM blocklist_entries WHERE media_type='tv'"
        )
        .await,
        1
    );
    // Unknown semantics and unresolved episode ownership archive the whole source record.
    for changes in [
        "UPDATE Blocklist SET EpisodeIds='[1,999]';",
        "UPDATE Blocklist SET Protocol=99;",
        "UPDATE Blocklist SET Languages='[999]';",
        "UPDATE Blocklist SET Quality='{\"quality\":999}';",
    ] {
        let bytes = fixture(&files, TV, changes).await;
        let before = count(&c, "SELECT count(*) FROM blocklist_entries").await;
        let r = snapshots::import(&db, Application::Sonarr, bytes, false).await?;
        assert!(r.applied);
        assert!(r.unsupported.iter().any(|u| {
            u.table == "Blocklist"
                && u.columns
                    .iter()
                    .any(|f| f.contains("unsupported") || f == "target.unresolved")
        }));
        assert_eq!(
            count(&c, "SELECT count(*) FROM blocklist_entries").await,
            before
        );
    }
    for changes in [
        "UPDATE Blocklist SET EpisodeIds='not json';",
        "UPDATE Blocklist SET EpisodeIds='[1,1]';",
        "UPDATE Blocklist SET Languages='not json';",
        "UPDATE Blocklist SET Date='2026-01-01 00:00:00.1234567890';",
        "UPDATE Blocklist SET Size=-1;",
        "INSERT INTO Blocklist SELECT * FROM Blocklist;",
    ] {
        let before = count(&c, "SELECT count(*) FROM snapshot_imports").await;
        let bytes = fixture(&files, TV, changes).await;
        assert!(
            snapshots::import(&db, Application::Sonarr, bytes, false)
                .await
                .is_err()
        );
        assert_eq!(
            count(&c, "SELECT count(*) FROM snapshot_imports").await,
            before
        );
    }
    // Resource bounds and malformed facts remain errors even alongside unsupported semantics.
    let ids = (1..=10_001).collect::<Vec<i64>>();
    let oversized = format!(
        "UPDATE Blocklist SET EpisodeIds='{}'",
        serde_json::to_string(&ids)?
    );
    let repeated = format!(
        "UPDATE Blocklist SET EpisodeIds='{}'; WITH RECURSIVE n(x) AS(VALUES(2) UNION ALL SELECT x+1 FROM n WHERE x<11) INSERT INTO Blocklist SELECT x,SeriesId,EpisodeIds,MovieId,Date,PublishedDate,SourceTitle,Size,Protocol,Quality,Languages,Message,Indexer,Source,UnknownColumn FROM n CROSS JOIN Blocklist WHERE Id=1;",
        serde_json::to_string(&ids[..10_000])?
    );
    let many = "WITH RECURSIVE n(x) AS(VALUES(2) UNION ALL SELECT x+1 FROM n WHERE x<10001) INSERT INTO Blocklist SELECT x,SeriesId,EpisodeIds,MovieId,Date,PublishedDate,SourceTitle,Size,Protocol,Quality,Languages,Message,Indexer,Source,UnknownColumn FROM n CROSS JOIN Blocklist WHERE Id=1;";
    for changes in [
        oversized.as_str(),
        repeated.as_str(),
        many,
        "UPDATE Blocklist SET Protocol=99,Languages='broken';",
        "UPDATE Blocklist SET Quality='[]';",
        "UPDATE Blocklist SET Languages='[999,\"bad\"]';",
    ] {
        let before = count(&c, "SELECT count(*) FROM snapshot_imports").await;
        let bytes = fixture(&files, TV, changes).await;
        assert!(
            snapshots::import(&db, Application::Sonarr, bytes, false)
                .await
                .is_err()
        );
        assert_eq!(
            count(&c, "SELECT count(*) FROM snapshot_imports").await,
            before
        );
    }
    // Inject failure after core/archive writes but before source blocklist activation.
    c.execute_batch("CREATE TRIGGER late_blocklist BEFORE INSERT ON blocklist_entries BEGIN SELECT RAISE(ABORT,'fixture'); END;").await?;
    let bytes=fixture(&files,OLD,"UPDATE Movies SET Id=88,TmdbId=8888,Title='Late',Path='/movies/Late';UPDATE Blocklist SET MovieId=88;").await;
    let before = count(&c, "SELECT count(*) FROM snapshot_imports").await;
    assert!(
        snapshots::import(&db, Application::Radarr, bytes, false)
            .await
            .is_err()
    );
    assert_eq!(
        count(&c, "SELECT count(*) FROM snapshot_imports").await,
        before
    );
    assert_eq!(
        count(&c, "SELECT count(*) FROM movie_metadata WHERE tmdb_id=8888").await,
        0
    );
    c.execute("DROP TRIGGER late_blocklist", ()).await?;
    // Removing library membership removes its complete active record and retains the tombstone.
    c.execute("DELETE FROM library_settings WHERE movie_id=1", ())
        .await?;
    c.execute("DELETE FROM movies WHERE id=1", ()).await?;
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM snapshot_blocklist WHERE removed_at IS NOT NULL"
        )
        .await,
        2
    );
    drop(c);
    drop(db);
    let db = Database::open_local(&path).await?;
    let c = db.connect().await?;
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM snapshot_blocklist WHERE removed_at IS NOT NULL"
        )
        .await,
        2
    );
    assert_eq!(count(&c,"SELECT count(*) FROM snapshot_records WHERE source_table='Blocklist' AND record_json LIKE '%PRIVATE_SECRET%'").await>0,true);
    Ok(())
}
