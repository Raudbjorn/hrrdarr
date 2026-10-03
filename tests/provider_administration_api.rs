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
    let app =
        providers::router(db.clone(), key).merge(hrrdarr::completed_download_handling::router(db));
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
const PRIVATE: &str = "ADMIN_PRIVATE_CREDENTIAL";
struct Remote {
    calls: Mutex<Vec<String>>,
    blocked: AtomicBool,
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
    if uri.path().contains("indexer") {
        let args: std::collections::HashMap<_, _> =
            url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
                .into_owned()
                .collect();
        assert_eq!(args.get("apikey").unwrap(), PRIVATE);
        return ([("content-type","application/xml")],if args.get("t").is_some_and(|v|v=="caps") {r#"<caps><searching><search available="yes" supportedParams="q"/><tv-search available="yes" supportedParams="q,season,ep"/><movie-search available="yes" supportedParams="q,imdbid"/></searching><categories><category id="5000"/><category id="2000"/></categories></caps>"#}else{r#"<rss><channel/></rss>"#}).into_response();
    }
    assert_eq!(headers["authorization"], format!("Bearer {PRIVATE}"));
    if uri.path().ends_with("webapiVersion") && s.blocked.load(Ordering::SeqCst) {
        let active = s.active.fetch_add(1, Ordering::SeqCst) + 1;
        s.peak.fetch_max(active, Ordering::SeqCst);
        s.started.notify_one();
        let permit = s.release.acquire().await.unwrap();
        permit.forget();
        s.active.fetch_sub(1, Ordering::SeqCst);
    }
    if uri.path().contains("failure") {
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
fn selection(items: &[Value], domain: &str, kind: &str) -> Value {
    json!({"media_type":domain,"kind":kind,"items":items.iter().map(|p|json!({"id":p["id"],"revision":p["revision"]})).collect::<Vec<_>>()})
}
#[tokio::test]
async fn native_administration_is_atomic_scoped_bounded_and_secret_safe() -> Result<(), Error> {
    let scratch = Sandbox::new();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await?);
    let conn = db.connect().await?;
    let key = Arc::new(providers::CredentialKey::from_hex(&"12".repeat(32)).unwrap());
    let (address, server) = serve(db.clone(), Some(key.clone())).await;
    let remote_state = Arc::new(Remote::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let router = axum::Router::new()
        .fallback(remote)
        .with_state(remote_state.clone());
    let upstream = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    for domain in ["tv", "movies"] {
        for kind in ["indexer", "download_client"] {
            let (code, schema) = request(
                address,
                "GET",
                &format!("/api/v1/providers/schema?media_type={domain}&kind={kind}"),
                "",
            )
            .await;
            assert_eq!(code, 200);
            let templates = schema["templates"].as_array().unwrap();
            // Reasoning: 0050 adds the generic torrentrss feed implementation to the indexer template list.
            assert_eq!(templates.len(), if kind == "indexer" { 3 } else { 1 });
            assert_eq!(
                templates
                    .iter()
                    .map(|t| t["implementation"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                if kind == "indexer" {
                    vec!["newznab", "torznab", "torrentrss"]
                } else {
                    vec!["qbittorrent"]
                }
            );
            for t in templates {
                assert_eq!(t["enabled"], false);
                assert_eq!(t["priority"], 1);
                if t["implementation"] == "torrentrss" {
                    // Reasoning: feed defaults carry no categories or search flags; both scopes default to RSS on.
                    assert_eq!(t["defaults"]["kind"], "feed");
                    assert_eq!(
                        t["defaults"]["tv"],
                        json!({"enable_rss":true,"minimum_seeders":null})
                    );
                    assert_eq!(t["defaults"]["movies"], t["defaults"]["tv"]);
                    assert_eq!(t["presets"], json!([]));
                } else if kind == "indexer" {
                    assert_eq!(t["defaults"]["tv"]["categories"], json!([]));
                    assert_eq!(t["defaults"]["tv"]["anime_standard_format_search"], false);
                    assert_eq!(t["defaults"]["movies"]["remove_year"], false);
                } else {
                    assert_eq!(t["defaults"]["initial_state"], "started");
                    assert_eq!(t["defaults"]["content_layout"], "default");
                    assert_eq!(t["defaults"]["sequential_order"], false);
                    assert_eq!(t["defaults"]["add_tags"], false);
                    assert!(t["defaults"].get("category").is_none());
                }

                assert_eq!(t["supported_media"], json!(["tv", "movies"]));
                assert!(t.get("endpoint").is_none());
                assert!(!t.to_string().contains(PRIVATE));
            }
        }
    }
    assert_eq!(
        request(address, "GET", "/api/v1/providers/schema?kind=indexer", "")
            .await
            .0,
        400
    );
    let (code, empty) = request(
        address,
        "POST",
        "/api/v1/providers/testall",
        r#"{"media_type":"movies","kind":"indexer"}"#,
    )
    .await;
    assert_eq!(code, 200);
    assert_eq!(empty["items"], json!([]));
    let mut first = create(
        address,
        &config("00good", &format!("{endpoint}/good"), "qbittorrent"),
    )
    .await;
    let mut second = create(
        address,
        &config("01failure", &format!("{endpoint}/failure"), "qbittorrent"),
    )
    .await;
    let cipher = conn
        .query(
            "SELECT credentials FROM providers WHERE id=?",
            [first["id"].as_str().unwrap()],
        )
        .await?
        .next()
        .await?
        .unwrap()
        .get::<Vec<u8>>(0)?;
    let original_settings = first["settings"].clone();
    let selected = selection(&[first.clone(), second.clone()], "tv", "download_client");
    let mut maximum = selection(&[first.clone()], "tv", "download_client");
    maximum["items"][0]["revision"] = json!(9007199254740991_i64);
    assert_eq!(
        request(
            address,
            "DELETE",
            "/api/v1/providers/bulk",
            &maximum.to_string()
        )
        .await
        .0,
        409,
        "The maximum safe revision reaches the stale check, not a range rejection"
    );
    maximum["changes"] = json!({"enabled":false});
    assert_eq!(
        request(
            address,
            "PUT",
            "/api/v1/providers/bulk",
            &maximum.to_string()
        )
        .await
        .0,
        400,
        "An exhausted revision cannot be incremented"
    );
    let mut patch = selected.clone();
    patch["changes"] = json!({"priority":100});
    for mutation in ["duplicate", "stale", "missing", "scope", "empty"] {
        let mut bad = patch.clone();
        match mutation {
            "duplicate" => bad["items"][1] = bad["items"][0].clone(),
            "stale" => bad["items"][1]["revision"] = json!(99),
            "missing" => bad["items"][1]["id"] = json!(uuid::Uuid::new_v4()),
            "scope" => bad["kind"] = json!("indexer"),
            _ => bad["changes"] = json!({}),
        };
        let (code, _) = request(address, "PUT", "/api/v1/providers/bulk", &bad.to_string()).await;
        assert!(matches!(code, 400 | 404 | 409));
    }
    // Obtain a real observation so rollback must restore both configuration and its test status.
    assert_eq!(
        request(
            address,
            "POST",
            &format!("/api/v1/providers/{}/test", first["id"].as_str().unwrap()),
            ""
        )
        .await
        .0,
        200
    );
    // A failure on the second SQL write must undo the first update, including status invalidation.
    conn.execute_batch("CREATE TRIGGER admin_late_failure BEFORE UPDATE ON providers WHEN OLD.name='01failure' BEGIN SELECT RAISE(ABORT,'private failure'); END;").await?;
    assert_eq!(
        request(address, "PUT", "/api/v1/providers/bulk", &patch.to_string())
            .await
            .0,
        500
    );
    conn.execute_batch("DROP TRIGGER admin_late_failure")
        .await?;
    assert_eq!(
        request(
            address,
            "GET",
            &format!("/api/v1/providers/{}", first["id"].as_str().unwrap()),
            ""
        )
        .await
        .1["revision"],
        1
    );
    let retained = request(
        address,
        "GET",
        &format!("/api/v1/providers/{}", first["id"].as_str().unwrap()),
        "",
    )
    .await
    .1;
    assert_eq!(retained["test_status"], "success");
    assert_eq!(retained["last_test"]["revision"], 1);
    for material in [
        None,
        Some(Arc::new(
            providers::CredentialKey::from_hex(&"34".repeat(32)).unwrap(),
        )),
    ] {
        let (locked, task) = serve(db.clone(), material).await;
        for method in ["PUT", "DELETE"] {
            let body = if method == "PUT" { &patch } else { &selected };
            assert_eq!(
                request(locked, method, "/api/v1/providers/bulk", &body.to_string())
                    .await
                    .0,
                503
            );
        }
        let before = remote_state.calls.lock().unwrap().len();
        let (code, result) = request(
            locked,
            "POST",
            "/api/v1/providers/testall",
            r#"{"media_type":"tv","kind":"download_client"}"#,
        )
        .await;
        assert_eq!(code, 200);
        assert_eq!(result["items"].as_array().unwrap().len(), 2);
        assert!(
            result["items"]
                .as_array()
                .unwrap()
                .iter()
                .all(|v| v["outcome"]["code"] == "provider_credentials_locked")
        );
        assert_eq!(before, remote_state.calls.lock().unwrap().len());
        task.abort();
        let _ = task.await;
    }
    let (code, updated) =
        request(address, "PUT", "/api/v1/providers/bulk", &patch.to_string()).await;
    assert_eq!(code, 200, "{updated}");
    first = updated["items"][0].clone();
    assert_eq!(first["test_status"], "never_tested");
    second = updated["items"][1].clone();
    assert_eq!(first["settings"], original_settings);
    assert_eq!(first["revision"], 2);
    assert_eq!(first["priority"], 100);
    assert_eq!(
        conn.query(
            "SELECT credentials FROM providers WHERE id=?",
            [first["id"].as_str().unwrap()]
        )
        .await?
        .next()
        .await?
        .unwrap()
        .get::<Vec<u8>>(0)?,
        cipher
    );
    let mut disabled = config("disabled", &format!("{endpoint}/disabled"), "qbittorrent");
    disabled["enabled"] = json!(false);
    create(address, &disabled).await;
    let mut movie_only = config("movie-only", &format!("{endpoint}/good"), "qbittorrent");
    movie_only["settings"]["tv"] = Value::Null;
    movie_only["enabled"] = json!(false);
    let mut movie_only = create(address, &movie_only).await;
    let wrong_media = selection(&[movie_only.clone()], "tv", "download_client");
    assert_eq!(
        request(
            address,
            "DELETE",
            "/api/v1/providers/bulk",
            &wrong_media.to_string()
        )
        .await
        .0,
        400
    );
    // Isolated boundary fixture: bypass only the revision-step trigger, then restore its exact stored SQL atomically.
    let sql = conn
        .query(
            "SELECT sql FROM sqlite_master WHERE type='trigger' AND name='provider_revision_step'",
            (),
        )
        .await?
        .next()
        .await?
        .unwrap()
        .get::<String>(0)?;
    let tx = conn.transaction().await?;
    tx.execute_batch("DROP TRIGGER provider_revision_step")
        .await?;
    tx.execute(
        "UPDATE providers SET revision=9007199254740991 WHERE id=?",
        [movie_only["id"].as_str().unwrap()],
    )
    .await?;
    tx.execute_batch(&sql).await?;
    tx.commit().await?;
    movie_only["revision"] = json!(9007199254740991_i64);
    let exhausted = selection(&[movie_only], "movies", "download_client");
    assert_eq!(
        request(
            address,
            "DELETE",
            "/api/v1/providers/bulk",
            &exhausted.to_string()
        )
        .await
        .0,
        204
    );
    let invalid = create(
        address,
        &config("invalid-stored", &format!("{endpoint}/good"), "qbittorrent"),
    )
    .await;
    conn.execute(
        "UPDATE providers SET endpoint=?,revision=revision+1 WHERE id=?",
        libsql::params![PRIVATE, invalid["id"].as_str().unwrap()],
    )
    .await?;
    for domain in ["tv", "movies"] {
        let (code, batch) = request(
            address,
            "POST",
            "/api/v1/providers/testall",
            &json!({"media_type":domain,"kind":"download_client"}).to_string(),
        )
        .await;
        assert_eq!(code, 200, "{batch}");
        assert_eq!(batch["items"].as_array().unwrap().len(), 3);
        assert!(
            batch["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["outcome"]["code"] == "invalid_stored_provider")
        );
        assert!(
            batch["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["outcome"]["status"] == "success")
        );
        assert!(
            batch["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["outcome"]["code"] == "authentication")
        );
        assert!(!batch.to_string().contains(PRIVATE));
        tokio::time::sleep(Duration::from_millis(110)).await;
    }
    assert!(
        !remote_state
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|p| p.contains("disabled"))
    );
    conn.execute(
        "UPDATE providers SET endpoint=?,enabled=0,revision=revision+1 WHERE id=?",
        libsql::params![format!("{endpoint}/good"), invalid["id"].as_str().unwrap()],
    )
    .await?;
    for implementation in ["torznab", "newznab"] {
        create(
            address,
            &config(
                implementation,
                &format!("{endpoint}/indexer"),
                implementation,
            ),
        )
        .await;
    }
    for domain in ["tv", "movies"] {
        let call_offset = remote_state.calls.lock().unwrap().len();
        let (code, batch) = request(
            address,
            "POST",
            "/api/v1/providers/testall",
            &json!({"media_type":domain,"kind":"indexer"}).to_string(),
        )
        .await;
        assert_eq!(code, 200, "{batch}");
        assert_eq!(batch["items"].as_array().unwrap().len(), 2);
        assert!(
            batch["items"]
                .as_array()
                .unwrap()
                .iter()
                .all(|v| v["outcome"]["status"] == "success")
        );
        // Check each batch separately: combining TV and movie batches could hide a scope-only probe.
        {
            let calls = remote_state.calls.lock().unwrap();
            let batch_calls = &calls[call_offset..];
            assert!(batch_calls.iter().any(|p| p.contains("cat=5000")));
            assert!(batch_calls.iter().any(|p| p.contains("cat=2000")));
        }
        tokio::time::sleep(Duration::from_millis(110)).await;
    }
    let (_, listed) = request(address, "GET", "/api/v1/providers?media_type=movies", "").await;
    let indexers: Vec<Value> = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["settings"]["implementation"] != "qbittorrent")
        .cloned()
        .collect();
    let mut edit = selection(&indexers, "movies", "indexer");
    edit["changes"] = json!({"enabled":false,"priority":100});
    let (code, updated) =
        request(address, "PUT", "/api/v1/providers/bulk", &edit.to_string()).await;
    assert_eq!(code, 200);
    for (old, new) in indexers.iter().zip(updated["items"].as_array().unwrap()) {
        assert_eq!(old["settings"], new["settings"]);
        assert_eq!(new["has_credentials"], true);
        assert_eq!(new["enabled"], false);
        assert_eq!(new["priority"], 100);
    }
    let before = remote_state.calls.lock().unwrap().len();
    let (code, empty) = request(
        address,
        "POST",
        "/api/v1/providers/testall",
        r#"{"media_type":"tv","kind":"indexer"}"#,
    )
    .await;
    assert_eq!(code, 200);
    assert_eq!(empty["items"], json!([]));
    assert_eq!(remote_state.calls.lock().unwrap().len(), before);
    let delete_indexers = selection(updated["items"].as_array().unwrap(), "tv", "indexer");
    assert_eq!(
        request(
            address,
            "DELETE",
            "/api/v1/providers/bulk",
            &delete_indexers.to_string()
        )
        .await
        .0,
        204
    );
    for n in 0..4 {
        create(
            address,
            &config(
                &format!("parallel{n}"),
                &format!("{endpoint}/good"),
                "qbittorrent",
            ),
        )
        .await;
    }
    remote_state.blocked.store(true, Ordering::SeqCst);
    let pending = tokio::spawn(async move {
        request(
            address,
            "POST",
            "/api/v1/providers/testall",
            r#"{"media_type":"tv","kind":"download_client"}"#,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        while remote_state.active.load(Ordering::SeqCst) < 4 {
            remote_state.started.notified().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        request(
            address,
            "POST",
            "/api/v1/providers/testall",
            r#"{"media_type":"movies","kind":"download_client"}"#
        )
        .await
        .0,
        429 // Batch admission reuses the existing static provider_busy response.
    );
    let mut changed = selection(&[first.clone()], "movies", "download_client");
    changed["changes"] = json!({"enabled":false});
    assert_eq!(
        request(
            address,
            "PUT",
            "/api/v1/providers/bulk",
            &changed.to_string()
        )
        .await
        .0,
        200
    );
    remote_state.release.add_permits(32);
    let (code, batch) = tokio::time::timeout(Duration::from_secs(5), pending)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(code, 200);
    assert_eq!(remote_state.peak.load(Ordering::SeqCst), 4);
    assert!(
        batch["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["provider_id"] == first["id"] && v["outcome"]["status"] == "changed")
    );
    let after_edit = request(
        address,
        "GET",
        &format!("/api/v1/providers/{}", first["id"].as_str().unwrap()),
        "",
    )
    .await
    .1;
    assert_eq!(after_edit["revision"], 3);
    assert_eq!(after_edit["test_status"], "never_tested");
    remote_state.blocked.store(false, Ordering::SeqCst);
    // Stale deletion preflight leaves all selected rows; late delete failure rolls back an earlier delete.
    let selected = selection(&[first.clone(), second.clone()], "tv", "download_client");
    assert_eq!(
        request(
            address,
            "DELETE",
            "/api/v1/providers/bulk",
            &selected.to_string()
        )
        .await
        .0,
        409
    );
    first["revision"] = json!(3);
    let selected = selection(
        &[first.clone(), second.clone()],
        "movies",
        "download_client",
    );
    conn.execute_batch("CREATE TRIGGER admin_delete_failure BEFORE DELETE ON providers WHEN OLD.name='01failure' BEGIN SELECT RAISE(ABORT,'private failure'); END;").await?;
    assert_eq!(
        request(
            address,
            "DELETE",
            "/api/v1/providers/bulk",
            &selected.to_string()
        )
        .await
        .0,
        500
    );
    conn.execute_batch("DROP TRIGGER admin_delete_failure")
        .await?;
    assert_eq!(
        request(
            address,
            "GET",
            &format!("/api/v1/providers/{}", first["id"].as_str().unwrap()),
            ""
        )
        .await
        .0,
        200
    );
    assert_eq!(
        request(
            address,
            "DELETE",
            "/api/v1/providers/bulk",
            &selected.to_string()
        )
        .await
        .0,
        204
    );
    // Isolate provider-test batch capacity from the independent 64-schedule CDH cap:
    // 33 enabled dual-domain clients would otherwise require 66 inherited schedules.
    // Disable only domain import intent through the public CAS API; provider enablement,
    // scopes, the testall limit and its no-upstream-call assertion remain unchanged.
    for domain in ["tv", "movies"] {
        let path = format!("/api/v1/{domain}/completed-download-handling");
        let (code, settings) = request(address, "GET", &path, "").await;
        assert_eq!(code, 200, "{settings}");
        let (code, disabled) = request(
            address,
            "PUT",
            &path,
            &json!({"enabled":false,"revision":settings["revision"]}).to_string(),
        )
        .await;
        assert_eq!(code, 200, "{disabled}");
        assert_eq!(disabled["enabled"], false);
    }
    // Overflow is determined before the first upstream call, rather than silently testing a prefix.
    for n in 0..29 {
        create(
            address,
            &config(
                &format!("overflow{n}"),
                &format!("{endpoint}/good"),
                "qbittorrent",
            ),
        )
        .await;
    }
    let before = remote_state.calls.lock().unwrap().len();
    let (code, error) = request(
        address,
        "POST",
        "/api/v1/providers/testall",
        r#"{"media_type":"tv","kind":"download_client"}"#,
    )
    .await;
    assert_eq!(code, 400);
    assert_eq!(error["error"]["code"], "provider_batch_too_large");
    assert_eq!(remote_state.calls.lock().unwrap().len(), before);
    server.abort();
    upstream.abort();
    let _ = server.await;
    let _ = upstream.await;
    drop(conn);
    drop(db);
    let reopened = Database::open_local(scratch.0.join("db")).await?;
    let conn = reopened.connect().await?;
    assert_eq!(
        conn.query(
            "SELECT count(*) FROM providers WHERE id IN (?,?)",
            libsql::params![
                first["id"].as_str().unwrap(),
                second["id"].as_str().unwrap()
            ]
        )
        .await?
        .next()
        .await?
        .unwrap()
        .get::<i64>(0)?,
        0
    );
    Ok(())
}
