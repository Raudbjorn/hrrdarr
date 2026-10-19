//! Shared in-process fixtures for import-list tests: a scriptable fake list source, a fake metadata
//! service (the library add path's catalog), a real import-list API on a loopback port and a real
//! database. No real third-party service is contacted. Include with `mod import_lists_support;`.
#![allow(dead_code)]
use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::get,
};
use hrrdarr::{
    db::Database,
    import_lists::{self, Deps, sync::Engine},
    metadata::MetadataClient,
    notifications::transport::Transport,
    providers::CredentialKey,
    quality_profiles,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex},
};

pub const KEY_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
/// Production origins of fixed-endpoint providers; the harness rewrites each to the fake source.
/// A provider adds its origin here when it is implemented.
pub const FIXED_ORIGINS: &[&str] = &[
    "https://api.trakt.tv",
    "https://api.themoviedb.org",
    "https://metadata.provider.plex.tv",
    "https://api.myanimelist.net",
    "https://graphql.anilist.co",
    "https://api.simkl.com",
    "https://www.imdb.com",
    "https://s3.amazonaws.com",
];

#[derive(Clone, Debug)]
pub struct Seen {
    pub method: String,
    pub target: String,
    pub headers: HashMap<String, String>,
    pub body: String,
}
impl Seen {
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}
type Route = (u16, String, Vec<u8>);
/// Fake list source: routes are exact `path` or `path?query` strings set by the test.
#[derive(Clone, Default)]
pub struct Feed {
    routes: Arc<Mutex<HashMap<String, Route>>>,
    pub seen: Arc<Mutex<Vec<Seen>>>,
}
impl Feed {
    pub fn set(&self, target: &str, status: u16, content_type: &str, body: impl Into<Vec<u8>>) {
        self.routes
            .lock()
            .unwrap()
            .insert(target.to_owned(), (status, content_type.to_owned(), body.into()));
    }
    pub fn json(&self, target: &str, body: Value) {
        self.set(target, 200, "application/json", body.to_string());
    }
    pub fn requests(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
    pub fn clear(&self) {
        self.seen.lock().unwrap().clear();
    }
}
async fn feed_handler(
    State(feed): State<Feed>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let target = uri
        .path_and_query()
        .map_or(uri.path().to_owned(), |p| p.as_str().to_owned());
    feed.seen.lock().unwrap().push(Seen {
        method: method.to_string(),
        target: target.clone(),
        headers: headers
            .iter()
            .map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap_or("").to_owned()))
            .collect(),
        body: String::from_utf8_lossy(&body).into_owned(),
    });
    let routes = feed.routes.lock().unwrap();
    match routes.get(&target).or_else(|| routes.get(uri.path())) {
        Some((status, content_type, body)) => (
            StatusCode::from_u16(*status).unwrap(),
            [("content-type", content_type.clone())],
            body.clone(),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Fake metadata catalog. A show or movie must be registered to be found.
#[derive(Clone, Default)]
pub struct Meta {
    shows: Arc<Mutex<BTreeMap<i64, (String, Option<i64>)>>>,
    movies: Arc<Mutex<BTreeMap<i64, (String, Option<i64>, Option<String>)>>>,
    pub down: Arc<std::sync::atomic::AtomicBool>,
    pub lookups: Arc<Mutex<Vec<String>>>,
}
impl Meta {
    pub fn show(&self, tvdb: i64, title: &str, year: Option<i64>) {
        self.shows.lock().unwrap().insert(tvdb, (title.to_owned(), year));
    }
    pub fn movie(&self, tmdb: i64, title: &str, year: Option<i64>, imdb: Option<&str>) {
        self.movies
            .lock()
            .unwrap()
            .insert(tmdb, (title.to_owned(), year, imdb.map(str::to_owned)));
    }
}
fn show_json(id: i64, title: &str, year: Option<i64>) -> Value {
    json!({"tvdbId":id,"title":title,"firstAired":format!("{}-01-01",year.unwrap_or(2020)),
        "seasons":[{"seasonNumber":0},{"seasonNumber":1},{"seasonNumber":2}],
        "episodes":[
            {"tvdbId":id*10+1,"seasonNumber":1,"episodeNumber":1,"title":"Pilot","airDate":"2020-01-02","airDateUtc":"2020-01-02T12:00:00Z"},
            {"tvdbId":id*10+2,"seasonNumber":2,"episodeNumber":1,"title":"Return","airDate":"2021-01-02","airDateUtc":"2021-01-02T12:00:00Z"}]})
}
fn movie_json(id: i64, title: &str, year: Option<i64>, imdb: &Option<String>) -> Value {
    let mut v = json!({"tmdbId":id,"title":title});
    if let Some(y) = year {
        v["year"] = json!(y);
    }
    if let Some(i) = imdb {
        v["imdbId"] = json!(i);
    }
    v
}
async fn meta_show(State(m): State<Meta>, Path(id): Path<i64>) -> Response {
    m.lookups.lock().unwrap().push(format!("show:{id}"));
    if m.down.load(std::sync::atomic::Ordering::SeqCst) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    match m.shows.lock().unwrap().get(&id) {
        Some((t, y)) => Json(show_json(id, t, *y)).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
async fn meta_movie(State(m): State<Meta>, Path(id): Path<i64>) -> Response {
    m.lookups.lock().unwrap().push(format!("movie:{id}"));
    if m.down.load(std::sync::atomic::Ordering::SeqCst) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    match m.movies.lock().unwrap().get(&id) {
        Some((t, y, i)) => Json(movie_json(id, t, *y, i)).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
async fn meta_movie_imdb(State(m): State<Meta>, Path(imdb): Path<String>) -> Response {
    m.lookups.lock().unwrap().push(format!("imdb:{imdb}"));
    if m.down.load(std::sync::atomic::Ordering::SeqCst) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    match m.movies.lock().unwrap().iter().find(|(_, (_, _, i))| i.as_deref() == Some(imdb.as_str())) {
        Some((id, (t, y, i))) => Json(movie_json(*id, t, *y, i)).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
async fn meta_search_tv(State(m): State<Meta>, Query(q): Query<HashMap<String, String>>) -> Response {
    let term = q.get("term").cloned().unwrap_or_default();
    m.lookups.lock().unwrap().push(format!("tv-search:{term}"));
    if m.down.load(std::sync::atomic::Ordering::SeqCst) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let needle = term.to_lowercase();
    Json(
        m.shows
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, (t, _))| t.to_lowercase().contains(&needle))
            .map(|(id, (t, y))| show_json(*id, t, *y))
            .collect::<Vec<_>>(),
    )
    .into_response()
}
async fn meta_search_movie(State(m): State<Meta>, Query(q): Query<HashMap<String, String>>) -> Response {
    let term = q.get("q").cloned().unwrap_or_default();
    m.lookups.lock().unwrap().push(format!("movie-search:{term}"));
    if m.down.load(std::sync::atomic::Ordering::SeqCst) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let needle = term.to_lowercase();
    Json(
        m.movies
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, (t, _, _))| t.to_lowercase().contains(&needle))
            .map(|(id, (t, y, i))| movie_json(*id, t, *y, i))
            .collect::<Vec<_>>(),
    )
    .into_response()
}

pub struct Harness {
    pub dir: std::path::PathBuf,
    pub db: Arc<Database>,
    pub engine: Arc<Engine>,
    pub feed: Feed,
    pub feed_base: String,
    pub meta: Meta,
    pub api_base: String,
    pub tv_profile: i64,
    pub movie_profile: i64,
    pub tv_root: i64,
    pub movie_root: i64,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}
impl Drop for Harness {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
async fn serve(router: Router) -> (String, tokio::task::JoinHandle<()>) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    (base, tokio::spawn(async move { axum::serve(l, router).await.unwrap() }))
}
impl Harness {
    pub async fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("hrrdarr-lists-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let db = Arc::new(Database::open_local(dir.join("db")).await.unwrap());
        quality_profiles::initialize_defaults(&db).await.unwrap();
        let feed = Feed::default();
        let (feed_base, t1) = serve(Router::new().fallback(feed_handler).with_state(feed.clone())).await;
        let meta = Meta::default();
        let (meta_base, t2) = serve(
            Router::new()
                .route("/shows/en/{id}", get(meta_show))
                .route("/search/en", get(meta_search_tv))
                .route("/movie/imdb/{id}", get(meta_movie_imdb))
                .route("/movie/{id}", get(meta_movie))
                .route("/search", get(meta_search_movie))
                .with_state(meta.clone()),
        )
        .await;
        let metadata = Arc::new(MetadataClient::with_origins(&format!("{meta_base}/"), &format!("{meta_base}/")).unwrap());
        let map = FIXED_ORIGINS.iter().map(|o| ((*o).to_owned(), feed_base.clone())).collect();
        let deps = Deps {
            transport: Arc::new(Transport::new().unwrap().with_origin_map(map)),
            metadata,
        };
        let key = Some(Arc::new(CredentialKey::from_hex(KEY_HEX).unwrap()));
        let engine = Arc::new(import_lists::engine(db.clone(), key, deps));
        let (api_base, t3) = serve(import_lists::api::router(engine.clone())).await;
        let c = db.connect().await.unwrap();
        let mut ids = [0i64; 2];
        for (i, media) in ["tv", "movies"].iter().enumerate() {
            let mut rows = c
                .query("SELECT min(id) FROM quality_profiles WHERE media_type=?", [*media])
                .await
                .unwrap();
            ids[i] = rows.next().await.unwrap().unwrap().get(0).unwrap();
            std::fs::create_dir(dir.join(media)).unwrap();
            c.execute(
                "INSERT INTO root_folders(media_type,path) VALUES(?,?)",
                libsql::params![*media, dir.join(media).to_str().unwrap().to_owned()],
            )
            .await
            .unwrap();
        }
        let mut rows = c.query("SELECT id FROM root_folders ORDER BY media_type DESC", ()).await.unwrap();
        let tv_root: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
        let movie_root: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
        Self {
            dir,
            db,
            engine,
            feed,
            feed_base,
            meta,
            api_base,
            tv_profile: ids[0],
            movie_profile: ids[1],
            tv_root,
            movie_root,
            tasks: vec![t1, t2, t3],
        }
    }
    pub async fn call(&self, method: &str, path: &str, body: Value) -> (u16, Value) {
        let mut request = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .unwrap()
            .request(method.parse().unwrap(), format!("{}{path}", self.api_base));
        if !body.is_null() {
            request = request.header("content-type", "application/json").body(body.to_string());
        }
        let r = request.send().await.unwrap();
        let status = r.status().as_u16();
        let text = r.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }
    /// Minimal valid input for a list of `implementation` in `media` (`tv` or `movies`).
    pub fn input(&self, name: &str, implementation: &str, media: &str, settings: Value) -> Value {
        let tv = media == "tv";
        json!({"name":name,"implementation":implementation,"media_type":media,
            "quality_profile_id":if tv {self.tv_profile} else {self.movie_profile},
            "root_folder_id":if tv {self.tv_root} else {self.movie_root},
            "settings":settings})
    }
    pub async fn create(&self, input: Value) -> String {
        let (status, body) = self.call("POST", "/api/v1/importlist", input).await;
        assert_eq!(status, 201, "{body}");
        body["id"].as_str().unwrap().to_owned()
    }
    /// Queues a manual sync and runs every runnable pass to completion; returns the final run.
    pub async fn sync(&self, id: &str) -> Value {
        let (status, run) = self.call("POST", &format!("/api/v1/importlist/{id}/sync"), Value::Null).await;
        assert_eq!(status, 202, "{run}");
        self.engine.drain().await.unwrap();
        let (_, runs) = self.call("GET", &format!("/api/v1/importlist/{id}/runs?limit=1"), Value::Null).await;
        runs["items"][0].clone()
    }
    pub async fn items(&self, id: &str, state: &str) -> Vec<Value> {
        let (status, page) = self
            .call("GET", &format!("/api/v1/importlist/{id}/items?limit=500&state={state}"), Value::Null)
            .await;
        assert_eq!(status, 200, "{page}");
        page["items"].as_array().unwrap().clone()
    }
    pub async fn count(&self, sql: &str) -> i64 {
        let c = self.db.connect().await.unwrap();
        let mut rows = c.query(sql, ()).await.unwrap();
        rows.next().await.unwrap().unwrap().get(0).unwrap()
    }
    pub async fn sql(&self, sql: &str) {
        self.db.connect().await.unwrap().execute_batch(sql).await.unwrap();
    }
}
