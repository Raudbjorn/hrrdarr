use hrrdarr::{
    db::Database,
    snapshots::{self, Application},
};
use serde_json::{Value, json};
use std::sync::Arc;
static SNAPSHOTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
struct Scratch(std::path::PathBuf);
impl Scratch {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("hrrdarr-cdh-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
struct Server(tokio::task::JoinHandle<()>);
impl Server {
    async fn stop(mut self) {
        self.0.abort();
        let _ = (&mut self.0).await;
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn request(base: &str, method: &str, path: &str, v: Value) -> (u16, Value) {
    let r = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap()
        .request(method.parse().unwrap(), format!("{base}{path}"))
        .header("content-type", "application/json")
        .body(v.to_string())
        .send()
        .await
        .unwrap();
    let status = r.status().as_u16();
    let text = r.text().await.unwrap();
    (
        status,
        if status == 204 {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap()
        },
    )
}
async fn server(db: Arc<Database>) -> (String, Server) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let app = hrrdarr::completed_download_handling::router(db.clone())
        .merge(hrrdarr::commands::router(db.clone()))
        .merge(hrrdarr::providers::router(db, None));
    (
        base,
        Server(tokio::spawn(
            async move { axum::serve(l, app).await.unwrap() },
        )),
    )
}
async fn fixture(s: &Scratch, version: i64, suffix: &str, extra: &str) -> Vec<u8> {
    let p = s.0.join(format!("source-{version}-{suffix}"));
    let db = libsql::Builder::new_local(&p).build().await.unwrap();
    let c = db.connect().unwrap();
    c.execute_batch(&format!("CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES({version});CREATE TABLE Tags(Id INTEGER,Label TEXT);INSERT INTO Tags VALUES(7,'Imported'),(8,'unused');")).await.unwrap();
    let sql = match version {
        233 => {
            "CREATE TABLE Series(Id INTEGER,TvdbId INTEGER,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,Seasons TEXT,Tags TEXT);INSERT INTO Series VALUES(1,233,'TV',2020,'/tv/imported',1,'[]','[7]');CREATE TABLE Episodes(Id INTEGER,SeriesId INTEGER,SeasonNumber INTEGER,EpisodeNumber INTEGER,Title TEXT,Monitored INTEGER,EpisodeFileId INTEGER);CREATE TABLE EpisodeFiles(Id INTEGER,SeriesId INTEGER,RelativePath TEXT);"
        }
        206 => {
            "CREATE TABLE Movies(Id INTEGER,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER,Tags TEXT);INSERT INTO Movies VALUES(1,206,NULL,'Old',2020,'/movies/old',1,0,'[7]');CREATE TABLE MovieFiles(Id INTEGER,MovieId INTEGER,RelativePath TEXT,Edition TEXT);"
        }
        _ => {
            "CREATE TABLE MovieMetadata(Id INTEGER,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER);INSERT INTO MovieMetadata VALUES(10,242,NULL,'New',2020);CREATE TABLE Movies(Id INTEGER,MovieMetadataId INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER,Tags TEXT);INSERT INTO Movies VALUES(1,10,'/movies/new',1,0,'[7]');CREATE TABLE MovieFiles(Id INTEGER,MovieId INTEGER,RelativePath TEXT,Edition TEXT);"
        }
    };
    c.execute_batch(sql).await.unwrap();
    c.execute_batch(extra).await.unwrap();
    drop(c);
    drop(db);
    std::fs::read(p).unwrap()
}
async fn scalar(c: &libsql::Connection, sql: &str) -> i64 {
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
fn client_input() -> Value {
    json!({"name":"CDH fixture","enabled":true,"priority":1,"settings":{"implementation":"qbittorrent","endpoint":"http://127.0.0.1:9","tv":{"category":"tv","imported_category":null,"recent_priority":0,"older_priority":0},"movies":{"category":"movies","imported_category":null,"recent_priority":0,"older_priority":0}}})
}
async fn settings(base: &str, media: &str) -> Value {
    let (code, value) = request(
        base,
        "GET",
        &format!("/api/v1/{media}/completed-download-handling"),
        Value::Null,
    )
    .await;
    assert_eq!(code, 200, "{value}");
    value
}
async fn master(base: &str, media: &str, enabled: bool) -> Value {
    let current = settings(base, media).await;
    let (code, value) = request(
        base,
        "PUT",
        &format!("/api/v1/{media}/completed-download-handling"),
        json!({"revision":current["revision"],"enabled":enabled}),
    )
    .await;
    assert_eq!(code, 200, "{value}");
    value
}
async fn policy(base: &str, id: &Value, media: &str) -> Value {
    let (code, v) = request(
        base,
        "GET",
        &format!(
            "/api/v1/download-processing/policies/{}/{media}",
            id.as_str().unwrap()
        ),
        Value::Null,
    )
    .await;
    assert_eq!(code, 200, "{v}");
    v
}
#[tokio::test]
async fn both_domain_default_authority_master_override_suppression_and_cas() {
    let scratch = Scratch::new();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let (base, server) = server(db.clone()).await;
    for media in ["tv", "movies"] {
        assert_eq!(
            settings(&base, media).await["reconciliation_reason"],
            "pending",
            "new settings have not yet been reconciled by worker startup"
        );
    }
    let (code, provider) = request(&base, "POST", "/api/v1/providers", client_input()).await;
    assert_eq!(code, 201, "{provider}");
    let id = &provider["id"];
    for media in ["tv", "movies"] {
        let p = policy(&base, id, media).await;
        assert_eq!(p["enabled"], true);
        assert_eq!(p["desired_enabled"], true);
        assert!(p["enabled_override"].is_null());
        let (_, schedules) = request(
            &base,
            "GET",
            "/api/v1/download-refresh/schedules",
            Value::Null,
        )
        .await;
        let schedule = schedules
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["target"]["media_type"] == media)
            .unwrap();
        assert_eq!(schedule["intent"], "inherited");
        assert_eq!(schedule["enabled"], true);
        assert_eq!(schedule["interval_seconds"], 60);
        let path = format!(
            "/api/v1/download-processing/policies/{}/{media}",
            id.as_str().unwrap()
        );
        let (code, v) = request(
            &base,
            "PUT",
            &path,
            json!({"revision":null,"provider_revision":1,"enabled":false,"mode":"copy"}),
        )
        .await;
        assert_eq!(code, 409, "{v}");
        let (code,v)=request(&base,"PUT",&path,json!({"revision":p["revision"],"provider_revision":1,"enabled":true,"mode":"hardlink"})).await;
        assert_eq!(code, 200, "{v}");
        master(&base, media, false).await;
        let p = policy(&base, id, media).await;
        assert_eq!(p["enabled_override"], true);
        assert_eq!(p["enabled"], false);
        assert_eq!(p["desired_enabled"], false);
        master(&base, media, true).await;
        assert_eq!(policy(&base, id, media).await["enabled"], true);
        let p = policy(&base, id, media).await;
        let (code,v)=request(&base,"PUT",&path,json!({"revision":p["revision"],"provider_revision":1,"enabled":false,"mode":"hardlink"})).await;
        assert_eq!(code, 200, "{v}");
        master(&base, media, false).await;
        master(&base, media, true).await;
        let p = policy(&base, id, media).await;
        assert_eq!(p["enabled"], false);
        assert_eq!(p["enabled_override"], false);
        assert_eq!(p["mode"], "hardlink");
        let (code, v) = request(
            &base,
            "PUT",
            &format!("{path}/inherit"),
            json!({"revision":p["revision"],"provider_revision":1,"mode":"hardlink"}),
        )
        .await;
        assert_eq!(code, 200, "{v}");
        assert_eq!(v["enabled"], true);
        let (_, schedules) = request(
            &base,
            "GET",
            "/api/v1/download-refresh/schedules",
            Value::Null,
        )
        .await;
        let old = schedules
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["target"]["media_type"] == media)
            .unwrap();
        let (code, v) = request(
            &base,
            "DELETE",
            "/api/v1/download-refresh/schedules",
            json!({"target":old["target"],"revision":old["revision"]}),
        )
        .await;
        assert_eq!(code, 204, "{v}");
        master(&base, media, false).await;
        master(&base, media, true).await;
        assert_eq!(
            settings(&base, media).await["observation_disabled_scopes"],
            1
        );
        let (_, schedules) = request(
            &base,
            "GET",
            "/api/v1/download-refresh/schedules",
            Value::Null,
        )
        .await;
        assert!(
            !schedules
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["target"]["media_type"] == media)
        );
        let (code, v) = request(
            &base,
            "PUT",
            "/api/v1/download-refresh/schedules/inherit",
            json!({"target":old["target"],"revision":null,"provider_revision":1}),
        )
        .await;
        assert_eq!(code, 200, "{v}");
        assert!(v["revision"].as_i64().unwrap() > old["revision"].as_i64().unwrap());
        assert_eq!(v["enabled"], true);
    }
    // A provider revision change reauthorizes only current policy/schedule rows, never old receipts.
    let (code,changed)=request(&base,"PUT",&format!("/api/v1/providers/{}",id.as_str().unwrap()),json!({"revision":1,"name":"edited","enabled":true,"priority":1,"settings":provider["settings"]})).await;
    assert_eq!(code, 200, "{changed}");
    for media in ["tv", "movies"] {
        assert_eq!(policy(&base, id, media).await["provider_revision"], 2);
    }
    server.stop().await;
    drop(db);
    let db = Database::open_local(scratch.0.join("db")).await.unwrap();
    let c = db.connect().await.unwrap();
    assert_eq!(scalar(&c,"SELECT count(*) FROM download_processing_policies WHERE enabled=1 AND provider_revision=2").await,2);
}
#[tokio::test]
async fn source_boolean_definedness_replay_local_intent_and_rollback() {
    let _serial = SNAPSHOTS.lock().await;
    for version in [233, 206, 242] {
        for (index, source, enabled, defined) in [
            (0, "", true, false),
            (
                1,
                "INSERT INTO Config VALUES('enablecompleteddownloadhandling','False');",
                false,
                true,
            ),
            (
                2,
                "INSERT INTO Config VALUES('enablecompleteddownloadhandling',NULL);",
                true,
                true,
            ),
            (
                3,
                "INSERT INTO Config VALUES('enablecompleteddownloadhandling','');",
                true,
                true,
            ),
            (
                4,
                "INSERT INTO Config VALUES('enablecompleteddownloadhandling',' tRuE ');",
                true,
                true,
            ),
        ] {
            let scratch = Scratch::new();
            let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
            let bytes = fixture(
                &scratch,
                version,
                &index.to_string(),
                &format!("CREATE TABLE Config(Key TEXT,Value TEXT);{source}"),
            )
            .await;
            let app = if version == 233 {
                Application::Sonarr
            } else {
                Application::Radarr
            };
            let media = if version == 233 { "tv" } else { "movies" };
            assert!(
                !snapshots::import(&db, app, bytes.clone(), true)
                    .await
                    .unwrap()
                    .applied
            );
            let c = db.connect().await.unwrap();
            assert_eq!(scalar(&c, "SELECT count(*) FROM snapshot_imports").await, 0);
            assert!(
                snapshots::import(&db, app, bytes.clone(), false)
                    .await
                    .unwrap()
                    .applied
            );
            let (base, server) = server(db.clone()).await;
            let current = settings(&base, media).await;
            assert_eq!(current["enabled"], enabled);
            assert_eq!(current["defined"], defined);
            let revision = current["revision"].clone();
            assert!(
                snapshots::import(&db, app, bytes.clone(), false)
                    .await
                    .unwrap()
                    .applied
            );
            assert_eq!(settings(&base, media).await["revision"], revision);
            assert_eq!(
                scalar(&c, "SELECT sum(cdh_version) FROM snapshot_imports").await,
                1
            );
            server.stop().await;
        }
    }
    let scratch = Scratch::new();
    let db = Arc::new(Database::open_local(scratch.0.join("local")).await.unwrap());
    let (base, server) = server(db.clone()).await;
    master(&base, "tv", true).await;
    let source=fixture(&scratch,233,"equal","CREATE TABLE Config(Key TEXT,Value TEXT);INSERT INTO Config VALUES('enablecompleteddownloadhandling','True');").await;
    assert!(
        snapshots::import(&db, Application::Sonarr, source, false)
            .await
            .unwrap()
            .applied
    );
    let c = db.connect().await.unwrap();
    assert_eq!(
        scalar(&c, "SELECT sum(cdh_version) FROM snapshot_imports").await,
        0,
        "equal native intent never gains source provenance"
    );
    let bad=fixture(&scratch,233,"bad","CREATE TABLE Config(Key TEXT,Value TEXT);INSERT INTO Config VALUES('enablecompleteddownloadhandling','1');").await;
    let before = scalar(&c, "SELECT count(*) FROM snapshot_imports").await;
    assert!(
        snapshots::import(&db, Application::Sonarr, bad, false)
            .await
            .is_err()
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM snapshot_imports").await,
        before
    );
    server.stop().await;
}
#[tokio::test]
async fn pending_capacity_recovers_through_real_schedule_delete_without_partial_authority() {
    let scratch = Scratch::new();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let c = db.connect().await.unwrap();
    let mut first = String::new();
    for i in 0..65 {
        let id = uuid::Uuid::new_v4().to_string();
        if i == 0 {
            first = id.clone();
        }
        c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint)VALUES(?,'qbittorrent','legacy',1,1,1,1,'http://127.0.0.1:9')",[id.clone()]).await.unwrap();
        c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags)VALUES(?,'qbittorrent','tv','tv',0,0,'started','default',0,0,0)",[id]).await.unwrap();
    }
    c.execute("INSERT INTO download_processing_policies(provider_id,media_type,provider_revision,revision,enabled,mode,enabled_override)VALUES(?,'tv',1,1,1,'hardlink',1)",[first.clone()]).await.unwrap();
    c.execute("INSERT INTO download_refresh_schedules(provider_id,media_type,provider_revision,enabled,interval_seconds,next_run_at,intent,requested_enabled)VALUES(?,'tv',1,1,123,100,'explicit',1)",[first.clone()]).await.unwrap();
    let (base, server) = server(db.clone()).await;
    let (code, value) = request(
        &base,
        "PUT",
        "/api/v1/tv/completed-download-handling",
        json!({"enabled":true,"revision":1}),
    )
    .await;
    assert_eq!(code, 429, "{value}");
    assert_eq!(settings(&base, "tv").await["revision"], 1);
    assert_eq!(settings(&base, "tv").await["reconciliation_pending"], true);
    assert_eq!(
        settings(&base, "tv").await["reconciliation_reason"],
        "schedule_limit"
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM download_processing_policies").await,
        1
    );
    assert_eq!(scalar(&c,"SELECT count(*) FROM download_processing_policies WHERE enabled=1 AND enabled_override=1").await,1);
    let (code, v) = request(
        &base,
        "DELETE",
        "/api/v1/download-refresh/schedules",
        json!({"target":{"provider_id":first,"media_type":"tv"},"revision":1}),
    )
    .await;
    assert_eq!(code, 204, "{v}");
    assert_eq!(settings(&base, "tv").await["reconciliation_pending"], false);
    assert!(settings(&base, "tv").await["reconciliation_reason"].is_null());
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM download_processing_policies WHERE enabled=1"
        )
        .await,
        65
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM download_refresh_schedules WHERE enabled=1 AND intent='inherited'"
        )
        .await,
        64
    );
    let before = scalar(&c, "SELECT count(*) FROM providers").await;
    let (code, v) = request(&base, "POST", "/api/v1/providers", client_input()).await;
    assert_eq!(code, 429, "{v}");
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM providers").await,
        before,
        "shared two-domain provider creation rolls back both scopes on capacity failure"
    );
    server.stop().await;
}
#[tokio::test]
async fn snapshot_unavailable_unsupported_malformed_late_failure_and_disabled_clients() {
    let _serial = SNAPSHOTS.lock().await;
    for version in [233, 206, 242] {
        let app = if version == 233 {
            Application::Sonarr
        } else {
            Application::Radarr
        };
        for (i, extra, invalid) in [
            (0, "", false),
            (
                1,
                "CREATE TABLE Config(Key TEXT,Value TEXT);INSERT INTO Config VALUES('EnableCompletedDownloadHandling','False');",
                false,
            ),
            (
                2,
                "CREATE TABLE Config(Key TEXT,Value TEXT,Unknown TEXT);INSERT INTO Config VALUES('enablecompleteddownloadhandling','False','kept');",
                false,
            ),
            (
                3,
                "CREATE TABLE Config(Key TEXT,Value TEXT);INSERT INTO Config VALUES('enablecompleteddownloadhandling','false'),('enablecompleteddownloadhandling','true');",
                true,
            ),
            (
                4,
                "CREATE TABLE Config(Key TEXT,Value TEXT);INSERT INTO Config VALUES('enablecompleteddownloadhandling','garbage');",
                true,
            ),
            (
                5,
                "CREATE TABLE Config(Key TEXT,Value BLOB);INSERT INTO Config VALUES('enablecompleteddownloadhandling',x'0102');",
                true,
            ),
        ] {
            let s = Scratch::new();
            let db = Database::open_local(s.0.join("db")).await.unwrap();
            let c = db.connect().await.unwrap();
            let bytes = fixture(&s, version, &i.to_string(), extra).await;
            let result = snapshots::import(&db, app, bytes, false).await;
            if invalid {
                assert!(result.is_err());
                assert_eq!(scalar(&c, "SELECT count(*) FROM snapshot_imports").await, 0);
            } else {
                let report = result.unwrap();
                assert!(report.applied);
                assert!(report.unsupported.iter().any(|u| {
                    serde_json::to_string(u)
                        .unwrap()
                        .contains("completed download handling")
                }));
                assert_eq!(
                    scalar(&c, "SELECT sum(cdh_version) FROM snapshot_imports").await,
                    0
                );
            }
            assert_eq!(scalar(&c,"SELECT count(*) FROM completed_download_handling_settings WHERE enabled=1 AND defined=0 AND locally_edited=0").await,2);
        }
        let s = Scratch::new();
        let db = Database::open_local(s.0.join("db")).await.unwrap();
        let c = db.connect().await.unwrap();
        let bytes=fixture(&s,version,"rollback","CREATE TABLE Config(Key TEXT,Value TEXT);INSERT INTO Config VALUES('enablecompleteddownloadhandling','False');").await;
        c.execute_batch("CREATE TRIGGER fail_cdh_marker BEFORE UPDATE OF cdh_version ON snapshot_imports BEGIN SELECT RAISE(ABORT,'synthetic late failure');END;").await.unwrap();
        assert!(
            snapshots::import(&db, app, bytes.clone(), false)
                .await
                .is_err()
        );
        assert_eq!(scalar(&c, "SELECT count(*) FROM snapshot_imports").await, 0);
        assert_eq!(scalar(&c, "SELECT count(*) FROM tags").await, 0);
        assert_eq!(scalar(&c,"SELECT count(*) FROM completed_download_handling_settings WHERE enabled=1 AND defined=0 AND revision=1").await,2);
        c.execute("DROP TRIGGER fail_cdh_marker", ()).await.unwrap();
        assert!(
            snapshots::import(&db, app, bytes, false)
                .await
                .unwrap()
                .applied
        );
        let s = Scratch::new();
        let path = s.0.join("db");
        let db = Database::open_local(&path).await.unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let settings = if version == 233 {
            json!({"host":"127.0.0.1","port":9,"useSsl":false,"tvCategory":"tv","recentTvPriority":0,"olderTvPriority":0})
        } else {
            json!({"host":"127.0.0.1","port":9,"useSsl":false,"movieCategory":"movies","recentMoviePriority":0,"olderMoviePriority":0})
        };
        let extra = format!(
            "CREATE TABLE Config(Key TEXT,Value TEXT);INSERT INTO Config VALUES('enablecompleteddownloadhandling','True');CREATE TABLE DownloadClients(Id INTEGER,Name TEXT,Implementation TEXT,ConfigContract TEXT,Settings TEXT,Priority INTEGER,Enable INTEGER);INSERT INTO DownloadClients VALUES(1,'Client','QBittorrent','QBittorrentSettings','{settings}',1,1);"
        );
        let bytes = fixture(&s, version, "providers", &extra).await;
        let key = hrrdarr::providers::CredentialKey::from_hex(&"ac".repeat(32)).unwrap();
        let report = snapshots::import_with_providers(&db, app, bytes, false, Some(&key))
            .await
            .unwrap();
        assert!(report.applied, "{report:?}");
        let c = db.connect().await.unwrap();
        assert_eq!(
            scalar(&c, "SELECT count(*) FROM providers WHERE enabled=0").await,
            1
        );
        assert_eq!(scalar(&c,"SELECT count(*) FROM download_processing_policies WHERE enabled=0 AND enabled_override IS NULL").await,1);
        assert_eq!(
            scalar(
                &c,
                "SELECT count(*) FROM download_refresh_schedules WHERE enabled=1"
            )
            .await,
            0
        );
    }
}

