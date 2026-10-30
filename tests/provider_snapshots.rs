use hrrdarr::{
    db::{Database, Error},
    providers::CredentialKey,
    snapshots::{self, Application, ProviderOptions, Report},
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
async fn allowing_unmapped(
    db: &Database,
    app: Application,
    bytes: Vec<u8>,
    dry_run: bool,
    key: Option<&CredentialKey>,
) -> Result<Report, snapshots::ImportError> {
    snapshots::import_with_provider_options(
        db,
        app,
        bytes,
        dry_run,
        ProviderOptions {
            key,
            allow_unmapped: true,
        },
    )
    .await
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
    assert_eq!(
        serde_json::to_value(&conflict.provider_conflicts)?,
        serde_json::json!([{"table":"Indexers","source_id":1,"reason":"mapping_changed","candidates":1,"blocking":true}])
    );
    c.execute("DELETE FROM providers WHERE id=(SELECT provider_id FROM snapshot_provider_mappings WHERE application='radarr' AND source_table='DownloadClients' LIMIT 1)",()).await?;
    let conflict =
        snapshots::import_with_providers(&db, Application::Radarr, film, false, Some(&key)).await?;
    assert!(!conflict.applied);
    assert_eq!(conflict.conflicts, 1);
    // A deleted mapped target is still blocking; zero candidates remain.
    assert_eq!(
        serde_json::to_value(&conflict.provider_conflicts)?,
        serde_json::json!([{"table":"DownloadClients","source_id":1,"reason":"mapping_changed","candidates":0,"blocking":true}])
    );
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
        // A changed export is a new fingerprint without a stored mapping: the differing indexer is a
        // `differs` row, blocking by default and reported with its source ID.
        let entry = |blocking: bool| serde_json::json!([{"table":"Indexers","source_id":1,"reason":"differs","candidates":1,"blocking":blocking}]);
        assert!(
            !changed.applied && changed.conflicts == 1,
            "Changed source policies block by default: {changed:?}"
        );
        assert_eq!(
            serde_json::to_value(&changed.provider_conflicts)?,
            entry(true)
        );
        // Reasoning: with the explicit opt-in (#20) the rest of the import applies and only that row
        // stays unmapped; the native scope is read directly to show it was not overwritten.
        let allowed =
            allowing_unmapped(&db, app, std::fs::read(&source_path)?, false, Some(&key)).await?;
        assert!(allowed.applied && allowed.conflicts == 0, "{allowed:?}");
        assert_eq!(
            serde_json::to_value(&allowed.provider_conflicts)?,
            entry(false)
        );
        assert_eq!(
            scalar(&c, "SELECT count(*) FROM provider_scopes WHERE implementation IN ('torznab','newznab') AND enable_rss=1").await,
            0,
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
        // Reasoning: 0053 adds one client path key; one indexer class mark advances five keys,
        // and one client class mark advances five, for ten generations in the imported domain.
        assert_eq!(
            scalar(&c, "SELECT sum(generation) FROM health_checks").await,
            before + 10
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
        // A changed source association is a `differs` row: blocking by default, unmapped with the opt-in,
        // and never an overwrite of the native binding.
        let raw = libsql::Builder::new_local(&path).build().await?;
        let source = raw.connect()?;
        source
            .execute("UPDATE Indexers SET DownloadClientId=0 WHERE Id=1", ())
            .await?;
        drop(source);
        drop(raw);
        let entry = |blocking: bool| serde_json::json!([{"table":"Indexers","source_id":1,"reason":"differs","candidates":1,"blocking":blocking}]);
        let changed =
            snapshots::import_with_providers(&db, app, std::fs::read(&path)?, false, Some(&key))
                .await
                .unwrap_or_else(|error| panic!("{label}: changed source association: {error:?}"));
        assert!(!changed.applied && changed.conflicts == 1, "{changed:?}");
        assert_eq!(
            serde_json::to_value(&changed.provider_conflicts)?,
            entry(true)
        );
        let allowed = allowing_unmapped(&db, app, std::fs::read(&path)?, false, Some(&key))
            .await
            .unwrap_or_else(|error| panic!("{label}: allowed changed association: {error:?}"));
        assert!(allowed.applied && allowed.conflicts == 0, "{allowed:?}");
        assert_eq!(
            serde_json::to_value(&allowed.provider_conflicts)?,
            entry(false)
        );
        let bound = format!(
            "SELECT count(*) FROM provider_scopes WHERE implementation='torznab' AND media_type='{label}' AND download_client_id='{client}'"
        );
        assert_eq!(scalar(&c, &bound).await, 1, "{label}: native binding kept");
        let indexer:String=c.query("SELECT provider_id FROM snapshot_provider_mappings WHERE application=? AND fingerprint=? AND source_table='Indexers' AND source_id=1",params![if matches!(app,Application::Sonarr){"sonarr"}else{"radarr"},repeat.fingerprint.clone()]).await?.next().await?.unwrap().get(0)?;
        // Reasoning: enabling a mapped provider is local operation, not configuration. It bumps the
        // revision but must not strand replay (#20 review); exact replay still maps without conflict.
        c.execute(
            "UPDATE providers SET enabled=1, revision=revision+1 WHERE id=?",
            [indexer.clone()],
        )
        .await?;
        let replay = snapshots::import_with_providers(&db, app, bytes.clone(), false, Some(&key))
            .await
            .unwrap_or_else(|error| panic!("{label}: replay after enabling: {error:?}"));
        assert!(
            replay.applied && replay.conflicts == 0 && replay.provider_conflicts.is_empty(),
            "{label}: {replay:?}"
        );
        // A native association edit changes reconstructed configuration, so replay reports it.
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
        assert_eq!(
            serde_json::to_value(&repeat.provider_conflicts)?,
            serde_json::json!([{"table":"Indexers","source_id":1,"reason":"mapping_changed","candidates":1,"blocking":true}])
        );
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

#[tokio::test]
async fn download_client_snapshot_tags_map_both_domains_and_never_drop_constraints()
-> Result<(), Error> {
    let _guard = IMPORT_LOCK.lock().await;
    let scratch = Sandbox::new();
    let key = CredentialKey::from_hex(&"ab".repeat(32)).unwrap();
    for (app, media) in [(Application::Sonarr, "tv"), (Application::Radarr, "movies")] {
        for mapped in [true, false] {
            let path = scratch.0.join(format!("{media}-{mapped}-source.db"));
            fixture(&path, app, false).await;
            let raw = libsql::Builder::new_local(&path).build().await?;
            let source = raw.connect()?;
            source.execute_batch("CREATE TABLE Tags(Id INTEGER,Label TEXT); INSERT INTO Tags VALUES(7,'eligible'); ALTER TABLE DownloadClients ADD COLUMN Tags TEXT DEFAULT '[]';").await?;
            source
                .execute(
                    "UPDATE DownloadClients SET Tags=? WHERE Implementation='QBittorrent'",
                    [if mapped { "[7]" } else { "[8]" }],
                )
                .await?;
            drop(source);
            drop(raw);
            let db = destination(&scratch.0.join(format!("{media}-{mapped}-destination.db"))).await;
            let c = db.connect().await?;
            c.execute(
                "INSERT INTO tags(id,media_type,label)VALUES(7,?,'unrelated')",
                [media],
            )
            .await?;
            let bytes = std::fs::read(&path)?;
            let report =
                snapshots::import_with_providers(&db, app, bytes.clone(), false, Some(&key))
                    .await
                    .unwrap_or_else(|error| {
                        panic!("{media}/{mapped}: initial tag import: {error:?}")
                    });
            assert!(
                report.applied,
                "{media}/{mapped}: initial import must commit"
            );
            let count = scalar(&c, "SELECT count(*) FROM download_client_tags").await;
            if mapped {
                assert_eq!(count, 1);
                let row=c.query("SELECT t.id,t.label,t.media_type FROM download_client_tags d JOIN tags t ON t.id=d.tag_id",()).await?.next().await?.unwrap();
                assert_ne!(row.get::<i64>(0)?, 7);
                assert_eq!(row.get::<String>(1)?, "eligible");
                assert_eq!(row.get::<String>(2)?, media);
                // libsql Row retains its statement. Release this read before replay
                // commits through a separate connection; otherwise the fixture holds a lock.
                drop(row);
                let replay = snapshots::import_with_providers(&db, app, bytes, false, Some(&key))
                    .await
                    .unwrap_or_else(|error| panic!("{media}: mapped tag replay: {error:?}"));
                assert!(replay.applied, "{media}: replay must commit");
                assert_eq!(replay.conflicts, 0);
                assert_eq!(
                    scalar(&c, "SELECT count(*) FROM download_client_tags").await,
                    1
                );
            } else {
                assert_eq!(count, 0);
                assert_eq!(
                    scalar(
                        &c,
                        "SELECT count(*) FROM providers WHERE implementation='qbittorrent'"
                    )
                    .await,
                    0
                );
                assert!(
                    report
                        .unsupported
                        .iter()
                        .any(|u| u.table == "DownloadClients"
                            && u.columns.iter().any(|c| c.contains("unmapped")))
                );
            }
        }
    }
    Ok(())
}

#[tokio::test]
async fn credential_presence_difference_is_classified_without_the_key() -> Result<(), Error> {
    let _guard = IMPORT_LOCK.lock().await;
    let scratch = Sandbox::new();
    let key = CredentialKey::from_hex(&"ab".repeat(32)).unwrap();
    for (app, label) in [(Application::Sonarr, "tv"), (Application::Radarr, "movies")] {
        let path = scratch.0.join(format!("{label}.db"));
        let credentialed = fixture(&path, app, false).await;
        let db = destination(&scratch.0.join(format!("{label}-dest.db"))).await;
        assert!(
            snapshots::import_with_providers(&db, app, credentialed, false, Some(&key))
                .await?
                .applied
        );
        // A newer export of the same client endpoint without credentials needs no key itself.
        let raw = libsql::Builder::new_local(&path).build().await?;
        let source = raw.connect()?;
        let settings = if matches!(app, Application::Sonarr) {
            r#"{"host":"client.example","port":8080,"useSsl":true,"urlBase":"/qbit"}"#
        } else {
            r#"{"host":"client.example","port":8080,"useSsl":true,"urlBase":"/qbit","movieCategory":"movies"}"#
        };
        source.execute("DELETE FROM Indexers", ()).await?;
        source
            .execute("UPDATE DownloadClients SET Settings=?", [settings])
            .await?;
        drop(source);
        drop(raw);
        // Reasoning: the destination candidate holds ciphertext and the source has none, so they
        // differ without decrypting. Before the fix the importer decrypted first and failed with the
        // key error; now the strict default reports a blocking `differs` and the opt-in applies.
        let strict =
            snapshots::import_with_providers(&db, app, std::fs::read(&path)?, false, None).await?;
        assert!(
            !strict.applied && strict.conflicts == 1,
            "{label}: {strict:?}"
        );
        let report = allowing_unmapped(&db, app, std::fs::read(&path)?, false, None).await?;
        assert!(
            report.applied && report.conflicts == 0,
            "{label}: {report:?}"
        );
        for (outcome, blocking) in [(&strict, true), (&report, false)] {
            assert_eq!(
                serde_json::to_value(&outcome.provider_conflicts)?,
                serde_json::json!([{"table":"DownloadClients","source_id":1,"reason":"differs","candidates":1,"blocking":blocking}]),
                "{label}"
            );
        }
        assert_eq!(
            scalar(&db.connect().await?, "SELECT count(*) FROM providers WHERE implementation='qbittorrent' AND credentials IS NOT NULL").await,
            1,
            "{label}: the native client is untouched"
        );
    }
    Ok(())
}

async fn edit(path: &Path, sql: &str) -> Vec<u8> {
    let raw = libsql::Builder::new_local(path).build().await.unwrap();
    raw.connect().unwrap().execute_batch(sql).await.unwrap();
    drop(raw);
    std::fs::read(path).unwrap()
}

#[tokio::test]
async fn same_endpoint_siblings_ambiguity_enabled_matches_and_dry_runs() -> Result<(), Error> {
    let _guard = IMPORT_LOCK.lock().await;
    let scratch = Sandbox::new();
    let key = CredentialKey::from_hex(&"ab".repeat(32)).unwrap();
    for (app, label) in [(Application::Sonarr, "tv"), (Application::Radarr, "movies")] {
        let path = scratch.0.join(format!("{label}.db"));
        fixture(&path, app, false).await;
        // Two source clients on one host:port ("qBit TV" and "qBit Anime"), each with a bound indexer.
        let siblings = edit(&path, "INSERT INTO DownloadClients SELECT 2,'Client Anime',Implementation,ConfigContract,Settings,Priority,Enable FROM DownloadClients WHERE Id=1; ALTER TABLE Indexers ADD COLUMN DownloadClientId INTEGER DEFAULT 0; UPDATE Indexers SET DownloadClientId=Id WHERE Id IN (1,2);").await;
        let db = destination(&scratch.0.join(format!("{label}-dest.db"))).await;
        let c = db.connect().await?;
        // Reasoning: row 2's candidate query used to see row 1's new provider and report `differs`,
        // dropping row 2 and its indexer. A sibling's destination is never a candidate.
        let report =
            snapshots::import_with_providers(&db, app, siblings.clone(), false, Some(&key)).await?;
        assert!(
            report.applied && report.provider_conflicts.is_empty(),
            "{label}: {report:?}"
        );
        assert_eq!(
            scalar(
                &c,
                "SELECT count(*) FROM providers WHERE implementation='qbittorrent'"
            )
            .await,
            2
        );
        // Each source indexer is bound to its own source client's destination.
        let bindings = "SELECT count(*) FROM provider_scopes i JOIN snapshot_provider_mappings mi ON mi.provider_id=i.provider_id AND mi.source_table='Indexers' JOIN snapshot_provider_mappings mc ON mc.provider_id=i.download_client_id AND mc.source_table='DownloadClients' AND mc.fingerprint=mi.fingerprint WHERE mc.source_id=mi.source_id";
        assert_eq!(scalar(&c, bindings).await, 2, "{label}");
        let rerun =
            snapshots::import_with_providers(&db, app, siblings.clone(), false, Some(&key)).await?;
        assert!(
            rerun.applied && rerun.conflicts == 0 && rerun.mapped == 0,
            "{label}: {rerun:?}"
        );
        // A newer export binds each sibling to its single exact match among the two candidates.
        let newer = edit(&path, "CREATE TABLE Extra(Value TEXT);").await;
        let reexport =
            snapshots::import_with_providers(&db, app, newer.clone(), false, Some(&key)).await?;
        assert!(
            reexport.applied && reexport.provider_conflicts.is_empty() && reexport.mapped == 0,
            "{label}: {reexport:?}"
        );
        assert_eq!(scalar(&c, "SELECT count(*) FROM providers").await, 4);

        // An enabled (locally operated) exact match binds unchanged and is never duplicated.
        c.execute("UPDATE providers SET enabled=1, revision=revision+1 WHERE implementation='qbittorrent'", ()).await?;
        let enabled = edit(&path, "CREATE TABLE Extra2(Value TEXT);").await;
        let report = snapshots::import_with_providers(&db, app, enabled, false, Some(&key)).await?;
        assert!(
            report.applied && report.provider_conflicts.is_empty() && report.mapped == 0,
            "{label}: {report:?}"
        );
        assert_eq!(
            scalar(&c, "SELECT count(*) FROM providers WHERE enabled=1").await,
            2,
            "{label}: bound providers were not modified"
        );

        // Two identical source clients create two identical destinations; a later single-client
        // export then exactly matches both, which is genuinely ambiguous.
        let twins = edit(&path, "UPDATE DownloadClients SET Name='Client' WHERE Id=2; UPDATE Indexers SET DownloadClientId=0; CREATE TABLE Extra3(Value TEXT);").await;
        let ambiguous_db = destination(&scratch.0.join(format!("{label}-twins.db"))).await;
        assert!(
            snapshots::import_with_providers(&ambiguous_db, app, twins, false, Some(&key))
                .await?
                .applied
        );
        let single = edit(
            &path,
            "DELETE FROM DownloadClients WHERE Id=2; CREATE TABLE Extra4(Value TEXT);",
        )
        .await;
        let before = ambiguous_db.connect().await?;
        let state = scalar(&before, "SELECT count(*) FROM snapshot_imports").await;
        let entry = |blocking: bool| serde_json::json!([{"table":"DownloadClients","source_id":1,"reason":"ambiguous","candidates":2,"blocking":blocking}]);
        // Dry runs report the same entries as the real run and write nothing, in both modes.
        for dry_run in [true, false] {
            let strict = snapshots::import_with_providers(
                &ambiguous_db,
                app,
                single.clone(),
                dry_run,
                Some(&key),
            )
            .await?;
            assert!(
                !strict.applied && strict.conflicts == 1,
                "{label}: {strict:?}"
            );
            assert_eq!(
                serde_json::to_value(&strict.provider_conflicts)?,
                entry(true)
            );
            assert_eq!(
                scalar(&before, "SELECT count(*) FROM snapshot_imports").await,
                state
            );
        }
        let preview =
            allowing_unmapped(&ambiguous_db, app, single.clone(), true, Some(&key)).await?;
        assert!(
            !preview.applied && preview.conflicts == 0,
            "{label}: {preview:?}"
        );
        assert_eq!(
            serde_json::to_value(&preview.provider_conflicts)?,
            entry(false)
        );
        assert_eq!(
            scalar(&before, "SELECT count(*) FROM snapshot_imports").await,
            state
        );
        let allowed = allowing_unmapped(&ambiguous_db, app, single, false, Some(&key)).await?;
        assert!(allowed.applied, "{label}: {allowed:?}");
        assert_eq!(
            serde_json::to_value(&allowed.provider_conflicts)?,
            entry(false)
        );
        assert_eq!(
            scalar(
                &before,
                "SELECT count(*) FROM providers WHERE implementation='qbittorrent'"
            )
            .await,
            2,
            "{label}: no guessed mapping and no third copy"
        );
    }
    Ok(())
}
