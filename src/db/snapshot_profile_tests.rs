use super::*;
use crate::snapshots::{self, Application};
struct Sandbox(PathBuf);
impl Drop for Sandbox {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
async fn source(files: &Path, version: i64) -> Result<Vec<u8>, Error> {
    let path = files.join(format!("source-{version}.db"));
    let db = libsql::Builder::new_local(&path).build().await?;
    let c = db.connect()?;
    c.execute_batch(&format!("CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES({version}); CREATE TABLE CustomFormats(Id INTEGER);")).await?;
    let core = match version {
        233 => {
            "CREATE TABLE Series(Id INTEGER,TvdbId INTEGER,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,Seasons TEXT,QualityProfileId INTEGER);INSERT INTO Series VALUES(1,777,'TV',2020,'/tv/TV',1,'[{\"seasonNumber\":1,\"monitored\":true}]',7);CREATE TABLE Episodes(Id INTEGER,SeriesId INTEGER,SeasonNumber INTEGER,EpisodeNumber INTEGER,Title TEXT,Monitored INTEGER,EpisodeFileId INTEGER);INSERT INTO Episodes VALUES(1,1,1,1,'One',1,0);CREATE TABLE EpisodeFiles(Id INTEGER,SeriesId INTEGER,RelativePath TEXT);"
        }
        206 => {
            "CREATE TABLE Movies(Id INTEGER,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER,ProfileId INTEGER);INSERT INTO Movies VALUES(1,206,NULL,'Old',2000,'/movies/Old',1,0,7);CREATE TABLE MovieFiles(Id INTEGER,MovieId INTEGER,RelativePath TEXT,Edition TEXT);"
        }
        _ => {
            "CREATE TABLE MovieMetadata(Id INTEGER,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER);INSERT INTO MovieMetadata VALUES(1,242,NULL,'New',2000);CREATE TABLE Movies(Id INTEGER,MovieMetadataId INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER,QualityProfileId INTEGER);INSERT INTO Movies VALUES(1,1,'/movies/New',1,0,7);CREATE TABLE MovieFiles(Id INTEGER,MovieId INTEGER,RelativePath TEXT,Edition TEXT);"
        }
    };
    c.execute_batch(core).await?;
    let table = if version == 206 {
        "Profiles"
    } else {
        "QualityProfiles"
    };
    c.execute_batch(&format!("CREATE TABLE {table}(Id INTEGER,Name TEXT,Items TEXT,Cutoff INTEGER,UpgradeAllowed INTEGER,MinFormatScore INTEGER,CutoffFormatScore INTEGER,FormatItems TEXT{}{});",if version==233{""}else{",Language INTEGER"},if version==206{""}else{",MinUpgradeFormatScore INTEGER"})).await?;
    let items = r#"[{"quality":1,"allowed":true,"items":[]},{"id":1000,"name":"Group","allowed":true,"items":[{"quality":7,"allowed":true,"items":[]}]}]"#;
    let mut values = vec![
        libsql::Value::Integer(7),
        "Imported".into(),
        items.into(),
        1000.into(),
        1.into(),
        0.into(),
        0.into(),
        "[]".into(),
    ];
    if version != 233 {
        values.push((-2).into());
    }
    if version != 206 {
        values.push(1.into());
    }
    c.execute(
        &format!(
            "INSERT INTO {table} VALUES({})",
            vec!["?"; values.len()].join(",")
        ),
        values,
    )
    .await?;
    drop(c);
    drop(db);
    Ok(std::fs::read(path)?)
}
#[tokio::test]
async fn snapshot_profile_schema19_backfill_rollback_reopen_and_intact_mapping_guards()
-> Result<(), Error> {
    let _guard = crate::snapshots::IMPORT_TEST_LOCK.lock().await;
    let files = Sandbox(std::env::temp_dir().join(format!(
        "hrrdarr-snapshot-profiles-{}",
        uuid::Uuid::new_v4()
    )));
    std::fs::create_dir(&files.0)?;
    let sources = [
        (Application::Sonarr, source(&files.0, 233).await?),
        (Application::Radarr, source(&files.0, 206).await?),
        (Application::Radarr, source(&files.0, 242).await?),
    ];
    let path = files.0.join("old.db");
    let raw = libsql::Builder::new_local(&path).build().await?;
    let c = raw.connect()?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    c.execute(HISTORY_SQL, ()).await?;
    for (i, (name, sql)) in MIGRATIONS.iter().take(19).enumerate() {
        c.execute_batch(sql).await?;
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
            params![i as i64 + 1, *name, checksum(sql), *sql],
        )
        .await?;
    }
    let tx = c.transaction().await?;
    for (app, bytes) in &sources {
        snapshots::write_core_snapshot_fixture(&tx, *app, bytes.clone()).await?;
    }
    tx.commit().await?;
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM library_settings WHERE quality_profile_id IS NULL"
        )
        .await?,
        3
    );
    let tx = c.transaction().await?;
    tx.execute_batch(MIGRATIONS[19].1).await?;
    assert!(tx.execute_batch(MIGRATIONS[19].1).await.is_err());
    tx.rollback().await?;
    assert_eq!(version(&c).await?, 19);
    assert!(
        c.query("SELECT profile_version FROM snapshot_imports", ())
            .await
            .is_err()
    );
    drop(c);
    drop(raw);
    for (case, change) in [
        ("valid", ""),
        (
            "missing_mapping",
            "DELETE FROM snapshot_mappings WHERE application='sonarr' AND destination_table='library_settings'",
        ),
        (
            "missing_row",
            "DELETE FROM library_settings WHERE media_type='tv'",
        ),
        (
            "changed_assignment",
            "INSERT INTO quality_profiles(id,media_type,name) VALUES(99,'tv','Local');UPDATE library_settings SET quality_profile_id=99 WHERE media_type='tv'",
        ),
    ] {
        let copy = files.0.join(format!("{case}.db"));
        std::fs::copy(&path, &copy)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&copy, std::fs::Permissions::from_mode(0o600))?;
        }
        let db = Database::open_local(&copy).await?;
        assert!(db.migration_backup().is_some());
        let c = db.connect().await?;
        assert_eq!(version(&c).await?, 25); // Latest open adds RSS intent/catalog contracts; predecessor stays fixed.
        assert_eq!(
            scalar(&c, "SELECT count(*) FROM quality_profile_policies").await?,
            0
        );
        assert_eq!(
            scalar(&c, "SELECT sum(profile_version) FROM snapshot_imports").await?,
            0
        );
        c.execute_batch(change).await?;
        for (app, bytes) in &sources {
            let report = snapshots::import(&db, *app, bytes.clone(), false).await?;
            if case != "valid" && matches!(app, Application::Sonarr) {
                assert!(!report.applied);
                assert!(report.conflicts > 0);
                assert_eq!(
                    scalar(
                        &c,
                        "SELECT count(*) FROM quality_profiles WHERE name='Imported'"
                    )
                    .await?,
                    0
                );
                break;
            }
            assert!(report.applied);
            assert_eq!(report.metadata_backfilled, 1);
            let repeat = snapshots::import(&db, *app, bytes.clone(), false).await?;
            assert!(repeat.applied);
            assert_eq!(repeat.mapped, 0);
            assert_eq!(repeat.metadata_backfilled, 0);
        }
        if case == "valid" {
            assert_eq!(
                scalar(&c, "SELECT count(*) FROM quality_profiles").await?,
                2
            );
            assert_eq!(
                scalar(
                    &c,
                    "SELECT count(*) FROM library_settings WHERE quality_profile_id IS NOT NULL"
                )
                .await?,
                3
            );
            assert_eq!(
                scalar(&c, "SELECT count(*) FROM episodes WHERE id=1").await?,
                1
            );
            assert_eq!(
                scalar(&c, "SELECT count(*) FROM movies WHERE id=1").await?,
                1
            );
        }
        integrity(&c).await?;
        drop(c);
        drop(db);
        let db = Database::open_local(&copy).await?;
        assert!(db.migration_backup().is_none());
        let c = db.connect().await?;
        if case == "valid" {
            assert_eq!(
                scalar(&c, "SELECT sum(profile_version) FROM snapshot_imports").await?,
                3
            );
            assert_eq!(
                scalar(
                    &c,
                    "SELECT count(*) FROM library_settings WHERE quality_profile_id IS NOT NULL"
                )
                .await?,
                3
            );
        }
        if case == "changed_assignment" {
            assert_eq!(
                scalar(
                    &c,
                    "SELECT quality_profile_id FROM library_settings WHERE media_type='tv'"
                )
                .await?,
                99
            );
        }
        integrity(&c).await?;
    }
    Ok(())
}
