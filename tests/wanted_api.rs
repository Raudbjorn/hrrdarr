//! Wanted missing/cutoff lists and Wanted search admission over a real owned HTTP server.
//! Library rows are seeded in SQL (no metadata provider); profiles, providers and every
//! search command are created through the production HTTP routes. No command worker runs,
//! so admitted searches stay queued and no provider request is ever made.
use hrrdarr::{db::Database, providers};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn serve(app: axum::Router) -> (String, Server) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    (
        base,
        Server(tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap()
        })),
    )
}
async fn request(base: &str, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap();
    let mut builder = client.request(method.parse().unwrap(), format!("{base}{path}"));
    if let Some(body) = body {
        builder = builder
            .header("content-type", "application/json")
            .body(body.to_string());
    }
    let response = builder.send().await.unwrap();
    let status = response.status().as_u16();
    let text = response.text().await.unwrap();
    (
        status,
        if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or_else(|_| panic!("{status} {text}"))
        },
    )
}
async fn get(base: &str, path: &str) -> Value {
    let (status, body) = request(base, "GET", path, None).await;
    assert_eq!(status, 200, "{path}: {body}");
    body
}
fn ids(page: &Value) -> Vec<i64> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["target"]["id"].as_i64().unwrap())
        .collect()
}
fn targets(page: &Value, kind: &str) -> bool {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| item["target"]["media_type"] == kind)
}
struct App {
    base: String,
    db: Arc<Database>,
    _server: Server,
    _remote: Server,
    remote: String,
    _scratch: Scratch,
}
async fn app() -> App {
    let scratch =
        Scratch(std::env::temp_dir().join(format!("hrrdarr-wanted-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&scratch.0).unwrap();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let key = Arc::new(providers::CredentialKey::from_hex(&"21".repeat(32)).unwrap());
    let (router, client) = providers::router_with_refresh(db.clone(), Some(key));
    let (base, server) = serve(
        router
            .merge(hrrdarr::commands::router(db.clone()))
            .merge(hrrdarr::search::router(db.clone(), client.clone()))
            .merge(hrrdarr::wanted::router(db.clone(), client))
            .merge(hrrdarr::quality_profiles::router(db.clone()))
            .merge(hrrdarr::episodes::router(db.clone())),
    )
    .await;
    // Owned peer that is never expected to be contacted (no worker is started).
    let (remote, remote_server) = serve(axum::Router::new()).await;
    App {
        base,
        db,
        _server: server,
        _remote: remote_server,
        remote,
        _scratch: scratch,
    }
}
async fn profile(base: &str, media: &str, name: &str, upgrade: bool, cutoff: Value) -> i64 {
    let leaf = |id: i64, allowed: bool| json!({"kind":"quality","quality_id":id,"allowed":allowed});
    let (status, body) = request(
        base,
        "POST",
        &format!("/api/v1/{media}/quality-profiles"),
        Some(json!({
            "name": name,
            "items": [
                leaf(1, false),
                leaf(4, true),
                {"kind":"group","name":"WEB 1080p","allowed":true,"items":[{"quality_id":3,"allowed":true},{"quality_id":15,"allowed":true}]},
                leaf(7, true),
            ],
            "policy": {"upgrade_allowed":upgrade,"cutoff":cutoff,"min_format_score":0,"cutoff_format_score":100,"min_upgrade_format_score":1,"language_id":if media=="tv"{Value::Null}else{json!(-1)},"format_items":[]}
        })),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    body["id"].as_i64().unwrap()
}
fn ago(seconds: i64) -> String {
    (chrono::Utc::now() - chrono::Duration::seconds(seconds))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}
/// Episode and movie IDs deliberately collide (1..9); typed targets keep them apart.
async fn seed(app: &App) {
    let grouped = profile(
        &app.base,
        "tv",
        "Grouped",
        true,
        json!({"kind":"group","position":2}),
    )
    .await;
    let first_allowed = profile(
        &app.base,
        "tv",
        "No upgrades",
        false,
        json!({"kind":"quality","quality_id":7}),
    )
    .await;
    let movie_profile = profile(
        &app.base,
        "movies",
        "Grouped",
        true,
        json!({"kind":"group","position":2}),
    )
    .await;
    let c = app.db.connect().await.unwrap();
    c.execute_batch(&format!(
        "INSERT INTO series(id,tvdb_id,title,path,monitored,runtime) VALUES
          (1,101,'Harbor','/owned/harbor',1,30),(2,102,'anchor','/owned/anchor',0,30),
          (3,103,'Delta','/owned/delta',1,NULL),(4,104,'Echo Daily','/owned/echo',1,60);
         INSERT INTO seasons(series_id,number,monitored) VALUES(1,0,1),(1,1,1),(1,2,0),(2,1,1),(3,1,1),(4,1,1);
         INSERT INTO library_settings(media_type,series_id,quality_profile_id,series_type,use_scene_numbering) VALUES
          ('tv',1,{grouped},'standard',0),('tv',2,{grouped},'standard',0),('tv',3,{first_allowed},'anime',0),('tv',4,{first_allowed},'daily',0);
         INSERT INTO episode_files(id,series_id,path) VALUES(1,1,'/owned/harbor/s01e02e03.mkv'),(2,1,'/owned/harbor/s00e02.mkv'),
          (3,1,'/owned/harbor/s01e06.mkv'),(4,1,'/owned/harbor/s01e07.mkv'),(5,1,'/owned/harbor/s02e01.mkv'),
          (6,4,'/owned/echo/e1.mkv'),(7,4,'/owned/echo/e2.mkv'),(8,1,'/owned/harbor/s01e08.mkv'),(9,1,'/owned/harbor/s02e02.mkv');
         INSERT INTO file_metadata(media_type,episode_file_id,quality_id) VALUES('tv',1,4),('tv',2,1),('tv',3,15),('tv',4,7),('tv',5,1),('tv',6,4),('tv',7,1),('tv',8,9);
         INSERT INTO episodes(id,series_id,season,number,title,monitored,air_date_utc,episode_file_id,absolute_episode_number) VALUES
          (1,1,1,1,'Pilot',1,'2020-01-01T00:00:00Z',NULL,1),
          (2,1,1,2,'Shared A',1,'2020-01-08T00:00:00Z',1,2),
          (3,1,1,3,'Shared B',1,'2020-01-08T00:00:00Z',1,3),
          (4,1,1,4,'Future',1,'2099-01-01T00:00:00Z',NULL,4),
          (5,1,0,1,'Special aired',1,'2019-06-01T00:00:00Z',NULL,NULL),
          (6,1,0,2,'Special low file',1,'2019-06-02T00:00:00Z',2,NULL),
          (7,1,1,5,'Unmonitored episode',0,'2020-02-01T00:00:00Z',NULL,5),
          (8,2,1,1,'Unmonitored series',1,'2020-01-01T00:00:00Z',NULL,1),
          (9,3,1,1,'Unknown runtime',1,'2020-01-01T00:00:00Z',NULL,1),
          (10,3,1,2,'Unknown air',1,NULL,NULL,2),
          (11,1,1,6,'Grouped equal',1,'2020-01-09T00:00:00Z',3,6),
          (12,1,1,7,'Above cutoff',1,'2020-01-10T00:00:00Z',4,7),
          (13,1,2,1,'Disallowed lower',1,NULL,5,8),
          (14,4,1,1,'Daily first allowed',1,'2021-01-01T00:00:00Z',6,NULL),
          (15,4,1,2,'Daily below',1,'2021-01-02T00:00:00Z',7,NULL),
          (16,1,1,8,'Unknown profile quality',1,'2020-01-11T00:00:00Z',8,9),
          (17,1,1,9,'Airing now',1,'{airing}',NULL,10),
          (18,1,1,10,'Just ended',1,'{ended}',NULL,11),
          (19,1,2,2,'No file metadata',1,'2020-03-02T00:00:00Z',9,12);
         INSERT INTO movie_metadata(id,tmdb_id,title,year,in_cinemas,digital_release) VALUES
          (1,201,'Alpha',2020,'2020-01-01 00:00:00','2020-06-01 00:00:00'),(2,202,'Bravo',2099,NULL,'2099-06-01 00:00:00'),
          (3,203,'Charlie',NULL,NULL,NULL),(4,204,'delta',2019,NULL,'2019-06-01 00:00:00'),(5,205,'Echo',2018,NULL,NULL),
          (6,206,'Foxtrot',2018,NULL,NULL),(7,207,'Golf',2018,NULL,NULL),(8,208,'Hotel',2018,NULL,NULL),(9,209,'India',2017,NULL,NULL),
          (10,210,'Orphan catalog',2015,NULL,NULL);
         INSERT INTO movies(id,metadata_id,path,monitored) VALUES(1,1,'/owned/m1',1),(2,2,'/owned/m2',1),(3,3,'/owned/m3',1),(4,4,'/owned/m4',0),
          (5,5,'/owned/m5',1),(6,6,'/owned/m6',1),(7,7,'/owned/m7',1),(8,8,'/owned/m8',1),(9,9,'/owned/m9',1);
         INSERT INTO library_settings(media_type,movie_id,quality_profile_id,minimum_availability)
          SELECT 'movies',id,{movie_profile},'released' FROM movies;
         INSERT INTO movie_files(id,movie_id,path) VALUES(5,5,'/owned/m5/f.mkv'),(6,6,'/owned/m6/f.mkv'),(7,7,'/owned/m7/f.mkv'),(8,8,'/owned/m8/f.mkv'),(9,9,'/owned/m9/f.mkv');
         INSERT INTO file_metadata(media_type,movie_file_id,quality_id) VALUES('movies',5,4),('movies',6,15),('movies',7,7),('movies',8,1),('movies',9,9);
         INSERT INTO release_delay_policies VALUES('tv',0,0,0),('movies',0,0,0);",
        // Air end = air time + 30 runtime minutes: one minute in the future vs one minute past.
        airing = ago(29 * 60),
        ended = ago(31 * 60),
    ))
    .await
    .unwrap();
}
async fn data_version(c: &libsql::Connection) -> i64 {
    c.query("PRAGMA data_version", ())
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
async fn missing_and_cutoff_lists_classify_both_domains_without_writes() {
    let app = app().await;
    seed(&app).await;
    let base = &app.base;
    let observer = app.db.connect().await.unwrap();

    // TV missing: episode AND series monitored, aired end (air + series runtime) <= now,
    // specials included, future/in-progress/unknown excluded and counted.
    let missing = get(base, "/api/v1/tv/wanted/missing").await;
    assert_eq!(ids(&missing), [5, 1, 18], "{missing}");
    assert!(targets(&missing, "episode"));
    assert_eq!(
        (missing["total"].as_i64(), missing["limit"].as_i64()),
        (Some(3), Some(50))
    );
    assert_eq!(missing["sort"], "air_date");
    assert_eq!(missing["direction"], "asc");
    assert_eq!(missing["diagnostics"]["unknown_air_date"], 1, "{missing}");
    assert_eq!(missing["diagnostics"]["unknown_series_runtime"], 1);
    assert!(missing["diagnostics"]["unknown_year"].is_null());
    let pilot = &missing["items"][1];
    assert_eq!(pilot["series_title"], "Harbor");
    assert_eq!(pilot["effective_monitored"], true);
    assert!(pilot["file"].is_null() && pilot["cutoff"].is_null());
    assert_eq!(
        pilot["aired_end_at"], 1577838600,
        "2020-01-01T00:00Z + 30 minutes"
    );
    let unmonitored = get(base, "/api/v1/tv/wanted/missing?monitored=unmonitored").await;
    assert_eq!(
        ids(&unmonitored),
        [8, 7],
        "episode-or-series unmonitored partition"
    );
    assert_eq!(unmonitored["items"][0]["series_monitored"], false);
    assert_eq!(unmonitored["items"][0]["effective_monitored"], false);
    assert_eq!(
        ids(&get(base, "/api/v1/tv/wanted/missing?monitored=all").await),
        [5, 1, 8, 7, 18]
    );

    // TV cutoff: grouped qualities equal, disallowed lower below, upgrade-disabled profiles use
    // the first allowed item, specials excluded, a shared file yields one row per episode.
    let cutoff = get(base, "/api/v1/tv/wanted/cutoff").await;
    assert_eq!(
        ids(&cutoff),
        [2, 3, 15, 13],
        "null air date sorts last: {cutoff}"
    );
    assert_eq!(cutoff["diagnostics"]["unknown_cutoff"], 2, "{cutoff}");
    assert_eq!(cutoff["items"][0]["file"]["id"], 1);
    assert_eq!(cutoff["items"][1]["file"]["id"], 1);
    assert_eq!(cutoff["items"][0]["file"]["quality_id"], 4);
    assert_eq!(cutoff["items"][0]["cutoff"]["quality_rank"], 1);
    assert_eq!(cutoff["items"][0]["cutoff"]["threshold_rank"], 2);
    assert_eq!(cutoff["items"][0]["cutoff"]["basis"], "configured_cutoff");
    assert_eq!(cutoff["items"][2]["cutoff"]["basis"], "first_allowed");
    assert_eq!(cutoff["items"][2]["series_type"], "daily");
    assert_eq!(
        ids(&get(base, "/api/v1/tv/wanted/cutoff?direction=desc").await),
        [15, 2, 3, 13],
        "descending keeps nulls last and stable ascending ties"
    );
    assert_eq!(
        ids(&get(base, "/api/v1/tv/wanted/cutoff?sort=quality").await),
        [13, 15, 2, 3]
    );
    assert_eq!(
        ids(&get(
            base,
            "/api/v1/tv/wanted/cutoff?sort=series_title&direction=desc"
        )
        .await),
        [2, 3, 13, 15],
        "Harbor > Echo Daily case-insensitively; season/number ties"
    );
    assert_eq!(
        get(base, "/api/v1/tv/wanted/cutoff?monitored=unmonitored").await["total"],
        0
    );

    // Pagination is over every matching row; an offset past the end keeps the total.
    let first = get(base, "/api/v1/tv/wanted/missing?limit=2").await;
    assert_eq!(
        (ids(&first), first["total"].as_i64()),
        (vec![5, 1], Some(3))
    );
    let second = get(base, "/api/v1/tv/wanted/missing?limit=2&offset=2").await;
    assert_eq!(
        (ids(&second), second["offset"].as_i64()),
        (vec![18], Some(2))
    );
    let past = get(base, "/api/v1/tv/wanted/missing?limit=2&offset=10").await;
    assert_eq!((ids(&past), past["total"].as_i64()), (vec![], Some(3)));

    // Movies missing: library membership, no file, positive year; unreleased movies stay
    // listed by default and carry the background availability fact.
    let movies = get(base, "/api/v1/movies/wanted/missing").await;
    assert_eq!(ids(&movies), [1, 2], "{movies}");
    assert!(targets(&movies, "movie"));
    assert_eq!(movies["sort"], "title");
    assert_eq!(movies["items"][0]["availability"], "available");
    assert_eq!(movies["items"][1]["availability"], "unavailable");
    assert_eq!(movies["items"][1]["minimum_availability"], "released");
    assert_eq!(movies["diagnostics"]["unknown_year"], 1);
    assert!(movies["diagnostics"]["unknown_air_date"].is_null());
    assert_eq!(
        ids(&get(base, "/api/v1/movies/wanted/missing?availability=available").await),
        [1]
    );
    let unavailable = get(
        base,
        "/api/v1/movies/wanted/missing?availability=unavailable",
    )
    .await;
    assert_eq!(
        (ids(&unavailable), unavailable["total"].as_i64()),
        (vec![2], Some(1))
    );
    assert_eq!(
        ids(&get(base, "/api/v1/movies/wanted/missing?monitored=unmonitored").await),
        [4]
    );
    assert_eq!(
        ids(&get(
            base,
            "/api/v1/movies/wanted/missing?monitored=all&sort=year&direction=desc"
        )
        .await),
        [2, 1, 4]
    );
    assert_eq!(
        ids(&get(
            base,
            "/api/v1/movies/wanted/missing?monitored=all&sort=digital_release"
        )
        .await),
        [4, 1, 2]
    );
    assert_eq!(
        ids(&get(
            base,
            "/api/v1/movies/wanted/missing?monitored=all&sort=in_cinemas&direction=desc"
        )
        .await),
        [1, 2, 4],
        "unknown cinema dates stay last, ordered by movie id"
    );

    // Movies cutoff: same quality-only threshold; custom-format score never joins membership.
    let movie_cutoff = get(base, "/api/v1/movies/wanted/cutoff?sort=year").await;
    assert_eq!(ids(&movie_cutoff), [5, 8], "year ties order by movie id");
    assert_eq!(movie_cutoff["diagnostics"]["unknown_cutoff"], 1);
    assert_eq!(movie_cutoff["items"][1]["file"]["quality_id"], 1);
    assert_eq!(movie_cutoff["items"][0]["cutoff"]["threshold_rank"], 2);

    // Ungated: with no persisted movie availability policy, background applies no veto.
    observer
        .execute(
            "DELETE FROM release_delay_policies WHERE media_type='movies'",
            (),
        )
        .await
        .unwrap();
    let ungated = get(base, "/api/v1/movies/wanted/missing").await;
    assert_eq!(ungated["items"][1]["availability"], "ungated");
    assert_eq!(
        ids(&get(base, "/api/v1/movies/wanted/missing?availability=available").await),
        [1, 2]
    );
    observer
        .execute(
            "INSERT INTO release_delay_policies VALUES('movies',0,0,0)",
            (),
        )
        .await
        .unwrap();
    let before = data_version(&observer).await;
    for path in [
        "/api/v1/tv/wanted/missing",
        "/api/v1/tv/wanted/cutoff",
        "/api/v1/movies/wanted/missing?availability=available",
        "/api/v1/movies/wanted/cutoff",
    ] {
        get(base, path).await;
    }
    assert_eq!(
        data_version(&observer).await,
        before,
        "GET never commits a write from any connection"
    );

    // An actual monitoring write moves the episode between partitions.
    let (status, body) = request(
        base,
        "PUT",
        "/api/v1/episodes/1",
        Some(json!({"monitored":false})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(ids(&get(base, "/api/v1/tv/wanted/missing").await), [5, 18]);
    assert_eq!(
        ids(&get(base, "/api/v1/tv/wanted/missing?monitored=unmonitored").await),
        [1, 8, 7]
    );
}

#[tokio::test]
async fn invalid_queries_are_rejected_with_static_errors() {
    let app = app().await;
    for path in [
        "/api/v1/tv/wanted/missing?limit=1&limit=2",
        "/api/v1/tv/wanted/missing?limit=0",
        "/api/v1/tv/wanted/missing?limit=101",
        "/api/v1/tv/wanted/missing?offset=-1",
        "/api/v1/tv/wanted/missing?offset=2147483648",
        "/api/v1/tv/wanted/missing?page=1",
        "/api/v1/tv/wanted/missing?monitored=",
        "/api/v1/tv/wanted/missing?monitored=true",
        "/api/v1/tv/wanted/missing?sort=title",
        "/api/v1/tv/wanted/missing?sort=quality",
        "/api/v1/tv/wanted/missing?availability=available",
        "/api/v1/tv/wanted/cutoff?direction=up",
        "/api/v1/movies/wanted/missing?sort=air_date",
        "/api/v1/movies/wanted/cutoff?availability=available",
        "/api/v1/movies/wanted/missing?include_specials=true",
    ] {
        let (status, body) = request(&app.base, "GET", path, None).await;
        assert_eq!(status, 400, "{path}: {body}");
        assert_eq!(body["error"]["code"], "invalid_wanted_query", "{path}");
        assert!(
            body["error"]["message"]
                .as_str()
                .is_some_and(|m| !m.contains("SELECT"))
        );
    }
    let long = format!(
        "/api/v1/movies/wanted/missing?monitored=all&x={}",
        "a".repeat(4200)
    );
    let (status, body) = request(&app.base, "GET", &long, None).await;
    assert_eq!(
        (status, &body["error"]["code"]),
        (400, &json!("invalid_wanted_query"))
    );
    let (status, _) = request(&app.base, "GET", "/api/v1/books/wanted/missing", None).await;
    assert_eq!(status, 404);
}

async fn providers(app: &App) -> (Value, Value) {
    let mut values = Vec::new();
    for settings in [
        json!({"implementation":"torznab","endpoint":app.remote,"tv":{"categories":[5030],"anime_categories":[]},"movies":{"categories":[2030]}}),
        json!({"implementation":"qbittorrent","endpoint":app.remote,"tv":{"category":"tv","imported_category":null,"recent_priority":0,"older_priority":0},"movies":{"category":"movies","imported_category":null,"recent_priority":0,"older_priority":0}}),
    ] {
        let (code, value) = request(
            &app.base,
            "POST",
            "/api/v1/providers",
            Some(json!({"name":"owned","enabled":true,"priority":1,"settings":settings})),
        )
        .await;
        assert_eq!(code, 201, "{value}");
        values.push(value)
    }
    (values.remove(0), values.remove(0))
}
fn search_body(
    request_id: &str,
    kind: &str,
    selection: Value,
    indexer: &Value,
    client: &Value,
) -> Value {
    json!({"request_id":request_id,"kind":kind,"selection":selection,"indexer_id":indexer["id"],"indexer_revision":indexer["revision"],"client_selection":{"kind":"explicit","provider_id":client["id"],"provider_revision":client["revision"]},"priority":"normal"})
}
async fn search_rows(c: &libsql::Connection) -> i64 {
    c.query("SELECT count(*) FROM search_commands", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}
fn command_targets(receipt: &Value) -> Vec<(String, i64)> {
    receipt["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["target"]["media_type"].as_str().unwrap().to_owned(),
                c["target"]["id"].as_i64().unwrap(),
            )
        })
        .collect()
}

#[tokio::test]
async fn wanted_searches_admit_typed_existing_search_commands_atomically() {
    let app = app().await;
    seed(&app).await;
    let base = &app.base;
    let (indexer, client) = providers(&app).await;
    let c = app.db.connect().await.unwrap();

    // Selected episodes: duplicates removed; explicit intent ignores list membership.
    let selected = uuid::Uuid::new_v4().to_string();
    let body = search_body(
        &selected,
        "cutoff",
        json!({"kind":"ids","ids":[2,1,2]}),
        &indexer,
        &client,
    );
    let (status, receipt) =
        request(base, "POST", "/api/v1/tv/wanted/search", Some(body.clone())).await;
    assert_eq!(status, 202, "{receipt}");
    assert_eq!(
        command_targets(&receipt),
        [("episode".into(), 1), ("episode".into(), 2)]
    );
    assert_eq!(
        (
            receipt["matched"].as_i64(),
            receipt["skipped_active"].as_i64()
        ),
        (Some(2), Some(0))
    );
    assert_eq!(receipt["replayed"], false);
    for command in receipt["commands"].as_array().unwrap() {
        let (status, row) = request(
            base,
            "GET",
            &format!(
                "/api/v1/search/commands/{}",
                command["command_id"].as_str().unwrap()
            ),
            None,
        )
        .await;
        assert_eq!(status, 200, "{row}");
        assert_eq!(row["target"], command["target"]);
        assert_eq!(
            (row["mode"].as_str(), row["status"].as_str()),
            (Some("automatic"), Some("queued"))
        );
        assert_eq!(row["source"]["indexer_id"], indexer["id"]);
        assert_eq!(row["source"]["selection_kind"], "explicit");
        assert_eq!(row["source"]["client_id"], client["id"]);
    }
    // Exact replay returns the original capture; a different body under that UUID conflicts.
    let (status, replay) = request(base, "POST", "/api/v1/tv/wanted/search", Some(body)).await;
    assert_eq!(status, 202);
    assert_eq!(replay["replayed"], true);
    // Reasoning: the durable receipt (migration 0091) stores the original result counters, so a
    // replay now reports them instead of null; null remains only for pre-receipt batches.
    assert_eq!(
        (
            replay["matched"].as_i64(),
            replay["skipped_active"].as_i64(),
            replay["admitted"].as_i64()
        ),
        (Some(2), Some(0), Some(2))
    );
    assert_eq!(replay["commands"], receipt["commands"]);
    let changed = search_body(
        &selected,
        "cutoff",
        json!({"kind":"ids","ids":[1]}),
        &indexer,
        &client,
    );
    let (status, conflict) = request(base, "POST", "/api/v1/tv/wanted/search", Some(changed)).await;
    assert_eq!(
        (status, &conflict["error"]["code"]),
        (409, &json!("command_conflict"))
    );
    assert_eq!(search_rows(&c).await, 2);

    // Search all captures every current member and skips targets already being searched.
    let all = search_body(
        &uuid::Uuid::new_v4().to_string(),
        "missing",
        json!({"kind":"all","monitored":"monitored"}),
        &indexer,
        &client,
    );
    let (status, receipt) = request(base, "POST", "/api/v1/tv/wanted/search", Some(all)).await;
    assert_eq!(status, 202, "{receipt}");
    assert_eq!(
        (
            receipt["matched"].as_i64(),
            receipt["skipped_active"].as_i64()
        ),
        (Some(3), Some(1))
    );
    assert_eq!(
        command_targets(&receipt),
        [("episode".into(), 5), ("episode".into(), 18)]
    );

    // Movie 1 is distinct from episode 1 even though both are active with equal numbers.
    let movies = search_body(
        &uuid::Uuid::new_v4().to_string(),
        "missing",
        json!({"kind":"all","monitored":"monitored","availability":"available"}),
        &indexer,
        &client,
    );
    let (status, receipt) =
        request(base, "POST", "/api/v1/movies/wanted/search", Some(movies)).await;
    assert_eq!(status, 202, "{receipt}");
    assert_eq!(command_targets(&receipt), [("movie".into(), 1)]);
    let cutoff = search_body(
        &uuid::Uuid::new_v4().to_string(),
        "cutoff",
        json!({"kind":"all","monitored":"monitored"}),
        &indexer,
        &client,
    );
    let (status, receipt) =
        request(base, "POST", "/api/v1/movies/wanted/search", Some(cutoff)).await;
    assert_eq!(status, 202, "{receipt}");
    assert_eq!(
        command_targets(&receipt),
        [("movie".into(), 5), ("movie".into(), 8)]
    );
    let (_, page) = request(
        base,
        "GET",
        "/api/v1/search/commands?target_type=movie&target_id=1",
        None,
    )
    .await;
    assert_eq!(page["total"], 1);
    let (_, page) = request(
        base,
        "GET",
        "/api/v1/search/commands?target_type=episode&target_id=1",
        None,
    )
    .await;
    assert_eq!(page["total"], 1);

    // Invalid or wrong-domain requests admit nothing.
    let rows = search_rows(&c).await;
    for (path, body, code, expected) in [
        (
            "/api/v1/tv/wanted/search",
            search_body(
                &uuid::Uuid::new_v4().to_string(),
                "missing",
                json!({"kind":"ids","ids":[1,999]}),
                &indexer,
                &client,
            ),
            409,
            "target_changed",
        ),
        (
            "/api/v1/tv/wanted/search",
            search_body(
                &uuid::Uuid::new_v4().to_string(),
                "missing",
                json!({"kind":"ids","ids":[]}),
                &indexer,
                &client,
            ),
            400,
            "invalid_command_request",
        ),
        (
            "/api/v1/tv/wanted/search",
            search_body(
                &uuid::Uuid::new_v4().to_string(),
                "missing",
                json!({"kind":"all","monitored":"monitored","availability":"available"}),
                &indexer,
                &client,
            ),
            400,
            "invalid_command_request",
        ),
        (
            "/api/v1/movies/wanted/search",
            search_body(
                &uuid::Uuid::new_v4().to_string(),
                "cutoff",
                json!({"kind":"all","monitored":"monitored","availability":"available"}),
                &indexer,
                &client,
            ),
            400,
            "invalid_command_request",
        ),
        (
            "/api/v1/movies/wanted/search",
            search_body(
                &uuid::Uuid::new_v4().to_string(),
                "missing",
                json!({"kind":"ids","ids":(1..=1025).collect::<Vec<i64>>()}),
                &indexer,
                &client,
            ),
            413,
            "wanted_search_target_limit",
        ),
        (
            "/api/v1/movies/wanted/search",
            json!({"request_id":uuid::Uuid::new_v4(),"kind":"missing","selection":{"kind":"ids","ids":[1]},"indexer_id":indexer["id"],"indexer_revision":indexer["revision"],"client_selection":{"kind":"default"},"priority":"normal","mode":"interactive"}),
            400,
            "invalid_command_request",
        ),
        (
            "/api/v1/movies/wanted/search",
            json!({"request_id":uuid::Uuid::new_v4(),"kind":"missing","selection":{"kind":"ids","ids":[1]},"indexer_id":indexer["id"],"indexer_revision":indexer["revision"].as_i64().unwrap() + 1,"client_selection":{"kind":"default"},"priority":"normal"}),
            409,
            "provider_changed",
        ),
    ] {
        let (status, error) = request(base, "POST", path, Some(body)).await;
        assert_eq!(
            (status, error["error"]["code"].as_str()),
            (code, Some(expected)),
            "{error}"
        );
    }
    let (status, _) = request(
        base,
        "POST",
        "/api/v1/tv/wanted/search?dry_run=1",
        Some(search_body(
            &uuid::Uuid::new_v4().to_string(),
            "missing",
            json!({"kind":"ids","ids":[1]}),
            &indexer,
            &client,
        )),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(
        search_rows(&c).await,
        rows,
        "rejected requests left no commands"
    );
}

#[tokio::test]
async fn search_all_refuses_whole_requests_over_target_or_pool_capacity() {
    let app = app().await;
    let base = &app.base;
    let grouped = profile(
        base,
        "tv",
        "Grouped",
        true,
        json!({"kind":"group","position":2}),
    )
    .await;
    let (indexer, client) = providers(&app).await;
    let c = app.db.connect().await.unwrap();
    // 1025 aired, monitored, missing episodes plus two extra targets outside the filter.
    let mut sql = format!(
        "INSERT INTO series(id,title,path,monitored,runtime) VALUES(1,'Bulk','/owned/bulk',1,30);
         INSERT INTO seasons(series_id,number) VALUES(1,1);
         INSERT INTO library_settings(media_type,series_id,quality_profile_id,series_type,use_scene_numbering) VALUES('tv',1,{grouped},'standard',0);"
    );
    sql.push_str(
        "INSERT INTO episodes(id,series_id,season,number,title,monitored,air_date_utc) VALUES",
    );
    let rows: Vec<String> = (1..=1027)
        .map(|n| {
            format!(
                "({n},1,1,{n},'Episode {n}',{},'2020-01-01T00:00:00Z')",
                i64::from(n <= 1025)
            )
        })
        .collect();
    sql.push_str(&rows.join(","));
    sql.push(';');
    c.execute_batch(&sql).await.unwrap();
    let all = |id: &str| {
        search_body(
            id,
            "missing",
            json!({"kind":"all","monitored":"monitored"}),
            &indexer,
            &client,
        )
    };
    let (status, error) = request(
        base,
        "POST",
        "/api/v1/tv/wanted/search",
        Some(all(&uuid::Uuid::new_v4().to_string())),
    )
    .await;
    assert_eq!(
        (status, error["error"]["code"].as_str()),
        (413, Some("wanted_search_target_limit"))
    );
    assert_eq!(
        search_rows(&c).await,
        0,
        "over-bound capture admits nothing"
    );

    c.execute(
        "UPDATE episodes SET monitored=0 WHERE id IN (1024,1025)",
        (),
    )
    .await
    .unwrap();
    let (status, receipt) = request(
        base,
        "POST",
        "/api/v1/tv/wanted/search",
        Some(all(&uuid::Uuid::new_v4().to_string())),
    )
    .await;
    assert_eq!(status, 202, "{receipt}");
    assert_eq!(receipt["admitted"], 1023);
    assert_eq!(search_rows(&c).await, 1023);

    // One slot remains in the shared active pool: two targets are refused as a whole.
    let ids = |list: Vec<i64>| {
        search_body(
            &uuid::Uuid::new_v4().to_string(),
            "missing",
            json!({"kind":"ids","ids":list}),
            &indexer,
            &client,
        )
    };
    let (status, error) = request(
        base,
        "POST",
        "/api/v1/tv/wanted/search",
        Some(ids(vec![1026, 1027])),
    )
    .await;
    assert_eq!(
        (status, error["error"]["code"].as_str()),
        (429, Some("command_history_full"))
    );
    assert_eq!(search_rows(&c).await, 1023);
    let (status, receipt) = request(
        base,
        "POST",
        "/api/v1/tv/wanted/search",
        Some(ids(vec![1026])),
    )
    .await;
    assert_eq!(status, 202, "{receipt}");
    assert_eq!(search_rows(&c).await, 1024);
    // Every monitored member is already active: an empty, explained receipt.
    let (status, receipt) = request(
        base,
        "POST",
        "/api/v1/tv/wanted/search",
        Some(all(&uuid::Uuid::new_v4().to_string())),
    )
    .await;
    assert_eq!(status, 202, "{receipt}");
    assert_eq!(
        (
            receipt["matched"].as_i64(),
            receipt["skipped_active"].as_i64(),
            receipt["admitted"].as_i64()
        ),
        (Some(1023), Some(1023), Some(0))
    );
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}
async fn receipts(c: &libsql::Connection) -> i64 {
    scalar(c, "SELECT count(*) FROM wanted_search_receipts").await
}
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
const TV: &str = "/api/v1/tv/wanted/search";
const MOVIES: &str = "/api/v1/movies/wanted/search";
fn all_missing(id: &str, indexer: &Value, client: &Value) -> Value {
    search_body(
        id,
        "missing",
        json!({"kind":"all","monitored":"monitored"}),
        indexer,
        client,
    )
}
fn counts(receipt: &Value) -> (Option<i64>, Option<i64>, Option<i64>) {
    (
        receipt["matched"].as_i64(),
        receipt["skipped_active"].as_i64(),
        receipt["admitted"].as_i64(),
    )
}

/// A search-all that matches nothing is an authoritative empty result: a retry replays it even
/// after targets become eligible, and a changed body or another domain conflicts.
#[tokio::test]
async fn zero_match_admissions_persist_receipts_and_reject_changed_retries() {
    let app = app().await;
    let base = &app.base;
    let (indexer, client) = providers(&app).await;
    let c = app.db.connect().await.unwrap();
    let (tv_id, movie_id) = (
        uuid::Uuid::new_v4().to_string(),
        uuid::Uuid::new_v4().to_string(),
    );
    let tv = all_missing(&tv_id, &indexer, &client);
    let movies = all_missing(&movie_id, &indexer, &client);
    for (path, body) in [(TV, &tv), (MOVIES, &movies)] {
        let (status, receipt) = request(base, "POST", path, Some(body.clone())).await;
        assert_eq!(status, 202, "{receipt}");
        assert_eq!(receipt["replayed"], false);
        assert_eq!(counts(&receipt), (Some(0), Some(0), Some(0)));
        assert_eq!(receipt["commands"], json!([]));
    }
    assert_eq!((search_rows(&c).await, receipts(&c).await), (0, 2));

    // Targets become eligible in both domains; the retries must not capture them.
    seed(&app).await;
    for (path, body) in [(TV, &tv), (MOVIES, &movies)] {
        let (status, replay) = request(base, "POST", path, Some(body.clone())).await;
        assert_eq!(status, 202, "{replay}");
        assert_eq!(replay["replayed"], true);
        assert_eq!(counts(&replay), (Some(0), Some(0), Some(0)));
        assert_eq!(replay["commands"], json!([]));
    }
    assert_eq!(search_rows(&c).await, 0, "retries created no commands");

    // Changed kind, filter, priority, provider revision or client selection under the same UUID.
    let mut priority = tv.clone();
    priority["priority"] = json!("high");
    let mut kind = tv.clone();
    kind["kind"] = json!("cutoff");
    let mut filter = tv.clone();
    filter["selection"] = json!({"kind":"all","monitored":"all"});
    let mut revision = tv.clone();
    revision["indexer_revision"] = json!(indexer["revision"].as_i64().unwrap() + 1);
    let mut selection = tv.clone();
    selection["client_selection"] = json!({"kind":"default"});
    for changed in [priority, kind, filter, revision, selection] {
        let (status, error) = request(base, "POST", TV, Some(changed)).await;
        assert_eq!(
            (status, error["error"]["code"].as_str()),
            (409, Some("command_conflict")),
            "{error}"
        );
    }
    // A UUID reused across media domains conflicts: the digest includes `media_type`.
    let mut reused = tv.clone();
    reused["request_id"] = json!(movie_id);
    let (status, error) = request(base, "POST", TV, Some(reused)).await;
    assert_eq!(
        (status, error["error"]["code"].as_str()),
        (409, Some("command_conflict"))
    );
    let mut reused = movies.clone();
    reused["request_id"] = json!(tv_id);
    let (status, error) = request(base, "POST", MOVIES, Some(reused)).await;
    assert_eq!(
        (status, error["error"]["code"].as_str()),
        (409, Some("command_conflict"))
    );
    assert_eq!((search_rows(&c).await, receipts(&c).await), (0, 2));

    // Deleting the provider after admission does not turn the retry into a fresh capture.
    let (status, _) = request(
        base,
        "DELETE",
        &format!(
            "/api/v1/providers/{}?revision={}",
            indexer["id"].as_str().unwrap(),
            indexer["revision"]
        ),
        None,
    )
    .await;
    assert!(matches!(status, 200 | 204), "{status}");
    let (status, replay) = request(base, "POST", TV, Some(tv.clone())).await;
    assert_eq!(status, 202, "{replay}");
    assert_eq!(replay["replayed"], true);
    assert_eq!(search_rows(&c).await, 0);
}

/// Every matched target already being searched is also an authoritative result, bound to its
/// own UUID: cancelling the first request's children later does not make the retry capture.
#[tokio::test]
async fn all_active_admissions_persist_receipts() {
    let app = app().await;
    seed(&app).await;
    let base = &app.base;
    let (indexer, client) = providers(&app).await;
    let c = app.db.connect().await.unwrap();
    let (a, b) = (
        uuid::Uuid::new_v4().to_string(),
        uuid::Uuid::new_v4().to_string(),
    );
    let (status, first) = request(base, "POST", TV, Some(all_missing(&a, &indexer, &client))).await;
    assert_eq!(status, 202, "{first}");
    assert_eq!(counts(&first), (Some(3), Some(0), Some(3)));
    let body_b = all_missing(&b, &indexer, &client);
    let (status, second) = request(base, "POST", TV, Some(body_b.clone())).await;
    assert_eq!(status, 202, "{second}");
    assert_eq!(counts(&second), (Some(3), Some(3), Some(0)));
    assert_eq!(second["commands"], json!([]));
    assert_eq!((search_rows(&c).await, receipts(&c).await), (3, 2));

    // Cancel A's children: they leave the active-target query, so a re-capture would admit.
    for command in first["commands"].as_array().unwrap() {
        let (status, cancelled) = request(
            base,
            "POST",
            &format!(
                "/api/v1/search/commands/{}/cancel",
                command["command_id"].as_str().unwrap()
            ),
            Some(json!({})),
        )
        .await;
        assert_eq!(status, 200, "{cancelled}");
    }
    let (status, replay) = request(base, "POST", TV, Some(body_b.clone())).await;
    assert_eq!(status, 202, "{replay}");
    assert_eq!(replay["replayed"], true);
    assert_eq!(counts(&replay), (Some(3), Some(3), Some(0)));
    assert_eq!(replay["commands"], json!([]));
    assert_eq!(search_rows(&c).await, 3, "no new children after the retry");
    let mut changed = body_b;
    changed["priority"] = json!("high");
    let (status, error) = request(base, "POST", TV, Some(changed)).await;
    assert_eq!(
        (status, error["error"]["code"].as_str()),
        (409, Some("command_conflict"))
    );
    assert_eq!(search_rows(&c).await, 3);
}

/// A non-empty receipt no longer depends on its child commands: terminal-history deletion and
/// housekeeping leave replay intact until the receipt itself expires.
#[tokio::test]
async fn receipts_outlive_child_history_and_expire_explicitly() {
    let app = app().await;
    seed(&app).await;
    let base = &app.base;
    let (indexer, client) = providers(&app).await;
    let c = app.db.connect().await.unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let body = search_body(
        &id,
        "missing",
        json!({"kind":"ids","ids":[1,5]}),
        &indexer,
        &client,
    );
    let (status, original) = request(base, "POST", TV, Some(body.clone())).await;
    assert_eq!(status, 202, "{original}");
    for command in original["commands"].as_array().unwrap() {
        request(
            base,
            "POST",
            &format!(
                "/api/v1/search/commands/{}/cancel",
                command["command_id"].as_str().unwrap()
            ),
            Some(json!({})),
        )
        .await;
    }
    // Child history is pruned (the commands are terminal), receipt retained.
    c.execute("DELETE FROM search_commands WHERE status='cancelled'", ())
        .await
        .unwrap();
    assert_eq!((search_rows(&c).await, receipts(&c).await), (0, 1));
    let (status, replay) = request(base, "POST", TV, Some(body.clone())).await;
    assert_eq!(status, 202, "{replay}");
    assert_eq!(replay["replayed"], true);
    assert_eq!(
        replay["commands"], original["commands"],
        "same identities as the original"
    );
    assert_eq!(
        search_rows(&c).await,
        0,
        "a pruned child never causes new external work"
    );

    // Housekeeping keeps an unexpired receipt even far beyond command-history retention...
    let policy = hrrdarr::commands::housekeeping::Policy {
        retention_seconds: 1,
        min_keep: 0,
        ..Default::default()
    };
    let before_expiry = now() + 6 * 24 * 60 * 60;
    let report = hrrdarr::commands::housekeeping::prune(&app.db, &policy, before_expiry)
        .await
        .unwrap();
    assert_eq!(report.deleted_from("wanted_search_receipts", "tv"), 0);
    assert_eq!(receipts(&c).await, 1);
    // ...and removes it, with its targets, only after the documented 7 days.
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM wanted_search_receipt_targets").await,
        2
    );
    let after_expiry = now() + 7 * 24 * 60 * 60 + 5;
    let report = hrrdarr::commands::housekeeping::prune(&app.db, &policy, after_expiry)
        .await
        .unwrap();
    assert_eq!(report.deleted_from("wanted_search_receipts", "tv"), 1);
    assert_eq!(report.deleted_from("wanted_search_receipts", "movies"), 0);
    assert_eq!(
        (
            receipts(&c).await,
            scalar(&c, "SELECT count(*) FROM wanted_search_receipt_targets").await
        ),
        (0, 0)
    );
    // An expired identity is explicit: the UUID is a new request, not a replay of the old one.
    let (status, fresh) = request(base, "POST", TV, Some(body)).await;
    assert_eq!(status, 202, "{fresh}");
    assert_eq!(fresh["replayed"], false);
    assert_eq!(search_rows(&c).await, 2);
}

/// Concurrent duplicates capture once; a full store refuses instead of discarding an unexpired
/// receipt, expired ones make room, and a refused admission leaves no receipt behind.
#[tokio::test]
async fn concurrent_duplicates_capture_once_and_capacity_never_drops_unexpired_receipts() {
    let app = app().await;
    seed(&app).await;
    let base = app.base.clone();
    let (indexer, client) = providers(&app).await;
    let c = app.db.connect().await.unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let body = all_missing(&id, &indexer, &client);
    let mut pending = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let (base, body) = (base.clone(), body.clone());
        pending.spawn(async move { request(&base, "POST", TV, Some(body)).await });
    }
    let mut replies = Vec::new();
    while let Some(reply) = pending.join_next().await {
        replies.push(reply.unwrap());
    }
    assert!(
        replies.iter().all(|(status, _)| *status == 202),
        "{replies:?}"
    );
    assert_eq!(
        replies
            .iter()
            .filter(|(_, r)| r["replayed"] == false)
            .count(),
        1
    );
    assert_eq!((search_rows(&c).await, receipts(&c).await), (3, 1));

    // Storage refuses a receipt beyond 4096 unexpired ones (and 65536 admitted targets).
    let t = now();
    c.execute(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<4095) INSERT INTO wanted_search_receipts(request_id,media_type,digest,matched,skipped_active,admitted,created_at,expires_at) SELECT printf('00000000-0000-4000-8000-%012x',i),'movies',zeroblob(32),0,0,0,?1,?1+1000 FROM n",
        [t],
    )
    .await
    .unwrap();
    assert_eq!(receipts(&c).await, 4096);
    let raw = c
        .execute(
            "INSERT INTO wanted_search_receipts(request_id,media_type,digest,matched,skipped_active,admitted,created_at,expires_at)VALUES('11111111-1111-4111-8111-111111111111','tv',zeroblob(32),0,0,0,?1,?1+1000)",
            [t],
        )
        .await;
    assert!(raw.is_err());
    // The API reports the full store and writes nothing: no receipt, no child commands.
    let fresh = search_body(
        &uuid::Uuid::new_v4().to_string(),
        "missing",
        json!({"kind":"ids","ids":[11]}),
        &indexer,
        &client,
    );
    let (status, error) = request(&base, "POST", TV, Some(fresh.clone())).await;
    assert_eq!(
        (status, error["error"]["code"].as_str()),
        (429, Some("wanted_receipt_capacity"))
    );
    assert_eq!((search_rows(&c).await, receipts(&c).await), (3, 4096));
    // The unexpired receipts are all still there and still authoritative.
    let (status, replay) = request(&base, "POST", TV, Some(body)).await;
    assert_eq!((status, replay["replayed"].as_bool()), (202, Some(true)));

    // Expired receipts do not count and are swept at the next admission: ten synthetic receipts
    // are replaced by expired ones, then the same request is admitted and they disappear.
    c.execute("DELETE FROM wanted_search_receipts WHERE request_id BETWEEN '00000000-0000-4000-8000-000000000001' AND '00000000-0000-4000-8000-00000000000a'", ()).await.unwrap();
    let removed = 4096 - receipts(&c).await;
    assert_eq!(removed, 10);
    c.execute(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<10) INSERT INTO wanted_search_receipts(request_id,media_type,digest,matched,skipped_active,admitted,created_at,expires_at) SELECT printf('00000000-0000-4000-8000-%012x',i),'movies',zeroblob(32),0,0,0,?1,?2 FROM n",
        [t - 100_000, t - 10],
    )
    .await
    .unwrap();
    assert_eq!(
        scalar(
            &c,
            &format!(
                "SELECT count(*) FROM wanted_search_receipts WHERE expires_at<={}",
                t - 10
            )
        )
        .await,
        10
    );
    let (status, receipt) = request(&base, "POST", TV, Some(fresh)).await;
    assert_eq!(status, 202, "{receipt}");
    assert_eq!(receipt["replayed"], false);
    assert_eq!(
        scalar(
            &c,
            &format!(
                "SELECT count(*) FROM wanted_search_receipts WHERE expires_at<={}",
                t - 10
            )
        )
        .await,
        0,
        "expired receipts were swept"
    );
    assert_eq!(
        receipts(&c).await,
        4096 - 10 + 1,
        "unexpired receipts all remain"
    );
}
