use axum::{
    body::Bytes,
    extract::{OriginalUri, State},
    http::{Method, StatusCode},
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
// Owned HTTP peers; private locators never become client-supplied grab authority.
#[derive(Default)]
struct Remote {
    items: Mutex<Vec<Value>>,
    adds: Mutex<Vec<String>>,
    offsets: Mutex<Vec<u32>>,
    endpoint: Mutex<String>,
    mode: AtomicU8,
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
fn hash(tv: bool, better: bool) -> String {
    (match (tv, better) {
        (true, false) => "1",
        (true, true) => "2",
        (false, false) => "3",
        (false, true) => "4",
    })
    .repeat(40)
}
fn item(hash: &str, category: &str) -> Value {
    json!({"hash":hash,"infohash_v1":hash,"infohash_v2":"","category":category,"name":"safe item","state":"downloading","progress":0.5,"size":100,"amount_left":50,"dlspeed":2,"upspeed":1,"eta":25,"ratio":0.2,"seeding_time":0,"priority":5,"force_start":false,"ratio_limit":-2.0,"seeding_time_limit":-2,"inactive_seeding_time_limit":-1,"seq_dl":false,"f_l_piece_prio":false,"auto_tmm":false,"tags":""})
}
async fn remote(
    State(s): State<Arc<Remote>>,
    method: Method,
    OriginalUri(uri): OriginalUri,
    body: Bytes,
) -> Response {
    let q: std::collections::HashMap<_, _> =
        url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();
    if uri.path() == "/" || uri.path() == "/api" {
        if q.get("t").is_some_and(|v| v == "caps") {
            return r#"<caps><limits max="100" default="100"/><searching><search available="yes" supportedParams="q"/><tv-search available="yes" supportedParams="q,tvdbid,season,ep"/><movie-search available="yes" supportedParams="q,tmdbid"/></searching><categories><category id="5000"><subcat id="5030"/></category><category id="2000"><subcat id="2030"/></category></categories></caps>"#.into_response();
        }
        let offset = q
            .get("offset")
            .map(|v| v.parse::<u32>().unwrap())
            .unwrap_or(0);
        s.offsets.lock().unwrap().push(offset);
        if s.mode.load(Ordering::SeqCst) == 5 && offset > 0 {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        let tv = q.get("cat").is_some_and(|v| v.contains("5030"));
        let better = offset > 0;
        let identity = if tv {
            "Harbor.S01E01"
        } else if s.mode.load(Ordering::SeqCst) == 2 {
            "Harbor.2021"
        } else {
            "Harbor.2020"
        };
        let quality = if better { "1080p" } else { "720p" };
        let cat = if s.mode.load(Ordering::SeqCst) == 1 {
            if tv { 2030 } else { 5030 }
        } else if tv {
            5030
        } else {
            2030
        };
        let hash = hash(tv, better);
        let locator = if s.mode.load(Ordering::SeqCst) == 3 {
            format!("{}/torrent", s.endpoint.lock().unwrap())
        } else {
            format!("magnet:?xt=urn:btih:{hash}")
        };
        let entry = if offset < 2 {
            format!(
                r#"<item><title>{identity}.{quality}.WEB-DL</title><guid>PRIVATE_RSS_GUID-{hash}</guid><pubDate>Mon, 01 Jan 2024 12:00:00 +0000</pubDate><link>{locator}</link><torznab:attr name="category" value="{cat}"/><torznab:attr name="size" value="1073741824"/></item>"#
            )
        } else {
            String::new()
        };
        return format!(r#"<rss xmlns:torznab="http://torznab.com/schemas/2015/feed"><channel><torznab:response offset="{offset}" total="2"/>{entry}</channel></rss>"#).into_response();
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
        let rows = s
            .items
            .lock()
            .unwrap()
            .iter()
            .filter(|r| {
                q.get("category")
                    .is_none_or(|v| r["category"] == v.as_str())
                    && q.get("hashes")
                        .is_none_or(|v| v.split('|').any(|h| r["hash"] == h))
            })
            .cloned()
            .collect::<Vec<_>>();
        return axum::Json(rows).into_response();
    }
    if uri.path().ends_with("torrents/add") {
        assert_eq!(method, Method::POST);
        let fields: std::collections::HashMap<_, _> =
            url::form_urlencoded::parse(&body).into_owned().collect();
        let magnet = url::Url::parse(fields.get("urls").expect("selected magnet")).unwrap();
        let selected = magnet
            .query_pairs()
            .find(|(k, _)| k == "xt")
            .unwrap()
            .1
            .strip_prefix("urn:btih:")
            .unwrap()
            .to_string();
        {
            let mut adds = s.adds.lock().unwrap();
            assert!(adds.len() < 8, "bounded owned submission fixture");
            adds.push(selected.clone());
        }
        s.items
            .lock()
            .unwrap()
            .push(item(&selected, fields.get("category").unwrap()));
        if s.mode.load(Ordering::SeqCst) == 4 {
            s.started.notify_one();
            s.release.notified().await;
        }
        return "Ok.".into_response();
    }
    (StatusCode::NOT_FOUND, "unexpected owned request").into_response()
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
            .merge(hrrdarr::search::router(db, client.clone())),
    )
    .await;
    (base, server, client)
}
async fn seed(db: &Database) {
    db.connect().await.unwrap().execute_batch("INSERT INTO series(id,tvdb_id,title,path)VALUES(1,101,'Harbor','/owned-fictional-tv'); INSERT INTO seasons(series_id,number)VALUES(1,1); INSERT INTO episodes(id,series_id,season,number,title,runtime,air_date_utc)VALUES(1,1,1,1,'Pilot',45,'2099-01-01 00:00:00'); INSERT INTO movie_metadata(id,tmdb_id,title,year,runtime,digital_release)VALUES(1,201,'Harbor',2020,100,'2099-01-01 00:00:00'); INSERT INTO movies(id,metadata_id,path)VALUES(1,1,'/owned-fictional-movie'); INSERT INTO quality_profiles VALUES(1,'tv','HD'),(2,'movies','HD'); INSERT INTO quality_profile_items(profile_id,media_type,quality_id,position,allowed)VALUES(1,'tv',5,0,1),(1,'tv',3,1,1),(2,'movies',5,0,1),(2,'movies',3,1,1); INSERT INTO quality_profile_policies(profile_id,media_type,upgrade_allowed,cutoff_quality_id,min_format_score,cutoff_format_score,min_upgrade_format_score,language_id)VALUES(1,'tv',1,3,0,0,1,NULL),(2,'movies',1,3,0,0,1,-2); INSERT INTO library_settings(media_type,series_id,quality_profile_id,series_type,use_scene_numbering)VALUES('tv',1,1,'standard',0); INSERT INTO library_settings(media_type,movie_id,quality_profile_id,minimum_availability)VALUES('movies',1,2,'released'); INSERT INTO release_delay_policies VALUES('tv',0,0,0),('movies',0,0,0);").await.unwrap();
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
fn input(indexer: &Value, client: &Value, tv: bool, mode: &str) -> Value {
    json!({"request_id":uuid::Uuid::new_v4(),"mode":mode,"target":{"media_type":if tv {"episode"}else{"movie"},"id":1},"indexer_id":indexer["id"],"indexer_revision":indexer["revision"],"client_id":client["id"],"client_revision":client["revision"],"priority":"normal"})
}
async fn settled(base: &str, id: &Value) -> Value {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let (status, row) = request(
                base,
                "GET",
                &format!("/api/v1/search/commands/{}", id.as_str().unwrap()),
                Value::Null,
            )
            .await;
            assert_eq!(status, 200, "{row}");
            if matches!(
                row["status"].as_str(),
                Some("succeeded" | "failed" | "cancelled")
            ) {
                return row;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .expect("search command settlement deadline")
}
async fn candidate_status(base: &str, id: &str, expected: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let (status, rows) = request(base, "GET", "/api/v1/rss/candidates", Value::Null).await;
            assert_eq!(status, 200, "{rows}");
            if let Some(row) = rows["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["id"] == id)
            {
                if row["status"] == expected {
                    return row.clone();
                }
                assert!(
                    !matches!(row["status"].as_str(), Some("failed" | "rejected")),
                    "selected candidate rejected: {row}"
                );
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .expect("selected candidate observation deadline")
}
async fn observed(base: &str, id: &str) -> Value {
    candidate_status(base, id, "observed").await
}
async fn offers(base: &str, id: &Value) -> Vec<Value> {
    let (status, page) = request(
        base,
        "GET",
        &format!("/api/v1/search/commands/{}/results", id.as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_eq!(status, 200, "{page}");
    page["items"].as_array().unwrap().clone()
}

// Actual HTTP production creates every command/result/candidate. Only library fixtures
// are seeded; neither a candidate nor an observed submission is fabricated in SQL.
#[tokio::test]
async fn automatic_and_interactive_search_preserve_context_targets_and_replay() {
    for interactive in [false, true] {
        let scratch =
            Scratch(std::env::temp_dir().join(format!("hrrdarr-search-{}", uuid::Uuid::new_v4())));
        std::fs::create_dir(&scratch.0).unwrap();
        let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
        seed(&db).await;
        let remote_state = Arc::new(Remote::default());
        let (remote_base, remote_server) = serve(
            axum::Router::new()
                .fallback(remote)
                .with_state(remote_state.clone()),
        )
        .await;
        let (base, server, client) = app(db.clone()).await;
        let (indexer, download) = providers(&base, &remote_base).await;
        let runtime = commands::start(db.clone(), client).await.unwrap();
        for tv in [true, false] {
            // The same private release was already rejected by background RSS.
            // A user search must retain its exception without weakening RSS rules.
            let (status, background) = request(&base, "POST", "/api/v1/rss/commands", json!({"target":{"media_type":if tv {"tv"}else{"movies"},"indexer_id":indexer["id"],"indexer_revision":1,"client_id":download["id"],"client_revision":1},"priority":"normal"})).await;
            assert_eq!(status, 202, "{background}");
            tokio::time::timeout(Duration::from_secs(20), async {
                loop {
                    let (status, row) = request(
                        &base,
                        "GET",
                        &format!(
                            "/api/v1/rss/commands/{}",
                            background["id"].as_str().unwrap()
                        ),
                        Value::Null,
                    )
                    .await;
                    assert_eq!(status, 200);
                    if row["status"] == "succeeded" {
                        break;
                    }
                    assert_ne!(row["status"], "failed", "{row}");
                    tokio::time::sleep(Duration::from_millis(30)).await;
                }
            })
            .await
            .expect("background rejection deadline");
            let (_, receipts) = request(
                &base,
                "GET",
                &format!(
                    "/api/v1/rss/candidates?command_id={}",
                    background["id"].as_str().unwrap()
                ),
                Value::Null,
            )
            .await;
            assert!(!receipts["items"].as_array().unwrap().is_empty());
            assert!(
                receipts["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|r| r["status"] == "rejected"),
                "{receipts}"
            );
            assert_eq!(
                remote_state.adds.lock().unwrap().len(),
                if tv { 0 } else { 1 }
            );
            let intent = input(
                &indexer,
                &download,
                tv,
                if interactive {
                    "interactive"
                } else {
                    "automatic"
                },
            );
            let (code, command) =
                request(&base, "POST", "/api/v1/search/commands", intent.clone()).await;
            assert_eq!(code, 202, "{command}");
            let done = settled(&base, &command["id"]).await;
            assert_eq!(done["status"], "succeeded", "{done}");
            assert_eq!(
                done["fetch_complete"], true,
                "must finish pagination before ranking"
            );
            let results = offers(&base, &command["id"]).await;
            assert_eq!(results.len(), 2, "{results:?}");
            let before = if tv { 0 } else { 1 };
            let candidate_id = if interactive {
                assert_eq!(
                    remote_state.adds.lock().unwrap().len(),
                    before,
                    "listing must not submit"
                );
                let chosen = results
                    .iter()
                    .find(|r| r["metadata"]["title"].as_str().unwrap().contains("720p"))
                    .unwrap();
                let path = format!(
                    "/api/v1/search/results/{}/grab",
                    chosen["id"].as_str().unwrap()
                );
                let (a, b) = tokio::join!(
                    request(&base, "POST", &path, json!({})),
                    request(&base, "POST", &path, json!({}))
                );
                assert!((200..300).contains(&a.0), "{a:?}");
                assert!((200..300).contains(&b.0), "{b:?}");
                assert_eq!(
                    a.1["id"], b.1["id"],
                    "concurrent selection replays one receipt"
                );
                let other = results.iter().find(|r| r["id"] != chosen["id"]).unwrap();
                assert_eq!(
                    request(
                        &base,
                        "POST",
                        &format!(
                            "/api/v1/search/results/{}/grab",
                            other["id"].as_str().unwrap()
                        ),
                        json!({})
                    )
                    .await
                    .0,
                    409
                );
                a.1["id"].as_str().unwrap().to_owned()
            } else {
                done["selected_candidate_id"].as_str().unwrap().to_owned()
            };
            let receipt = observed(&base, &candidate_id).await;
            assert_eq!(
                receipt["target"]["media_type"],
                if tv { "tv" } else { "movies" }
            );
            assert_eq!(
                receipt["target"][if tv { "series_id" } else { "movie_id" }],
                1
            );
            assert_eq!(
                remote_state.adds.lock().unwrap()[before],
                hash(tv, !interactive),
                "profile rank beats numeric quality ID; interactive uses exact choice"
            );
            let (code, replay) =
                request(&base, "POST", "/api/v1/search/commands", intent.clone()).await;
            assert!((200..300).contains(&code), "{replay}");
            assert_eq!(replay["id"], command["id"]);
            let mut conflict = intent;
            conflict["mode"] = json!(if interactive {
                "automatic"
            } else {
                "interactive"
            });
            assert_eq!(
                request(&base, "POST", "/api/v1/search/commands", conflict)
                    .await
                    .0,
                409
            );
        }
        assert_eq!(remote_state.adds.lock().unwrap().len(), 2);
        assert!(
            remote_state.offsets.lock().unwrap().contains(&1),
            "better release must come from continuation"
        );
        runtime.shutdown().await;
        server.stop().await;
        remote_server.stop().await;
    }
}

#[tokio::test]
async fn retained_interactive_offer_survives_database_reopen_without_becoming_automatic() {
    let scratch = Scratch(
        std::env::temp_dir().join(format!("hrrdarr-search-reopen-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&scratch.0).unwrap();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    seed(&db).await;
    let remote_state = Arc::new(Remote::default());
    let (remote_base, remote_server) = serve(
        axum::Router::new()
            .fallback(remote)
            .with_state(remote_state.clone()),
    )
    .await;
    let (base, server, client) = app(db.clone()).await;
    let (indexer, download) = providers(&base, &remote_base).await;
    let runtime = commands::start(db.clone(), client).await.unwrap();
    let intent = input(&indexer, &download, false, "interactive");
    let (code, command) = request(&base, "POST", "/api/v1/search/commands", intent.clone()).await;
    assert_eq!(code, 202, "{command}");
    assert_eq!(settled(&base, &command["id"]).await["status"], "succeeded");
    let results = offers(&base, &command["id"]).await;
    assert_eq!(results.len(), 2);
    let chosen = results
        .iter()
        .find(|r| r["metadata"]["title"].as_str().unwrap().contains("1080p"))
        .unwrap();
    assert!(remote_state.adds.lock().unwrap().is_empty());
    runtime.shutdown().await;
    server.stop().await;
    drop(db);
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let (base, server, client) = app(db.clone()).await;
    let runtime = commands::start(db, client).await.unwrap();
    let (_, replay) = request(&base, "POST", "/api/v1/search/commands", intent).await;
    assert_eq!(replay["id"], command["id"]);
    let path = format!(
        "/api/v1/search/results/{}/grab",
        chosen["id"].as_str().unwrap()
    );
    // A client cannot replace retained authority with an arbitrary locator or target.
    let(code,_)=request(&base,"POST",&path,json!({"url":"http://127.0.0.1:1/private","target":{"media_type":"episode","id":1},"context":"rss"})).await;
    assert_eq!(code, 400);
    assert!(remote_state.adds.lock().unwrap().is_empty());
    let (code, receipt) = request(&base, "POST", &path, json!({})).await;
    assert!((200..300).contains(&code), "{receipt}");
    observed(&base, receipt["id"].as_str().unwrap()).await;
    let (code, replay) = request(&base, "POST", &path, json!({})).await;
    assert!((200..300).contains(&code));
    assert_eq!(replay["id"], receipt["id"]);
    assert_eq!(
        remote_state.adds.lock().unwrap().as_slice(),
        [hash(false, true)]
    );
    runtime.shutdown().await;
    server.stop().await;
    remote_server.stop().await;
}

#[tokio::test]
async fn unselected_offers_revalidate_provider_revision_and_library_identity() {
    for stale_provider in [true, false] {
        let scratch = Scratch(
            std::env::temp_dir().join(format!("hrrdarr-search-stale-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir(&scratch.0).unwrap();
        let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
        seed(&db).await;
        let remote_state = Arc::new(Remote::default());
        let (remote_base, remote_server) = serve(
            axum::Router::new()
                .fallback(remote)
                .with_state(remote_state.clone()),
        )
        .await;
        let (base, server, client) = app(db.clone()).await;
        let (indexer, download) = providers(&base, &remote_base).await;
        let runtime = commands::start(db.clone(), client).await.unwrap();
        let (code, command) = request(
            &base,
            "POST",
            "/api/v1/search/commands",
            input(&indexer, &download, false, "interactive"),
        )
        .await;
        assert_eq!(code, 202, "{command}");
        assert_eq!(settled(&base, &command["id"]).await["status"], "succeeded");
        let results = offers(&base, &command["id"]).await;
        assert_eq!(results.len(), 2);
        if stale_provider {
            let(code,row)=request(&base,"PUT",&format!("/api/v1/providers/{}",download["id"].as_str().unwrap()),json!({"revision":download["revision"],"name":"changed client","enabled":true,"priority":1,"settings":download["settings"]})).await;
            assert_eq!(code, 200, "{row}");
        } else {
            // Metadata refresh can legitimately change target identity after a search.
            db.connect()
                .await
                .unwrap()
                .execute("UPDATE movie_metadata SET year=2021 WHERE id=1", ())
                .await
                .unwrap();
        }
        let (code, reason) = request(
            &base,
            "POST",
            &format!(
                "/api/v1/search/results/{}/grab",
                results[0]["id"].as_str().unwrap()
            ),
            json!({}),
        )
        .await;
        assert_eq!(
            code, 409,
            "stale retained result must not authorize a download: {reason}"
        );
        assert!(remote_state.adds.lock().unwrap().is_empty());
        runtime.shutdown().await;
        server.stop().await;
        remote_server.stop().await;
    }
}

#[tokio::test]
async fn wrong_category_and_movie_remake_never_reach_download_client() {
    for (tv, mode) in [(true, 1), (false, 1), (false, 2)] {
        let scratch = Scratch(
            std::env::temp_dir().join(format!("hrrdarr-search-reject-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir(&scratch.0).unwrap();
        let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
        seed(&db).await;
        let remote_state = Arc::new(Remote::default());
        remote_state.mode.store(mode, Ordering::SeqCst);
        let (remote_base, remote_server) = serve(
            axum::Router::new()
                .fallback(remote)
                .with_state(remote_state.clone()),
        )
        .await;
        let (base, server, client) = app(db.clone()).await;
        let (indexer, download) = providers(&base, &remote_base).await;
        let runtime = commands::start(db, client).await.unwrap();
        let (code, command) = request(
            &base,
            "POST",
            "/api/v1/search/commands",
            input(&indexer, &download, tv, "automatic"),
        )
        .await;
        assert_eq!(code, 202, "{command}");
        let done = settled(&base, &command["id"]).await;
        assert!(
            done["selected_candidate_id"].is_null(),
            "wrong media/year cannot be selected: {done}"
        );
        // Only the intended domain/matching failure can satisfy this regression;
        // a storage or generic transport failure must fail the test.
        if done["error_code"] == "invalid_release" {
            assert_eq!(mode, 1, "remake must reach the matcher");
            assert_eq!(done["status"], "failed");
        } else {
            assert!(
                done["status"] == "succeeded"
                    || (done["status"] == "failed" && done["error_code"] == "no_eligible_release"),
                "{done}"
            );
            let results = offers(&base, &command["id"]).await;
            assert!(!results.is_empty(), "nonvacuous rejection fixture");
            let expected = if mode == 1 {
                "wrong_media_category"
            } else {
                "no_library_match"
            };
            assert!(
                results
                    .iter()
                    .all(|r| r["decision"]["disposition"] == "reject"
                        && r["decision"]["reasons"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|v| v == expected)),
                "expected {expected}: {results:?}"
            );
        }
        assert!(remote_state.adds.lock().unwrap().is_empty());
        runtime.shutdown().await;
        server.stop().await;
        remote_server.stop().await;
    }
}

async fn age_offer(db: &Database, id: &str) {
    let connection = db.connect().await.unwrap();
    let tx = connection
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await
        .unwrap();
    let trigger=tx.query("SELECT sql FROM sqlite_master WHERE type='trigger' AND name='search_results_immutable'",()).await.unwrap().next().await.unwrap().unwrap().get::<String>(0).unwrap();
    // Isolated elapsed-time fault injection: this ages an API-produced offer, with
    // no locator/target/payload changes. It proves expiry handling, not wall-clock
    // integration. Restore the exact original trigger in this same transaction.
    tx.execute_batch("DROP TRIGGER search_results_immutable")
        .await
        .unwrap();
    tx.execute("UPDATE search_results SET created_at=created_at-3600,expires_at=expires_at-3600 WHERE id=?",[id]).await.unwrap();
    tx.execute_batch(&trigger).await.unwrap();
    tx.commit().await.unwrap();
}
#[tokio::test]
async fn expired_unselected_offer_rejects_but_selected_receipt_remains_replayable() {
    let scratch = Scratch(
        std::env::temp_dir().join(format!("hrrdarr-search-expiry-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&scratch.0).unwrap();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    seed(&db).await;
    let remote_state = Arc::new(Remote::default());
    let (remote_base, remote_server) = serve(
        axum::Router::new()
            .fallback(remote)
            .with_state(remote_state.clone()),
    )
    .await;
    let (base, server, client) = app(db.clone()).await;
    let (indexer, download) = providers(&base, &remote_base).await;
    let runtime = commands::start(db.clone(), client).await.unwrap();
    let (code, command) = request(
        &base,
        "POST",
        "/api/v1/search/commands",
        input(&indexer, &download, true, "interactive"),
    )
    .await;
    assert_eq!(code, 202, "{command}");
    assert_eq!(settled(&base, &command["id"]).await["status"], "succeeded");
    let rows = offers(&base, &command["id"]).await;
    assert_eq!(rows.len(), 2);
    let expired = rows[0]["id"].as_str().unwrap();
    age_offer(&db, expired).await;
    let (code, error) = request(
        &base,
        "POST",
        &format!("/api/v1/search/results/{expired}/grab"),
        json!({}),
    )
    .await;
    // The periodic expiry sweep may remove this aged unselected offer first.
    assert!(
        (code == 409 && error["error"]["code"] == "result_expired")
            || (code == 404 && error["error"]["code"] == "search_result_not_found"),
        "{code}: {error}"
    );
    assert!(remote_state.adds.lock().unwrap().is_empty());
    let selected = rows[1]["id"].as_str().unwrap();
    let path = format!("/api/v1/search/results/{selected}/grab");
    let (code, receipt) = request(&base, "POST", &path, json!({})).await;
    assert!((200..300).contains(&code), "{receipt}");
    observed(&base, receipt["id"].as_str().unwrap()).await;
    age_offer(&db, selected).await;
    let (code, replay) = request(&base, "POST", &path, json!({})).await;
    assert!((200..300).contains(&code), "{replay}");
    assert_eq!(replay["id"], receipt["id"]);
    assert_eq!(remote_state.adds.lock().unwrap().len(), 1);
    runtime.shutdown().await;
    server.stop().await;
    remote_server.stop().await;
}

#[tokio::test]
async fn target_identity_is_checked_again_after_remote_preparation() {
    let scratch = Scratch(
        std::env::temp_dir().join(format!("hrrdarr-search-barrier-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&scratch.0).unwrap();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    seed(&db).await;
    let remote_state = Arc::new(Remote::default());
    let (remote_base, remote_server) = serve(
        axum::Router::new()
            .fallback(remote)
            .with_state(remote_state.clone()),
    )
    .await;
    *remote_state.endpoint.lock().unwrap() = remote_base.clone();
    remote_state.mode.store(3, Ordering::SeqCst);
    let (base, server, client) = app(db.clone()).await;
    let (indexer, download) = providers(&base, &remote_base).await;
    let runtime = commands::start(db.clone(), client).await.unwrap();
    let (code, command) = request(
        &base,
        "POST",
        "/api/v1/search/commands",
        input(&indexer, &download, false, "interactive"),
    )
    .await;
    assert_eq!(code, 202, "{command}");
    assert_eq!(settled(&base, &command["id"]).await["status"], "succeeded");
    let rows = offers(&base, &command["id"]).await;
    remote_state.mode.store(3, Ordering::SeqCst);
    let (code, receipt) = request(
        &base,
        "POST",
        &format!(
            "/api/v1/search/results/{}/grab",
            rows[0]["id"].as_str().unwrap()
        ),
        json!({}),
    )
    .await;
    assert!((200..300).contains(&code), "{receipt}");
    tokio::time::timeout(Duration::from_secs(10), remote_state.started.notified())
        .await
        .expect("owned prepare boundary reached");
    db.connect()
        .await
        .unwrap()
        .execute("UPDATE movie_metadata SET year=2021 WHERE id=1", ())
        .await
        .unwrap();
    remote_state.mode.store(0, Ordering::SeqCst);
    remote_state.release.notify_one();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let (_, page) = request(&base, "GET", "/api/v1/rss/candidates", Value::Null).await;
            let row = page["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["id"] == receipt["id"])
                .unwrap();
            if matches!(row["status"].as_str(), Some("rejected" | "failed")) {
                assert!(
                    !row["reasons"].as_array().unwrap().is_empty() || row["error_code"].is_string(),
                    "{row}"
                );
                break;
            }
            assert_ne!(
                row["status"], "observed",
                "identity drift must be caught before external submission"
            );
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .expect("post-prepare identity rejection");
    assert!(remote_state.adds.lock().unwrap().is_empty());
    runtime.shutdown().await;
    server.stop().await;
    remote_server.stop().await;
}

#[tokio::test]
async fn interrupted_selected_submission_reopens_and_only_reconciles() {
    for tv in [true, false] {
        let scratch = Scratch(std::env::temp_dir().join(format!(
            "hrrdarr-search-submit-reopen-{}",
            uuid::Uuid::new_v4()
        )));
        std::fs::create_dir(&scratch.0).unwrap();
        let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
        seed(&db).await;
        let remote_state = Arc::new(Remote::default());
        remote_state.mode.store(4, Ordering::SeqCst);
        let (remote_base, remote_server) = serve(
            axum::Router::new()
                .fallback(remote)
                .with_state(remote_state.clone()),
        )
        .await;
        let (base, server, client) = app(db.clone()).await;
        let (indexer, download) = providers(&base, &remote_base).await;
        let runtime = commands::start(db.clone(), client).await.unwrap();
        let intent = input(&indexer, &download, tv, "automatic");
        let (code, command) =
            request(&base, "POST", "/api/v1/search/commands", intent.clone()).await;
        assert_eq!(code, 202, "{command}");
        tokio::time::timeout(Duration::from_secs(15), remote_state.started.notified())
            .await
            .expect("client accepted selected submission before losing reply");
        let (_, page) = request(&base, "GET", "/api/v1/rss/candidates", Value::Null).await;
        let receipt = page["items"][0].clone();
        assert_eq!(receipt["status"], "submitting");
        runtime.shutdown().await;
        server.stop().await;
        drop(db);
        remote_state.mode.store(0, Ordering::SeqCst);
        remote_state.release.notify_one();
        let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
        let (base, server, client) = app(db.clone()).await;
        let runtime = commands::start(db, client).await.unwrap();
        // A lost add acknowledgement cannot prove that remote presence belongs to
        // this attempt. Reuse the existing conservative reconciliation contract.
        let recovered =
            candidate_status(&base, receipt["id"].as_str().unwrap(), "needs_attention").await;
        assert_eq!(recovered["error_code"], "presence_unconfirmed");
        assert_eq!(
            recovered["target"]["media_type"],
            if tv { "tv" } else { "movies" }
        );
        let (code, replay) = request(&base, "POST", "/api/v1/search/commands", intent).await;
        assert!((200..300).contains(&code), "{replay}");
        assert_eq!(replay["id"], command["id"]);
        assert_eq!(
            remote_state.adds.lock().unwrap().as_slice(),
            [hash(tv, true)],
            "lost response/restart must reconcile rather than resubmit"
        );
        runtime.shutdown().await;
        server.stop().await;
        remote_server.stop().await;
    }
}

#[tokio::test]
async fn failed_continuation_never_exposes_or_selects_partial_results() {
    let scratch = Scratch(
        std::env::temp_dir().join(format!("hrrdarr-search-partial-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&scratch.0).unwrap();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    seed(&db).await;
    let remote_state = Arc::new(Remote::default());
    remote_state.mode.store(5, Ordering::SeqCst);
    let (remote_base, remote_server) = serve(
        axum::Router::new()
            .fallback(remote)
            .with_state(remote_state.clone()),
    )
    .await;
    let (base, server, client) = app(db.clone()).await;
    let (indexer, download) = providers(&base, &remote_base).await;
    let runtime = commands::start(db, client).await.unwrap();
    for mode in ["automatic", "interactive"] {
        let (code, command) = request(
            &base,
            "POST",
            "/api/v1/search/commands",
            input(&indexer, &download, true, mode),
        )
        .await;
        assert_eq!(code, 202, "{command}");
        let done = settled(&base, &command["id"]).await;
        assert_eq!(done["status"], "failed", "{done}");
        assert_eq!(
            done["error_code"], "provider_unavailable",
            "second-page HTTP401 must surface specifically"
        );
        assert_eq!(done["fetch_complete"], false);
        assert!(done["selected_candidate_id"].is_null());
        assert!(
            offers(&base, &command["id"]).await.is_empty(),
            "partial capture cannot authorize manual grab either"
        );
    }
    assert!(remote_state.offsets.lock().unwrap().contains(&1));
    assert!(remote_state.adds.lock().unwrap().is_empty());
    runtime.shutdown().await;
    server.stop().await;
    remote_server.stop().await;
}
