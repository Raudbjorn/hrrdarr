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
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Server(tokio::task::JoinHandle<()>);
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
    mode: AtomicU8,
    v2: AtomicU8,
    date: Mutex<Option<String>>,
    endpoint: Mutex<String>,
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
        let magnet = if s.mode.load(Ordering::SeqCst) == 4 {
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
        return format!(r#"<rss xmlns:torznab="http://torznab.com/schemas/2015/feed"><channel><item><title>{title}</title><guid>PRIVATE_RSS_GUID-{hash}</guid><pubDate>{date}</pubDate><link>{link}</link>{magnet}<torznab:attr name="category" value="{cat}"/><torznab:attr name="size" value="1073741824"/></item>{missing}</channel></rss>"#).into_response();
    }
    if uri.path() == "/torrent" {
        s.started.notify_one();
        s.release.notified().await;
        return b"d4:infod6:lengthi1e4:name1:x12:piece lengthi16384e6:pieces20:aaaaaaaaaaaaaaaaaaaaee".to_vec().into_response();
    }
    if uri.path().ends_with("webapiVersion") {
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
        let hash = if multipart.is_some() {
            "fd1ecef9f83ef3d11c63557b271320a134a6add7"
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
        Server(tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap()
        })),
    )
}
async fn request(base: &str, method: &str, path: &str, body: Value) -> (u16, Value) {
    let response = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(6))
        .build()
        .unwrap()
        .request(method.parse().unwrap(), format!("{base}{path}"))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
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
    let key = Arc::new(providers::CredentialKey::from_hex(&"11".repeat(32)).unwrap());
    let (router, client) = providers::router_with_refresh(db.clone(), Some(key));
    let (base, server) = serve(
        router
            .merge(commands::router(db.clone()))
            .merge(hrrdarr::delay_profiles::router(db)),
    )
    .await;
    (base, server, client)
}
async fn until(base: &str, id: &Value) -> Value {
    tokio::time::timeout(Duration::from_secs(18), async {
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
    let runtime = commands::start(db.clone(), client).await.unwrap();
    for media in ["tv", "movies"] {
        let command = enqueue(&base, target(&indexer, &download, media)).await;
        let done = until(&base, &command["id"]).await;
        assert_eq!(done["status"], "succeeded", "{done}");
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
    }
    assert_eq!(remote_state.adds.lock().unwrap().as_slice(), [TV, MOVIE]);
    runtime.shutdown().await;
}
async fn receipt_until(base: &str, status: &str) -> Value {
    let outcome = tokio::time::timeout(Duration::from_secs(12), async {
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
    let runtime = commands::start(db.clone(), client).await.unwrap();
    for media in ["tv", "movies"] {
        let command = enqueue(&base, target(&indexer, &download, media)).await;
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
    runtime.shutdown().await;
}
#[tokio::test]
async fn interrupted_submission_reopens_with_typed_identity_and_only_reconciles() {
    for (media, mode) in [("tv", 1), ("movies", 1), ("tv", 2)] {
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
        let command = enqueue(&base, target(&indexer, &download, media)).await;
        let runtime = commands::start(db.clone(), client).await.unwrap();
        tokio::time::timeout(Duration::from_secs(12), state.started.notified())
            .await
            .unwrap();
        let (_, before) = request(&base, "GET", "/api/v1/rss/candidates", Value::Null).await;
        let id = before["items"][0]["id"].clone();
        assert_eq!(before["items"][0]["status"], "submitting");
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
            tokio::time::timeout(Duration::from_secs(10), async {
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
        if mode == 1 {
            assert_eq!(receipt["error_code"], "presence_unconfirmed");
        } else {
            let final_receipt = tokio::time::timeout(Duration::from_secs(75), async {
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
    for (mode, media, delay_change) in [
        (4, "tv", false),
        (1, "movies", false),
        (4, "tv", true),
        (4, "movies", true),
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
        let command = enqueue(&base, target(&indexer, &download, media)).await;
        let runtime = commands::start(db.clone(), client.clone()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(12), state.started.notified())
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
                    tokio::time::timeout(Duration::from_secs(12), barriers.started.notified())
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
    tokio::time::timeout(Duration::from_secs(12), async {
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
        tokio::time::timeout(Duration::from_secs(10), async {
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
    for media in ["tv", "movies"] {
        assert_eq!(
            request(
                &base,
                "POST",
                "/api/v1/blocklist/clear-commands",
                json!({"target":{"media_type":media},"priority":"normal"})
            )
            .await
            .0,
            202
        );
    }
    let runtime = commands::start(db.clone(), client).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
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
    let delayed = tokio::time::timeout(Duration::from_secs(8), async {
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
    for media in ["tv", "movies"] {
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
        c.execute(
            "UPDATE providers SET enabled=0,revision=revision+1 WHERE id=?",
            [old_indexer["id"].as_str().unwrap()],
        )
        .await
        .unwrap();
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
        tokio::time::timeout(Duration::from_secs(12), state.started.notified())
            .await
            .unwrap();
        state.release.notify_one();
        tokio::time::timeout(Duration::from_secs(12), state.started.notified())
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
