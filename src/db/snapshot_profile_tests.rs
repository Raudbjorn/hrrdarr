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
        assert_eq!(version(&c).await?, 36); // Latest open adds snapshot CF activation; historical migration starts remain unchanged.
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

async fn cf_source(files: &Path, version: i64, change: &str) -> Result<Vec<u8>, Error> {
    source(files, version).await?;
    let path = files.join(format!("source-{version}.db"));
    let raw = libsql::Builder::new_local(&path).build().await?;
    let c = raw.connect()?;
    c.execute_batch("ALTER TABLE CustomFormats ADD COLUMN Name TEXT; ALTER TABLE CustomFormats ADD COLUMN Specifications TEXT; ALTER TABLE CustomFormats ADD COLUMN IncludeCustomFormatWhenRenaming INTEGER;").await?;
    let spec = r#"[{"type":"LanguageSpecification","body":{"name":"English","negate":false,"required":true,"value":1,"exceptLanguage":false,"order":3,"implementationName":"Language","infoLink":"https://example.invalid"}}]"#;
    c.execute("INSERT INTO CustomFormats VALUES(7,'English',?,1)", [spec])
        .await?;
    let table = if version == 206 {
        "Profiles"
    } else {
        "QualityProfiles"
    };
    c.execute_batch(&format!(
        "UPDATE {table} SET FormatItems='[{{\"format\":7,\"score\":10}}]', MinFormatScore=10;"
    ))
    .await?;
    c.execute_batch(change).await?;
    drop(c);
    drop(raw);
    Ok(std::fs::read(path)?)
}
#[tokio::test]
async fn snapshot_custom_formats_three_layouts_atomic_replay_and_zero_scores() -> Result<(), Error>
{
    let _guard = crate::snapshots::IMPORT_TEST_LOCK.lock().await;
    let files =
        Sandbox(std::env::temp_dir().join(format!("hrrdarr-cf-snapshot-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&files.0)?;
    let db = Database::open_local(files.0.join("destination.db")).await?;
    let c = db.connect().await?;
    for (app, version) in [
        (Application::Sonarr, 233),
        (Application::Radarr, 206),
        (Application::Radarr, 242),
    ] {
        let bytes = cf_source(&files.0, version, "").await?;
        let dry = snapshots::import(&db, app, bytes.clone(), true).await?;
        assert!(!dry.applied);
        assert_eq!(dry.conflicts, 0);
        assert_eq!(
            scalar(&c, "SELECT count(*) FROM snapshot_imports").await?,
            if version == 233 {
                0
            } else if version == 206 {
                1
            } else {
                2
            }
        );
        let first = snapshots::import(&db, app, bytes.clone(), false).await?;
        assert!(first.applied, "{first:?}");
        assert!(!first.unsupported.iter().any(|u| u.table == "CustomFormats"));
        let repeat = snapshots::import(&db, app, bytes.clone(), false).await?;
        assert!(repeat.applied, "{repeat:?}");
        assert_eq!(repeat.mapped, 0);
        assert_eq!(
            bytes,
            std::fs::read(files.0.join(format!("source-{version}.db")))?
        );
    }
    assert_eq!(scalar(&c, "SELECT count(*) FROM custom_formats").await?, 2);
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM quality_profile_format_scores WHERE score=10"
        )
        .await?,
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
        scalar(
            &c,
            "SELECT sum(custom_format_version) FROM snapshot_imports"
        )
        .await?,
        3
    );
    c.execute("INSERT INTO custom_formats(media_type,name,include_when_renaming,specifications_json) SELECT media_type,'Local zero',0,specifications_json FROM custom_formats WHERE media_type='tv'",()).await?;
    let bytes = std::fs::read(files.0.join("source-233.db"))?;
    assert!(
        snapshots::import(&db, Application::Sonarr, bytes.clone(), false)
            .await?
            .applied
    );
    c.execute("INSERT INTO quality_profile_format_scores SELECT p.id,f.id,'tv',-1 FROM quality_profiles p JOIN custom_formats f ON f.media_type=p.media_type WHERE p.media_type='tv' AND f.name='Local zero'",()).await?;
    let conflict = snapshots::import(&db, Application::Sonarr, bytes.clone(), false).await?;
    assert!(!conflict.applied);
    assert!(conflict.conflicts > 0);
    c.execute(
        "UPDATE quality_profile_format_scores SET score=1 WHERE score=-1",
        (),
    )
    .await?;
    let conflict = snapshots::import(&db, Application::Sonarr, bytes, false).await?;
    assert!(!conflict.applied);
    assert!(conflict.conflicts > 0);
    integrity(&c).await?;
    Ok(())
}

#[tokio::test]
async fn snapshot_custom_formats_schema35_activation_and_replay_guards() -> Result<(), Error> {
    let _guard = crate::snapshots::IMPORT_TEST_LOCK.lock().await;
    for version in [233, 206, 242] {
        let app = if version == 233 {
            Application::Sonarr
        } else {
            Application::Radarr
        };
        let files = Sandbox(
            std::env::temp_dir().join(format!("hrrdarr-cf-upgrade-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir(&files.0)?;
        // Nonempty catalogs blocked predecessor profiles even with empty score arrays.
        let bytes = cf_source(
            &files.0,
            version,
            if version == 233 {
                "UPDATE QualityProfiles SET FormatItems='[]',MinFormatScore=0"
            } else {
                ""
            },
        )
        .await?;
        let path = files.0.join("old.db");
        let raw = libsql::Builder::new_local(&path).build().await?;
        let c = raw.connect()?;
        c.execute("PRAGMA foreign_keys=ON", ()).await?;
        c.execute(HISTORY_SQL, ()).await?;
        for (i, (name, sql)) in MIGRATIONS.iter().take(35).enumerate() {
            c.execute_batch(sql).await?;
            c.execute(
                "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
                params![i as i64 + 1, *name, checksum(sql), *sql],
            )
            .await?;
        }
        let tx = c.transaction().await?;
        snapshots::write_core_snapshot_fixture(&tx, app, bytes.clone()).await?;
        // Schema35's old CF reader archived this whole graph but completed profile activation.
        tx.execute("UPDATE snapshot_imports SET profile_version=1", ())
            .await?;
        tx.commit().await?;
        let tx = c.transaction().await?;
        tx.execute_batch(MIGRATIONS[35].1).await?;
        tx.rollback().await?;
        assert!(
            c.query("SELECT custom_format_version FROM snapshot_imports", ())
                .await
                .is_err()
        );
        drop(c);
        drop(raw);
        for (case, change) in [
            ("valid", ""),
            (
                "missing_mapping",
                "DELETE FROM snapshot_mappings WHERE destination_table='library_settings'",
            ),
            ("missing_row", "DELETE FROM library_settings"),
            (
                "local_assignment",
                "INSERT INTO quality_profiles(id,media_type,name) VALUES(99,'tv','Local');UPDATE library_settings SET quality_profile_id=99",
            ),
            (
                "late_failure",
                "CREATE TRIGGER fail_cf_finish BEFORE UPDATE OF custom_format_version ON snapshot_imports BEGIN SELECT RAISE(ABORT,'injected'); END",
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
            let c = db.connect().await?;
            assert_eq!(scalar(&c, "SELECT count(*) FROM custom_formats").await?, 0);
            assert_eq!(
                scalar(&c, "SELECT custom_format_version FROM snapshot_imports").await?,
                0
            );
            c.execute_batch(
                &change.replace("'tv'", if version == 233 { "'tv'" } else { "'movies'" }),
            )
            .await?;
            let dry = snapshots::import(&db, app, bytes.clone(), true).await;
            assert_eq!(scalar(&c, "SELECT count(*) FROM custom_formats").await?, 0);
            if case == "late_failure" {
                assert!(dry.is_err());
            } else {
                assert!(!dry?.applied);
            }
            let result = snapshots::import(&db, app, bytes.clone(), false).await;
            if case == "valid" {
                let report = result?;
                assert!(report.applied, "{report:?}");
                assert_eq!(report.metadata_backfilled, 1);
                let repeat = snapshots::import(&db, app, bytes.clone(), false).await?;
                assert!(repeat.applied);
                assert_eq!(repeat.mapped, 0);
                assert_eq!(repeat.metadata_backfilled, 0);
            } else {
                if case == "late_failure" {
                    assert!(result.is_err())
                } else {
                    let r = result?;
                    assert!(!r.applied);
                    assert!(r.conflicts > 0)
                }
                assert_eq!(scalar(&c, "SELECT count(*) FROM custom_formats").await?, 0);
                assert_eq!(
                    scalar(&c, "SELECT custom_format_version FROM snapshot_imports").await?,
                    0
                );
            }
            integrity(&c).await?;
            drop(c);
            drop(db);
            let db = Database::open_local(&copy).await?;
            let c = db.connect().await?;
            assert_eq!(
                scalar(&c, "SELECT custom_format_version FROM snapshot_imports").await?,
                i64::from(case == "valid")
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn snapshot_custom_formats_unsupported_graphs_and_deleted_supported_profiles()
-> Result<(), Error> {
    let _guard = crate::snapshots::IMPORT_TEST_LOCK.lock().await;
    let files = Sandbox(
        std::env::temp_dir().join(format!("hrrdarr-cf-unsupported-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&files.0)?;
    for (case, change, error, profiles) in [
        (
            "unknown",
            "UPDATE CustomFormats SET Specifications='[{\"type\":\"FutureSpecification\",\"body\":{\"name\":\"Future\",\"negate\":false,\"required\":false,\"value\":1}}]'",
            false,
            0,
        ),
        (
            "regex",
            "UPDATE CustomFormats SET Specifications='[{\"type\":\"ReleaseTitleSpecification\",\"body\":{\"name\":\"Regex\",\"negate\":false,\"required\":false,\"value\":\"(\"}}]'",
            false,
            0,
        ),
        (
            "bounded_regex",
            "UPDATE CustomFormats SET Specifications='[{\"type\":\"ReleaseTitleSpecification\",\"body\":{\"name\":\"Regex\",\"negate\":false,\"required\":false,\"value\":\"((?=a)){1000000000}\"}}]'",
            false,
            0,
        ),
        (
            "unresolved_zero",
            "UPDATE QualityProfiles SET FormatItems='[{\"format\":99,\"score\":0}]',MinFormatScore=0",
            false,
            0,
        ),
        (
            "unresolved_positive",
            "UPDATE QualityProfiles SET FormatItems='[{\"format\":99,\"score\":10}]'",
            false,
            0,
        ),
        (
            "unknown_profile_column",
            "ALTER TABLE QualityProfiles ADD COLUMN FuturePolicy INTEGER",
            false,
            0,
        ),
        (
            "empty_scores",
            "UPDATE QualityProfiles SET FormatItems='[]',MinFormatScore=0",
            false,
            1,
        ),
        (
            "malformed",
            "UPDATE CustomFormats SET Specifications='['",
            true,
            0,
        ),
        (
            "duplicate",
            "INSERT INTO CustomFormats SELECT * FROM CustomFormats",
            true,
            0,
        ),
    ] {
        let dir = files.0.join(case);
        std::fs::create_dir(&dir)?;
        let bytes = cf_source(&dir, 233, change).await?;
        let db = Database::open_local(dir.join("destination.db")).await?;
        let result = snapshots::import(&db, Application::Sonarr, bytes, false).await;
        let c = db.connect().await?;
        if error {
            assert!(result.is_err(), "{case}");
            assert_eq!(
                scalar(&c, "SELECT count(*) FROM snapshot_imports").await?,
                0
            )
        } else {
            let r = result?;
            assert!(r.applied, "{case}: {r:?}");
            if profiles == 0 {
                assert!(!r.unsupported.is_empty(), "{case}: {r:?}");
            }
            assert_eq!(
                scalar(&c, "SELECT count(*) FROM quality_profiles").await?,
                profiles
            );
            assert!(
                scalar(
                    &c,
                    "SELECT count(*) FROM snapshot_records WHERE source_table='CustomFormats'"
                )
                .await?
                    > 0
            );
        }
        integrity(&c).await?;
    }
    for (case, change) in [
        (
            "changed_definition",
            "UPDATE custom_formats SET include_when_renaming=0",
        ),
        ("deleted_definition", "DELETE FROM custom_formats"),
        (
            "deleted_cf_mapping",
            "DELETE FROM snapshot_mappings WHERE destination_table='custom_formats'",
        ),
        (
            "deleted_profile_mapping",
            "DELETE FROM snapshot_mappings WHERE destination_table='quality_profiles'",
        ),
        (
            "changed_score",
            "UPDATE quality_profile_format_scores SET score=11",
        ),
    ] {
        let dir = files.0.join(case);
        std::fs::create_dir(&dir)?;
        let bytes = cf_source(&dir, 233, "").await?;
        let db = Database::open_local(dir.join("destination.db")).await?;
        assert!(
            snapshots::import(&db, Application::Sonarr, bytes.clone(), false)
                .await?
                .applied
        );
        let c = db.connect().await?;
        c.execute_batch(change).await?;
        let r = snapshots::import(&db, Application::Sonarr, bytes, false).await?;
        assert!(!r.applied, "{case}");
        assert!(r.conflicts > 0, "{case}");
        if case == "deleted_definition" {
            assert_eq!(scalar(&c, "SELECT count(*) FROM custom_formats").await?, 0);
        }
        integrity(&c).await?;
    }
    // An already-supported empty-catalog profile must not be resurrected on first CF activation.
    let dir = files.0.join("deleted_supported");
    std::fs::create_dir(&dir)?;
    let bytes = source(&dir, 233).await?;
    let db = Database::open_local(dir.join("destination.db")).await?;
    assert!(
        snapshots::import(&db, Application::Sonarr, bytes.clone(), false)
            .await?
            .applied
    );
    let c = db.connect().await?;
    c.execute_batch("UPDATE snapshot_imports SET custom_format_version=0; UPDATE library_settings SET quality_profile_id=NULL; DELETE FROM snapshot_mappings WHERE destination_table='quality_profiles'; DELETE FROM quality_profiles;").await?;
    let r = snapshots::import(&db, Application::Sonarr, bytes, false).await?;
    assert!(!r.applied);
    assert!(r.conflicts > 0);
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM quality_profiles").await?,
        0
    );
    assert_eq!(
        scalar(&c, "SELECT custom_format_version FROM snapshot_imports").await?,
        0
    );
    Ok(())
}
