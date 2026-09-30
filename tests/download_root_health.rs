//! Independent hc.020 behavior through the real health API/worker and owned HTTP peers.
use axum::{
    Router,
    extract::{OriginalUri, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
};
use hrrdarr::{
    commands,
    db::Database,
    health, providers, remote_paths, root_folders,
    snapshots::{self, Application},
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

const KEY: &str = "download_client_root_folder";
const SECRET: &str = "ROOT_HEALTH_CREDENTIAL_SENTINEL";
const PRIVATE: &str = "ROOT_HEALTH_PRIVATE_SENTINEL";
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}
impl Server {
    async fn stop(mut self) {
        self.0.abort();
        let _ = (&mut self.0).await;
    }
}
async fn serve(app: Router) -> (String, Server) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    (
        endpoint,
        Server(tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        })),
    )
}
#[derive(Clone)]
struct Peer {
    paths: Arc<Mutex<(String, Value)>>,
    mode: Arc<AtomicUsize>,
    block: Arc<AtomicBool>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    writes: Arc<AtomicUsize>,
}
impl Peer {
    fn outputs(&self, default: &str, tv: Option<&str>, movies: Option<&str>) {
        let mut categories = json!({});
        for (name, path) in [("tv", tv), ("movies", movies)] {
            if let Some(path) = path {
                categories[name] = json!({"name":name,"savePath":path});
            }
        }
        *self.paths.lock().unwrap() = (default.into(), categories);
    }
}
async fn peer(State(p): State<Peer>, method: Method, OriginalUri(uri): OriginalUri) -> Response {
    if method != Method::GET {
        p.writes.fetch_add(1, Ordering::SeqCst);
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    match uri.path() {
        "/api/v2/app/webapiVersion" => "2.8.0".into_response(),
        "/api/v2/app/version" => "4.5.0".into_response(),
        "/api/v2/torrents/info" => axum::Json(json!([])).into_response(),
        "/api/v2/app/preferences" => {
            let mode = p.mode.load(Ordering::SeqCst);
            if mode == 1 {
                return (StatusCode::BAD_GATEWAY, format!("{SECRET} {PRIVATE}")).into_response();
            }
            if mode == 2 {
                return format!("malformed {PRIVATE}").into_response();
            }
            let mut value = json!({"queueing_enabled":true,"max_ratio_enabled":false,"max_seeding_time_enabled":false,"max_seeding_time":-1,"max_ratio_act":0,"save_path":p.paths.lock().unwrap().0});
            if mode == 3 {
                value.as_object_mut().unwrap().remove("save_path");
            }
            if mode == 4 {
                value["save_path"] = json!("");
            }
            axum::Json(value).into_response()
        }
        "/api/v2/torrents/categories" => {
            let value = p.paths.lock().unwrap().1.clone();
            if p.block.swap(false, Ordering::SeqCst) {
                p.entered.notify_one();
                tokio::time::timeout(Duration::from_secs(8), p.release.notified())
                    .await
                    .unwrap();
            }
            axum::Json(value).into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}
struct Fixture {
    runtime: Option<commands::Runtime>,
    app: Option<Server>,
    upstream: Option<Server>,
    db: Arc<Database>,
    refresh: providers::RefreshClient,
    client: reqwest::Client,
    base: String,
    peer: Peer,
    provider: Value,
    endpoint: String,
    scratch: Scratch,
}
impl Fixture {
    async fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "hrrdarr-download-root-health-api-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&path).unwrap();
        let db = Arc::new(Database::open_local(path.join("db")).await.unwrap());
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path.join("db"), std::fs::Permissions::from_mode(0o600)).unwrap();
        let peer = Peer {
            paths: Arc::new(Mutex::new(("/safe-default".into(), json!({})))),
            mode: Arc::new(AtomicUsize::new(0)),
            block: Arc::new(AtomicBool::new(false)),
            entered: Arc::new(tokio::sync::Notify::new()),
            release: Arc::new(tokio::sync::Notify::new()),
            writes: Arc::new(AtomicUsize::new(0)),
        };
        let (endpoint, upstream) = serve(
            Router::new()
                .fallback(peer_handler())
                .with_state(peer.clone()),
        )
        .await;
        let credential_key =
            Arc::new(providers::CredentialKey::from_hex(&"12".repeat(32)).unwrap());
        let (routes, refresh) = providers::router_with_refresh(db.clone(), Some(credential_key));
        let app = routes
            .merge(health::router(db.clone()))
            .merge(root_folders::router(db.clone()))
            .merge(remote_paths::router(db.clone()));
        let (base, app) = serve(app).await;
        let mut f = Self {
            runtime: None,
            app: Some(app),
            upstream: Some(upstream),
            db,
            refresh,
            client: reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap(),
            base,
            peer,
            provider: Value::Null,
            endpoint,
            scratch: Scratch(path),
        };
        let mut config = f.config();
        config["credentials"] = json!({"kind":"api_key","api_key":SECRET});
        f.provider = f.call("POST", "/api/v1/providers", Some(config), 201).await;
        f.runtime = Some(
            commands::start(f.db.clone(), f.refresh.clone())
                .await
                .unwrap(),
        );
        f.settle().await;
        f
    }
    fn path(&self, name: &str) -> String {
        self.scratch
            .0
            .join(PRIVATE)
            .join(name)
            .to_str()
            .unwrap()
            .to_owned()
    }
    fn config(&self) -> Value {
        let scope = |name| json!({"category":name,"imported_category":null,"recent_priority":0,"older_priority":0});
        json!({"name":"Root health fixture","enabled":true,"priority":1,"settings":{"implementation":"qbittorrent","endpoint":self.endpoint,"tv":scope("tv"),"movies":scope("movies")}})
    }
    async fn call(&self, method: &str, path: &str, body: Option<Value>, expected: u16) -> Value {
        let mut req = self.client.request(
            method.parse::<Method>().unwrap(),
            format!("{}{path}", self.base),
        );
        if let Some(body) = body {
            req = req
                .header("content-type", "application/json")
                .body(body.to_string());
        }
        let response = req.send().await.unwrap();
        let status = response.status().as_u16();
        let bytes = response.bytes().await.unwrap();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        assert_eq!(status, expected, "{method} {path}: {value}");
        value
    }
    async fn health(&self) -> Value {
        self.call("GET", "/api/v1/health", None, 200).await
    }
    async fn settle(&self) -> Value {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let health = self.health().await;
                if ["tv", "movies"]
                    .iter()
                    .all(|scope| check(&health, scope)["evaluation"] == "current")
                    && health["active_command"].is_null()
                {
                    return health;
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .expect("root health must settle through owned worker")
    }
    async fn enqueue(&self) -> Value {
        self.call(
            "POST",
            "/api/v1/health/commands",
            Some(json!({"scope":"all","priority":"high"})),
            202,
        )
        .await
    }
    async fn blocked_movie_root(&self) -> Value {
        // Source-confirmed native order: Movies CDH does not probe status; communication
        // only requests torrents/info. Thus this categories request is the root evaluator,
        // after it captured all configured roots, not the TV CDH status request.
        self.peer.block.store(true, Ordering::SeqCst);
        let command = self
            .call(
                "POST",
                "/api/v1/health/commands",
                Some(json!({"scope":"movies","priority":"high"})),
                202,
            )
            .await;
        tokio::time::timeout(Duration::from_secs(5), self.peer.entered.notified())
            .await
            .unwrap();
        let detail = self
            .call(
                "GET",
                &format!(
                    "/api/v1/health/commands/{}",
                    command["command_id"].as_str().unwrap()
                ),
                None,
                200,
            )
            .await;
        assert_eq!(detail["status"], "running");
        assert_eq!(detail["attempts"], 1);
        let members = detail["members"].as_array().unwrap();
        assert!(members.iter().all(|m| m["identity"]["scope"] == "movies"));
        let root = members
            .iter()
            .find(|m| m["identity"]["check_key"] == KEY)
            .unwrap();
        assert_eq!(
            root["captured_generation"],
            check(&self.health().await, "movies")["generation"]
        );
        assert!(!root["captured_generation"].is_null());
        command
    }
    async fn evaluate(&self, tv: bool, movies: bool) -> Value {
        self.enqueue().await;
        let value = self.settle().await;
        for (scope, warn) in [("tv", tv), ("movies", movies)] {
            let issue = issue(&value, scope);
            assert_eq!(issue.is_some(), warn, "{scope}: {value}");
            if let Some(issue) = issue {
                assert_eq!(issue["severity"], "warning");
                assert_eq!(issue["compatibility_type"], "DownloadClientRootFolderCheck");
                assert!(
                    issue["wiki_url"]
                        .as_str()
                        .unwrap()
                        .ends_with("#downloads-in-root-folder")
                );
            }
        }
        self.redacted(&value);
        value
    }
    fn redacted(&self, value: &Value) {
        let text = value.to_string();
        for private in [SECRET, PRIVATE] {
            assert!(!text.contains(private), "health must redact {private}");
        }
    }
    async fn root(&self, scope: &str, path: &str) -> Value {
        assert!(Path::new(path).starts_with(&self.scratch.0));
        std::fs::create_dir_all(path).unwrap();
        self.call(
            "POST",
            &format!("/api/v1/{scope}/root-folders"),
            Some(json!({"path":path})),
            201,
        )
        .await
    }
    async fn map(&self, scope: &str, local: &str) -> Value {
        assert!(Path::new(local).starts_with(&self.scratch.0));
        std::fs::create_dir_all(local).unwrap();
        self.call(
            "POST",
            &format!("/api/v1/{scope}/remote-path-mappings"),
            Some(json!({"host":"127.0.0.1","remote_path":"/remote","local_path":local})),
            201,
        )
        .await
    }
    async fn wait_retry(&self, id: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(12), async {
            loop {
                let command = self
                    .call("GET", &format!("/api/v1/health/commands/{id}"), None, 200)
                    .await;
                if command["status"] == "retry_wait" || command["status"] == "failed" {
                    return command;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("attempt must report its failed/stale outcome")
    }
    async fn stop(mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown().await;
        }
        self.app.take().unwrap().stop().await;
        self.upstream.take().unwrap().stop().await;
        assert_eq!(
            self.peer.writes.load(Ordering::SeqCst),
            0,
            "health cannot submit/change downloads"
        );
    }
}
fn peer_handler() -> axum::routing::MethodRouter<Peer> {
    axum::routing::any(peer)
}
fn check<'a>(value: &'a Value, scope: &str) -> &'a Value {
    value["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["identity"]["scope"] == scope && v["identity"]["check_key"] == KEY)
        .unwrap()
}
fn issue<'a>(value: &'a Value, scope: &str) -> Option<&'a Value> {
    value["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["identity"]["scope"] == scope && v["identity"]["check_key"] == KEY)
}
fn generations(value: &Value) -> [i64; 2] {
    [
        check(value, "tv")["generation"].as_i64().unwrap(),
        check(value, "movies")["generation"].as_i64().unwrap(),
    ]
}

#[tokio::test]
async fn shared_client_unused_roots_boundaries_and_default_category_paths() {
    let f = Fixture::new().await;
    let tv = f.path("tv%_[x]");
    let movie = f.path("movies");
    f.root("tv", &tv).await;
    f.root("movies", &movie).await;
    let c = f.db.connect().await.unwrap();
    assert_eq!(
        c.query(
            "SELECT (SELECT count(*) FROM series)+(SELECT count(*) FROM movies)",
            ()
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<i64>(0)
        .unwrap(),
        0,
        "configured unused roots must be enough"
    );
    for (tv_output, movie_output, tv_warn, movie_warn) in [
        (tv.clone(), "/safe".into(), true, false),
        ("/safe".into(), format!("{movie}/child"), false, true),
        (format!("{movie}/child"), format!("{tv}/child"), true, true),
        (format!("{tv}-sibling"), f.path("tvZZa[x]"), false, false),
        (f.path(""), "/safe".into(), false, false),
    ] {
        f.peer
            .outputs("/safe-default", Some(&tv_output), Some(&movie_output));
        f.evaluate(tv_warn, movie_warn).await;
    }
    // Absent and empty category both use existing qBittorrent default fallback.
    f.peer.outputs(&tv, None, Some(""));
    f.evaluate(true, true).await;
    f.peer
        .outputs(&tv, Some("/safe-override"), Some("relative-child"));
    f.evaluate(false, true).await;
    let transitions = f.call("GET", "/api/v1/health/transitions", None, 200).await;
    f.redacted(&transitions);
    drop(c);
    f.stop().await;
}

#[tokio::test]
async fn scoped_mappings_failures_mutation_rollback_and_recovery() {
    let mut f = Fixture::new().await;
    let collision = f.path("declared");
    let root = f.root("movies", &collision).await;
    f.peer.outputs("/remote/downloads", None, None);
    let tv = f.map("tv", &collision).await;
    let movie = f.map("movies", &f.path("safe-mapped")).await;
    f.evaluate(true, false).await;
    // Mapping CAS failure must not dirty either diagnostic generation.
    let before = generations(&f.health().await);
    f.call("PUT",&format!("/api/v1/movies/remote-path-mappings/{}",movie["id"]),Some(json!({"host":"127.0.0.1","remote_path":"/remote","local_path":collision,"revision":movie["revision"].as_i64().unwrap()+1})),409).await;
    assert_eq!(generations(&f.health().await), before);
    let updated=f.call("PUT",&format!("/api/v1/movies/remote-path-mappings/{}",movie["id"]),Some(json!({"host":"127.0.0.1","remote_path":"/remote","local_path":collision,"revision":movie["revision"]})),200).await;
    let after = generations(&f.health().await);
    assert_eq!(after, [before[0], before[1] + 1]);
    f.evaluate(true, true).await;
    for mode in [3, 4, 2, 1] {
        f.peer.mode.store(mode, Ordering::SeqCst);
        let command = f.enqueue().await;
        f.wait_retry(command["command_id"].as_str().unwrap()).await;
        let failed = f.health().await;
        assert!(
            issue(&failed, "tv").is_some(),
            "failed status must retain previous warning"
        );
        assert_ne!(check(&failed, "tv")["evaluation"], "current");
        f.redacted(&failed);
        f.call(
            "POST",
            &format!(
                "/api/v1/health/commands/{}/cancel",
                command["command_id"].as_str().unwrap()
            ),
            None,
            200,
        )
        .await;
    }
    f.peer.mode.store(0, Ordering::SeqCst);
    f.call(
        "DELETE",
        &format!(
            "/api/v1/tv/remote-path-mappings/{}?revision={}",
            tv["id"], tv["revision"]
        ),
        None,
        204,
    )
    .await;
    f.call(
        "DELETE",
        &format!(
            "/api/v1/movies/remote-path-mappings/{}?revision={}",
            updated["id"], updated["revision"]
        ),
        None,
        204,
    )
    .await;
    f.evaluate(false, false).await;
    // Provider revision stale save is rejected; deletion removes applicability without media effects.
    let mut config = f.config();
    config["revision"] = json!(f.provider["revision"].as_i64().unwrap() + 1);
    f.call(
        "PUT",
        &format!("/api/v1/providers/{}", f.provider["id"].as_str().unwrap()),
        Some(config),
        409,
    )
    .await;
    let owned = f.scratch.0.join("sentinel.mkv");
    std::fs::write(&owned, b"original-media").unwrap();
    f.call(
        "DELETE",
        &format!("/api/v1/movies/root-folders/{}", root["id"]),
        None,
        204,
    )
    .await;
    f.call(
        "DELETE",
        &format!(
            "/api/v1/providers/{}?revision={}",
            f.provider["id"].as_str().unwrap(),
            f.provider["revision"]
        ),
        None,
        204,
    )
    .await;
    f.evaluate(false, false).await;
    f.runtime.take().unwrap().shutdown().await;
    f.runtime = Some(
        commands::start(f.db.clone(), f.refresh.clone())
            .await
            .unwrap(),
    );
    f.settle().await;
    assert_eq!(std::fs::read(owned).unwrap(), b"original-media");
    f.stop().await;
}

#[tokio::test]
async fn root_mapping_and_provider_mutations_fence_inflight_health_publication() {
    for mutation in ["root", "mapping", "provider"] {
        let mut f = Fixture::new().await;
        let root = f.path("race-root");
        let mut mapping = Value::Null;
        if mutation == "mapping" {
            f.root("tv", &root).await;
            mapping = f.map("movies", &f.path("safe")).await;
            f.peer.outputs("/remote/downloads", Some("/safe"), None);
        } else {
            f.peer
                .outputs("/safe", Some("/safe"), Some(&format!("{root}/downloads")));
        }
        f.evaluate(false, false).await;
        let command = f.blocked_movie_root().await;
        let before = generations(&f.health().await);
        match mutation {
            "root" => {
                f.root("tv", &root).await;
            }
            "mapping" => {
                f.call("PUT",&format!("/api/v1/movies/remote-path-mappings/{}",mapping["id"]),Some(json!({"host":"127.0.0.1","remote_path":"/remote","local_path":root,"revision":mapping["revision"]})),200).await;
            }
            "provider" => {
                let mut config = f.config();
                config["revision"] = f.provider["revision"].clone();
                config["settings"]["movies"] = Value::Null;
                f.provider = f
                    .call(
                        "PUT",
                        &format!("/api/v1/providers/{}", f.provider["id"].as_str().unwrap()),
                        Some(config),
                        200,
                    )
                    .await;
            }
            _ => unreachable!(),
        }
        let after = generations(&f.health().await);
        if mutation == "mapping" {
            assert_eq!(after, [before[0], before[1] + 1]);
        } else {
            assert_eq!(after, [before[0] + 1, before[1] + 1]);
        }
        f.peer.release.notify_one();
        let id = command["command_id"].as_str().unwrap();
        let stale = f.wait_retry(id).await;
        assert_eq!(stale["error_code"], "stale_inputs", "{mutation}: {stale}");
        let pending = f.health().await;
        assert_ne!(
            check(&pending, "movies")["evaluation"],
            "current",
            "old attempt cannot publish current for mutated inputs"
        );
        f.call(
            "POST",
            &format!("/api/v1/health/commands/{id}/cancel"),
            None,
            200,
        )
        .await;
        f.evaluate(false, mutation != "provider").await;
        f.stop().await;
    }
}

async fn snapshot(path: &Path, app: Application, root: &str) -> Vec<u8> {
    let db = libsql::Builder::new_local(path).build().await.unwrap();
    let c = db.connect().unwrap();
    c.execute_batch(if matches!(app,Application::Sonarr){"CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES(233);CREATE TABLE Series(Id INTEGER,TvdbId INTEGER,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,Seasons TEXT);CREATE TABLE Episodes(Id INTEGER,SeriesId INTEGER,SeasonNumber INTEGER,EpisodeNumber INTEGER,Title TEXT,Monitored INTEGER,EpisodeFileId INTEGER);CREATE TABLE EpisodeFiles(Id INTEGER,SeriesId INTEGER,RelativePath TEXT);"}else{"CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES(242);CREATE TABLE MovieMetadata(Id INTEGER,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER);CREATE TABLE Movies(Id INTEGER,MovieMetadataId INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER);CREATE TABLE MovieFiles(Id INTEGER,MovieId INTEGER,RelativePath TEXT,Edition TEXT);"}).await.unwrap();
    c.execute_batch("CREATE TABLE RootFolders(Id INTEGER,Path TEXT);")
        .await
        .unwrap();
    c.execute("INSERT INTO RootFolders VALUES(1,?)", [root])
        .await
        .unwrap();
    drop(c);
    drop(db);
    std::fs::read(path).unwrap()
}

#[tokio::test]
async fn both_snapshot_roots_invalidate_atomically_and_restart_preserves_media() {
    let mut f = Fixture::new().await;
    let owned = f.scratch.0.join("media-sentinel.mkv");
    std::fs::write(&owned, b"preserved").unwrap();
    for (index, app) in [Application::Sonarr, Application::Radarr]
        .into_iter()
        .enumerate()
    {
        let root = f.path(&format!("snapshot-{index}"));
        let source = f.scratch.0.join(format!("source-{index}.db"));
        let bytes = snapshot(&source, app, &root).await;
        f.peer
            .outputs("/safe", Some("/safe"), Some(&format!("{root}/downloads")));
        f.evaluate(false, false).await;
        let before = generations(&f.health().await);
        let dry = snapshots::import(&f.db, app, bytes.clone(), true)
            .await
            .unwrap();
        assert!(!dry.applied);
        assert_eq!(generations(&f.health().await), before);
        let c = f.db.connect().await.unwrap();
        c.execute_batch("CREATE TRIGGER fail_root_snapshot BEFORE INSERT ON snapshot_records BEGIN SELECT RAISE(ABORT,'owned late snapshot failure'); END;").await.unwrap();
        assert!(
            snapshots::import(&f.db, app, bytes.clone(), false)
                .await
                .is_err()
        );
        c.execute_batch("DROP TRIGGER fail_root_snapshot;")
            .await
            .unwrap();
        assert_eq!(
            generations(&f.health().await),
            before,
            "snapshot rollback includes health invalidation"
        );
        assert_eq!(
            c.query(
                "SELECT count(*) FROM root_folders WHERE path=?",
                [root.clone()]
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<i64>(0)
            .unwrap(),
            0
        );
        // Block a captured old attempt while snapshot adds a previously unused root.
        let command = f.blocked_movie_root().await;
        let before = generations(&f.health().await);
        let imported = snapshots::import(&f.db, app, bytes.clone(), false)
            .await
            .unwrap();
        assert!(imported.applied);
        assert_eq!(
            generations(&f.health().await),
            [before[0] + 1, before[1] + 1]
        );
        f.peer.release.notify_one();
        let id = command["command_id"].as_str().unwrap();
        assert_eq!(f.wait_retry(id).await["error_code"], "stale_inputs");
        f.call(
            "POST",
            &format!("/api/v1/health/commands/{id}/cancel"),
            None,
            200,
        )
        .await;
        f.evaluate(false, true).await;
        let before = generations(&f.health().await);
        snapshots::import(&f.db, app, bytes.clone(), false)
            .await
            .unwrap();
        assert_eq!(
            generations(&f.health().await),
            before,
            "replay does not insert or invalidate roots"
        );
        assert_eq!(std::fs::read(&source).unwrap(), bytes);
        drop(c);
    }
    // Restart interrupts a real running read; original media and durable roots remain.
    f.blocked_movie_root().await;
    f.runtime.take().unwrap().shutdown().await;
    f.peer.release.notify_one();
    f.runtime = Some(
        commands::start(f.db.clone(), f.refresh.clone())
            .await
            .unwrap(),
    );
    f.settle().await;
    f.evaluate(false, true).await;
    assert_eq!(std::fs::read(owned).unwrap(), b"preserved");
    f.stop().await;
}

#[tokio::test]
async fn definite_collision_survives_failed_client_in_both_sorted_orders() {
    let mut f = Fixture::new().await;
    let root = f.path("mixed");
    f.root("tv", &root).await;
    f.peer.outputs(&root, None, None);
    let failed = Peer {
        paths: Arc::new(Mutex::new(("/safe".into(), json!({})))),
        mode: Arc::new(AtomicUsize::new(1)),
        block: Arc::new(AtomicBool::new(false)),
        entered: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
        writes: Arc::new(AtomicUsize::new(0)),
    };
    let (failed_endpoint, failed_server) = serve(
        Router::new()
            .fallback(peer_handler())
            .with_state(failed.clone()),
    )
    .await;
    let mut config = f.config();
    config["name"] = json!("Second root fixture");
    config["settings"]["endpoint"] = json!(failed_endpoint);
    let mut second = f.call("POST", "/api/v1/providers", Some(config), 201).await;
    let mut orders = Vec::new();
    for swapped in [false, true] {
        if swapped {
            let mut config = f.config();
            config["revision"] = f.provider["revision"].clone();
            config["settings"]["endpoint"] = json!(failed_endpoint);
            f.provider = f
                .call(
                    "PUT",
                    &format!("/api/v1/providers/{}", f.provider["id"].as_str().unwrap()),
                    Some(config),
                    200,
                )
                .await;
            let mut config = f.config();
            config["name"] = json!("Second root fixture");
            config["revision"] = second["revision"].clone();
            second = f
                .call(
                    "PUT",
                    &format!("/api/v1/providers/{}", second["id"].as_str().unwrap()),
                    Some(config),
                    200,
                )
                .await;
        }
        let c = f.db.connect().await.unwrap();
        let first_id = c
            .query(
                "SELECT id FROM providers WHERE enabled=1 ORDER BY id LIMIT 1",
                (),
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap();
        let first_is_primary = first_id == f.provider["id"].as_str().unwrap();
        orders.push(first_is_primary != swapped);
        drop(c);
        let command = f.enqueue().await;
        // TV CDH cannot evaluate the failed client, so whole-command retry is expected.
        // Root outcomes must nevertheless publish the definite collision in both scopes.
        f.wait_retry(command["command_id"].as_str().unwrap()).await;
        let health = f.health().await;
        for scope in ["tv", "movies"] {
            assert_eq!(issue(&health, scope).unwrap()["severity"], "warning");
            assert_eq!(
                check(&health, scope)["observed_generation"],
                check(&health, scope)["generation"]
            );
            assert!(
                check(&health, scope)["last_error"].is_null(),
                "definite collision cannot be hidden by another client failure"
            );
        }
        f.redacted(&health);
        f.call(
            "POST",
            &format!(
                "/api/v1/health/commands/{}/cancel",
                command["command_id"].as_str().unwrap()
            ),
            None,
            200,
        )
        .await;
    }
    assert_ne!(
        orders[0], orders[1],
        "actual ORDER BY id evaluated conflict-first and failure-first"
    );
    assert_eq!(failed.writes.load(Ordering::SeqCst), 0);
    failed_server.stop().await;
    f.stop().await;
}
