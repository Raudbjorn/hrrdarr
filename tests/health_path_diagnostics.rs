//! hc.021 through actual import/Health APIs, an owned client fixture and scratch files.
use axum::{
    Router,
    extract::{OriginalUri, State},
    response::{IntoResponse, Response},
};
use hrrdarr::{
    commands, completed_download_handling, db::Database, health, import, providers, remote_paths,
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn serve(app: Router) -> (String, Server) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    (
        address,
        Server(tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap()
        })),
    )
}
async fn peer(State(path): State<Arc<Mutex<Value>>>, OriginalUri(uri): OriginalUri) -> Response {
    match uri.path() {
        "/api/v2/app/webapiVersion"=>"2.8.0".into_response(),
        "/api/v2/app/version"=>"4.5.0".into_response(),
        "/api/v2/torrents/info"=>axum::Json(json!([])).into_response(),
        "/api/v2/torrents/categories"=>axum::Json(json!({})).into_response(),
        "/api/v2/app/preferences"=>axum::Json(json!({"queueing_enabled":true,"max_ratio_enabled":false,"max_seeding_time_enabled":false,"max_seeding_time":-1,"max_ratio_act":0,"save_path":path.lock().unwrap().clone()})).into_response(),
        _=>axum::http::StatusCode::NOT_FOUND.into_response(),
    }
}
async fn call(
    client: &reqwest::Client,
    base: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected: u16,
) -> Value {
    let mut request = client.request(method.parse().unwrap(), format!("{base}{path}"));
    if let Some(body) = body {
        request = request
            .header("content-type", "application/json")
            .body(body.to_string());
    }
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    let value: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
    assert_eq!(status, expected, "{method} {path}: {value}");
    value
}
async fn check(client: &reqwest::Client, base: &str, domain: &str) -> Value {
    let value = call(
        client,
        base,
        "GET",
        &format!("/api/v1/health?scope={domain}"),
        None,
        200,
    )
    .await;
    let mut check = value["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["identity"]["check_key"] == "remote_path_mapping")
        .unwrap()
        .clone();
    check["issue"] = value["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["identity"]["check_key"] == "remote_path_mapping")
        .cloned()
        .unwrap_or(Value::Null);
    check
}
async fn recheck(client: &reqwest::Client, base: &str, domain: &str) -> Value {
    let previous = check(client, base, domain).await;
    call(
        client,
        base,
        "POST",
        "/api/v1/health/commands",
        Some(json!({"scope":domain,"priority":"high"})),
        202,
    )
    .await;
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let value = check(client, base, domain).await;
            if value["evaluation"] == "current"
                && value["observed_generation"].as_i64().unwrap()
                    > previous["observed_generation"].as_i64().unwrap_or(-1)
            {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .expect("owned Health worker must publish a fresh path observation")
}
#[tokio::test]
async fn both_domains_report_source_loss_and_recover_only_after_actual_import() {
    let path = std::env::temp_dir().join(format!("hrrdarr-path-health-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&path).unwrap();
    let _scratch = Scratch(path.clone());
    for name in ["tv", "movies", "incoming"] {
        std::fs::create_dir(path.join(name)).unwrap();
    }
    let db = Arc::new(Database::open_local(path.join("db")).await.unwrap());
    let c = db.connect().await.unwrap();
    c.execute(
        "INSERT INTO series(id,title,path) VALUES(1,'TV',?)",
        [path.join("tv").to_str().unwrap()],
    )
    .await
    .unwrap();
    c.execute_batch("INSERT INTO seasons VALUES(1,1,1); INSERT INTO episodes(id,series_id,season,number,title) VALUES(1,1,1,1,'Episode'); INSERT INTO movie_metadata(id,title) VALUES(1,'Movie');").await.unwrap();
    c.execute(
        "INSERT INTO movies(id,metadata_id,path) VALUES(1,1,?)",
        [path.join("movies").to_str().unwrap()],
    )
    .await
    .unwrap();
    let (routes, refresh) = providers::router_with_refresh(db.clone(), None);
    let app = routes
        .merge(health::router(db.clone()))
        .merge(import::router(db.clone()))
        .merge(completed_download_handling::router(db.clone()));
    let (base, _server) = serve(app).await;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let runtime = commands::start(db.clone(), refresh).await.unwrap();
    for (domain, target, root) in [("tv", "episode", "tv"), ("movies", "movie", "movies")] {
        let settings = call(
            &client,
            &base,
            "GET",
            &format!("/api/v1/{domain}/completed-download-handling"),
            None,
            200,
        )
        .await;
        call(
            &client,
            &base,
            "PUT",
            &format!("/api/v1/{domain}/completed-download-handling"),
            Some(json!({"enabled":true,"revision":settings["revision"]})),
            200,
        )
        .await;
        let source_directory = path.join("incoming").join(domain);
        std::fs::create_dir(&source_directory).unwrap();
        let source = source_directory.join("file.mkv");
        let destination = path.join(root).join("file.mkv");
        std::fs::write(&source, b"original diagnostic fixture bytes").unwrap();
        let preview=call(&client,&base,"POST","/api/v1/imports",Some(json!({"target":{"media_type":target,"id":1},"source":source,"destination":destination,"mode":"copy"})),202).await;
        let operation = preview["id"].as_str().unwrap();
        // Renaming the parent preserves source inode/content/ctime and parent identity.
        let hidden = source_directory.with_extension("retained");
        std::fs::rename(&source_directory, &hidden).unwrap();
        call(
            &client,
            &base,
            "POST",
            &format!("/api/v1/imports/{operation}/execute"),
            None,
            409,
        )
        .await;
        let issue = recheck(&client, &base, domain).await;
        assert_eq!(issue["issue"]["reason"], "path_source_removed", "{issue}");
        assert!(!issue.to_string().contains(path.to_str().unwrap()));
        assert!(!destination.exists());
        std::fs::rename(&hidden, &source_directory).unwrap();
        // Restored accessibility alone cannot clear the recorded failed import.
        assert_eq!(
            recheck(&client, &base, domain).await["issue"]["reason"],
            "path_source_removed"
        );
        let done = call(
            &client,
            &base,
            "POST",
            &format!("/api/v1/imports/{operation}/execute"),
            None,
            200,
        )
        .await;
        assert_eq!(done["status"], "complete");
        assert!(done["path_failure_code"].is_null());
        assert_eq!(
            std::fs::read(&destination).unwrap(),
            b"original diagnostic fixture bytes"
        );
        assert!(recheck(&client, &base, domain).await["issue"].is_null());
    }
    runtime.shutdown().await;
}
#[tokio::test]
async fn client_outputs_mapping_and_cdh_are_observed_without_private_paths() {
    let path = std::env::temp_dir().join(format!("hrrdarr-path-output-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&path).unwrap();
    let _scratch = Scratch(path.clone());
    let output = path.join("PRIVATE_DOWNLOAD_OUTPUT");
    std::fs::create_dir(&output).unwrap();
    let state = Arc::new(Mutex::new(json!(output)));
    let (endpoint, _peer) = serve(Router::new().fallback(peer).with_state(state.clone())).await;
    let db = Arc::new(Database::open_local(path.join("db")).await.unwrap());
    let (routes, refresh) = providers::router_with_refresh(db.clone(), None);
    let (base, _server) = serve(
        routes
            .merge(health::router(db.clone()))
            .merge(remote_paths::router(db.clone()))
            .merge(completed_download_handling::router(db.clone())),
    )
    .await;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let scope = |category| json!({"category":category,"imported_category":null,"recent_priority":0,"older_priority":0});
    call(&client,&base,"POST","/api/v1/providers",Some(json!({"name":"Client","enabled":true,"priority":1,"settings":{"implementation":"qbittorrent","endpoint":endpoint,"tv":scope("tv"),"movies":scope("movies")}})),201).await;
    let runtime = commands::start(db, refresh).await.unwrap();
    for domain in ["tv", "movies"] {
        let settings = call(
            &client,
            &base,
            "GET",
            &format!("/api/v1/{domain}/completed-download-handling"),
            None,
            200,
        )
        .await;
        call(
            &client,
            &base,
            "PUT",
            &format!("/api/v1/{domain}/completed-download-handling"),
            Some(json!({"enabled":true,"revision":settings["revision"]})),
            200,
        )
        .await;
        assert!(recheck(&client, &base, domain).await["issue"].is_null());
    }
    *state.lock().unwrap() = json!(output.join("missing"));
    for domain in ["tv", "movies"] {
        let result = recheck(&client, &base, domain).await;
        assert!(result["issue"].is_object(), "{result}");
        assert_ne!(result["issue"]["reason"], "path_output_permission_denied");
        assert!(!result.to_string().contains("PRIVATE_DOWNLOAD_OUTPUT"));
    }
    *state.lock().unwrap() = json!("Z:\\remote-downloads");
    for domain in ["tv", "movies"] {
        let result = recheck(&client, &base, domain).await;
        assert!(result["issue"].is_object());
        call(&client,&base,"POST",&format!("/api/v1/{domain}/remote-path-mappings"),Some(json!({"host":"127.0.0.1","remote_path":"Z:\\remote-downloads","local_path":output})),201).await;
        assert!(recheck(&client, &base, domain).await["issue"].is_null());
    }
    *state.lock().unwrap() = Value::Null;
    for domain in ["tv", "movies"] {
        assert_eq!(
            recheck(&client, &base, domain).await["issue"]["reason"],
            "path_output_missing"
        );
        let settings = call(
            &client,
            &base,
            "GET",
            &format!("/api/v1/{domain}/completed-download-handling"),
            None,
            200,
        )
        .await;
        call(
            &client,
            &base,
            "PUT",
            &format!("/api/v1/{domain}/completed-download-handling"),
            Some(json!({"enabled":false,"revision":settings["revision"]})),
            200,
        )
        .await;
        assert!(recheck(&client, &base, domain).await["issue"].is_null());
    }
    runtime.shutdown().await;
}
