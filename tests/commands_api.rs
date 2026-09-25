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
    started: tokio::sync::Notify,
    release: tokio::sync::Semaphore,
}
impl Default for Remote {
    fn default() -> Self {
        Self {
            mode: AtomicU8::new(0),
            calls: Mutex::new(vec![]),
            started: tokio::sync::Notify::new(),
            release: tokio::sync::Semaphore::new(0),
        }
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
    if uri.path().ends_with("torrents/info") {
        match s.mode.load(Ordering::SeqCst) {
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
    let response = client
        .request(method.parse().unwrap(), format!("{base}{path}"))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    let text = response.text().await.unwrap();
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
    let config = json!({"name":"shared","enabled":true,"priority":1,"settings":{"implementation":"qbittorrent","endpoint":endpoint,"tv":scope("tv"),"movies":scope("movies")},"credentials":null});
    let (code, provider) = request(&base, "POST", "/api/v1/providers", config.clone()).await;
    assert_eq!(code, 201, "{provider}");
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
    let cancelled = submit(&base, &provider, "movies").await;
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
    let interrupted = submit(&base, &provider, "tv").await;
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
    // Explicit schedules survive restart, enqueue both scopes once, and do not catch up missed ticks.
    for media in ["tv", "movies"] {
        let(code,schedule)=request(&base,"PUT","/api/v1/download-refresh/schedules",json!({"target":{"provider_id":provider["id"],"media_type":media},"revision":null,"provider_revision":1,"enabled":true,"interval_seconds":60})).await;
        assert_eq!(code, 200, "{schedule}");
    }
    let runtime = commands::start(db.clone(), client.clone()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let (_, v) = request(
                &base,
                "GET",
                "/api/v1/download-refresh/schedules",
                Value::Null,
            )
            .await;
            if v.as_array()
                .unwrap()
                .iter()
                .all(|s| !s["last_run_at"].is_null())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    runtime.shutdown().await;
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
    // Provider edit invalidates snapshots and disables preserved schedules; queued old revision fails.
    let stale = submit(&base, &provider, "tv").await;
    let mut changed = config;
    changed["revision"] = json!(1);
    changed["name"] = json!("edited");
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
        assert_eq!(s["enabled"], false);
        assert_eq!(s["error_code"], "provider_changed");
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
    let calls = remote_state.calls.lock().unwrap().len();
    let runtime = commands::start(db.clone(), client.clone()).await.unwrap();
    let failed = until(&base, stale["id"].as_str().unwrap(), "failed").await;
    assert_eq!(failed["error_code"], "provider_changed");
    assert_eq!(failed["attempts"], 0);
    runtime.shutdown().await;
    assert_eq!(remote_state.calls.lock().unwrap().len(), calls);
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
    let (base, server) = serve(app.merge(commands::router(db.clone()))).await;
    let scope =
        |c| json!({"category":c,"imported_category":null,"recent_priority":0,"older_priority":1});
    let config = json!({"name":"shared","enabled":true,"priority":1,"settings":{"implementation":"qbittorrent","endpoint":endpoint,"tv":scope("tv"),"movies":scope("movies")},"credentials":null});
    let (code, provider) = request(&base, "POST", "/api/v1/providers", config.clone()).await;
    assert_eq!(code, 201);
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
    let mut change = config;
    change["revision"] = json!(1);
    change["name"] = json!("new revision");
    let (code, provider) = request(
        &base,
        "PUT",
        &format!("/api/v1/providers/{}", provider["id"].as_str().unwrap()),
        change,
    )
    .await;
    assert_eq!(code, 200);
    let calls = state.calls.lock().unwrap().len();
    let runtime = commands::start(db.clone(), client.clone()).await.unwrap();
    let failed = until(&base, stale["id"].as_str().unwrap(), "failed").await;
    assert_eq!(failed["attempts"], 1);
    assert_eq!(failed["error_code"], "provider_changed");
    assert_eq!(state.calls.lock().unwrap().len(), calls);
    let corrected = submit(&base, &provider, "tv").await;
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
    let calls = state.calls.lock().unwrap().len();
    let runtime = commands::start(db.clone(), client.clone()).await.unwrap();
    let failed = until(&base, exhausted["id"].as_str().unwrap(), "failed").await;
    assert_eq!(failed["attempts"], 3);
    assert_eq!(failed["error_code"], "interrupted");
    runtime.shutdown().await;
    assert_eq!(state.calls.lock().unwrap().len(), calls);
    // Fill explicitly retained history without network calls; no implicit pruning or ignored scheduler failure.
    let(code,_)=request(&base,"PUT","/api/v1/download-refresh/schedules",json!({"target":{"provider_id":provider["id"],"media_type":"movies"},"revision":null,"provider_revision":provider["revision"],"enabled":true,"interval_seconds":60})).await;
    assert_eq!(code, 200);
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
    assert_eq!(
        request(&base, "POST", "/api/v1/commands", input(&provider, "tv"))
            .await
            .0,
        429
    );
    let calls = state.calls.lock().unwrap().len();
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
    assert_eq!(state.calls.lock().unwrap().len(), calls);
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
    let config = json!({"name": "shared", "enabled": true, "priority": 1, "settings": {"implementation": "qbittorrent", "endpoint": endpoint, "tv": scope("tv"), "movies": scope("movies")}, "credentials": null});
    let (code, mut provider) = request(&base, "POST", "/api/v1/providers", config.clone()).await;
    assert_eq!(code, 201);
    for media in ["tv", "movies"] {
        for mode in [2, 3] {
            state
                .release
                .forget_permits(state.release.available_permits());
            state.mode.store(mode, Ordering::SeqCst);
            let command = submit(&base, &provider, media).await;
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
            let calls = state.calls.lock().unwrap().len();
            tokio::time::sleep(Duration::from_millis(1100)).await;
            assert_eq!(
                request(&base, "GET", &format!("/api/v1/commands/{id}"), Value::Null)
                    .await
                    .1["status"],
                "failed"
            );
            assert_eq!(state.calls.lock().unwrap().len(), calls);
            runtime.shutdown().await;
        }
    }
    stop(server).await;
    drop(client);
    drop(db);
    stop(upstream).await;
}
