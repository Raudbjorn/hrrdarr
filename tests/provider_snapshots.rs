use hrrdarr::{
    db::{Database, Error},
    providers::CredentialKey,
    snapshots::{self, Application},
};
use libsql::{Connection, params};
use std::path::{Path, PathBuf};
// The importer has a process-wide admission gate, even for separate scratch databases.
static IMPORT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "hrrdarr-provider-snapshot-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Sandbox {
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
async fn destination(path: &Path) -> Database {
    let db = Database::open_local(path).await.unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    db
}
async fn fixture(path: &Path, app: Application, old_movie: bool) -> Vec<u8> {
    let db = libsql::Builder::new_local(path).build().await.unwrap();
    let c = db.connect().unwrap();
    let tv = matches!(app, Application::Sonarr);
    c.execute_batch(if tv {r#"CREATE TABLE VersionInfo(Version INTEGER); INSERT INTO VersionInfo VALUES(233);
CREATE TABLE Series(Id INTEGER,TvdbId INTEGER,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,Seasons TEXT);
INSERT INTO Series VALUES(1,123,'TV',2020,'/tv/TV',1,'[]');
CREATE TABLE Episodes(Id INTEGER,SeriesId INTEGER,SeasonNumber INTEGER,EpisodeNumber INTEGER,Title TEXT,Monitored INTEGER,EpisodeFileId INTEGER);
CREATE TABLE EpisodeFiles(Id INTEGER,SeriesId INTEGER,RelativePath TEXT);"#}else if old_movie {r#"CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES(206);
CREATE TABLE Movies(Id INTEGER,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER);
INSERT INTO Movies VALUES(1,456,'tt456','Film',2021,'/movies/Film',1,0);
CREATE TABLE MovieFiles(Id INTEGER,MovieId INTEGER,RelativePath TEXT,Edition TEXT);"#}else{r#"CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES(242);
CREATE TABLE MovieMetadata(Id INTEGER,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER);INSERT INTO MovieMetadata VALUES(1,456,'tt456','Film',2021);
CREATE TABLE Movies(Id INTEGER,MovieMetadataId INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER);INSERT INTO Movies VALUES(1,1,'/movies/Film',1,0);
CREATE TABLE MovieFiles(Id INTEGER,MovieId INTEGER,RelativePath TEXT,Edition TEXT);"#}).await.unwrap();
    c.execute_batch("CREATE TABLE Indexers(Id INTEGER,Name TEXT,Implementation TEXT,ConfigContract TEXT,Settings TEXT,Priority INTEGER,EnableRss INTEGER,EnableAutomaticSearch INTEGER,EnableInteractiveSearch INTEGER); CREATE TABLE DownloadClients(Id INTEGER,Name TEXT,Implementation TEXT,ConfigContract TEXT,Settings TEXT,Priority INTEGER,Enable INTEGER);").await.unwrap();
    for (id, implementation) in [(1, "Torznab"), (2, "Newznab")] {
        let settings = if tv {
            serde_json::json!({"baseUrl":format!("https://{}.example",implementation.to_lowercase()),"apiPath":"/api","apiKey":"PRIVATE_INDEXER_SECRET","categories":[5000],"animeCategories":[5070],"animeStandardFormatSearch":true,"additionalParameters":"&custom=PRIVATE_QUERY_SECRET","PRIVATE_UNKNOWN_KEY_SECRET":{"unknown":true}})
        } else {
            serde_json::json!({"baseUrl":format!("https://{}.example",implementation.to_lowercase()),"apiPath":"/api","apiKey":"PRIVATE_INDEXER_SECRET","categories":[2000],"removeYear":true,"additionalParameters":"&custom=PRIVATE_QUERY_SECRET"})
        };
        c.execute(
            "INSERT INTO Indexers VALUES(?,?,?,?,?,1,0,1,0)",
            params![
                id,
                implementation,
                implementation,
                format!("{implementation}Settings"),
                settings.to_string()
            ],
        )
        .await
        .unwrap();
    }
    let settings = if tv {
        serde_json::json!({"host":"client.example","port":8080,"useSsl":true,"urlBase":"/qbit","username":"PRIVATE_USERNAME","password":"PRIVATE_PASSWORD","tvCategory":"tv","tvImportedCategory":"tv-done","recentTvPriority":1,"olderTvPriority":0,"initialState":1,"contentLayout":2,"sequentialOrder":true,"firstAndLast":true,"addSeriesTags":true})
    } else {
        serde_json::json!({"host":"client.example","port":8080,"useSsl":true,"urlBase":"/qbit","apiKey":"PRIVATE_BEARER","movieCategory":"movies","movieImportedCategory":"movies-done","recentMoviePriority":0,"olderMoviePriority":1,"initialState":2,"contentLayout":1})
    };
    c.execute(
        "INSERT INTO DownloadClients VALUES(1,'Client','QBittorrent','QBittorrentSettings',?,1,1)",
        [settings.to_string()],
    )
    .await
    .unwrap();
    c.execute("INSERT INTO Indexers VALUES(3,'Unknown','FutureIndexer','FutureSettings','{\"unknown\":1}',1,1,1,1)",()).await.unwrap();
    drop(c);
    drop(db);
    std::fs::read(path).unwrap()
}
#[tokio::test]
async fn provider_reconstruction_is_opt_in_atomic_scoped_private_and_replay_safe()
-> Result<(), Error> {
    let _guard = IMPORT_LOCK.lock().await;
    let s = Sandbox::new();
    let key = CredentialKey::from_hex(&"ab".repeat(32)).unwrap();
    let wrong = CredentialKey::from_hex(&"cd".repeat(32)).unwrap();
    let tv_path = s.0.join("tv.db");
    let tv = fixture(&tv_path, Application::Sonarr, false).await;
    let film = fixture(&s.0.join("film.db"), Application::Radarr, false).await;
    let db = destination(&s.0.join("destination.db")).await;
    let c = db.connect().await?;
    assert!(
        snapshots::import(&db, Application::Sonarr, tv.clone(), false)
            .await?
            .applied
    );
    assert_eq!(scalar(&c, "SELECT count(*) FROM providers").await, 0);
    assert!(
        snapshots::import_with_providers(&db, Application::Sonarr, tv.clone(), false, None)
            .await
            .is_err()
    );
    assert_eq!(scalar(&c, "SELECT count(*) FROM providers").await, 0);
    let preview =
        snapshots::import_with_providers(&db, Application::Sonarr, tv.clone(), true, Some(&key))
            .await?;
    assert!(!preview.applied);
    assert_eq!(preview.mapped, 3);
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM snapshot_provider_mappings").await,
        0
    );
    let report =
        snapshots::import_with_providers(&db, Application::Sonarr, tv.clone(), false, Some(&key))
            .await?;
    assert!(report.applied);
    assert_eq!(
        serde_json::to_value(&preview.unsupported)?,
        serde_json::to_value(&report.unsupported)?
    );
    assert_eq!(report.mapped, 3);
    assert_eq!(scalar(&c, "SELECT count(*) FROM provider_scopes WHERE implementation IN ('torznab','newznab') AND enable_rss=0 AND enable_automatic_search=1 AND enable_interactive_search=0").await, 2);
    // The source fixture predates a DownloadClientId column, so the missing association is reported, never guessed.
    assert!(report.unsupported.iter().any(|u| {
        u.columns
            .iter()
            .any(|c| c == "DownloadClientId (absent; source association unknown)")
    }));
    let public = serde_json::to_string(&report)?;
    assert!(!public.contains("PRIVATE_"));
    assert!(!public.contains("client.example"));
    assert!(
        report
            .unsupported
            .iter()
            .any(|u| u.columns.contains(&"Settings (unsupported fields)".into()))
    );
    assert!(
        report
            .unsupported
            .iter()
            .any(|u| u.table == "Indexers" && u.columns.contains(&"Implementation".into()))
    );
    assert_eq!(scalar(&c,"SELECT count(*) FROM providers WHERE enabled=0 AND revision=1 AND typeof(credentials)='blob'").await,3);
    assert_eq!(scalar(&c, "SELECT count(*) FROM provider_tests").await, 0);
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM provider_scopes WHERE media_type='tv'"
        )
        .await,
        3
    );
    assert_eq!(scalar(&c,"SELECT count(*) FROM provider_scopes WHERE initial_state='forced' AND content_layout='subfolder' AND add_tags=1").await,1);
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM snapshot_provider_mappings WHERE source_id=1"
        )
        .await,
        2
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM snapshot_records WHERE instr(record_json,'PRIVATE_PASSWORD')>0"
        )
        .await,
        1
    );
    let mut rows = c.query("SELECT credentials FROM providers", ()).await?;
    while let Some(row) = rows.next().await? {
        let v: Vec<u8> = row.get(0)?;
        assert!(!String::from_utf8_lossy(&v).contains("PRIVATE_"));
    }
    let replay =
        snapshots::import_with_providers(&db, Application::Sonarr, tv.clone(), false, Some(&key))
            .await?;
    // Zero insertions alone also permits a rolled-back conflict; require successful reconciliation.
    assert!(replay.applied && replay.conflicts == 0 && replay.duplicates >= 3);
    assert_eq!(replay.mapped, 0);
    assert!(
        snapshots::import_with_providers(&db, Application::Sonarr, tv.clone(), false, Some(&wrong))
            .await
            .is_err()
    );
    // New export fingerprint, same settings: exact plaintext equality deduplicates randomized ciphertext.
    let raw = libsql::Builder::new_local(&tv_path).build().await?;
    let rc = raw.connect()?;
    rc.execute_batch("CREATE TABLE Extra(Value TEXT);INSERT INTO Extra VALUES('new backup');")
        .await?;
    drop(rc);
    drop(raw);
    let newer = std::fs::read(&tv_path)?;
    let unchanged_newer = newer.clone();
    let replay =
        snapshots::import_with_providers(&db, Application::Sonarr, newer, false, Some(&key))
            .await?;
    // A different fingerprint must commit mappings to the same providers, not merely avoid insertion.
    assert!(replay.applied && replay.conflicts == 0 && replay.duplicates >= 3);
    assert_eq!(replay.mapped, 0);
    assert_eq!(scalar(&c, "SELECT count(*) FROM providers").await, 3);
    assert!(
        snapshots::import_with_providers(&db, Application::Radarr, film.clone(), false, Some(&key))
            .await?
            .applied
    );
    assert_eq!(scalar(&c, "SELECT count(*) FROM providers").await, 6);
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM provider_scopes WHERE media_type='movies'"
        )
        .await,
        3
    );
    assert_eq!(scalar(&c,"SELECT count(*) FROM provider_scopes WHERE initial_state='stopped' AND content_layout='original' AND add_tags=0").await,1);
    let legacy = fixture(&s.0.join("legacy.db"), Application::Radarr, true).await;
    assert!(
        snapshots::import_with_providers(&db, Application::Radarr, legacy, false, Some(&key))
            .await?
            .applied
    );
    assert_eq!(scalar(&c, "SELECT count(*) FROM providers").await, 6);
    assert_eq!(scalar(&c, "SELECT count(*) FROM provider_scopes WHERE implementation IN ('torznab','newznab') AND enable_rss=0 AND enable_automatic_search=1 AND enable_interactive_search=0").await, 4);
    // A native policy edit is revisioned and must conflict rather than overwrite source intent.
    c.execute("UPDATE providers SET revision=revision+1 WHERE id=(SELECT provider_id FROM snapshot_provider_mappings WHERE application='sonarr' AND source_table='Indexers' AND source_id=1 LIMIT 1)",()).await?;
    c.execute("UPDATE provider_scopes SET enable_rss=1 WHERE provider_id=(SELECT provider_id FROM snapshot_provider_mappings WHERE application='sonarr' AND source_table='Indexers' AND source_id=1 LIMIT 1)", ()).await?;
    let conflict =
        snapshots::import_with_providers(&db, Application::Sonarr, tv.clone(), false, Some(&key))
            .await?;
    assert!(!conflict.applied);
    assert_eq!(conflict.conflicts, 1);
    c.execute("DELETE FROM providers WHERE id=(SELECT provider_id FROM snapshot_provider_mappings WHERE application='radarr' AND source_table='DownloadClients' LIMIT 1)",()).await?;
    let conflict =
        snapshots::import_with_providers(&db, Application::Radarr, film, false, Some(&key)).await?;
    assert!(!conflict.applied);
    assert_eq!(conflict.conflicts, 1);
    assert_eq!(scalar(&c, "SELECT count(*) FROM providers").await, 5);
    // Credential-free configurations require no key; source constructor defaults are explicit.
    let trusted_path = s.0.join("trusted.db");
    fixture(&trusted_path, Application::Sonarr, false).await;
    let raw = libsql::Builder::new_local(&trusted_path).build().await?;
    let rc = raw.connect()?;
    rc.execute_batch("DELETE FROM Indexers;UPDATE DownloadClients SET Settings='{}';")
        .await?;
    drop(rc);
    drop(raw);
    let trusted = destination(&s.0.join("trusted-destination.db")).await;
    assert!(
        snapshots::import_with_providers(
            &trusted,
            Application::Sonarr,
            std::fs::read(&trusted_path)?,
            false,
            None
        )
        .await?
        .applied
    );
    assert_eq!(
        scalar(
            &trusted.connect().await?,
            "SELECT count(*) FROM providers WHERE credentials IS NULL AND enabled=0"
        )
        .await,
        1
    );
    // Late destination failure after core/provider writes rolls everything back.
    let failure = destination(&s.0.join("failure.db")).await;
    let fc = failure.connect().await?;
    fc.execute_batch("CREATE TRIGGER fail_provider_snapshot BEFORE INSERT ON snapshot_provider_mappings WHEN NEW.source_table='DownloadClients' BEGIN SELECT RAISE(ABORT,'private failure');END;").await?;
    assert!(
        snapshots::import_with_providers(
            &failure,
            Application::Sonarr,
            tv.clone(),
            false,
            Some(&key)
        )
        .await
        .is_err()
    );
    for table in [
        "series",
        "providers",
        "provider_scopes",
        "snapshot_imports",
        "snapshot_records",
        "snapshot_provider_mappings",
    ] {
        assert_eq!(
            scalar(&fc, &format!("SELECT count(*) FROM {table}")).await,
            0
        );
    }
    // Source bytes are never changed by the importer (the newer export was our explicit fixture edit).
    assert_eq!(std::fs::read(&tv_path)?, unchanged_newer);
    drop(c);
    drop(db);
    let db = destination(&s.0.join("destination.db")).await;
    let c = db.connect().await?;
    assert_eq!(scalar(&c, "SELECT count(*) FROM providers").await, 5);
    assert!(scalar(&c, "SELECT count(*) FROM snapshot_provider_mappings").await > 5);
    Ok(())
}

