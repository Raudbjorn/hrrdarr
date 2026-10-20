//! Wanted missing/cutoff lists and Wanted search admission over a real owned HTTP server.
//! Library rows are seeded in SQL (no metadata provider); profiles, providers and every
//! search command are created through the production HTTP routes. Only the last-search
//! producer test starts the command worker, and its indexer is an owned 127.0.0.1 peer that
//! answers 404 (a caught, classified indexer failure); no other test makes a provider request.
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
    client: providers::RefreshClient,
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
            .merge(hrrdarr::wanted::router(db.clone(), client.clone()))
            .merge(hrrdarr::quality_profiles::router(db.clone()))
            .merge(hrrdarr::episodes::router(db.clone())),
    )
    .await;
    // Owned peer that is never expected to be contacted (no worker is started).
    let (remote, remote_server) = serve(axum::Router::new()).await;
    App {
        base,
        db,
        client,
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
    assert!(replay["matched"].is_null());
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

    // Oldest-searched-first under the bound: the capture order is total (never searched, then
    // oldest instant, then ID), so a bounded read and the admission order are deterministic.
    c.execute_batch(
        "UPDATE episodes SET monitored=0 WHERE id IN (1024,1025);
         UPDATE episodes SET native_last_search_at=10 WHERE id=3;
         UPDATE episodes SET native_last_search_at=5 WHERE id=1000;
         UPDATE episodes SET last_search_time='1970-01-01 00:00:07Z' WHERE id=500;",
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
    let order: Vec<i64> = command_targets(&receipt)
        .into_iter()
        .map(|(_, id)| id)
        .collect();
    assert_eq!(&order[..3], [1, 2, 4], "never searched first, by ID");
    assert_eq!(&order[1020..], [1000, 500, 3], "then oldest instant first");

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

fn item_by_id(page: &Value, id: i64) -> Value {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["target"]["id"] == id)
        .unwrap_or_else(|| panic!("{id} missing from {page}"))
        .clone()
}
fn command_ids(receipt: &Value) -> Vec<String> {
    receipt["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["command_id"].as_str().unwrap().to_owned())
        .collect()
}
async fn native_count(c: &libsql::Connection, table: &str) -> i64 {
    c.query(
        &format!("SELECT count(*) FROM {table} WHERE native_last_search_at IS NOT NULL"),
        (),
    )
    .await
    .unwrap()
    .next()
    .await
    .unwrap()
    .unwrap()
    .get(0)
    .unwrap()
}
async fn native_at(c: &libsql::Connection, table: &str, id: i64) -> Option<i64> {
    c.query(
        &format!("SELECT native_last_search_at FROM {table} WHERE id=?"),
        [id],
    )
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
async fn last_search_sort_and_oldest_first_admission_in_both_domains() {
    let app = app().await;
    seed(&app).await;
    let base = &app.base;
    let c = app.db.connect().await.unwrap();
    // Native dispatch facts and imported snapshot text compete by instant; equal instants tie
    // by typed ID; an unparseable import counts as never searched. Episode 1 and movie 1 carry
    // different facts although their numeric IDs collide.
    c.execute_batch(
        "UPDATE episodes SET native_last_search_at=1700000000 WHERE id=1;
         UPDATE episodes SET last_search_time='2023-11-14 22:13:20Z' WHERE id=5;
         UPDATE episodes SET native_last_search_at=1600000000,last_search_time='2024-01-01T00:00:00Z' WHERE id=7;
         UPDATE episodes SET native_last_search_at=1800000000,last_search_time='2020-01-01 00:00:00' WHERE id=8;
         UPDATE episodes SET last_search_time='not a time' WHERE id=18;
         UPDATE movies SET native_last_search_at=1750000000 WHERE id=1;
         UPDATE movies SET native_last_search_at=1700000000 WHERE id=4;
         UPDATE movies SET native_last_search_at=1650000000 WHERE id IN (5,8);",
    )
    .await
    .unwrap();

    let tv = get(
        base,
        "/api/v1/tv/wanted/missing?monitored=all&sort=last_search",
    )
    .await;
    assert_eq!(tv["sort"], "last_search");
    assert_eq!(
        ids(&tv),
        [18, 1, 5, 7, 8],
        "never searched first, then oldest; 1 and 5 tie by ID: {tv}"
    );
    for (id, time, source) in [
        (18, Value::Null, Value::Null),
        (1, json!("2023-11-14T22:13:20Z"), json!("native")),
        (5, json!("2023-11-14T22:13:20Z"), json!("imported")),
        (7, json!("2024-01-01T00:00:00Z"), json!("imported")),
        (8, json!("2027-01-15T08:00:00Z"), json!("native")),
    ] {
        let item = item_by_id(&tv, id);
        assert_eq!(
            (&item["last_search_time"], &item["last_search_source"]),
            (&time, &source),
            "{item}"
        );
    }
    assert_eq!(
        ids(&get(
            base,
            "/api/v1/tv/wanted/missing?monitored=all&sort=last_search&direction=desc"
        )
        .await),
        [8, 7, 1, 5, 18],
        "never searched trails descending; equal instants stay ascending by ID"
    );
    assert_eq!(
        ids(&get(base, "/api/v1/tv/wanted/cutoff?sort=last_search").await),
        [2, 3, 13, 15],
        "all never searched: typed-ID ties"
    );
    let first = get(
        base,
        "/api/v1/tv/wanted/missing?monitored=all&sort=last_search&limit=2&offset=1",
    )
    .await;
    assert_eq!(
        (ids(&first), first["total"].as_i64()),
        (vec![1, 5], Some(5))
    );

    let movies = get(
        base,
        "/api/v1/movies/wanted/missing?monitored=all&sort=last_search",
    )
    .await;
    assert_eq!(ids(&movies), [2, 4, 1], "{movies}");
    assert_eq!(
        item_by_id(&movies, 1)["last_search_time"],
        "2025-06-15T15:06:40Z",
        "movie 1 has its own fact, not episode 1's"
    );
    assert_eq!(item_by_id(&movies, 1)["last_search_source"], "native");
    assert!(item_by_id(&movies, 2)["last_search_time"].is_null());
    assert_eq!(
        ids(&get(
            base,
            "/api/v1/movies/wanted/missing?monitored=all&sort=last_search&direction=desc"
        )
        .await),
        [1, 4, 2]
    );
    for direction in ["asc", "desc"] {
        assert_eq!(
            ids(&get(
                base,
                &format!("/api/v1/movies/wanted/cutoff?sort=last_search&direction={direction}")
            )
            .await),
            [5, 8],
            "equal instants keep ascending movie IDs ({direction})"
        );
    }
    assert_eq!(
        ids(&get(
            base,
            "/api/v1/movies/wanted/missing?sort=last_search&availability=all"
        )
        .await),
        [2, 1]
    );

    // Search all captures and admits oldest-searched-first; child IDs sort in admission order,
    // so the worker's (priority, created_at, id) claim order follows it. Replay keeps it.
    let (indexer, client) = providers(&app).await;
    let all = search_body(
        &uuid::Uuid::new_v4().to_string(),
        "missing",
        json!({"kind":"all","monitored":"all"}),
        &indexer,
        &client,
    );
    let (status, receipt) =
        request(base, "POST", "/api/v1/tv/wanted/search", Some(all.clone())).await;
    assert_eq!(status, 202, "{receipt}");
    assert_eq!(
        command_targets(&receipt),
        [18, 1, 5, 7, 8].map(|id| ("episode".to_owned(), id)),
        "{receipt}"
    );
    let order = command_ids(&receipt);
    let mut sorted = order.clone();
    sorted.sort();
    assert_eq!(sorted, order, "claim order equals admission order");
    let (status, replay) = request(base, "POST", "/api/v1/tv/wanted/search", Some(all)).await;
    assert_eq!((status, &replay["replayed"]), (202, &json!(true)));
    assert_eq!(replay["commands"], receipt["commands"]);
    assert_eq!(
        native_count(&c, "episodes").await,
        3,
        "admission writes no fact (only the three seeded native facts exist)"
    );

    // Explicit selections are ordered the same way (active targets are not skipped for IDs).
    let explicit = search_body(
        &uuid::Uuid::new_v4().to_string(),
        "missing",
        json!({"kind":"ids","ids":[8,1,18,8]}),
        &indexer,
        &client,
    );
    let (status, receipt) = request(base, "POST", "/api/v1/tv/wanted/search", Some(explicit)).await;
    assert_eq!(status, 202, "{receipt}");
    assert_eq!(
        command_targets(&receipt),
        [18, 1, 8].map(|id| ("episode".to_owned(), id))
    );

    let movie_all = search_body(
        &uuid::Uuid::new_v4().to_string(),
        "missing",
        json!({"kind":"all","monitored":"all"}),
        &indexer,
        &client,
    );
    let (status, receipt) = request(
        base,
        "POST",
        "/api/v1/movies/wanted/search",
        Some(movie_all),
    )
    .await;
    assert_eq!(status, 202, "{receipt}");
    assert_eq!(
        command_targets(&receipt),
        [2, 4, 1].map(|id| ("movie".to_owned(), id))
    );
    let order = command_ids(&receipt);
    let mut sorted = order.clone();
    sorted.sort();
    assert_eq!(sorted, order);
    let available = search_body(
        &uuid::Uuid::new_v4().to_string(),
        "missing",
        json!({"kind":"all","monitored":"all","availability":"available"}),
        &indexer,
        &client,
    );
    let (status, receipt) = request(
        base,
        "POST",
        "/api/v1/movies/wanted/search",
        Some(available),
    )
    .await;
    assert_eq!(status, 202, "{receipt}");
    assert_eq!(
        (
            receipt["matched"].as_i64(),
            receipt["skipped_active"].as_i64()
        ),
        (Some(2), Some(2)),
        "availability-filtered capture uses the same order: {receipt}"
    );
}

#[tokio::test]
async fn native_last_search_records_dispatch_with_caught_failure_never_admission_or_cancel() {
    let app = app().await;
    seed(&app).await;
    let base = &app.base;
    let (indexer, client) = providers(&app).await;
    let c = app.db.connect().await.unwrap();
    let admit = async |path: &str, ids: Value| {
        let (status, receipt) = request(
            base,
            "POST",
            path,
            Some(search_body(
                &uuid::Uuid::new_v4().to_string(),
                "missing",
                json!({"kind":"ids","ids":ids}),
                &indexer,
                &client,
            )),
        )
        .await;
        assert_eq!(status, 202, "{receipt}");
        receipt["commands"][0]["command_id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    // Equal numeric targets in both domains, plus an episode whose search is cancelled before
    // any worker exists. Admission alone writes nothing.
    let episode = admit("/api/v1/tv/wanted/search", json!([1])).await;
    let movie = admit("/api/v1/movies/wanted/search", json!([1])).await;
    let cancelled = admit("/api/v1/tv/wanted/search", json!([18])).await;
    let (status, row) = request(
        base,
        "POST",
        &format!("/api/v1/search/commands/{cancelled}/cancel"),
        None,
    )
    .await;
    assert_eq!(
        (status, &row["status"]),
        (200, &json!("cancelled")),
        "{row}"
    );
    assert_eq!(native_count(&c, "episodes").await, 0);
    assert_eq!(native_count(&c, "movies").await, 0);
    assert!(
        item_by_id(&get(base, "/api/v1/tv/wanted/missing").await, 1)["last_search_time"].is_null()
    );

    // The owned indexer peer answers 404: the dispatch completes with a caught, classified
    // indexer failure, which still records the fact before the command settles.
    let runtime = hrrdarr::commands::start(app.db.clone(), app.client.clone())
        .await
        .unwrap();
    let read = async |id: &str| {
        let (status, row) =
            request(base, "GET", &format!("/api/v1/search/commands/{id}"), None).await;
        assert_eq!(status, 200, "{row}");
        row
    };
    let mut rows = Vec::new();
    for id in [&episode, &movie] {
        let row = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let row = read(id).await;
                if !row["dispatch_completed_at"].is_null()
                    && row["status"] != "running"
                    && row["status"] != "queued"
                {
                    return row;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("dispatch completion deadline");
        assert!(
            matches!(row["status"].as_str(), Some("retry_wait" | "failed")),
            "{row}"
        );
        // An indexer-side classification, not a pre-dispatch refusal or local failure.
        assert!(
            !matches!(
                row["error_code"].as_str(),
                None | Some("provider_changed" | "target_changed" | "storage_error")
            ),
            "{row}"
        );
        eprintln!("wanted_dispatch_failure_code={}", row["error_code"]);
        assert_eq!(row["fetch_complete"], false, "{row}");
        // Stop retries; a later attempt may still be in flight, so ignore a 409 here.
        request(
            base,
            "POST",
            &format!("/api/v1/search/commands/{id}/cancel"),
            None,
        )
        .await;
        rows.push(row);
    }
    runtime.shutdown().await;
    let episode_row = read(&episode).await;
    let movie_row = read(&movie).await;
    let episode_at = native_at(&c, "episodes", 1).await.expect("episode fact");
    let movie_at = native_at(&c, "movies", 1).await.expect("movie fact");
    assert_eq!(
        Some(episode_at),
        episode_row["dispatch_completed_at"].as_i64(),
        "fact and command progress commit together"
    );
    assert_eq!(Some(movie_at), movie_row["dispatch_completed_at"].as_i64());
    assert!(rows[0]["dispatch_completed_at"].as_i64().unwrap() <= episode_at);
    assert!(
        episode_row["started_at"].as_i64().unwrap() <= episode_at
            && episode_at >= episode_row["created_at"].as_i64().unwrap()
    );
    assert_eq!(
        (
            native_count(&c, "episodes").await,
            native_count(&c, "movies").await
        ),
        (1, 1),
        "only the dispatched typed targets"
    );
    let cancelled_row = read(&cancelled).await;
    assert!(cancelled_row["dispatch_completed_at"].is_null());
    assert!(cancelled_row["started_at"].is_null(), "{cancelled_row}");
    assert_eq!(native_at(&c, "episodes", 18).await, None);

    let expected = chrono::DateTime::from_timestamp(episode_at, 0)
        .unwrap()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let tv = get(base, "/api/v1/tv/wanted/missing?sort=last_search").await;
    assert_eq!(ids(&tv), [5, 18, 1], "{tv}");
    assert_eq!(item_by_id(&tv, 1)["last_search_time"], expected);
    assert_eq!(item_by_id(&tv, 1)["last_search_source"], "native");
    let movies = get(base, "/api/v1/movies/wanted/missing?sort=last_search").await;
    assert_eq!(ids(&movies), [2, 1], "{movies}");
    assert!(item_by_id(&movies, 1)["last_search_time"].is_string());
}
