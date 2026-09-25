use axum::{
    extract::{OriginalUri, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
};
use hrrdarr::{commands, db::Database, library, metadata::MetadataClient, providers};
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
        let p = std::env::temp_dir().join(format!(
            "hrrdarr-metadata-commands-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Remote {
    mode: AtomicU8,
    version: AtomicU8,
    calls: Mutex<Vec<String>>,
    started: tokio::sync::Notify,
    release: tokio::sync::Semaphore,
}
impl Default for Remote {
    fn default() -> Self {
        Self {
            mode: AtomicU8::new(0),
            version: AtomicU8::new(0),
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
    assert_eq!(
        method,
        Method::GET,
        "refresh must only read external services"
    );
    s.calls.lock().unwrap().push(uri.to_string());
    if uri.path().ends_with("webapiVersion") {
        return "2.8.3".into_response();
    }
    if uri.path().ends_with("torrents/info") {
        return axum::Json(json!([])).into_response();
    }
    let mode = s.mode.load(Ordering::SeqCst);
    if mode == 2 {
        s.started.notify_one();
        let permit = s.release.acquire().await.unwrap();
        permit.forget();
    }
    if mode == 4 {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [("retry-after", "60")],
            "PRIVATE_METADATA_ERROR",
        )
            .into_response();
    }
    if mode == 1 {
        return (StatusCode::SERVICE_UNAVAILABLE, "PRIVATE_METADATA_ERROR").into_response();
    }
    let version = s.version.load(Ordering::SeqCst);
    let id = if mode == 3 { 999 } else { 101 };
    if uri.path().starts_with("/shows/en/") {
        axum::Json(json!({"tvdbId":id,"title":format!("Series {version}"),"firstAired":"2020-01-01","seasons":[{"seasonNumber":1}],"episodes":[{"tvdbId":501,"seasonNumber":1,"episodeNumber":1,"title":format!("Episode {version}"),"airDate":"2020-01-01"}]})).into_response()
    } else if uri.path().starts_with("/movie/") {
        axum::Json(json!({"tmdbId":id,"title":format!("Movie {version}"),"year":2021}))
            .into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
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
    let response = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
        .request(method.parse().unwrap(), format!("{base}{path}"))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    let code = response.status().as_u16();
    let text = response.text().await.unwrap();
    assert!(!text.contains("PRIVATE_METADATA_ERROR"));
    (
        code,
        if code == 204 {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or_else(|_| panic!("{code}: {text}"))
        },
    )
}
async fn stop(task: JoinHandle<()>) {
    task.abort();
    let _ = task.await;
}
async fn app(
    db: Arc<Database>,
    client: Arc<MetadataClient>,
) -> (String, JoinHandle<()>, providers::RefreshClient) {
    let (providers, refresh) = providers::router_with_refresh(db.clone(), None);
    let (base, server) = serve(
        providers
            .merge(commands::router(db.clone()))
            .merge(library::router(db.clone()))
            .merge(library::metadata_router(db, client)),
    )
    .await;
    (base, server, refresh)
}
async fn enqueue(base: &str, target: &Value, priority: &str) -> Value {
    let (code, value) = request(
        base,
        "POST",
        "/api/v1/metadata-refresh/commands",
        json!({"target":target,"priority":priority}),
    )
    .await;
    assert_eq!(code, 202, "{value}");
    value
}
async fn until(base: &str, id: &Value, status: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(16), async {
        loop {
            let (code, value) = request(
                base,
                "GET",
                &format!("/api/v1/metadata-refresh/commands/{}", id.as_str().unwrap()),
                Value::Null,
            )
            .await;
            assert_eq!(code, 200, "{value}");
            if value["status"] == status {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("metadata command {id} did not reach {status}"))
}
async fn title(db: &Database, media: &str) -> String {
    let sql = if media == "tv" {
        "SELECT title FROM series WHERE id=1"
    } else {
        "SELECT title FROM movie_metadata WHERE id=1"
    };
    db.connect()
        .await
        .unwrap()
        .query(sql, ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}
async fn seed(base: &str, scratch: &Scratch) -> [Value; 2] {
    let mut targets = vec![];
    for (route, media, key) in [
        ("tv/series", "tv", "tvdb_id"),
        ("movies", "movies", "tmdb_id"),
    ] {
        let path = scratch.0.join(media);
        std::fs::create_dir(&path).unwrap();
        let mut body = json!({"path":path,"settings":{"monitored":false}});
        body[key] = json!(101);
        let (code, item) = request(base, "POST", &format!("/api/v1/{route}/lookup"), body).await;
        assert_eq!(code, 201, "{item}");
        // Equal library IDs intentionally exercise typed command targets rather than shared integers.
        assert_eq!(item["id"], 1);
        targets.push(if media == "tv" {
            json!({"media_type":"tv","series_id":1})
        } else {
            json!({"media_type":"movies","movie_id":1})
        });
    }
    targets.try_into().unwrap()
}
#[tokio::test]
async fn metadata_commands_share_worker_preserve_targets_and_recover_atomic_updates() {
    let scratch = Scratch::new();
    let path = scratch.0.join("db");
    let state = Arc::new(Remote::default());
    let (endpoint, upstream) = serve(
        axum::Router::new()
            .fallback(remote)
            .with_state(state.clone()),
    )
    .await;
    let client = Arc::new(
        MetadataClient::with_origins(&format!("{endpoint}/"), &format!("{endpoint}/")).unwrap(),
    );
    let db = Arc::new(Database::open_local(&path).await.unwrap());
    let (base, server, refresh) = app(db.clone(), client.clone()).await;
    let targets = seed(&base, &scratch).await;
    let scope =
        json!({"category":"tv","imported_category":null,"recent_priority":0,"older_priority":1});
    let(code,provider)=request(&base,"POST","/api/v1/providers",json!({"name":"shared","enabled":true,"priority":1,"settings":{"implementation":"qbittorrent","endpoint":endpoint,"tv":scope,"movies":null},"credentials":null})).await;
    assert_eq!(code, 201, "{provider}");
    let(code,download)=request(&base,"POST","/api/v1/commands",json!({"name":"refresh_downloads","target":{"provider_id":provider["id"],"media_type":"tv"},"provider_revision":provider["revision"],"priority":"normal"})).await;
    assert_eq!(code, 202, "{download}");
    for input in [
        json!({"target":{"media_type":"tv","movie_id":1},"priority":"normal"}),
        json!({"target":{"media_type":"movies","movie_id":0},"priority":"normal"}),
        json!({"target":targets[0],"priority":"normal","name":"refresh_series"}),
    ] {
        assert_eq!(
            request(&base, "POST", "/api/v1/metadata-refresh/commands", input)
                .await
                .0,
            400
        );
    }
    let tv = enqueue(&base, &targets[0], "high").await;
    let movie = enqueue(&base, &targets[1], "normal").await;
    assert_eq!(enqueue(&base, &targets[0], "normal").await["id"], tv["id"]);
    assert_ne!(tv["id"], movie["id"]);
    assert_eq!(
        request(&base, "GET", "/api/v1/commands", Value::Null)
            .await
            .1["total"],
        1,
        "existing download history excludes metadata commands"
    );
    assert_eq!(
        request(
            &base,
            "DELETE",
            &format!(
                "/api/v1/metadata-refresh/commands/{}",
                tv["id"].as_str().unwrap()
            ),
            Value::Null
        )
        .await
        .0,
        409
    );
    for query in [
        "media_type=movies&series_id=1",
        "series_id=1",
        "media_type=tv&series_id=1&movie_id=1",
        "limit=101",
        "offset=1025",
    ] {
        assert_eq!(
            request(
                &base,
                "GET",
                &format!("/api/v1/metadata-refresh/commands?{query}"),
                Value::Null
            )
            .await
            .0,
            400
        )
    }
    let filtered = request(
        &base,
        "GET",
        "/api/v1/metadata-refresh/commands?media_type=tv&series_id=1",
        Value::Null,
    )
    .await
    .1;
    assert_eq!(filtered["total"], 1);
    assert_eq!(filtered["items"][0]["target"], targets[0]);
    state.version.store(1, Ordering::SeqCst);
    state.calls.lock().unwrap().clear();
    let runtime = commands::start_with_metadata(db.clone(), refresh, client.clone())
        .await
        .unwrap();
    let done = until(&base, &tv["id"], "succeeded").await;
    assert_eq!(done["records_updated"], 2);
    assert_eq!(done["attempts"], 1);
    until(&base, &movie["id"], "succeeded").await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let value = request(
                &base,
                "GET",
                &format!("/api/v1/commands/{}", download["id"].as_str().unwrap()),
                Value::Null,
            )
            .await
            .1;
            if value["status"] == "succeeded" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    // Empty query serialization may append ?; priority concerns the requested resource.
    assert_eq!(
        state.calls.lock().unwrap()[0].split('?').next().unwrap(),
        "/shows/en/101",
        "high priority metadata precedes queued normal download work"
    );
    for (media, target) in [("tv", &targets[0]), ("movies", &targets[1])] {
        assert_eq!(
            title(&db, media).await,
            if media == "tv" { "Series 1" } else { "Movie 1" }
        );
        let noop = enqueue(&base, target, "normal").await;
        assert_eq!(
            until(&base, &noop["id"], "succeeded").await["records_updated"],
            0
        );
    }
    // Fail settlement after writer changes: the same transaction must roll back every fact.
    let c = db.connect().await.unwrap();
    c.execute_batch("CREATE TRIGGER fixture_settlement_failure BEFORE UPDATE ON metadata_refresh_commands WHEN NEW.status='succeeded' BEGIN SELECT RAISE(ABORT,'owned fixture settlement failure'); END;").await.unwrap();
    state.version.store(2, Ordering::SeqCst);
    let retry = enqueue(&base, &targets[0], "normal").await;
    let retrying = until(&base, &retry["id"], "retry_wait").await;
    assert_eq!(retrying["error_code"], "storage_error");
    assert_eq!(title(&db, "tv").await, "Series 1");
    c.execute_batch("DROP TRIGGER fixture_settlement_failure")
        .await
        .unwrap();
    assert_eq!(until(&base, &retry["id"], "succeeded").await["attempts"], 2);
    assert_eq!(title(&db, "tv").await, "Series 2");
    runtime.shutdown().await;
    stop(server).await;
    drop(c);
    drop(db);
    // Both typed targets must survive a complete close/reopen from an actual running HTTP read.
    for (media, target) in [("tv", &targets[0]), ("movies", &targets[1])] {
        let db = Arc::new(Database::open_local(&path).await.unwrap());
        let (base, server, refresh) = app(db.clone(), client.clone()).await;
        let runtime = commands::start_with_metadata(db.clone(), refresh, client.clone())
            .await
            .unwrap();
        state.mode.store(2, Ordering::SeqCst);
        state.version.store(3, Ordering::SeqCst);
        let interrupted = enqueue(&base, target, "normal").await;
        tokio::time::timeout(Duration::from_secs(5), state.started.notified())
            .await
            .unwrap();
        assert_eq!(
            until(&base, &interrupted["id"], "running").await["attempts"],
            1
        );
        runtime.shutdown().await;
        stop(server).await;
        drop(db);
        state.mode.store(0, Ordering::SeqCst);
        let db = Arc::new(Database::open_local(&path).await.unwrap());
        let (base, server, refresh) = app(db.clone(), client.clone()).await;
        let runtime = commands::start_with_metadata(db.clone(), refresh, client.clone())
            .await
            .unwrap();
        let recovered = until(&base, &interrupted["id"], "succeeded").await;
        assert_eq!(recovered["id"], interrupted["id"]);
        assert_eq!(recovered["attempts"], 2);
        assert_eq!(recovered["target"], *target);
        assert_eq!(
            title(&db, media).await,
            if media == "tv" { "Series 3" } else { "Movie 3" }
        );
        assert_eq!(
            db.connect()
                .await
                .unwrap()
                .query(
                    if media == "tv" {
                        "SELECT monitored FROM series WHERE id=1"
                    } else {
                        "SELECT monitored FROM movies WHERE id=1"
                    },
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
            0
        );
        runtime.shutdown().await;
        stop(server).await;
        drop(db);
    }
    stop(upstream).await;
}
#[tokio::test]
async fn cancellation_stale_identity_and_remote_failures_never_publish_partial_metadata() {
    let scratch = Scratch::new();
    let state = Arc::new(Remote::default());
    let (endpoint, upstream) = serve(
        axum::Router::new()
            .fallback(remote)
            .with_state(state.clone()),
    )
    .await;
    let client = Arc::new(
        MetadataClient::with_origins(&format!("{endpoint}/"), &format!("{endpoint}/")).unwrap(),
    );
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let (base, server, refresh) = app(db.clone(), client.clone()).await;
    let targets = seed(&base, &scratch).await;
    let runtime = commands::start_with_metadata(db.clone(), refresh, client)
        .await
        .unwrap();
    state.version.store(1, Ordering::SeqCst);
    for (media, target) in [("tv", &targets[0]), ("movies", &targets[1])] {
        let original = title(&db, media).await;
        state.mode.store(2, Ordering::SeqCst);
        let cancelled = enqueue(&base, target, "normal").await;
        tokio::time::timeout(Duration::from_secs(5), state.started.notified())
            .await
            .unwrap();
        assert_eq!(
            request(
                &base,
                "POST",
                &format!(
                    "/api/v1/metadata-refresh/commands/{}/cancel",
                    cancelled["id"].as_str().unwrap()
                ),
                Value::Null
            )
            .await
            .0,
            200
        );
        assert_eq!(
            request(
                &base,
                "DELETE",
                &format!(
                    "/api/v1/metadata-refresh/commands/{}",
                    cancelled["id"].as_str().unwrap()
                ),
                Value::Null
            )
            .await
            .0,
            204
        );
        // Keep the abandoned remote handler blocked: publication must not be needed for cancellation.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(title(&db, media).await, original);
        state.mode.store(3, Ordering::SeqCst);
        let invalid = enqueue(&base, target, "normal").await;
        let failed = until(&base, &invalid["id"], "failed").await;
        assert_eq!(failed["error_code"], "invalid_metadata_response");
        assert_eq!(failed["attempts"], 1);
        assert_eq!(title(&db, media).await, original);
        state.mode.store(1, Ordering::SeqCst);
        let unavailable = enqueue(&base, target, "normal").await;
        let retrying = until(&base, &unavailable["id"], "retry_wait").await;
        assert_eq!(retrying["error_code"], "metadata_unavailable");
        assert_eq!(title(&db, media).await, original);
        state.mode.store(0, Ordering::SeqCst);
        assert_eq!(
            until(&base, &unavailable["id"], "succeeded").await["attempts"],
            2
        );
        // A captured external identity changed while HTTP was pending must never apply old facts.
        state.mode.store(2, Ordering::SeqCst);
        let stale = enqueue(&base, target, "normal").await;
        tokio::time::timeout(Duration::from_secs(5), state.started.notified())
            .await
            .unwrap();
        let c = db.connect().await.unwrap();
        c.execute(
            if media == "tv" {
                "UPDATE series SET tvdb_id=102 WHERE id=1"
            } else {
                "UPDATE movie_metadata SET tmdb_id=102 WHERE id=1"
            },
            (),
        )
        .await
        .unwrap();
        // Release all outstanding fixture handlers, including cancelled reads, then settle stale work.
        state.release.add_permits(4);
        let failed = until(&base, &stale["id"], "failed").await;
        assert_eq!(failed["error_code"], "target_changed");
        assert_eq!(failed["attempts"], 1);
        assert_eq!(
            title(&db, media).await,
            if media == "tv" { "Series 1" } else { "Movie 1" }
        );
        c.execute(
            if media == "tv" {
                "UPDATE series SET tvdb_id=101 WHERE id=1"
            } else {
                "UPDATE movie_metadata SET tmdb_id=101 WHERE id=1"
            },
            (),
        )
        .await
        .unwrap();
        state
            .release
            .forget_permits(state.release.available_permits());
        state.mode.store(4, Ordering::SeqCst);
        let limited = enqueue(&base, target, "normal").await;
        let waiting = until(&base, &limited["id"], "retry_wait").await;
        assert_eq!(waiting["error_code"], "metadata_rate_limited");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert!(
            waiting["next_attempt_at"].as_i64().unwrap() >= now + 55,
            "remote cooldown must not be shortened to ordinary exponential retry"
        );
        c.execute(
            if media == "tv" {
                "UPDATE series SET tvdb_id=102 WHERE id=1"
            } else {
                "UPDATE movie_metadata SET tmdb_id=102 WHERE id=1"
            },
            (),
        )
        .await
        .unwrap();
        let calls = state.calls.lock().unwrap().len();
        let failed = until(&base, &limited["id"], "failed").await;
        assert_eq!(failed["error_code"], "target_changed");
        assert_eq!(failed["attempts"], 1);
        assert_eq!(
            state.calls.lock().unwrap().len(),
            calls,
            "stale future retry terminalizes without network or consuming another attempt"
        );
    }
    runtime.shutdown().await;
    stop(server).await;
    stop(upstream).await;
}