#[tokio::test]
async fn absent_and_malformed_snapshot_policy_is_conservative_and_atomic() -> Result<(), Error> {
    let _guard = IMPORT_LOCK.lock().await;
    let scratch = Sandbox::new();
    let key = CredentialKey::from_hex(&"ab".repeat(32)).unwrap();
    for (app, label) in [(Application::Sonarr, "tv"), (Application::Radarr, "movies")] {
        let source_path = scratch.0.join(format!("{label}-source.db"));
        fixture(&source_path, app, false).await;
        let raw = libsql::Builder::new_local(&source_path).build().await?;
        let c = raw.connect()?;
        c.execute_batch("ALTER TABLE Indexers DROP COLUMN EnableAutomaticSearch;")
            .await?;
        drop(c);
        drop(raw);
        let bytes = std::fs::read(&source_path)?;
        let db = destination(&scratch.0.join(format!("{label}-dest.db"))).await;
        let preview =
            snapshots::import_with_providers(&db, app, bytes.clone(), true, Some(&key)).await?;
        let report =
            snapshots::import_with_providers(&db, app, bytes.clone(), false, Some(&key)).await?;
        assert!(report.applied);
        assert_eq!(
            serde_json::to_value(&preview.unsupported)?,
            serde_json::to_value(&report.unsupported)?
        );
        assert!(report.unsupported.iter().any(|u| {
            u.columns
                .iter()
                .any(|c| c == "EnableAutomaticSearch (absent; imported disabled)")
        }));
        let c = db.connect().await?;
        assert_eq!(scalar(&c, "SELECT count(*) FROM provider_scopes WHERE implementation IN ('torznab','newznab') AND enable_automatic_search=0 AND enable_rss=0 AND enable_interactive_search=0").await, 2);
        let repeat = snapshots::import_with_providers(&db, app, bytes, false, Some(&key)).await?;
        assert!(repeat.applied && repeat.conflicts == 0 && repeat.duplicates >= 3);
        let before = scalar(&c, "SELECT sum(generation) FROM health_checks").await;
        let raw = libsql::Builder::new_local(&source_path).build().await?;
        let source = raw.connect()?;
        source
            .execute("UPDATE Indexers SET EnableRss=1 WHERE Id=1", ())
            .await?;
        drop(source);
        drop(raw);
        let changed = snapshots::import_with_providers(
            &db,
            app,
            std::fs::read(&source_path)?,
            false,
            Some(&key),
        )
        .await?;
        assert!(
            !changed.applied && changed.conflicts > 0,
            "Changed source policies must not overwrite a native provider"
        );
        assert_eq!(
            scalar(&c, "SELECT sum(generation) FROM health_checks").await,
            before
        );
        let raw = libsql::Builder::new_local(&source_path).build().await?;
        let source = raw.connect()?;
        source
            .execute("UPDATE Indexers SET EnableRss='unknown' WHERE Id=2", ())
            .await?;
        drop(source);
        drop(raw);
        assert!(
            snapshots::import_with_providers(
                &db,
                app,
                std::fs::read(&source_path)?,
                false,
                Some(&key)
            )
            .await
            .is_err()
        );
        assert_eq!(scalar(&c, "SELECT count(*) FROM providers").await, 3);
        assert_eq!(
            scalar(&c, "SELECT sum(generation) FROM health_checks").await,
            before
        );
    }
    Ok(())
}

