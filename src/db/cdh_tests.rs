use super::*;
async fn count(c: &Connection, sql: &str) -> Result<i64, Error> {
    Ok(c.query(sql, ()).await?.next().await?.unwrap().get(0)?)
}
async fn seed(c: &Connection, id: &str, media: &str) -> Result<(), Error> {
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint)VALUES(?,'qbittorrent','synthetic',1,1,1,1,'http://127.0.0.1:9')",[id]).await?;
    c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags)VALUES(?,'qbittorrent',?,?,0,0,'started','default',0,0,0)",params![id,media,media]).await?;
    Ok(())
}
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("hrrdarr-cdh-schema-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
#[tokio::test]
async fn cdh_upgrade40_false_intent_rollback_reopen_and_capacity_recovery() -> Result<(), Error> {
    let s = Scratch::new();
    let path = s.0.join("db");
    let raw = libsql::Builder::new_local(&path).build().await?;
    let c = raw.connect()?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    c.execute(HISTORY_SQL, ()).await?;
    for (i, (name, sql)) in MIGRATIONS.iter().take(40).enumerate() {
        c.execute_batch(sql).await?;
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql)VALUES(?,?,?,?)",
            params![i as i64 + 1, *name, checksum(sql), *sql],
        )
        .await?;
    }
    let kept = uuid::Uuid::new_v4().to_string();
    seed(&c, &kept, "tv").await?;
    c.execute(
        "INSERT INTO download_processing_policies VALUES(?,'tv',1,1,1,'hardlink')",
        [kept.clone()],
    )
    .await?;
    c.execute("INSERT INTO download_refresh_schedules(provider_id,media_type,provider_revision,enabled,interval_seconds,next_run_at)VALUES(?,'tv',1,1,123,100)",[kept.clone()]).await?;
    let disabled = uuid::Uuid::new_v4().to_string();
    seed(&c, &disabled, "movies").await?;
    c.execute(
        "INSERT INTO download_processing_policies VALUES(?,'movies',1,1,1,'copy')",
        [disabled.clone()],
    )
    .await?;
    c.execute("INSERT INTO download_refresh_schedules(provider_id,media_type,provider_revision,enabled,interval_seconds,next_run_at)VALUES(?,'movies',1,1,777,100)",[disabled.clone()]).await?;
    // Exercise the actual old invalidation trigger, not a guessed reason for false.
    c.execute(
        "UPDATE providers SET name='edited',revision=revision+1 WHERE id=?",
        [disabled],
    )
    .await?;
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM download_processing_policies WHERE enabled=0 AND revision=2"
        )
        .await?,
        1
    );
    c.execute_batch("INSERT INTO series(id,tvdb_id,title,path)VALUES(1,101,'TV','/tv');INSERT INTO seasons VALUES(1,1,1);INSERT INTO episodes(id,series_id,season,number,title)VALUES(1,1,1,1,'Pilot');INSERT INTO operations(id,media_type,episode_id,source,mode,destination,status,message)VALUES('kept-operation','episode',1,'/source','copy','/tv/new.mkv','preview','fixture');INSERT INTO import_journal(operation_id,plan_json,phase)VALUES('kept-operation','{ \"immutable\": true }','preview');").await?;
    let indexer = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint)VALUES(?,'torznab','synthetic indexer',1,1,1,1,'http://127.0.0.1:9')",[indexer.clone()]).await?;
    c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search)VALUES(?,'torznab','tv','[5000]','[]',0)",[indexer.clone()]).await?;
    let command = super::rss_tests::command(&c, &indexer, &kept, "tv").await?;
    let receipt =
        super::processing_tests::observed(&c, &command, &kept, "tv", &"a".repeat(40)).await?;
    let receipt_sql = "SELECT json_object('identity',submission_identity_json,'facts',comparison_facts_json,'hash',observed_hash,'client_revision',client_revision,'status',status,'payload',private_payload) FROM rss_candidates WHERE id=?";
    let receipt_before = c
        .query(receipt_sql, [receipt.clone()])
        .await?
        .next()
        .await?
        .unwrap()
        .get::<String>(0)?;
    let journal_before = c
        .query(
            "SELECT plan_json FROM import_journal WHERE operation_id='kept-operation'",
            (),
        )
        .await?
        .next()
        .await?
        .unwrap()
        .get::<String>(0)?;
    //63 missing inherited observers plus2 retained explicit schedules exceed64 by exactly1.
    for _ in 0..63 {
        seed(&c, &uuid::Uuid::new_v4().to_string(), "tv").await?;
    }
    let tx = c.transaction().await?;
    tx.execute_batch(MIGRATIONS[40].1).await?;
    assert!(tx.execute_batch(MIGRATIONS[40].1).await.is_err());
    tx.rollback().await?;
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM sqlite_schema WHERE name='completed_download_handling_settings'"
        )
        .await?,
        0
    );
    assert_eq!(version(&c).await?, 40);
    drop(c);
    drop(raw);
    let db = Database::open_local(&path).await?;
    let c = db.connect().await?;
    assert_eq!(version(&c).await?, 43); // Latest open includes communication storage; predecessor remains40.
    assert_eq!(count(&c,"SELECT count(*) FROM download_processing_policies WHERE enabled_override=enabled AND revision IN (1,2)").await?,2);
    assert_eq!(count(&c,"SELECT count(*) FROM download_refresh_schedules WHERE requested_enabled=enabled AND intent='explicit' AND interval_seconds IN (123,777)").await?,2);
    assert_eq!(
        c.query(receipt_sql, [receipt])
            .await?
            .next()
            .await?
            .unwrap()
            .get::<String>(0)?,
        receipt_before,
        "migration preserves actual observed receipt bytes"
    );
    assert_eq!(
        c.query(
            "SELECT plan_json FROM import_journal WHERE operation_id='kept-operation'",
            ()
        )
        .await?
        .next()
        .await?
        .unwrap()
        .get::<String>(0)?,
        journal_before,
        "migration preserves immutable journal text including whitespace"
    );
    crate::completed_download_handling::startup(&db)
        .await
        .unwrap();
    assert_eq!(
        count(&c, "SELECT count(*) FROM download_processing_policies").await?,
        2,
        "overflow must not commit a prefix of inherited authority"
    );
    assert_eq!(
        count(
            &c,
            "SELECT sum(reconciliation_pending) FROM completed_download_handling_settings"
        )
        .await?,
        2
    );
    assert_eq!(count(&c,"SELECT count(*) FROM download_processing_policies WHERE enabled=1 AND enabled_override=1 AND mode='hardlink'").await?,1,"valid explicit authority survives pending");
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    tx.execute("UPDATE download_refresh_schedules SET intent='suppressed',requested_enabled=NULL,enabled=0,revision=revision+1 WHERE provider_id=?",[kept.clone()]).await?;
    crate::completed_download_handling::reconcile_pending(&tx)
        .await
        .unwrap();
    tx.commit().await?;
    assert_eq!(
        count(
            &c,
            "SELECT sum(reconciliation_pending) FROM completed_download_handling_settings"
        )
        .await?,
        0
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM download_refresh_schedules WHERE intent!='suppressed'"
        )
        .await?,
        64
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM download_processing_policies WHERE enabled=1"
        )
        .await?,
        64
    );
    assert_eq!(count(&c,"SELECT count(*) FROM download_processing_policies WHERE enabled_override=0 AND enabled=0").await?,1);
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    crate::completed_download_handling::set(&tx, crate::api::MediaDomain::Tv, false, true, 1, true)
        .await
        .unwrap();
    tx.commit().await?;
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM download_processing_policies WHERE enabled=1"
        )
        .await?,
        0,
        "master off also gates preserved explicit-on"
    );
    assert!(c.execute("UPDATE download_processing_policies SET enabled=1,revision=revision+1 WHERE provider_id=?",[kept]).await.is_err());
    drop(c);
    drop(db);
    let db = Database::open_local(&path).await?;
    assert_eq!(version(&db.connect().await?).await?, 43); // Reopen retains latest43; predecessor remains40.
    Ok(())
}

