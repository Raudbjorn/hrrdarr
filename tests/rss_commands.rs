use axum::{
    body::Bytes,
    extract::{OriginalUri, State},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
};
use hrrdarr::{commands, db::Database, providers};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};

/// Hang detector scale for waits on the real workers (issue #38). The suite runs scenarios side by
/// side, each slowed several-fold by the others, so a window sized for an idle host expires without
/// anything being wrong. A passing wait returns as soon as its state holds.
fn hang(seconds: u64) -> Duration {
    Duration::from_secs(seconds * 4)
}
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
/// Scenarios that drive the real one-step-per-second workers at once. All 31 run in one process, and
/// side by side they starve each other's workers and HTTP mocks (issue #38); a few at a time keep
/// every scenario near its idle-host speed. The permit lives as long as the scenario's server.
static SCENARIOS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(6);
struct Server(
    tokio::task::JoinHandle<()>,
    Option<tokio::sync::SemaphorePermit<'static>>,
);
impl Server {
    async fn stop(mut self) {
        self.0.abort();
        let _ = (&mut self.0).await;
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}
#[derive(Default)]
struct Remote {
    items: Mutex<Vec<Value>>,
    adds: Mutex<Vec<String>>,
    indexer_requests: Mutex<Vec<String>>,
    mode: AtomicU8,
    v2: AtomicU8,
    date: Mutex<Option<String>>,
    endpoint: Mutex<String>,
    extra_feed: Mutex<String>,
    feed_override: Mutex<Option<(String, String)>>,
    torrent_requests: AtomicU8,
    torrent_barrier_limit: AtomicU8,
    torrent_change_after: AtomicU8,
    submission_barrier_db: Mutex<Option<std::sync::Weak<Database>>>,
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
const TV: &str = "1111111111111111111111111111111111111111";
const MOVIE: &str = "2222222222222222222222222222222222222222";
fn item(hash: &str, category: &str) -> Value {
    json!({"hash":hash,"infohash_v1":hash,"infohash_v2":"","category":category,"name":"safe item","state":"downloading","progress":0.5,"size":100,"amount_left":50,"dlspeed":2,"upspeed":1,"eta":25,"ratio":0.2,"seeding_time":0,"priority":5,"force_start":false,"ratio_limit":-2.0,"seeding_time_limit":-2,"inactive_seeding_time_limit":-1,"seq_dl":false,"f_l_piece_prio":false,"auto_tmm":false,"tags":""})
}
async fn remote(
    State(s): State<Arc<Remote>>,
    method: Method,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let q: std::collections::HashMap<_, _> =
        url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();
    if uri.path() == "/api" || uri.path() == "/" {
        s.indexer_requests.lock().unwrap().push(uri.to_string());
        if s.mode.load(Ordering::SeqCst) == 8 {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                [("retry-after", "120")],
                "PRIVATE_RSS_LOCATOR",
            )
                .into_response();
        }
        if q.get("t").is_some_and(|v| v == "caps") {
            return r#"<caps><limits max="100" default="100"/><searching><search available="yes" supportedParams="q"/><tv-search available="yes" supportedParams="q,tvdbid,season,ep"/><movie-search available="yes" supportedParams="q,tmdbid"/></searching><categories><category id="5000"><subcat id="5030"/></category><category id="2000"><subcat id="2030"/></category></categories></caps>"#.into_response();
        }
        let tv = q.get("cat").is_some_and(|v| v.contains("5030"));
        let (title, hash, cat) = if tv {
            ("Harbor.S01E01.1080p.WEB-DL", TV, 5030)
        } else {
            ("Harbor.2020.1080p.WEB-DL", MOVIE, 2030)
        };
        let override_release = s.feed_override.lock().unwrap().clone();
        let (title, hash) = override_release
            .as_ref()
            .map(|(title, hash)| (title.as_str(), hash.as_str()))
            .unwrap_or((title, hash));
        let hash = match s.mode.load(Ordering::SeqCst) {
            6 => "3333333333333333333333333333333333333333",
            7 => TV,
            _ => hash,
        };
        let date = s
            .date
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| "Mon, 01 Jan 2024 12:00:00 +0000".into());
        let link = format!(
            "{}/torrent?passkey=PRIVATE_RSS_LOCATOR",
            s.endpoint.lock().unwrap()
        );
        let magnet = if matches!(s.mode.load(Ordering::SeqCst), 4 | 9 | 10) {
            String::new()
        } else if s.v2.load(Ordering::SeqCst) == 1 {
            let v2 = if tv { "a".repeat(64) } else { "b".repeat(64) };
            format!(r#"<torznab:attr name="magneturl" value="magnet:?xt=urn:btmh:1220{v2}"/>"#)
        } else {
            format!(r#"<torznab:attr name="magneturl" value="magnet:?xt=urn:btih:{hash}"/>"#)
        };
        let missing = if s.mode.load(Ordering::SeqCst) == 5 {
            format!(
                r#"<item><guid>PRIVATE_RSS_GUID-missing</guid><pubDate>{date}</pubDate><link>{link}</link>{magnet}<torznab:attr name="category" value="{cat}"/><torznab:attr name="size" value="1073741824"/></item>"#
            )
        } else {
            String::new()
        };
        let extra = s.extra_feed.lock().unwrap().clone();
        return format!(r#"<rss xmlns:torznab="http://torznab.com/schemas/2015/feed"><channel><item><title>{title}</title><guid>PRIVATE_RSS_GUID-{hash}</guid><pubDate>{date}</pubDate><link>{link}</link>{magnet}<torznab:attr name="category" value="{cat}"/><torznab:attr name="size" value="1073741824"/></item>{missing}{extra}</channel></rss>"#).into_response();
    }
    if uri.path() == "/torrent" {
        let request = s.torrent_requests.fetch_add(1, Ordering::SeqCst);
        if s.mode.load(Ordering::SeqCst) == 9 {
            return b"invalid torrent".to_vec().into_response();
        }
        if s.mode.load(Ordering::SeqCst) == 10 {
            let name = if request < 2 { "x" } else { "y" };
            return format!("d4:infod6:lengthi1e4:name1:{name}12:piece lengthi16384e6:pieces20:aaaaaaaaaaaaaaaaaaaaee").into_response();
        }
        let limit = s.torrent_barrier_limit.load(Ordering::SeqCst);
        if limit == 0 || request < limit {
            s.started.notify_one();
            s.release.notified().await;
        }
        let change_after = s.torrent_change_after.load(Ordering::SeqCst);
        let name = if change_after > 0 && request >= change_after {
            "y"
        } else {
            "x"
        };
        return format!("d4:infod6:lengthi1e4:name1:{name}12:piece lengthi16384e6:pieces20:aaaaaaaaaaaaaaaaaaaaee").into_response();
    }
    if uri.path().ends_with("auth/login") {
        return if s.mode.load(Ordering::SeqCst) == 13 {
            "Fails."
        } else {
            "Ok."
        }
        .into_response();
    }
    if uri.path().ends_with("webapiVersion") {
        if s.mode.load(Ordering::SeqCst) == 14 {
            let db = s
                .submission_barrier_db
                .lock()
                .unwrap()
                .as_ref()
                .and_then(std::sync::Weak::upgrade);
            // Startup health uses this endpoint too. Only pause the real
            // submission after its durable intent, never an earlier probe.
            let submitting = if let Some(db) = db {
                db.connect()
                    .await
                    .unwrap()
                    .query(
                        "SELECT EXISTS(SELECT 1 FROM rss_candidates WHERE status='submitting')",
                        (),
                    )
                    .await
                    .unwrap()
                    .next()
                    .await
                    .unwrap()
                    .unwrap()
                    .get::<i64>(0)
                    .unwrap()
                    == 1
            } else {
                false
            };
            if submitting {
                s.started.notify_one();
                s.release.notified().await;
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
        }
        if s.mode.load(Ordering::SeqCst) == 13 {
            return StatusCode::FORBIDDEN.into_response();
        }
        if s.mode.load(Ordering::SeqCst) == 12 {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        return "2.8.4".into_response();
    }
    if uri.path().ends_with("preferences") {
        return axum::Json(json!({"queueing_enabled":true,"dht":true,"save_path":"/remote","max_ratio_enabled":false,"max_ratio":-1,"max_seeding_time_enabled":false,"max_seeding_time":-1,"max_ratio_act":0})).into_response();
    }
    if uri.path().ends_with("categories") {
        return axum::Json(
            json!({"tv":{"savePath":"/remote/tv"},"movies":{"savePath":"/remote/movies"}}),
        )
        .into_response();
    }
    if uri.path().ends_with("torrents/info") {
        if s.mode.load(Ordering::SeqCst) == 11 && !s.adds.lock().unwrap().is_empty() {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        let items = s
            .items
            .lock()
            .unwrap()
            .iter()
            .filter(|v| {
                q.get("category")
                    .is_none_or(|cat| v["category"] == cat.as_str())
                    && q.get("hashes").is_none_or(|hashes| {
                        hashes
                            .split('|')
                            .any(|hash| v["hash"] == hash || v["infohash_v2"] == hash)
                    })
            })
            .cloned()
            .collect::<Vec<_>>();
        return axum::Json(items).into_response();
    }
    if uri.path().ends_with("torrents/add") {
        assert_eq!(method, Method::POST);
        let multipart = headers
            .get("content-type")
            .and_then(|header| header.to_str().ok())
            .and_then(|header| header.split("boundary=").nth(1));
        // A successful file-based preparation posts multipart bytes; earlier mode4
        // coverage fenced before POST. Preserve the existing magnet form path too.
        let pairs: std::collections::HashMap<String, String> = if let Some(boundary) = multipart {
            let body = std::str::from_utf8(&body).unwrap();
            assert!(body.contains("name=\"torrents\"; filename="));
            assert!(body.contains("d4:infod6:lengthi1e4:name1:x12:piece lengthi16384e6:pieces20:aaaaaaaaaaaaaaaaaaaaee"));
            body.split(&format!("--{boundary}"))
                .filter_map(|part| {
                    let (head, value) = part.split_once("\r\n\r\n")?;
                    if head.contains("filename=") {
                        return None;
                    }
                    let name = head.split("name=\"").nth(1)?.split('"').next()?;
                    Some((name.to_owned(), value.trim_end_matches("\r\n").to_owned()))
                })
                .collect()
        } else {
            url::form_urlencoded::parse(&body).into_owned().collect()
        };
        let category = pairs.get("category").unwrap();
        assert!(matches!(category.as_str(), "tv" | "movies"));
        // SHA1 of the exact bencoded info dictionary served by /torrent. Remote
        // presence must describe the uploaded bytes, not the magnet-only fixture ID.
        let magnet_hash = pairs
            .get("urls")
            .and_then(|v| url::Url::parse(v).ok())
            .and_then(|u| {
                u.query_pairs()
                    .find(|(k, v)| k == "xt" && v.starts_with("urn:btih:"))
                    .map(|(_, v)| v.trim_start_matches("urn:btih:").to_owned())
            });
        let hash = if multipart.is_some() {
            "fd1ecef9f83ef3d11c63557b271320a134a6add7"
        } else if let Some(hash) = magnet_hash.as_deref() {
            hash
        } else if category == "tv" {
            TV
        } else {
            MOVIE
        };
        s.adds.lock().unwrap().push(hash.into());
        let mode = s.mode.load(Ordering::SeqCst);
        if mode != 2 {
            let mut added = item(hash, category);
            if s.v2.load(Ordering::SeqCst) == 1 {
                let character = if category == "tv" { "a" } else { "b" };
                added["hash"] = json!(character.repeat(40));
                added["infohash_v1"] = json!("");
                added["infohash_v2"] = json!(character.repeat(64));
            }
            s.items.lock().unwrap().push(added);
        }
        if matches!(mode, 1 | 2) {
            s.started.notify_one();
            s.release.notified().await;
        }
        return "Ok.".into_response();
    }
    (StatusCode::NOT_FOUND, "unexpected owned mock request").into_response()
}
async fn serve(app: axum::Router) -> (String, Server) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    (
        base,
        Server(
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() }),
            None,
        ),
    )
}
async fn request(base: &str, method: &str, path: &str, body: Value) -> (u16, Value) {
    let response = reqwest::Client::builder()
        .no_proxy()
        .timeout(hang(6))
        .build()
        .unwrap()
        .request(method.parse().unwrap(), format!("{base}{path}"))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!(
                "owned HTTP request {method} {path} failed: {}",
                error.without_url()
            )
        });
    let status = response.status().as_u16();
    let text = response.text().await.unwrap();
    assert!(
        !text.contains("PRIVATE_RSS_GUID") && !text.contains("PRIVATE_RSS_LOCATOR"),
        "private feed facts cannot enter API responses"
    );
    (
        status,
        if status == 204 {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or_else(|_| panic!("{status} {text}"))
        },
    )
}
async fn app(db: Arc<Database>) -> (String, Server, providers::RefreshClient) {
    let permit = SCENARIOS.acquire().await.unwrap();
    let key = Arc::new(providers::CredentialKey::from_hex(&"11".repeat(32)).unwrap());
    let (router, client) = providers::router_with_refresh(db.clone(), Some(key));
    let (base, mut server) = serve(
        router
            .merge(commands::router(db.clone()))
            .merge(hrrdarr::search::router(db.clone(), client.clone()))
            .merge(hrrdarr::delay_profiles::router(db.clone()))
            .merge(hrrdarr::release_profiles::router(db)),
    )
    .await;
    server.1 = Some(permit);
    (base, server, client)
}
async fn until(base: &str, id: &Value) -> Value {
    tokio::time::timeout(hang(18), async {
        loop {
            let (code, v) = request(
                base,
                "GET",
                &format!("/api/v1/rss/commands/{}", id.as_str().unwrap()),
                Value::Null,
            )
            .await;
            assert_eq!(code, 200, "{v}");
            if matches!(
                v["status"].as_str(),
                Some("succeeded" | "failed" | "cancelled")
            ) {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .expect("RSS settlement deadline")
}
async fn seed(db: &Database) {
    // Any (-1) keeps this fixture language-unrestricted; Original (-2) requires audio matching.
    db.connect().await.unwrap().execute_batch("INSERT INTO series(id,tvdb_id,title,path)VALUES(1,101,'Harbor','/owned-fictional-tv'); INSERT INTO seasons(series_id,number)VALUES(1,1); INSERT INTO episodes(id,series_id,season,number,title,runtime,air_date_utc)VALUES(1,1,1,1,'Pilot',45,'2020-01-01 00:00:00'); INSERT INTO movie_metadata(id,tmdb_id,title,year,runtime,digital_release)VALUES(1,201,'Harbor',2020,100,'2020-01-01 00:00:00'); INSERT INTO movies(id,metadata_id,path)VALUES(1,1,'/owned-fictional-movie'); INSERT INTO quality_profiles VALUES(1,'tv','HD'),(2,'movies','HD'); INSERT INTO quality_profile_items(profile_id,media_type,quality_id,position,allowed)VALUES(1,'tv',3,0,1),(2,'movies',3,0,1); INSERT INTO quality_profile_policies(profile_id,media_type,upgrade_allowed,cutoff_quality_id,min_format_score,cutoff_format_score,min_upgrade_format_score,language_id)VALUES(1,'tv',1,3,0,0,1,NULL),(2,'movies',1,3,0,0,1,-1); INSERT INTO library_settings(media_type,series_id,quality_profile_id,series_type,use_scene_numbering)VALUES('tv',1,1,'standard',0); INSERT INTO library_settings(media_type,movie_id,quality_profile_id,minimum_availability)VALUES('movies',1,2,'released'); INSERT INTO release_delay_policies VALUES('tv',0,0,0),('movies',0,0,0);").await.unwrap();
}
async fn providers(base: &str, remote: &str) -> (Value, Value) {
    let mut values = Vec::new();
    for settings in [
        json!({"implementation":"torznab","endpoint":remote,"tv":{"categories":[5030],"anime_categories":[]},"movies":{"categories":[2030]}}),
        json!({"implementation":"qbittorrent","endpoint":remote,"tv":{"category":"tv","imported_category":null,"recent_priority":0,"older_priority":0},"movies":{"category":"movies","imported_category":null,"recent_priority":0,"older_priority":0}}),
    ] {
        let (code, value) = request(
            base,
            "POST",
            "/api/v1/providers",
            json!({"name":"owned","enabled":true,"priority":1,"settings":settings}),
        )
        .await;
        assert_eq!(code, 201, "{value}");
        values.push(value)
    }
    (values.remove(0), values.remove(0))
}
async fn bind_client(base: &str, indexer: &Value, media: &str, client: &Value) -> Value {
    let mut settings = indexer["settings"].clone();
    settings[media]["download_client_id"] = client["id"].clone();
    let (code, updated) = request(
        base, "PUT", &format!("/api/v1/providers/{}", indexer["id"].as_str().unwrap()),
        json!({"revision":indexer["revision"],"name":"bound indexer","enabled":true,"priority":1,"settings":settings}),
    ).await;
    assert_eq!(code, 200, "{updated}");
    updated
}
fn target(indexer: &Value, client: &Value, media: &str) -> Value {
    json!({"media_type":media,"indexer_id":indexer["id"],"indexer_revision":indexer["revision"],"client_id":client["id"],"client_revision":client["revision"]})
}
async fn enqueue(base: &str, target: Value) -> Value {
    let (code, v) = request(
        base,
        "POST",
        "/api/v1/rss/commands",
        json!({"target":target,"priority":"normal"}),
    )
    .await;
    assert_eq!(code, 202, "{v}");
    v
}
#[tokio::test]
async fn rss_grabs_both_domains_and_replay_does_not_duplicate_submissions() {
    let scratch =
        Scratch(std::env::temp_dir().join(format!("hrrdarr-rss-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&scratch.0).unwrap();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    seed(&db).await;
    let remote_state = Arc::new(Remote::default());
    let (remote_base, _remote_server) = serve(
        axum::Router::new()
            .fallback(remote)
            .with_state(remote_state.clone()),
    )
    .await;
    *remote_state.endpoint.lock().unwrap() = remote_base.clone();
    let (base, _server, client) = app(db.clone()).await;
    let (indexer, download) = providers(&base, &remote_base).await;
    let indexer = bind_client(&base, &indexer, "tv", &download).await;
    let indexer = bind_client(&base, &indexer, "movies", &download).await;
    let c = db.connect().await.unwrap();
    let timestamp = chrono::Utc::now().timestamp();
    // Seed persisted outage history; the new sends and their recovery below use real loopback HTTP.
    for media in ["tv", "movies"] {
        c.execute("INSERT INTO download_client_status(provider_id,media_type,config_revision,escalation_level,initial_failure_at,last_failure_at,disabled_until,next_health_at) VALUES(?,?,?,3,?,?,?,?)",libsql::params![download["id"].as_str().unwrap(),media,download["revision"].as_i64().unwrap(),timestamp-600,timestamp-10,timestamp+900,timestamp+900]).await.unwrap();
    }
    // Keep unrelated successful polling from decrementing the same history during send assertions.
    assert_eq!(
        c.execute(
            "UPDATE download_refresh_schedules SET next_run_at=? WHERE provider_id=?",
            libsql::params![timestamp + 3600, download["id"].as_str().unwrap()]
        )
        .await
        .unwrap(),
        2
    );
    async fn backoff(c: &libsql::Connection, media: &str) -> (i64, Option<i64>) {
        let row = c.query("SELECT escalation_level,disabled_until FROM download_client_status WHERE media_type=?",[media]).await.unwrap().next().await.unwrap().unwrap();
        (row.get(0).unwrap(), row.get(1).unwrap())
    }
    let runtime = commands::start(db.clone(), client).await.unwrap();
    for media in ["tv", "movies"] {
        let command = enqueue(&base, target(&indexer, &download, media)).await;
        let done = until(&base, &command["id"]).await;
        assert_eq!(done["status"], "succeeded", "{done}");
        assert_eq!(done["observed"], 1, "{done}");
        assert_eq!(
            backoff(&c, media).await,
            (2, None),
            "a fresh accepted send recovers only its scope"
        );
        if media == "tv" {
            assert_eq!(
                backoff(&c, "movies").await,
                (3, Some(timestamp + 900)),
                "TV submission must leave movie backoff intact"
            );
        } else {
            assert_eq!(backoff(&c, "tv").await, (2, None));
        }
        let (_, receipts) = request(
            &base,
            "GET",
            &format!(
                "/api/v1/rss/candidates?command_id={}",
                command["id"].as_str().unwrap()
            ),
            Value::Null,
        )
        .await;
        assert_eq!(receipts["items"][0]["target"]["media_type"], media);
        let idfield = if media == "tv" {
            "series_id"
        } else {
            "movie_id"
        };
        assert_eq!(
            receipts["items"][0]["target"][idfield], 1,
            "equal numeric IDs retain their domain"
        );
        let replay = enqueue(&base, target(&indexer, &download, media)).await;
        assert_eq!(until(&base, &replay["id"]).await["status"], "succeeded");
        assert_eq!(
            backoff(&c, media).await,
            (2, None),
            "replay must not publish another fresh-send recovery"
        );
    }
    assert_eq!(remote_state.adds.lock().unwrap().as_slice(), [TV, MOVIE]);
    runtime.shutdown().await;
}
async fn receipt_until(base: &str, status: &str) -> Value {
    let outcome = tokio::time::timeout(hang(12), async {
        loop {
            let (code, v) = request(base, "GET", "/api/v1/rss/candidates", Value::Null).await;
            assert_eq!(code, 200, "{v}");
            if let Some(item) = v["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["status"] == status)
            {
                return item.clone();
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await;
    match outcome {
        Ok(receipt) => receipt,
        Err(error) => {
            let (_, receipts) = request(base, "GET", "/api/v1/rss/candidates", Value::Null).await;
            panic!("receipt deadline: {error}; public receipts: {receipts}")
        }
    }
}
#[tokio::test]
async fn existing_hash_never_becomes_a_trusted_association() {
    for default_selection in [false, true] {
        let scratch = Scratch(
            std::env::temp_dir().join(format!("hrrdarr-rss-existing-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir(&scratch.0).unwrap();
        let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
        seed(&db).await;
        let state = Arc::new(Remote::default());
        state
            .items
            .lock()
            .unwrap()
            .extend([item(TV, "tv"), item(MOVIE, "movies")]);
        let (remote_base, _remote) = serve(
            axum::Router::new()
                .fallback(remote)
                .with_state(state.clone()),
        )
        .await;
        *state.endpoint.lock().unwrap() = remote_base.clone();
        let (base, _server, client) = app(db.clone()).await;
        let (indexer, download) = providers(&base, &remote_base).await;
        let alternative = Arc::new(Remote::default());
        let (alternative_base, _alternative_server) = serve(
            axum::Router::new()
                .fallback(remote)
                .with_state(alternative.clone()),
        )
        .await;
        if default_selection {
            let (_, provider) = providers(&base, &alternative_base).await;
            let (code, provider) = request(&base,"PUT",&format!("/api/v1/providers/{}",provider["id"].as_str().unwrap()),json!({"revision":provider["revision"],"name":"alternate","enabled":true,"priority":50,"settings":provider["settings"]})).await;
            assert_eq!(code, 200, "{provider}");
            db.connect()
                .await
                .unwrap()
                .execute(
                    "UPDATE download_refresh_schedules SET next_run_at=unixepoch()+3600",
                    (),
                )
                .await
                .unwrap();
        }
        let runtime = commands::start(db.clone(), client).await.unwrap();
        for media in ["tv", "movies"] {
            let request_target = if default_selection {
                selection_target(&indexer, media, json!({"kind":"default"}))
            } else {
                target(&indexer, &download, media)
            };
            let command = enqueue(&base, request_target).await;
            assert_eq!(until(&base, &command["id"]).await["status"], "failed");
            let (_, receipts) = request(
                &base,
                "GET",
                &format!(
                    "/api/v1/rss/candidates?command_id={}",
                    command["id"].as_str().unwrap()
                ),
                Value::Null,
            )
            .await;
            assert_eq!(receipts["items"][0]["error_code"], "preexisting_download");
            assert_eq!(receipts["items"][0]["status"], "needs_attention");
            assert_eq!(
                receipts["items"][0]["attempts"], 0,
                "AlreadyPresent does not increment reconciliation attempts"
            );
            if default_selection {
                assert_eq!(receipts["items"][0]["source"]["client_id"], download["id"]);
                assert_eq!(
                    receipts["items"][0]["client_attempts"]
                        .as_array()
                        .unwrap()
                        .len(),
                    1
                );
                assert_eq!(
                    receipts["items"][0]["client_attempts"][0]["state"],
                    "unknown"
                );
            }
        }
        assert!(
            state.adds.lock().unwrap().is_empty(),
            "remote presence must not authorize a new POST or trusted receipt"
        );
        assert_eq!(
            db.connect()
                .await
                .unwrap()
                .query(
                    "SELECT count(*) FROM rss_candidates WHERE status='observed'",
                    ()
                )
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap()
                .get::<i64>(0)
                .unwrap(),
            0
        );
        assert!(
            alternative.adds.lock().unwrap().is_empty(),
            "preexisting download never authorizes an alternate POST"
        );
        runtime.shutdown().await;
    }
}
#[tokio::test]
async fn interrupted_submission_reopens_with_typed_identity_and_only_reconciles() {
    for (media, mode, default_selection) in [
        ("tv", 1, false),
        ("movies", 1, false),
        ("tv", 2, false),
        ("tv", 1, true),
        ("movies", 1, true),
    ] {
        let scratch = Scratch(
            std::env::temp_dir().join(format!("hrrdarr-rss-reopen-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir(&scratch.0).unwrap();
        let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
        seed(&db).await;
        let state = Arc::new(Remote::default());
        state.mode.store(mode, Ordering::SeqCst);
        let (remote_base, _remote) = serve(
            axum::Router::new()
                .fallback(remote)
                .with_state(state.clone()),
        )
        .await;
        *state.endpoint.lock().unwrap() = remote_base.clone();
        let (base, server, client) = app(db.clone()).await;
        let (indexer, download) = providers(&base, &remote_base).await;
        let alternative_state = Arc::new(Remote::default());
        let (alternative_base, _alternative_server) = serve(
            axum::Router::new()
                .fallback(remote)
                .with_state(alternative_state.clone()),
        )
        .await;
        let (_, alternative) = providers(&base, &alternative_base).await;
        let (code, alternative) = request(&base, "PUT", &format!("/api/v1/providers/{}",alternative["id"].as_str().unwrap()), json!({"revision":alternative["revision"],"name":"alternative","enabled":true,"priority":50,"settings":alternative["settings"]})).await;
        assert_eq!(code, 200, "{alternative}");
        let requested = if default_selection {
            selection_target(&indexer, media, json!({"kind":"default"}))
        } else {
            target(&indexer, &download, media)
        };
        let command = enqueue(&base, requested).await;
        let runtime = commands::start(db.clone(), client).await.unwrap();
        tokio::time::timeout(hang(12), state.started.notified())
            .await
            .unwrap();
        let (_, before) = request(&base, "GET", "/api/v1/rss/candidates", Value::Null).await;
        let id = before["items"][0]["id"].clone();
        assert_eq!(before["items"][0]["status"], "submitting");
        let (_, other) = providers(&base, &remote_base).await;
        bind_client(&base, &indexer, media, &other).await;
        runtime.shutdown().await;
        server.stop().await;
        drop(db);
        state.release.notify_one();
        let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
        let (base, _server, client) = app(db.clone()).await;
        let runtime = commands::start(db.clone(), client).await.unwrap();
        let done = until(&base, &command["id"]).await;
        assert_eq!(
            done["attempts"], 2,
            "same durable command claims once after restart"
        );
        assert_eq!(done["status"], "failed");
        let receipt = if mode == 1 {
            receipt_until(&base, "needs_attention").await
        } else {
            tokio::time::timeout(hang(10), async {
                loop {
                    let v = receipt_until(&base, "reconciling").await;
                    if v["attempts"] == 2 {
                        return v;
                    }
                    tokio::time::sleep(Duration::from_millis(40)).await;
                }
            })
            .await
            .unwrap()
        };
        assert_eq!(receipt["id"], id);
        assert_eq!(receipt["target"]["media_type"], media);
        assert_eq!(receipt["source"]["client_id"], download["id"]);
        if default_selection {
            assert_eq!(receipt["client_attempts"].as_array().unwrap().len(), 1);
            assert_eq!(receipt["client_attempts"][0]["state"], "unknown");
        }
        assert!(
            alternative_state.adds.lock().unwrap().is_empty(),
            "an uncertain add must retain its original client after restart"
        );
        if mode == 1 {
            assert_eq!(receipt["error_code"], "presence_unconfirmed");
        } else {
            let final_receipt = tokio::time::timeout(hang(75), async {
                loop {
                    let (_, v) = request(&base, "GET", "/api/v1/rss/candidates", Value::Null).await;
                    let v = &v["items"][0];
                    if v["status"] == "needs_attention" {
                        return v.clone();
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(final_receipt["attempts"], 3);
            assert_eq!(
                final_receipt["error_code"], "not_observed",
                "bounded absence never permits resubmission"
            );
        }
        assert_eq!(
            state.adds.lock().unwrap().len(),
            1,
            "both observed and NotObserved reconciliation forbid a second POST"
        );
        runtime.shutdown().await;
    }
}
#[tokio::test]
async fn local_policy_rechecked_after_preparation_and_postsubmit_failure_recovers() {
    for (mode, media, delay_change, default_selection) in [
        (4, "tv", false, false),
        (1, "movies", false, false),
        (4, "tv", true, false),
        (4, "movies", true, false),
        (1, "tv", false, true),
    ] {
        let scratch = Scratch(
            std::env::temp_dir().join(format!("hrrdarr-rss-barrier-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir(&scratch.0).unwrap();
        let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
        seed(&db).await;
        let state = Arc::new(Remote::default());
        state.mode.store(mode, Ordering::SeqCst);
        if delay_change {
            *state.date.lock().unwrap() = Some(chrono::Utc::now().to_rfc2822());
        }
        let (remote_base, _remote) = serve(
            axum::Router::new()
                .fallback(remote)
                .with_state(state.clone()),
        )
        .await;
        *state.endpoint.lock().unwrap() = remote_base.clone();
        let (base, _server, client) = app(db.clone()).await;
        let (indexer, download) = providers(&base, &remote_base).await;
        if delay_change {
            save_delay(&base, media, 0).await;
        }
        let alternative = Arc::new(Remote::default());
        let (alternative_base, _alternative_server) = serve(
            axum::Router::new()
                .fallback(remote)
                .with_state(alternative.clone()),
        )
        .await;
        if default_selection {
            let (_, provider) = providers(&base, &alternative_base).await;
            let (code, updated) = request(&base,"PUT",&format!("/api/v1/providers/{}",provider["id"].as_str().unwrap()),json!({"revision":provider["revision"],"name":"healthy alternative","enabled":true,"priority":50,"settings":provider["settings"]})).await;
            assert_eq!(code, 200, "{updated}");
        }
        let request_target = if default_selection {
            selection_target(&indexer, media, json!({"kind":"default"}))
        } else {
            target(&indexer, &download, media)
        };
        let command = enqueue(&base, request_target).await;
        let runtime = commands::start(db.clone(), client.clone()).await.unwrap();
        tokio::time::timeout(hang(12), state.started.notified())
            .await
            .unwrap();
        let c = db.connect().await.unwrap();
        let pending_before = if delay_change {
            Some(
                c.query("SELECT id,hex(private_payload) FROM rss_candidates", ())
                    .await
                    .unwrap()
                    .next()
                    .await
                    .unwrap()
                    .unwrap(),
            )
        } else {
            None
        };
        let pending_before = pending_before
            .map(|row| (row.get::<String>(0).unwrap(), row.get::<String>(1).unwrap()));
        if delay_change {
            save_delay(&base, media, 60).await;
        } else if mode == 4 {
            db.connect()
                .await
                .unwrap()
                .execute("UPDATE series SET monitored=0 WHERE id=1", ())
                .await
                .unwrap();
        } else {
            db.connect().await.unwrap().execute_batch("CREATE TRIGGER reject_observed BEFORE UPDATE ON rss_candidates WHEN NEW.status='observed' BEGIN SELECT RAISE(ABORT,'owned postsubmit settlement failure'); END;").await.unwrap();
        }
        state.release.notify_one();
        let done = until(&base, &command["id"]).await;
        if delay_change {
            assert_eq!(done["status"], "succeeded", "{done}");
            assert_eq!(done["pending"], 1, "{done}");
            assert_eq!(done["rejected"], 0);
            assert!(
                state.adds.lock().unwrap().is_empty(),
                "A fresh delay parks the receipt before any POST"
            );
            let (id, payload) = pending_before.unwrap();
            let row=c.query("SELECT status,hex(private_payload),not_before,submission_identity_json,comparison_facts_json FROM rss_candidates WHERE id=?",[id.as_str()]).await.unwrap().next().await.unwrap().unwrap();
            assert_eq!(row.get::<String>(0).unwrap(), "pending");
            assert_eq!(row.get::<String>(1).unwrap(), payload);
            assert!(row.get::<i64>(2).unwrap() > chrono::Utc::now().timestamp() + 3500);
            assert!(row.get::<Option<String>>(3).unwrap().is_none());
            assert!(row.get::<Option<String>>(4).unwrap().is_none());
            drop(row);
            assert_eq!(
                c.query("SELECT count(*) FROM rss_hash_claims", ())
                    .await
                    .unwrap()
                    .next()
                    .await
                    .unwrap()
                    .unwrap()
                    .get::<i64>(0)
                    .unwrap(),
                0
            );
            runtime.shutdown().await;
            // A real policy PUT after worker restart wakes this same immutable receipt.
            let runtime = commands::start(db.clone(), client).await.unwrap();
            let barriers = state.clone();
            let responder = tokio::spawn(async move {
                // Pending preparation and prepared revalidation each read /torrent.
                for _ in 0..2 {
                    tokio::time::timeout(hang(12), barriers.started.notified())
                        .await
                        .unwrap();
                    barriers.release.notify_one();
                }
            });
            save_delay(&base, media, 0).await;
            let observed = receipt_until(&base, "observed").await;
            responder.await.unwrap();
            assert_eq!(observed["id"], id);
            assert_eq!(state.adds.lock().unwrap().len(), 1);
            runtime.shutdown().await;
            continue;
        }
        if mode == 4 {
            assert_eq!(done["status"], "succeeded");
            assert_eq!(done["rejected"], 1);
            assert!(
                state.adds.lock().unwrap().is_empty(),
                "policy change while read-only preparation blocked must fence POST"
            );
        } else {
            assert_eq!(done["status"], "failed");
            let receipt = receipt_until(&base, "needs_attention").await;
            assert_eq!(receipt["error_code"], "presence_unconfirmed");
            if default_selection {
                assert_eq!(receipt["source"]["client_id"], download["id"]);
                assert_eq!(receipt["client_attempts"].as_array().unwrap().len(), 1);
                assert_eq!(receipt["client_attempts"][0]["state"], "unknown");
                assert!(
                    alternative.adds.lock().unwrap().is_empty(),
                    "a failed observed-receipt transaction never permits an alternate POST"
                );
            }
            assert_eq!(
                state.adds.lock().unwrap().len(),
                1,
                "postsubmit DB rollback recovers without process restart or second POST"
            );
        }
        runtime.shutdown().await;
    }
}
async fn save_delay(base: &str, media: &str, minutes: u32) {
    let path = format!("/api/v1/{media}/delay-profiles");
    let (status, catalog) = request(base, "GET", &path, Value::Null).await;
    assert_eq!(status, 200, "{catalog}");
    let global = catalog["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|profile| profile["is_global"] == true)
        .unwrap();
    let (status,result)=request(base,"PUT",&format!("{path}/{}",global["id"]),json!({"revision":catalog["revision"],"profile":{"tag_ids":[],"settings":{"torrent_delay_minutes":minutes,"usenet_delay_minutes":minutes,"enable_torrent":true,"enable_usenet":true,"preferred_protocol":"torrent","bypass_if_highest_quality":false,"bypass_if_above_custom_format_score":false,"minimum_custom_format_score":0}}})).await;
    assert_eq!(status, 200, "{result}");
}

#[tokio::test]
async fn rejected_availability_is_reevaluated_and_schedules_are_revision_fenced() {
    let scratch =
        Scratch(std::env::temp_dir().join(format!("hrrdarr-rss-policy-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&scratch.0).unwrap();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    seed(&db).await;
    db.connect()
        .await
        .unwrap()
        .execute(
            "UPDATE movie_metadata SET digital_release='2099-01-01 00:00:00'",
            (),
        )
        .await
        .unwrap();
    let state = Arc::new(Remote::default());
    let (remote_base, _remote) = serve(
        axum::Router::new()
            .fallback(remote)
            .with_state(state.clone()),
    )
    .await;
    *state.endpoint.lock().unwrap() = remote_base.clone();
    let (base, _server, client) = app(db.clone()).await;
    let (indexer, download) = providers(&base, &remote_base).await;
    let queued = enqueue(&base, target(&indexer, &download, "tv")).await;
    let (code, cancelled) = request(
        &base,
        "POST",
        &format!(
            "/api/v1/rss/commands/{}/cancel",
            queued["id"].as_str().unwrap()
        ),
        Value::Null,
    )
    .await;
    assert_eq!(code, 200);
    assert_eq!(cancelled["status"], "cancelled");
    let runtime = commands::start(db.clone(), client).await.unwrap();
    let command = enqueue(&base, target(&indexer, &download, "movies")).await;
    assert_eq!(until(&base, &command["id"]).await["rejected"], 1);
    assert!(state.adds.lock().unwrap().is_empty());
    db.connect()
        .await
        .unwrap()
        .execute(
            "UPDATE movie_metadata SET digital_release='2020-01-01 00:00:00'",
            (),
        )
        .await
        .unwrap();
    let command = enqueue(&base, target(&indexer, &download, "movies")).await;
    assert_eq!(
        until(&base, &command["id"]).await["observed"],
        1,
        "same release must be reconsidered after availability changes"
    );
    let body =
        json!({"target":target(&indexer,&download,"tv"),"interval_seconds":60,"enabled":true});
    let (code, schedule) = request(&base, "POST", "/api/v1/rss/schedules", body.clone()).await;
    assert_eq!(code, 200, "{schedule}");
    assert_eq!(
        request(&base, "POST", "/api/v1/rss/schedules", body)
            .await
            .0,
        409,
        "updating an existing schedule needs its revision"
    );
    db.connect()
        .await
        .unwrap()
        .execute("UPDATE rss_schedules SET next_run_at=0", ())
        .await
        .unwrap();
    let _ = receipt_until(&base, "observed").await;
    tokio::time::timeout(hang(12), async {
        loop {
            if state.adds.lock().unwrap().len() == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    let (code, _) = request(
        &base,
        "DELETE",
        &format!(
            "/api/v1/rss/schedules/{}?revision=999",
            schedule["id"].as_str().unwrap()
        ),
        Value::Null,
    )
    .await;
    assert_eq!(code, 409);
    assert_eq!(
        request(
            &base,
            "DELETE",
            &format!(
                "/api/v1/rss/schedules/{}?revision={}",
                schedule["id"].as_str().unwrap(),
                schedule["revision"]
            ),
            Value::Null
        )
        .await
        .0,
        204
    );
    runtime.shutdown().await;
}
#[tokio::test]
async fn pure_v2_remote_handles_keep_both_queue_associations() {
    let scratch =
        Scratch(std::env::temp_dir().join(format!("hrrdarr-rss-v2-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&scratch.0).unwrap();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    seed(&db).await;
    let state = Arc::new(Remote::default());
    state.v2.store(1, Ordering::SeqCst);
    let (remote_base, _remote) = serve(
        axum::Router::new()
            .fallback(remote)
            .with_state(state.clone()),
    )
    .await;
    *state.endpoint.lock().unwrap() = remote_base.clone();
    let (base, _server, client) = app(db.clone()).await;
    let (indexer, download) = providers(&base, &remote_base).await;
    let runtime = commands::start(db.clone(), client).await.unwrap();
    for media in ["tv", "movies"] {
        let command = enqueue(&base, target(&indexer, &download, media)).await;
        let done = until(&base, &command["id"]).await;
        assert_eq!(done["observed"], 1, "{done}");
        let(code,refresh)=request(&base,"POST","/api/v1/commands",json!({"name":"refresh_downloads","target":{"media_type":media,"provider_id":download["id"]},"provider_revision":download["revision"],"priority":"normal"})).await;
        assert_eq!(code, 202, "{refresh}");
        tokio::time::timeout(hang(10), async {
            loop {
                let (_, v) = request(
                    &base,
                    "GET",
                    &format!("/api/v1/commands/{}", refresh["id"].as_str().unwrap()),
                    Value::Null,
                )
                .await;
                if v["status"] == "succeeded" {
                    break;
                }
                assert_ne!(v["status"], "failed", "{v}");
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
        })
        .await
        .unwrap();
        let (code, queue) = request(
            &base,
            "GET",
            &format!(
                "/api/v1/queue?provider_id={}&media_type={media}",
                download["id"].as_str().unwrap()
            ),
            Value::Null,
        )
        .await;
        assert_eq!(code, 200, "{queue}");
        assert_eq!(
            queue["items"][0]["association"],
            json!({"media_type":if media=="tv"{"episode"}else{"movie"},"id":1}),
            "the API handle differs from prepared v2 hash but retains its proven typed target"
        );
        assert_eq!(
            queue["items"][0]["download"]["hash"]
                .as_str()
                .unwrap()
                .len(),
            40
        );
    }
    assert_eq!(state.adds.lock().unwrap().len(), 2);
    runtime.shutdown().await;
}
#[tokio::test]
async fn delayed_payloads_are_private_and_missing_key_rejects_without_starvation() {
    let scratch =
        Scratch(std::env::temp_dir().join(format!("hrrdarr-rss-key-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&scratch.0).unwrap();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    seed(&db).await;
    db.connect()
        .await
        .unwrap()
        .execute(
            "UPDATE release_delay_policies SET torrent_delay_minutes=60",
            (),
        )
        .await
        .unwrap();
    let state = Arc::new(Remote::default());
    *state.date.lock().unwrap() = Some(chrono::Utc::now().to_rfc2822());
    let (remote_base, _remote) = serve(
        axum::Router::new()
            .fallback(remote)
            .with_state(state.clone()),
    )
    .await;
    *state.endpoint.lock().unwrap() = remote_base.clone();
    let (base, server, client) = app(db.clone()).await;
    let (indexer, download) = providers(&base, &remote_base).await;
    let runtime = commands::start(db.clone(), client).await.unwrap();
    for media in ["tv", "movies"] {
        let command = enqueue(&base, target(&indexer, &download, media)).await;
        let done = until(&base, &command["id"]).await;
        assert_eq!(done["pending"], 1, "{done}");
        assert_eq!(done["status"], "succeeded");
    }
    let c = db.connect().await.unwrap();
    let mut rows = c
        .query("SELECT private_payload FROM rss_candidates", ())
        .await
        .unwrap();
    while let Some(row) = rows.next().await.unwrap() {
        let bytes: Vec<u8> = row.get(0).unwrap();
        assert!(
            !bytes.windows(7).any(|w| w == b"PRIVATE"),
            "only AEAD ciphertext is durable"
        );
    }
    drop(rows);
    drop(c);
    runtime.shutdown().await;
    server.stop().await;
    drop(db);
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    db.connect()
        .await
        .unwrap()
        .execute(
            "UPDATE rss_candidates SET not_before=0 WHERE status='pending'",
            (),
        )
        .await
        .unwrap();
    let (router, client) = providers::router_with_refresh(db.clone(), None);
    let (base, _server) = serve(router.merge(commands::router(db.clone()))).await;
    db.connect().await.unwrap().execute_batch("CREATE TABLE owned_order(seq INTEGER PRIMARY KEY,kind TEXT); CREATE TRIGGER owned_candidate AFTER UPDATE ON rss_candidates WHEN NEW.status='rejected' BEGIN INSERT INTO owned_order(kind)VALUES('candidate'); END; CREATE TRIGGER owned_command AFTER UPDATE ON blocklist_clear_commands WHEN NEW.status='running' BEGIN INSERT INTO owned_order(kind)VALUES('command'); END;").await.unwrap();
    let candidate_created_at = {
        let c = db.connect().await.unwrap();
        let row = c
            .query(
                "SELECT count(*),max(created_at) FROM rss_candidates WHERE status='pending'",
                (),
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.get::<i64>(0).unwrap(), 2);
        row.get::<i64>(1).unwrap()
    };
    // Claims order equal priorities by whole-second created_at, then UUID, not
    // admission order. Establish the strict age assumed by the assertion below
    // through the real clock/API rather than bypassing immutable timestamps.
    tokio::time::timeout(hang(10), async {
        loop {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            if i64::try_from(now).unwrap() > candidate_created_at {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("setup clock must advance beyond the captured candidate creation second");
    for media in ["tv", "movies"] {
        let (code, command) = request(
            &base,
            "POST",
            "/api/v1/blocklist/clear-commands",
            json!({"target":{"media_type":media},"priority":"normal"}),
        )
        .await;
        assert_eq!(code, 202, "{command}");
        assert!(
            command["created_at"].as_i64().unwrap() > candidate_created_at,
            "clear command must be strictly newer than both candidates: {command}; candidate max={candidate_created_at}"
        );
    }
    let runtime = commands::start(db.clone(), client).await.unwrap();
    tokio::time::timeout(hang(10), async {
        loop {
            let (code, v) = request(&base, "GET", "/api/v1/rss/candidates", Value::Null).await;
            assert_eq!(code, 200, "{v}");
            assert_eq!(
                v["items"].as_array().unwrap().len(),
                2,
                "both durable targets must survive reopening; an empty all() is not evidence"
            );
            let domains = v["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["target"]["media_type"].as_str().unwrap())
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(domains, std::collections::BTreeSet::from(["tv", "movies"]));
            if v["items"]
                .as_array()
                .unwrap()
                .iter()
                .all(|v| v["status"] == "rejected")
            {
                for item in v["items"].as_array().unwrap() {
                    assert_eq!(item["error_code"], "key_unavailable");
                }
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    let c = db.connect().await.unwrap();
    let mut rows = c
        .query("SELECT kind FROM owned_order ORDER BY seq LIMIT 2", ())
        .await
        .unwrap();
    let mut kinds = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        kinds.push(row.get::<String>(0).unwrap());
    }
    assert_eq!(
        kinds,
        ["candidate", "candidate"],
        "older due candidates share priority ordering and advance ahead of newer queued normal commands"
    );
    assert!(state.adds.lock().unwrap().is_empty());
    runtime.shutdown().await;
}
#[tokio::test]
async fn owned_targets_hashes_and_missing_titles_remain_explicit() {
    let scratch =
        Scratch(std::env::temp_dir().join(format!("hrrdarr-rss-owned-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&scratch.0).unwrap();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    seed(&db).await;
    let state = Arc::new(Remote::default());
    state.mode.store(5, Ordering::SeqCst);
    let (remote_base, _remote) = serve(
        axum::Router::new()
            .fallback(remote)
            .with_state(state.clone()),
    )
    .await;
    *state.endpoint.lock().unwrap() = remote_base.clone();
    let (base, _server, client) = app(db.clone()).await;
    let (indexer, download) = providers(&base, &remote_base).await;
    let runtime = commands::start(db.clone(), client).await.unwrap();
    let command = enqueue(&base, target(&indexer, &download, "tv")).await;
    let done = until(&base, &command["id"]).await;
    assert_eq!(done["observed"], 1, "{done}");
    assert_eq!(
        done["rejected"], 1,
        "missing title records rejection without blocking valid release"
    );
    for (mode, media, error) in [(6, "tv", "target_conflict"), (7, "movies", "hash_conflict")] {
        state.mode.store(mode, Ordering::SeqCst);
        let command = enqueue(&base, target(&indexer, &download, media)).await;
        let done = until(&base, &command["id"]).await;
        assert_eq!(done["rejected"], 1, "{done}");
        let (_, v) = request(
            &base,
            "GET",
            &format!(
                "/api/v1/rss/candidates?command_id={}",
                command["id"].as_str().unwrap()
            ),
            Value::Null,
        )
        .await;
        assert_eq!(v["items"][0]["error_code"], error);
    }
    assert_eq!(
        state.adds.lock().unwrap().len(),
        1,
        "neither another hash for an owned target nor a cross-domain hash may dispatch"
    );
    for query in ["limit=0", "offset=10001", "status=unknown", "extra=1"] {
        assert_eq!(
            request(
                &base,
                "GET",
                &format!("/api/v1/rss/candidates?{query}"),
                Value::Null
            )
            .await
            .0,
            400
        );
    }
    runtime.shutdown().await;
}
#[tokio::test]
async fn rate_limit_due_time_survives_retry_without_feed_or_mutation_hammering() {
    let scratch =
        Scratch(std::env::temp_dir().join(format!("hrrdarr-rss-rate-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&scratch.0).unwrap();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    seed(&db).await;
    let state = Arc::new(Remote::default());
    state.mode.store(8, Ordering::SeqCst);
    let (remote_base, _remote) = serve(
        axum::Router::new()
            .fallback(remote)
            .with_state(state.clone()),
    )
    .await;
    *state.endpoint.lock().unwrap() = remote_base.clone();
    let (base, _server, client) = app(db.clone()).await;
    let (indexer, download) = providers(&base, &remote_base).await;
    let command = enqueue(&base, target(&indexer, &download, "tv")).await;
    let runtime = commands::start(db.clone(), client).await.unwrap();
    let delayed = tokio::time::timeout(hang(8), async {
        loop {
            let (_, v) = request(
                &base,
                "GET",
                &format!("/api/v1/rss/commands/{}", command["id"].as_str().unwrap()),
                Value::Null,
            )
            .await;
            if v["status"] == "retry_wait" {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        delayed["next_attempt_at"].as_i64().unwrap()
            >= delayed["started_at"].as_i64().unwrap() + 120,
        "provider Retry-After is durable rather than reduced to default backoff"
    );
    assert_eq!(delayed["attempts"], 1);
    assert!(state.adds.lock().unwrap().is_empty());
    runtime.shutdown().await;
}
#[tokio::test]
async fn delayed_release_never_silently_switches_to_a_new_library_target() {
    let scratch =
        Scratch(std::env::temp_dir().join(format!("hrrdarr-rss-target-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&scratch.0).unwrap();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    seed(&db).await;
    db.connect()
        .await
        .unwrap()
        .execute(
            "UPDATE release_delay_policies SET torrent_delay_minutes=60",
            (),
        )
        .await
        .unwrap();
    let state = Arc::new(Remote::default());
    *state.date.lock().unwrap() = Some(chrono::Utc::now().to_rfc2822());
    let (remote_base, _remote) = serve(
        axum::Router::new()
            .fallback(remote)
            .with_state(state.clone()),
    )
    .await;
    *state.endpoint.lock().unwrap() = remote_base.clone();
    let (base, _server, client) = app(db.clone()).await;
    let (indexer, download) = providers(&base, &remote_base).await;
    let runtime = commands::start(db.clone(), client.clone()).await.unwrap();
    let command = enqueue(&base, target(&indexer, &download, "tv")).await;
    assert_eq!(until(&base, &command["id"]).await["pending"], 1);
    runtime.shutdown().await;
    db.connect().await.unwrap().execute_batch("UPDATE series SET title='Former title' WHERE id=1; INSERT INTO series(id,tvdb_id,title,path)VALUES(2,102,'Harbor','/owned-other'); INSERT INTO seasons(series_id,number)VALUES(2,1); INSERT INTO episodes(id,series_id,season,number,title,runtime,air_date_utc)VALUES(2,2,1,1,'Other pilot',45,'2020-01-01 00:00:00'); INSERT INTO library_settings(media_type,series_id,quality_profile_id,series_type,use_scene_numbering)VALUES('tv',2,1,'standard',0); UPDATE release_delay_policies SET torrent_delay_minutes=0; UPDATE rss_candidates SET not_before=0 WHERE status='pending';").await.unwrap();
    let runtime = commands::start(db.clone(), client).await.unwrap();
    let receipt = receipt_until(&base, "rejected").await;
    assert_eq!(receipt["error_code"], "target_changed");
    assert_eq!(
        receipt["target"],
        json!({"media_type":"tv","series_id":1,"episode_ids":[1]}),
        "a delayed intent retains the original typed target despite a new title match"
    );
    assert!(state.adds.lock().unwrap().is_empty());
    runtime.shutdown().await;
}

#[tokio::test]
async fn pending_cross_command_winner_keeps_provider_and_submits_once() {
    for (media, restriction) in [
        ("tv", false),
        ("movies", false),
        ("tv", true),
        ("movies", true),
    ] {
        let scratch = Scratch(std::env::temp_dir().join(format!(
            "hrrdarr-rss-cohort-worker-{}",
            uuid::Uuid::new_v4()
        )));
        std::fs::create_dir_all(&scratch.0).unwrap();
        let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
        seed(&db).await;
        let c = db.connect().await.unwrap();
        c.execute(
            "UPDATE release_delay_policies SET torrent_delay_minutes=60,usenet_delay_minutes=60",
            (),
        )
        .await
        .unwrap();
        let timestamp = chrono::Utc::now().timestamp();
        let old_state = Arc::new(Remote::default());
        let young_state = Arc::new(Remote::default());
        *old_state.date.lock().unwrap() = Some(
            chrono::DateTime::from_timestamp(timestamp - 601, 0)
                .unwrap()
                .to_rfc2822(),
        );
        *young_state.date.lock().unwrap() = Some(
            chrono::DateTime::from_timestamp(timestamp, 0)
                .unwrap()
                .to_rfc2822(),
        );
        let (old_base, _old_server) = serve(
            axum::Router::new()
                .fallback(remote)
                .with_state(old_state.clone()),
        )
        .await;
        let (young_base, _young_server) = serve(
            axum::Router::new()
                .fallback(remote)
                .with_state(young_state.clone()),
        )
        .await;
        *old_state.endpoint.lock().unwrap() = old_base.clone();
        *young_state.endpoint.lock().unwrap() = young_base.clone();
        let (base, _server, client) = app(db.clone()).await;
        let (old_indexer, old_download) = providers(&base, &old_base).await;
        let (young_indexer, young_download) = providers(&base, &young_base).await;
        let old_command = enqueue(&base, target(&old_indexer, &old_download, media)).await;
        let young_command = enqueue(&base, target(&young_indexer, &young_download, media)).await;
        let runtime = commands::start(db.clone(), client.clone()).await.unwrap();
        for command in [&old_command, &young_command] {
            let done = until(&base, &command["id"]).await;
            assert_eq!(done["status"], "succeeded", "{done}");
            assert_eq!(done["pending"], 1, "{done}");
        }
        runtime.shutdown().await;
        assert!(old_state.adds.lock().unwrap().is_empty());
        assert!(young_state.adds.lock().unwrap().is_empty());
        if restriction {
            let mut profile = admission_profile(media);
            profile["indexers"] = json!([{"kind":"provider","id":old_indexer["id"]}]);
            let (code, value) = request(
                &base,
                "POST",
                &format!("/api/v1/{media}/release-profiles"),
                json!({"revision":1,"profile":profile}),
            )
            .await;
            assert_eq!(code, 201, "{value}");
        } else {
            c.execute(
                "UPDATE providers SET enabled=0,revision=revision+1 WHERE id=?",
                [old_indexer["id"].as_str().unwrap()],
            )
            .await
            .unwrap();
        }
        save_delay(&base, media, 10).await;
        let young_id: String = c
            .query(
                "SELECT id FROM rss_candidates WHERE command_id=?",
                [young_command["id"].as_str().unwrap()],
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        // Deterministic scheduler seam: retain a stale future deadline on the young
        // candidate. Only the old command's receipt is initially due; its actual
        // run_due cohort selection must pick the other command's eligible offer.
        c.execute(
            "UPDATE rss_candidates SET not_before=? WHERE id=?",
            libsql::params![timestamp + 3600, young_id.as_str()],
        )
        .await
        .unwrap();
        let due: i64 = c
            .query(
                "SELECT count(*) FROM rss_candidates WHERE status='pending' AND not_before<=?",
                [chrono::Utc::now().timestamp()],
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        assert_eq!(due, 1);
        let runtime = commands::start(db.clone(), client).await.unwrap();
        let observed = receipt_until(&base, "observed").await;
        assert_eq!(observed["id"], young_id);
        assert_eq!(observed["command_id"], young_command["id"]);
        assert_eq!(observed["source"]["indexer_id"], young_indexer["id"]);
        assert_eq!(observed["source"]["client_id"], young_download["id"]);
        assert_eq!(observed["source"]["media_type"], media);
        assert!(old_state.adds.lock().unwrap().is_empty());
        assert_eq!(
            young_state.adds.lock().unwrap().as_slice(),
            [if media == "tv" { TV } else { MOVIE }]
        );
        runtime.shutdown().await;
        let claimed: String = c
            .query("SELECT candidate_id FROM rss_hash_claims", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        assert_eq!(claimed, young_id);
    }
}

#[tokio::test]
async fn prepared_owner_still_rechecks_increased_delay_before_post() {
    for media in ["tv", "movies"] {
        let scratch = Scratch(std::env::temp_dir().join(format!(
            "hrrdarr-rss-prepared-delay-{}",
            uuid::Uuid::new_v4()
        )));
        std::fs::create_dir_all(&scratch.0).unwrap();
        let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
        seed(&db).await;
        let state = Arc::new(Remote::default());
        state.mode.store(4, Ordering::SeqCst);
        *state.date.lock().unwrap() = Some(chrono::Utc::now().to_rfc2822());
        let (remote_base, _remote) = serve(
            axum::Router::new()
                .fallback(remote)
                .with_state(state.clone()),
        )
        .await;
        *state.endpoint.lock().unwrap() = remote_base.clone();
        let (base, _server, client) = app(db.clone()).await;
        let (indexer, download) = providers(&base, &remote_base).await;
        save_delay(&base, media, 0).await;
        let command = enqueue(&base, target(&indexer, &download, media)).await;
        let runtime = commands::start(db.clone(), client).await.unwrap();
        tokio::time::timeout(hang(12), state.started.notified())
            .await
            .unwrap();
        state.release.notify_one();
        tokio::time::timeout(hang(12), state.started.notified())
            .await
            .unwrap();
        let prepared = receipt_until(&base, "prepared").await;
        let c = db.connect().await.unwrap();
        let identity: String = c
            .query(
                "SELECT submission_identity_json FROM rss_candidates WHERE id=?",
                [prepared["id"].as_str().unwrap()],
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        assert!(state.adds.lock().unwrap().is_empty());
        save_delay(&base, media, 60).await;
        let after: String = c
            .query(
                "SELECT submission_identity_json FROM rss_candidates WHERE id=?",
                [prepared["id"].as_str().unwrap()],
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        assert_eq!(after, identity);
        let claims: i64 = c
            .query("SELECT count(*) FROM rss_hash_claims", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        assert_eq!(claims, 0);
        // Policy wake deliberately leaves the prepared owner untouched. Its final
        // current-policy check still rejects a newly delayed offer; only pending
        // offers are preserved for later retry by the delay settlement path.
        assert_eq!(receipt_until(&base, "prepared").await["id"], prepared["id"]);
        state.release.notify_one();
        let done = until(&base, &command["id"]).await;
        assert_eq!(done["status"], "succeeded", "{done}");
        assert_eq!(done["rejected"], 1);
        let rejected = receipt_until(&base, "rejected").await;
        assert_eq!(rejected["id"], prepared["id"]);
        assert_eq!(rejected["error_code"], "target_changed");
        assert_eq!(rejected["reasons"], json!([]));
        assert!(state.adds.lock().unwrap().is_empty());
        runtime.shutdown().await;
    }
}

fn admission_profile(media: &str) -> Value {
    let mut p = json!({"name":"Admission","enabled":true,"required":["NEVER_MATCH_THIS_TITLE"],"ignored":[],"tag_ids":[],"indexers":[]});
    if media == "tv" {
        p["excluded_tag_ids"] = json!([]);
        p["air_date_restriction"] = json!(false);
        p["air_date_grace_period_days"] = json!(0);
        p["allow_season_pack_without_all_episodes_aired"] = json!(false);
    }
    p
}
#[tokio::test]
async fn release_restriction_change_during_preparation_never_posts() {
    for media in ["tv", "movies"] {
        let scratch = Scratch(
            std::env::temp_dir().join(format!("hrrdarr-restriction-held-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir_all(&scratch.0).unwrap();
        let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
        seed(&db).await;
        let state = Arc::new(Remote::default());
        state.mode.store(4, Ordering::SeqCst);
        let (remote_base, _remote) = serve(
            axum::Router::new()
                .fallback(remote)
                .with_state(state.clone()),
        )
        .await;
        *state.endpoint.lock().unwrap() = remote_base.clone();
        let (base, _server, client) = app(db.clone()).await;
        let (indexer, download) = providers(&base, &remote_base).await;
        let command = enqueue(&base, target(&indexer, &download, media)).await;
        let runtime = commands::start(db.clone(), client).await.unwrap();
        tokio::time::timeout(hang(12), state.started.notified())
            .await
            .unwrap();
        let c = db.connect().await.unwrap();
        let id = c
            .query(
                "SELECT id FROM rss_candidates WHERE command_id=?",
                [command["id"].as_str().unwrap()],
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap();
        let payload_before: Vec<u8> = c
            .query(
                "SELECT private_payload FROM rss_candidates WHERE id=?",
                [id.clone()],
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        // A unique term forces cold writer evidence even when another serial test
        // has warmed the global cache. The writer must retry without submitting.
        let mut profile = admission_profile(media);
        profile["required"] = json!([format!("NEVER_{}", uuid::Uuid::new_v4())]);
        let (code, value) = request(
            &base,
            "POST",
            &format!("/api/v1/{media}/release-profiles"),
            json!({"revision":1,"profile":profile}),
        )
        .await;
        assert_eq!(code, 201, "{value}");
        let released_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        state.release.notify_one();
        let retry = tokio::time::timeout(hang(12), async {
            loop {
                let (code, value) = request(
                    &base,
                    "GET",
                    &format!("/api/v1/rss/commands/{}", command["id"].as_str().unwrap()),
                    Value::Null,
                )
                .await;
                assert_eq!(code, 200, "{value}");
                if value["status"] == "retry_wait" {
                    break value;
                }
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
        })
        .await
        .expect("cold policy evidence must retain a retryable parent");
        assert_eq!(retry["error_code"], "storage_error");
        assert_eq!(retry["attempts"], 1);
        assert_eq!(retry["fetch_complete"], true);
        let retry_deadline = retry["next_attempt_at"].as_i64().unwrap();
        assert!((released_at + 30..=released_at + 42).contains(&retry_deadline));
        assert_eq!(retry["completed_at"], Value::Null);
        let pending = receipt_until(&base, "pending").await;
        assert_eq!(pending["id"], id);
        assert!(state.adds.lock().unwrap().is_empty());
        let payload_after: Vec<u8> = c
            .query(
                "SELECT private_payload FROM rss_candidates WHERE id=?",
                [id.clone()],
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        assert_eq!(payload_after, payload_before);
        let rejected = if media == "tv" {
            // A real accepted save wakes pending ownership and its parent atomically.
            let (code, saved) = request(
                &base,
                "PUT",
                &format!(
                    "/api/v1/{media}/release-profiles/{}",
                    value["profiles"][0]["id"].as_i64().unwrap()
                ),
                json!({"revision":value["revision"],"profile":profile}),
            )
            .await;
            assert_eq!(code, 200, "{saved}");
            receipt_until(&base, "rejected").await
        } else {
            // Observe the actual 30-second automatic retry without a settings wake.
            // This separate bound includes that backoff; existing request/receipt
            // timeouts remain unchanged for all immediate-settlement scenarios.
            tokio::time::timeout(hang(45), async {
                loop {
                    let (code, values) =
                        request(&base, "GET", "/api/v1/rss/candidates", Value::Null).await;
                    assert_eq!(code, 200, "{values}");
                    if let Some(value) = values["items"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|v| v["id"] == id && v["status"] == "rejected")
                    {
                        break value.clone();
                    }
                    tokio::time::sleep(Duration::from_millis(40)).await;
                }
            })
            .await
            .expect("automatic retry must evaluate fresh policy after its backoff")
        };
        assert_eq!(rejected["id"], id);
        // A current policy veto records its precise decision reason; target_changed
        // is reserved for identity changes or a prepared candidate becoming delayed.
        assert_eq!(rejected["error_code"], Value::Null);
        assert_eq!(
            rejected["reasons"],
            json!(["release_required_term_missing"])
        );
        assert!(state.adds.lock().unwrap().is_empty());
        assert_eq!(
            c.query("SELECT count(*) FROM rss_hash_claims", ())
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap()
                .get::<i64>(0)
                .unwrap(),
            0
        );
        runtime.shutdown().await;
    }
}

#[tokio::test]
async fn indexer_policy_rss_schedule_and_queued_revision_send_no_http() {
    for media in ["tv", "movies"] {
        let scratch = Scratch(
            std::env::temp_dir().join(format!("hrrdarr-rss-operation-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir(&scratch.0).unwrap();
        let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
        seed(&db).await;
        let peer = Arc::new(Remote::default());
        let (remote_base, _remote) = serve(
            axum::Router::new()
                .fallback(remote)
                .with_state(peer.clone()),
        )
        .await;
        let (base, _server, client) = app(db.clone()).await;
        let (indexer, download) = providers(&base, &remote_base).await;
        let queued = enqueue(&base, target(&indexer, &download, media)).await;
        let (code, schedule) = request(
            &base,
            "POST",
            "/api/v1/rss/schedules",
            json!({"target":target(&indexer,&download,media),"interval_seconds":60,"enabled":true}),
        )
        .await;
        assert_eq!(code, 200, "{schedule}");
        let mut settings = indexer["settings"].clone();
        settings[media]["enable_rss"] = json!(false);
        let (code,updated)=request(&base,"PUT",&format!("/api/v1/providers/{}",indexer["id"].as_str().unwrap()),json!({"revision":indexer["revision"],"name":"RSS disabled","enabled":true,"priority":1,"settings":settings})).await;
        assert_eq!(code, 200, "{updated}");
        assert_eq!(request(&base,"POST","/api/v1/rss/schedules",json!({"revision":schedule["revision"],"target":target(&updated,&download,media),"interval_seconds":60,"enabled":true})).await.0,409);
        // Advance only the owned scheduler fixture deadline; provider policy uses the real API.
        db.connect()
            .await
            .unwrap()
            .execute("UPDATE rss_schedules SET next_run_at=0", ())
            .await
            .unwrap();
        let runtime = commands::start(db.clone(), client.clone()).await.unwrap();
        assert_eq!(until(&base, &queued["id"]).await["status"], "failed");
        tokio::time::timeout(hang(10), async {
            loop {
                let (_, rows) = request(&base, "GET", "/api/v1/rss/schedules", Value::Null).await;
                if rows[0]["enabled"] == false {
                    assert_eq!(rows[0]["error_code"], "provider_changed");
                    break;
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .expect("stale schedule records disabled provider policy");
        assert!(
            peer.indexer_requests.lock().unwrap().is_empty(),
            "stale queued feed and schedule must send neither CAPS nor query"
        );
        assert!(peer.adds.lock().unwrap().is_empty());
        runtime.shutdown().await;
        // Search switches are independent: re-enable RSS alone and execute actual feed/send.
        let mut settings = updated["settings"].clone();
        settings[media]["enable_rss"] = json!(true);
        settings[media]["enable_automatic_search"] = json!(false);
        settings[media]["enable_interactive_search"] = json!(false);
        let(code,updated)=request(&base,"PUT",&format!("/api/v1/providers/{}",updated["id"].as_str().unwrap()),json!({"revision":updated["revision"],"name":"RSS only","enabled":true,"priority":1,"settings":settings})).await;
        assert_eq!(code, 200, "{updated}");
        let runtime = commands::start(db, client).await.unwrap();
        let command = enqueue(&base, target(&updated, &download, media)).await;
        let done = until(&base, &command["id"]).await;
        assert_eq!(done["status"], "succeeded", "{done}");
        assert_eq!(done["observed"], 1, "{done}");
        assert_eq!(peer.adds.lock().unwrap().len(), 1);
        assert!(!peer.indexer_requests.lock().unwrap().is_empty());
        runtime.shutdown().await;
    }
}

#[tokio::test]
async fn indexer_policy_retry_wait_feed_never_reuses_changed_revision() {
    for media in ["tv", "movies"] {
        let scratch = Scratch(
            std::env::temp_dir().join(format!("hrrdarr-rss-policy-retry-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir(&scratch.0).unwrap();
        let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
        seed(&db).await;
        let peer = Arc::new(Remote::default());
        peer.mode.store(8, Ordering::SeqCst);
        let (remote_base, _remote) = serve(
            axum::Router::new()
                .fallback(remote)
                .with_state(peer.clone()),
        )
        .await;
        let (base, _server, client) = app(db.clone()).await;
        let (indexer, download) = providers(&base, &remote_base).await;
        let runtime = commands::start(db.clone(), client.clone()).await.unwrap();
        let command = enqueue(&base, target(&indexer, &download, media)).await;
        tokio::time::timeout(hang(10), async {
            loop {
                let (_, row) = request(
                    &base,
                    "GET",
                    &format!("/api/v1/rss/commands/{}", command["id"].as_str().unwrap()),
                    Value::Null,
                )
                .await;
                if row["status"] == "retry_wait" {
                    assert_eq!(row["attempts"], 1);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .expect("real remote rate limit leaves retry intent");
        runtime.shutdown().await;
        let before = peer.indexer_requests.lock().unwrap().len();
        assert!(before > 0);
        let mut settings = indexer["settings"].clone();
        settings[media]["enable_rss"] = json!(false);
        let (code,updated)=request(&base,"PUT",&format!("/api/v1/providers/{}",indexer["id"].as_str().unwrap()),json!({"revision":indexer["revision"],"name":"policy changed","enabled":true,"priority":1,"settings":settings})).await;
        assert_eq!(code, 200, "{updated}");
        peer.mode.store(0, Ordering::SeqCst);
        db.connect()
            .await
            .unwrap()
            .execute(
                "UPDATE rss_commands SET next_attempt_at=0 WHERE status='retry_wait'",
                (),
            )
            .await
            .unwrap();
        let runtime = commands::start(db, client).await.unwrap();
        let done = until(&base, &command["id"]).await;
        assert_eq!(done["status"], "failed", "{done}");
        assert_eq!(
            peer.indexer_requests.lock().unwrap().len(),
            before,
            "retry must reject captured stale revision before CAPS"
        );
        assert!(peer.adds.lock().unwrap().is_empty());
        runtime.shutdown().await;
    }
}

#[tokio::test]
async fn client_binding_fences_rss_admission_schedules_and_prepared_dispatch() {
    for media in ["tv", "movies"] {
        for preparing in [false, true] {
            eprintln!("event=binding_fixture media_type={media} preparing={preparing}");
            let scratch = Scratch(
                std::env::temp_dir().join(format!("hrrdarr-rss-binding-{}", uuid::Uuid::new_v4())),
            );
            std::fs::create_dir(&scratch.0).unwrap();
            let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
            seed(&db).await;
            let peer = Arc::new(Remote::default());
            let (remote_base, _remote) = serve(
                axum::Router::new()
                    .fallback(remote)
                    .with_state(peer.clone()),
            )
            .await;
            *peer.endpoint.lock().unwrap() = remote_base.clone();
            let (base, _server, client) = app(db.clone()).await;
            let (indexer, download) = providers(&base, &remote_base).await;
            let (_, other) = providers(&base, &remote_base).await;
            let bound = bind_client(&base, &indexer, media, &download).await;
            for invalid in [
                target(&bound, &other, media),
                target(&indexer, &download, media),
            ] {
                for (path, body) in [
                    (
                        "/api/v1/rss/commands",
                        json!({"target":invalid,"priority":"normal"}),
                    ),
                    (
                        "/api/v1/rss/schedules",
                        json!({"target":invalid,"interval_seconds":60,"enabled":true}),
                    ),
                ] {
                    let (code, error) = request(&base, "POST", path, body).await;
                    assert_eq!(code, 409, "{error}");
                    assert_eq!(error["error"]["code"], "provider_changed");
                }
            }
            assert!(peer.indexer_requests.lock().unwrap().is_empty());
            let command = enqueue(&base, target(&bound, &download, media)).await;
            let (code, schedule) = request(&base, "POST", "/api/v1/rss/schedules", json!({"target":target(&bound,&download,media),"interval_seconds":60,"enabled":true})).await;
            assert_eq!(code, 200, "{schedule}");
            let mut runtime = None;
            if preparing {
                peer.mode.store(4, Ordering::SeqCst);
                runtime = Some(commands::start(db.clone(), client.clone()).await.unwrap());
                tokio::time::timeout(hang(12), peer.started.notified())
                    .await
                    .expect("torrent preparation boundary");
            }
            bind_client(&base, &bound, media, &other).await;
            // Advance only the owned scheduler deadline; binding changes use the provider API.
            db.connect()
                .await
                .unwrap()
                .execute("UPDATE rss_schedules SET next_run_at=0", ())
                .await
                .unwrap();
            if preparing {
                peer.release.notify_one();
            } else {
                runtime = Some(commands::start(db.clone(), client).await.unwrap());
            }
            let done = until(&base, &command["id"]).await;
            if preparing {
                assert_eq!(done["rejected"], 1, "{done}");
                assert_eq!(
                    receipt_until(&base, "rejected").await["error_code"],
                    "provider_changed"
                );
            } else {
                assert_eq!(done["status"], "failed", "{done}");
            }
            tokio::time::timeout(hang(10), async {
                loop {
                    let (_, schedules) =
                        request(&base, "GET", "/api/v1/rss/schedules", Value::Null).await;
                    if schedules[0]["enabled"] == false {
                        assert_eq!(schedules[0]["error_code"], "provider_changed");
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(30)).await;
                }
            })
            .await
            .expect("changed binding disables old schedule");
            assert!(
                peer.adds.lock().unwrap().is_empty(),
                "binding change must never reroute or submit old intent"
            );
            runtime.unwrap().shutdown().await;
        }
    }
}

fn selection_target(indexer: &Value, media: &str, selection: Value) -> Value {
    json!({"media_type":media,"indexer_id":indexer["id"],"indexer_revision":indexer["revision"],"client_selection":selection})
}
#[tokio::test]
async fn client_selection_tv_falls_back_but_movies_and_bound_defaults_do_not() {
    let scratch =
        Scratch(std::env::temp_dir().join(format!("hrrdarr-selection-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&scratch.0).unwrap();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    seed(&db).await;
    let state = Arc::new(Remote::default());
    let (remote_base, _remote) = serve(
        axum::Router::new()
            .fallback(remote)
            .with_state(state.clone()),
    )
    .await;
    *state.endpoint.lock().unwrap() = remote_base.clone();
    let (failed_base, _failed) = serve(
        axum::Router::new()
            .fallback(|| async { (StatusCode::SERVICE_UNAVAILABLE, "owned client unavailable") }),
    )
    .await;
    let (base, _server, client) = app(db.clone()).await;
    let (indexer, healthy) = providers(&base, &remote_base).await;
    let (code,healthy)=request(&base,"PUT",&format!("/api/v1/providers/{}",healthy["id"].as_str().unwrap()),json!({"revision":healthy["revision"],"name":"fallback","enabled":true,"priority":10,"settings":healthy["settings"]})).await;
    assert_eq!(code, 200, "{healthy}");
    let (code,failed)=request(&base,"POST","/api/v1/providers",json!({"name":"first","enabled":true,"priority":1,"settings":{"implementation":"qbittorrent","endpoint":failed_base,"tv":{"category":"tv","imported_category":null,"recent_priority":0,"older_priority":0},"movies":{"category":"movies","imported_category":null,"recent_priority":0,"older_priority":0}}})).await;
    assert_eq!(code, 201, "{failed}");
    let c = db.connect().await.unwrap();
    c.execute(
        "UPDATE download_refresh_schedules SET next_run_at=unixepoch()+3600",
        (),
    )
    .await
    .unwrap();
    let runtime = commands::start(db.clone(), client).await.unwrap();
    for media in ["tv", "movies"] {
        let command = enqueue(
            &base,
            selection_target(&indexer, media, json!({"kind":"default"})),
        )
        .await;
        assert_eq!(command["target"]["selection_kind"], "default");
        assert!(command["target"]["client_id"].is_null());
        let done = until(&base, &command["id"]).await;
        assert_eq!(done["status"], "succeeded", "{done}");
        if media == "tv" {
            receipt_until(&base, "observed").await;
        }
        let (_, receipts) = request(
            &base,
            "GET",
            &format!(
                "/api/v1/rss/candidates?command_id={}",
                command["id"].as_str().unwrap()
            ),
            Value::Null,
        )
        .await;
        let receipt = &receipts["items"][0];
        let attempts = receipt["client_attempts"].as_array().unwrap();
        assert_eq!(attempts[0]["client_id"], failed["id"]);
        assert_eq!(attempts[0]["state"], "definitely_not_submitted");
        if media == "tv" {
            assert_eq!(receipt["status"], "observed", "{receipt}");
            assert_eq!(receipt["source"]["client_id"], healthy["id"]);
            assert_eq!(attempts.len(), 2);
            assert_eq!(attempts[1]["state"], "observed");
            assert_eq!(
                c.query(
                    "SELECT count(*) FROM rss_hash_claims WHERE client_id=?",
                    [failed["id"].as_str().unwrap()]
                )
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap()
                .get::<i64>(0)
                .unwrap(),
                0
            );
        } else {
            assert_eq!(receipt["status"], "rejected", "{receipt}");
            assert_eq!(attempts.len(), 1);
        }
    }
    assert_eq!(state.adds.lock().unwrap().as_slice(), [TV]);
    // A bound default remains a single-client choice even though the alternative is healthy.
    let bound = bind_client(&base, &indexer, "movies", &failed).await;
    let command = enqueue(
        &base,
        selection_target(&bound, "movies", json!({"kind":"default"})),
    )
    .await;
    until(&base, &command["id"]).await;
    let (_, receipts) = request(
        &base,
        "GET",
        &format!(
            "/api/v1/rss/candidates?command_id={}",
            command["id"].as_str().unwrap()
        ),
        Value::Null,
    )
    .await;
    assert_eq!(
        receipts["items"][0]["client_attempts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // New explicit intent can override the binding; legacy fixed requests cannot.
    let command=enqueue(&base,selection_target(&bound,"movies",json!({"kind":"explicit","provider_id":healthy["id"],"provider_revision":healthy["revision"]}))).await;
    assert_eq!(until(&base, &command["id"]).await["observed"], 1);
    assert_eq!(state.adds.lock().unwrap().as_slice(), [TV, MOVIE]);
    runtime.shutdown().await;
}

#[tokio::test]
async fn default_rss_selects_per_library_owner_within_one_feed() {
    let scratch = Scratch(
        std::env::temp_dir().join(format!("hrrdarr-owner-selection-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&scratch.0).unwrap();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    seed(&db).await;
    let first = Arc::new(Remote::default());
    let second = Arc::new(Remote::default());
    let (first_base, _first_server) = serve(
        axum::Router::new()
            .fallback(remote)
            .with_state(first.clone()),
    )
    .await;
    let (second_base, _second_server) = serve(
        axum::Router::new()
            .fallback(remote)
            .with_state(second.clone()),
    )
    .await;
    *first.endpoint.lock().unwrap() = first_base.clone();
    let second_hash = "3333333333333333333333333333333333333333";
    *first.extra_feed.lock().unwrap() = format!(
        r#"<item><title>Beacon.S01E01.1080p.WEB-DL</title><guid>PRIVATE_RSS_GUID-beacon</guid><pubDate>Mon, 01 Jan 2024 12:00:00 +0000</pubDate><link>magnet:?xt=urn:btih:{second_hash}</link><torznab:attr name="magneturl" value="magnet:?xt=urn:btih:{second_hash}"/><torznab:attr name="category" value="5030"/><torznab:attr name="size" value="1073741824"/></item>"#
    );
    let (base, _server, client) = app(db.clone()).await;
    let (indexer, first_client) = providers(&base, &first_base).await;
    let (_, second_client) = providers(&base, &second_base).await;
    // Only library/tag facts are seeded. The production fetch, matching, selector,
    // attempt journal, POST, observation, and receipt publication all run normally.
    let c = db.connect().await.unwrap();
    c.execute_batch("INSERT INTO series(id,tvdb_id,title,path)VALUES(2,102,'Beacon','/owned-beacon'); INSERT INTO seasons(series_id,number)VALUES(2,1); INSERT INTO episodes(id,series_id,season,number,title,runtime,air_date_utc)VALUES(2,2,1,1,'Pilot',45,'2020-01-01 00:00:00'); INSERT INTO library_settings(media_type,series_id,quality_profile_id,series_type,use_scene_numbering)VALUES('tv',2,1,'standard',0); INSERT INTO tags(id,media_type,label)VALUES(1,'tv','harbor'),(2,'tv','beacon'); INSERT INTO series_tags(series_id,tag_id)VALUES(1,1),(2,2); UPDATE download_refresh_schedules SET next_run_at=unixepoch()+3600;").await.unwrap();
    for (provider, tag) in [(&first_client, 1i64), (&second_client, 2i64)] {
        c.execute(
            "INSERT INTO download_client_tags(provider_id,media_type,tag_id)VALUES(?,'tv',?)",
            libsql::params![provider["id"].as_str().unwrap(), tag],
        )
        .await
        .unwrap();
    }
    let command = enqueue(
        &base,
        selection_target(&indexer, "tv", json!({"kind":"default"})),
    )
    .await;
    let runtime = commands::start(db.clone(), client).await.unwrap();
    let done = until(&base, &command["id"]).await;
    assert_eq!(done["observed"], 2, "{done}");
    let (_, receipts) = request(
        &base,
        "GET",
        &format!(
            "/api/v1/rss/candidates?command_id={}",
            command["id"].as_str().unwrap()
        ),
        Value::Null,
    )
    .await;
    for receipt in receipts["items"].as_array().unwrap() {
        let expected = if receipt["target"]["series_id"] == 1 {
            &first_client
        } else {
            &second_client
        };
        assert_eq!(receipt["source"]["client_id"], expected["id"], "{receipt}");
        assert_eq!(receipt["client_attempts"].as_array().unwrap().len(), 1);
    }
    assert_eq!(first.adds.lock().unwrap().as_slice(), [TV]);
    assert_eq!(second.adds.lock().unwrap().as_slice(), [second_hash]);
    runtime.shutdown().await;
}

#[tokio::test]
async fn unavailable_default_retry_stays_strict_until_client_block_expires() {
    for media in ["tv", "movies"] {
        let scratch = Scratch(
            std::env::temp_dir().join(format!("hrrdarr-strict-selection-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir(&scratch.0).unwrap();
        let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
        seed(&db).await;
        let state = Arc::new(Remote::default());
        let (remote_base, _remote) = serve(
            axum::Router::new()
                .fallback(remote)
                .with_state(state.clone()),
        )
        .await;
        let (base, server, client) = app(db.clone()).await;
        let (indexer, download) = providers(&base, &remote_base).await;
        let c = db.connect().await.unwrap();
        c.execute(
            "INSERT INTO tags(id,media_type,label)VALUES(1,?,'restricted')",
            [media],
        )
        .await
        .unwrap();
        c.execute(
            "INSERT INTO download_client_tags(provider_id,media_type,tag_id)VALUES(?,?,1)",
            libsql::params![download["id"].as_str().unwrap(), media],
        )
        .await
        .unwrap();
        c.execute(
            "UPDATE download_refresh_schedules SET next_run_at=unixepoch()+3600",
            (),
        )
        .await
        .unwrap();
        let command = enqueue(
            &base,
            selection_target(&indexer, media, json!({"kind":"default"})),
        )
        .await;
        let runtime = commands::start(db.clone(), client).await.unwrap();
        until(&base, &command["id"]).await;
        let receipt = receipt_until(&base, "pending").await;
        assert_eq!(
            receipt["error_code"], "download_client_unavailable",
            "{receipt}"
        );
        assert!(receipt["client_attempts"].as_array().unwrap().is_empty());
        runtime.shutdown().await;
        server.stop().await;
        // Provenance: seed a durable block and newly matching owner tag, then reopen
        // the service. Only scheduler deadlines are advanced; no attempts are seeded.
        c.execute(
            if media == "tv" {
                "INSERT INTO series_tags(series_id,tag_id)VALUES(1,1)"
            } else {
                "INSERT INTO movie_tags(movie_id,tag_id)VALUES(1,1)"
            },
            (),
        )
        .await
        .unwrap();
        c.execute("INSERT INTO download_client_status(provider_id,media_type,config_revision,escalation_level,initial_failure_at,last_failure_at,disabled_until,next_health_at)VALUES(?,?,?,2,unixepoch()-100,unixepoch(),unixepoch()+300,unixepoch()+300)",libsql::params![download["id"].as_str().unwrap(),media,download["revision"].as_i64().unwrap()]).await.unwrap();
        c.execute(
            "UPDATE rss_candidates SET not_before=0 WHERE id=?",
            [receipt["id"].as_str().unwrap()],
        )
        .await
        .unwrap();
        drop(c);
        drop(db);
        let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
        let (base, _server, client) = app(db.clone()).await;
        let runtime = commands::start(db.clone(), client).await.unwrap();
        tokio::time::timeout(hang(10), async {
            loop {
                let receipt = receipt_until(&base, "pending").await;
                if receipt["not_before"].as_i64().is_some_and(|v| v > 0) {
                    assert_eq!(receipt["error_code"], "download_client_unavailable");
                    assert!(receipt["client_attempts"].as_array().unwrap().is_empty());
                    break;
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .unwrap();
        assert!(state.adds.lock().unwrap().is_empty());
        let c = db.connect().await.unwrap();
        c.execute(
            "UPDATE download_client_status SET disabled_until=unixepoch()-1,next_health_at=NULL",
            (),
        )
        .await
        .unwrap();
        c.execute(
            "UPDATE rss_candidates SET not_before=0 WHERE id=?",
            [receipt["id"].as_str().unwrap()],
        )
        .await
        .unwrap();
        let observed = receipt_until(&base, "observed").await;
        assert_eq!(observed["source"]["client_id"], download["id"]);
        assert_eq!(observed["client_attempts"].as_array().unwrap().len(), 1);
        assert_eq!(state.adds.lock().unwrap().len(), 1);
        runtime.shutdown().await;
    }
}

#[tokio::test]
async fn tv_bound_default_and_explicit_failure_never_post_to_alternative() {
    for explicit in [false, true] {
        let scratch = Scratch(std::env::temp_dir().join(format!(
            "hrrdarr-fixed-tv-selection-{}",
            uuid::Uuid::new_v4()
        )));
        std::fs::create_dir(&scratch.0).unwrap();
        let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
        seed(&db).await;
        let state = Arc::new(Remote::default());
        let (remote_base, _remote) = serve(
            axum::Router::new()
                .fallback(remote)
                .with_state(state.clone()),
        )
        .await;
        let (failed_base, _failed) =
            serve(axum::Router::new().fallback(|| async { StatusCode::SERVICE_UNAVAILABLE })).await;
        let (base, _server, client) = app(db.clone()).await;
        let (indexer, _healthy) = providers(&base, &remote_base).await;
        let (_, failed) = providers(&base, &failed_base).await;
        let indexer = if explicit {
            indexer
        } else {
            bind_client(&base, &indexer, "tv", &failed).await
        };
        let selection = if explicit {
            json!({"kind":"explicit","provider_id":failed["id"],"provider_revision":failed["revision"]})
        } else {
            json!({"kind":"default"})
        };
        let command = enqueue(&base, selection_target(&indexer, "tv", selection)).await;
        let runtime = commands::start(db.clone(), client).await.unwrap();
        until(&base, &command["id"]).await;
        // Default automation retains unavailable work; explicit intent remains a
        // direct failure. Neither is allowed to use an unbound alternate.
        let receipt = receipt_until(&base, if explicit { "rejected" } else { "pending" }).await;
        if !explicit {
            assert_eq!(receipt["error_code"], "download_client_unavailable");
        }
        assert_eq!(
            receipt["client_attempts"].as_array().unwrap().len(),
            1,
            "{receipt}"
        );
        assert_eq!(receipt["client_attempts"][0]["client_id"], failed["id"]);
        assert_eq!(
            receipt["client_attempts"][0]["state"],
            "definitely_not_submitted"
        );
        assert!(
            state.adds.lock().unwrap().is_empty(),
            "fixed TV choice cannot fall through to healthy alternative"
        );
        runtime.shutdown().await;
    }
}

#[tokio::test]
async fn tv_default_client_revision_change_during_preparation_uses_safe_alternative() {
    for changed_bytes in [false, true] {
        let scratch = Scratch(std::env::temp_dir().join(format!(
            "hrrdarr-selection-revision-{}",
            uuid::Uuid::new_v4()
        )));
        std::fs::create_dir(&scratch.0).unwrap();
        let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
        seed(&db).await;
        let first = Arc::new(Remote::default());
        first.mode.store(4, Ordering::SeqCst);
        first.torrent_barrier_limit.store(2, Ordering::SeqCst);
        if changed_bytes {
            first.torrent_change_after.store(1, Ordering::SeqCst);
        }
        let second = Arc::new(Remote::default());
        let (first_base, _first_server) = serve(
            axum::Router::new()
                .fallback(remote)
                .with_state(first.clone()),
        )
        .await;
        *first.endpoint.lock().unwrap() = first_base.clone();
        let (second_base, _second_server) = serve(
            axum::Router::new()
                .fallback(remote)
                .with_state(second.clone()),
        )
        .await;
        let (base, _server, client) = app(db.clone()).await;
        let (indexer, first_client) = providers(&base, &first_base).await;
        let (_, second_client) = providers(&base, &second_base).await;
        let (code,second_client)=request(&base,"PUT",&format!("/api/v1/providers/{}",second_client["id"].as_str().unwrap()),json!({"revision":second_client["revision"],"name":"alternative","enabled":true,"priority":50,"settings":second_client["settings"]})).await;
        assert_eq!(code, 200, "{second_client}");
        db.connect()
            .await
            .unwrap()
            .execute(
                "UPDATE download_refresh_schedules SET next_run_at=unixepoch()+3600",
                (),
            )
            .await
            .unwrap();
        let command = enqueue(
            &base,
            selection_target(&indexer, "tv", json!({"kind":"default"})),
        )
        .await;
        let runtime = commands::start(db.clone(), client).await.unwrap();
        tokio::time::timeout(hang(12), first.started.notified())
            .await
            .unwrap();
        let (code,changed)=request(&base,"PUT",&format!("/api/v1/providers/{}",first_client["id"].as_str().unwrap()),json!({"revision":first_client["revision"],"name":"changed while preparing","enabled":true,"priority":1,"settings":first_client["settings"]})).await;
        assert_eq!(code, 200, "{changed}");
        first.release.notify_one();
        // Fallback re-fetches only the retained release URL; immutable payload SHA
        // must match before the alternate can post those prepared torrent bytes.
        tokio::time::timeout(hang(12), first.started.notified())
            .await
            .unwrap();
        first.release.notify_one();
        let receipt = receipt_until(
            &base,
            if changed_bytes {
                "rejected"
            } else {
                "observed"
            },
        )
        .await;
        assert_eq!(receipt["source"]["client_id"], second_client["id"]);
        assert_eq!(receipt["client_attempts"].as_array().unwrap().len(), 2);
        assert_eq!(
            receipt["client_attempts"][0]["error_code"],
            "provider_changed"
        );
        assert_eq!(
            receipt["client_attempts"][1]["state"],
            if changed_bytes {
                "definitely_not_submitted"
            } else {
                "observed"
            }
        );
        if changed_bytes {
            assert_eq!(receipt["error_code"], "release_changed");
        }
        let c = db.connect().await.unwrap();
        assert_eq!(c.query("SELECT identity_json IS NOT NULL FROM rss_client_attempts WHERE candidate_id=? AND ordinal=1",[receipt["id"].as_str().unwrap()]).await.unwrap().next().await.unwrap().unwrap().get::<i64>(0).unwrap(),1,"configuration race retains the first fetched-byte identity");
        assert!(first.adds.lock().unwrap().is_empty());
        assert_eq!(
            second.adds.lock().unwrap().len(),
            usize::from(!changed_bytes)
        );
        assert_eq!(until(&base, &command["id"]).await["status"], "succeeded");
        runtime.shutdown().await;
    }
}

#[tokio::test]
async fn tv_fallback_stops_on_invalid_or_changed_release_bytes() {
    for mode in [9, 10] {
        let scratch = Scratch(
            std::env::temp_dir().join(format!("hrrdarr-selection-bytes-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir(&scratch.0).unwrap();
        let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
        seed(&db).await;
        let state = Arc::new(Remote::default());
        state.mode.store(mode, Ordering::SeqCst);
        let (remote_base, _remote) = serve(
            axum::Router::new()
                .fallback(remote)
                .with_state(state.clone()),
        )
        .await;
        *state.endpoint.lock().unwrap() = remote_base.clone();
        let (failed_base, _failed) =
            serve(axum::Router::new().fallback(|| async { StatusCode::SERVICE_UNAVAILABLE })).await;
        let (base, _server, client) = app(db.clone()).await;
        let (indexer, healthy) = providers(&base, &remote_base).await;
        let (code,healthy)=request(&base,"PUT",&format!("/api/v1/providers/{}",healthy["id"].as_str().unwrap()),json!({"revision":healthy["revision"],"name":"alternate","enabled":true,"priority":50,"settings":healthy["settings"]})).await;
        assert_eq!(code, 200, "{healthy}");
        let (_, failed) = providers(&base, &failed_base).await;
        db.connect()
            .await
            .unwrap()
            .execute(
                "UPDATE download_refresh_schedules SET next_run_at=unixepoch()+3600",
                (),
            )
            .await
            .unwrap();
        let command = enqueue(
            &base,
            selection_target(&indexer, "tv", json!({"kind":"default"})),
        )
        .await;
        let runtime = commands::start(db.clone(), client).await.unwrap();
        until(&base, &command["id"]).await;
        let receipt = receipt_until(&base, "rejected").await;
        assert_eq!(receipt["client_attempts"][0]["client_id"], failed["id"]);
        assert_eq!(
            receipt["client_attempts"].as_array().unwrap().len(),
            if mode == 9 { 1 } else { 2 },
            "{receipt}"
        );
        if mode == 10 {
            assert_eq!(receipt["error_code"], "release_changed");
        }
        assert!(
            state.adds.lock().unwrap().is_empty(),
            "invalid or changed release bytes never permit an alternate POST"
        );
        assert_eq!(
            state.torrent_requests.load(Ordering::SeqCst),
            if mode == 9 { 1 } else { 3 }
        );
        let c = db.connect().await.unwrap();
        assert_eq!(
            c.query(
                "SELECT private_payload IS NULL FROM rss_candidates WHERE id=?",
                [receipt["id"].as_str().unwrap()]
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<i64>(0)
            .unwrap(),
            1,
            "terminal settlement erases credentials-bearing retained release"
        );
        runtime.shutdown().await;
    }
}

#[tokio::test]
async fn default_tv_post_add_read_failure_retains_original_client_without_alternate_post() {
    let scratch = Scratch(std::env::temp_dir().join(format!(
        "hrrdarr-selection-read-failure-{}",
        uuid::Uuid::new_v4()
    )));
    std::fs::create_dir(&scratch.0).unwrap();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    seed(&db).await;
    let first = Arc::new(Remote::default());
    first.mode.store(11, Ordering::SeqCst);
    let alternative = Arc::new(Remote::default());
    let (first_base, _first_server) = serve(
        axum::Router::new()
            .fallback(remote)
            .with_state(first.clone()),
    )
    .await;
    let (alternative_base, _alternative_server) = serve(
        axum::Router::new()
            .fallback(remote)
            .with_state(alternative.clone()),
    )
    .await;
    let (base, _server, client) = app(db.clone()).await;
    let (indexer, download) = providers(&base, &first_base).await;
    let (_, other) = providers(&base, &alternative_base).await;
    let (code,other)=request(&base,"PUT",&format!("/api/v1/providers/{}",other["id"].as_str().unwrap()),json!({"revision":other["revision"],"name":"alternate","enabled":true,"priority":50,"settings":other["settings"]})).await;
    assert_eq!(code, 200, "{other}");
    db.connect()
        .await
        .unwrap()
        .execute(
            "UPDATE download_refresh_schedules SET next_run_at=unixepoch()+3600",
            (),
        )
        .await
        .unwrap();
    let command = enqueue(
        &base,
        selection_target(&indexer, "tv", json!({"kind":"default"})),
    )
    .await;
    let runtime = commands::start(db.clone(), client).await.unwrap();
    assert_eq!(until(&base, &command["id"]).await["status"], "failed");
    let receipt = receipt_until(&base, "reconciling").await;
    assert_eq!(receipt["source"]["client_id"], download["id"]);
    assert_eq!(receipt["client_attempts"].as_array().unwrap().len(), 1);
    assert_eq!(receipt["client_attempts"][0]["state"], "unknown");
    assert_eq!(first.adds.lock().unwrap().len(), 1);
    assert!(alternative.adds.lock().unwrap().is_empty());
    first.mode.store(0, Ordering::SeqCst);
    // Reconciliation owns its attempt/deadline transition. Wait for the real
    // thirty-second retry instead of editing that invariant in the fixture.
    let settled = tokio::time::timeout(hang(45), async {
        loop {
            let (_, page) = request(&base, "GET", "/api/v1/rss/candidates", Value::Null).await;
            let current = page["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["id"] == receipt["id"])
                .unwrap();
            if current["status"] == "needs_attention" {
                break current.clone();
            }
            assert_eq!(current["status"], "reconciling", "{current}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("real reconciliation retry deadline");
    assert_eq!(settled["error_code"], "presence_unconfirmed");
    assert_eq!(settled["source"]["client_id"], download["id"]);
    assert_eq!(settled["client_attempts"].as_array().unwrap().len(), 1);
    assert_eq!(first.adds.lock().unwrap().len(), 1);
    assert!(alternative.adds.lock().unwrap().is_empty());
    runtime.shutdown().await;
}

#[tokio::test]
async fn bound_movie_default_does_not_consume_unbound_worker_rotation() {
    let scratch = Scratch(std::env::temp_dir().join(format!(
        "hrrdarr-selection-rotation-{}",
        uuid::Uuid::new_v4()
    )));
    std::fs::create_dir(&scratch.0).unwrap();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    seed(&db).await;
    let left = Arc::new(Remote::default());
    let right = Arc::new(Remote::default());
    let (left_base, _left_server) = serve(
        axum::Router::new()
            .fallback(remote)
            .with_state(left.clone()),
    )
    .await;
    let (right_base, _right_server) = serve(
        axum::Router::new()
            .fallback(remote)
            .with_state(right.clone()),
    )
    .await;
    let (base, _server, client) = app(db.clone()).await;
    let (mut indexer, left_client) = providers(&base, &left_base).await;
    let (_, right_client) = providers(&base, &right_base).await;
    let (first, second) =
        if left_client["id"].as_str().unwrap() < right_client["id"].as_str().unwrap() {
            (&left_client, &right_client)
        } else {
            (&right_client, &left_client)
        };
    // Three distinct owned movie targets avoid reusing any receipt or torrent hash.
    db.connect().await.unwrap().execute_batch("INSERT INTO movie_metadata(id,tmdb_id,title,year,runtime,digital_release)VALUES(2,202,'Beacon',2020,100,'2020-01-01 00:00:00'),(3,203,'Cedar',2020,100,'2020-01-01 00:00:00'); INSERT INTO movies(id,metadata_id,path)VALUES(2,2,'/owned-beacon'),(3,3,'/owned-cedar'); INSERT INTO library_settings(media_type,movie_id,quality_profile_id,minimum_availability)VALUES('movies',2,2,'released'),('movies',3,2,'released'); UPDATE download_refresh_schedules SET next_run_at=unixepoch()+3600;").await.unwrap();
    let runtime = commands::start(db.clone(), client).await.unwrap();
    for step in 0..3 {
        if step == 1 {
            indexer = bind_client(&base, &indexer, "movies", second).await;
            *left.feed_override.lock().unwrap() =
                Some(("Beacon.2020.1080p.WEB-DL".into(), "3".repeat(40)));
        } else if step == 2 {
            let mut settings = indexer["settings"].clone();
            settings["movies"]["download_client_id"] = Value::Null;
            let (code,updated)=request(&base,"PUT",&format!("/api/v1/providers/{}",indexer["id"].as_str().unwrap()),json!({"revision":indexer["revision"],"name":"unbound","enabled":true,"priority":1,"settings":settings})).await;
            assert_eq!(code, 200, "{updated}");
            indexer = updated;
            *left.feed_override.lock().unwrap() =
                Some(("Cedar.2020.1080p.WEB-DL".into(), "4".repeat(40)));
        }
        let command = enqueue(
            &base,
            selection_target(&indexer, "movies", json!({"kind":"default"})),
        )
        .await;
        let done = until(&base, &command["id"]).await;
        assert_eq!(done["observed"], 1, "{done}");
        let (_, receipts) = request(
            &base,
            "GET",
            &format!(
                "/api/v1/rss/candidates?command_id={}",
                command["id"].as_str().unwrap()
            ),
            Value::Null,
        )
        .await;
        let expected = if step == 0 { first } else { second };
        assert_eq!(
            receipts["items"][0]["source"]["client_id"], expected["id"],
            "bound choice must not consume the next unbound rotation turn: step={step}"
        );
        assert_eq!(
            receipts["items"][0]["client_attempts"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
    let first_remote = if first["id"] == left_client["id"] {
        &left
    } else {
        &right
    };
    let second_remote = if second["id"] == left_client["id"] {
        &left
    } else {
        &right
    };
    assert_eq!(first_remote.adds.lock().unwrap().as_slice(), [MOVIE]);
    assert_eq!(
        second_remote.adds.lock().unwrap().as_slice(),
        ["3".repeat(40), "4".repeat(40)]
    );
    runtime.shutdown().await;
}

#[tokio::test]
async fn worker_rotation_advances_movies_on_choice_and_tv_only_on_success() {
    for (media, failed) in [("movies", true), ("tv", true), ("tv", false)] {
        let scratch = Scratch(
            std::env::temp_dir().join(format!("hrrdarr-worker-rotation-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir(&scratch.0).unwrap();
        let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
        seed(&db).await;
        let left = Arc::new(Remote::default());
        let right = Arc::new(Remote::default());
        if failed {
            left.mode.store(12, Ordering::SeqCst);
            right.mode.store(12, Ordering::SeqCst);
        }
        *right.feed_override.lock().unwrap() = Some((
            if media == "tv" {
                "Beacon.S01E01.1080p.WEB-DL"
            } else {
                "Beacon.2020.1080p.WEB-DL"
            }
            .into(),
            "3".repeat(40),
        ));
        let (left_base, _left_server) = serve(
            axum::Router::new()
                .fallback(remote)
                .with_state(left.clone()),
        )
        .await;
        let (right_base, _right_server) = serve(
            axum::Router::new()
                .fallback(remote)
                .with_state(right.clone()),
        )
        .await;
        let (base, _server, client) = app(db.clone()).await;
        let (left_indexer, left_client) = providers(&base, &left_base).await;
        let (right_indexer, right_client) = providers(&base, &right_base).await;
        let mut sorted = [
            left_client["id"].as_str().unwrap().to_owned(),
            right_client["id"].as_str().unwrap().to_owned(),
        ];
        sorted.sort();
        // Distinct real library targets/indexer feeds prevent receipt deduplication
        // and target contention from obscuring the worker's client rotation.
        db.connect().await.unwrap().execute_batch("INSERT INTO series(id,tvdb_id,title,path)VALUES(2,102,'Beacon','/owned-beacon-tv'); INSERT INTO seasons(series_id,number)VALUES(2,1); INSERT INTO episodes(id,series_id,season,number,title,runtime,air_date_utc)VALUES(2,2,1,1,'Pilot',45,'2020-01-01 00:00:00'); INSERT INTO library_settings(media_type,series_id,quality_profile_id,series_type,use_scene_numbering)VALUES('tv',2,1,'standard',0); INSERT INTO movie_metadata(id,tmdb_id,title,year,runtime,digital_release)VALUES(2,202,'Beacon',2020,100,'2020-01-01 00:00:00'); INSERT INTO movies(id,metadata_id,path)VALUES(2,2,'/owned-beacon-movie'); INSERT INTO library_settings(media_type,movie_id,quality_profile_id,minimum_availability)VALUES('movies',2,2,'released'); UPDATE download_refresh_schedules SET next_run_at=unixepoch()+3600;").await.unwrap();
        let runtime = commands::start(db.clone(), client).await.unwrap();
        let left_target = selection_target(&left_indexer, media, json!({"kind":"default"}));
        let right_target = selection_target(&right_indexer, media, json!({"kind":"default"}));
        if failed {
            let (left_command, right_command) =
                tokio::join!(enqueue(&base, left_target), enqueue(&base, right_target));
            until(&base, &left_command["id"]).await;
            until(&base, &right_command["id"]).await;
            let receipts = tokio::time::timeout(hang(12), async {
                loop {
                    let (_, page) =
                        request(&base, "GET", "/api/v1/rss/candidates", Value::Null).await;
                    let items = page["items"].as_array().unwrap();
                    // TV all-safe exhaustion now retains unavailable work; generic
                    // movie probe failure still terminates without retry authority.
                    if items.len() == 2
                        && items.iter().all(|v| {
                            if media == "tv" {
                                v["status"] == "pending"
                                    && v["error_code"] == "download_client_unavailable"
                            } else {
                                v["status"] == "rejected"
                            }
                        })
                    {
                        break items.clone();
                    }
                    tokio::time::sleep(Duration::from_millis(30)).await;
                }
            })
            .await
            .unwrap();
            let mut movie_choices = Vec::new();
            for receipt in receipts {
                let attempts = receipt["client_attempts"].as_array().unwrap();
                assert!(
                    attempts
                        .iter()
                        .all(|v| v["state"] == "definitely_not_submitted"),
                    "{receipt}"
                );
                let chosen = attempts
                    .iter()
                    .map(|v| v["client_id"].as_str().unwrap().to_owned())
                    .collect::<Vec<_>>();
                if media == "movies" {
                    assert_eq!(chosen.len(), 1);
                    movie_choices.extend(chosen);
                } else {
                    assert_eq!(
                        chosen, sorted,
                        "failed TV attempts cannot consume the success cursor"
                    );
                }
            }
            if media == "movies" {
                movie_choices.sort();
                assert_eq!(
                    movie_choices, sorted,
                    "concurrent movie admissions consume distinct turns even when both chosen clients fail"
                );
            }
            assert!(left.adds.lock().unwrap().is_empty());
            assert!(right.adds.lock().unwrap().is_empty());
        } else {
            // TV advances after accepted success, so admit the second distinct
            // target only once the first remote add has an observed receipt.
            for (ordinal, target) in [left_target, right_target].into_iter().enumerate() {
                let command = enqueue(&base, target).await;
                assert_eq!(until(&base, &command["id"]).await["observed"], 1);
                let (_, page) = request(
                    &base,
                    "GET",
                    &format!(
                        "/api/v1/rss/candidates?command_id={}",
                        command["id"].as_str().unwrap()
                    ),
                    Value::Null,
                )
                .await;
                assert_eq!(page["items"][0]["source"]["client_id"], sorted[ordinal]);
                assert_eq!(
                    page["items"][0]["client_attempts"]
                        .as_array()
                        .unwrap()
                        .len(),
                    1
                );
            }
            assert_eq!(left.adds.lock().unwrap().len(), 1);
            assert_eq!(right.adds.lock().unwrap().len(), 1);
        }
        runtime.shutdown().await;
    }
}

// Wait for a real worker failure transition, not the initially admitted pending row.
async fn unavailable_receipt(base: &str) -> Value {
    tokio::time::timeout(hang(18), async {
        loop {
            let (_, page) = request(base, "GET", "/api/v1/rss/candidates", Value::Null).await;
            if let Some(row) = page["items"].as_array().unwrap().iter().find(|r| {
                r["status"] == "pending" && r["error_code"] == "download_client_unavailable"
            }) {
                return row.clone();
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .expect("unavailable receipt deadline")
}

#[tokio::test]
async fn default_safe_retry_rounds_reopen_same_candidate_and_preserve_payload_authority() {
    for (media, automatic_search, changed_bytes) in [
        ("tv", false, false),
        ("tv", true, false),
        ("movies", false, false),
        ("movies", true, false),
        ("tv", false, true),
    ] {
        let scratch = Scratch(
            std::env::temp_dir().join(format!("hrrdarr-selection-rounds-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir(&scratch.0).unwrap();
        let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
        seed(&db).await;
        let feed = Arc::new(Remote::default());
        let peer = Arc::new(Remote::default());
        peer.mode
            .store(if media == "tv" { 12 } else { 13 }, Ordering::SeqCst);
        if changed_bytes {
            feed.mode.store(10, Ordering::SeqCst);
        }
        let (feed_base, _feed_server) = serve(
            axum::Router::new()
                .fallback(remote)
                .with_state(feed.clone()),
        )
        .await;
        *feed.endpoint.lock().unwrap() = feed_base.clone();
        let (peer_base, _peer_server) = serve(
            axum::Router::new()
                .fallback(remote)
                .with_state(peer.clone()),
        )
        .await;
        let (base, server, client) = app(db.clone()).await;
        let (indexer, mut download) = providers(&base, &feed_base).await;
        let mut settings = download["settings"].clone();
        settings["endpoint"] = json!(peer_base);
        let (code,updated)=request(&base,"PUT",&format!("/api/v1/providers/{}",download["id"].as_str().unwrap()),json!({"revision":download["revision"],"name":"round client","enabled":true,"priority":1,"settings":settings,"credentials":{"kind":"username_password","username":"owned","password":"owned"}})).await;
        assert_eq!(code, 200, "{updated}");
        download = updated;
        // A bound singleton exercises safe exhaustion without introducing an
        // alternate client or changing the original target's ownership.
        let indexer = bind_client(&base, &indexer, media, &download).await;
        let c = db.connect().await.unwrap();
        c.execute(
            "UPDATE download_refresh_schedules SET next_run_at=unixepoch()+3600",
            (),
        )
        .await
        .unwrap();
        let search_command = if automatic_search {
            if media == "tv" {
                c.execute("UPDATE series SET monitored=0", ())
                    .await
                    .unwrap();
            } else {
                c.execute(
                    "UPDATE movie_metadata SET digital_release='2099-01-01 00:00:00'",
                    (),
                )
                .await
                .unwrap();
            }
            let (code,command)=request(&base,"POST","/api/v1/search/commands",json!({"request_id":uuid::Uuid::new_v4(),"mode":"automatic","target":{"media_type":if media=="tv" {"episode"}else{"movie"},"id":1},"indexer_id":indexer["id"],"indexer_revision":indexer["revision"],"client_selection":{"kind":"default"},"priority":"normal"})).await;
            assert_eq!(code, 202, "{command}");
            Some(command)
        } else {
            enqueue(
                &base,
                selection_target(&indexer, media, json!({"kind":"default"})),
            )
            .await;
            None
        };
        let runtime = commands::start(db.clone(), client).await.unwrap();
        let waiting = unavailable_receipt(&base).await;
        let id = waiting["id"].as_str().unwrap();
        assert_eq!(
            waiting["client_attempts"].as_array().unwrap().len(),
            1,
            "{waiting}"
        );
        assert_eq!(waiting["client_attempts"][0]["selection_round"], 1);
        assert_eq!(
            waiting["client_attempts"][0]["state"],
            "definitely_not_submitted"
        );
        assert!(waiting["source"]["client_id"].is_null());
        assert!(peer.adds.lock().unwrap().is_empty());
        let row=c.query("SELECT private_payload,payload_expires_at,selection_round,selection_strict,selection_plan_json FROM rss_candidates WHERE id=?",[id]).await.unwrap().next().await.unwrap().unwrap();
        let encrypted = row.get::<Vec<u8>>(0).unwrap();
        let expiry = row.get::<i64>(1).unwrap();
        assert_eq!(row.get::<i64>(2).unwrap(), 1);
        assert_eq!(row.get::<i64>(3).unwrap(), 1);
        assert!(row.get::<Option<String>>(4).unwrap().is_none());
        drop(row);
        runtime.shutdown().await;
        server.stop().await;
        // Only an elapsed pending deadline is seeded. Every attempt and remote
        // observation is produced by the actual worker before/after reopening.
        c.execute("UPDATE rss_candidates SET not_before=0 WHERE id=?", [id])
            .await
            .unwrap();
        peer.mode.store(0, Ordering::SeqCst);
        drop(c);
        drop(db);
        let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
        let c = db.connect().await.unwrap();
        let row = c
            .query(
                "SELECT private_payload,payload_expires_at FROM rss_candidates WHERE id=?",
                [id],
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.get::<Vec<u8>>(0).unwrap(), encrypted);
        assert_eq!(row.get::<i64>(1).unwrap(), expiry);
        drop(row);
        let (base, _server, client) = app(db.clone()).await;
        let runtime = commands::start(db.clone(), client).await.unwrap();
        let receipt = receipt_until(
            &base,
            if changed_bytes {
                "rejected"
            } else {
                "observed"
            },
        )
        .await;
        assert_eq!(receipt["id"], waiting["id"]);
        let attempts = receipt["client_attempts"].as_array().unwrap();
        assert_eq!(attempts.len(), 2, "{receipt}");
        assert_eq!(attempts[0]["ordinal"], 1);
        assert_eq!(attempts[1]["ordinal"], 2);
        assert_eq!(attempts[1]["selection_round"], 2);
        assert!(attempts.iter().all(|a| a["client_id"] == download["id"]));
        if changed_bytes {
            assert_eq!(receipt["error_code"], "release_changed");
            assert!(peer.adds.lock().unwrap().is_empty());
        } else {
            assert_eq!(receipt["source"]["client_id"], download["id"]);
            assert_eq!(peer.adds.lock().unwrap().len(), 1);
        }
        let row=c.query("SELECT private_payload,payload_expires_at,selection_round FROM rss_candidates WHERE id=?",[id]).await.unwrap().next().await.unwrap().unwrap();
        assert!(row.get::<Option<Vec<u8>>>(0).unwrap().is_none());
        assert_eq!(row.get::<i64>(1).unwrap(), expiry);
        assert_eq!(row.get::<i64>(2).unwrap(), 2);
        if let Some(command) = search_command {
            let (_, summary) = request(
                &base,
                "GET",
                &format!(
                    "/api/v1/search/commands/{}",
                    command["id"].as_str().unwrap()
                ),
                Value::Null,
            )
            .await;
            assert_eq!(summary["selected_client_id"], download["id"]);
            assert_eq!(
                summary["selected_client_attempts"],
                receipt["client_attempts"]
            );
            assert_eq!(receipt["origin"]["mode"], "automatic");
        }
        runtime.shutdown().await;
    }
}

#[tokio::test]
async fn unavailable_protocol_parks_only_qualified_siblings_in_its_rss_batch() {
    let scratch = Scratch(
        std::env::temp_dir().join(format!("hrrdarr-selection-batch-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&scratch.0).unwrap();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    seed(&db).await;
    let failed = Arc::new(Remote::default());
    failed.mode.store(12, Ordering::SeqCst);
    let healthy = Arc::new(Remote::default());
    let (failed_base, _failed_server) = serve(
        axum::Router::new()
            .fallback(remote)
            .with_state(failed.clone()),
    )
    .await;
    let (healthy_base, _healthy_server) = serve(
        axum::Router::new()
            .fallback(remote)
            .with_state(healthy.clone()),
    )
    .await;
    let published = chrono::Utc::now().to_rfc2822();
    for (episode, date) in [
        (2, "Mon, 01 Jan 2024 12:00:00 +0000"),
        (3, published.as_str()),
    ] {
        let hash = format!("{episode:040}");
        failed.extra_feed.lock().unwrap().push_str(&format!(r#"<item><title>Harbor.S01E0{episode}.1080p.WEB-DL</title><guid>PRIVATE_BATCH_{episode}</guid><pubDate>{date}</pubDate><link>magnet:?xt=urn:btih:{hash}</link><torznab:attr name="magneturl" value="magnet:?xt=urn:btih:{hash}"/><torznab:attr name="category" value="5030"/><torznab:attr name="size" value="1073741824"/></item>"#));
    }
    let (base, server, client) = app(db.clone()).await;
    let (indexer, download) = providers(&base, &failed_base).await;
    let (other_indexer, other_client) = providers(&base, &healthy_base).await;
    let indexer = bind_client(&base, &indexer, "tv", &download).await;
    let other_indexer = bind_client(&base, &other_indexer, "movies", &other_client).await;
    let c = db.connect().await.unwrap();
    c.execute_batch("INSERT INTO episodes(id,series_id,season,number,title,runtime,air_date_utc) VALUES(2,1,1,2,'Second',45,'2020-01-01 00:00:00'),(3,1,1,3,'Third',45,'2020-01-01 00:00:00'); UPDATE download_refresh_schedules SET next_run_at=unixepoch()+3600;").await.unwrap();
    save_delay(&base, "tv", 60).await;
    let batch = enqueue(
        &base,
        selection_target(&indexer, "tv", json!({"kind":"default"})),
    )
    .await;
    let independent = enqueue(
        &base,
        selection_target(&other_indexer, "movies", json!({"kind":"default"})),
    )
    .await;
    let runtime = commands::start(db.clone(), client).await.unwrap();
    until(&base, &batch["id"]).await;
    assert_eq!(until(&base, &independent["id"]).await["observed"], 1);
    let (_, page) = request(
        &base,
        "GET",
        &format!(
            "/api/v1/rss/candidates?command_id={}",
            batch["id"].as_str().unwrap()
        ),
        Value::Null,
    )
    .await;
    let rows = page["items"].as_array().unwrap();
    assert_eq!(rows.len(), 3, "{page}");
    let qualified = rows
        .iter()
        .filter(|r| !r["title"].as_str().unwrap().contains("E03"))
        .collect::<Vec<_>>();
    assert!(
        qualified
            .iter()
            .all(|r| r["status"] == "pending" && r["error_code"] == "download_client_unavailable"),
        "{page}"
    );
    assert_eq!(
        qualified
            .iter()
            .map(|r| r["client_attempts"].as_array().unwrap().len())
            .sum::<usize>(),
        1,
        "the second qualified release must not make another client attempt"
    );
    let delayed = rows
        .iter()
        .find(|r| r["title"].as_str().unwrap().contains("E03"))
        .unwrap();
    assert_eq!(delayed["status"], "pending");
    assert!(delayed["error_code"].is_null());
    assert!(delayed["client_attempts"].as_array().unwrap().is_empty());
    assert!(!delayed["reasons"].as_array().unwrap().is_empty());
    assert!(delayed["not_before"].as_i64().unwrap() > chrono::Utc::now().timestamp() + 3000);
    assert!(failed.adds.lock().unwrap().is_empty());
    assert_eq!(healthy.adds.lock().unwrap().len(), 1);
    runtime.shutdown().await;
    server.stop().await;
    drop(c);
    drop(db);
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let (base, _server, client) = app(db.clone()).await;
    let runtime = commands::start(db.clone(), client).await.unwrap();
    let (_, reopened) = request(
        &base,
        "GET",
        &format!(
            "/api/v1/rss/candidates?command_id={}",
            batch["id"].as_str().unwrap()
        ),
        Value::Null,
    )
    .await;
    assert_eq!(
        reopened, page,
        "future unavailable/delay deadlines survive reopening unchanged"
    );
    runtime.shutdown().await;
}

#[tokio::test]
async fn interactive_default_failure_does_not_acquire_automatic_retry_authority() {
    for (media, no_client) in [("tv", false), ("tv", true), ("movies", true)] {
        let scratch = Scratch(
            std::env::temp_dir().join(format!("hrrdarr-direct-selection-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir(&scratch.0).unwrap();
        let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
        seed(&db).await;
        let peer = Arc::new(Remote::default());
        peer.mode.store(12, Ordering::SeqCst);
        let (peer_base, _peer_server) = serve(
            axum::Router::new()
                .fallback(remote)
                .with_state(peer.clone()),
        )
        .await;
        let (base, _server, client) = app(db.clone()).await;
        let (indexer, download) = providers(&base, &peer_base).await;
        let c = db.connect().await.unwrap();
        c.execute(
            "UPDATE download_refresh_schedules SET next_run_at=unixepoch()+3600",
            (),
        )
        .await
        .unwrap();
        if no_client {
            c.execute(
                "INSERT INTO tags(id,media_type,label)VALUES(1,?,'exclusive')",
                [media],
            )
            .await
            .unwrap();
            c.execute(
                "INSERT INTO download_client_tags(provider_id,media_type,tag_id)VALUES(?,?,1)",
                libsql::params![download["id"].as_str().unwrap(), media],
            )
            .await
            .unwrap();
        }
        let (code,command)=request(&base,"POST","/api/v1/search/commands",json!({"request_id":uuid::Uuid::new_v4(),"mode":"interactive","target":{"media_type":if media=="tv" {"episode"}else{"movie"},"id":1},"indexer_id":indexer["id"],"indexer_revision":indexer["revision"],"client_selection":{"kind":"default"},"priority":"normal"})).await;
        assert_eq!(code, 202, "{command}");
        let runtime = commands::start(db.clone(), client).await.unwrap();
        let result = tokio::time::timeout(hang(18), async {
            loop {
                let (_, results) = request(
                    &base,
                    "GET",
                    &format!(
                        "/api/v1/search/commands/{}/results",
                        command["id"].as_str().unwrap()
                    ),
                    Value::Null,
                )
                .await;
                if let Some(result) = results["items"].as_array().unwrap().first() {
                    break result.clone();
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .unwrap();
        let (code, selected) = request(
            &base,
            "POST",
            &format!(
                "/api/v1/search/results/{}/grab",
                result["id"].as_str().unwrap()
            ),
            json!({}),
        )
        .await;
        assert!((200..300).contains(&code), "{selected}");
        let receipt = receipt_until(&base, "rejected").await;
        assert_eq!(receipt["id"], selected["id"]);
        assert_eq!(receipt["origin"]["mode"], "interactive");
        assert_eq!(
            receipt["client_attempts"].as_array().unwrap().len(),
            usize::from(!no_client)
        );
        let row=c.query("SELECT selection_strict,private_payload,selection_round FROM rss_candidates WHERE id=?",[receipt["id"].as_str().unwrap()]).await.unwrap().next().await.unwrap().unwrap();
        assert_eq!(row.get::<i64>(0).unwrap(), 0);
        assert!(row.get::<Option<Vec<u8>>>(1).unwrap().is_none());
        assert_eq!(row.get::<i64>(2).unwrap(), i64::from(!no_client));
        assert!(peer.adds.lock().unwrap().is_empty());
        runtime.shutdown().await;
    }
}

#[tokio::test]
async fn selection_capacity_change_during_preparation_is_terminal_without_fallback() {
    for media in ["tv", "movies"] {
        let scratch = Scratch(std::env::temp_dir().join(format!(
            "hrrdarr-revalidation-capacity-{}",
            uuid::Uuid::new_v4()
        )));
        std::fs::create_dir(&scratch.0).unwrap();
        let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
        seed(&db).await;
        let selected = Arc::new(Remote::default());
        selected.mode.store(4, Ordering::SeqCst);
        selected.torrent_barrier_limit.store(1, Ordering::SeqCst);
        let alternative = Arc::new(Remote::default());
        let (selected_base, _selected_server) = serve(
            axum::Router::new()
                .fallback(remote)
                .with_state(selected.clone()),
        )
        .await;
        *selected.endpoint.lock().unwrap() = selected_base.clone();
        let (alternative_base, _alternative_server) = serve(
            axum::Router::new()
                .fallback(remote)
                .with_state(alternative.clone()),
        )
        .await;
        let (base, _server, client) = app(db.clone()).await;
        let (indexer, download) = providers(&base, &selected_base).await;
        let (_, alternate) = providers(&base, &alternative_base).await;
        let (code,alternate)=request(&base,"PUT",&format!("/api/v1/providers/{}",alternate["id"].as_str().unwrap()),json!({"revision":alternate["revision"],"name":"alternate","enabled":true,"priority":50,"settings":alternate["settings"]})).await;
        assert_eq!(code, 200, "{alternate}");
        let c = db.connect().await.unwrap();
        c.execute(
            "UPDATE download_refresh_schedules SET next_run_at=unixepoch()+3600",
            (),
        )
        .await
        .unwrap();
        let command = enqueue(
            &base,
            selection_target(&indexer, media, json!({"kind":"default"})),
        )
        .await;
        let runtime = commands::start(db.clone(), client).await.unwrap();
        tokio::time::timeout(hang(12), selected.started.notified())
            .await
            .unwrap();
        // Real selection captured the two-client configuration. While its first
        // torrent fetch is paused, add valid imported client configurations to
        // cross the selector's 128-client bound. No receipts, attempts, schedules
        // or invariant triggers are fabricated/removed by this fixture seam.
        let tx = c
            .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
            .await
            .unwrap();
        for _ in 0..127 {
            let id = uuid::Uuid::new_v4().to_string();
            tx.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint)VALUES(?,'qbittorrent',?,1,50,1,1,?)",libsql::params![id.clone(),id.clone(),alternative_base.clone()]).await.unwrap();
            tx.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags)VALUES(?,'qbittorrent',?,?,0,0,'started','default',0,0,0)",libsql::params![id,media,media]).await.unwrap();
        }
        tx.commit().await.unwrap();
        selected.release.notify_one();
        let receipt = receipt_until(&base, "rejected").await;
        assert_eq!(receipt["error_code"], "refresh_limit", "{receipt}");
        assert_eq!(receipt["source"]["client_id"], download["id"]);
        let attempts = receipt["client_attempts"].as_array().unwrap();
        assert_eq!(attempts.len(), 1, "{receipt}");
        assert_eq!(attempts[0]["state"], "definitely_not_submitted");
        assert_eq!(attempts[0]["error_code"], "refresh_limit");
        assert_eq!(attempts[0]["selection_round"], 1);
        let row=c.query("SELECT selection_strict,private_payload,not_before,(SELECT identity_json IS NOT NULL FROM rss_client_attempts WHERE candidate_id=c.id AND ordinal=1),(SELECT count(*) FROM rss_hash_claims WHERE candidate_id=c.id) FROM rss_candidates c WHERE id=?",[receipt["id"].as_str().unwrap()]).await.unwrap().next().await.unwrap().unwrap();
        assert_eq!(row.get::<i64>(0).unwrap(), 0);
        assert!(row.get::<Option<Vec<u8>>>(1).unwrap().is_none());
        assert!(row.get::<Option<i64>>(2).unwrap().is_none());
        assert_eq!(
            row.get::<i64>(3).unwrap(),
            1,
            "retain the fetched identity even when capacity aborts the selected attempt"
        );
        assert_eq!(row.get::<i64>(4).unwrap(), 0);
        assert_eq!(selected.torrent_requests.load(Ordering::SeqCst), 1);
        assert!(selected.adds.lock().unwrap().is_empty());
        assert!(alternative.adds.lock().unwrap().is_empty());
        assert_eq!(until(&base, &command["id"]).await["status"], "succeeded");
        runtime.shutdown().await;
    }
}

// A view preserves every invariant-bearing table/trigger while producing a real
// SQLite lookup error only when the selected provider column is actually read.
async fn install_provider_lookup_fault(c: &libsql::Connection, column: &str, provider_id: &str) {
    assert!(matches!(column, "endpoint" | "revision"));
    let mut rows = c.query("PRAGMA table_info(providers)", ()).await.unwrap();
    let mut columns = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        let name = row.get::<String>(1).unwrap();
        columns.push(if name==column {format!("CASE WHEN (SELECT enabled FROM provider_lookup_fault)=1 AND id=(SELECT provider_id FROM provider_lookup_fault) THEN abs(-9223372036854775808) ELSE {name} END AS {name}")} else {name});
    }
    drop(rows);
    // Scope the injected fault to the selected client. Failing every endpoint
    // would stop the indexer's feed capture before preparation is ever reached.
    c.execute_batch(
        "CREATE TABLE provider_lookup_fault(enabled INTEGER NOT NULL, provider_id TEXT NOT NULL)",
    )
    .await
    .unwrap();
    c.execute(
        "INSERT INTO provider_lookup_fault VALUES(0,?)",
        [provider_id],
    )
    .await
    .unwrap();
    c.execute_batch(&format!("ALTER TABLE providers RENAME TO provider_lookup_source; CREATE VIEW providers AS SELECT {} FROM provider_lookup_source;",columns.join(","))).await.unwrap();
}
async fn remove_provider_lookup_fault(c: &libsql::Connection) {
    c.execute_batch("DROP VIEW providers; ALTER TABLE provider_lookup_source RENAME TO providers; DROP TABLE provider_lookup_fault;").await.unwrap();
}

#[tokio::test]
async fn preparation_local_storage_failure_preserves_attempt_and_fetched_byte_anchor() {
    for (media, after_fetch, changed_bytes) in [
        ("tv", false, false),
        ("movies", false, false),
        ("tv", true, false),
        ("movies", true, false),
        ("tv", true, true),
        ("movies", true, true),
    ] {
        eprintln!(
            "preparation storage case media={media} after_fetch={after_fetch} changed_bytes={changed_bytes}"
        );
        let scratch = Scratch(std::env::temp_dir().join(format!(
            "hrrdarr-preparation-storage-{}",
            uuid::Uuid::new_v4()
        )));
        std::fs::create_dir(&scratch.0).unwrap();
        let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
        seed(&db).await;
        let peer = Arc::new(Remote::default());
        peer.mode.store(4, Ordering::SeqCst);
        peer.torrent_barrier_limit.store(1, Ordering::SeqCst);
        if changed_bytes {
            peer.torrent_change_after.store(1, Ordering::SeqCst);
        }
        let (peer_base, _peer_server) = serve(
            axum::Router::new()
                .fallback(remote)
                .with_state(peer.clone()),
        )
        .await;
        *peer.endpoint.lock().unwrap() = peer_base.clone();
        let (base, _server, client) = app(db.clone()).await;
        let (indexer, download) = providers(&base, &peer_base).await;
        let c = db.connect().await.unwrap();
        c.execute(
            "UPDATE download_refresh_schedules SET next_run_at=unixepoch()+3600",
            (),
        )
        .await
        .unwrap();
        let command = enqueue(
            &base,
            selection_target(&indexer, media, json!({"kind":"default"})),
        )
        .await;
        install_provider_lookup_fault(
            &c,
            if after_fetch { "revision" } else { "endpoint" },
            download["id"].as_str().unwrap(),
        )
        .await;
        if !after_fetch {
            c.execute("UPDATE provider_lookup_fault SET enabled=1", ())
                .await
                .unwrap();
        }
        let runtime = commands::start(db.clone(), client).await.unwrap();
        if after_fetch {
            tokio::time::timeout(hang(12), peer.started.notified())
                .await
                .unwrap();
            c.execute("UPDATE provider_lookup_fault SET enabled=1", ())
                .await
                .unwrap();
            peer.release.notify_one();
        }
        tokio::time::timeout(hang(12), async {
            loop {
                if c.query(
                    "SELECT 1 FROM rss_commands WHERE id=? AND error_code='storage_error'",
                    [command["id"].as_str().unwrap()],
                )
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .is_some()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .expect("local preparation lookup failure must propagate to command retry");
        let row=c.query("SELECT id,private_payload,payload_expires_at,selection_round,active_attempt,client_id,status,selection_strict FROM rss_candidates WHERE command_id=?",[command["id"].as_str().unwrap()]).await.unwrap().next().await.unwrap().unwrap();
        let id = row.get::<String>(0).unwrap();
        let payload = row.get::<Vec<u8>>(1).unwrap();
        let expiry = row.get::<i64>(2).unwrap();
        assert_eq!(row.get::<i64>(3).unwrap(), 1);
        assert_eq!(row.get::<i64>(4).unwrap(), 1);
        assert_eq!(
            row.get::<String>(5).unwrap(),
            download["id"].as_str().unwrap()
        );
        assert_eq!(row.get::<String>(6).unwrap(), "pending");
        assert_eq!(row.get::<i64>(7).unwrap(), 0);
        drop(row);
        let row=c.query("SELECT state,identity_json,count(*) OVER() FROM rss_client_attempts WHERE candidate_id=?",[id.as_str()]).await.unwrap().next().await.unwrap().unwrap();
        assert_eq!(
            row.get::<String>(0).unwrap(),
            if after_fetch { "prepared" } else { "selected" }
        );
        assert_eq!(row.get::<Option<String>>(1).unwrap().is_some(), after_fetch);
        assert_eq!(row.get::<i64>(2).unwrap(), 1);
        drop(row);
        assert!(peer.adds.lock().unwrap().is_empty());
        assert_eq!(
            peer.torrent_requests.load(Ordering::SeqCst),
            u8::from(after_fetch)
        );
        assert_eq!(
            c.query("SELECT count(*) FROM download_client_status", ())
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap()
                .get::<i64>(0)
                .unwrap(),
            0
        );
        remove_provider_lookup_fault(&c).await;
        // Only the command retry deadline is elapsed; attempt/round/payload facts
        // remain exclusively worker-owned throughout the real storage fault.
        c.execute(
            "UPDATE rss_commands SET next_attempt_at=0 WHERE id=?",
            [command["id"].as_str().unwrap()],
        )
        .await
        .unwrap();
        let row = c
            .query(
                "SELECT private_payload,payload_expires_at FROM rss_candidates WHERE id=?",
                [id.as_str()],
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.get::<Vec<u8>>(0).unwrap(), payload);
        assert_eq!(row.get::<i64>(1).unwrap(), expiry);
        drop(row);
        if !after_fetch {
            tokio::time::timeout(hang(12), peer.started.notified())
                .await
                .unwrap();
            peer.release.notify_one();
        }
        let receipt = receipt_until(
            &base,
            if changed_bytes {
                "rejected"
            } else {
                "observed"
            },
        )
        .await;
        assert_eq!(receipt["id"], id);
        assert_eq!(receipt["source"]["client_id"], download["id"]);
        assert_eq!(receipt["client_attempts"].as_array().unwrap().len(), 1);
        assert_eq!(receipt["client_attempts"][0]["ordinal"], 1);
        assert_eq!(receipt["client_attempts"][0]["selection_round"], 1);
        if changed_bytes {
            assert_eq!(receipt["error_code"], "release_changed");
        }
        assert_eq!(peer.adds.lock().unwrap().len(), usize::from(!changed_bytes));
        assert_eq!(
            c.query(
                "SELECT payload_expires_at FROM rss_candidates WHERE id=?",
                [id]
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<i64>(0)
            .unwrap(),
            expiry
        );
        runtime.shutdown().await;
    }
}

#[tokio::test]
async fn safe_submission_storage_failure_reopens_same_attempt_without_alternative() {
    for (media, after_safe_result, changed_bytes) in [
        ("movies", true, false),
        ("tv", false, false),
        ("movies", false, false),
        ("tv", true, false),
        ("tv", false, true),
        ("movies", false, true),
    ] {
        eprintln!(
            "submission storage case media={media} after_safe_result={after_safe_result} changed_bytes={changed_bytes}"
        );
        let scratch = Scratch(
            std::env::temp_dir().join(format!("hrrdarr-submit-storage-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir(&scratch.0).unwrap();
        let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
        seed(&db).await;
        let feed = Arc::new(Remote::default());
        feed.mode.store(10, Ordering::SeqCst);
        feed.torrent_barrier_limit.store(1, Ordering::SeqCst);
        let peer = Arc::new(Remote::default());
        if after_safe_result {
            peer.mode.store(14, Ordering::SeqCst);
            *peer.submission_barrier_db.lock().unwrap() = Some(Arc::downgrade(&db));
        }
        let alternate = Arc::new(Remote::default());
        let (feed_base, _feed_server) = serve(
            axum::Router::new()
                .fallback(remote)
                .with_state(feed.clone()),
        )
        .await;
        *feed.endpoint.lock().unwrap() = feed_base.clone();
        let (peer_base, _peer_server) = serve(
            axum::Router::new()
                .fallback(remote)
                .with_state(peer.clone()),
        )
        .await;
        let (alternate_base, _alternate_server) = serve(
            axum::Router::new()
                .fallback(remote)
                .with_state(alternate.clone()),
        )
        .await;
        let (base, server, client) = app(db.clone()).await;
        let (indexer, download) = providers(&base, &feed_base).await;
        let mut settings = download["settings"].clone();
        settings["endpoint"] = json!(peer_base);
        let (code,download)=request(&base,"PUT",&format!("/api/v1/providers/{}",download["id"].as_str().unwrap()),json!({"revision":download["revision"],"name":"selected","enabled":true,"priority":1,"settings":settings})).await;
        assert_eq!(code, 200, "{download}");
        let (_, other) = providers(&base, &alternate_base).await;
        let (code,other)=request(&base,"PUT",&format!("/api/v1/providers/{}",other["id"].as_str().unwrap()),json!({"revision":other["revision"],"name":"alternate","enabled":true,"priority":50,"settings":other["settings"]})).await;
        assert_eq!(code, 200, "{other}");
        let c = db.connect().await.unwrap();
        c.execute(
            "UPDATE download_refresh_schedules SET next_run_at=unixepoch()+3600",
            (),
        )
        .await
        .unwrap();
        let _command = enqueue(
            &base,
            selection_target(&indexer, media, json!({"kind":"default"})),
        )
        .await;
        install_provider_lookup_fault(
            &c,
            if after_safe_result {
                "revision"
            } else {
                "endpoint"
            },
            download["id"].as_str().unwrap(),
        )
        .await;
        if !after_safe_result {
            // Fault activation follows the real dispatch journal transaction, so
            // preparation succeeds and submit's initial snapshot is the failing read.
            c.execute_batch("CREATE TRIGGER inject_submit_lookup_fault AFTER UPDATE OF status ON rss_candidates WHEN NEW.status='submitting' BEGIN UPDATE provider_lookup_fault SET enabled=1; END;").await.unwrap();
        }
        let runtime = commands::start(db.clone(), client).await.unwrap();
        if after_safe_result {
            tokio::time::timeout(hang(12), peer.started.notified())
                .await
                .unwrap();
            // A probe from an unrelated startup check cannot stand in for the
            // selected attempt's submission boundary.
            let (_, page) = request(&base, "GET", "/api/v1/rss/candidates", Value::Null).await;
            assert!(
                page["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|r| r["status"] == "submitting"
                        && r["source"]["client_id"] == download["id"]),
                "probe barrier must belong to the selected submission: {page}"
            );
            c.execute("UPDATE provider_lookup_fault SET enabled=1", ())
                .await
                .unwrap();
            peer.release.notify_one();
        }
        let receipt = tokio::time::timeout(hang(12), async {
            loop {
                let (_, page) = request(&base, "GET", "/api/v1/rss/candidates", Value::Null).await;
                if let Some(row) = page["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|r| r["error_code"] == "local_storage_retry")
                {
                    break row.clone();
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .expect("safe local submission storage retry deadline");
        let id = receipt["id"].as_str().unwrap().to_owned();
        assert_eq!(receipt["status"], "pending");
        assert_eq!(receipt["source"]["client_id"], download["id"]);
        assert_eq!(receipt["client_attempts"].as_array().unwrap().len(), 1);
        assert_eq!(receipt["client_attempts"][0]["state"], "prepared");
        assert_eq!(
            receipt["client_attempts"][0]["error_code"],
            "local_storage_retry"
        );
        let row=c.query("SELECT private_payload,payload_expires_at,selection_round,active_attempt,selection_strict,submission_identity_json,(SELECT count(*) FROM rss_hash_claims WHERE candidate_id=c.id) FROM rss_candidates c WHERE id=?",[id.as_str()]).await.unwrap().next().await.unwrap().unwrap();
        let payload = row.get::<Vec<u8>>(0).unwrap();
        let expiry = row.get::<i64>(1).unwrap();
        let identity = row.get::<String>(5).unwrap();
        assert_eq!(row.get::<i64>(2).unwrap(), 1);
        assert_eq!(row.get::<i64>(3).unwrap(), 1);
        assert_eq!(row.get::<i64>(4).unwrap(), 0);
        assert_eq!(row.get::<i64>(6).unwrap(), 0);
        drop(row);
        assert_eq!(feed.torrent_requests.load(Ordering::SeqCst), 2);
        assert!(peer.adds.lock().unwrap().is_empty());
        assert!(alternate.adds.lock().unwrap().is_empty());
        assert_eq!(
            c.query("SELECT count(*) FROM download_client_status", ())
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap()
                .get::<i64>(0)
                .unwrap(),
            0
        );
        runtime.shutdown().await;
        server.stop().await;
        if !after_safe_result {
            c.execute_batch("DROP TRIGGER inject_submit_lookup_fault")
                .await
                .unwrap();
        }
        remove_provider_lookup_fault(&c).await;
        // Reopen with the real thirty-second deadline intact. The original
        // journal entry, client, payload, expiry and SHA remain unchanged.
        peer.mode.store(0, Ordering::SeqCst);
        if !changed_bytes {
            feed.mode.store(0, Ordering::SeqCst);
        }
        drop(c);
        drop(db);
        let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
        let c = db.connect().await.unwrap();
        let row=c.query("SELECT private_payload,payload_expires_at,submission_identity_json FROM rss_candidates WHERE id=?",[id.as_str()]).await.unwrap().next().await.unwrap().unwrap();
        assert_eq!(row.get::<Vec<u8>>(0).unwrap(), payload);
        assert_eq!(row.get::<i64>(1).unwrap(), expiry);
        assert_eq!(row.get::<String>(2).unwrap(), identity);
        drop(row);
        let (base, _server, client) = app(db.clone()).await;
        let runtime = commands::start(db.clone(), client).await.unwrap();
        let expected = if changed_bytes {
            "rejected"
        } else {
            "observed"
        };
        let result = tokio::time::timeout(hang(45), async {
            loop {
                let (_, page) = request(&base, "GET", "/api/v1/rss/candidates", Value::Null).await;
                if let Some(row) = page["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|r| r["id"] == id && r["status"] == expected)
                {
                    break row.clone();
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("real local-storage retry deadline");
        assert_eq!(result["id"], id);
        assert_eq!(result["source"]["client_id"], download["id"]);
        assert_eq!(result["client_attempts"].as_array().unwrap().len(), 1);
        assert_eq!(result["client_attempts"][0]["ordinal"], 1);
        assert_eq!(result["client_attempts"][0]["selection_round"], 1);
        if changed_bytes {
            assert_eq!(result["error_code"], "release_changed");
        } else {
            assert!(result["client_attempts"][0]["error_code"].is_null());
        }
        assert_eq!(peer.adds.lock().unwrap().len(), usize::from(!changed_bytes));
        assert!(alternate.adds.lock().unwrap().is_empty());
        assert_eq!(
            c.query(
                "SELECT payload_expires_at FROM rss_candidates WHERE id=?",
                [id]
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<i64>(0)
            .unwrap(),
            expiry
        );
        runtime.shutdown().await;
    }
}
