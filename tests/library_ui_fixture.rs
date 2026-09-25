//! Opt-in local browser fixture. No catalog/library rows are inserted directly.
use axum::{
    Json, Router,
    extract::{Path, Query},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use hrrdarr::{db::Database, episodes, import, library, metadata::MetadataClient};
use serde_json::json;
use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};

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
    let (api, _api) = serve(
        library::router(db.clone())
            .merge(library::metadata_router(db.clone(), client))
            .merge(episodes::router(db.clone()))
            .merge(import::router(db)),
    )
    .await;
    let shutdown = scratch.0.join("shutdown");
    println!(
        "UI_FIXTURE {}",
        json!({"api":api,"metadata":metadata_origin,"scratch":scratch.0,"tv_path":tv,"movie_path":movies,"episode_source":incoming.join("episode.mkv"),"movie_source":incoming.join("movie.mkv"),"episode_destination":tv.join("pilot.mkv"),"movie_destination":movies.join("movie.mkv"),"shutdown":shutdown})
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15 * 60);
    while !shutdown.exists() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    // Join aborted servers before removing their database and scratch files.
    _api.stop().await;
    _metadata.stop().await;
}
