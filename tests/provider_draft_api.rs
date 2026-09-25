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
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
const PRIVATE: &str = "DRAFT_PRIVATE_CREDENTIAL";
const REPLACEMENT: &str = "DRAFT_REPLACEMENT_SECRET";
struct Remote {
    calls: Mutex<Vec<String>>,
    blocked: AtomicBool,
    failure: AtomicBool,
    active: AtomicUsize,
    peak: AtomicUsize,
    started: tokio::sync::Notify,
    release: tokio::sync::Semaphore,
}
impl Default for Remote {
    fn default() -> Self {
        Self {
            calls: Mutex::new(vec![]),
            blocked: AtomicBool::new(false),
            failure: AtomicBool::new(false),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            started: tokio::sync::Notify::new(),
            release: tokio::sync::Semaphore::new(0),
        }
    }
}
async fn remote(
    State(s): State<Arc<Remote>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    s.calls.lock().unwrap().push(uri.to_string());
    let expected = if uri.path().contains("cleared") {
        None
    } else if uri.path().contains("replacement") {
        Some(REPLACEMENT)
    } else {
        Some(PRIVATE)
    };
    if uri.path().contains("indexer") {
        let args: std::collections::HashMap<_, _> =
            url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
                .into_owned()
                .collect();
        assert_eq!(args.get("apikey").map(String::as_str), expected);
        if s.failure.load(Ordering::SeqCst) && args.get("t").is_some_and(|v| v != "caps") {
            return (StatusCode::UNAUTHORIZED, PRIVATE).into_response();
        }

        return ([("content-type","application/xml")],if args.get("t").is_some_and(|v|v=="caps") {r#"<caps><searching><search available="yes" supportedParams="q"/><tv-search available="yes" supportedParams="q,season,ep"/><movie-search available="yes" supportedParams="q,imdbid"/></searching><categories><category id="5000"/><category id="2000"/></categories></caps>"#}else{r#"<rss><channel/></rss>"#}).into_response();
    }
    assert_eq!(
        headers
            .get("authorization")
            .map(|v| v.to_str().unwrap().to_owned()),
        expected.map(|v| format!("Bearer {v}"))
    );
    if uri.path().ends_with("webapiVersion") && s.blocked.load(Ordering::SeqCst) {
        let active = s.active.fetch_add(1, Ordering::SeqCst) + 1;
        s.peak.fetch_max(active, Ordering::SeqCst);
        s.started.notify_one();
        let permit = s.release.acquire().await.unwrap();
        permit.forget();
        s.active.fetch_sub(1, Ordering::SeqCst);
    }
    if uri.path().contains("failure") || s.failure.load(Ordering::SeqCst) {
        return (StatusCode::UNAUTHORIZED, PRIVATE).into_response();
    }
    if uri.path().ends_with("webapiVersion") {
        return "2.8.3".into_response();
    }
    if uri.path().ends_with("app/version") {
        return "v4.6.0".into_response();
    }
    if uri.path().ends_with("preferences") {
        return axum::Json(json!({"queueing_enabled":true,"save_path":"/remote","max_ratio_enabled":false,"max_ratio":-1,"max_seeding_time_enabled":false,"max_seeding_time":-1,"max_ratio_act":0})).into_response();
    }
    if uri.path().ends_with("categories") {
        return axum::Json(
            json!({"tv":{"savePath":"/remote/tv"},"movies":{"savePath":"/remote/movies"}}),
        )
        .into_response();
    }
    if uri.path().ends_with("torrents/info") {
        return axum::Json(json!([])).into_response();
    }
    StatusCode::NOT_FOUND.into_response()
}
fn config(name: &str, endpoint: &str, implementation: &str) -> Value {
    let settings = if implementation == "qbittorrent" {
        let scope = |category| json!({"category":category,"imported_category":null,"recent_priority":0,"older_priority":1});
        json!({"implementation":implementation,"endpoint":endpoint,"tv":scope("tv"),"movies":scope("movies")})
    } else {
        json!({"implementation":implementation,"endpoint":endpoint,"tv":{"categories":[5000],"anime_categories":[]},"movies":{"categories":[2000]}})
    };
    json!({"name":name,"enabled":true,"priority":1,"settings":settings,"credentials":{"kind":"api_key","api_key":PRIVATE}})
}
async fn create(address: SocketAddr, input: &Value) -> Value {
    let (code, body) = request(address, "POST", "/api/v1/providers", &input.to_string()).await;
    assert_eq!(code, 201, "{body}");
    body
}
async fn snapshot(address: SocketAddr, conn: &libsql::Connection) -> Value {
    let (status, page) = request(address, "GET", "/api/v1/providers", "").await;
    assert_eq!(status, 200);
    let mut rows = conn
        .query("SELECT id,hex(credentials) FROM providers ORDER BY id", ())
        .await
        .unwrap();
    let mut ciphertext = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        ciphertext.push((row.get::<String>(0).unwrap(), row.get::<String>(1).unwrap()));
    }
    json!({"page":page,"ciphertext":ciphertext})
}
fn draft(config: Value, source: Option<&Value>) -> Value {
    json!({"source":source.map(|p|json!({"id":p["id"],"revision":p["revision"]})),"config":config})
}
#[tokio::test]
async fn draft_tests_preserve_state_bind_credentials_and_cover_all_providers() -> Result<(), Error>
{
    let scratch = Sandbox::new();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await?);
    let conn = db.connect().await?;
    let key = Arc::new(providers::CredentialKey::from_hex(&"12".repeat(32)).unwrap());
    let (address, server) = serve(db.clone(), Some(key)).await;
    let (keyless, keyless_server) = serve(db.clone(), None).await;
    let state = Arc::new(Remote::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let app = axum::Router::new()
        .fallback(remote)
        .with_state(state.clone());
    let upstream = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let other = Arc::new(Remote::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let destination = format!("http://{}", listener.local_addr().unwrap());
    let app = axum::Router::new()
        .fallback(remote)
        .with_state(other.clone());
    let other_server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let before = snapshot(address, &conn).await;
    for implementation in ["torznab", "newznab", "qbittorrent"] {
        let path = if implementation == "qbittorrent" {
            "client"
        } else {
            "indexer"
        };
        for media in ["tv", "movies", "both"] {
            let mut input = config("unsaved", &format!("{endpoint}/{path}"), implementation);
            input["enabled"] = json!(false);
            if media != "both" {
                input["settings"][if media == "tv" { "movies" } else { "tv" }] = Value::Null;
            }
            let (code, result) = request(
                keyless,
                "POST",
                "/api/v1/providers/test-draft",
                &draft(input, None).to_string(),
            )
            .await;
            assert_eq!(code, 200, "{result}");
            assert!(result["source"].is_null());
            assert!(result.get("provider_id").is_none());
            assert!(result.get("revision").is_none());
            assert_eq!(
                result["result"]["domains"],
                if media == "both" {
                    json!(["tv", "movies"])
                } else {
                    json!([media])
                }
            );
            assert!(!result.to_string().contains(PRIVATE));
        }
    }
    assert_eq!(
        snapshot(address, &conn).await,
        before,
        "New draft testing creates no providers or observations"
    );
    let mut saved = Vec::new();
    for implementation in ["torznab", "newznab", "qbittorrent"] {
        let path = if implementation == "qbittorrent" {
            "client"
        } else {
            "indexer"
        };
        let mut input = config(
            implementation,
            &format!("{endpoint}/{path}"),
            implementation,
        );
        if implementation != "qbittorrent" {
            input["credentials"] = json!({"kind":"indexer","api_key":PRIVATE,"tv_parameters":[{"name":"private_tv","value":"TV_DRAFT_PARAM"}],"movie_parameters":[{"name":"private_movie","value":"MOVIE_DRAFT_PARAM"}]});
        }
        let provider = create(address, &input).await;
        assert_eq!(
            request(
                address,
                "POST",
                &format!(
                    "/api/v1/providers/{}/test",
                    provider["id"].as_str().unwrap()
                ),
                ""
            )
            .await
            .0,
            200
        );
        tokio::time::sleep(Duration::from_millis(110)).await;
        let baseline = snapshot(address, &conn).await;
        let mut input = input;
        input.as_object_mut().unwrap().remove("credentials");
        let probe_start = state.calls.lock().unwrap().len();
        let (code, result) = request(
            address,
            "POST",
            "/api/v1/providers/test-draft",
            &draft(input.clone(), Some(&provider)).to_string(),
        )
        .await;
        assert_eq!(code, 200, "{result}");
        assert_eq!(result["source"]["id"], provider["id"]);
        if implementation != "qbittorrent" {
            let calls = state.calls.lock().unwrap();
            let calls = &calls[probe_start..];
            assert!(
                calls
                    .iter()
                    .any(|s| s.contains("private_tv=TV_DRAFT_PARAM"))
            );
            assert!(
                calls
                    .iter()
                    .any(|s| s.contains("private_movie=MOVIE_DRAFT_PARAM"))
            );
            assert!(!result.to_string().contains("DRAFT_PARAM"));
        }

        tokio::time::sleep(Duration::from_millis(110)).await;
        state.failure.store(true, Ordering::SeqCst);
        let (code, error) = request(
            address,
            "POST",
            "/api/v1/providers/test-draft",
            &draft(input.clone(), Some(&provider)).to_string(),
        )
        .await;
        assert_eq!(code, 502);
        assert_eq!(error["error"]["code"], "authentication");
        assert!(!error.to_string().contains(PRIVATE));
        assert!(!error.to_string().contains("DRAFT_PARAM"));
        state.failure.store(false, Ordering::SeqCst);
        assert_eq!(
            snapshot(address, &conn).await,
            baseline,
            "Failed probes also leave saved observations and encrypted bundles intact"
        );
        for locked_key in [
            None,
            Some(Arc::new(
                providers::CredentialKey::from_hex(&"34".repeat(32)).unwrap(),
            )),
        ] {
            let (locked, task) = serve(db.clone(), locked_key).await;
            let count = state.calls.lock().unwrap().len();
            assert_eq!(
                request(
                    locked,
                    "POST",
                    "/api/v1/providers/test-draft",
                    &draft(input.clone(), Some(&provider)).to_string()
                )
                .await
                .0,
                503
            );
            assert_eq!(state.calls.lock().unwrap().len(), count);
            for clear in [false, true] {
                let mut changed = input.clone();
                changed["settings"]["endpoint"] = json!(format!(
                    "{destination}/{}/{path}",
                    if clear { "cleared" } else { "replacement" }
                ));
                changed["credentials"] = if clear {
                    Value::Null
                } else {
                    json!({"kind":"api_key","api_key":REPLACEMENT})
                };
                let (code, result) = request(
                    locked,
                    "POST",
                    "/api/v1/providers/test-draft",
                    &draft(changed, Some(&provider)).to_string(),
                )
                .await;
                assert_eq!(code, 200, "{result}");
                assert!(!result.to_string().contains(REPLACEMENT));
                tokio::time::sleep(Duration::from_millis(110)).await;
            }
            task.abort();
            let _ = task.await;
        }
        for changed_endpoint in [
            format!("{endpoint}/another/{path}"),
            format!("{destination}/{path}"),
        ] {
            let mut changed = input.clone();
            changed["settings"]["endpoint"] = json!(changed_endpoint);
            let a = state.calls.lock().unwrap().len();
            let b = other.calls.lock().unwrap().len();
            let (code, result) = request(
                address,
                "POST",
                "/api/v1/providers/test-draft",
                &draft(changed, Some(&provider)).to_string(),
            )
            .await;
            assert_eq!(code, 422);
            assert_eq!(result["error"]["code"], "provider_credential_binding");
            assert_eq!(state.calls.lock().unwrap().len(), a);
            assert_eq!(other.calls.lock().unwrap().len(), b);
        }
        assert_eq!(
            snapshot(address, &conn).await,
            baseline,
            "Preserve/replace/clear drafts never rewrite cipher/config/revision/status"
        );
        saved.push((provider, input));
    }
    let before_calls = state.calls.lock().unwrap().len();
    let (provider, input) = &saved[0];
    assert_eq!(
        request(
            address,
            "POST",
            "/api/v1/providers/test-draft?forceTest=true",
            &draft(input.clone(), Some(provider)).to_string()
        )
        .await
        .0,
        400
    );

    for fault in [
        "name",
        "endpoint",
        "source",
        "revision",
        "implementation",
        "unknown",
    ] {
        let mut bad = draft(input.clone(), Some(provider));
        match fault {
            "name" => bad["config"]["name"] = json!(""),
            "endpoint" => {
                bad["config"]["settings"]["endpoint"] =
                    json!(format!("{endpoint}?apikey={PRIVATE}"))
            }
            "source" => bad["source"]["id"] = json!(uuid::Uuid::new_v4()),
            "revision" => bad["source"]["revision"] = json!(9),
            "implementation" => bad["config"]["settings"]["implementation"] = json!("newznab"),
            _ => bad["unknown"] = json!(PRIVATE),
        };
        let (code, error) = request(
            address,
            "POST",
            "/api/v1/providers/test-draft",
            &bad.to_string(),
        )
        .await;
        assert!(matches!(code, 400 | 404 | 409));
        assert!(!error.to_string().contains(PRIVATE));
    }
    assert_eq!(state.calls.lock().unwrap().len(), before_calls);
    // The same saved-provider lane cannot be bypassed by a concurrent draft. Concurrent edits invalidate failures too.
    let (provider, input) = &saved[2];
    let bound = draft(input.clone(), Some(provider));
    state.blocked.store(true, Ordering::SeqCst);
    let pending = {
        let bound = bound.clone();
        tokio::spawn(async move {
            request(
                address,
                "POST",
                "/api/v1/providers/test-draft",
                &bound.to_string(),
            )
            .await
        })
    };
    tokio::time::timeout(Duration::from_secs(3), state.started.notified())
        .await
        .unwrap();
    assert_eq!(
        request(
            address,
            "POST",
            "/api/v1/providers/test-draft",
            &bound.to_string()
        )
        .await
        .0,
        429
    );
    let mut edit = input.clone();
    edit["revision"] = provider["revision"].clone();
    edit["name"] = json!("changed while testing");
    assert_eq!(
        request(
            address,
            "PUT",
            &format!("/api/v1/providers/{}", provider["id"].as_str().unwrap()),
            &edit.to_string()
        )
        .await
        .0,
        200
    );
    state.release.add_permits(1);
    let (code, _) = tokio::time::timeout(Duration::from_secs(5), pending)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(code, 409);
    state.blocked.store(false, Ordering::SeqCst);
    let (_, after) = request(
        address,
        "GET",
        &format!("/api/v1/providers/{}", provider["id"].as_str().unwrap()),
        "",
    )
    .await;
    assert_eq!(after["revision"], 2);
    assert_eq!(after["test_status"], "never_tested");
    // A protocol authentication failure cannot hide deletion of its revision-bound source while in flight.
    tokio::time::sleep(Duration::from_millis(110)).await;
    state.blocked.store(true, Ordering::SeqCst);
    state.failure.store(true, Ordering::SeqCst);
    let mut failed_bound = bound.clone();
    failed_bound["source"]["revision"] = json!(2);
    let failed = tokio::spawn(async move {
        request(
            address,
            "POST",
            "/api/v1/providers/test-draft",
            &failed_bound.to_string(),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(3), state.started.notified())
        .await
        .unwrap();
    // Delete while the remote failure is blocked; the post-probe source check must take precedence.
    assert_eq!(
        request(
            address,
            "DELETE",
            &format!(
                "/api/v1/providers/{}?revision=2",
                provider["id"].as_str().unwrap()
            ),
            ""
        )
        .await
        .0,
        204
    );
    state.release.add_permits(1);
    let (code, error) = tokio::time::timeout(Duration::from_secs(5), failed)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(code, 409);
    assert_eq!(error["error"]["code"], "provider_revision_conflict");
    assert!(!error.to_string().contains(PRIVATE));
    state.blocked.store(false, Ordering::SeqCst);
    state.failure.store(false, Ordering::SeqCst);
    // A subsequent call with this deleted source is rejected before outbound requests.
    let before_calls = state.calls.lock().unwrap().len();
    assert_eq!(
        request(
            address,
            "POST",
            "/api/v1/providers/test-draft",
            &bound.to_string()
        )
        .await
        .0,
        404
    );
    assert_eq!(state.calls.lock().unwrap().len(), before_calls);
    server.abort();
    keyless_server.abort();
    upstream.abort();
    other_server.abort();
    let _ = server.await;
    let _ = keyless_server.await;
    let _ = upstream.await;
    let _ = other_server.await;
    Ok(())
}
