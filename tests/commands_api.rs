use axum::{
    extract::{OriginalUri, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
};
use hrrdarr::{commands, db::Database, providers};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};
use tokio::task::JoinHandle;
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("hrrdarr-commands-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
struct Remote {
    mode: AtomicU8,
    calls: Mutex<Vec<String>>,
    blocked_prefix: Mutex<Option<String>>,
    started: tokio::sync::Notify,
    release: tokio::sync::Semaphore,
}
impl Default for Remote {
    fn default() -> Self {
        Self {
            mode: AtomicU8::new(0),
            calls: Mutex::new(vec![]),
            blocked_prefix: Mutex::new(None),
            started: tokio::sync::Notify::new(),
            release: tokio::sync::Semaphore::new(0),
        }
    }
}
impl Remote {
    fn refresh_reads_for(&self, prefix: &str) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|uri| {
                uri.split('?').next().is_some_and(|path| {
                    path.starts_with(prefix) && path.ends_with("/torrents/info")
                })
            })
            .count()
    }
    fn refresh_reads(&self) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|uri| {
                uri.split('?')
                    .next()
                    .is_some_and(|path| path.ends_with("/torrents/info"))
            })
            .count()
    }
}
async fn remote(
    State(s): State<Arc<Remote>>,
    method: Method,
    OriginalUri(uri): OriginalUri,
) -> Response {
    assert_eq!(method, Method::GET, "refresh must never mutate the client");
    s.calls.lock().unwrap().push(uri.to_string());
    if uri.path().ends_with("webapiVersion") {
        return "2.8.3".into_response();
    }
    // CDH status checks use preferences/categories; communication health also fetches items.
    if uri.path().ends_with("app/preferences") {
        return axum::Json(json!({"save_path":"/fixture-downloads","max_ratio_enabled":false,"max_seeding_time_enabled":false,"max_seeding_time":-1,"max_ratio_act":0})).into_response();
    }
    if uri.path().ends_with("torrents/categories") {
        return axum::Json(
            json!({"tv":{"name":"tv","savePath":""},"movies":{"name":"movies","savePath":""}}),
        )
        .into_response();
    }
    if uri.path().ends_with("torrents/info") {
        // Revision-specific endpoint prefixes let the edit-race test hold only the old
        // configuration's request. New-revision health traffic has independent ownership.
        let selected = s
            .blocked_prefix
            .lock()
            .unwrap()
            .as_ref()
            .is_none_or(|prefix| uri.path().starts_with(prefix));
        match if selected {
            s.mode.load(Ordering::SeqCst)
        } else {
            0
        } {
            1 => return (StatusCode::SERVICE_UNAVAILABLE, "PRIVATE_REMOTE_ERROR").into_response(),
            mode @ (2 | 3) => {
                s.started.notify_one();
                let permit = s.release.acquire().await.unwrap();
                permit.forget();
                if mode == 3 {
                    return (StatusCode::SERVICE_UNAVAILABLE, "PRIVATE_REMOTE_ERROR")
                        .into_response();
                }
            }
            _ => {}
        }
        let args: std::collections::HashMap<_, _> =
            url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
                .into_owned()
                .collect();
        let category = args.get("category").unwrap();
        // Deliberately identical hash and title across domain categories: neither is a library ID.
        return axum::Json(json!([{"hash":"1111111111111111111111111111111111111111","category":category,"name":"Unassociated 2024","state":"downloading","progress":0.5,"size":100,"amount_left":50,"dlspeed":1,"upspeed":0,"ratio":0.0}])).into_response();
    }
    StatusCode::NOT_FOUND.into_response()
}
async fn serve(app: axum::Router) -> (String, JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    (
        address,
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() }),
    )
}
async fn request(base: &str, method: &str, path: &str, body: Value) -> (u16, Value) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let started = std::time::Instant::now();
    let response = client
        .request(method.parse().unwrap(), format!("{base}{path}"))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!(
                "owned request failed: method={method} path={path} elapsed={:?} error={error:?}",
                started.elapsed()
            )
        });
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_else(|error| {
        panic!(
            "owned response body failed: method={method} path={path} status={status} elapsed={:?} error={error:?}",
            started.elapsed()
        )
    });
    assert!(!text.contains("PRIVATE_REMOTE_ERROR"));
    (
        status,
        if status == 204 {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or_else(|_| panic!("{status}: {text}"))
        },
    )
}
async fn until(base: &str, id: &str, status: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            let (code, v) =
                request(base, "GET", &format!("/api/v1/commands/{id}"), Value::Null).await;
            assert_eq!(code, 200, "{v}");
            if v["status"] == status {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("command {id} did not reach {status}"))
}
fn input(provider: &Value, media: &str) -> Value {
    json!({"name":"refresh_downloads","target":{"provider_id":provider["id"],"media_type":media},"provider_revision":provider["revision"],"priority":"normal"})
}
async fn submit(base: &str, provider: &Value, media: &str) -> Value {
    let (c, v) = request(base, "POST", "/api/v1/commands", input(provider, media)).await;
    assert_eq!(c, 202, "{v}");
    v
}
// A held-request handshake must belong to the commanded refresh, not normal-priority
// startup communication health. Other tests retain normal priority to exercise ordering.
async fn submit_high(base: &str, provider: &Value, media: &str) -> Value {
    let mut body = input(provider, media);
    body["priority"] = json!("high");
    let (code, command) = request(base, "POST", "/api/v1/commands", body).await;
    assert_eq!(code, 202, "{command}");
    command
}
// These command/lease tests control every admitted read. CDH now provisions
// inherited observation, so explicitly suppress only this fixture's scopes before
// starting a worker. The separate CDH default-flow test retains automatic schedules.
async fn suppress_observation(base: &str, provider: &Value) {
    let (code, schedules) = request(
        base,
        "GET",
        "/api/v1/download-refresh/schedules",
        Value::Null,
    )
    .await;
    assert_eq!(code, 200, "{schedules}");
    let mut domains = schedules
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["target"]["provider_id"] == provider["id"])
        .map(|s| s["target"]["media_type"].as_str().unwrap())
        .collect::<Vec<_>>();
    domains.sort_unstable();
    assert_eq!(
        domains,
        vec!["movies", "tv"],
        "suppress both actual provider scopes"
    );
    for schedule in schedules
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["target"]["provider_id"] == provider["id"])
    {
        let (code, value) = request(
            base,
            "DELETE",
            "/api/v1/download-refresh/schedules",
            json!({"target":schedule["target"],"revision":schedule["revision"]}),
        )
        .await;
        assert_eq!(code, 204, "{value}");
    }
    let (code, schedules) = request(
        base,
        "GET",
        "/api/v1/download-refresh/schedules",
        Value::Null,
    )
    .await;
    assert_eq!(code, 200, "{schedules}");
    assert!(
        !schedules
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["target"]["provider_id"] == provider["id"])
    );
}
async fn enable_observation(base: &str, provider: &Value, media: &str) {
    // Respect provisioned-row CAS; a deliberately suppressed row is publicly absent.
    let (code, schedules) = request(
        base,
        "GET",
        "/api/v1/download-refresh/schedules",
        Value::Null,
    )
    .await;
    assert_eq!(code, 200, "{schedules}");
    let revision = schedules
        .as_array()
        .unwrap()
        .iter()
        .find(|s| {
            s["target"]["provider_id"] == provider["id"] && s["target"]["media_type"] == media
        })
        .map(|s| s["revision"].clone())
        .unwrap_or(Value::Null);
    let (code,value)=request(base,"PUT","/api/v1/download-refresh/schedules",json!({"target":{"provider_id":provider["id"],"media_type":media},"revision":revision,"provider_revision":provider["revision"],"enabled":true,"interval_seconds":60})).await;
    assert_eq!(code, 200, "{value}");
}
async fn stop(task: JoinHandle<()>) {
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn durable_refresh_preserves_domains_retries_cancellation_schedules_and_restart() {
    let scratch = Scratch::new();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let remote_state = Arc::new(Remote {
        release: tokio::sync::Semaphore::new(0),
        ..Default::default()
    });
    let (endpoint, upstream) = serve(
        axum::Router::new()
            .fallback(remote)
            .with_state(remote_state.clone()),
    )
    .await;
    let (app, client) = providers::router_with_refresh(db.clone(), None);
    let (base, server) = serve(app.merge(commands::router(db.clone()))).await;
    let scope = |category| json!({"category":category,"imported_category":null,"recent_priority":0,"older_priority":1});
    let config = json!({"name":"shared","enabled":true,"priority":1,"settings":{"implementation":"qbittorrent","endpoint":format!("{endpoint}/config-1/"),"tv":scope("tv"),"movies":scope("movies")},"credentials":null});
    let (code, provider) = request(&base, "POST", "/api/v1/providers", config.clone()).await;
    assert_eq!(code, 201, "{provider}");
    suppress_observation(&base, &provider).await;
    for body in [
        json!({}),
        json!({"name":"refresh_series","target":{"provider_id":provider["id"],"media_type":"tv"},"provider_revision":1,"priority":"normal"}),
        json!({"name":"refresh_downloads","target":{"provider_id":provider["id"],"media_type":"episode"},"provider_revision":1,"priority":"normal"}),
    ] {
        assert_eq!(
            request(&base, "POST", "/api/v1/commands", body).await.0,
            400
        )
    }
    assert_eq!(
        request(&base, "GET", "/api/v1/commands/not-a-uuid", Value::Null)
            .await
            .0,
        400
    );
    let tv = submit(&base, &provider, "tv").await;
    let movie = submit(&base, &provider, "movies").await;
    assert_eq!(
        submit(&base, &provider, "tv").await["id"],
        tv["id"],
        "active scope enqueue is idempotent"
    );
    assert_eq!(
        request(
            &base,
            "DELETE",
            &format!("/api/v1/commands/{}", tv["id"].as_str().unwrap()),
            Value::Null
        )
        .await
        .0,
        409
    );
    let runtime = commands::start(db.clone(), client.clone()).await.unwrap();
    assert!(
        commands::start(db.clone(), client.clone()).await.is_err(),
        "same DB cannot start another worker"
    );
    for (command, media) in [(&tv, "tv"), (&movie, "movies")] {
        let done = until(&base, command["id"].as_str().unwrap(), "succeeded").await;
        assert_eq!(done["attempts"], 1);
        assert_eq!(done["items_observed"], 1);
        assert_eq!(done["target"]["media_type"], media);
        let (code, snapshot) = request(
            &base,
            "GET",
            &format!(
                "/api/v1/queue?provider_id={}&media_type={media}",
                provider["id"].as_str().unwrap()
            ),
            Value::Null,
        )
        .await;
        assert_eq!(code, 200);
        assert_eq!(snapshot["total"], 1);
        assert!(snapshot["items"][0]["association"].is_null());
        assert_eq!(snapshot["items"][0]["download"]["domain"], media);
    }
    runtime.shutdown().await;
    // Failed refresh preserves the dated last success. Repeated reads are safe; attempts are durable.
    remote_state.mode.store(1, Ordering::SeqCst);
    let retry = submit(&base, &provider, "tv").await;
    let runtime = commands::start(db.clone(), client.clone()).await.unwrap();
    let pending = until(&base, retry["id"].as_str().unwrap(), "retry_wait").await;
    assert_eq!(pending["attempts"], 1);
    assert_eq!(pending["error_code"], "refresh_failed");
    runtime.shutdown().await;
    let (_, old) = request(
        &base,
        "GET",
        &format!(
            "/api/v1/queue?provider_id={}&media_type=tv",
            provider["id"].as_str().unwrap()
        ),
        Value::Null,
    )
    .await;
    assert_eq!(old["command_id"], tv["id"]);
    remote_state.mode.store(0, Ordering::SeqCst);
    let runtime = commands::start(db.clone(), client.clone()).await.unwrap();
    let done = until(&base, retry["id"].as_str().unwrap(), "succeeded").await;
    assert_eq!(done["attempts"], 2);
    runtime.shutdown().await;
    // Active cancellation wins publication and releases the actual transport operation.
    remote_state.mode.store(2, Ordering::SeqCst);
    let cancelled = submit_high(&base, &provider, "movies").await;
    let runtime = commands::start(db.clone(), client.clone()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), remote_state.started.notified())
        .await
        .unwrap();
    assert_eq!(
        request(
            &base,
            "POST",
            &format!(
                "/api/v1/commands/{}/cancel",
                cancelled["id"].as_str().unwrap()
            ),
            Value::Null
        )
        .await
        .1["status"],
        "cancelled"
    );
    remote_state.mode.store(0, Ordering::SeqCst);
    remote_state.release.add_permits(1);
    tokio::time::sleep(Duration::from_millis(350)).await;
    let (_, old) = request(
        &base,
        "GET",
        &format!(
            "/api/v1/queue?provider_id={}&media_type=movies",
            provider["id"].as_str().unwrap()
        ),
        Value::Null,
    )
    .await;
    assert_eq!(old["command_id"], movie["id"]);
    runtime.shutdown().await;
    // Stop while the read is live, close every DB owner, then reopen actual on-disk state.
    remote_state
        .release
        .forget_permits(remote_state.release.available_permits());
    remote_state.mode.store(2, Ordering::SeqCst);
    let interrupted = submit_high(&base, &provider, "tv").await;
    let runtime = commands::start(db.clone(), client.clone()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), remote_state.started.notified())
        .await
        .unwrap();
    assert_eq!(
        request(
            &base,
            "GET",
            &format!("/api/v1/commands/{}", interrupted["id"].as_str().unwrap()),
            Value::Null
        )
        .await
        .1["status"],
        "running"
    );
    runtime.shutdown().await;
    stop(server).await;
    drop(client);
    drop(db);
    remote_state.mode.store(0, Ordering::SeqCst);
    remote_state.release.add_permits(1);
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let (app, client) = providers::router_with_refresh(db.clone(), None);
    let (base, server) = serve(app.merge(commands::router(db.clone()))).await;
    let runtime = commands::start(db.clone(), client.clone()).await.unwrap();
    let done = until(&base, interrupted["id"].as_str().unwrap(), "succeeded").await;
    assert_eq!(done["attempts"], 2);
    runtime.shutdown().await;
    // Schedule last_run_at records admission, not completion. Capture history first,
    // then wait for the actual new command in each domain before stopping the worker.
    let (code, prior_history) =
        request(&base, "GET", "/api/v1/commands?limit=100", Value::Null).await;
    assert_eq!(code, 200, "{prior_history}");
    let prior_ids: Vec<Value> = prior_history["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["id"].clone())
        .collect();
    assert_eq!(
        prior_history["total"].as_u64().unwrap(),
        prior_ids.len() as u64,
        "capture every pre-schedule ID"
    );
    for media in ["tv", "movies"] {
        enable_observation(&base, &provider, media).await;
    }
    let runtime = commands::start(db.clone(), client.clone()).await.unwrap();
    let mut scheduled_ids = std::collections::BTreeMap::<String, Value>::new();
    let mut last_history = Value::Null;
    let completion = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let (code, history) =
                request(&base, "GET", "/api/v1/commands?limit=100", Value::Null).await;
            last_history = history;
            if code != 200 {
                return false;
            }
            let items = last_history["items"].as_array().unwrap();
            if last_history["total"].as_u64().unwrap() != items.len() as u64 {
                return false;
            }
            for command in items.iter().filter(|v| !prior_ids.contains(&v["id"])) {
                let Some(media) = command["target"]["media_type"].as_str() else {
                    return false;
                };
                if !matches!(media, "tv" | "movies")
                    || command["target"]["provider_id"] != provider["id"]
                    || command["provider_revision"] != provider["revision"]
                {
                    return false;
                }
                if let Some(previous) =
                    scheduled_ids.insert(media.to_string(), command["id"].clone())
                {
                    if previous != command["id"] {
                        return false;
                    }
                }
                if matches!(command["status"].as_str(), Some("failed" | "cancelled")) {
                    return false;
                }
            }
            if scheduled_ids.len() == 2
                && scheduled_ids.values().all(|id| {
                    items
                        .iter()
                        .any(|v| v["id"] == *id && v["status"] == "succeeded")
                })
            {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await;
    runtime.shutdown().await;
    if !matches!(completion, Ok(true)) {
        stop(server).await;
        stop(upstream).await;
        panic!(
            "scheduled TV/movie commands did not both succeed: {last_history}; captured={scheduled_ids:?}; outcome={completion:?}"
        );
    }
    let (code, schedules) = request(
        &base,
        "GET",
        "/api/v1/download-refresh/schedules",
        Value::Null,
    )
    .await;
    assert_eq!(code, 200, "{schedules}");
    assert_eq!(schedules.as_array().unwrap().len(), 2);
    for schedule in schedules.as_array().unwrap() {
        assert!(!schedule["last_run_at"].is_null(), "{schedules}");
    }
    let (_, history) = request(&base, "GET", "/api/v1/commands", Value::Null).await;
    let total = history["total"].clone();
    let runtime = commands::start(db.clone(), client.clone()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    runtime.shutdown().await;
    assert_eq!(
        request(&base, "GET", "/api/v1/commands", Value::Null)
            .await
            .1["total"],
        total
    );
    // Provider edit invalidates snapshots and fences queued old revisions. CDH preserves
    // explicit observation intent and reauthorizes schedules for the current provider.
    // Shutdown cannot reset attempts on recovered work. Prove this is fresh admission
    // before expecting provider revision fencing to fail it without a first attempt.
    let (code, before_stale) =
        request(&base, "GET", "/api/v1/commands?limit=100", Value::Null).await;
    assert_eq!(code, 200, "{before_stale}");
    let previous = before_stale["items"].as_array().unwrap();
    assert_eq!(
        before_stale["total"].as_u64().unwrap(),
        previous.len() as u64
    );
    assert!(
        !previous
            .iter()
            .any(|v| v["target"]["provider_id"] == provider["id"]
                && v["target"]["media_type"] == "tv"
                && matches!(
                    v["status"].as_str(),
                    Some("queued" | "running" | "retry_wait")
                )),
        "{before_stale}"
    );
    let stale = submit(&base, &provider, "tv").await;
    assert!(!previous.iter().any(|v| v["id"] == stale["id"]), "{stale}");
    assert_eq!(stale["status"], "queued", "{stale}");
    assert_eq!(stale["attempts"], 0, "{stale}");
    let mut changed = config;
    changed["revision"] = json!(1);
    changed["name"] = json!("edited");
    changed["settings"]["endpoint"] = json!(format!("{endpoint}/config-2/"));
    let (code, new_provider) = request(
        &base,
        "PUT",
        &format!("/api/v1/providers/{}", provider["id"].as_str().unwrap()),
        changed,
    )
    .await;
    assert_eq!(code, 200, "{new_provider}");
    let (_, schedules) = request(
        &base,
        "GET",
        "/api/v1/download-refresh/schedules",
        Value::Null,
    )
    .await;
    assert_eq!(schedules.as_array().unwrap().len(), 2);
    for s in schedules.as_array().unwrap() {
        assert_eq!(s["enabled"], true);
        assert_eq!(s["intent"], "explicit");
        assert_eq!(s["requested_enabled"], true);
        assert_eq!(s["provider_revision"], new_provider["revision"]);
        assert!(s["error_code"].is_null());
    }
    assert_eq!(
        request(
            &base,
            "GET",
            &format!(
                "/api/v1/queue?provider_id={}&media_type=tv",
                provider["id"].as_str().unwrap()
            ),
            Value::Null
        )
        .await
        .0,
        404
    );
    // Distinct endpoint paths identify captured old-revision requests. New health may
    // fetch items on config-2; the obsolete command must never fetch config-1 again.
    suppress_observation(&base, &new_provider).await;
    let calls = remote_state.refresh_reads_for("/config-1/");
    let runtime = commands::start(db.clone(), client.clone()).await.unwrap();
    let failed = until(&base, stale["id"].as_str().unwrap(), "failed").await;
    assert_eq!(failed["error_code"], "provider_changed");
    assert_eq!(failed["attempts"], 0);
    runtime.shutdown().await;
    assert_eq!(remote_state.refresh_reads_for("/config-1/"), calls);
    assert_eq!(
        request(
            &base,
            "DELETE",
            &format!("/api/v1/commands/{}", stale["id"].as_str().unwrap()),
            Value::Null
        )
        .await
        .0,
        204
    );
    stop(server).await;
    drop(client);
    drop(db);
    stop(upstream).await;
}

#[tokio::test]
async fn priority_shared_transport_future_retries_and_history_admission_are_bounded() {
    let scratch = Scratch::new();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let state = Arc::new(Remote::default());
    let (endpoint, upstream) = serve(
        axum::Router::new()
            .fallback(remote)
            .with_state(state.clone()),
    )
    .await;
    let (app, client) = providers::router_with_refresh(db.clone(), None);
    // Only this fixture traces owned command reads. Sequential polling correlates
    // method/path with the phase IDs below; no request headers or budgets change.
    let trace_origin = std::time::Instant::now();
    let app = app.merge(commands::router(db.clone())).layer(axum::middleware::from_fn(
        move |request: axum::extract::Request, next: axum::middleware::Next| async move {
            let trace = request.method() == Method::GET
                && request.uri().path().starts_with("/api/v1/commands/");
            let path = request.uri().path().to_owned();
            let arrived = std::time::Instant::now();
            if trace {
                eprintln!("event=owned_command_read_arrived path={path} since_start={:?}", trace_origin.elapsed());
            }
            let response = next.run(request).await;
            if trace {
                eprintln!("event=owned_command_read_completed path={path} status={} elapsed={:?} since_start={:?}", response.status(), arrived.elapsed(), trace_origin.elapsed());
            }
            response
        },
    ));
    let (base, server) = serve(app).await;
    let scope =
        |c| json!({"category":c,"imported_category":null,"recent_priority":0,"older_priority":1});
    let config = json!({"name":"shared","enabled":true,"priority":1,"settings":{"implementation":"qbittorrent","endpoint":format!("{endpoint}/config-1/"),"tv":scope("tv"),"movies":scope("movies")},"credentials":null});
    let (code, provider) = request(&base, "POST", "/api/v1/providers", config.clone()).await;
    assert_eq!(code, 201);
    suppress_observation(&base, &provider).await;
    let tv = submit(&base, &provider, "tv").await;
    let mut high = input(&provider, "movies");
    high["priority"] = json!("high");
    let (code, movie) = request(&base, "POST", "/api/v1/commands", high).await;
    assert_eq!(code, 202);
    state.mode.store(2, Ordering::SeqCst);
    let runtime = commands::start(db.clone(), client.clone()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), state.started.notified())
        .await
        .unwrap();
    assert_eq!(
        request(
            &base,
            "GET",
            &format!("/api/v1/commands/{}", movie["id"].as_str().unwrap()),
            Value::Null
        )
        .await
        .1["status"],
        "running",
        "high priority claims before earlier normal command"
    );
    let (code, busy) = request(
        &base,
        "POST",
        &format!(
            "/api/v1/providers/{}/downloads",
            provider["id"].as_str().unwrap()
        ),
        json!({"domain":"tv","offset":0,"limit":10,"imported":false}),
    )
    .await;
    assert_eq!(code, 429, "{busy}");
    request(
        &base,
        "POST",
        &format!("/api/v1/commands/{}/cancel", movie["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    state.mode.store(0, Ordering::SeqCst);
    state.release.add_permits(1);
    eprintln!(
        "event=owned_command_wait phase=initial_normal id={}",
        tv["id"]
    );
    until(&base, tv["id"].as_str().unwrap(), "succeeded").await;
    runtime.shutdown().await;
    // Synthetic persisted Retry-After boundary: a future obsolete retry must not reserve its scope for a day.
    let stale = submit(&base, &provider, "tv").await;
    let conn = db.connect().await.unwrap();
    conn.execute(
        "UPDATE commands SET status='running',attempts=1,started_at=1 WHERE id=?",
        [stale["id"].as_str().unwrap()],
    )
    .await
    .unwrap();
    conn.execute("UPDATE commands SET status='retry_wait',next_attempt_at=9007199254740000,error_code='refresh_failed' WHERE id=?",[stale["id"].as_str().unwrap()]).await.unwrap();
    let mut change = config.clone();
    change["revision"] = json!(1);
    change["name"] = json!("new revision");
    change["settings"]["endpoint"] = json!(format!("{endpoint}/config-2/"));
    let (code, provider) = request(
        &base,
        "PUT",
        &format!("/api/v1/providers/{}", provider["id"].as_str().unwrap()),
        change,
    )
    .await;
    assert_eq!(code, 200);
    // New-revision health GetItems is legitimate; only the captured old endpoint
    // must remain unread by the obsolete retry.
    let calls = state.refresh_reads_for("/config-1/");
    let runtime = commands::start(db.clone(), client.clone()).await.unwrap();
    let failed = until(&base, stale["id"].as_str().unwrap(), "failed").await;
    assert_eq!(failed["attempts"], 1);
    assert_eq!(failed["error_code"], "provider_changed");
    assert_eq!(state.refresh_reads_for("/config-1/"), calls);
    let corrected = submit(&base, &provider, "tv").await;
    eprintln!(
        "event=owned_command_wait phase=corrected_revision id={}",
        corrected["id"]
    );
    until(&base, corrected["id"].as_str().unwrap(), "succeeded").await;
    runtime.shutdown().await;
    // Restart cannot reset a consumed attempt budget, even when the third attempt was interrupted.
    let exhausted = submit(&base, &provider, "tv").await;
    for attempt in 1..=3 {
        conn.execute("UPDATE commands SET status='running',attempts=?,started_at=1,error_code=NULL WHERE id=?",libsql::params![attempt,exhausted["id"].as_str().unwrap()]).await.unwrap();
        if attempt < 3 {
            conn.execute(
                "UPDATE commands SET status='retry_wait',error_code='refresh_failed' WHERE id=?",
                [exhausted["id"].as_str().unwrap()],
            )
            .await
            .unwrap();
        }
    }
    // Keep the same valid revision: attempt exhaustion, not provider invalidation,
    // must prevent another refresh. Startup health legitimately reads one item page
    // per configured domain; wait for that exact batch before checking traffic below.
    let calls = state.refresh_reads_for("/config-2/");
    let runtime = commands::start(db.clone(), client.clone()).await.unwrap();
    let failed = until(&base, exhausted["id"].as_str().unwrap(), "failed").await;
    assert_eq!(failed["attempts"], 3);
    assert_eq!(failed["error_code"], "interrupted");
    // Let the actual startup-health batch finish before constructing the full-pool case.
    // Otherwise an already-admitted health command could legitimately fetch items even
    // though capacity correctly rejects every NEW refresh/health admission below.
    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            let current = conn.query("SELECT count(*) FROM health_checks h JOIN health_lifecycle l ON l.id=1 WHERE h.observed_epoch=l.epoch AND h.observed_generation=h.generation AND h.last_error IS NULL AND h.pending_reasons=0",()).await.unwrap().next().await.unwrap().unwrap().get::<i64>(0).unwrap();
            // Migration46 adds two removed-metadata checks; startup must settle all eight.
            if current == 8 { break; }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }).await.unwrap();
    runtime.shutdown().await;
    assert_eq!(
        state.refresh_reads_for("/config-2/"),
        calls + 2,
        "exactly one communication-health page per domain; exhausted refresh adds none"
    );
    assert_eq!(failed["id"], exhausted["id"]);
    assert_eq!(
        request(
            &base,
            "GET",
            &format!(
                "/api/v1/queue?provider_id={}&media_type=tv",
                provider["id"].as_str().unwrap()
            ),
            Value::Null
        )
        .await
        .1["command_id"],
        corrected["id"],
        "health and exhausted recovery must preserve the previous snapshot owner"
    );
    // Fill explicitly retained history without network calls; no implicit pruning or ignored scheduler failure.
    enable_observation(&base, &provider, "movies").await;
    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await
        .unwrap();
    let count = tx
        .query("SELECT count(*) FROM commands", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<i64>(0)
        .unwrap();
    for _ in count..1024 {
        let id = uuid::Uuid::new_v4().to_string();
        tx.execute("INSERT INTO commands(id,provider_id,media_type,provider_revision,next_attempt_at,created_at) VALUES(?,?,'tv',?,0,0)",libsql::params![id.clone(),provider["id"].as_str().unwrap(),provider["revision"].as_i64().unwrap()]).await.unwrap();
        tx.execute(
            "UPDATE commands SET status='cancelled',completed_at=0 WHERE id=?",
            [id],
        )
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
    // Every row above is terminal (migration 0033: only active queued/running/retry_wait rows
    // occupy the shared pool), so it is not actually full yet. Pad it with genuinely active
    // search_commands rows (no per-target uniqueness, unlike `commands` itself) against a minimal
    // fixture episode, using a far-future next_attempt_at so neither live worker below claims them.
    // Count every command kind in the same 1024-slot pool; health has settled above.
    const POOL_ACTIVE_SQL: &str = "(SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))";
    conn.execute_batch("INSERT INTO series(id,tvdb_id,title,path) VALUES(1,101,'Pad','/pad');INSERT INTO seasons VALUES(1,1,1);INSERT INTO episodes(id,series_id,season,number,title) VALUES(1,1,1,1,'One');").await.unwrap();
    let search_indexer_id = uuid::Uuid::new_v4().to_string();
    conn.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'torznab','Padding Indexer',1,1,1,1,'http://127.0.0.1:1/')",[search_indexer_id.clone()]).await.unwrap();
    conn.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year) VALUES(?,'torznab','tv','[5000]','[]',0,NULL)",[search_indexer_id.clone()]).await.unwrap();
    let captured = json!({"media_type":"tv","series_id":1,"episode_id":1,"tvdb_id":101,"title":"Padding","season":1,"number":1,"series_type":"standard","use_scene_numbering":false});
    let active: i64 = conn
        .query(&format!("SELECT {POOL_ACTIVE_SQL}"), ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    let mut padding = Vec::new();
    for _ in 0..1024 - active {
        let id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO search_commands(id,mode,media_type,requested_episode_id,captured_target_json,indexer_id,indexer_revision,client_id,client_revision,next_attempt_at,created_at) VALUES(?,'automatic','tv',1,?,?,1,?,?,9007199254740000,0)",
            libsql::params![
                id.clone(),
                captured.to_string(),
                search_indexer_id.clone(),
                provider["id"].as_str().unwrap(),
                provider["revision"].as_i64().unwrap()
            ],
        )
        .await
        .unwrap();
        padding.push(id);
    }
    assert_eq!(
        conn.query(&format!("SELECT {POOL_ACTIVE_SQL}"), ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<i64>(0)
            .unwrap(),
        1024
    );
    assert_eq!(
        request(&base, "POST", "/api/v1/commands", input(&provider, "tv"))
            .await
            .0,
        429
    );
    // All slots now belong to future-due padding, so both new refresh and new health
    // admission are rejected. With no previously active probe, zero item reads is exact.
    let calls = state.refresh_reads();
    let runtime = commands::start(db.clone(), client.clone()).await.unwrap();
    let full = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (_, v) = request(
                &base,
                "GET",
                "/api/v1/download-refresh/schedules",
                Value::Null,
            )
            .await;
            if v[0]["error_code"] == "command_history_full" {
                break v;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert!(full[0]["last_run_at"].is_null());
    assert!(full[0]["next_run_at"].as_i64().unwrap() > 0);
    runtime.shutdown().await;
    assert_eq!(state.refresh_reads(), calls);
    assert_eq!(
        request(
            &base,
            "DELETE",
            &format!("/api/v1/commands/{}", exhausted["id"].as_str().unwrap()),
            Value::Null
        )
        .await
        .0,
        204
    );
    // The DELETE above only removed an already-terminal row, so it did not free the shared pool
    // (migration 0033): admission is still rejected.
    assert_eq!(
        request(&base, "POST", "/api/v1/commands", input(&provider, "tv"))
            .await
            .0,
        429
    );
    // Cancelling one active padding row instead so the scheduler's retry below can actually claim
    // a slot.
    conn.execute(
        "UPDATE search_commands SET status='cancelled',completed_at=0 WHERE id=?",
        [padding[0].clone()],
    )
    .await
    .unwrap();
    conn.execute("UPDATE download_refresh_schedules SET next_run_at=0", ())
        .await
        .unwrap();
    let runtime = commands::start(db.clone(), client.clone()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (_, v) = request(
                &base,
                "GET",
                "/api/v1/download-refresh/schedules",
                Value::Null,
            )
            .await;
            if v[0]["error_code"].is_null() && !v[0]["last_run_at"].is_null() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    runtime.shutdown().await;
    let (_, schedules) = request(
        &base,
        "GET",
        "/api/v1/download-refresh/schedules",
        Value::Null,
    )
    .await;
    assert_eq!(
        request(
            &base,
            "DELETE",
            "/api/v1/download-refresh/schedules",
            json!({"target":schedules[0]["target"],"revision":schedules[0]["revision"]})
        )
        .await
        .0,
        204
    );
    stop(server).await;
    drop(conn);
    drop(client);
    drop(db);
    stop(upstream).await;
}

#[tokio::test]
async fn provider_edits_during_successful_and_failed_reads_invalidate_both_domains() {
    let scratch = Scratch::new();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let state = Arc::new(Remote::default());
    let (endpoint, upstream) = serve(
        axum::Router::new()
            .fallback(remote)
            .with_state(state.clone()),
    )
    .await;
    let (app, client) = providers::router_with_refresh(db.clone(), None);
    let (base, server) = serve(app.merge(commands::router(db.clone()))).await;
    let scope = |category| json!({"category": category, "imported_category": null, "recent_priority": 0, "older_priority": 1});
    let config = json!({"name": "shared", "enabled": true, "priority": 1, "settings": {"implementation": "qbittorrent", "endpoint": format!("{endpoint}/config-1/"), "tv": scope("tv"), "movies": scope("movies")}, "credentials": null});
    let (code, mut provider) = request(&base, "POST", "/api/v1/providers", config.clone()).await;
    assert_eq!(code, 201);
    suppress_observation(&base, &provider).await;
    for media in ["tv", "movies"] {
        for mode in [2, 3] {
            state
                .release
                .forget_permits(state.release.available_permits());
            let old_prefix = format!("/config-{}/", provider["revision"].as_i64().unwrap());
            *state.blocked_prefix.lock().unwrap() = Some(old_prefix.clone());
            state.mode.store(mode, Ordering::SeqCst);
            let old_calls = state.refresh_reads_for(&old_prefix);
            // An explicit high-priority command runs before the normal startup-health batch,
            // so this fixture's started notification cannot belong to health instead.
            let mut command_input = input(&provider, media);
            command_input["priority"] = json!("high");
            let (code, command) = request(&base, "POST", "/api/v1/commands", command_input).await;
            assert_eq!(code, 202, "{command}");
            let id = command["id"].as_str().unwrap();
            let runtime = commands::start(db.clone(), client.clone()).await.unwrap();
            tokio::time::timeout(Duration::from_secs(5), state.started.notified())
                .await
                .unwrap();
            assert_eq!(
                request(&base, "GET", &format!("/api/v1/commands/{id}"), Value::Null)
                    .await
                    .1["status"],
                "running"
            );
            let mut edited = config.clone();
            edited["revision"] = provider["revision"].clone();
            edited["name"] = json!(format!("edited {media} {mode}"));
            let new_prefix = format!("/config-{}/", provider["revision"].as_i64().unwrap() + 1);
            edited["settings"]["endpoint"] = json!(format!("{endpoint}{new_prefix}"));
            let (code, updated) = request(
                &base,
                "PUT",
                &format!("/api/v1/providers/{}", provider["id"].as_str().unwrap()),
                edited,
            )
            .await;
            assert_eq!(code, 200, "{updated}");
            provider = updated;
            state.release.add_permits(1);
            let failed = until(&base, id, "failed").await;
            // Configuration changes win over both a successful stale page and a retryable remote
            // failure: neither outcome may publish a snapshot or schedule another old-revision read.
            assert_eq!(
                failed["error_code"], "provider_changed",
                "{media} mode {mode}"
            );
            assert_eq!(failed["attempts"], 1);
            assert_eq!(failed["items_observed"], 0);
            assert!(!failed["completed_at"].is_null());
            assert_eq!(
                request(
                    &base,
                    "GET",
                    &format!(
                        "/api/v1/queue?provider_id={}&media_type={media}",
                        provider["id"].as_str().unwrap()
                    ),
                    Value::Null
                )
                .await
                .0,
                404
            );
            // The old refresh owns config-N's endpoint; new health probes use config-(N+1).
            // Wait for the actual new-revision health batch, rather than counting its legal
            // GetItems requests as retries or relying on an arbitrary sleep/count allowance.
            assert_eq!(state.refresh_reads_for(&old_prefix), old_calls + 1);
            let conn = db.connect().await.unwrap();
            tokio::time::timeout(Duration::from_secs(12), async {
                loop {
                    let current = conn.query("SELECT count(*) FROM health_checks h JOIN health_lifecycle l ON l.id=1 WHERE h.observed_epoch=l.epoch AND h.observed_generation=h.generation AND h.last_error IS NULL AND h.pending_reasons=0",()).await.unwrap().next().await.unwrap().unwrap().get::<i64>(0).unwrap();
                    // Migration46 adds two local removed-metadata checks; all eight must settle
                    // without changing the exact communication traffic assertion below.
                    if current == 8 && state.refresh_reads_for(&new_prefix) >= 2 { break; }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            }).await.unwrap();
            assert_eq!(
                request(&base, "GET", &format!("/api/v1/commands/{id}"), Value::Null)
                    .await
                    .1["status"],
                "failed"
            );
            assert_eq!(
                state.refresh_reads_for(&old_prefix),
                old_calls + 1,
                "old revision must never reread"
            );
            assert_eq!(
                conn.query(
                    "SELECT count(*) FROM download_refresh_snapshots WHERE provider_id=?",
                    [provider["id"].as_str().unwrap()]
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
                "neither stale refresh nor observation-only health may publish a snapshot"
            );
            runtime.shutdown().await;
        }
    }
    stop(server).await;
    drop(client);
    drop(db);
    stop(upstream).await;
}

// Executed only by the ownership regression below, in a separate OS process.
#[tokio::test]
async fn command_owner_child() {
    let Ok(path) = std::env::var("HRRDARR_COMMAND_OWNER_DB") else {
        return;
    };
    let ready = std::env::var("HRRDARR_COMMAND_OWNER_READY").unwrap();
    if std::env::var_os("HRRDARR_COMMAND_OWNER_REJECT").is_some() {
        let error = match Database::open_local(path).await {
            Ok(_) => panic!("competing process acquired the database"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("already owned or cannot be locked")
        );
        std::fs::write(ready, "rejected").unwrap();
        return;
    }
    let db = Arc::new(Database::open_local(path).await.unwrap());
    let (app, client) = providers::router_with_refresh(db.clone(), None);
    let (base, _server) = serve(app.merge(commands::router(db.clone()))).await;
    let _runtime = commands::start(db, client).await.unwrap();
    std::fs::write(ready, base).unwrap();
    std::future::pending::<()>().await;
}

fn owner_child(
    db: &std::path::Path,
    ready: &std::path::Path,
    reject: bool,
) -> tokio::process::Child {
    let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "command_owner_child", "--nocapture"])
        .env("HRRDARR_COMMAND_OWNER_DB", db)
        .env("HRRDARR_COMMAND_OWNER_READY", ready)
        .kill_on_drop(true);
    if reject {
        command.env("HRRDARR_COMMAND_OWNER_REJECT", "1");
    }
    command.spawn().unwrap()
}

async fn child_ready(path: &std::path::Path) -> String {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(value) = tokio::fs::read_to_string(path).await {
                if !value.is_empty() {
                    return value;
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("child did not become ready")
}

#[tokio::test]
async fn process_ownership_rejects_competitor_and_recovers_killed_reads_in_both_domains() {
    for media in ["tv", "movies"] {
        let scratch = Scratch::new();
        let path = scratch.0.join("db");
        let state = Arc::new(Remote::default());
        state.mode.store(2, Ordering::SeqCst);
        let (endpoint, upstream) = serve(
            axum::Router::new()
                .fallback(remote)
                .with_state(state.clone()),
        )
        .await;
        let db = Arc::new(Database::open_local(&path).await.unwrap());
        let (app, _) = providers::router_with_refresh(db.clone(), None);
        let (base, setup) = serve(app.merge(commands::router(db.clone()))).await;
        let scope = |category| json!({"category":category,"imported_category":null,"recent_priority":0,"older_priority":1});
        let (code, provider) = request(&base, "POST", "/api/v1/providers", json!({
            "name":"owned-process", "enabled":true,"priority":1,
            "settings":{"implementation":"qbittorrent","endpoint":endpoint,"tv":scope("tv"),"movies":scope("movies")},"credentials":null
        })).await;
        assert_eq!(code, 201, "{provider}");
        suppress_observation(&base, &provider).await;
        let command = submit_high(&base, &provider, media).await;
        let id = command["id"].as_str().unwrap();
        stop(setup).await;
        drop(db);

        let ready = scratch.0.join("first-ready");
        let mut first = owner_child(&path, &ready, false);
        let base = child_ready(&ready).await;
        tokio::time::timeout(Duration::from_secs(12), state.started.notified())
            .await
            .unwrap();
        let running = until(&base, id, "running").await;
        assert_eq!(running["attempts"], 1);
        assert_eq!(running["target"], command["target"]);
        // High priority plus the actual handler handshake identifies the sole worker's
        // refresh request; no startup-health request can stand in for this blocked read.
        let before = state.calls.lock().unwrap().len();
        assert_eq!(state.refresh_reads(), 1);

        let rejected = scratch.0.join("rejected");
        let mut competitor = owner_child(&path, &rejected, true);
        assert_eq!(child_ready(&rejected).await, "rejected");
        assert!(
            tokio::time::timeout(Duration::from_secs(5), competitor.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
        // Rejection must precede startup recovery: it cannot claim/reset the live owner's row.
        let unchanged = until(&base, id, "running").await;
        assert_eq!(unchanged["attempts"], 1);
        assert_eq!(state.calls.lock().unwrap().len(), before);
        assert_eq!(
            state.refresh_reads(),
            1,
            "rejected process must not dispatch a second items read"
        );

        // Child::kill terminates without Runtime::shutdown or Database destructors.
        first.kill().await.unwrap();
        assert!(!first.wait().await.unwrap().success());
        state.mode.store(0, Ordering::SeqCst);
        state.release.add_permits(1);
        let reopened = scratch.0.join("second-ready");
        let mut second = owner_child(&path, &reopened, false);
        let base = child_ready(&reopened).await;
        let completed = until(&base, id, "succeeded").await;
        // Exact identity and count prove real persisted recovery, not replacement enqueue.
        assert_eq!(completed["id"], command["id"]);
        assert_eq!(completed["target"], command["target"]);
        assert_eq!(completed["attempts"], 2);
        let (code, queue) = request(
            &base,
            "GET",
            &format!(
                "/api/v1/queue?provider_id={}&media_type={media}",
                provider["id"].as_str().unwrap()
            ),
            Value::Null,
        )
        .await;
        assert_eq!(code, 200, "{queue}");
        assert_eq!(queue["command_id"], command["id"]);
        assert_eq!(queue["target"], command["target"]);
        let peer = if media == "tv" { "movies" } else { "tv" };
        // Shared provider identity must not publish the observation into its other scope.
        assert_eq!(
            request(
                &base,
                "GET",
                &format!(
                    "/api/v1/queue?provider_id={}&media_type={peer}",
                    provider["id"].as_str().unwrap()
                ),
                Value::Null
            )
            .await
            .0,
            404
        );
        second.kill().await.unwrap();
        second.wait().await.unwrap();
        stop(upstream).await;
    }
}
