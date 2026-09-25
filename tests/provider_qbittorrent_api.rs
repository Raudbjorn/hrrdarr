use hrrdarr::{
    db::{Database, Error},
    providers,
};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("hrrdarr-provider-api-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
async fn request(addr: SocketAddr, method: &str, path: &str, body: &str) -> (u16, Value) {
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    tokio::task::spawn_blocking(move || {
        let mut stream =
            std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(5)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut bytes = Vec::new();
        stream
            .take(9 * 1024 * 1024)
            .read_to_end(&mut bytes)
            .unwrap();
        let response = String::from_utf8(bytes).unwrap();
        let (headers, body) = response.split_once("\r\n\r\n").unwrap();
        let status: u16 = headers.split_whitespace().nth(1).unwrap().parse().unwrap();
        (
            status,
            if status == 204 {
                assert!(body.is_empty());
                Value::Null
            } else {
                serde_json::from_str(body).expect("provider response must be JSON")
            },
        )
    })
    .await
    .unwrap()
}
async fn serve(
    db: Arc<Database>,
    key: Option<Arc<providers::CredentialKey>>,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = providers::router(db, key);
    (
        address,
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        }),
    )
}
use axum::{
    extract::{OriginalUri, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use std::sync::atomic::{AtomicU8, Ordering};
const SECRET: &str = "QBIT_API_PRIVATE_SENTINEL";
#[derive(Default)]
struct Fixture {
    mode: AtomicU8,
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
async fn upstream(
    State(state): State<Arc<Fixture>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    assert_eq!(headers["authorization"], format!("Bearer {SECRET}"));
    if state.mode.load(Ordering::SeqCst) == 1 {
        return (StatusCode::UNAUTHORIZED, SECRET).into_response();
    }
    let args: std::collections::HashMap<String, String> =
        url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();
    match uri.path() {
        "/api/v2/app/webapiVersion" => {
            if state.mode.load(Ordering::SeqCst) == 2 { state.started.notify_one(); state.release.notified().await; }
            "2.8.3".into_response()
        },
        "/api/v2/app/version" => "v4.6.0".into_response(),
        // Explicit disabled retention settings keep this authentication/scope fixture safe under connection validation.
        "/api/v2/app/preferences" => {
            let unsafe_retention = state.mode.load(Ordering::SeqCst) == 3;
            axum::Json(json!({"queueing_enabled":true,"max_ratio_enabled":unsafe_retention,"max_ratio":if unsafe_retention {1} else {-1},"max_seeding_time_enabled":false,"max_seeding_time":-1,"max_ratio_act":if unsafe_retention {1} else {0},"dht":false,"private_unused_value":SECRET})).into_response()
        },
        "/api/v2/torrents/categories" => axum::Json(json!({"tv":{"savePath":"/private/tv"},"movies":{"savePath":"/private/movies"}})).into_response(),
        "/api/v2/torrents/info" => {
            let category = args.get("category").map(String::as_str).unwrap_or("tv");
            if state.mode.load(Ordering::SeqCst) == 4 {
                let rows: Vec<Value> = ["error", "missingFiles", "stalledDL", "metaDL", "raw_private_state"].iter().enumerate().map(|(i, remote)| json!({"hash":format!("{:040x}", i + 1),"category":category,"name":format!("Title {SECRET}"),"state":remote,"progress":0.5,"size":10,"amount_left":5,"dlspeed":0,"upspeed":0,"ratio":0,"eta":8640000,"message":SECRET,"error":SECRET})).collect();
                return axum::Json(rows).into_response();
            }
            axum::Json(json!([{"hash":"a".repeat(40),"category":category,"name":format!("Title {SECRET}"),"state":"uploading","progress":1.0,"size":10,"amount_left":0,"dlspeed":0,"upspeed":1,"ratio":1.0}])).into_response()
        },
        "/api/v2/torrents/files" => axum::Json(json!([{"index":0,"name":format!("Folder/{SECRET}.mkv"),"size":10,"progress":1.0,"priority":1}])).into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}
#[tokio::test]
async fn shared_client_api_scopes_defaults_tests_reads_and_stale_results() -> Result<(), Error> {
    let sandbox = Sandbox::new();
    let path = sandbox.0.join("db");
    let db = Arc::new(Database::open_local(&path).await?);
    let key = Arc::new(providers::CredentialKey::from_hex(&"12".repeat(32)).unwrap());
    let state = Arc::new(Fixture::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let app = axum::Router::new()
        .fallback(upstream)
        .with_state(state.clone());
    let upstream_server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (address, server) = serve(db.clone(), Some(key.clone())).await;
    let scope = |category| json!({"category":category,"imported_category":null,"recent_priority":0,"older_priority":1});
    let mut input = json!({"name":"Shared client","enabled":true,"priority":1,"settings":{"implementation":"qbittorrent","endpoint":endpoint,"tv":scope("tv"),"movies":scope("movies")},"credentials":{"kind":"api_key","api_key":SECRET}});
    input["settings"]["tv"]["initial_state"] = json!("forced");
    input["settings"]["tv"]["content_layout"] = json!("subfolder");
    input["settings"]["tv"]["sequential_order"] = json!(true);
    input["settings"]["tv"]["first_last_first"] = json!(true);
    input["settings"]["tv"]["add_tags"] = json!(true);
    let (status, created) = request(address, "POST", "/api/v1/providers", &input.to_string()).await;
    assert_eq!(status, 201, "{created}");
    assert_eq!(created["test_supported"], true);
    assert_eq!(created["settings"]["movies"]["initial_state"], "started");
    assert_eq!(created["settings"]["movies"]["add_tags"], false);
    assert_eq!(created["settings"]["tv"]["initial_state"], "forced");
    let route = format!("/api/v1/providers/{}", created["id"].as_str().unwrap());
    let (status, result) = request(address, "POST", &format!("{route}/test"), "").await;
    assert_eq!(status, 200, "{result}");
    assert_eq!(result["result"]["domains"], json!(["tv", "movies"]));
    assert!(result["result"].get("capabilities").is_none());
    for domain in ["tv", "movies"] {
        let (status, page) = request(
            address,
            "POST",
            &format!("{route}/downloads"),
            &json!({"domain":domain,"offset":0,"limit":10,"imported":false}).to_string(),
        )
        .await;
        assert_eq!(status, 200, "{page}");
        assert_eq!(page["page"]["items"][0]["domain"], domain);
        assert!(!page.to_string().contains(SECRET));
        assert!(!page.to_string().contains("/private"));
    }
    // Native bounded diagnostics replace remote states/messages, including DHT metadata warnings.
    state.mode.store(4, Ordering::SeqCst);
    for domain in ["tv", "movies"] {
        let (status, page) = request(
            address,
            "POST",
            &format!("{route}/downloads"),
            &json!({"domain":domain,"offset":0,"limit":10,"imported":false}).to_string(),
        )
        .await;
        assert_eq!(status, 200, "{page}");
        let items = page["page"]["items"].as_array().unwrap();
        for (item, diagnostic) in items.iter().zip([
            "error",
            "missing_files",
            "stalled",
            "dht_disabled",
            "unknown_state",
        ]) {
            assert_eq!(item["domain"], domain);
            assert_eq!(item["diagnostic"], diagnostic);
            assert_eq!(
                item["status"],
                if diagnostic == "unknown_state" {
                    "unknown"
                } else {
                    "warning"
                }
            );
            assert!(item["eta_seconds"].is_null());
        }
        assert_eq!(items.len(), 5);
        assert!(!page.to_string().contains(SECRET));
        assert!(!page.to_string().contains("raw_private_state"));
    }
    state.mode.store(0, Ordering::SeqCst);
    let files_input = json!({"domain":"tv","hash":"a".repeat(40)}).to_string();
    let (status, files) = request(address, "POST", &format!("{route}/files"), &files_input).await;
    assert_eq!(status, 200, "{files}");
    assert!(!files.to_string().contains(SECRET));
    let (status, _) = request(
        address,
        "POST",
        &format!("{route}/files"),
        &json!({"domain":"movies","hash":"a".repeat(40)}).to_string(),
    )
    .await;
    assert_eq!(
        status, 400,
        "A hash found in TV must not become movie-owned"
    );
    let reopened = db.clone();
    let (other, other_server) = serve(reopened, Some(key.clone())).await;
    let (_, persisted) = request(other, "GET", &route, "").await;
    assert_eq!(persisted["test_status"], "success");
    assert_eq!(persisted["settings"], created["settings"]);
    // A valid but unsafe retention configuration is a configuration error, not malformed protocol data.
    state.mode.store(3, Ordering::SeqCst);
    let (status, error) = request(address, "POST", &format!("{route}/test"), "").await;
    assert_eq!(status, 422);
    assert_eq!(error["error"]["code"], "unsafe_retention");
    assert_eq!(
        error["error"]["message"],
        "Client is configured to remove downloads before completed download handling can retain them"
    );
    assert!(!error.to_string().contains(SECRET));
    let (_, unsafe_status) = request(other, "GET", &route, "").await;
    assert_eq!(unsafe_status["test_status"], "failure");
    assert_eq!(
        unsafe_status["last_test"]["error_code"], "unsupported",
        "Persist the existing static configuration-failure category without storing remote preference contents"
    );
    assert_eq!(unsafe_status["last_test"]["revision"], 1);
    assert!(!unsafe_status.to_string().contains(SECRET));
    state.mode.store(1, Ordering::SeqCst);
    let (status, error) = request(address, "POST", &format!("{route}/test"), "").await;
    assert_eq!(status, 502);
    assert!(!error.to_string().contains(SECRET));
    let (_, failed) = request(other, "GET", &route, "").await;
    assert_eq!(failed["last_test"]["error_code"], "authentication");
    state.mode.store(2, Ordering::SeqCst);
    let test_route = format!("{route}/test");
    let pending = tokio::spawn(async move { request(address, "POST", &test_route, "").await });
    tokio::time::timeout(Duration::from_secs(5), state.started.notified())
        .await
        .unwrap();
    input.as_object_mut().unwrap().remove("credentials");
    input["revision"] = json!(1);
    input["name"] = json!("Changed");
    let (status, updated) = request(address, "PUT", &route, &input.to_string()).await;
    assert_eq!(status, 200, "{updated}");
    state.mode.store(0, Ordering::SeqCst);
    state.release.notify_one();
    let (status, _) = tokio::time::timeout(Duration::from_secs(5), pending)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, 409);
    let (_, current) = request(other, "GET", &route, "").await;
    assert_eq!(current["test_status"], "never_tested");
    let mut invalid = input.clone();
    invalid.as_object_mut().unwrap().remove("revision");
    invalid["settings"]["movies"]["category"] = json!("tv/child");
    assert_eq!(
        request(address, "POST", "/api/v1/providers", &invalid.to_string())
            .await
            .0,
        400
    );
    invalid["settings"]["movies"]["category"] = json!("movies");
    invalid["settings"]["movies"]["add_tags"] = json!(true);
    assert_eq!(
        request(address, "POST", "/api/v1/providers", &invalid.to_string())
            .await
            .0,
        400
    );
    let (status, result) = request(address, "POST", &format!("{route}/test"), "").await;
    assert_eq!(status, 200, "{result}");
    for task in [server, other_server, upstream_server] {
        task.abort();
        let _ = task.await;
    }
    drop(db);
    let reopened = Arc::new(Database::open_local(&path).await?);
    let (address, server) = serve(reopened, Some(key)).await;
    let (_, persisted) = request(address, "GET", &route, "").await;
    assert_eq!(persisted["test_status"], "success");
    assert_eq!(persisted["settings"]["tv"]["initial_state"], "forced");
    server.abort();
    let _ = server.await;
    Ok(())
}

const SESSION_USER: &str = "AUTH_PRIVATE_USER";
const SESSION_PASSWORD: &str = "AUTH_PRIVATE_PASSWORD&=+";
const OLD_SID: &str = "AUTH_PRIVATE_OLD_SID";
const NEW_SID: &str = "AUTH_PRIVATE_NEW_SID";
#[derive(Default)]
struct AuthFixture {
    // 0: explicit bypass; 1: expired SID recovers; 2: reauthentication denied; 3: retried read denied.
    mode: AtomicU8,
    logins: AtomicU8,
    reads: AtomicU8,
    paths: std::sync::Mutex<Vec<String>>,
}
async fn auth_upstream(
    State(state): State<Arc<AuthFixture>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    state.paths.lock().unwrap().push(uri.path().to_owned());
    assert!(!headers.contains_key("authorization"));
    assert!(!uri.to_string().contains(SESSION_USER));
    assert!(!uri.to_string().contains(SESSION_PASSWORD));
    let mode = state.mode.load(Ordering::SeqCst);
    let cookie = headers.get("cookie").and_then(|value| value.to_str().ok());
    if mode == 0 {
        assert!(
            cookie.is_none(),
            "Explicit bypass must not reuse a previous operation's SID"
        );
    }
    match uri.path() {
        "/api/v2/auth/login" => {
            assert_ne!(mode, 0, "Bypass must never attempt login");
            assert!(cookie.is_none(), "Expired SID must be removed before login");
            let form: std::collections::HashMap<String, String> =
                url::form_urlencoded::parse(&body).into_owned().collect();
            assert!(form.get("username").is_some_and(|v| v == SESSION_USER));
            assert!(form.get("password").is_some_and(|v| v == SESSION_PASSWORD));
            let attempt = state.logins.fetch_add(1, Ordering::SeqCst);
            assert!(attempt < 2, "No repeated reauthentication loop");
            if mode == 2 && attempt == 1 {
                return (
                    StatusCode::OK,
                    format!("Rejected {SESSION_PASSWORD} {OLD_SID}"),
                )
                    .into_response();
            }
            let sid = if attempt == 0 { OLD_SID } else { NEW_SID };
            ([("set-cookie", format!("SID={sid}; HttpOnly"))], "Ok.").into_response()
        }
        "/api/v2/app/webapiVersion" => {
            if mode != 0 && cookie.is_none() {
                return (StatusCode::FORBIDDEN, SESSION_PASSWORD).into_response();
            }
            if mode != 0 {
                assert!(cookie == Some(format!("SID={OLD_SID}").as_str()));
            }
            "2.8.3".into_response()
        }
        "/api/v2/torrents/info" => {
            state.reads.fetch_add(1, Ordering::SeqCst);
            if mode != 0 && cookie == Some(format!("SID={OLD_SID}").as_str()) {
                return (
                    StatusCode::FORBIDDEN,
                    format!("expired {OLD_SID} {SESSION_PASSWORD}"),
                )
                    .into_response();
            }
            if mode != 0 {
                assert!(cookie == Some(format!("SID={NEW_SID}").as_str()));
            }
            if mode == 3 {
                return (
                    StatusCode::UNAUTHORIZED,
                    format!("denied {NEW_SID} {SESSION_PASSWORD}"),
                )
                    .into_response();
            }
            let args: std::collections::HashMap<String, String> =
                url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
                    .into_owned()
                    .collect();
            let category = args.get("category").unwrap();
            let name = if mode == 0 {
                "Bypass title".to_owned()
            } else {
                format!("Title {SESSION_USER} {SESSION_PASSWORD} {OLD_SID} {NEW_SID}")
            };
            axum::Json(json!([{"hash":"b".repeat(40),"category":category,"name":name,"state":"downloading","progress":0.5,"size":10,"amount_left":5,"dlspeed":1,"upspeed":0,"ratio":0.0}])).into_response()
        }
        _ => panic!("Unexpected protocol path: no legacy fallback or mutation is authorized"),
    }
}
#[tokio::test]
async fn bypass_and_expired_sid_reads_are_scoped_bounded_and_redacted() -> Result<(), Error> {
    let sandbox = Sandbox::new();
    let db = Arc::new(Database::open_local(sandbox.0.join("db")).await?);
    let key = Arc::new(providers::CredentialKey::from_hex(&"34".repeat(32)).unwrap());
    let (address, server) = serve(db, Some(key)).await;
    let fixture = Arc::new(AuthFixture::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let app = axum::Router::new()
        .fallback(auth_upstream)
        .with_state(fixture.clone());
    let upstream = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let scope = |category| json!({"category":category,"imported_category":null,"recent_priority":0,"older_priority":0});
    let base = json!({"name":"Auth fixture","enabled":true,"priority":1,"settings":{"implementation":"qbittorrent","endpoint":endpoint,"tv":scope("tv"),"movies":scope("movies")}});
    // Run bypass after SID scenarios too: per-operation cookies must never become ambient credentials.
    for mode in [0, 1, 2, 3, 0] {
        let mut input = base.clone();
        if mode != 0 {
            input["credentials"] = json!({"kind":"username_password","username":SESSION_USER,"password":SESSION_PASSWORD});
        }
        let (status, created) =
            request(address, "POST", "/api/v1/providers", &input.to_string()).await;
        assert_eq!(status, 201);
        assert_eq!(created["has_credentials"], mode != 0);
        let route = format!(
            "/api/v1/providers/{}/downloads",
            created["id"].as_str().unwrap()
        );
        for domain in ["tv", "movies"] {
            fixture.mode.store(mode, Ordering::SeqCst);
            fixture.logins.store(0, Ordering::SeqCst);
            fixture.reads.store(0, Ordering::SeqCst);
            fixture.paths.lock().unwrap().clear();
            let (status, response) = request(
                address,
                "POST",
                &route,
                &json!({"domain":domain,"offset":0,"limit":10,"imported":false}).to_string(),
            )
            .await;
            for secret in [SESSION_USER, SESSION_PASSWORD, OLD_SID, NEW_SID] {
                assert!(
                    !response.to_string().contains(secret),
                    "Public response contains a private authentication value"
                );
            }
            let expected = match mode {
                0 => vec!["/api/v2/app/webapiVersion", "/api/v2/torrents/info"],
                1 | 3 => vec![
                    "/api/v2/app/webapiVersion",
                    "/api/v2/auth/login",
                    "/api/v2/app/webapiVersion",
                    "/api/v2/torrents/info",
                    "/api/v2/auth/login",
                    "/api/v2/torrents/info",
                ],
                2 => vec![
                    "/api/v2/app/webapiVersion",
                    "/api/v2/auth/login",
                    "/api/v2/app/webapiVersion",
                    "/api/v2/torrents/info",
                    "/api/v2/auth/login",
                ],
                _ => unreachable!(),
            };
            assert_eq!(
                *fixture.paths.lock().unwrap(),
                expected,
                "Authentication failure must not retry repeatedly or probe legacy endpoints"
            );
            assert_eq!(
                fixture.logins.load(Ordering::SeqCst),
                if mode == 0 { 0 } else { 2 }
            );
            assert_eq!(
                fixture.reads.load(Ordering::SeqCst),
                if mode == 0 || mode == 2 { 1 } else { 2 }
            );
            if mode < 2 {
                assert_eq!(status, 200);
                assert_eq!(response["page"]["items"][0]["domain"], domain);
                assert_eq!(response["page"]["items"][0]["category"], domain);
            } else {
                assert_eq!(status, 502);
                assert_eq!(response["error"]["code"], "authentication");
                assert_eq!(
                    response["error"]["message"],
                    "Provider rejected authentication"
                );
            }
        }
    }
    for task in [server, upstream] {
        task.abort();
        let _ = task.await;
    }
    Ok(())
}
