//! Calendar JSON and iCalendar feed over a real owned HTTP server and scratch databases.
//! Library rows are seeded in SQL (no metadata provider); episode and movie IDs deliberately
//! collide so typed targets and event IDs must keep the domains apart. The production-binary
//! test checks route wiring and the host authority guard; no installed service is contacted.
use hrrdarr::db::Database;
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("hrrdarr-calendar-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
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
struct App {
    base: String,
    db: Arc<Database>,
    _server: Server,
    _scratch: Scratch,
}
async fn app() -> App {
    let scratch = Scratch::new();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let router = hrrdarr::calendar::router(db.clone());
    let server = Server(tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap()
    }));
    App {
        base,
        db,
        _server: server,
        _scratch: scratch,
    }
}
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap()
}
async fn fetch(base: &str, path: &str) -> (u16, String, String) {
    let response = client().get(format!("{base}{path}")).send().await.unwrap();
    let status = response.status().as_u16();
    let kind = response
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    (status, kind, response.text().await.unwrap())
}
async fn json(base: &str, path: &str) -> (u16, Value) {
    let (status, _, text) = fetch(base, path).await;
    (
        status,
        serde_json::from_str(&text).unwrap_or_else(|_| panic!("{status} {text}")),
    )
}
async fn ok(base: &str, path: &str) -> Value {
    let (status, body) = json(base, path).await;
    assert_eq!(status, 200, "{path}: {body}");
    body
}
fn events(page: &Value) -> Vec<String> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["event_id"].as_str().unwrap().to_string())
        .collect()
}
async fn exec(db: &Database, sql: &str) {
    db.connect()
        .await
        .unwrap()
        .execute_batch(sql)
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

/// Episodes 1–14 and movies 1–8 share numeric IDs. Window under test: October 2026 UTC.
const SEED: &str = "
INSERT INTO series(id,tvdb_id,title,path,monitored,runtime,network) VALUES
 (1,9001,'Harbor','/owned/tv/harbor',1,45,'Net'),
 (2,9002,'Daily Desk','/owned/tv/daily',0,NULL,NULL);
INSERT INTO seasons(series_id,number,monitored) VALUES (1,0,1),(1,1,1),(2,2026,1);
INSERT INTO library_settings(media_type,series_id,series_type) VALUES ('tv',1,'standard'),('tv',2,'daily');
INSERT INTO episode_files(id,series_id,path) VALUES (1,1,'/owned/tv/harbor/s01e12.mkv');
INSERT INTO episodes(id,series_id,season,number,title,monitored,air_date,air_date_utc,runtime,episode_file_id,overview) VALUES
 (1,1,1,1,'At start',1,'2026-09-30','2026-10-01T00:00:00Z',50,NULL,'First'),
 (2,1,1,2,'Canonical encoding',1,'2026-10-31','2026-10-31 23:59:59',NULL,NULL,NULL),
 (3,1,1,3,'At end',1,'2026-10-31','2026-11-01T00:00:00Z',NULL,NULL,NULL),
 (4,1,1,4,'Before start',1,'2026-09-30','2026-09-30T23:59:59Z',NULL,NULL,NULL),
 (5,1,1,5,'Offset stored',1,'2026-10-10','2026-10-10T22:30:00-03:00',NULL,NULL,NULL),
 (6,1,1,6,'Date only',1,'2026-10-12',NULL,NULL,NULL,NULL),
 (7,1,1,7,'Unmonitored episode',0,'2026-10-05','2026-10-05T00:00:00Z',NULL,NULL,NULL),
 (8,1,0,1,'Special',1,'2026-10-06','2026-10-06T00:00:00Z',NULL,NULL,NULL),
 (9,1,1,9,'Impossible day',1,NULL,'2026-02-30 10:00:00',NULL,NULL,NULL),
 (10,1,1,10,'Garbage',1,NULL,'not a date',NULL,NULL,NULL),
 (11,2,2026,1,'Unmonitored series',1,'2026-10-07','2026-10-07T12:00:00Z',30,NULL,NULL),
 (12,1,1,12,'With file',1,'2026-10-08','2026-10-08T00:00:00Z',NULL,1,NULL),
 (13,1,1,13,'Fraction before end',1,'2026-10-31','2026-10-31T23:59:59.999Z',NULL,NULL,NULL),
 (14,1,1,14,'No date',1,NULL,NULL,NULL,NULL,NULL);
INSERT INTO movie_metadata(id,tmdb_id,title,year,in_cinemas,digital_release,physical_release,status,studio,runtime) VALUES
 (1,7001,'Triple',2026,'2026-10-01 00:00:00','2026-10-15 12:00:00','2026-10-31 23:59:59','released','Studio',100),
 (2,7002,'Same Day',2026,'2026-10-20 10:00:00','2026-10-20 11:00:00','2026-10-20 12:00:00','announced',NULL,NULL),
 (3,7003,'Edges',2026,'2026-09-30T23:59:59Z','2026-11-01T00:00:00Z','2026-10-10 00:00:00',NULL,NULL,NULL),
 (4,7004,'Unmonitored',2026,NULL,'2026-10-05 00:00:00',NULL,NULL,NULL,NULL),
 (5,7005,'Offset',2026,NULL,'2026-10-09T00:00:00+05:00',NULL,NULL,NULL,NULL),
 (6,7006,'Impossible',2026,NULL,'2026-10-31 00:00:00x','xxxxxxxxxxxxxxxxxxxx',NULL,NULL,NULL),
 (7,7007,'Catalog only',2026,NULL,'2026-10-05 00:00:00',NULL,NULL,NULL,NULL),
 (8,7008,'No dates',2026,NULL,NULL,NULL,NULL,NULL,NULL);
INSERT INTO movies(id,metadata_id,path,monitored) VALUES
 (1,1,'/owned/movies/triple',1),(2,2,'/owned/movies/same',1),(3,3,'/owned/movies/edges',1),
 (4,4,'/owned/movies/unmonitored',0),(5,5,'/owned/movies/offset',1),(6,6,'/owned/movies/impossible',1),
 (8,8,'/owned/movies/nodates',1);
INSERT INTO movie_files(id,movie_id,path) VALUES (1,5,'/owned/movies/offset/offset.mkv');
INSERT INTO tags(id,media_type,label) VALUES (1,'tv','harbor'),(2,'movies','same-day');
INSERT INTO series_tags(series_id,tag_id) VALUES (1,1);
INSERT INTO movie_tags(movie_id,tag_id) VALUES (2,2);
";
const OCTOBER: &str = "start=2026-10-01&end=2026-11-01";

#[tokio::test]
async fn tv_window_monitoring_specials_tags_and_invalid_dates() {
    let app = app().await;
    exec(&app.db, SEED).await;
    let watcher = app.db.connect().await.unwrap();
    let before = data_version(&watcher).await;

    let page = ok(&app.base, &format!("/api/v1/tv/calendar?{OCTOBER}")).await;
    // Start inclusive, end exclusive (half-open); stored encodings RFC 3339, naive-UTC SQL,
    // non-UTC offset and fractional seconds are all compared as instants.
    assert_eq!(
        events(&page),
        [
            "episode-1",
            "episode-8",
            "episode-12",
            "episode-5",
            "episode-2",
            "episode-13"
        ]
    );
    assert_eq!(page["media_type"], "tv");
    assert_eq!(
        (&page["start"], &page["end"]),
        (
            &Value::from("2026-10-01T00:00:00Z"),
            &Value::from("2026-11-01T00:00:00Z")
        )
    );
    assert_eq!(page["total"], 6);
    assert_eq!(page["limit"], 2000);
    // Impossible day and garbage are invalid (never shown or guessed); the date-only episode
    // is reported, not placed at an invented time; an episode with no dates is not counted.
    assert_eq!(page["diagnostics"]["invalid_dates"], 2);
    assert_eq!(page["diagnostics"]["date_only_air_dates"], 1);
    let items = page["items"].as_array().unwrap();
    for item in items {
        assert_eq!(item["target"]["media_type"], "episode");
        assert_eq!(
            item["event_id"],
            format!("episode-{}", item["target"]["id"])
        );
    }
    let first = &items[0];
    assert_eq!(first["starts_at"], "2026-10-01T00:00:00Z");
    // Source-local date is preserved separately from the UTC instant.
    assert_eq!(first["air_date"], "2026-09-30");
    assert_eq!(first["network"], "Net");
    assert_eq!(first["overview"], "First");
    // Series runtime wins over the episode runtime; the end is derived from it.
    assert_eq!(
        (
            &first["runtime_minutes"],
            &first["runtime_source"],
            &first["ends_at"]
        ),
        (
            &Value::from(45),
            &Value::from("series"),
            &Value::from("2026-10-01T00:45:00Z")
        )
    );
    assert_eq!(first["has_file"], false);
    assert_eq!(items[2]["has_file"], true);
    assert_eq!(items[3]["starts_at"], "2026-10-11T01:30:00Z");
    assert_eq!(items[4]["starts_at"], "2026-10-31T23:59:59Z");
    assert_eq!(items[5]["starts_at"], "2026-10-31T23:59:59.999Z");
    assert_eq!(items[1]["season"], 0);

    // Unmonitored episode and episodes of an unmonitored series appear only on request.
    let all = ok(
        &app.base,
        &format!("/api/v1/tv/calendar?{OCTOBER}&unmonitored=true"),
    )
    .await;
    assert_eq!(
        events(&all),
        [
            "episode-1",
            "episode-7",
            "episode-8",
            "episode-11",
            "episode-12",
            "episode-5",
            "episode-2",
            "episode-13"
        ]
    );
    let daily = &all["items"][3];
    assert_eq!(
        (
            &daily["series_type"],
            &daily["series_monitored"],
            &daily["monitored"],
            &daily["effective_monitored"]
        ),
        (
            &Value::from("daily"),
            &Value::from(false),
            &Value::from(true),
            &Value::from(false)
        )
    );
    // Missing series runtime falls back to the episode runtime, labelled as such.
    assert_eq!(
        (&daily["runtime_minutes"], &daily["runtime_source"]),
        (&Value::from(30), &Value::from("episode"))
    );
    assert_eq!(all["items"][1]["effective_monitored"], false);

    let no_specials = ok(
        &app.base,
        &format!("/api/v1/tv/calendar?{OCTOBER}&specials=false"),
    )
    .await;
    assert!(!events(&no_specials).contains(&"episode-8".to_string()));
    assert_eq!(no_specials["include_specials"], false);

    // Tags are OR-filtered within the TV domain; the movie tag ID matches no series.
    let tagged = ok(
        &app.base,
        &format!("/api/v1/tv/calendar?{OCTOBER}&unmonitored=true&tags=1"),
    )
    .await;
    assert!(!events(&tagged).contains(&"episode-11".to_string()));
    assert_eq!(tagged["total"], 7);
    let cross = ok(&app.base, &format!("/api/v1/tv/calendar?{OCTOBER}&tags=2")).await;
    assert_eq!(cross["total"], 0);
    assert_eq!(cross["diagnostics"]["invalid_dates"], 0);
    let either = ok(
        &app.base,
        &format!("/api/v1/tv/calendar?{OCTOBER}&tags=2,1"),
    )
    .await;
    assert_eq!(either["total"], 6);

    // Window bounds are instants: a local-offset window moves the boundary by the offset.
    let local = ok(
        &app.base,
        "/api/v1/tv/calendar?start=2026-10-10T00:00:00-03:00&end=2026-10-11T00:00:00-03:00",
    )
    .await;
    assert_eq!(events(&local), ["episode-5"]);
    assert_eq!(local["first_date"], "2026-10-10");
    let utc_day = ok(
        &app.base,
        "/api/v1/tv/calendar?start=2026-10-11&end=2026-10-12",
    )
    .await;
    assert_eq!(events(&utc_day), ["episode-5"]);
    let none = ok(
        &app.base,
        "/api/v1/tv/calendar?start=2026-10-11T00:00:00-03:00&end=2026-10-12T00:00:00-03:00",
    )
    .await;
    assert_eq!(events(&none), Vec::<String>::new());
    // Exactly the end instant is excluded; one second earlier is included.
    let tail = ok(
        &app.base,
        "/api/v1/tv/calendar?start=2026-10-31T23:59:59Z&end=2026-11-01T00:00:00Z",
    )
    .await;
    assert_eq!(events(&tail), ["episode-2", "episode-13"]);
    let after = ok(
        &app.base,
        "/api/v1/tv/calendar?start=2026-11-01T00:00:00Z&end=2026-11-01T00:00:01Z",
    )
    .await;
    assert_eq!(events(&after), ["episode-3"]);

    // GET never writes.
    assert_eq!(data_version(&watcher).await, before);
}

#[tokio::test]
async fn movie_release_types_are_separate_all_day_entries() {
    let app = app().await;
    exec(&app.db, SEED).await;
    let page = ok(&app.base, &format!("/api/v1/movies/calendar?{OCTOBER}")).await;
    // Each release type is its own entry; same-day releases stay distinct; out-of-window
    // release dates of a selected movie are not emitted (strict event window); unmonitored and
    // catalog-only movies are excluded.
    assert_eq!(
        events(&page),
        [
            "movie-1-in_cinemas",
            "movie-5-digital",
            "movie-3-physical",
            "movie-1-digital",
            "movie-2-in_cinemas",
            "movie-2-digital",
            "movie-2-physical",
            "movie-1-physical"
        ]
    );
    assert_eq!(page["diagnostics"]["invalid_dates"], 2);
    assert_eq!(page["diagnostics"]["date_only_air_dates"], Value::Null);
    assert_eq!(
        page["release_types"],
        serde_json::json!(["in_cinemas", "digital", "physical"])
    );
    let items = page["items"].as_array().unwrap();
    for item in items {
        assert_eq!(item["target"]["media_type"], "movie");
    }
    let triple: Vec<_> = items
        .iter()
        .filter(|i| i["target"]["id"] == 1)
        .map(|i| {
            (
                i["release_type"].as_str().unwrap(),
                i["date"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        triple,
        [
            ("in_cinemas", "2026-10-01"),
            ("digital", "2026-10-15"),
            ("physical", "2026-10-31")
        ]
    );
    // Offset-stored release: the all-day date is the UTC date of the stored instant.
    let offset = items.iter().find(|i| i["target"]["id"] == 5).unwrap();
    assert_eq!(
        (&offset["date"], &offset["released_at"], &offset["has_file"]),
        (
            &Value::from("2026-10-08"),
            &Value::from("2026-10-08T19:00:00Z"),
            &Value::from(true)
        )
    );
    assert_eq!(items[0]["studio"], "Studio");
    assert_eq!(items[0]["runtime_minutes"], 100);

    let digital = ok(
        &app.base,
        &format!("/api/v1/movies/calendar?{OCTOBER}&release_types=digital"),
    )
    .await;
    assert_eq!(
        events(&digital),
        ["movie-5-digital", "movie-1-digital", "movie-2-digital"]
    );
    // Only the selected release columns are diagnosed.
    assert_eq!(digital["diagnostics"]["invalid_dates"], 1);
    let all = ok(
        &app.base,
        &format!("/api/v1/movies/calendar?{OCTOBER}&unmonitored=true&release_types=digital"),
    )
    .await;
    assert_eq!(
        events(&all),
        [
            "movie-4-digital",
            "movie-5-digital",
            "movie-1-digital",
            "movie-2-digital"
        ]
    );
    assert_eq!(all["items"][0]["monitored"], false);
    let tagged = ok(
        &app.base,
        &format!("/api/v1/movies/calendar?{OCTOBER}&tags=2"),
    )
    .await;
    assert_eq!(
        events(&tagged),
        ["movie-2-in_cinemas", "movie-2-digital", "movie-2-physical"]
    );
    let cross = ok(
        &app.base,
        &format!("/api/v1/movies/calendar?{OCTOBER}&tags=1"),
    )
    .await;
    assert_eq!(cross["total"], 0);

    // All-day dates do not shift with the viewer offset: a +05:00 local day selects the
    // movie whose UTC release date is that civil date, and the end date is exclusive.
    let local = ok(
        &app.base,
        "/api/v1/movies/calendar?start=2026-10-08T00:00:00%2B05:00&end=2026-10-09T00:00:00%2B05:00",
    )
    .await;
    assert_eq!(events(&local), ["movie-5-digital"]);
    assert_eq!(
        (&local["first_date"], &local["end_date"]),
        (&Value::from("2026-10-08"), &Value::from("2026-10-09"))
    );
    let edge = ok(
        &app.base,
        "/api/v1/movies/calendar?start=2026-10-31&end=2026-11-01",
    )
    .await;
    assert_eq!(events(&edge), ["movie-1-physical"]);
    let next = ok(
        &app.base,
        "/api/v1/movies/calendar?start=2026-11-01&end=2026-11-02",
    )
    .await;
    assert_eq!(events(&next), ["movie-3-digital"]);
    // A partial-day instant window still covers the whole all-day date.
    let partial = ok(
        &app.base,
        "/api/v1/movies/calendar?start=2026-10-20T13:00:00Z&end=2026-10-20T14:00:00Z",
    )
    .await;
    assert_eq!(events(&partial).len(), 3);
}

#[tokio::test]
async fn colliding_ids_stay_typed_across_domains() {
    let app = app().await;
    exec(&app.db, SEED).await;
    let tv = ok(&app.base, &format!("/api/v1/tv/calendar?{OCTOBER}")).await;
    let movies = ok(&app.base, &format!("/api/v1/movies/calendar?{OCTOBER}")).await;
    let tv_ids: Vec<_> = tv["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["target"]["id"].as_i64().unwrap())
        .collect();
    let movie_ids: Vec<_> = movies["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["target"]["id"].as_i64().unwrap())
        .collect();
    assert!(
        tv_ids.contains(&1)
            && movie_ids.contains(&1)
            && tv_ids.contains(&5)
            && movie_ids.contains(&5)
    );
    let tv_events = events(&tv);
    for event in events(&movies) {
        assert!(!tv_events.contains(&event), "{event} collides");
    }
    // Deleting movie 1 changes only the movie calendar, never episode 1.
    exec(&app.db, "DELETE FROM movies WHERE id=1;").await;
    let tv_after = ok(&app.base, &format!("/api/v1/tv/calendar?{OCTOBER}")).await;
    assert_eq!(events(&tv_after), tv_events);
    let movies_after = ok(&app.base, &format!("/api/v1/movies/calendar?{OCTOBER}")).await;
    assert!(
        !events(&movies_after)
            .iter()
            .any(|e| e.starts_with("movie-1-"))
    );
}

#[tokio::test]
async fn invalid_queries_are_rejected_with_a_static_envelope() {
    let app = app().await;
    for path in [
        "/api/v1/tv/calendar",
        "/api/v1/tv/calendar?start=2026-10-01",
        "/api/v1/tv/calendar?start=2026-10-05&end=2026-10-01",
        "/api/v1/tv/calendar?start=2026-01-01&end=2027-01-03",
        "/api/v1/tv/calendar?start=2026-10-01&end=2026-10-05&release_types=digital",
        "/api/v1/movies/calendar?start=2026-10-01&end=2026-10-05&specials=false",
        "/api/v1/movies/calendar?start=2026-10-01&end=2026-10-05&unmonitored=yes",
        "/api/v1/movies/calendar?start=2026-10-01T00:00:00+05:00&end=2026-10-05",
        "/api/v1/movies/calendar?start=2026-10-01&end=2026-10-05&end=2026-10-06",
        "/api/v1/movies/calendar?start=2026-10-01&end=2026-10-05&limit=10",
        "/feed/v1/calendar/tv.ics?start=2026-10-01",
        "/feed/v1/calendar/movies.ics?past_days=300&future_days=100",
        "/feed/v1/calendar/movies.ics?specials=false",
    ] {
        let (status, body) = json(&app.base, path).await;
        assert_eq!(status, 400, "{path}: {body}");
        assert_eq!(body["error"]["code"], "invalid_calendar_query", "{path}");
    }
    // The largest window is accepted and an empty library is a successful empty calendar.
    let page = ok(
        &app.base,
        "/api/v1/tv/calendar?start=2024-01-01&end=2025-01-01",
    )
    .await;
    assert_eq!(
        (
            page["total"].as_i64(),
            page["diagnostics"]["invalid_dates"].as_i64()
        ),
        (Some(0), Some(0))
    );
}

#[tokio::test]
async fn entry_bound_is_an_explicit_413_for_both_domains() {
    let app = app().await;
    exec(
        &app.db,
        "INSERT INTO series(id,title,path,monitored) VALUES (1,'Bulk','/owned/bulk',1);
         INSERT INTO seasons(series_id,number) VALUES (1,1);
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<2000)
         INSERT INTO episodes(series_id,season,number,title,air_date_utc)
         SELECT 1,1,i,'E'||i,datetime(1790812800+i*60,'unixepoch') FROM n;",
    )
    .await;
    let window = "start=2026-10-01&end=2026-10-08";
    let page = ok(&app.base, &format!("/api/v1/tv/calendar?{window}")).await;
    assert_eq!(page["total"], 2000);
    exec(
        &app.db,
        "INSERT INTO episodes(series_id,season,number,title,air_date_utc) VALUES (1,1,2001,'Over','2026-10-02 00:00:00');",
    )
    .await;
    let (status, body) = json(&app.base, &format!("/api/v1/tv/calendar?{window}")).await;
    assert_eq!(status, 413, "{body}");
    assert_eq!(body["error"]["code"], "calendar_resource_limit");
    // Narrowing the window is the documented remedy.
    let narrow = ok(
        &app.base,
        "/api/v1/tv/calendar?start=2026-10-01&end=2026-10-02",
    )
    .await;
    assert!(narrow["total"].as_i64().unwrap() < 2000);

    // 667 movies x 3 release types = 2001 entries: release types count separately.
    exec(
        &app.db,
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<667)
         INSERT INTO movie_metadata(id,title,year,in_cinemas,digital_release,physical_release)
         SELECT i,'M'||i,2026,'2026-10-02 00:00:00','2026-10-03 00:00:00','2026-10-04 00:00:00' FROM n;
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<667)
         INSERT INTO movies(id,metadata_id,path) SELECT i,i,'/owned/m/'||i FROM n;",
    )
    .await;
    let (status, body) = json(&app.base, &format!("/api/v1/movies/calendar?{window}")).await;
    assert_eq!(status, 413, "{body}");
    assert_eq!(body["error"]["code"], "calendar_resource_limit");
    let two = ok(
        &app.base,
        &format!("/api/v1/movies/calendar?{window}&release_types=in_cinemas,physical"),
    )
    .await;
    assert_eq!(two["total"], 1334);
}

/// Independent RFC 5545 unfolding: CRLF followed by one space or tab is removed.
fn unfold(text: &str) -> Vec<String> {
    text.replace("\r\n ", "")
        .replace("\r\n\t", "")
        .split("\r\n")
        .map(str::to_owned)
        .collect()
}
fn vevents(text: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut current: Option<Vec<String>> = None;
    for line in unfold(text) {
        match line.as_str() {
            "BEGIN:VEVENT" => current = Some(Vec::new()),
            "END:VEVENT" => out.push(current.take().expect("END without BEGIN")),
            _ => {
                if let Some(event) = current.as_mut() {
                    event.push(line)
                }
            }
        }
    }
    assert!(current.is_none(), "unterminated VEVENT");
    out
}
fn property<'a>(event: &'a [String], name: &str) -> Option<&'a str> {
    event.iter().find_map(|line| {
        line.strip_prefix(name).and_then(|rest| {
            rest.strip_prefix(':')
                .or_else(|| rest.strip_prefix(";VALUE=DATE:"))
        })
    })
}
fn assert_wire_format(text: &str) {
    assert!(
        text.starts_with("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n"),
        "{text}"
    );
    assert!(text.ends_with("END:VCALENDAR\r\n"));
    let stripped = text.replace("\r\n", "");
    assert!(
        !stripped.contains('\n') && !stripped.contains('\r'),
        "bare line break"
    );
    for line in text.trim_end_matches("\r\n").split("\r\n") {
        assert!(line.len() <= 75, "{} octets: {line}", line.len());
        assert!(!line.is_empty());
    }
    for event in vevents(text) {
        for required in ["UID", "DTSTAMP", "DTSTART", "SUMMARY", "STATUS"] {
            assert!(
                property(&event, required).is_some(),
                "{required} missing: {event:?}"
            );
        }
    }
}
fn ymd(days: i64) -> String {
    (chrono::Utc::now().date_naive() + chrono::Duration::days(days))
        .format("%Y-%m-%d")
        .to_string()
}

#[tokio::test]
async fn ical_feeds_are_valid_stable_and_fail_closed() {
    let app = app().await;
    // Empty library: a valid empty calendar for both domains.
    for domain in ["tv", "movies"] {
        let (status, kind, text) =
            fetch(&app.base, &format!("/feed/v1/calendar/{domain}.ics")).await;
        assert_eq!(status, 200, "{text}");
        assert_eq!(kind, "text/calendar; charset=utf-8");
        assert_wire_format(&text);
        assert!(vevents(&text).is_empty());
    }
    let overview = "Ünïcödé 🎬 recap, with; specials\\ and\nline breaks. ".repeat(4);
    let tomorrow = ymd(1);
    exec(
        &app.db,
        &format!(
            "INSERT INTO series(id,title,path,monitored,runtime,network) VALUES (1,'Harbor, Inc; Ltd','/owned/tv/h',1,45,'Net, East'),(2,'Daily Desk','/owned/tv/d',1,NULL,NULL);
             INSERT INTO seasons(series_id,number) VALUES (1,1),(2,2026);
             INSERT INTO library_settings(media_type,series_id,series_type) VALUES ('tv',2,'daily');
             INSERT INTO episode_files(id,series_id,path) VALUES (1,1,'/owned/tv/h/e2.mkv');
             INSERT INTO episodes(id,series_id,season,number,title,air_date_utc,overview,episode_file_id) VALUES
              (1,1,1,1,'Pilot','{tomorrow} 01:00:00','{}',NULL),
              (2,1,1,2,'Second','{tomorrow} 02:00:00',NULL,1),
              (3,2,2026,1,'Monday','{tomorrow}T03:00:00Z',NULL,NULL),
              (4,1,1,4,'Far future','{} 01:00:00',NULL,NULL);
             INSERT INTO movie_metadata(id,title,year,in_cinemas,digital_release,physical_release,status,studio) VALUES
              (1,'Harbor',2026,'{tomorrow} 00:00:00','{tomorrow} 00:00:00','{} 00:00:00','announced','Studio; One');
             INSERT INTO movies(id,metadata_id,path) VALUES (1,1,'/owned/m/1');",
            overview.replace('\'', "''"),
            ymd(400),
            ymd(2),
        ),
    )
    .await;
    let (status, kind, tv) = fetch(&app.base, "/feed/v1/calendar/tv.ics").await;
    assert_eq!(
        (status, kind.as_str()),
        (200, "text/calendar; charset=utf-8"),
        "{tv}"
    );
    assert_wire_format(&tv);
    let tv_events = vevents(&tv);
    let uids: Vec<_> = tv_events
        .iter()
        .map(|e| property(e, "UID").unwrap())
        .collect();
    // Default feed window (7 past, 28 future days) excludes the far-future episode.
    assert_eq!(
        uids,
        [
            "episode-1@hrrdarr",
            "episode-2@hrrdarr",
            "episode-3@hrrdarr"
        ]
    );
    let compact = tomorrow.replace('-', "");
    let pilot = &tv_events[0];
    assert_eq!(
        property(pilot, "DTSTART"),
        Some(format!("{compact}T010000Z").as_str())
    );
    assert_eq!(property(pilot, "DURATION"), Some("PT45M"));
    assert_eq!(
        property(pilot, "SUMMARY"),
        Some("Harbor\\, Inc\\; Ltd - 1x01 - Pilot")
    );
    assert_eq!(property(pilot, "CATEGORIES"), Some("Net\\, East"));
    assert_eq!(property(pilot, "STATUS"), Some("TENTATIVE"));
    let description = property(pilot, "DESCRIPTION").unwrap();
    assert_eq!(
        description,
        overview
            .replace('\\', "\\\\")
            .replace(',', "\\,")
            .replace(';', "\\;")
            .replace('\n', "\\n")
    );
    assert!(tv.contains("\r\n "), "long description must be folded");
    assert_eq!(property(&tv_events[1], "STATUS"), Some("CONFIRMED"));
    // Daily series summaries omit season/episode numbering; unknown runtime has no duration.
    assert_eq!(
        property(&tv_events[2], "SUMMARY"),
        Some("Daily Desk - Monday")
    );
    assert_eq!(property(&tv_events[2], "DURATION"), None);

    let (status, _, movies) = fetch(&app.base, "/feed/v1/calendar/movies.ics").await;
    assert_eq!(status, 200, "{movies}");
    assert_wire_format(&movies);
    let movie_events = vevents(&movies);
    let uids: Vec<_> = movie_events
        .iter()
        .map(|e| property(e, "UID").unwrap())
        .collect();
    assert_eq!(
        uids,
        [
            "movie-1-in_cinemas@hrrdarr",
            "movie-1-digital@hrrdarr",
            "movie-1-physical@hrrdarr"
        ]
    );
    let next_day = ymd(2).replace('-', "");
    for event in &movie_events[..2] {
        assert!(event.contains(&format!("DTSTART;VALUE=DATE:{compact}")));
        assert!(
            event.contains(&format!("DTEND;VALUE=DATE:{next_day}")),
            "exclusive end"
        );
        assert_eq!(property(event, "STATUS"), Some("TENTATIVE"));
        assert_eq!(property(event, "CATEGORIES"), Some("Studio\\; One"));
    }
    assert_eq!(
        property(&movie_events[0], "SUMMARY"),
        Some("Harbor - In cinemas")
    );
    assert_eq!(
        property(&movie_events[1], "SUMMARY"),
        Some("Harbor - Digital release")
    );
    assert_eq!(
        property(&movie_events[2], "SUMMARY"),
        Some("Harbor - Physical release")
    );
    let physical = fetch(
        &app.base,
        "/feed/v1/calendar/movies.ics?release_types=physical",
    )
    .await
    .2;
    assert_eq!(vevents(&physical).len(), 1);

    // UIDs are stable identities: a date change moves the event, never renames it.
    exec(
        &app.db,
        &format!("UPDATE movie_metadata SET digital_release='{} 00:00:00' WHERE id=1; UPDATE episodes SET air_date_utc='{} 05:00:00' WHERE id=1;", ymd(3), ymd(3)),
    )
    .await;
    let moved = vevents(&fetch(&app.base, "/feed/v1/calendar/movies.ics").await.2);
    let digital = moved
        .iter()
        .find(|e| property(e, "UID") == Some("movie-1-digital@hrrdarr"))
        .unwrap();
    assert_eq!(
        property(digital, "DTSTART"),
        Some(ymd(3).replace('-', "").as_str())
    );
    let moved_tv = vevents(&fetch(&app.base, "/feed/v1/calendar/tv.ics").await.2);
    assert_eq!(
        property(moved_tv.last().unwrap(), "UID"),
        Some("episode-1@hrrdarr")
    );

    // Past/future days are explicit bounds; past_days=0&future_days=0 is today only.
    let today_only = fetch(
        &app.base,
        "/feed/v1/calendar/tv.ics?past_days=0&future_days=0",
    )
    .await
    .2;
    assert!(vevents(&today_only).is_empty());

    // An invalid stored date anywhere under the filters refuses the whole feed.
    exec(
        &app.db,
        "UPDATE episodes SET air_date_utc='2026-13-01 00:00:00' WHERE id=4;",
    )
    .await;
    let (status, body) = json(&app.base, "/feed/v1/calendar/tv.ics").await;
    assert_eq!(status, 500, "{body}");
    assert_eq!(body["error"]["code"], "calendar_state_invalid");
    // The movie feed is unaffected by a TV fact.
    assert_eq!(
        fetch(&app.base, "/feed/v1/calendar/movies.ics").await.0,
        200
    );
}

struct Process {
    child: std::process::Child,
    output: PathBuf,
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Process {
    fn spawn(directory: &Path, hosts: &str) -> Self {
        let output = directory.join("output");
        let file = std::fs::File::create(&output).unwrap();
        let child = std::process::Command::new(env!("CARGO_BIN_EXE_hrrdarr"))
            .env_clear()
            .env("HRRDARR_DATABASE_PATH", directory.join("db"))
            .env("HRRDARR_BIND", "127.0.0.1:0")
            .env("HRRDARR_ALLOWED_HOSTS", hosts)
            .current_dir(directory)
            .stdout(file.try_clone().unwrap())
            .stderr(file)
            .spawn()
            .unwrap();
        Self { child, output }
    }
    async fn ready(&mut self) -> String {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            assert!(self.child.try_wait().unwrap().is_none(), "process exited");
            let text = std::fs::read_to_string(&self.output).unwrap_or_default();
            if let Some(origin) = text
                .lines()
                .find_map(|line| line.strip_prefix("hrrdarr listening on "))
            {
                return origin.trim().to_string();
            }
            assert!(Instant::now() < deadline, "startup timeout: {text}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}
/// Raw HTTP/1.1 so the Host header is exactly what the test sets.
async fn raw_get(origin: &str, path: &str, host: &str) -> (u16, String, String) {
    let address = origin.strip_prefix("http://").unwrap();
    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(20), stream.read_to_end(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    let text = String::from_utf8(bytes).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    let kind = head
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-type: ")
                .map(str::to_owned)
        })
        .unwrap_or_default();
    (status, kind, body.to_string())
}

#[tokio::test]
async fn production_routes_pass_the_same_host_authority_guard() {
    let scratch = Scratch::new();
    let mut process = Process::spawn(&scratch.0, "allowed.example,127.0.0.1");
    let origin = process.ready().await;
    for path in [
        "/feed/v1/calendar/tv.ics",
        "/feed/v1/calendar/movies.ics?past_days=1&future_days=1",
    ] {
        let (status, kind, body) = raw_get(&origin, path, "allowed.example").await;
        assert_eq!(status, 200, "{path}: {body}");
        assert_eq!(kind, "text/calendar; charset=utf-8");
        assert!(body.starts_with("BEGIN:VCALENDAR\r\n"));
        // The feed never echoes request authority into its content.
        assert!(!body.contains("allowed.example"));
        let (status, _, body) = raw_get(&origin, path, "evil.example").await;
        assert_eq!(status, 403, "{path}: {body}");
        assert!(body.contains("host_not_allowed"), "{body}");
    }
    for domain in ["tv", "movies"] {
        let path = format!("/api/v1/{domain}/calendar?start=2026-10-01&end=2026-10-02");
        let (status, _, body) = raw_get(&origin, &path, "allowed.example").await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap()["media_type"],
            domain
        );
        assert_eq!(raw_get(&origin, &path, "evil.example").await.0, 403);
    }
}

/// Candidate selection must keep supported sub-second precision: a row an instant before a window
/// boundary belongs to the earlier window and is never counted as invalid in the next one.
#[tokio::test]
async fn fractional_seconds_never_cross_a_window_boundary() {
    let app = app().await;
    exec(
        &app.db,
        "INSERT INTO series(id,title,path,monitored) VALUES (1,'Edge','/owned/edge',1);
         INSERT INTO seasons(series_id,number) VALUES (1,1);
         INSERT INTO episodes(series_id,season,number,title,air_date_utc) VALUES (1,1,1,'Late','2026-10-31T23:59:59.9995Z');
         INSERT INTO movie_metadata(id,title,year,digital_release) VALUES (1,'Late film',2026,'2026-10-31T23:59:59.9995Z');
         INSERT INTO movies(id,metadata_id,path) VALUES (1,1,'/owned/m/1');",
    )
    .await;
    for domain in ["tv", "movies"] {
        let before = ok(
            &app.base,
            &format!("/api/v1/{domain}/calendar?start=2026-10-31&end=2026-11-01"),
        )
        .await;
        let after = ok(
            &app.base,
            &format!("/api/v1/{domain}/calendar?start=2026-11-01&end=2026-11-02"),
        )
        .await;
        assert_eq!(
            (
                before["total"].as_i64(),
                before["diagnostics"]["invalid_dates"].as_i64(),
                after["total"].as_i64(),
                after["diagnostics"]["invalid_dates"].as_i64()
            ),
            (Some(1), Some(0), Some(0), Some(0)),
            "{domain}: {before} / {after}"
        );
    }
}