#[tokio::test]
async fn indexer_client_snapshots_remap_both_domains_and_preserve_unknown_intent()
-> Result<(), Error> {
    let _guard = IMPORT_LOCK.lock().await;
    let scratch = Sandbox::new();
    let key = CredentialKey::from_hex(&"ab".repeat(32)).unwrap();
    let db = destination(&scratch.0.join("bindings.db")).await;
    let c = db.connect().await?;
    let mut native_clients = Vec::new();
    for (app, label) in [(Application::Sonarr, "tv"), (Application::Radarr, "movies")] {
        // Report.mapped includes core entities: series+library_settings, or movie_metadata+movies+library_settings.
        let core_mapped = if matches!(app, Application::Sonarr) {
            2
        } else {
            3
        };
        let path = scratch.0.join(format!("{label}.db"));
        fixture(&path, app, false).await;
        let raw = libsql::Builder::new_local(&path).build().await?;
        let source = raw.connect()?;
        source.execute_batch("ALTER TABLE Indexers ADD COLUMN DownloadClientId INTEGER DEFAULT 0; UPDATE Indexers SET DownloadClientId=1 WHERE Id IN (1,2);").await?;
        drop(source);
        drop(raw);
        let bytes = std::fs::read(&path)?;
        let before = scalar(&c, "SELECT sum(generation) FROM health_checks").await;
        let preview = snapshots::import_with_providers(&db, app, bytes.clone(), true, Some(&key))
            .await
            .unwrap_or_else(|error| panic!("{label}: binding dry-run: {error:?}"));
        assert!(!preview.applied);
        // This destination has no prior core import: preview includes core rows and all three providers.
        assert_eq!(preview.mapped, core_mapped + 3);
        assert_eq!(
            scalar(&c, "SELECT sum(generation) FROM health_checks").await,
            before
        );
        let report = snapshots::import_with_providers(&db, app, bytes.clone(), false, Some(&key))
            .await
            .unwrap_or_else(|error| panic!("{label}: binding commit: {error:?}"));
        assert!(report.applied);
        // Dry-run rolled back its core rows too, so commit has the same aggregate mapping count.
        assert_eq!(report.mapped, core_mapped + 3);
        assert_eq!(
            serde_json::to_value(&preview.unsupported)?,
            serde_json::to_value(&report.unsupported)?
        );
        // One marker for each affected class key, not once per imported indexer.
        // Reasoning: per domain the commit marks the indexer class once (search, RSS and the 0049 binding
        // check = 3 keys) and the download-client class once (CDH, communication and root folder = 3 keys),
        // so 6 generations in total; the pre-0048/0049 expectation of 3 counted only CDH, communication and binding.
        assert_eq!(
            scalar(&c, "SELECT sum(generation) FROM health_checks").await,
            before + 6
        );
        let row=c.query("SELECT provider_id FROM snapshot_provider_mappings WHERE application=? AND fingerprint=? AND source_table='DownloadClients' AND source_id=1",params![if matches!(app,Application::Sonarr){"sonarr"}else{"radarr"},report.fingerprint.clone()]).await?.next().await?.unwrap();
        let client: String = row.get(0)?;
        // libsql Row owns its Statement; release this read lock before imports write through another connection.
        drop(row);
        native_clients.push(client.clone());
        let count: i64=c.query("SELECT count(*) FROM provider_scopes s JOIN providers p ON p.id=s.provider_id WHERE s.media_type=? AND s.download_client_id=? AND p.enabled=0",params![label,client.clone()]).await?.next().await?.unwrap().get(0)?;
        assert_eq!(count, 2);
        let before = scalar(&c, "SELECT sum(generation) FROM health_checks").await;
        let repeat = snapshots::import_with_providers(&db, app, bytes.clone(), false, Some(&key))
            .await
            .unwrap_or_else(|error| panic!("{label}: exact replay: {error:?}"));
        assert!(repeat.applied && repeat.conflicts == 0 && repeat.duplicates >= 3);
        assert_eq!(
            scalar(&c, "SELECT sum(generation) FROM health_checks").await,
            before
        );
        // A changed source association must conflict instead of overwriting the native binding.
        let raw = libsql::Builder::new_local(&path).build().await?;
        let source = raw.connect()?;
        source
            .execute("UPDATE Indexers SET DownloadClientId=0 WHERE Id=1", ())
            .await?;
        drop(source);
        drop(raw);
        let changed =
            snapshots::import_with_providers(&db, app, std::fs::read(&path)?, false, Some(&key))
                .await
                .unwrap_or_else(|error| panic!("{label}: changed source association: {error:?}"));
        assert!(!changed.applied && changed.conflicts == 1);
        assert_eq!(
            scalar(&c, "SELECT sum(generation) FROM health_checks").await,
            before
        );
        // A revisioned native association edit also conflicts with exact source replay.
        let indexer:String=c.query("SELECT provider_id FROM snapshot_provider_mappings WHERE application=? AND fingerprint=? AND source_table='Indexers' AND source_id=1",params![if matches!(app,Application::Sonarr){"sonarr"}else{"radarr"},repeat.fingerprint.clone()]).await?.next().await?.unwrap().get(0)?;
        let tx = c.transaction().await?;
        tx.execute(
            "UPDATE providers SET revision=revision+1 WHERE id=?",
            [indexer.clone()],
        )
        .await?;
        tx.execute(
            "UPDATE provider_scopes SET download_client_id=NULL WHERE provider_id=?",
            [indexer],
        )
        .await?;
        tx.commit().await?;
        let repeat = snapshots::import_with_providers(&db, app, bytes, false, Some(&key))
            .await
            .unwrap_or_else(|error| panic!("{label}: changed native association: {error:?}"));
        assert!(!repeat.applied && repeat.conflicts == 1);
        // Unknown positive intent is retained as unsupported, never reconstructed with NULL.
        let raw = libsql::Builder::new_local(&path).build().await?;
        let source = raw.connect()?;
        source
            .execute(
                "UPDATE Indexers SET DownloadClientId=999 WHERE Id IN (1,2)",
                (),
            )
            .await?;
        drop(source);
        drop(raw);
        let isolated = destination(&scratch.0.join(format!("{label}-unmapped.db"))).await;
        let report = snapshots::import_with_providers(
            &isolated,
            app,
            std::fs::read(&path)?,
            false,
            Some(&key),
        )
        .await
        .unwrap_or_else(|error| panic!("{label}: unmapped client: {error:?}"));
        assert!(report.applied);
        // Both bound indexers are omitted; fresh core entities and the one client still map.
        assert_eq!(report.mapped, core_mapped + 1);
        assert_eq!(
            report
                .unsupported
                .iter()
                .filter(|u| u
                    .columns
                    .iter()
                    .any(|c| c == "DownloadClientId (unmapped; indexer not reconstructed)"))
                .count(),
            2
        );
        let isolated_c = isolated.connect().await?;
        assert_eq!(
            scalar(
                &isolated_c,
                "SELECT count(*) FROM providers WHERE implementation!='qbittorrent'"
            )
            .await,
            0
        );
        assert_eq!(
            scalar(
                &isolated_c,
                "SELECT count(*) FROM snapshot_records WHERE source_table='Indexers'"
            )
            .await,
            3
        );
        // Missing/unsupported source client has the same no-executable-fallback rule.
        let raw = libsql::Builder::new_local(&path).build().await?;
        let source = raw.connect()?;
        source.execute_batch("UPDATE Indexers SET DownloadClientId=1 WHERE Id IN (1,2); UPDATE DownloadClients SET Implementation='Unknown';").await?;
        drop(source);
        drop(raw);
        let unsupported = destination(&scratch.0.join(format!("{label}-unsupported.db"))).await;
        let report = snapshots::import_with_providers(
            &unsupported,
            app,
            std::fs::read(&path)?,
            false,
            Some(&key),
        )
        .await
        .unwrap_or_else(|error| panic!("{label}: unsupported client: {error:?}"));
        assert!(report.applied);
        // No executable providers map here; Report.mapped still includes this fresh destination's core entities.
        assert_eq!(report.mapped, core_mapped);
        assert_eq!(
            scalar(
                &unsupported.connect().await?,
                "SELECT count(*) FROM providers"
            )
            .await,
            0
        );
        let raw = libsql::Builder::new_local(&path).build().await?;
        let source = raw.connect()?;
        source.execute("DELETE FROM DownloadClients", ()).await?;
        drop(source);
        drop(raw);
        let missing = destination(&scratch.0.join(format!("{label}-missing.db"))).await;
        let report = snapshots::import_with_providers(
            &missing,
            app,
            std::fs::read(&path)?,
            false,
            Some(&key),
        )
        .await
        .unwrap_or_else(|error| panic!("{label}: missing client: {error:?}"));
        assert!(report.applied);
        // No executable providers map here; Report.mapped still includes this fresh destination's core entities.
        assert_eq!(report.mapped, core_mapped);
        assert_eq!(
            report
                .unsupported
                .iter()
                .filter(|u| u
                    .columns
                    .iter()
                    .any(|c| c == "DownloadClientId (unmapped; indexer not reconstructed)"))
                .count(),
            2
        );
        // Malformed source values fail before retaining any partial provider or marker writes.
        for malformed in ["-1", "'unknown'", "1.5", "NULL"] {
            let before = scalar(&isolated_c, "SELECT sum(generation) FROM health_checks").await;
            let raw = libsql::Builder::new_local(&path).build().await?;
            let source = raw.connect()?;
            source
                .execute(
                    &format!("UPDATE Indexers SET DownloadClientId={malformed} WHERE Id=1"),
                    (),
                )
                .await?;
            drop(source);
            drop(raw);
            assert!(
                snapshots::import_with_providers(
                    &isolated,
                    app,
                    std::fs::read(&path)?,
                    false,
                    Some(&key)
                )
                .await
                .is_err(),
                "{label}: malformed DownloadClientId={malformed} must reject"
            );
            assert_eq!(
                scalar(&isolated_c, "SELECT count(*) FROM providers").await,
                1
            );
            assert_eq!(
                scalar(&isolated_c, "SELECT sum(generation) FROM health_checks").await,
                before
            );
        }
    }
    assert_ne!(
        native_clients[0], native_clients[1],
        "Same source client ID is namespaced by application"
    );
    Ok(())
}
