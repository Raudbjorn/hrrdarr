use axum::{
    extract::{OriginalUri, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
};
use hrrdarr::{
    blocklist, commands,
    db::Database,
    providers,
    snapshots::{self, Application},
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::task::JoinHandle;
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let p =
            std::env::temp_dir().join(format!("hrrdarr-clear-blocklist-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.0) {
            eprintln!("scratch cleanup failed: {e}")
        }
    }
}
async fn source(path: &Path, root: &Path, tv: bool, title: &str) -> Vec<u8> {
    let db = libsql::Builder::new_local(path).build().await.unwrap();
    let c = db.connect().unwrap();
    if tv {
        c.execute_batch("CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES(233);CREATE TABLE Series(Id INTEGER,TvdbId INTEGER,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,Seasons TEXT);CREATE TABLE Episodes(Id INTEGER,SeriesId INTEGER,SeasonNumber INTEGER,EpisodeNumber INTEGER,Title TEXT,Monitored INTEGER,EpisodeFileId INTEGER);CREATE TABLE EpisodeFiles(Id INTEGER,SeriesId INTEGER,RelativePath TEXT);CREATE TABLE Blocklist(Id INTEGER,SeriesId INTEGER,EpisodeIds TEXT,Date TEXT,SourceTitle TEXT);").await.unwrap();
        c.execute("INSERT INTO Series VALUES(1,101,'Series',2020,?,1,'[{\"seasonNumber\":1,\"monitored\":true}]')",[root.to_str().unwrap()]).await.unwrap();
        c.execute_batch("INSERT INTO Episodes VALUES(1,1,1,1,'Episode',1,0)")
            .await
            .unwrap();
        c.execute(
            "INSERT INTO Blocklist VALUES(1,1,'[1]','2020-01-01T00:00:00Z',?)",
            [title],
        )
        .await
        .unwrap();
    } else {
        c.execute_batch("CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES(206);CREATE TABLE Movies(Id INTEGER,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER);CREATE TABLE MovieFiles(Id INTEGER,MovieId INTEGER,RelativePath TEXT,Edition TEXT);CREATE TABLE Blocklist(Id INTEGER,MovieId INTEGER,Date TEXT,SourceTitle TEXT)").await.unwrap();
        c.execute(
            "INSERT INTO Movies VALUES(1,101,'tt101','Movie',2020,?,1,0)",
            [root.to_str().unwrap()],
        )
        .await
        .unwrap();
        c.execute(
            "INSERT INTO Blocklist VALUES(1,1,'2020-01-01T00:00:00Z',?)",
            [title],
        )
        .await
        .unwrap();
    }
    drop(c);
    drop(db);
    std::fs::read(path).unwrap()
}
async fn import(db: &Database, tv: bool, bytes: Vec<u8>) {
    let report = snapshots::import(
        db,
        if tv {
            Application::Sonarr
        } else {
            Application::Radarr
        },
        bytes,
        false,
    )
    .await
    .unwrap();
    assert!(report.applied);
    assert_eq!(report.conflicts, 0);
}
async fn serve(app: axum::Router) -> (String, JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    (
        base,
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() }),
    )
}
async fn app(db: Arc<Database>) -> (String, JoinHandle<()>, providers::RefreshClient) {
    let (p, client) = providers::router_with_refresh(db.clone(), None);
    let (base, server) = serve(
        p.merge(commands::router(db.clone()))
            .merge(blocklist::router(db)),
    )
    .await;
    (base, server, client)
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
    (
        code,
        if code == 204 {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or_else(|_| panic!("{code}: {text}"))
        },
    )
}
const ROUTE: &str = "/api/v1/blocklist/clear-commands";
async fn enqueue(base: &str, media: &str, priority: &str) -> Value {
    let (code, v) = request(
        base,
        "POST",
        ROUTE,
        json!({"target":{"media_type":media},"priority":priority}),
    )
    .await;
    assert_eq!(code, 202, "{v}");
    v
}
async fn until(base: &str, id: &Value, status: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            let (code, v) = request(
                base,
                "GET",
                &format!("{ROUTE}/{}", id.as_str().unwrap()),
                Value::Null,
            )
            .await;
            assert_eq!(code, 200, "{v}");
            if v["status"] == status {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("command {id} did not reach {status}"))
}
async fn count(base: &str, media: &str) -> i64 {
    let (code, v) = request(
        base,
        "GET",
        &format!("/api/v1/blocklist?media_type={media}"),
        Value::Null,
    )
    .await;
    assert_eq!(code, 200, "{v}");
    v["total"].as_i64().unwrap()
}
async fn stop(task: JoinHandle<()>) {
    task.abort();
    let _ = task.await;
}
struct Remote {
    started: tokio::sync::Notify,
    release: tokio::sync::Semaphore,
}
async fn remote(
    State(s): State<Arc<Remote>>,
    method: Method,
    OriginalUri(uri): OriginalUri,
) -> Response {
    assert_eq!(
        method,
        Method::GET,
        "clear must not issue external mutations"
    );
    if uri.path().ends_with("webapiVersion") {
        s.started.notify_one();
        // Closing this test gate permanently releases current and future callers.
        // Health/recovery may probe the same endpoint; no fixed request count is assumed.
        if let Ok(permit) = s.release.acquire().await {
            permit.forget();
        }
        return "2.8.3".into_response();
    }
    if uri.path().ends_with("torrents/info") {
        return axum::Json(json!([])).into_response();
    }
    StatusCode::NOT_FOUND.into_response()
}
#[tokio::test]
async fn domain_clear_is_explicit_durable_atomic_and_shares_existing_worker() {
    let scratch = Scratch::new();
    let dbpath = scratch.0.join("db");
    let db = Arc::new(Database::open_local(&dbpath).await.unwrap());
    let (base, server, client) = app(db.clone()).await;
    let tv1 = source(
        &scratch.0.join("tv1"),
        &scratch.0.join("tv"),
        true,
        "TV first",
    )
    .await;
    let tv2 = source(
        &scratch.0.join("tv2"),
        &scratch.0.join("tv"),
        true,
        "TV later",
    )
    .await;
    let movie = source(
        &scratch.0.join("movie"),
        &scratch.0.join("movies"),
        false,
        "Movie first",
    )
    .await;
    import(&db, true, tv1.clone()).await;
    import(&db, false, movie.clone()).await;
    for body in [
        json!({"target":{},"priority":"normal"}),
        json!({"target":{"media_type":"all"},"priority":"normal"}),
        json!({"target":{"media_type":"tv","series_id":1},"priority":"normal"}),
    ] {
        assert_eq!(request(&base, "POST", ROUTE, body).await.0, 400)
    }
    let tv = enqueue(&base, "tv", "high").await;
    assert_eq!(enqueue(&base, "tv", "normal").await["id"], tv["id"]);
    assert_eq!(
        request(
            &base,
            "DELETE",
            &format!("{ROUTE}/{}", tv["id"].as_str().unwrap()),
            Value::Null
        )
        .await
        .0,
        409
    );
    // Purge scope is evaluated at execution, not captured from the UI's page or enqueue time.
    import(&db, true, tv2.clone()).await;
    assert_eq!(count(&base, "tv").await, 2);
    let state = Arc::new(Remote {
        started: tokio::sync::Notify::new(),
        release: tokio::sync::Semaphore::new(0),
    });
    let (endpoint, upstream) = serve(
        axum::Router::new()
            .fallback(remote)
            .with_state(state.clone()),
    )
    .await;
    let scope =
        json!({"category":"tv","imported_category":null,"recent_priority":0,"older_priority":1});
    let(code,p)=request(&base,"POST","/api/v1/providers",json!({"name":"shared","enabled":true,"priority":1,"settings":{"implementation":"qbittorrent","endpoint":endpoint,"tv":scope,"movies":null},"credentials":null})).await;
    assert_eq!(code, 201, "{p}");
    let(code,refresh)=request(&base,"POST","/api/v1/commands",json!({"name":"refresh_downloads","target":{"provider_id":p["id"],"media_type":"tv"},"provider_revision":p["revision"],"priority":"normal"})).await;
    assert_eq!(code, 202);
    let runtime = commands::start(db.clone(), client).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), state.started.notified())
        .await
        .unwrap();
    let done = until(&base, &tv["id"], "succeeded").await;
    assert_eq!(done["records_removed"], 2);
    assert_eq!(done["attempts"], 1);
    assert_eq!(count(&base, "tv").await, 0);
    assert_eq!(
        count(&base, "movies").await,
        1,
        "high-priority TV clear must run before blocked normal download and preserve movies"
    );
    let cancelled = enqueue(&base, "movies", "normal").await;
    let path = format!("{ROUTE}/{}", cancelled["id"].as_str().unwrap());
    assert_eq!(
        request(&base, "POST", &format!("{path}/cancel"), Value::Null)
            .await
            .1["status"],
        "cancelled"
    );
    assert_eq!(count(&base, "movies").await, 1);
    assert_eq!(request(&base, "DELETE", &path, Value::Null).await.0, 204);
    let queued = enqueue(&base, "movies", "high").await;
    runtime.shutdown().await;
    stop(server).await;
    drop(db);
    // A queued typed target survives reopen, while an unrelated running download is safely recovered.
    let db = Arc::new(Database::open_local(&dbpath).await.unwrap());
    let (base, server, client) = app(db.clone()).await;
    state.release.close();
    let c = db.connect().await.unwrap();
    c.execute_batch("CREATE TRIGGER fixture_clear_settlement BEFORE UPDATE ON blocklist_clear_commands WHEN NEW.status='succeeded' BEGIN SELECT RAISE(ABORT,'owned failure injection'); END;").await.unwrap();
    let runtime = commands::start(db.clone(), client).await.unwrap();
    let waiting = until(&base, &queued["id"], "retry_wait").await;
    assert_eq!(waiting["error_code"], "storage_error");
    assert_eq!(waiting["records_removed"], 0);
    assert_eq!(count(&base, "movies").await, 1);
    // Movie command failure after DELETE must roll back the entry and provenance tombstone.
    assert_eq!(c.query("SELECT count(*) FROM snapshot_blocklist WHERE application='radarr' AND removed_at IS NOT NULL",()).await.unwrap().next().await.unwrap().unwrap().get::<i64>(0).unwrap(),0);
    c.execute_batch("DROP TRIGGER fixture_clear_settlement")
        .await
        .unwrap();
    let done = until(&base, &queued["id"], "succeeded").await;
    assert_eq!(done["attempts"], 2);
    assert_eq!(done["target"]["media_type"], "movies");
    assert_eq!(done["records_removed"], 1);
    assert_eq!(count(&base, "movies").await, 0);
    // A successful clear does not prove the unrelated recovered refresh settled.
    // Keep its exact admission identity so the later capacity POST is genuinely new.
    let mut last_refresh = Value::Null;
    let recovered = tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            let (code, value) = request(
                &base,
                "GET",
                &format!("/api/v1/commands/{}", refresh["id"].as_str().unwrap()),
                Value::Null,
            )
            .await;
            last_refresh = value;
            if code != 200
                || matches!(
                    last_refresh["status"].as_str(),
                    Some("failed" | "cancelled")
                )
            {
                return false;
            }
            if last_refresh["status"] == "succeeded" {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    if !matches!(recovered, Ok(true)) {
        runtime.shutdown().await;
        stop(server).await;
        stop(upstream).await;
        panic!("recovered refresh did not succeed: {last_refresh}; outcome={recovered:?}");
    }
    assert_eq!(last_refresh["id"], refresh["id"]);
    assert_eq!(last_refresh["target"], refresh["target"]);
    assert_eq!(last_refresh["provider_revision"], p["revision"]);
    assert_eq!(last_refresh["items_observed"], 0);
    let empty = enqueue(&base, "movies", "normal").await;
    assert_eq!(
        until(&base, &empty["id"], "succeeded").await["records_removed"],
        0
    );
    import(&db, true, tv1).await;
    import(&db, true, tv2).await;
    import(&db, false, movie).await;
    assert_eq!(count(&base, "tv").await, 0);
    assert_eq!(count(&base, "movies").await, 0);
    let later = source(
        &scratch.0.join("later"),
        &scratch.0.join("tv"),
        true,
        "After completed clear",
    )
    .await;
    import(&db, true, later).await;
    // Reading/cancelling/deleting a terminal command never re-executes its old domain purge.
    assert_eq!(
        request(
            &base,
            "POST",
            &format!("{ROUTE}/{}/cancel", tv["id"].as_str().unwrap()),
            Value::Null
        )
        .await
        .1["status"],
        "succeeded"
    );
    assert_eq!(
        request(
            &base,
            "DELETE",
            &format!("{ROUTE}/{}", tv["id"].as_str().unwrap()),
            Value::Null
        )
        .await
        .0,
        204
    );
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(count(&base, "tv").await, 1);
    let page = request(
        &base,
        "GET",
        &format!("{ROUTE}?media_type=movies&status=succeeded&limit=1"),
        Value::Null,
    )
    .await;
    assert_eq!(page.0, 200);
    assert_eq!(page.1["total"], 2);
    assert_eq!(page.1["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        request(&base, "GET", &format!("{ROUTE}?limit=101"), Value::Null)
            .await
            .0,
        400
    );
    // Worker/restart behavior is proved above. Join it before capacity-only assertions so
    // a health completion cannot free a slot between padding and the HTTP 429 checks.
    runtime.shutdown().await;
    assert_eq!(
        request(
            &base,
            "GET",
            &format!("/api/v1/commands/{}", refresh["id"].as_str().unwrap()),
            Value::Null
        )
        .await
        .1["status"],
        "succeeded"
    );
    assert_eq!(c.query("SELECT count(*) FROM commands WHERE provider_id=? AND media_type='tv' AND status IN ('queued','running','retry_wait')",[p["id"].as_str().unwrap()]).await.unwrap().next().await.unwrap().unwrap().get::<i64>(0).unwrap(),0,"capacity assertion requires a fresh target");
    // Bounded synthetic terminal history isolates HTTP admission at the shared cap;
    // blocklist facts above still come exclusively from real snapshot imports.
    let retained:i64=c.query("SELECT (SELECT count(*) FROM commands)+(SELECT count(*) FROM metadata_refresh_commands)+(SELECT count(*) FROM blocklist_clear_commands)",()).await.unwrap().next().await.unwrap().unwrap().get(0).unwrap();
    let mut fill = String::new();
    let mut last = uuid::Uuid::nil();
    for _ in retained..1024 {
        last = uuid::Uuid::new_v4();
        fill.push_str(&format!("INSERT INTO blocklist_clear_commands(id,name,media_type,priority,status,attempts,next_attempt_at,created_at,records_removed)VALUES('{last}','clear_blocklist','tv',0,'queued',0,0,0,0);UPDATE blocklist_clear_commands SET status='cancelled',completed_at=0 WHERE id='{last}';"));
    }
    c.execute_batch(&fill).await.unwrap();
    // Every row above is terminal (migration 0033: only active queued/running/retry_wait rows
    // occupy the shared pool), so it is not actually full yet. Pad it with genuinely active
    // search_commands rows (no per-target uniqueness, unlike the tables above) against the real
    // imported TV episode, with far-future readiness to keep the padding inert.
    const POOL_ACTIVE_SQL: &str = "(SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))";
    let search_indexer_id = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'torznab','Padding Indexer',1,1,1,1,'http://127.0.0.1:1/')",[search_indexer_id.clone()]).await.unwrap();
    c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year) VALUES(?,'torznab','tv','[5000]','[]',0,NULL)",[search_indexer_id.clone()]).await.unwrap();
    let (series_id, episode_id, tvdb_id): (i64, i64, i64) = {
        let mut rows = c
            .query(
                "SELECT s.id,e.id,s.tvdb_id FROM episodes e JOIN series s ON s.id=e.series_id LIMIT 1",
                (),
            )
            .await
            .unwrap();
        let row = rows
            .next()
            .await
            .unwrap()
            .expect("a real TV episode must already exist from the snapshot import above");
        (
            row.get(0).unwrap(),
            row.get(1).unwrap(),
            row.get(2).unwrap(),
        )
    };
    let captured = json!({"media_type":"tv","series_id":series_id,"episode_id":episode_id,"tvdb_id":tvdb_id,"title":"Padding","season":1,"number":1,"series_type":"standard","use_scene_numbering":false});
    // Count and fill the complete shared pool against one stable writer snapshot.
    let tx = c
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await
        .unwrap();
    let active: i64 = tx
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
        tx.execute(
            "INSERT INTO search_commands(id,mode,media_type,requested_episode_id,captured_target_json,indexer_id,indexer_revision,client_id,client_revision,next_attempt_at,created_at) VALUES(?,'automatic','tv',?,?,?,1,?,?,9007199254740000,0)",
            libsql::params![
                id.clone(),
                episode_id,
                captured.to_string(),
                search_indexer_id.clone(),
                p["id"].as_str().unwrap(),
                p["revision"].as_i64().unwrap()
            ],
        )
        .await
        .unwrap();
        padding.push(id);
    }
    assert_eq!(
        tx.query(&format!("SELECT {POOL_ACTIVE_SQL}"), ())
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
    tx.commit().await.unwrap();
    for (route, body) in [
        (
            ROUTE,
            json!({"target":{"media_type":"tv"},"priority":"normal"}),
        ),
        (
            "/api/v1/metadata-refresh/commands",
            json!({"target":{"media_type":"tv","series_id":1},"priority":"normal"}),
        ),
        (
            "/api/v1/commands",
            json!({"name":"refresh_downloads","target":{"provider_id":p["id"],"media_type":"tv"},"provider_revision":p["revision"],"priority":"normal"}),
        ),
    ] {
        let (code, value) = request(&base, "POST", route, body).await;
        assert_eq!(code, 429, "{value}");
        assert_eq!(value["error"]["code"], "command_history_full");
    }
    assert_eq!(
        request(&base, "DELETE", &format!("{ROUTE}/{last}"), Value::Null)
            .await
            .0,
        204
    );
    // The DELETE above only removed an already-terminal row, so it did not free the pool
    // (migration 0033): admission is still rejected.
    assert_eq!(
        request(
            &base,
            "POST",
            ROUTE,
            json!({"target":{"media_type":"tv"},"priority":"normal"}),
        )
        .await
        .0,
        429
    );
    // Cancelling one active padding row instead proves capacity really frees up.
    c.execute(
        "UPDATE search_commands SET status='cancelled',completed_at=0 WHERE id=?",
        [padding[0].clone()],
    )
    .await
    .unwrap();
    let admitted = enqueue(&base, "tv", "normal").await;
    // A duplicate owns no additional slot, even when the pool is full again.
    let duplicate = enqueue(&base, "tv", "normal").await;
    assert_eq!(duplicate["id"], admitted["id"]);
    assert_eq!(
        c.query(&format!("SELECT {POOL_ACTIVE_SQL}"), ())
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
    stop(server).await;
    stop(upstream).await;
}
