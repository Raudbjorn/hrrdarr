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
    http::StatusCode,
    response::{IntoResponse, Response},
};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicU8, Ordering},
};
const KEY: &str = "CATEGORY_API_SECRET";
const TV: &str = "TV_PRIVATE_PARAMETER";
const MOVIE: &str = "MOVIE_PRIVATE_PARAMETER";
struct Remote {
    mode: AtomicU8,
    failed: AtomicBool,
    allow_rss: AtomicBool,
    reflect: bool,
    auth: Mutex<Option<String>>,
    calls: Mutex<Vec<String>>,
    started: tokio::sync::Notify,
    release: tokio::sync::Semaphore,
}
impl Remote {
    fn new(reflect: bool) -> Self {
        Self {
            mode: AtomicU8::new(0),
            failed: AtomicBool::new(false),
            allow_rss: AtomicBool::new(false),
            reflect,
            auth: Mutex::new(Some(KEY.into())),
            calls: Mutex::new(vec![]),
            started: tokio::sync::Notify::new(),
            release: tokio::sync::Semaphore::new(0),
        }
    }
}
async fn remote(State(s): State<Arc<Remote>>, OriginalUri(uri): OriginalUri) -> Response {
    s.calls.lock().unwrap().push(uri.to_string());
    let args: std::collections::HashMap<_, _> =
        url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();
    assert_eq!(args.get("apikey"), s.auth.lock().unwrap().as_ref());
    if s.allow_rss.load(Ordering::SeqCst) && args.get("t").is_some_and(|t| t != "caps") {
        return (
            [("content-type", "application/xml")],
            "<rss><channel/></rss>",
        )
            .into_response();
    }
    assert_eq!(args.get("t").unwrap(), "caps");
    assert!(
        args.keys()
            .all(|k| ["t", "o", "apikey"].contains(&k.as_str())),
        "Category discovery must never forward scoped private parameters"
    );
    let mode = s.mode.load(Ordering::SeqCst);
    if mode == 5 {
        s.started.notify_one();
        let permit = s.release.acquire().await.unwrap();
        permit.forget();
    }
    if mode == 2 || s.failed.load(Ordering::SeqCst) {
        return (StatusCode::UNAUTHORIZED, KEY).into_response();
    }
    if mode == 6 {
        return (StatusCode::TOO_MANY_REQUESTS, [("retry-after", "17")], KEY).into_response();
    }
    let labels = if s.reflect {
        format!("{KEY} {TV} {MOVIE}")
    } else {
        "Public".into()
    };
    let xml = match mode {
        1 => "<caps><categories/></caps>".into(),
        3 => format!("<caps>{KEY}"),
        _ => format!(
            r#"<caps><limits max="{}" default="10"/><searching><search available="yes" supportedParams="q"/><tv-search available="yes" supportedParams="q,season,ep"/><movie-search available="yes" supportedParams="q,imdbid"/></searching><categories><category id="1000" name="Ignored"><subcat id="1001" name="Ignored child"/></category><category id="2000" name="Movies {labels}"><subcat id="2040" name="HD"/></category><category id="5000" name="TV {labels}"><subcat id="5070" name="Anime"/><subcat id="5030" name="SD"/></category><category id="100001" name="Custom"><subcat id="100002" name="Custom child"/></category></categories></caps>"#,
            if mode == 4 { "invalid" } else { "100" }
        ),
    };
    ([("content-type", "application/xml")], xml).into_response()
}
fn input(implementation: &str, endpoint: &str, media: &str) -> Value {
    json!({"source":null,"media_type":media,"connection":{"implementation":implementation,"endpoint":endpoint,"credentials":{"kind":"indexer","api_key":KEY,"tv_parameters":[{"name":"private_tv","value":TV}],"movie_parameters":[{"name":"private_movie","value":MOVIE}]}}})
}
async fn discover(addr: SocketAddr, value: &Value) -> (u16, Value) {
    request(
        addr,
        "POST",
        "/api/v1/providers/indexer-categories",
        &value.to_string(),
    )
    .await
}
async fn snapshot(address: SocketAddr, conn: &libsql::Connection) -> Value {
    let (code, page) = request(address, "GET", "/api/v1/providers", "").await;
    assert_eq!(code, 200);
    let mut rows = conn
        .query("SELECT id,hex(credentials) FROM providers ORDER BY id", ())
        .await
        .unwrap();
    let mut cipher = vec![];
    while let Some(row) = rows.next().await.unwrap() {
        cipher.push((row.get::<String>(0).unwrap(), row.get::<String>(1).unwrap()));
    }
    json!({"page":page,"cipher":cipher})
}
fn ids(response: &Value) -> Vec<u64> {
    response["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["id"].as_u64().unwrap())
        .collect()
}
#[tokio::test]
async fn category_discovery_is_caps_only_scoped_private_and_nonpersistent() -> Result<(), Error> {
    let scratch = Sandbox::new();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await?);
    let conn = db.connect().await?;
    let key = Arc::new(providers::CredentialKey::from_hex(&"12".repeat(32)).unwrap());
    let (address, server) = serve(db.clone(), Some(key)).await;
    let (keyless, keyless_server) = serve(db.clone(), None).await;
    let state = Arc::new(Remote::new(true));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let app = axum::Router::new()
        .fallback(remote)
        .with_state(state.clone());
    let upstream = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let other = Arc::new(Remote::new(false));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let destination = format!("http://{}", listener.local_addr().unwrap());
    let app = axum::Router::new()
        .fallback(remote)
        .with_state(other.clone());
    let other_server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let empty = snapshot(address, &conn).await;
    for implementation in ["torznab", "newznab"] {
        for media in ["tv", "movies"] {
            for blank in ["", " \t "] {
                let before = state.calls.lock().unwrap().len();
                let (code, result) = discover(keyless, &input(implementation, blank, media)).await;
                assert_eq!(code, 200);
                assert_eq!(result["origin"], "standard_fallback");
                assert_eq!(result["discovery_error"]["code"], "not_configured");
                assert_eq!(
                    ids(&result),
                    if media == "tv" {
                        vec![5000, 5010, 5020, 5030, 5040, 5045, 5050, 5060, 5070, 5080]
                    } else {
                        vec![2000, 2010, 2020, 2030, 2040, 2045, 2050, 2060]
                    }
                );
                assert_eq!(before, state.calls.lock().unwrap().len());
            }
            let wanted = if media == "tv" {
                vec![5000, 5030, 5070, 100001, 100002, 2000, 2040]
            } else {
                vec![2000, 2040, 100001, 100002, 5000, 5030, 5070]
            };
            for mode in [0, 4] {
                state.mode.store(mode, Ordering::SeqCst);
                let before = state.calls.lock().unwrap().len();
                let (code, result) =
                    discover(keyless, &input(implementation, &endpoint, media)).await;
                assert_eq!(code, 200, "{result}");
                assert_eq!(result["origin"], "advertised");
                assert!(result["discovery_error"].is_null());
                assert_eq!(
                    ids(&result),
                    wanted,
                    "Unrelated invalid limit metadata must not erase advertised categories"
                );
                assert_eq!(
                    result["options"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|v| v["id"] == 5030)
                        .unwrap()["parent_id"],
                    5000
                );
                assert!(!result.to_string().contains(KEY));
                assert!(!result.to_string().contains(TV));
                assert!(!result.to_string().contains(MOVIE));
                assert_eq!(state.calls.lock().unwrap().len(), before + 1);
            }
            for (mode, origin, code) in [
                (1, "advertised", None),
                (2, "standard_fallback", Some("authentication")),
                (3, "standard_fallback", Some("invalid_response")),
                (6, "standard_fallback", Some("rate_limited")),
            ] {
                state.mode.store(mode, Ordering::SeqCst);
                let (status, result) =
                    discover(keyless, &input(implementation, &endpoint, media)).await;
                assert_eq!(status, 200, "{result}");
                assert_eq!(result["origin"], origin);
                if let Some(code) = code {
                    assert_eq!(result["discovery_error"]["code"], code);
                    assert!(!result["options"].as_array().unwrap().is_empty());
                } else {
                    assert_eq!(result["options"], json!([]));
                }
                if mode == 6 {
                    assert_eq!(result["discovery_error"]["retry_after_seconds"], 17);
                }
                assert!(!result.to_string().contains(KEY));
            }
        }
    }
    state.mode.store(0, Ordering::SeqCst);
    assert_eq!(snapshot(address, &conn).await, empty);
    let mut saved = vec![];
    for implementation in ["torznab", "newznab"] {
        let mut discovery = input(implementation, &endpoint, "tv");
        let config = json!({"name":implementation,"enabled":true,"priority":1,"settings":{"implementation":implementation,"endpoint":endpoint,"tv":{"categories":[5000],"anime_categories":[]},"movies":{"categories":[2000]}},"credentials":discovery["connection"]["credentials"]});
        let (code, provider) =
            request(address, "POST", "/api/v1/providers", &config.to_string()).await;
        assert_eq!(code, 201);
        state.allow_rss.store(true, Ordering::SeqCst);
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
        state.allow_rss.store(false, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(110)).await;
        discovery["source"] = json!({"id":provider["id"],"revision":provider["revision"]});
        discovery["connection"]
            .as_object_mut()
            .unwrap()
            .remove("credentials");
        let baseline = snapshot(address, &conn).await;
        for media in ["tv", "movies"] {
            discovery["media_type"] = json!(media);
            let (code, result) = discover(address, &discovery).await;
            assert_eq!(code, 200);
            assert_eq!(result["origin"], "advertised");
            assert!(!result.to_string().contains(KEY));
            assert!(!result.to_string().contains(TV));
            tokio::time::sleep(Duration::from_millis(110)).await;
        }
        for locked_key in [
            None,
            Some(Arc::new(
                providers::CredentialKey::from_hex(&"34".repeat(32)).unwrap(),
            )),
        ] {
            let (locked, task) = serve(db.clone(), locked_key).await;
            let before = state.calls.lock().unwrap().len();
            assert_eq!(discover(locked, &discovery).await.0, 503);
            assert_eq!(state.calls.lock().unwrap().len(), before);
            let mut blank = discovery.clone();
            blank["connection"]["endpoint"] = json!(" ");
            let (code, result) = discover(locked, &blank).await;
            assert_eq!(code, 200);
            assert_eq!(result["discovery_error"]["code"], "not_configured");
            assert_eq!(state.calls.lock().unwrap().len(), before);
            for clear in [false, true] {
                let mut changed = discovery.clone();
                changed["connection"]["endpoint"] = json!(destination);
                changed["connection"]["credentials"] = if clear {
                    Value::Null
                } else {
                    json!({"kind":"api_key","api_key":"EXPLICIT_CATEGORY_KEY"})
                };
                *other.auth.lock().unwrap() = if clear {
                    None
                } else {
                    Some("EXPLICIT_CATEGORY_KEY".into())
                };
                // A fallback also returns 200; explicit credential choices must actually discover advertised data.
                let (code, result) = discover(locked, &changed).await;
                assert_eq!(code, 200);
                assert_eq!(result["origin"], "advertised");
                assert!(result["discovery_error"].is_null());
                assert!(!result.to_string().contains("EXPLICIT_CATEGORY_KEY"));
                tokio::time::sleep(Duration::from_millis(110)).await;
            }
            task.abort();
            let _ = task.await;
        }
        for endpoint in [format!("{endpoint}/changed"), destination.clone()] {
            let mut changed = discovery.clone();
            changed["connection"]["endpoint"] = json!(endpoint);
            let a = state.calls.lock().unwrap().len();
            let b = other.calls.lock().unwrap().len();
            let (code, error) = discover(address, &changed).await;
            assert_eq!(code, 422);
            assert_eq!(error["error"]["code"], "provider_credential_binding");
            assert_eq!(a, state.calls.lock().unwrap().len());
            assert_eq!(b, other.calls.lock().unwrap().len());
        }
        assert_eq!(snapshot(address, &conn).await, baseline);
        saved.push((provider, config, discovery));
    }
    let before = state.calls.lock().unwrap().len();
    for endpoint in [
        " ".repeat(2049),
        format!("{endpoint}?apikey={KEY}"),
        "not a url".into(),
    ] {
        assert_eq!(
            discover(address, &input("torznab", &endpoint, "tv"))
                .await
                .0,
            400
        );
    }
    let mut qbit = input("torznab", &endpoint, "tv");
    qbit["connection"]["implementation"] = json!("qbittorrent");
    assert_eq!(discover(address, &qbit).await.0, 400);
    assert_eq!(
        request(
            address,
            "POST",
            "/api/v1/providers/indexer-categories?forceTest=true",
            &input("torznab", &endpoint, "tv").to_string()
        )
        .await
        .0,
        400
    );
    assert_eq!(before, state.calls.lock().unwrap().len());
    for (index, (provider, config, discovery)) in saved.iter().enumerate() {
        state.mode.store(5, Ordering::SeqCst);
        state.failed.store(index == 1, Ordering::SeqCst);
        let d = discovery.clone();
        let pending = tokio::spawn(async move { discover(address, &d).await });
        tokio::time::timeout(Duration::from_secs(3), state.started.notified())
            .await
            .unwrap();
        assert_eq!(
            discover(address, discovery).await.0,
            429,
            "Same provider lane cannot be bypassed"
        );
        if index == 0 {
            let mut edit = config.clone();
            edit["revision"] = provider["revision"].clone();
            edit["name"] = json!("changed during caps");
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
        } else {
            assert_eq!(
                request(
                    address,
                    "DELETE",
                    &format!(
                        "/api/v1/providers/{}?revision=1",
                        provider["id"].as_str().unwrap()
                    ),
                    ""
                )
                .await
                .0,
                204
            );
        }
        state.release.add_permits(1);
        let (code, error) = tokio::time::timeout(Duration::from_secs(5), pending)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(code, 409);
        assert_eq!(error["error"]["code"], "provider_revision_conflict");
    }
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
