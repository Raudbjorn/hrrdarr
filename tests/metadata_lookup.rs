use axum::{
    Json, Router,
    extract::{OriginalUri, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use hrrdarr::{db::Database, episodes, import, library, providers};
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
const PROVIDER_SECRET: &str = "GATE_PROVIDER_PRIVATE_SENTINEL";
#[derive(Default)]
struct ProviderMock {
    fail: AtomicU8,
    requests: Mutex<Vec<(String, HashMap<String, String>)>>,
}
async fn provider_upstream(
    State(s): State<Arc<ProviderMock>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    let params: HashMap<String, String> =
        url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();
    s.requests
        .lock()
        .unwrap()
        .push((uri.path().into(), params.clone()));
    if matches!(uri.path(), "/torznab" | "/newznab") {
        assert_eq!(
            params.get("apikey").map(String::as_str),
            Some(PROVIDER_SECRET)
        );
        if params.get("t").is_some_and(|v| v == "caps") {
            return ([("content-type","application/xml")],r#"<caps><limits max="100" default="10"/><searching><search available="yes" supportedParams="q"/><tv-search available="yes" supportedParams="q,tvdbid,season,ep"/><movie-search available="yes" supportedParams="q,imdbid,tmdbid"/></searching><categories><category id="5000"><subcat id="5030"/></category><category id="2000"/></categories></caps>"#).into_response();
        }
        if s.fail.load(Ordering::SeqCst) == 1 {
            return (
                [("content-type", "application/xml")],
                format!("<error code=\"100\" description=\"{PROVIDER_SECRET}\"/>"),
            )
                .into_response();
        }
        return ([("content-type","application/xml")],r#"<rss xmlns:x="http://www.newznab.com/DTD/2010/feeds/attributes/"><channel><x:response offset="0" total="0"/></channel></rss>"#).into_response();
    }
    assert_eq!(
        headers.get("authorization").unwrap(),
        &format!("Bearer {PROVIDER_SECRET}")
    );
    if s.fail.load(Ordering::SeqCst) == 1 {
        return (StatusCode::UNAUTHORIZED, PROVIDER_SECRET).into_response();
    }
    match uri.path() {
        "/api/v2/app/webapiVersion" => "2.8.3".into_response(),
        "/api/v2/app/version" => "v4.6.0".into_response(),
        "/api/v2/torrents/info" => Json(json!([])).into_response(),
        "/api/v2/app/preferences" => Json(json!({"queueing_enabled":true,"max_ratio_enabled":false,"max_ratio":-1,"max_seeding_time_enabled":false,"max_seeding_time":-1,"max_ratio_act":0})).into_response(),
        "/api/v2/torrents/categories" => Json(json!({"tv":{"savePath":"/private/tv"},"movies":{"savePath":"/private/movies"}})).into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
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
    assert!(!text.contains(PROVIDER_SECRET));
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
    let provider_mock = Arc::new(ProviderMock::default());
    let (provider_origin, provider_task) = serve(
        Router::new()
            .fallback(provider_upstream)
            .with_state(provider_mock.clone()),
    )
    .await;
    let key = Arc::new(providers::CredentialKey::from_hex(&"42".repeat(32)).unwrap());
    let app = library::router(db.clone())
        .merge(library::metadata_router(db.clone(), metadata))
        .merge(episodes::router(db.clone()))
        .merge(import::router(db.clone()))
        .merge(providers::router(db.clone(), Some(key.clone())));
    let (base, api_task) = serve(app).await;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();
    let mut provider_results = Vec::new();
    for implementation in ["torznab", "newznab", "qbittorrent"] {
        let endpoint = if implementation == "qbittorrent" {
            provider_origin.clone()
        } else {
            format!("{provider_origin}/{implementation}")
        };
        let settings = if implementation == "qbittorrent" {
            let scope = |category| json!({"category":category,"imported_category":null,"recent_priority":0,"older_priority":1});
            json!({"implementation":implementation,"endpoint":endpoint,"tv":scope("tv"),"movies":scope("movies")})
        } else {
            json!({"implementation":implementation,"endpoint":endpoint,"tv":{"categories":[5030],"anime_categories":[]},"movies":{"categories":[2000]}})
        };
        let (status,created)=request(&client,&base,"POST","/api/v1/providers",Some(json!({"name":implementation,"enabled":true,"priority":1,"settings":settings,"credentials":{"kind":"api_key","api_key":PROVIDER_SECRET}}))).await;
        assert_eq!(status, 201, "{created}");
        assert_eq!(created["test_status"], "never_tested");
        let route = format!("/api/v1/providers/{}", created["id"].as_str().unwrap());
        provider_mock.fail.store(1, Ordering::SeqCst);
        let (status, error) = request(&client, &base, "POST", &format!("{route}/test"), None).await;
        assert_eq!(status, 502, "{error}");
        let (_, failed) = request(&client, &base, "GET", &route, None).await;
        assert_eq!(failed["test_status"], "failure");
        assert_eq!(failed["revision"], 1);
        provider_mock.fail.store(0, Ordering::SeqCst);
        let (status, tested) =
            request(&client, &base, "POST", &format!("{route}/test"), None).await;
        assert_eq!(status, 200, "{tested}");
        assert_eq!(tested["result"]["domains"], json!(["tv", "movies"]));
        let (_, saved) = request(&client, &base, "GET", &route, None).await;
        assert_eq!(saved["test_status"], "success");
        assert_eq!(saved["last_test"]["revision"], 1);
        provider_results.push((route, saved));
    }
    {
        let requests = provider_mock.requests.lock().unwrap();
        for implementation in ["torznab", "newznab"] {
            for category in ["5030", "2000"] {
                assert!(
                    requests
                        .iter()
                        .any(|(path, q)| path == &format!("/{implementation}")
                            && q.get("cat").is_some_and(|v| v == category)),
                    "missing {implementation}/{category} authenticated feed probe"
                );
            }
        }
        assert!(
            requests
                .iter()
                .any(|(path, _)| path == "/api/v2/torrents/categories")
        );
        for category in ["tv", "movies"] {
            assert!(
                requests
                    .iter()
                    .any(|(path, q)| path == "/api/v2/torrents/info"
                        && q.get("category").is_some_and(|v| v == category)),
                "missing client scope probe {category}"
            );
        }
    }
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
    let reopened = Arc::new(Database::open_local(&path).await.unwrap());
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
    let (reopened_base, reopened_task) = serve(
        providers::router(reopened.clone(), Some(key)).merge(library::router(reopened.clone())),
    )
    .await;
    for (route, expected) in provider_results {
        assert_eq!(
            request(&client, &reopened_base, "GET", &route, None).await,
            (200, expected)
        );
    }
    for route in [
        format!("/api/v1/tv/series/{tv_id}"),
        format!("/api/v1/movies/{movie_id}"),
    ] {
        let (status, item) = request(&client, &reopened_base, "GET", &route, None).await;
        assert_eq!(status, 200);
        assert_eq!(item["statistics"]["file_count"], 1);
        assert_eq!(item["monitored"], false);
    }
    assert_eq!(
        std::fs::read(tv_root.join("imported.mkv")).unwrap(),
        b"isolated episode media"
    );
    assert_eq!(
        std::fs::read(movie_root.join("imported.mkv")).unwrap(),
        b"isolated movie media"
    );
    reopened_task.abort();
    let _ = reopened_task.await;
    provider_task.abort();
    let _ = provider_task.await;
    upstream_task.abort();
    let _ = upstream_task.await;
}

#[tokio::test]
async fn tv_original_language_survives_lookup_add_refresh_failure_and_reopen() {
    async fn scalar(c: &libsql::Connection, sql: &str) -> i64 {
        c.query(sql, ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap()
    }
    use hrrdarr::library::refresh::{self, Details, Target};
    let scratch = Scratch::new();
    let source = Arc::new(Mutex::new(show()));
    source.lock().unwrap()["originalLanguage"] = json!("ara");
    let upstream = Router::new()
        .route(
            "/shows/en/{id}",
            get(|State(source): State<Arc<Mutex<Value>>>| async move {
                Json(source.lock().unwrap().clone())
            }),
        )
        .with_state(source.clone());
    let (origin, upstream_task) = serve(upstream).await;
    let metadata = Arc::new(
        hrrdarr::metadata::MetadataClient::with_origins(
            &format!("{origin}/"),
            &format!("{origin}/"),
        )
        .unwrap(),
    );
    let path = scratch.0.join("db");
    let db = Arc::new(Database::open_local(&path).await.unwrap());
    let (base, api_task) = serve(
        library::router(db.clone()).merge(library::metadata_router(db.clone(), metadata.clone())),
    )
    .await;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();
    let root = scratch.0.join("tv");
    std::fs::create_dir(&root).unwrap();
    let (status, added) = request(
        &client,
        &base,
        "POST",
        "/api/v1/tv/series/lookup",
        Some(json!({"tvdb_id":101,"path":root,"settings":{"monitored":false}})),
    )
    .await;
    assert_eq!(status, 201, "{added}");
    let id = added["id"].as_i64().unwrap();
    let c = db.connect().await.unwrap();
    assert_eq!(scalar(&c, "SELECT original_language FROM series").await, 26); // TV Arabic is 26; movies use 31.
    c.execute(
        "INSERT INTO episode_files(id,series_id,path)VALUES(1,?,'/owned-fixture/original.mkv')",
        [id],
    )
    .await
    .unwrap();
    c.execute(
        "UPDATE episodes SET episode_file_id=1 WHERE tvdb_id=502",
        (),
    )
    .await
    .unwrap();
    let captured = refresh::capture(&c, Target::Tv { series_id: id })
        .await
        .unwrap();
    for wire in [
        Some(json!("ISL")),
        None,
        Some(Value::Null),
        Some(json!("zz")),
        Some(json!("English")),
        Some(json!("")),
    ] {
        let mut body = show();
        if let Some(wire) = wire {
            body["originalLanguage"] = wire;
        }
        *source.lock().unwrap() = body;
        let detail = metadata.series(101).await.unwrap();
        let tx = c.transaction().await.unwrap();
        refresh::apply(&tx, &captured, Details::Series(detail))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(scalar(&c, "SELECT original_language FROM series").await, 9);
        assert_eq!(scalar(&c, "SELECT monitored FROM series").await, 0);
        assert_eq!(
            scalar(&c, "SELECT episode_file_id FROM episodes WHERE tvdb_id=502").await,
            1
        );
    }
    // Invalid producer values never enter the write transaction.
    for wire in [
        json!(31),
        json!({"id":26}),
        json!("ar/private"),
        json!("x".repeat(33)),
    ] {
        source.lock().unwrap()["originalLanguage"] = wire;
        assert!(metadata.series(101).await.is_err());
        assert_eq!(scalar(&c, "SELECT original_language FROM series").await, 9);
    }
    source.lock().unwrap()["originalLanguage"] = json!("ar");
    source.lock().unwrap()["title"] = json!("Must roll back");
    c.execute_batch("CREATE TRIGGER fixture_metadata_failure BEFORE UPDATE ON episodes BEGIN SELECT RAISE(ABORT,'fixture late failure'); END;").await.unwrap();
    source.lock().unwrap()["episodes"][1]["title"] = json!("Changed");
    let tx = c.transaction().await.unwrap();
    assert!(
        refresh::apply(
            &tx,
            &captured,
            Details::Series(metadata.series(101).await.unwrap())
        )
        .await
        .is_err()
    );
    tx.rollback().await.unwrap();
    assert_eq!(scalar(&c, "SELECT original_language FROM series").await, 9);
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM series WHERE title='Fixture series'"
        )
        .await,
        1
    );
    c.execute("DROP TRIGGER fixture_metadata_failure", ())
        .await
        .unwrap();
    // New library with absent metadata remains NULL, not English or inherited from the first series.
    let mut absent = show();
    absent["tvdbId"] = json!(303);
    absent["episodes"] = json!([]);
    absent["seasons"] = json!([]);
    *source.lock().unwrap() = absent;
    let root = scratch.0.join("absent");
    std::fs::create_dir(&root).unwrap();
    let (status, added) = request(
        &client,
        &base,
        "POST",
        "/api/v1/tv/series/lookup",
        Some(json!({"tvdb_id":303,"path":root})),
    )
    .await;
    assert_eq!(status, 201, "{added}");
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM series WHERE tvdb_id=303 AND original_language IS NULL"
        )
        .await,
        1
    );
    api_task.abort();
    let _ = api_task.await;
    upstream_task.abort();
    let _ = upstream_task.await;
    drop(c);
    drop(db);
    let db = Database::open_local(&path).await.unwrap();
    let c = db.connect().await.unwrap();
    assert_eq!(
        scalar(&c, "SELECT original_language FROM series WHERE tvdb_id=101").await,
        9
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM series WHERE tvdb_id=303 AND original_language IS NULL"
        )
        .await,
        1
    );
    assert_eq!(
        scalar(&c, "SELECT episode_file_id FROM episodes WHERE tvdb_id=502").await,
        1
    );
}
