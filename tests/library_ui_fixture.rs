//! Opt-in local browser fixture. No catalog/library rows are inserted directly.
use axum::{
    Json, Router,
    body::Bytes,
    extract::{OriginalUri, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use hrrdarr::{
    blocklist, commands, db::Database, episodes, import, library, metadata::MetadataClient,
    providers, snapshots,
};
use serde_json::json;
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
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Server(Option<tokio::task::JoinHandle<()>>);
impl Server {
    async fn stop(mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
            let _ = task.await;
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        if let Some(task) = &self.0 {
            task.abort();
        }
    }
}
async fn serve(router: Router) -> (String, Server) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    (
        origin,
        Server(Some(tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap()
        }))),
    )
}
fn show() -> serde_json::Value {
    json!({"tvdbId":101,"title":"Fixture series","firstAired":"2020-01-01","seasons":[{"seasonNumber":1}],"episodes":[{"tvdbId":501,"seasonNumber":1,"episodeNumber":1,"title":"Pilot","airDate":"2020-01-01"},{"tvdbId":502,"seasonNumber":1,"episodeNumber":2,"title":"Second episode","airDate":"2020-01-02"}]})
}
fn movie() -> serde_json::Value {
    json!({"tmdbId":101,"title":"Fixture movie","year":2021,"imdbId":"tt7654321"})
}
async fn metadata_mode(
    State(state): State<Arc<AtomicU8>>,
    Query(query): Query<HashMap<String, String>>,
) -> StatusCode {
    let value = match query.get("mode").map(String::as_str) {
        Some("0") => 0,
        Some("1") => 1,
        Some("2") => 2,
        Some("3") => 3,
        _ => return StatusCode::BAD_REQUEST,
    };
    state.store(value, Ordering::SeqCst);
    StatusCode::NO_CONTENT
}
async fn tv_detail(State(state): State<Arc<AtomicU8>>, Path(id): Path<String>) -> Response {
    let mode = state.load(Ordering::SeqCst);
    if mode == 2 {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if mode == 3 {
        tokio::time::sleep(Duration::from_secs(3)).await;
    }

    if id == "101" {
        let mut value = show();
        if mode == 1 || mode == 3 {
            value["title"] = json!("Refreshed series");
            value["episodes"][0]["title"] = json!("Refreshed pilot");
            value["episodes"].as_array_mut().unwrap().push(json!({"tvdbId":503,"seasonNumber":1,"episodeNumber":3,"title":"New episode","airDate":"2020-01-03"}));
        }
        Json(value).into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}
async fn movie_detail(State(state): State<Arc<AtomicU8>>, Path(id): Path<String>) -> Response {
    let mode = state.load(Ordering::SeqCst);
    if mode == 2 {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if mode == 3 {
        tokio::time::sleep(Duration::from_secs(3)).await;
    }

    if id == "101" {
        let mut value = movie();
        if mode == 1 || mode == 3 {
            value["title"] = json!("Refreshed movie");
        }
        Json(value).into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}
async fn search(Query(q): Query<HashMap<String, String>>) -> Response {
    let tv = q.contains_key("term");
    let term = q
        .get(if tv { "term" } else { "q" })
        .map(String::as_str)
        .unwrap_or("");
    if term == "slow" {
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Json(if term == "empty" {
        json!([])
    } else if tv {
        json!([show()])
    } else {
        json!([movie()])
    })
    .into_response()
}

#[derive(Default)]
struct ProviderObservations(Mutex<Vec<serde_json::Value>>, AtomicU8);
async fn mode(
    State(state): State<Arc<ProviderObservations>>,
    Query(query): Query<HashMap<String, String>>,
) -> StatusCode {
    let value = match query.get("mode").map(String::as_str) {
        Some("0") => 0,
        Some("1") => 1,
        Some("2") => 2,
        _ => return StatusCode::BAD_REQUEST,
    };
    state.1.store(value, Ordering::SeqCst);
    StatusCode::NO_CONTENT
}
async fn observations(
    State(state): State<Arc<ProviderObservations>>,
) -> Json<Vec<serde_json::Value>> {
    Json(state.0.lock().unwrap().clone())
}
// Synthetic credentials are accepted only by this owned loopback server.
async fn provider_mock(
    State(state): State<Arc<ProviderObservations>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let query: HashMap<String, String> =
        url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();
    {
        let mut seen = state.0.lock().unwrap();
        if seen.len() < 256 {
            seen.push(json!({"path":uri.path(),"category":query.get("cat").or_else(||query.get("category")),"private_tv":query.get("private_tv").is_some_and(|v|v=="fixture-tv"),"private_movie":query.get("private_movie").is_some_and(|v|v=="fixture-movie")}));
        }
    }
    if matches!(uri.path(), "/torznab" | "/newznab") {
        if query.get("t").is_some_and(|v| v == "caps") {
            return ([("content-type","application/xml")],r#"<caps><limits max="100" default="10"/><searching><search available="yes" supportedParams="q"/><tv-search available="yes" supportedParams="q,tvdbid,season,ep"/><movie-search available="yes" supportedParams="q,tmdbid,imdbid"/></searching><categories><category id="5030"/><category id="5070"/><category id="2000"/></categories></caps>"#).into_response();
        }
        if !query
            .get("apikey")
            .is_some_and(|v| matches!(v.as_str(), "fixture-good" | "fixture-replacement"))
        {
            return (
                [("content-type", "application/xml")],
                r#"<error code="100" description="PRIVATE_FIXTURE_AUTH_FAILURE"/>"#,
            )
                .into_response();
        }
        if !query
            .get("cat")
            .is_some_and(|v| matches!(v.as_str(), "5030" | "5030,5070" | "2000"))
        {
            return StatusCode::BAD_REQUEST.into_response();
        }
        return ([("content-type","application/xml")],r#"<rss xmlns:x="http://www.newznab.com/DTD/2010/feeds/attributes/"><channel><x:response offset="0" total="0"/></channel></rss>"#).into_response();
    }
    if uri.path() == "/api/v2/auth/login" {
        let fields: HashMap<String, String> =
            url::form_urlencoded::parse(&body).into_owned().collect();
        return if fields
            .get("password")
            .is_some_and(|v| matches!(v.as_str(), "fixture-good" | "fixture-replacement"))
        {
            (
                [("set-cookie", "SID=fixture-session; HttpOnly; Path=/")],
                "Ok.",
            )
                .into_response()
        } else {
            "Fails.".into_response()
        };
    }
    let authorized = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .is_some_and(|v| matches!(v, "Bearer fixture-good" | "Bearer fixture-replacement"))
        || headers
            .get("cookie")
            .and_then(|h| h.to_str().ok())
            .is_some_and(|v| v.contains("SID=fixture-session"));
    if !authorized {
        return (StatusCode::UNAUTHORIZED, "PRIVATE_FIXTURE_AUTH_FAILURE").into_response();
    }
    if uri.path() == "/api/v2/torrents/info" {
        match state.1.load(Ordering::SeqCst) {
            1 => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "PRIVATE_FIXTURE_REFRESH_FAILURE",
                )
                    .into_response();
            }
            2 => tokio::time::sleep(Duration::from_secs(3)).await,
            _ => {}
        }
    }
    match uri.path() {
        "/api/v2/app/webapiVersion"=>"2.8.3".into_response(),
        "/api/v2/app/version"=>"v4.6.0".into_response(),
        "/api/v2/app/preferences"=>Json(json!({"queueing_enabled":true,"max_ratio_enabled":false,"max_ratio":-1,"max_seeding_time_enabled":false,"max_seeding_time":-1,"max_ratio_act":0})).into_response(),
        "/api/v2/torrents/categories"=>Json(json!({"tv":{"savePath":"/fixture/tv"},"movies":{"savePath":"/fixture/movies"}})).into_response(),
        "/api/v2/torrents/info" if query.get("category").is_some_and(|v|matches!(v.as_str(),"tv"|"movies"))=>Json(json!([{"hash":"1111111111111111111111111111111111111111","category":query.get("category"),"name":"Fixture download","state":"downloading","progress":0.5,"size":100,"amount_left":50,"dlspeed":1,"upspeed":0,"ratio":0.0}])).into_response(),
        _=>StatusCode::NOT_FOUND.into_response(),
    }
}

/// Run explicitly, read the printed manifest, then create its shutdown file to stop.
struct BlocklistFixture {
    db: Arc<Database>,
    tv: Vec<u8>,
    movies: Vec<u8>,
}
async fn import_blocklists(State(state): State<Arc<BlocklistFixture>>) -> Response {
    let mut reports = Vec::new();
    for (app, bytes) in [
        (snapshots::Application::Sonarr, &state.tv),
        (snapshots::Application::Radarr, &state.movies),
    ] {
        match snapshots::import(&state.db, app, bytes.clone(), false).await {
            Ok(report) => reports.push(report),
            Err(error) => return (StatusCode::INTERNAL_SERVER_ERROR, error.0).into_response(),
        }
    }
    Json(reports).into_response()
}
async fn blocklist_fixture(db: Arc<Database>, root: &std::path::Path) -> Arc<BlocklistFixture> {
    let tv_root = root.join("blocklist-tv");
    let movie_root = root.join("blocklist-movie");
    std::fs::create_dir(&tv_root).unwrap();
    std::fs::create_dir(&movie_root).unwrap();
    let tv_path = root.join("blocklist-sonarr.db");
    let source = libsql::Builder::new_local(&tv_path).build().await.unwrap();
    let c = source.connect().unwrap();
    c.execute_batch(r#"
      CREATE TABLE VersionInfo(Version INTEGER); INSERT INTO VersionInfo VALUES(233);
      CREATE TABLE Series(Id INTEGER,TvdbId INTEGER,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,Seasons TEXT);
      INSERT INTO Series VALUES(1,901,'Blocklist series',2020,'/fixture/blocklist-tv',1,'[{"seasonNumber":1,"monitored":true}]');
      CREATE TABLE Episodes(Id INTEGER,SeriesId INTEGER,SeasonNumber INTEGER,EpisodeNumber INTEGER,Title TEXT,Monitored INTEGER,EpisodeFileId INTEGER);
      INSERT INTO Episodes VALUES(1,1,1,1,'Blocked pilot',1,0),(2,1,1,2,'Blocked second',1,0);
      CREATE TABLE EpisodeFiles(Id INTEGER,SeriesId INTEGER,RelativePath TEXT);
      CREATE TABLE Blocklist(Id INTEGER,SeriesId INTEGER,EpisodeIds TEXT,Date TEXT,SourceTitle TEXT,Protocol INTEGER,Size INTEGER,Quality TEXT,Languages TEXT,PublishedDate TEXT,Message TEXT,Indexer TEXT);
      INSERT INTO Blocklist VALUES(1,1,'[1,2]','2026-01-02 00:00:00','Blocked TV pack',2,100,'{"quality":1}','[1]','2026-01-01 00:00:00','PRIVATE_BLOCKLIST_SENTINEL','private-indexer');
      INSERT INTO Blocklist VALUES(2,1,'[1]','2026-01-03 00:00:00','Blocked TV pilot',1,200,NULL,NULL,NULL,'PRIVATE_BLOCKLIST_SENTINEL','private-indexer');
    "#).await.unwrap();
    c.execute("UPDATE Series SET Path=?", [tv_root.to_str().unwrap()])
        .await
        .unwrap();
    for id in 3..=26 {
        c.execute("INSERT INTO Blocklist(Id,SeriesId,EpisodeIds,Date,SourceTitle,Protocol) VALUES(?,1,'[2]','2026-01-01 00:00:00',?,0)", libsql::params![id,format!("Blocked TV extra {id:02}")]).await.unwrap();
    }
    drop(c);
    drop(source);
    let movie_path = root.join("blocklist-radarr.db");
    let source = libsql::Builder::new_local(&movie_path)
        .build()
        .await
        .unwrap();
    let c = source.connect().unwrap();
    c.execute_batch(r#"
      CREATE TABLE VersionInfo(Version INTEGER); INSERT INTO VersionInfo VALUES(242);
      CREATE TABLE MovieMetadata(Id INTEGER,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER);
      INSERT INTO MovieMetadata VALUES(1,901,'tt7654999','Blocklist movie',2021);
      CREATE TABLE Movies(Id INTEGER,MovieMetadataId INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER);
      INSERT INTO Movies VALUES(1,1,'/fixture/blocklist-movie',1,0);
      CREATE TABLE MovieFiles(Id INTEGER,MovieId INTEGER,RelativePath TEXT,Edition TEXT);
      CREATE TABLE Blocklist(Id INTEGER,MovieId INTEGER,Date TEXT,SourceTitle TEXT,Protocol INTEGER,Size INTEGER,Quality TEXT,Languages TEXT,PublishedDate TEXT,Message TEXT,Indexer TEXT);
      INSERT INTO Blocklist VALUES(1,1,'2026-01-04 00:00:00','Blocked movie',2,300,'{"quality":1}','[1]',NULL,'PRIVATE_BLOCKLIST_SENTINEL','private-indexer');
    "#).await.unwrap();
    c.execute("UPDATE Movies SET Path=?", [movie_root.to_str().unwrap()])
        .await
        .unwrap();
    drop(c);
    drop(source);
    Arc::new(BlocklistFixture {
        db,
        tv: std::fs::read(tv_path).unwrap(),
        movies: std::fs::read(movie_path).unwrap(),
    })
}

#[tokio::test]
#[ignore = "manual browser fixture; owned loopback servers, max 15 minutes"]
async fn library_ui_fixture() {
    let scratch =
        Scratch(std::env::temp_dir().join(format!("hrrdarr-library-ui-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&scratch.0).unwrap();
    let tv = scratch.0.join("tv");
    let movies = scratch.0.join("movies");
    let incoming = scratch.0.join("incoming");
    for dir in [&tv, &movies, &incoming] {
        std::fs::create_dir(dir).unwrap();
    }
    std::fs::write(incoming.join("episode.mkv"), b"scratch episode media").unwrap();
    std::fs::write(incoming.join("movie.mkv"), b"scratch movie media").unwrap();
    let (metadata_origin, _metadata) = serve(
        Router::new()
            .route("/fixture-metadata-mode", post(metadata_mode))
            .route("/shows/en/{id}", get(tv_detail))
            .route("/movie/{id}", get(movie_detail))
            .route("/search/en", get(search))
            .route("/search", get(search))
            .with_state(Arc::new(AtomicU8::new(0))),
    )
    .await;
    let client = Arc::new(
        MetadataClient::with_origins(
            &format!("{metadata_origin}/"),
            &format!("{metadata_origin}/"),
        )
        .unwrap(),
    );
    let db = Arc::new(
        Database::open_local(scratch.0.join("library.db"))
            .await
            .unwrap(),
    );
    let (provider_origin, _providers) = serve(
        Router::new()
            .route("/fixture-observations", get(observations))
            .route("/fixture-mode", post(mode))
            .fallback(provider_mock)
            .with_state(Arc::new(ProviderObservations::default())),
    )
    .await;
    let key = Arc::new(providers::CredentialKey::from_hex(&"42".repeat(32)).unwrap());
    let (provider_routes, refresh) = providers::router_with_refresh(db.clone(), Some(key));
    let snapshot_fixture = blocklist_fixture(db.clone(), &scratch.0).await;
    let worker = commands::start_with_metadata(db.clone(), refresh, client.clone())
        .await
        .unwrap();
    let (api, _api) = serve(
        library::router(db.clone())
            .merge(library::metadata_router(db.clone(), client))
            .merge(episodes::router(db.clone()))
            .merge(import::router(db.clone()))
            .merge(provider_routes)
            .merge(commands::router(db.clone()))
            .merge(blocklist::router(db))
            .merge(
                Router::new()
                    .route("/api/fixture/import-blocklists", post(import_blocklists))
                    .with_state(snapshot_fixture),
            ),
    )
    .await;
    let shutdown = scratch.0.join("shutdown");
    println!(
        "UI_FIXTURE {}",
        json!({"api":api,"metadata":metadata_origin,"provider_origin":provider_origin,"scratch":scratch.0,"tv_path":tv,"movie_path":movies,"episode_source":incoming.join("episode.mkv"),"movie_source":incoming.join("movie.mkv"),"episode_destination":tv.join("pilot.mkv"),"movie_destination":movies.join("movie.mkv"),"shutdown":shutdown})
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15 * 60);
    while !shutdown.exists() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    // Join aborted servers before removing their database and scratch files.
    _api.stop().await;
    worker.shutdown().await;
    _metadata.stop().await;
    _providers.stop().await;
}