#[tokio::test]
async fn provider_scope_lifecycle_preserves_intent_and_failed_credentials_are_atomic() {
    for suppressed in ["tv", "movies"] {
        let explicit = if suppressed == "tv" { "movies" } else { "tv" };
        let s = Scratch::new();
        let db = Arc::new(Database::open_local(s.0.join("db")).await.unwrap());
        let (base, server) = server(db.clone()).await;
        let (code, mut provider) =
            request(&base, "POST", "/api/v1/providers", client_input()).await;
        assert_eq!(code, 201, "{provider}");
        let id = provider["id"].clone();
        for media in ["tv", "movies"] {
            let p = policy(&base, &id, media).await;
            let (code,value)=request(&base,"PUT",&format!("/api/v1/download-processing/policies/{}/{media}",id.as_str().unwrap()),json!({"revision":p["revision"],"provider_revision":1,"enabled":false,"mode":"hardlink"})).await;
            assert_eq!(code, 200, "{value}");
        }
        let (_, schedules) = request(
            &base,
            "GET",
            "/api/v1/download-refresh/schedules",
            Value::Null,
        )
        .await;
        for schedule in schedules.as_array().unwrap() {
            let media = schedule["target"]["media_type"].as_str().unwrap();
            let (code, v) = if media == suppressed {
                request(
                    &base,
                    "DELETE",
                    "/api/v1/download-refresh/schedules",
                    json!({"target":schedule["target"],"revision":schedule["revision"]}),
                )
                .await
            } else {
                request(&base,"PUT","/api/v1/download-refresh/schedules",json!({"target":schedule["target"],"revision":schedule["revision"],"provider_revision":1,"enabled":false,"interval_seconds":777})).await
            };
            assert_eq!(code, if media == suppressed { 204 } else { 200 }, "{v}");
        }
        let path = format!("/api/v1/providers/{}", id.as_str().unwrap());
        let c = db.connect().await.unwrap();
        for (enabled, remove_scope) in [(false, false), (true, false), (true, true), (true, false)]
        {
            let mut config = client_input()["settings"].clone();
            if remove_scope {
                config[suppressed] = Value::Null;
            }
            let (code,v)=request(&base,"PUT",&path,json!({"revision":provider["revision"],"name":"lifecycle","enabled":enabled,"priority":1,"settings":config})).await;
            assert_eq!(code, 200, "{v}");
            provider = v;
            // Removed scopes retain intentional off/suppression; re-add cannot infer opt-in.
            assert_eq!(scalar(&c,"SELECT count(*) FROM download_processing_policies WHERE enabled=0 AND enabled_override=0 AND mode='hardlink'").await,2);
            assert_eq!(scalar(&c,&format!("SELECT count(*) FROM download_refresh_schedules WHERE media_type='{suppressed}' AND intent='suppressed' AND enabled=0")).await,1);
            assert_eq!(scalar(&c,&format!("SELECT count(*) FROM download_refresh_schedules WHERE media_type='{explicit}' AND intent='explicit' AND requested_enabled=0 AND enabled=0 AND interval_seconds=777")).await,1);
        }
        let before = database_state(&c).await;
        let (code,v)=request(&base,"PUT",&path,json!({"revision":provider["revision"],"name":"must rollback","enabled":false,"priority":1,"settings":client_input()["settings"],"credentials":{"kind":"username_password","username":"synthetic","password":"synthetic"}})).await;
        assert_eq!(code, 503, "{v}");
        assert_eq!(
            database_state(&c).await,
            before,
            "failed credential storage cannot mutate scopes, authority or schedule intent"
        );
        server.stop().await;
        drop(c);
        drop(db);
        let reopened = Database::open_local(s.0.join("db")).await.unwrap();
        let c = reopened.connect().await.unwrap();
        assert_eq!(
            database_state(&c).await,
            before,
            "intent and authority survive reopen unchanged"
        );
    }
}
// Compare every row/column in affected tables; counts alone miss changed revisions
// or partial settings writes on a failed source activation.
async fn database_state(c: &libsql::Connection) -> Vec<String> {
    let mut out = Vec::new();
    for table in [
        "providers",
        "provider_scopes",
        "snapshot_provider_mappings",
        "download_processing_policies",
        "download_refresh_schedules",
        "completed_download_handling_settings",
        "snapshot_imports",
        "snapshot_records",
        "snapshot_mappings",
        "tags",
        "series",
        "movies",
        "movie_metadata",
        "seasons",
        "episodes",
        "episode_files",
        "movie_files",
        "series_tags",
        "movie_tags",
    ] {
        let mut rows = c
            .query(&format!("SELECT * FROM {table} ORDER BY rowid"), ())
            .await
            .unwrap();
        out.push(table.to_owned());
        while let Some(row) = rows.next().await.unwrap() {
            let values = (0..row.column_count())
                .map(|i| format!("{:?}", row.get_value(i).unwrap()))
                .collect::<Vec<_>>();
            out.push(format!("{values:?}"));
        }
    }
    out
}
#[tokio::test]
async fn source_false_governs_later_clients_and_local_false_blocks_source_true() {
    let _serial = SNAPSHOTS.lock().await;
    for version in [233, 206, 242] {
        let app = if version == 233 {
            Application::Sonarr
        } else {
            Application::Radarr
        };
        let media = if version == 233 { "tv" } else { "movies" };
        let s = Scratch::new();
        let db = Arc::new(Database::open_local(s.0.join("db")).await.unwrap());
        let bytes=fixture(&s,version,"source-off","CREATE TABLE Config(Key TEXT,Value TEXT);INSERT INTO Config VALUES('enablecompleteddownloadhandling','False');").await;
        assert!(
            snapshots::import(&db, app, bytes, false)
                .await
                .unwrap()
                .applied
        );
        let (base, server) = server(db.clone()).await;
        let (code, p) = request(&base, "POST", "/api/v1/providers", client_input()).await;
        assert_eq!(code, 201, "{p}");
        let current = policy(&base, &p["id"], media).await;
        assert_eq!(current["enabled"], false);
        assert!(current["enabled_override"].is_null());
        assert_eq!(current["observation_enabled"], false);
        server.stop().await;
        // A separate fresh destination isolates native intent from prior source provenance.
        let db = Arc::new(Database::open_local(s.0.join("native-off")).await.unwrap());
        let (base, server) = self::server(db.clone()).await;
        master(&base, media, false).await;
        let c = db.connect().await.unwrap();
        let before = database_state(&c).await;
        let bytes=fixture(&s,version,"source-on","CREATE TABLE Config(Key TEXT,Value TEXT);INSERT INTO Config VALUES('enablecompleteddownloadhandling','True');").await;
        let report = snapshots::import(&db, app, bytes, false).await.unwrap();
        assert!(!report.applied);
        assert!(report.conflicts > 0);
        assert_eq!(database_state(&c).await, before);
        server.stop().await;
    }
}
#[tokio::test]
async fn snapshot_capacity_failure_rolls_back_providers_core_archive_and_settings() {
    let _serial = SNAPSHOTS.lock().await;
    for version in [233, 206, 242] {
        let media = if version == 233 { "tv" } else { "movies" };
        let app = if version == 233 {
            Application::Sonarr
        } else {
            Application::Radarr
        };
        let s = Scratch::new();
        let path = s.0.join("db");
        let db = Database::open_local(&path).await.unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let c = db.connect().await.unwrap();
        for _ in 0..65 {
            let id = uuid::Uuid::new_v4().to_string();
            c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint)VALUES(?,'qbittorrent','legacy',1,1,1,1,'http://127.0.0.1:9')",[id.clone()]).await.unwrap();
            c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags)VALUES(?,'qbittorrent',?,'owned',0,0,'started','default',0,0,0)",libsql::params![id,media]).await.unwrap();
        }
        let before = database_state(&c).await;
        let config = if version == 233 {
            json!({"host":"127.0.0.1","port":9,"tvCategory":"tv"})
        } else {
            json!({"host":"127.0.0.1","port":9,"movieCategory":"movies"})
        };
        let extra = format!(
            "CREATE TABLE Config(Key TEXT,Value TEXT);INSERT INTO Config VALUES('enablecompleteddownloadhandling','True');CREATE TABLE DownloadClients(Id INTEGER,Name TEXT,Implementation TEXT,ConfigContract TEXT,Settings TEXT,Priority INTEGER,Enable INTEGER);INSERT INTO DownloadClients VALUES(1,'Source client','QBittorrent','QBittorrentSettings','{config}',1,1);"
        );
        let bytes = fixture(&s, version, "capacity", &extra).await;
        let key = hrrdarr::providers::CredentialKey::from_hex(&"ac".repeat(32)).unwrap();
        let error = snapshots::import_with_providers(&db, app, bytes, false, Some(&key))
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "completed download handling activation failed"
        );
        assert_eq!(
            database_state(&c).await,
            before,
            "all source and native state rolls back on aggregate capacity failure"
        );
    }
}
