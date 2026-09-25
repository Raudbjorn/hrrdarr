use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use hrrdarr::{db::Database, episodes, import, library};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("hrrdarr-metadata-http-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
#[derive(Default)]
struct Mock {
    mode: AtomicU8,
    requests: Mutex<Vec<String>>,
}
fn show() -> Value {
    json!({"tvdbId":101,"title":"Fixture series","firstAired":"2020-01-01","imdbId":"tt1234567","seasons":[{"seasonNumber":0},{"seasonNumber":1}],"episodes":[{"tvdbId":501,"seasonNumber":0,"episodeNumber":1,"title":"Special","airDate":"2020-01-01","airDateUtc":"2020-01-01T12:00:00Z"},{"tvdbId":502,"seasonNumber":1,"episodeNumber":1,"title":"Pilot","airDate":"2020-01-02","airDateUtc":"2020-01-02T12:00:00Z","absoluteEpisodeNumber":1}]})
}
fn film() -> Value {
    json!({"tmdbId":101,"title":"Fixture movie","year":2021,"imdbId":"tt7654321"})
}
async fn tv_detail(State(s): State<Arc<Mock>>, Path(id): Path<String>) -> Response {
    s.requests.lock().unwrap().push(format!("tv:{id}"));
    match s.mode.load(Ordering::SeqCst) {
        1 => (StatusCode::BAD_GATEWAY, "PRIVATE_UPSTREAM_ERROR").into_response(),
        2 => {
            let mut body = show();
            body["tvdbId"] = json!(999);
            Json(body).into_response()
        }
        3 => {
            let mut body = show();
            body["episodes"][1] = body["episodes"][0].clone();
            Json(body).into_response()
        }
        6 => {
            let mut body = show();
            body.as_object_mut().unwrap().remove("episodes");
            Json(body).into_response()
        }
        7 => {
            let mut body = show();
            body.as_object_mut().unwrap().remove("seasons");
            Json(body).into_response()
        }
        _ if id == "303" => {
            let mut body = show();
            body["tvdbId"] = json!(303);
            body["title"] = json!("Upcoming");
            body["seasons"] = json!([]);
            body["episodes"] = json!([]);
            Json(body).into_response()
        }
        _ if id == "202" => {
            let mut body = show();
            body["tvdbId"] = json!(202);
            body["title"] = json!("Other series");
            Json(body).into_response()
        }
        _ if id == "101" => Json(show()).into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}
async fn movie_detail(State(s): State<Arc<Mock>>, Path(id): Path<String>) -> Response {
    s.requests.lock().unwrap().push(format!("movie:{id}"));
    if s.mode.load(Ordering::SeqCst) == 1 {
        return (StatusCode::BAD_GATEWAY, "PRIVATE_UPSTREAM_ERROR").into_response();
    }
    if id == "101" {
        Json(film()).into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}
async fn tv_search(
    State(s): State<Arc<Mock>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    s.requests.lock().unwrap().push(format!(
        "tv-search:{}",
        q.get("term").map(String::as_str).unwrap_or("")
    ));
    if s.mode.load(Ordering::SeqCst) == 4 {
        return Json(json!([show(), show()])).into_response();
    }
    if s.mode.load(Ordering::SeqCst) == 5 {
        return Json(Value::Array(
            (1..=101)
                .map(|id| {
                    let mut v = show();
                    v["tvdbId"] = json!(id);
                    v
                })
                .collect(),
        ))
        .into_response();
    }
    if q.get("term").is_some_and(|v| v == "empty") {
        Json(json!([])).into_response()
    } else {
        Json(json!([show()])).into_response()
    }
}
async fn movie_search(
    State(s): State<Arc<Mock>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    s.requests.lock().unwrap().push(format!(
        "movie-search:{}",
        q.get("q").map(String::as_str).unwrap_or("")
    ));
    if s.mode.load(Ordering::SeqCst) == 4 {
        return Json(json!([film(), film()])).into_response();
    }
    if s.mode.load(Ordering::SeqCst) == 5 {
        return Json(Value::Array(
            (1..=101)
                .map(|id| {
                    let mut v = film();
                    v["tmdbId"] = json!(id);
                    v
                })
                .collect(),
        ))
        .into_response();
    }
    if q.get("q").is_some_and(|v| v == "empty") {
        Json(json!([])).into_response()
    } else {
        Json(json!([film()])).into_response()
    }
}
async fn serve(app: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, task)
}
async fn request(
    client: &reqwest::Client,
    base: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let mut req = client.request(method.parse().unwrap(), format!("{base}{path}"));
    if let Some(body) = body {
        req = req
            .header("Content-Type", "application/json")
            .body(body.to_string());
    }
    let response = req.send().await.unwrap();
    let status = response.status().as_u16();
    let text = response.text().await.unwrap();
    assert!(!text.contains("PRIVATE_UPSTREAM_ERROR"));
    (
        status,
        serde_json::from_str(&text).unwrap_or_else(|_| panic!("non-JSON {status}: {text}")),
    )
}
async fn count(c: &libsql::Connection, table: &str) -> i64 {
    c.query(&format!("SELECT count(*) FROM {table}"), ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}

#[tokio::test]
async fn lookup_selected_add_and_safe_import_create_both_domain_targets() {
    let scratch = Scratch::new();
    let mock = Arc::new(Mock::default());
    let upstream = Router::new()
        .route("/shows/en/{id}", get(tv_detail))
        .route("/search/en", get(tv_search))
        .route("/movie/{id}", get(movie_detail))
        .route("/search", get(movie_search))
        .with_state(mock.clone());
    let (origin, upstream_task) = serve(upstream).await;
    let metadata = Arc::new(
        hrrdarr::metadata::MetadataClient::with_origins(
            &format!("{origin}/"),
            &format!("{origin}/"),
        )
        .unwrap(),
    );
    let path = scratch.0.join("library.db");
    let db = Arc::new(Database::open_local(&path).await.unwrap());
    let c = db.connect().await.unwrap();
    let app = library::router(db.clone())
        .merge(library::metadata_router(db.clone(), metadata))
        .merge(episodes::router(db.clone()))
        .merge(import::router(db.clone()));
    let (base, api_task) = serve(app).await;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();
    let tv_root = scratch.0.join("tv");
    let movie_root = scratch.0.join("movie");
    let incoming = scratch.0.join("incoming");
    for root in [&tv_root, &movie_root, &incoming] {
        std::fs::create_dir(root).unwrap();
    }
    let tv_add = json!({"tvdb_id":101,"path":tv_root,"settings":{"monitored":false,"seasons":[{"number":0,"monitored":false},{"number":1,"monitored":true}]}});
    let movie_add = json!({"tmdb_id":101,"path":movie_root,"settings":{"monitored":false,"minimum_availability":"released"}});
    for endpoint in ["/api/v1/tv/series/lookup", "/api/v1/movies/lookup"] {
        let (status, found) = request(
            &client,
            &base,
            "GET",
            &format!("{endpoint}?term=Fixture"),
            None,
        )
        .await;
        assert_eq!(status, 200, "{found}");
        assert_eq!(found.as_array().unwrap().len(), 1);
        assert_eq!(found[0]["external_id"], 101);
        assert_eq!(
            found[0]["media_type"],
            if endpoint.contains("tv/series") {
                "tv"
            } else {
                "movies"
            }
        );
        assert_eq!(
            found[0]["title"],
            if endpoint.contains("tv/series") {
                "Fixture series"
            } else {
                "Fixture movie"
            }
        );
        let (status, empty) = request(
            &client,
            &base,
            "GET",
            &format!("{endpoint}?term=empty"),
            None,
        )
        .await;
        assert_eq!(status, 200, "{empty}");
        assert_eq!(empty, json!([]));
        for mode in [4, 5] {
            mock.mode.store(mode, Ordering::SeqCst);
            assert_eq!(
                request(
                    &client,
                    &base,
                    "GET",
                    &format!("{endpoint}?term=Fixture"),
                    None
                )
                .await
                .0,
                502
            );
        }
        mock.mode.store(0, Ordering::SeqCst);
        let requests = mock.requests.lock().unwrap().len();
        assert_eq!(
            request(
                &client,
                &base,
                "GET",
                &format!("{endpoint}?term={}", "x".repeat(257)),
                None
            )
            .await
            .0,
            400
        );
        assert_eq!(mock.requests.lock().unwrap().len(), requests);
        for suffix in ["?term=", "?term=x&unknown=true", "/0", "/-1"] {
            let (status, error) =
                request(&client, &base, "GET", &format!("{endpoint}{suffix}"), None).await;
            assert_eq!(status, 400, "{error}");
        }
        let (status, error) =
            request(&client, &base, "GET", &format!("{endpoint}/9999"), None).await;
        assert_eq!(status, 404, "{error}");
    }
    for mode in [1, 2, 3, 6, 7] {
        mock.mode.store(mode, Ordering::SeqCst);
        let (status, error) = request(
            &client,
            &base,
            "POST",
            "/api/v1/tv/series/lookup",
            Some(tv_add.clone()),
        )
        .await;
        assert!(status >= 400, "{error}");
        if mode == 1 {
            let (status, error) = request(
                &client,
                &base,
                "POST",
                "/api/v1/movies/lookup",
                Some(movie_add.clone()),
            )
            .await;
            assert_eq!(status, 502, "{error}");
        }
        for table in [
            "series",
            "seasons",
            "episodes",
            "library_settings",
            "movies",
            "movie_metadata",
        ] {
            assert_eq!(count(&c, table).await, 0);
        }
    }
    mock.mode.store(0, Ordering::SeqCst);
    let mut forged = tv_add.clone();
    forged["episodes"] = json!([]);
    assert_eq!(
        request(
            &client,
            &base,
            "POST",
            "/api/v1/tv/series/lookup",
            Some(forged)
        )
        .await
        .0,
        400
    );
    for (route, body, table) in [
        ("/api/v1/tv/series/lookup", tv_add.clone(), "episodes"),
        (
            "/api/v1/movies/lookup",
            movie_add.clone(),
            "library_settings",
        ),
    ] {
        c.execute_batch(&format!("CREATE TRIGGER late_failure BEFORE INSERT ON {table} BEGIN SELECT RAISE(ABORT,'PRIVATE_UPSTREAM_ERROR');END;")).await.unwrap();
        assert_eq!(
            request(&client, &base, "POST", route, Some(body)).await.0,
            500
        );
        for table in [
            "series",
            "seasons",
            "episodes",
            "library_settings",
            "movies",
            "movie_metadata",
        ] {
            assert_eq!(count(&c, table).await, 0);
        }
        c.execute("DROP TRIGGER late_failure", ()).await.unwrap();
    }
    let (status, tv) = request(
        &client,
        &base,
        "POST",
        "/api/v1/tv/series/lookup",
        Some(tv_add.clone()),
    )
    .await;
    assert_eq!(status, 201, "{tv}");
    assert_eq!(tv["title"], "Fixture series");
    assert_eq!(tv["monitored"], false);
    let tv_id = tv["id"].as_i64().unwrap();
    let (status, movie) = request(
        &client,
        &base,
        "POST",
        "/api/v1/movies/lookup",
        Some(movie_add.clone()),
    )
    .await;
    assert_eq!(status, 201, "{movie}");
    assert_eq!(movie["title"], "Fixture movie");
    assert_eq!(movie["monitored"], false);
    let movie_id = movie["id"].as_i64().unwrap();
    let (status, episode_page) = request(
        &client,
        &base,
        "GET",
        &format!("/api/v1/episodes?series_id={tv_id}"),
        None,
    )
    .await;
    assert_eq!(status, 200, "{episode_page}");
    let episode_items = episode_page["items"].as_array().unwrap();
    assert_eq!(episode_items.len(), 2);
    let special = episode_items.iter().find(|e| e["season"] == 0).unwrap();
    let pilot = episode_items.iter().find(|e| e["season"] == 1).unwrap();
    assert_eq!(special["monitored"], false);
    assert_eq!(pilot["monitored"], true);
    let episode_id = special["id"].as_i64().unwrap();
    assert_eq!(episode_id, movie_id);
    for (domain, id, root, read_path) in [
        (
            "episode",
            episode_id,
            &tv_root,
            format!("/api/v1/tv/series/{tv_id}"),
        ),
        (
            "movie",
            movie_id,
            &movie_root,
            format!("/api/v1/movies/{movie_id}"),
        ),
    ] {
        let source = incoming.join(format!("{domain}.mkv"));
        let destination = root.join("imported.mkv");
        let bytes = format!("isolated {domain} media").into_bytes();
        std::fs::write(&source, &bytes).unwrap();
        let (status,preview)=request(&client,&base,"POST","/api/v1/imports",Some(json!({"target":{"media_type":domain,"id":id},"source":source,"destination":destination,"mode":"copy"}))).await;
        assert_eq!(status, 202, "{preview}");
        let execute = format!(
            "/api/v1/imports/{}/execute",
            preview["id"].as_str().unwrap()
        );
        let (status, result) = request(&client, &base, "POST", &execute, None).await;
        assert_eq!(status, 200, "{result}");
        assert_eq!(result["status"], "complete");
        assert_eq!(std::fs::read(&source).unwrap(), bytes);
        assert_eq!(std::fs::read(&destination).unwrap(), bytes);
        let (_, before) = request(&client, &base, "GET", &read_path, None).await;
        assert_eq!(before["statistics"]["file_count"], 1);
        assert_eq!(before["monitored"], false);
        let (route, body) = if domain == "episode" {
            ("/api/v1/tv/series/lookup", tv_add.clone())
        } else {
            ("/api/v1/movies/lookup", movie_add.clone())
        };
        assert_eq!(
            request(&client, &base, "POST", route, Some(body)).await.0,
            409
        );
        assert_eq!(
            request(&client, &base, "GET", &read_path, None).await.1,
            before
        );
        assert_eq!(std::fs::read(&destination).unwrap(), bytes);
    }
    let (status, error) = request(
        &client,
        &base,
        "POST",
        "/api/v1/tv/series/lookup",
        Some(json!({"tvdb_id":202,"path":scratch.0.join("other-series")})),
    )
    .await;
    assert_eq!(status, 409, "{error}");
    assert_eq!(count(&c, "series").await, 1);
    assert_eq!(count(&c, "episodes").await, 2);
    let (status, upcoming) = request(
        &client,
        &base,
        "POST",
        "/api/v1/tv/series/lookup",
        Some(json!({"tvdb_id":303,"path":scratch.0.join("upcoming")})),
    )
    .await;
    assert_eq!(status, 201, "{upcoming}");
    assert_eq!(upcoming["statistics"]["total_episode_count"], 0);
    assert_eq!(count(&c, "import_history").await, 2);
    api_task.abort();
    let _ = api_task.await;
    drop(c);
    drop(db);
    let reopened = Database::open_local(&path).await.unwrap();
    let c = reopened.connect().await.unwrap();
    for (table, expected) in [
        ("series", 2),
        ("movies", 1),
        ("episodes", 2),
        ("episode_files", 1),
        ("movie_files", 1),
        ("import_history", 2),
    ] {
        assert_eq!(count(&c, table).await, expected);
    }
    upstream_task.abort();
    let _ = upstream_task.await;
}
