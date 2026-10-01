//! Real SQLite backups exercise raw decoding, transactional planning and durable native intent.
use hrrdarr::{
    db::Database,
    snapshots::{self, Application},
};
use libsql::Connection;
use std::path::PathBuf;
static IMPORT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "hrrdarr-collection-snapshot-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
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
const MODERN: &str = r#"
CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES(242);
CREATE TABLE MovieMetadata(Id INTEGER,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER,CollectionTmdbId INTEGER,CollectionTitle TEXT);
INSERT INTO MovieMetadata VALUES(1,101,NULL,'Owned',2020,900,'Collection'),(2,102,NULL,'Catalog only',2021,900,'Collection');
CREATE TABLE Movies(Id INTEGER,MovieMetadataId INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER,QualityProfileId INTEGER);
INSERT INTO Movies VALUES(1,1,'/films/Owned',1,0,7);
CREATE TABLE MovieFiles(Id INTEGER,MovieId INTEGER,RelativePath TEXT,Edition TEXT);
CREATE TABLE RootFolders(Id INTEGER,Path TEXT);INSERT INTO RootFolders VALUES(5,'/films');
CREATE TABLE Tags(Id INTEGER,Label TEXT);INSERT INTO Tags VALUES(3,'collection');
CREATE TABLE CustomFormats(Id INTEGER,Name TEXT);
CREATE TABLE QualityProfiles(Id INTEGER,Name TEXT,Items TEXT,UpgradeAllowed INTEGER,Cutoff INTEGER,MinFormatScore INTEGER,CutoffFormatScore INTEGER,MinUpgradeFormatScore INTEGER,FormatItems TEXT,Language INTEGER);
-- Native whole-profile validation requires a positive minimum upgrade score.
-- Zero makes the source profile unsupported, so it cannot establish the resolved-defaults fixture.
INSERT INTO QualityProfiles VALUES(7,'Films','[{"quality":1,"allowed":true,"items":[]}]',1,1,0,0,1,'[]',-2);
CREATE TABLE Collections(Id INTEGER,TmdbId INTEGER,Title TEXT,SortTitle TEXT,CleanTitle TEXT,Overview TEXT,Images TEXT,Monitored INTEGER,QualityProfileId INTEGER,RootFolderPath TEXT,MinimumAvailability INTEGER,SearchOnAdd INTEGER,Added TEXT,LastInfoSync TEXT,Tags TEXT,PrivateFutureField TEXT);
INSERT INTO Collections VALUES(9,900,'Collection','collection','collection','Overview','[{"coverType":"poster","remoteUrl":"https://image.tmdb.org/t/p/original/test.jpg","url":"/private/cache"}]',1,7,'/films',3,1,'2020-01-01 00:00:00','2020-02-01T00:00:00Z','[3]','PRIVATE_COLLECTION_SENTINEL');
CREATE TABLE ImportExclusions(Id INTEGER,TmdbId INTEGER,MovieTitle TEXT,MovieYear INTEGER);INSERT INTO ImportExclusions VALUES(1,102,'Catalog only',2021),(2,999,'Never added',0);
"#;
const OLD: &str = r#"
CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES(206);
CREATE TABLE Movies(Id INTEGER,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER,Collection TEXT);
INSERT INTO Movies VALUES(1,201,NULL,'Inline',2000,'/old/Inline',1,0,'{"tmdbId":901,"name":"Old collection","future":"private"}');
CREATE TABLE MovieFiles(Id INTEGER,MovieId INTEGER,RelativePath TEXT,Edition TEXT);
CREATE TABLE ImportExclusions(Id INTEGER,TmdbId INTEGER,MovieTitle TEXT,MovieYear INTEGER);INSERT INTO ImportExclusions VALUES(1,202,NULL,NULL);
"#;
const TV: &str = r#"
CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES(233);
CREATE TABLE Series(Id INTEGER,TvdbId INTEGER,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,Seasons TEXT);INSERT INTO Series VALUES(1,101,'TV',2020,'/tv/Show',1,'[{"seasonNumber":1,"monitored":true}]');
CREATE TABLE Episodes(Id INTEGER,SeriesId INTEGER,SeasonNumber INTEGER,EpisodeNumber INTEGER,Title TEXT,Monitored INTEGER,EpisodeFileId INTEGER);INSERT INTO Episodes VALUES(1,1,1,1,'Pilot',1,0);
CREATE TABLE EpisodeFiles(Id INTEGER,SeriesId INTEGER,RelativePath TEXT);
"#;
async fn source(s: &Scratch, sql: &str, change: &str, encoding: Option<&str>) -> Vec<u8> {
    let path = s.0.join(format!("{}.db", uuid::Uuid::new_v4()));
    let db = libsql::Builder::new_local(&path).build().await.unwrap();
    let c = db.connect().unwrap();
    if let Some(encoding) = encoding {
        c.execute_batch(&format!("PRAGMA encoding='{encoding}';"))
            .await
            .unwrap();
    }
    c.execute_batch(sql).await.unwrap();
    c.execute_batch(change).await.unwrap();
    drop(c);
    drop(db);
    std::fs::read(path).unwrap()
}
#[tokio::test]
async fn modern_dry_run_catalog_defaults_exclusions_and_native_replay_intent() {
    let _guard = IMPORT_LOCK.lock().await;
    let s = Scratch::new();
    let bytes = source(&s, MODERN, "", None).await;
    let db = Database::open_local(s.0.join("native.db")).await.unwrap();
    let c = db.connect().await.unwrap();
    let preview = snapshots::import(&db, Application::Radarr, bytes.clone(), true)
        .await
        .unwrap();
    assert!(!preview.applied);
    assert_eq!(preview.conflicts, 0);
    assert_eq!(scalar(&c, "SELECT count(*) FROM snapshot_imports").await, 0);
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM movie_collections").await,
        0
    );
    let applied = snapshots::import(&db, Application::Radarr, bytes.clone(), false)
        .await
        .unwrap();
    assert!(applied.applied);
    assert_eq!(preview.mapped, applied.mapped);
    assert_eq!(scalar(&c, "SELECT count(*) FROM movies").await, 1);
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM movie_collection_members").await,
        2
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM snapshot_mappings WHERE destination_table='quality_profiles'"
        )
        .await,
        1,
        "fixture profile must be fully supported"
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM snapshot_mappings WHERE destination_table='root_folders'"
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM snapshot_mappings WHERE destination_table='tags'"
        )
        .await,
        1
    );
    assert_eq!(scalar(&c,"SELECT count(*) FROM movie_collection_settings WHERE monitored=1 AND minimum_availability='released' AND search_on_add=1 AND root_folder_id IS NOT NULL AND quality_profile_id IS NOT NULL").await,1);
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM movie_collection_tags").await,
        1
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM movie_import_exclusions WHERE excluded=1"
        )
        .await,
        2
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM movie_collections WHERE graph_complete=0 AND metadata_revision=0"
        )
        .await,
        1
    );
    assert!(
        applied
            .unsupported
            .iter()
            .any(|v| v.columns.iter().any(|c| c == "PrivateFutureField"))
    );
    assert!(
        !serde_json::to_string(&applied)
            .unwrap()
            .contains("PRIVATE_COLLECTION_SENTINEL")
    );
    // Persist actual native local-intent markers; replay must not infer absent policy is pristine.
    c.execute_batch("UPDATE movie_collection_settings SET monitored=0,local_edit=1,settings_revision=settings_revision+1;UPDATE movie_collection_intents SET local_edit=1,revision=revision+1;UPDATE movie_import_exclusions SET excluded=0,local_edit=1,revision=revision+1 WHERE tmdb_id=102;").await.unwrap();
    assert!(
        snapshots::import(&db, Application::Radarr, bytes, false)
            .await
            .unwrap()
            .applied
    );
    let changed = source(
        &s,
        MODERN,
        "UPDATE Collections SET Overview='Changed source';",
        None,
    )
    .await;
    assert!(
        snapshots::import(&db, Application::Radarr, changed, false)
            .await
            .unwrap()
            .applied
    );
    assert_eq!(
        scalar(&c, "SELECT monitored FROM movie_collection_settings").await,
        0
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT excluded FROM movie_import_exclusions WHERE tmdb_id=102"
        )
        .await,
        0
    );
    c.execute_batch("DELETE FROM movie_collection_tags;DELETE FROM movie_collection_settings;DELETE FROM movie_collection_members;DELETE FROM movie_collections;UPDATE movie_collection_intents SET removed=1,removal_reason='metadata_missing',revision=revision+1;").await.unwrap();
    let changed = source(
        &s,
        MODERN,
        "UPDATE Collections SET Overview='Another source';",
        None,
    )
    .await;
    assert!(
        snapshots::import(&db, Application::Radarr, changed, false)
            .await
            .unwrap()
            .applied
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM movie_collections").await,
        0
    );
    assert_eq!(scalar(&c, "SELECT count(*) FROM movies").await, 1);
}
#[tokio::test]
async fn inline_and_unresolved_modern_preserve_both_domains_after_reopen() {
    let _guard = IMPORT_LOCK.lock().await;
    let s = Scratch::new();
    let path = s.0.join("native.db");
    let db = Database::open_local(&path).await.unwrap();
    let tv = source(&s, TV, "", None).await;
    assert!(
        snapshots::import(&db, Application::Sonarr, tv, false)
            .await
            .unwrap()
            .applied
    );
    let old = source(&s, OLD, "", None).await;
    let report = snapshots::import(&db, Application::Radarr, old, false)
        .await
        .unwrap();
    assert!(report.applied);
    assert!(
        report
            .unsupported
            .iter()
            .any(|v| v.columns.iter().any(|c| c.contains("unknown fields")))
    );
    let modern = source(
        &s,
        MODERN,
        "UPDATE Collections SET QualityProfileId=999;",
        None,
    )
    .await;
    assert!(
        snapshots::import(&db, Application::Radarr, modern, false)
            .await
            .unwrap()
            .applied
    );
    drop(db);
    let db = Database::open_local(&path).await.unwrap();
    let c = db.connect().await.unwrap();
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM movie_collections").await,
        2
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM movie_collection_settings WHERE monitored=0"
        )
        .await,
        2
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM episodes WHERE monitored=1 AND season=1 AND number=1"
        )
        .await,
        1
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM movie_collection_members").await,
        3
    );
    assert_eq!(scalar(&c,"SELECT count(*) FROM movie_import_exclusions WHERE tmdb_id=202 AND title IS NULL AND year IS NULL").await,1);
}
#[tokio::test]
async fn recognized_raw_text_is_validated_before_lossy_driver_extraction() {
    let _guard = IMPORT_LOCK.lock().await;
    let s = Scratch::new();
    let db = Database::open_local(s.0.join("native.db")).await.unwrap();
    for (base, change) in [
        (
            MODERN,
            "UPDATE Collections SET Title=CAST(x'436f6c6c656374696f6e00626164' AS TEXT);",
        ),
        (
            MODERN,
            "UPDATE Collections SET Overview=CAST(x'ff' AS TEXT);",
        ),
        (MODERN, "UPDATE Collections SET Tags=x'5b335d';"),
        (
            MODERN,
            "UPDATE ImportExclusions SET MovieTitle=CAST(x'580059' AS TEXT);",
        ),
        (
            OLD,
            "UPDATE Movies SET Collection=Collection||char(0)||'ignored';",
        ),
        (OLD, "UPDATE Movies SET Collection=CAST(x'ff' AS TEXT);"),
        (
            MODERN,
            "UPDATE Collections SET Overview=CAST(zeroblob(1048577) AS TEXT);",
        ),
    ] {
        let bytes = source(&s, base, change, None).await;
        assert!(
            snapshots::import(&db, Application::Radarr, bytes, false)
                .await
                .is_err(),
            "{change}"
        );
    }
    let c = db.connect().await.unwrap();
    assert_eq!(scalar(&c, "SELECT count(*) FROM snapshot_imports").await, 0);
    // UTF16 BLOB bytes contain zero octets for valid characters; decoding must use DB encoding.
    for encoding in ["UTF-16le", "UTF-16be"] {
        let isolated = Database::open_local(s.0.join(format!("{encoding}.db")))
            .await
            .unwrap();
        let bytes = source(
            &s,
            MODERN,
            "UPDATE Collections SET Title='Ísland collection';",
            Some(encoding),
        )
        .await;
        assert!(
            snapshots::import(&isolated, Application::Radarr, bytes, false)
                .await
                .unwrap()
                .applied
        );
    }
    let tv = source(
        &s,
        TV,
        "CREATE TABLE Collections(Id INTEGER,Title BLOB);INSERT INTO Collections VALUES(1,x'ff');",
        None,
    )
    .await;
    let report = snapshots::import(&db, Application::Sonarr, tv, false)
        .await
        .unwrap();
    assert!(report.applied);
    assert!(report.unsupported.iter().any(|v| v.table == "Collections"));
}
#[tokio::test]
async fn late_failure_rolls_back_core_archive_and_collection_activation() {
    let _guard = IMPORT_LOCK.lock().await;
    let s = Scratch::new();
    let db = Database::open_local(s.0.join("native.db")).await.unwrap();
    let c = db.connect().await.unwrap();
    c.execute_batch("CREATE TRIGGER fixture_collection_failure BEFORE INSERT ON movie_import_exclusions BEGIN SELECT RAISE(ABORT,'owned fixture failure');END;").await.unwrap();
    let bytes = source(&s, MODERN, "", None).await;
    assert!(
        snapshots::import(&db, Application::Radarr, bytes.clone(), false)
            .await
            .is_err()
    );
    for table in [
        "snapshot_imports",
        "snapshot_mappings",
        "movie_collections",
        "movie_collection_settings",
        "movie_collection_members",
        "movies",
        "movie_metadata",
    ] {
        assert_eq!(
            scalar(&c, &format!("SELECT count(*) FROM {table}")).await,
            0,
            "{table}"
        );
    }
    c.execute_batch("DROP TRIGGER fixture_collection_failure;")
        .await
        .unwrap();
    assert!(
        snapshots::import(&db, Application::Radarr, bytes, false)
            .await
            .unwrap()
            .applied
    );
    // A malformed complete source graph is rejected before partial publication.
    let duplicate=source(&s,MODERN,"INSERT INTO Collections SELECT 10,TmdbId,Title,SortTitle,CleanTitle,Overview,Images,Monitored,QualityProfileId,RootFolderPath,MinimumAvailability,SearchOnAdd,Added,LastInfoSync,Tags,PrivateFutureField FROM Collections;",None).await;
    assert!(
        snapshots::import(&db, Application::Radarr, duplicate, false)
            .await
            .is_err()
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM movie_collections").await,
        1
    );
    let overflow=source(&s,MODERN,"WITH RECURSIVE n(x)AS(VALUES(3) UNION ALL SELECT x+1 FROM n WHERE x<1001) INSERT INTO MovieMetadata SELECT x,10000+x,NULL,'Extra '||x,2020,900,'Collection' FROM n;",None).await;
    assert!(
        snapshots::import(&db, Application::Radarr, overflow, false)
            .await
            .is_err()
    );
    let count = c
        .query("SELECT collection_version FROM snapshot_imports", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<i64>(0)
        .unwrap();
    assert_eq!(count, 1);
}