#[tokio::test]
async fn cdh_schema40_archive_backfill_preserves_core_and_replays() -> Result<(), Error> {
    let _serial = crate::snapshots::IMPORT_TEST_LOCK.lock().await;
    for version in [233, 206, 242] {
        let s = Scratch::new();
        let source_path = s.0.join("source");
        let source = libsql::Builder::new_local(&source_path).build().await?;
        let c = source.connect()?;
        c.execute_batch(&format!("CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES({version});CREATE TABLE Config(Key TEXT,Value TEXT);INSERT INTO Config VALUES('enablecompleteddownloadhandling','False');")).await?;
        let core = match version {
            233 => {
                "CREATE TABLE Series(Id INTEGER,TvdbId INTEGER,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,Seasons TEXT);INSERT INTO Series VALUES(1,233,'TV',2020,'/tv/imported',1,'[]');CREATE TABLE Episodes(Id INTEGER,SeriesId INTEGER,SeasonNumber INTEGER,EpisodeNumber INTEGER,Title TEXT,Monitored INTEGER,EpisodeFileId INTEGER);CREATE TABLE EpisodeFiles(Id INTEGER,SeriesId INTEGER,RelativePath TEXT);"
            }
            206 => {
                "CREATE TABLE Movies(Id INTEGER,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER);INSERT INTO Movies VALUES(1,206,NULL,'Old',2020,'/movies/old',1,0);CREATE TABLE MovieFiles(Id INTEGER,MovieId INTEGER,RelativePath TEXT,Edition TEXT);"
            }
            _ => {
                "CREATE TABLE MovieMetadata(Id INTEGER,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER);INSERT INTO MovieMetadata VALUES(10,242,NULL,'New',2020);CREATE TABLE Movies(Id INTEGER,MovieMetadataId INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER);INSERT INTO Movies VALUES(1,10,'/movies/new',1,0);CREATE TABLE MovieFiles(Id INTEGER,MovieId INTEGER,RelativePath TEXT,Edition TEXT);"
            }
        };
        c.execute_batch(core).await?;
        drop(c);
        drop(source);
        let bytes = std::fs::read(source_path)?;
        let app = if version == 233 {
            crate::snapshots::Application::Sonarr
        } else {
            crate::snapshots::Application::Radarr
        };
        let path = s.0.join("old");
        let raw = libsql::Builder::new_local(&path).build().await?;
        let c = raw.connect()?;
        c.execute("PRAGMA foreign_keys=ON", ()).await?;
        c.execute(HISTORY_SQL, ()).await?;
        for (i, (name, sql)) in MIGRATIONS.iter().take(40).enumerate() {
            c.execute_batch(sql).await?;
            c.execute(
                "INSERT INTO schema_migrations(version,name,checksum,sql)VALUES(?,?,?,?)",
                params![i as i64 + 1, *name, checksum(sql), *sql],
            )
            .await?;
        }
        // Build a genuine predecessor archive before the CDH marker/table exist.
        // Resetting a marker in schema41 would bypass the migration/backfill contract.
        let tx = c.transaction().await?;
        crate::snapshots::write_core_snapshot_fixture(&tx, app, bytes.clone()).await?;
        tx.commit().await?;
        assert_eq!(count(&c,"SELECT count(*) FROM pragma_table_info('snapshot_imports') WHERE name='cdh_version'").await?,0);
        let archive = c
            .query(
                "SELECT record_json FROM snapshot_records WHERE source_table='Config'",
                (),
            )
            .await?
            .next()
            .await?
            .unwrap()
            .get::<String>(0)?;
        let mappings = count(&c, "SELECT count(*) FROM snapshot_mappings").await?;
        drop(c);
        drop(raw);
        // Raw predecessor creation does not apply Database's private-file permissions.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let db = Database::open_local(&path).await?;
        let c = db.connect().await?;
        assert_eq!(
            count(&c, "SELECT cdh_version FROM snapshot_imports").await?,
            0
        );
        assert_eq!(count(&c,"SELECT count(*) FROM completed_download_handling_settings WHERE enabled=1 AND defined=0").await?,2);
        let dry = crate::snapshots::import(&db, app, bytes.clone(), true).await?;
        assert!(!dry.applied);
        assert_eq!(
            count(&c, "SELECT cdh_version FROM snapshot_imports").await?,
            0
        );
        for _ in 0..2 {
            let report = crate::snapshots::import(&db, app, bytes.clone(), false).await?;
            assert!(report.applied, "{report:?}");
            assert_eq!(report.conflicts, 0);
            assert_eq!(
                count(&c, "SELECT cdh_version FROM snapshot_imports").await?,
                1
            );
            assert_eq!(count(&c,"SELECT count(*) FROM completed_download_handling_settings WHERE enabled=0 AND defined=1 AND locally_edited=0").await?,1);
            assert_eq!(
                count(&c, "SELECT count(*) FROM snapshot_mappings").await?,
                mappings
            );
            assert_eq!(
                c.query(
                    "SELECT record_json FROM snapshot_records WHERE source_table='Config'",
                    ()
                )
                .await?
                .next()
                .await?
                .unwrap()
                .get::<String>(0)?,
                archive
            );
        }
    }
    Ok(())
}
