//! Opt-in local browser fixture. Library creation uses public APIs; an explicit file-metadata
//! seed hook adds owned pagination/shared-file records after a real import.
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
use serde::Deserialize;
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

// Opt-in start barrier lets the browser cancel genuinely queued commands before any claim.
struct CommandWorkerFixture {
    runtime: tokio::sync::Mutex<Option<commands::Runtime>>,
    db: Arc<Database>,
    refresh: hrrdarr::providers::RefreshClient,
    metadata: Arc<MetadataClient>,
}
async fn start_command_worker(State(state): State<Arc<CommandWorkerFixture>>) -> StatusCode {
    let mut runtime = state.runtime.lock().await;
    if runtime.is_some() {
        return StatusCode::CONFLICT;
    }
    match commands::start_with_metadata(
        state.db.clone(),
        state.refresh.clone(),
        state.metadata.clone(),
    )
    .await
    {
        Ok(worker) => {
            *runtime = Some(worker);
            StatusCode::NO_CONTENT
        }
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

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
// Original-language profiles require explicit metadata and matching offer language facts.
// Downloads keep their original filenames; accepted receipt evidence supplies the language.
fn movie() -> serde_json::Value {
    json!({"tmdbId":101,"title":"Fixture movie","year":2021,"originalLanguage":"en","imdbId":"tt7654321","runtime":100,"digitalRelease":"2021-01-01T00:00:00Z"})
}
fn search_show(id: u32) -> serde_json::Value {
    json!({"tvdbId":id,"title":format!("Search series {id}"),"seasons":[{"seasonNumber":1}],"episodes":[{"tvdbId":id*10,"seasonNumber":1,"episodeNumber":1,"title":"Search pilot","airDateUtc":"2099-01-01T00:00:00Z","runtime":45}]})
}
fn search_movie(id: u32) -> serde_json::Value {
    json!({"tmdbId":id,"title":format!("Search movie {id}"),"year":2030,"originalLanguage":"en","runtime":100,"digitalRelease":"2099-01-01T00:00:00Z"})
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
        // Owned removed-health browser: actual404 and explicit valid restoration.
        Some("4") => 4,
        Some("5") => 5,
        // Credits modes leave the original provider and removed-health scenarios unchanged.
        Some("6") => 6,
        Some("7") => 7,
        Some("8") => 8,
        Some("9") => 9,
        Some("10") => 10,
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
    if mode == 4 {
        return StatusCode::NOT_FOUND.into_response();
    }
    if mode == 2 {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if mode == 3 {
        tokio::time::sleep(Duration::from_secs(3)).await;
    }

    if id == "101" {
        let mut value = show();
        if mode == 5 {
            value["title"] = json!("Removed <img src=x onerror=alert(1)> series");
            value["status"] = json!("continuing");
        }
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
    if mode == 4 {
        return StatusCode::NOT_FOUND.into_response();
    }
    if mode == 2 {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if mode == 3 {
        tokio::time::sleep(Duration::from_secs(3)).await;
    }

    if id == "101" {
        let mut value = movie();
        if mode == 5 {
            value["title"] = json!("Removed <img src=x onerror=alert(1)> movie");
        }
        if mode == 1 || mode == 3 {
            value["title"] = json!("Refreshed movie");
        }
        match mode {
            6 => {
                let cast: Vec<_> = (0..102).map(|index| json!({
                    "creditId":format!("cast-{index}"),"tmdbId":1000+index,
                    "name":if index < 2 { "Same Name".to_owned() } else if index == 2 { "<img src=x onerror=alert(1)>".to_owned() } else { format!("Cast person {index}") },
                    "order":index,"character":format!("Character {index}"),
                    "images":if index == 0 { vec![json!({"coverType":"headshot","url":"https://image.tmdb.org/t/p/original/owned-credits-missing.jpg"})] } else { vec![] }
                })).collect();
                value["credits"] = json!({"cast":cast,"crew":[
                    {"creditId":"crew-director","tmdbId":1000,"name":"Same Name","order":0,"department":"Directing","job":"Director"},
                    {"creditId":"crew-writer","tmdbId":1000,"name":"Same Name","order":1,"department":"Writing","job":"Writer"},
                    {"creditId":"crew-last","tmdbId":2000,"name":"Crew final","order":2,"department":"Sound","job":"Sound editor"}
                ]});
            }
            7 => {
                value["credits"] = json!({"cast":[{"creditId":"cast-0","tmdbId":1000,"name":"Replacement name","order":0,"character":"Replacement role"}],"crew":[]})
            }
            8 => value["credits"] = json!({"cast":[],"crew":[]}),
            9 => {}
            10 => {
                value["title"] = json!("Invalid refresh must not persist");
                value["credits"] = json!({"cast":[{"creditId":"unsafe","tmdbId":1000,"name":"Unsafe image","images":[{"coverType":"headshot","url":"javascript:alert(1)"}]}],"crew":[]});
            }
            _ => {}
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
    AtomicU8,
);
async fn mode(
    State(state): State<Arc<ProviderObservations>>,
    Query(query): Query<HashMap<String, String>>,
) -> StatusCode {
    let value = match query.get("mode").map(String::as_str) {
        Some("0") => 0,
        Some("1") => 1,
        Some("2") => 2,
        Some("3") => 3,
        Some("4") => 4,
        // Record an add while withholding its successful response and observation.
        Some("5") => 5,
        _ => return StatusCode::BAD_REQUEST,
    };
    state.1.store(value, Ordering::SeqCst);
    StatusCode::NO_CONTENT
}
// Independent indexer fault control keeps existing download-client fixture modes unchanged.
async fn indexer_mode(
    State(state): State<Arc<ProviderObservations>>,
    Query(query): Query<HashMap<String, String>>,
) -> StatusCode {
    let value = match query.get("mode").map(String::as_str) {
        Some("0") => 0,
        Some("1") => 1,
        _ => return StatusCode::BAD_REQUEST,
    };
    state.4.store(value, Ordering::SeqCst);
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
// Only already-submitted owned mock torrents may complete; no application rows are fabricated.
async fn complete_search(
    State(state): State<Arc<ProviderObservations>>,
    Query(query): Query<HashMap<String, String>>,
) -> StatusCode {
    let Some(hash) = query
        .get("hash")
        .filter(|hash| hash.len() == 40 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
    else {
        return StatusCode::BAD_REQUEST;
    };
    let mut downloads = state.3.lock().unwrap();
    let Some(download) = downloads
        .iter_mut()
        .find(|row| row["hash"] == hash.as_str())
    else {
        return StatusCode::NOT_FOUND;
    };
    download["state"] = json!("uploading");
    download["progress"] = json!(1.0);
    download["amount_left"] = json!(0);
    StatusCode::NO_CONTENT
}
fn search_download_name(hash: &str) -> Option<String> {
    let digit = hash.as_bytes().first().copied()?;
    if hash.len() != 40 || !hash.bytes().all(|b| b == digit) {
        return None;
    }
    let (tv, external, quality) = match digit {
        b'1' => (true, 202, "720p"),
        b'2' => (true, 202, "1080p"),
        b'3' => (false, 202, "720p"),
        b'4' => (false, 202, "1080p"),
        b'5' => (true, 203, "720p"),
        b'6' => (true, 203, "1080p"),
        b'7' => (false, 203, "720p"),
        b'8' => (false, 203, "1080p"),
        _ => return None,
    };
    Some(if tv {
        format!("Search.series.{external}.S01E01.{quality}.WEB-DL.mkv")
    } else {
        format!("Search.movie.{external}.2030.{quality}.WEB-DL.English.mkv")
    })
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
// This one-use hook has no production route and is inert in existing browser flows.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileMetadataSeedInput {
    series_id: i64,
}
struct SeedDirectory {
    path: Option<PathBuf>,
    retain: bool,
}
impl Drop for SeedDirectory {
    fn drop(&mut self) {
        if !self.retain {
            if let Some(path) = &self.path {
                if std::fs::remove_dir_all(path).is_err() {
                    eprintln!("event=fixture_seed_cleanup_failed");
                }
            }
        }
    }
}
async fn seed_file_metadata(
    State((db, root)): State<(Arc<Database>, PathBuf)>,
    Json(input): Json<FileMetadataSeedInput>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    use std::io::Write;
    const FILE_COUNT: i64 = 25;
    const MEDIA_BYTES: &[u8] = b"owned file metadata pagination fixture\n";
    let storage = |_| StatusCode::INTERNAL_SERVER_ERROR;
    if input.series_id <= 0 {
        return Err(StatusCode::BAD_REQUEST);
    }
    // Declare before the transaction so cancellation releases database work before file cleanup.
    let mut directory = SeedDirectory {
        path: None,
        retain: false,
    };
    let c = db.connect().await.map_err(storage)?;
    let tx = c
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await
        .map_err(storage)?;
    let result = async {
        let row = tx.query("SELECT path FROM series WHERE id=? AND tvdb_id=101 AND EXISTS(SELECT 1 FROM seasons WHERE series_id=series.id AND number=1)", [input.series_id]).await.map_err(storage)?.next().await.map_err(storage)?.ok_or(StatusCode::CONFLICT)?;
        let stored_root = PathBuf::from(row.get::<String>(0).map_err(storage)?);
        // Only the original fixture root is eligible; never follow a substituted root symlink.
        if stored_root != root || !std::fs::symlink_metadata(&root).map_err(|_| StatusCode::CONFLICT)?.is_dir() || std::fs::canonicalize(&root).map_err(|_| StatusCode::CONFLICT)? != root {
            return Err(StatusCode::CONFLICT);
        }
        let mut files = tx.query("SELECT id,path FROM episode_files WHERE series_id=? ORDER BY id LIMIT 2", [input.series_id]).await.map_err(storage)?;
        let original = files.next().await.map_err(storage)?.ok_or(StatusCode::CONFLICT)?;
        // libSQL rows borrow the statement's current values; capture them before advancing it.
        let original_id = original.get::<i64>(0).map_err(storage)?;
        let original_path = PathBuf::from(original.get::<String>(1).map_err(storage)?);
        drop(original);
        if files.next().await.map_err(storage)?.is_some() { return Err(StatusCode::CONFLICT); }
        if original_path.parent() != Some(root.as_path()) || !std::fs::symlink_metadata(&original_path).map_err(|_| StatusCode::CONFLICT)?.is_file() {
            return Err(StatusCode::CONFLICT);
        }
        drop(files);
        let valid = tx.query("SELECT 1 WHERE EXISTS(SELECT 1 FROM import_history h JOIN import_journal j ON j.operation_id=h.operation_id WHERE h.episode_file_id=?1 AND j.phase='complete') AND (SELECT count(*) FROM episodes WHERE series_id=?2 AND episode_file_id=?1)=1 AND NOT EXISTS(SELECT 1 FROM episodes WHERE series_id=?2 AND season=1 AND number BETWEEN 100 AND 199) AND NOT EXISTS(SELECT 1 FROM operations o JOIN episodes e ON e.id=o.episode_id JOIN import_journal j ON j.operation_id=o.id WHERE e.series_id=?2 AND j.phase!='complete')", libsql::params![original_id,input.series_id]).await.map_err(storage)?.next().await.map_err(storage)?.is_some();
        if !valid { return Err(StatusCode::CONFLICT); }
        let seed_root = root.join("file-metadata-seed");
        std::fs::create_dir(&seed_root).map_err(|_| StatusCode::CONFLICT)?;
        directory.path = Some(seed_root.clone());
        let mut file_ids = vec![original_id];
        for index in 0..FILE_COUNT {
            let path = seed_root.join(format!("Fixture.series.S01E{:03}.mkv",100+index));
            let mut media = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            media.write_all(MEDIA_BYTES).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            drop(media);
            let file = tx.query("INSERT INTO episode_files(series_id,path)VALUES(?,?) RETURNING id", libsql::params![input.series_id,path.to_string_lossy().into_owned()]).await.map_err(storage)?.next().await.map_err(storage)?.ok_or(StatusCode::INTERNAL_SERVER_ERROR)?.get::<i64>(0).map_err(storage)?;
            tx.execute("INSERT INTO episodes(series_id,season,number,title,episode_file_id)VALUES(?,1,?,?,?)", libsql::params![input.series_id,100+index,format!("File metadata fixture {}",index+1),file]).await.map_err(storage)?;
            tx.execute("INSERT INTO file_metadata(media_type,episode_file_id,size,season_number)VALUES('tv',?,?,1)", libsql::params![file,MEDIA_BYTES.len() as i64]).await.map_err(storage)?;
            file_ids.push(file);
        }
        tx.execute("INSERT INTO episodes(series_id,season,number,title,episode_file_id)VALUES(?,1,199,'Shared file metadata fixture',?)", libsql::params![input.series_id,original_id]).await.map_err(storage)?;
        let mut shared = tx.query("SELECT id FROM episodes WHERE series_id=? AND episode_file_id=? ORDER BY id LIMIT 3",libsql::params![input.series_id,original_id]).await.map_err(storage)?;
        let mut episode_ids = Vec::new();
        while let Some(row) = shared.next().await.map_err(storage)? { episode_ids.push(row.get::<i64>(0).map_err(storage)?); }
        if episode_ids.len()!=2 { return Err(StatusCode::CONFLICT); }
        file_ids.sort_unstable();
        Ok(json!({"series_id":input.series_id,"file_ids":file_ids,"shared_file_id":original_id,"shared_episode_ids":episode_ids}))
    }.await;
    match result {
        Ok(value) => {
            // Retain created bytes if commit reports an ambiguous error; Scratch owns final cleanup.
            // Successful commit never leaves a row referring to a file removed by the guard.
            directory.retain = true;
            tx.commit().await.map_err(storage)?;
            Ok(Json(value))
        }
        Err(error) => {
            tx.rollback().await.map_err(storage)?;
            Err(error)
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientBackoffFixtureInput {
    provider_id: uuid::Uuid,
}
// Synthetic clock seed only: real failed Test observations must already exist in both domains.
async fn age_client_failure(
    State(db): State<Arc<Database>>,
    Json(input): Json<ClientBackoffFixtureInput>,
) -> StatusCode {
    let c = db.connect().await.unwrap();
    let tx = c
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await
        .unwrap();
    let changed = tx.execute("UPDATE download_client_status SET initial_failure_at=initial_failure_at-301 WHERE provider_id=? AND initial_failure_at>301 AND last_failure_at=initial_failure_at AND escalation_level=1 AND disabled_until IS NULL", [input.provider_id.to_string()]).await.unwrap();
    if changed != 2 {
        tx.rollback().await.unwrap();
        return StatusCode::CONFLICT;
    }
    tx.commit().await.unwrap();
    StatusCode::NO_CONTENT
}
// Seed pending automatic work only after real failures have blocked both scopes; the browser exercises real manual coalescing.
async fn pending_client_refreshes(
    State(db): State<Arc<Database>>,
    Json(input): Json<ClientBackoffFixtureInput>,
) -> Response {
    let c = db.connect().await.unwrap();
    let tx = c
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await
        .unwrap();
    let mut ids = serde_json::Map::new();
    for media in ["tv", "movies"] {
        let id = uuid::Uuid::new_v4().to_string();
        let changed = tx.execute("INSERT INTO commands(id,provider_id,media_type,provider_revision,next_attempt_at,created_at,origin) SELECT ?,p.id,s.media_type,p.revision,unixepoch(),unixepoch(),'automatic' FROM providers p JOIN download_client_status s ON s.provider_id=p.id AND s.config_revision=p.revision WHERE p.id=? AND s.media_type=? AND s.disabled_until>unixepoch()+30 AND NOT EXISTS(SELECT 1 FROM commands WHERE provider_id=p.id AND media_type=s.media_type AND status IN ('queued','running','retry_wait'))", libsql::params![id.clone(),input.provider_id.to_string(),media]).await.unwrap();
        if changed != 1 {
            tx.rollback().await.unwrap();
            return StatusCode::CONFLICT.into_response();
        }
        ids.insert(media.into(), json!(id));
    }
    tx.commit().await.unwrap();
    Json(ids).into_response()
}
async fn client_backoff_state(
    State(db): State<Arc<Database>>,
    Path(id): Path<uuid::Uuid>,
) -> Json<Vec<serde_json::Value>> {
    let c = db.connect().await.unwrap();
    let mut rows = c.query("SELECT media_type,config_revision,escalation_level,initial_failure_at,last_failure_at,disabled_until,next_health_at FROM download_client_status WHERE provider_id=? ORDER BY media_type",[id.to_string()]).await.unwrap();
    let mut values = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        values.push(json!({"media_type":row.get::<String>(0).unwrap(),"config_revision":row.get::<i64>(1).unwrap(),"escalation_level":row.get::<i64>(2).unwrap(),"initial_failure_at":row.get::<Option<i64>>(3).unwrap(),"last_failure_at":row.get::<Option<i64>>(4).unwrap(),"disabled_until":row.get::<Option<i64>>(5).unwrap(),"next_health_at":row.get::<Option<i64>>(6).unwrap()}));
    }
    Json(values)
}
async fn observations(
    State(state): State<Arc<ProviderObservations>>,
) -> Json<Vec<serde_json::Value>> {
    Json(state.0.lock().unwrap().clone())
}
// Synthetic credentials are accepted only by this owned loopback server.
fn provider_router() -> Router {
    Router::new()
        .route("/fixture-observations", get(observations))
        .route("/fixture-mode", post(mode))
        .route("/fixture-indexer-mode", post(indexer_mode))
        .route("/fixture-processing", post(processing_mode))
        .route("/fixture-search", post(search_mode))
        .route("/fixture-completed", get(completed_observations))
        .route("/fixture-complete-search", post(complete_search))
        .fallback(provider_mock)
        .with_state(Arc::new(ProviderObservations::default()))
}

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
        if state.4.load(Ordering::SeqCst) == 1 {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                [("retry-after", "600")],
                "PRIVATE_FIXTURE_INDEXER_FAILURE",
            )
                .into_response();
        }
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
                        format!("Search.movie.{external}.2030.{quality}.WEB-DL.English")
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
            (false, 0) => "Fixture.movie.2021.1080p.WEB-DL.English",
            (true, _) => "Refreshed.series.S01E02.1080p.WEB-DL",
            (false, _) => "Refreshed.movie.2021.1080p.WEB-DL.English",
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
    if uri.path().starts_with("/api/v2/") && state.1.load(Ordering::SeqCst) == 4 {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "PRIVATE_FIXTURE_CLIENT_FAILURE",
        )
            .into_response();
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
        // qBittorrent's support probe uses 403 for login; 401 is a generic probe failure.
        return (StatusCode::FORBIDDEN, "PRIVATE_FIXTURE_AUTH_FAILURE").into_response();
    }
    // Bounded owned health probe delay for pending-generation/cancel browser checks.
    if uri.path() == "/api/v2/app/preferences" && state.1.load(Ordering::SeqCst) == 3 {
        tokio::time::sleep(Duration::from_secs(8)).await;
    }
    if uri.path() == "/api/v2/torrents/info" {
        match state.1.load(Ordering::SeqCst) {
            // The add is recorded below, but its response and later observation remain uncertain.
            5 => return Json(json!([])).into_response(),
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
            let download_name = if searching {
                let Some(name) = search_download_name(&hash) else {
                    return StatusCode::BAD_REQUEST.into_response();
                };
                name
            } else {
                name(tv).to_owned()
            };
            state.3.lock().unwrap().push(json!({"hash":hash,"category":category,"name":download_name,"state":if searching {"downloading"}else{"uploading"},"progress":if searching {0.5}else{1.0},"size":1048576,"amount_left":if searching {524288}else{0},"dlspeed":0,"upspeed":0,"ratio":0.0,"seeding_time":0,"ratio_limit":-2.0,"seeding_time_limit":-2,"inactive_seeding_time_limit":-1,"seq_dl":false,"f_l_piece_prio":false,"auto_tmm":false,"force_start":false,"priority":5,"tags":"","save_path":"/remote","content_path":format!("/remote/{download_name}")}));
            if state.1.load(Ordering::SeqCst) == 5 {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
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
            let downloads = state.3.lock().unwrap();
            let Some(row) = downloads.iter().find(|row| {
                query
                    .get("hash")
                    .is_some_and(|hash| row["hash"] == hash.as_str())
            }) else {
                return StatusCode::NOT_FOUND.into_response();
            };
            return Json(json!([{"index":0,"name":row["name"],"size":1048576,"progress":row["progress"],"priority":1}])).into_response();
        }
        if uri.path() == "/api/v2/torrents/properties" {
            return Json(json!({"save_path":"/remote","total_size":1048576,"addition_date":1,"completion_date":2,"seeding_time":0})).into_response();
        }
    }
    match uri.path() {
        "/api/v2/app/webapiVersion"=>"2.8.3".into_response(),
        "/api/v2/app/version"=>"v4.6.0".into_response(),
        "/api/v2/app/preferences"=>Json(json!({"save_path":"/remote","queueing_enabled":true,"dht":true,"max_ratio_enabled":false,"max_ratio":-1,"max_seeding_time_enabled":false,"max_seeding_time":-1,"max_ratio_act":0})).into_response(),
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

// Genuine source backups seed historical unresolved references through the importer.
async fn release_profile_fixture(
    db: Arc<Database>,
    root: &std::path::Path,
) -> Arc<BlocklistFixture> {
    let mut bytes = Vec::new();
    for tv in [true, false] {
        let path = root.join(if tv {
            "release-sonarr.db"
        } else {
            "release-radarr.db"
        });
        let source = libsql::Builder::new_local(&path).build().await.unwrap();
        let c = source.connect().unwrap();
        c.execute_batch(if tv {r#"CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES(233);
CREATE TABLE Series(Id INTEGER,TvdbId INTEGER,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,Seasons TEXT);
CREATE TABLE Episodes(Id INTEGER,SeriesId INTEGER,SeasonNumber INTEGER,EpisodeNumber INTEGER,Title TEXT,Monitored INTEGER,EpisodeFileId INTEGER);
CREATE TABLE EpisodeFiles(Id INTEGER,SeriesId INTEGER,RelativePath TEXT);
CREATE TABLE ReleaseProfiles(Id INTEGER,Name TEXT,Enabled INTEGER,Required TEXT,Ignored TEXT,IndexerIds TEXT,Tags TEXT,ExcludedTags TEXT,AirDateRestriction INTEGER,AirDateGracePeriod INTEGER,AllowSeasonPackWithoutAllEpisodesAired INTEGER);
INSERT INTO ReleaseProfiles VALUES(77,'Historical unresolved',0,'["WEB"]','[]','[77]','[]','[]',0,0,0);"#} else {r#"CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES(242);
CREATE TABLE MovieMetadata(Id INTEGER,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER);
CREATE TABLE Movies(Id INTEGER,MovieMetadataId INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER);
CREATE TABLE MovieFiles(Id INTEGER,MovieId INTEGER,RelativePath TEXT,Edition TEXT);
CREATE TABLE ReleaseProfiles(Id INTEGER,Name TEXT,Enabled INTEGER,Required TEXT,Ignored TEXT,IndexerId INTEGER,Tags TEXT);
INSERT INTO ReleaseProfiles VALUES(77,'Historical unresolved',0,'["WEB"]','[]',77,'[]');"#}).await.unwrap();
        drop(c);
        drop(source);
        bytes.push(std::fs::read(path).unwrap());
    }
    Arc::new(BlocklistFixture {
        db,
        tv: bytes.remove(0),
        movies: bytes.remove(0),
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
    for digit in ["1", "3", "6", "8"] {
        std::fs::write(
            incoming.join(search_download_name(&digit.repeat(40)).unwrap()),
            vec![7u8; 1048576],
        )
        .unwrap();
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
    let (provider_origin, _providers) = serve(provider_router()).await;
    let (alternate_provider_origin, alternate_providers) = serve(provider_router()).await;
    let key = Arc::new(providers::CredentialKey::from_hex(&"42".repeat(32)).unwrap());
    let (provider_routes, refresh) = providers::router_with_refresh(db.clone(), Some(key));
    let release_fixture = release_profile_fixture(db.clone(), &scratch.0).await;
    let snapshot_fixture = blocklist_fixture(db.clone(), &scratch.0, 901).await;
    let clear_root = scratch.0.join("clear-source");
    std::fs::create_dir(&clear_root).unwrap();
    let clear_fixture = blocklist_fixture(db.clone(), &clear_root, 902).await;
    let worker = Arc::new(CommandWorkerFixture {
        runtime: tokio::sync::Mutex::new(None),
        db: db.clone(),
        refresh: refresh.clone(),
        metadata: client.clone(),
    });
    if std::env::var("HRRDARR_UI_PAUSE_COMMANDS").as_deref() != Ok("1") {
        assert_eq!(
            start_command_worker(State(worker.clone())).await,
            StatusCode::NO_CONTENT
        );
    }
    let (api, _api) = serve(
        library::router(db.clone())
            .merge(
                Router::new()
                    .route(
                        "/api/fixture/start-command-worker",
                        post(start_command_worker),
                    )
                    .with_state(worker.clone()),
            )
            .merge(library::metadata_router(db.clone(), client))
            .merge(episodes::router(db.clone()))
            .merge(hrrdarr::credits::router(db.clone()))
            .merge(import::router(db.clone()))
            .merge(provider_routes)
            .merge(commands::router(db.clone()))
            .merge(hrrdarr::health::router(db.clone()))
            .merge(hrrdarr::completed_download_handling::router(db.clone()))
            .merge(releases::router(db.clone(), refresh))
            .merge(quality_profiles::router(db.clone()))
            .merge(hrrdarr::qualities::router(db.clone()))
            .merge(hrrdarr::tags::router(db.clone()))
            .merge(hrrdarr::release_profiles::router(db.clone()))
            .merge(
                Router::new()
                    .route(
                        "/api/fixture/import-release-profiles",
                        post(import_blocklists),
                    )
                    .with_state(release_fixture),
            )
            .merge(hrrdarr::delay_profiles::router(db.clone()))
            .merge(hrrdarr::revision_policy::router(db.clone()))
            .merge(hrrdarr::import_policy::router(db.clone()))
            .merge(hrrdarr::remote_paths::router(db.clone()))
            .merge(hrrdarr::media_files::router(db.clone()))
            .merge(hrrdarr::naming::router(db.clone()))
            .merge(hrrdarr::custom_formats::router(db.clone()))
            .merge(hrrdarr::root_folders::router(db.clone()))
            .merge(
                Router::new()
                    .route("/api/fixture/fail-import-history", post(import_failure))
                    .route("/api/fixture/client-backoff/age", post(age_client_failure))
                    .route(
                        "/api/fixture/client-backoff/pending",
                        post(pending_client_refreshes),
                    )
                    .route(
                        "/api/fixture/client-backoff/{id}",
                        get(client_backoff_state),
                    )
                    .with_state(db.clone()),
            )
            .merge(
                Router::new()
                    .route("/api/fixture/file-metadata-seed", post(seed_file_metadata))
                    .layer(axum::extract::DefaultBodyLimit::max(1024))
                    .with_state((db.clone(), tv.clone())),
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
        json!({"api":api,"metadata":metadata_origin,"provider_origin":provider_origin,"alternate_provider_origin":alternate_provider_origin,"scratch":scratch.0,"tv_path":tv,"movie_path":movies,"episode_source":incoming.join("episode.mkv"),"movie_source":incoming.join("movie.mkv"),"episode_destination":tv.join("pilot.mkv"),"movie_destination":movies.join("movie.mkv"),"shutdown":shutdown})
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15 * 60);
    while !shutdown.exists() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    // Join aborted servers before removing their database and scratch files.
    _api.stop().await;
    if let Some(runtime) = worker.runtime.lock().await.take() {
        runtime.shutdown().await;
    }
    _metadata.stop().await;
    _providers.stop().await;
    alternate_providers.stop().await;
}
