//! Opt-in local browser fixture. No catalog/library rows are inserted directly.
use axum::{
    Json, Router,
    body::Bytes,
    extract::{OriginalUri, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use hrrdarr::{db::Database, episodes, import, library, metadata::MetadataClient, providers};
use serde_json::json;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
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
async fn tv_detail(Path(id): Path<String>) -> Response {
    if id == "101" {
        Json(show()).into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}
async fn movie_detail(Path(id): Path<String>) -> Response {
    if id == "101" {
        Json(movie()).into_response()
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
struct ProviderObservations(Mutex<Vec<serde_json::Value>>);
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
    match uri.path() {
        "/api/v2/app/webapiVersion"=>"2.8.3".into_response(),
        "/api/v2/app/version"=>"v4.6.0".into_response(),
        "/api/v2/app/preferences"=>Json(json!({"queueing_enabled":true,"max_ratio_enabled":false,"max_ratio":-1,"max_seeding_time_enabled":false,"max_seeding_time":-1,"max_ratio_act":0})).into_response(),
        "/api/v2/torrents/categories"=>Json(json!({"tv":{"savePath":"/fixture/tv"},"movies":{"savePath":"/fixture/movies"}})).into_response(),
        "/api/v2/torrents/info" if query.get("category").is_some_and(|v|matches!(v.as_str(),"tv"|"movies"))=>Json(json!([])).into_response(),
        _=>StatusCode::NOT_FOUND.into_response(),
    }
}

/// Run explicitly, read the printed manifest, then create its shutdown file to stop.
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
            .route("/shows/en/{id}", get(tv_detail))
            .route("/movie/{id}", get(movie_detail))
            .route("/search/en", get(search))
            .route("/search", get(search)),
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
            .fallback(provider_mock)
            .with_state(Arc::new(ProviderObservations::default())),
    )
    .await;
    let key = Arc::new(providers::CredentialKey::from_hex(&"42".repeat(32)).unwrap());
    let (api, _api) = serve(
        library::router(db.clone())
            .merge(library::metadata_router(db.clone(), client))
            .merge(episodes::router(db.clone()))
            .merge(import::router(db.clone()))
            .merge(providers::router(db, Some(key))),
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
    _metadata.stop().await;
    _providers.stop().await;
}
