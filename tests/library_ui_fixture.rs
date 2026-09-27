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
    providers, quality_profiles, search as releases, snapshots,
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
    json!({"tvdbId":101,"title":"Fixture series","firstAired":"2020-01-01","seasons":[{"seasonNumber":1}],"episodes":[{"tvdbId":501,"seasonNumber":1,"episodeNumber":1,"title":"Pilot","airDate":"2020-01-01","airDateUtc":"2020-01-01T00:00:00Z","runtime":45},{"tvdbId":502,"seasonNumber":1,"episodeNumber":2,"title":"Second episode","airDate":"2020-01-02","airDateUtc":"2020-01-02T00:00:00Z","runtime":45}]})
}
fn movie() -> serde_json::Value {
    json!({"tmdbId":101,"title":"Fixture movie","year":2021,"imdbId":"tt7654321","runtime":100,"digitalRelease":"2021-01-01T00:00:00Z"})
}
fn search_show(id: u32) -> serde_json::Value {
    json!({"tvdbId":id,"title":format!("Search series {id}"),"seasons":[{"seasonNumber":1}],"episodes":[{"tvdbId":id*10,"seasonNumber":1,"episodeNumber":1,"title":"Search pilot","airDateUtc":"2099-01-01T00:00:00Z","runtime":45}]})
}
fn search_movie(id: u32) -> serde_json::Value {
    json!({"tmdbId":id,"title":format!("Search movie {id}"),"year":2030,"runtime":100,"digitalRelease":"2099-01-01T00:00:00Z"})
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
    if let Ok(id @ (202 | 203)) = id.parse::<u32>() {
        return Json(search_show(id)).into_response();
    }
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
    if let Ok(id @ (202 | 203)) = id.parse::<u32>() {
        return Json(search_movie(id)).into_response();
    }
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
    if let Some(id) = term
        .strip_prefix("Search ")
        .and_then(|id| id.parse::<u32>().ok())
        .filter(|id| matches!(id, 202 | 203))
    {
        return Json(json!([if tv {
            search_show(id)
        } else {
            search_movie(id)
        }]))
        .into_response();
    }
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
struct ProviderObservations(
    Mutex<Vec<serde_json::Value>>,
    AtomicU8,
    AtomicU8,
    Mutex<Vec<serde_json::Value>>,
);
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
async fn processing_mode(State(state): State<Arc<ProviderObservations>>) -> StatusCode {
    state.2.store(1, Ordering::SeqCst);
    StatusCode::NO_CONTENT
}
async fn search_mode(State(state): State<Arc<ProviderObservations>>) -> StatusCode {
    state.2.store(2, Ordering::SeqCst);
    StatusCode::NO_CONTENT
}
async fn completed_observations(
    State(state): State<Arc<ProviderObservations>>,
) -> Json<Vec<serde_json::Value>> {
    Json(state.3.lock().unwrap().clone())
}
async fn import_failure(
    State(db): State<Arc<Database>>,
    Query(q): Query<HashMap<String, String>>,
) -> StatusCode {
    let sql = match q.get("enabled").map(String::as_str) {
        Some("1") => {
            "CREATE TRIGGER fixture_import_failure BEFORE INSERT ON import_history BEGIN SELECT RAISE(ABORT,'owned browser fixture failure');END;"
        }
        Some("0") => "DROP TRIGGER IF EXISTS fixture_import_failure;",
        _ => return StatusCode::BAD_REQUEST,
    };
    match db.connect().await.unwrap().execute_batch(sql).await {
        Ok(_) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
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
        if uri.path() == "/newznab" {
            return ([("content-type","application/xml")],r#"<rss xmlns:x="http://www.newznab.com/DTD/2010/feeds/attributes/"><channel><x:response offset="0" total="0"/></channel></rss>"#).into_response();
        }
        let tv = query.get("cat").is_some_and(|c| c.starts_with('5'));
        if state.2.load(Ordering::SeqCst) == 2 {
            let external = query
                .get(if tv { "tvdbid" } else { "tmdbid" })
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or_else(|| {
                    if query.get("q").is_some_and(|s| s.contains("203")) {
                        203
                    } else {
                        202
                    }
                });
            if !matches!(external, 202 | 203) {
                return StatusCode::BAD_REQUEST.into_response();
            }
            let offset = query
                .get("offset")
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(0);
            let mut entries = String::new();
            if offset == 0 {
                for (quality, wrong) in [("720p", false), ("1080p", false), ("1080p", true)] {
                    let title = if tv {
                        format!("Search.series.{external}.S01E01.{quality}.WEB-DL")
                    } else {
                        format!("Search.movie.{external}.2030.{quality}.WEB-DL")
                    };
                    let digit = match (tv, external, quality) {
                        (true, 202, "720p") => "1",
                        (true, 202, _) => "2",
                        (false, 202, "720p") => "3",
                        (false, 202, _) => "4",
                        (true, 203, "720p") => "5",
                        (true, 203, _) => "6",
                        (false, 203, "720p") => "7",
                        _ => "8",
                    };
                    let hash = digit.repeat(40);
                    let category = if tv != wrong { 5030 } else { 2000 };
                    entries.push_str(&format!(r#"<item><title>{title}</title><guid>search-{external}-{hash}-{wrong}</guid><pubDate>Thu, 24 Sep 2026 12:00:00 +0000</pubDate><link>magnet:?xt=urn:btih:{hash}</link><x:attr name="category" value="{category}"/><x:attr name="size" value="1073741824"/></item>"#));
                }
            }
            return ([("content-type","application/xml")],format!(r#"<rss xmlns:x="http://www.newznab.com/DTD/2010/feeds/attributes/"><channel><x:response offset="{offset}" total="3"/>{entries}</channel></rss>"#)).into_response();
        }
        let title = match (tv, state.2.load(Ordering::SeqCst)) {
            (true, 0) => "Fixture.series.S01E02.1080p.WEB-DL",
            (false, 0) => "Fixture.movie.2021.1080p.WEB-DL",
            (true, _) => "Refreshed.series.S01E02.1080p.WEB-DL",
            (false, _) => "Refreshed.movie.2021.1080p.WEB-DL",
        };
        let category = if tv { "5030" } else { "2000" };
        let identity = if tv { "tvdbid" } else { "tmdbid" };
        let hash = if tv {
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        } else {
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        };
        let receipt_suffix = if state.2.load(Ordering::SeqCst) == 1 {
            "-completed"
        } else {
            ""
        };
        return ([("content-type", "application/xml")],format!(r#"<rss xmlns:x="http://www.newznab.com/DTD/2010/feeds/attributes/"><channel><x:response offset="0" total="1"/><item><title>{title}</title><guid>fixture-{category}{receipt_suffix}</guid><pubDate>Thu, 24 Sep 2026 12:00:00 +0000</pubDate><link>magnet:?xt=urn:btih:{hash}</link><x:attr name="magneturl" value="magnet:?xt=urn:btih:{hash}"/><x:attr name="category" value="{category}"/><x:attr name="{identity}" value="101"/><x:attr name="size" value="1073741824"/></item></channel></rss>"#)).into_response();
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
    if state.2.load(Ordering::SeqCst) >= 1 {
        let name = |tv| {
            if tv {
                "Refreshed.series.S01E02.1080p.WEB-DL.mkv"
            } else {
                "Refreshed.movie.2021.1080p.WEB-DL.mkv"
            }
        };
        if uri.path() == "/api/v2/torrents/add" {
            let fields: HashMap<String, String> =
                url::form_urlencoded::parse(&body).into_owned().collect();
            let Some(category) = fields
                .get("category")
                .filter(|v| matches!(v.as_str(), "tv" | "movies"))
            else {
                return StatusCode::BAD_REQUEST.into_response();
            };
            let tv = category == "tv";
            let hash = if state.2.load(Ordering::SeqCst) == 2 {
                let magnet = url::Url::parse(fields.get("urls").unwrap()).unwrap();
                magnet
                    .query_pairs()
                    .find(|(key, _)| key == "xt")
                    .unwrap()
                    .1
                    .strip_prefix("urn:btih:")
                    .unwrap()
                    .to_string()
            } else if tv {
                "a".repeat(40)
            } else {
                "b".repeat(40)
            };
            let searching = state.2.load(Ordering::SeqCst) == 2;
            state.3.lock().unwrap().push(json!({"hash":hash,"category":category,"name":name(tv),"state":if searching {"downloading"}else{"uploading"},"progress":if searching {0.5}else{1.0},"size":1048576,"amount_left":if searching {524288}else{0},"dlspeed":0,"upspeed":0,"ratio":0.0,"seeding_time":0,"ratio_limit":-2.0,"seeding_time_limit":-2,"inactive_seeding_time_limit":-1,"seq_dl":false,"f_l_piece_prio":false,"auto_tmm":false,"force_start":false,"priority":5,"tags":"","save_path":"/remote","content_path":format!("/remote/{}",name(tv))}));
            return "Ok.".into_response();
        }
        if uri.path() == "/api/v2/torrents/setForceStart" {
            let fields: HashMap<String, String> =
                url::form_urlencoded::parse(&body).into_owned().collect();
            let Some(hash) = fields.get("hashes") else {
                return StatusCode::BAD_REQUEST.into_response();
            };
            for row in state
                .3
                .lock()
                .unwrap()
                .iter_mut()
                .filter(|r| r["hash"] == hash.as_str())
            {
                row["force_start"] = json!(fields.get("value").is_some_and(|v| v == "true"));
            }
            return "Ok.".into_response();
        }
        if uri.path() == "/api/v2/torrents/info" {
            let rows: Vec<_> = state
                .3
                .lock()
                .unwrap()
                .iter()
                .filter(|r| {
                    query
                        .get("category")
                        .is_none_or(|v| r["category"] == v.as_str())
                        && query
                            .get("hashes")
                            .is_none_or(|v| v.split('|').any(|h| r["hash"] == h))
                })
                .cloned()
                .collect();
            return Json(rows).into_response();
        }
        if uri.path() == "/api/v2/torrents/files" {
            let tv = query.get("hash").is_some_and(|v| v == &"a".repeat(40));
            return Json(
                json!([{"index":0,"name":name(tv),"size":1048576,"progress":1.0,"priority":1}]),
            )
            .into_response();
        }
        if uri.path() == "/api/v2/torrents/properties" {
            return Json(json!({"save_path":"/remote","total_size":1048576,"addition_date":1,"completion_date":2,"seeding_time":0})).into_response();
        }
    }
    match uri.path() {
        "/api/v2/app/webapiVersion"=>"2.8.3".into_response(),
        "/api/v2/app/version"=>"v4.6.0".into_response(),
        "/api/v2/app/preferences"=>Json(json!({"queueing_enabled":true,"dht":true,"max_ratio_enabled":false,"max_ratio":-1,"max_seeding_time_enabled":false,"max_seeding_time":-1,"max_ratio_act":0})).into_response(),
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
async fn blocklist_fixture(
    db: Arc<Database>,
    root: &std::path::Path,
    external_id: i64,
) -> Arc<BlocklistFixture> {
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
    c.execute("UPDATE Series SET TvdbId=?", [external_id])
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
    c.execute(
        "UPDATE MovieMetadata SET TmdbId=?,ImdbId=?",
        libsql::params![external_id, format!("tt7654{external_id}")],
    )
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
    for id in [202, 203] {
        for domain in ["tv", "movies"] {
            std::fs::create_dir(scratch.0.join(format!("search-{domain}-{id}"))).unwrap();
        }
    }
    for name in [
        "Refreshed.series.S01E02.1080p.WEB-DL.mkv",
        "Refreshed.movie.2021.1080p.WEB-DL.mkv",
    ] {
        std::fs::write(incoming.join(name), vec![7u8; 1048576]).unwrap();
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
            .route("/fixture-processing", post(processing_mode))
            .route("/fixture-search", post(search_mode))
            .route("/fixture-completed", get(completed_observations))
            .fallback(provider_mock)
            .with_state(Arc::new(ProviderObservations::default())),
    )
    .await;
    let key = Arc::new(providers::CredentialKey::from_hex(&"42".repeat(32)).unwrap());
    let (provider_routes, refresh) = providers::router_with_refresh(db.clone(), Some(key));
    let snapshot_fixture = blocklist_fixture(db.clone(), &scratch.0, 901).await;
    let clear_root = scratch.0.join("clear-source");
    std::fs::create_dir(&clear_root).unwrap();
    let clear_fixture = blocklist_fixture(db.clone(), &clear_root, 902).await;
    let worker = commands::start_with_metadata(db.clone(), refresh.clone(), client.clone())
        .await
        .unwrap();
    let (api, _api) = serve(
        library::router(db.clone())
            .merge(library::metadata_router(db.clone(), client))
            .merge(episodes::router(db.clone()))
            .merge(import::router(db.clone()))
            .merge(provider_routes)
            .merge(commands::router(db.clone()))
            .merge(releases::router(db.clone(), refresh))
            .merge(quality_profiles::router(db.clone()))
            .merge(hrrdarr::qualities::router(db.clone()))
            .merge(hrrdarr::tags::router(db.clone()))
            .merge(hrrdarr::remote_paths::router(db.clone()))
            .merge(hrrdarr::media_files::router(db.clone()))
            .merge(hrrdarr::naming::router(db.clone()))
            .merge(hrrdarr::custom_formats::router(db.clone()))
            .merge(hrrdarr::root_folders::router(db.clone()))
            .merge(
                Router::new()
                    .route("/api/fixture/fail-import-history", post(import_failure))
                    .with_state(db.clone()),
            )
            .merge(blocklist::router(db))
            .merge(
                Router::new()
                    .route(
                        "/api/fixture/import-clear-blocklists",
                        post(import_blocklists),
                    )
                    .with_state(clear_fixture),
            )
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
