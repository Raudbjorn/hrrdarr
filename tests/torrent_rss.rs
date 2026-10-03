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
const TV: &str = "1111111111111111111111111111111111111111";
const MOVIE: &str = "2222222222222222222222222222222222222222";
fn item(hash: &str, category: &str) -> Value {
    json!({"hash":hash,"infohash_v1":hash,"infohash_v2":"","category":category,"name":"safe item","state":"downloading","progress":0.5,"size":100,"amount_left":50,"dlspeed":2,"upspeed":1,"eta":25,"ratio":0.2,"seeding_time":0,"priority":5,"force_start":false,"ratio_limit":-2.0,"seeding_time_limit":-2,"inactive_seeding_time_limit":-1,"seq_dl":false,"f_l_piece_prio":false,"auto_tmm":false,"tags":""})
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
        ![
            "PRIVATE_RSS_GUID",
            "PRIVATE_RSS_LOCATOR",
            "PRIVATE_COOKIE",
            "PRIVATE_PASSKEY"
        ]
        .iter()
        .any(|secret| text.contains(secret)),
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
            .merge(hrrdarr::delay_profiles::router(db.clone()))
            .merge(hrrdarr::release_profiles::router(db)),
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

// Both secrets are write-only: no response, stored column or mock log of this file may echo them.
const COOKIE: &str = "uid=7; pass=PRIVATE_COOKIE";
const PASSKEY: &str = "PRIVATE_PASSKEY";

#[derive(Default)]
struct Remote {
    items: Mutex<Vec<Value>>,
    adds: Mutex<Vec<String>>,
    feed_requests: Mutex<Vec<(String, Option<String>)>>,
    torrent_requests: Mutex<Vec<(String, Option<String>)>>,
    feed_mode: AtomicU8,
    endpoint: Mutex<String>,
}
const NORMAL: u8 = 0;
const OVERSIZED: u8 = 1;
const DTD: u8 = 2;
const HTML: u8 = 3;
const SERVER_ERROR: u8 = 4;
const REDIRECT: u8 = 5;
const EMPTY: u8 = 6;
const MALFORMED_ITEM: u8 = 7;
const SEEDERS: u8 = 8;
const RATE_LIMITED: u8 = 9;
const ENCLOSURE_ONLY: u8 = 10;
const TORRENT_FILE: &str =
    "d4:infod6:lengthi1e4:name1:x12:piece lengthi16384e6:pieces20:aaaaaaaaaaaaaaaaaaaaee";
// SHA-1 of the exact bencoded info dictionary served above.
const FILE_HASH: &str = "fd1ecef9f83ef3d11c63557b271320a134a6add7";

fn feed_xml(kind: &str, mode: u8, endpoint: &str) -> String {
    let (title, hash) = if kind == "tv" {
        ("Harbor.S01E01.1080p.WEB-DL", TV)
    } else {
        ("Harbor.2020.1080p.WEB-DL", MOVIE)
    };
    let magnet = if mode == ENCLOSURE_ONLY {
        String::new()
    } else {
        format!("<torrent:magnetURI>magnet:?xt=urn:btih:{hash}</torrent:magnetURI>")
    };
    let good = format!(
        r#"<item><title>{title}</title><guid>PRIVATE_RSS_GUID-{kind}</guid><pubDate>Mon, 01 Jan 2024 12:00:00 +0000</pubDate><enclosure url="{endpoint}/torrent?passkey=PRIVATE_RSS_LOCATOR" length="1073741824" type="application/x-bittorrent"/>{magnet}<torrent:seeds>12</torrent:seeds></item>"#
    );
    let extra = match mode {
        MALFORMED_ITEM => {
            r#"<item><guid>PRIVATE_RSS_GUID-untitled</guid><pubDate>Mon, 01 Jan 2024 12:00:00 +0000</pubDate><link>https://t.example/x</link></item><item><title>Dated.Ago</title><pubDate>2 hours ago</pubDate><link>https://t.example/y</link></item>"#.to_string()
        }
        _ => String::new(),
    };
    let body = match mode {
        EMPTY => String::new(),
        SEEDERS => r#"<item><title>High.Seeds.S01E01.1080p</title><guid>g-high</guid><pubDate>Mon, 01 Jan 2024 12:00:00 +0000</pubDate><link>https://t.example/1</link><torrent:seeds>10</torrent:seeds></item><item><title>Low.Seeds.S01E01.1080p</title><guid>g-low</guid><pubDate>Mon, 01 Jan 2024 12:00:00 +0000</pubDate><link>https://t.example/2</link><torrent:seeds>1</torrent:seeds></item><item><title>Unknown.Seeds.S01E01.1080p</title><guid>g-unknown</guid><pubDate>Mon, 01 Jan 2024 12:00:00 +0000</pubDate><link>https://t.example/3</link></item>"#.to_string(),
        _ => format!("{good}{extra}"),
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><rss version="2.0" xmlns:torrent="http://xmlns.ezrss.it/0.1/"><channel><title>feed</title>{body}</channel></rss>"#
    )
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
    if uri.path() == "/feed" {
        let cookie = headers
            .get("cookie")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        s.feed_requests
            .lock()
            .unwrap()
            .push((uri.to_string(), cookie.clone()));
        if cookie.as_deref() != Some(COOKIE)
            || q.get("passkey").map(String::as_str) != Some(PASSKEY)
        {
            return (StatusCode::FORBIDDEN, "denied PRIVATE_COOKIE").into_response();
        }
        let kind = q.get("kind").map_or("tv", String::as_str);
        let endpoint = s.endpoint.lock().unwrap().clone();
        return match s.feed_mode.load(Ordering::SeqCst) {
            OVERSIZED => {
                let filler = "x".repeat(2 * 1024 * 1024);
                format!("<rss><channel><title>{filler}</title></channel></rss>").into_response()
            }
            DTD => r#"<?xml version="1.0"?><!DOCTYPE rss [<!ENTITY a "aaaaaaaaaa"><!ENTITY b "&a;&a;&a;&a;&a;&a;&a;&a;">]><rss><channel><title>&b;</title></channel></rss>"#.into_response(),
            HTML => "<html><body>Please log in</body></html>".into_response(),
            SERVER_ERROR => (StatusCode::INTERNAL_SERVER_ERROR, "boom").into_response(),
            REDIRECT => (StatusCode::FOUND, [("location", "http://127.0.0.1:1/elsewhere")], "").into_response(),
            RATE_LIMITED => (StatusCode::TOO_MANY_REQUESTS, [("retry-after", "120")], "slow down").into_response(),
            mode => feed_xml(kind, mode, &endpoint).into_response(),
        };
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
    if uri.path() == "/torrent" {
        // The .torrent download is fetched by the download client's transport, not by the feed's.
        s.torrent_requests.lock().unwrap().push((
            uri.to_string(),
            headers
                .get("cookie")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned),
        ));
        return TORRENT_FILE.as_bytes().to_vec().into_response();
    }
    if uri.path().ends_with("torrents/add") {
        assert_eq!(method, Method::POST);
        let multipart = headers
            .get("content-type")
            .and_then(|header| header.to_str().ok())
            .and_then(|header| header.split("boundary=").nth(1));
        let pairs: std::collections::HashMap<String, String> = if let Some(boundary) = multipart {
            let body = std::str::from_utf8(&body).unwrap();
            assert!(body.contains("name=\"torrents\"; filename="));
            assert!(body.contains(TORRENT_FILE));
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
        let hash = if multipart.is_some() {
            FILE_HASH
        } else if category == "tv" {
            TV
        } else {
            MOVIE
        };
        s.adds.lock().unwrap().push(hash.into());
        s.items.lock().unwrap().push(item(hash, category));
        return "Ok.".into_response();
    }
    (StatusCode::NOT_FOUND, "unexpected owned mock request").into_response()
}

fn scope(minimum: Option<u32>) -> Value {
    json!({"enable_rss":true,"minimum_seeders":minimum})
}
fn feed_credentials() -> Value {
    json!({"kind":"feed","cookie":COOKIE,
        "tv_parameters":[{"name":"kind","value":"tv"},{"name":"passkey","value":PASSKEY}],
        "movie_parameters":[{"name":"kind","value":"movies"},{"name":"passkey","value":PASSKEY}]})
}
fn feed_body(remote: &str, tv: Option<Value>, movies: Option<Value>) -> Value {
    json!({"name":"feed","enabled":true,"priority":1,
        "settings":{"implementation":"torrentrss","endpoint":format!("{remote}/feed"),"tv":tv,"movies":movies},
        "credentials":feed_credentials()})
}
async fn create_feed(base: &str, remote: &str, tv: Option<Value>, movies: Option<Value>) -> Value {
    let (code, value) = request(
        base,
        "POST",
        "/api/v1/providers",
        feed_body(remote, tv, movies),
    )
    .await;
    assert_eq!(code, 201, "{value}");
    value
}
async fn create_client(base: &str, remote: &str) -> Value {
    let (code, value) = request(
        base,
        "POST",
        "/api/v1/providers",
        json!({"name":"client","enabled":true,"priority":1,"settings":{"implementation":"qbittorrent","endpoint":remote,
            "tv":{"category":"tv","imported_category":null,"recent_priority":0,"older_priority":0},
            "movies":{"category":"movies","imported_category":null,"recent_priority":0,"older_priority":0}}}),
    )
    .await;
    assert_eq!(code, 201, "{value}");
    value
}
async fn harness() -> (
    Scratch,
    Arc<Database>,
    Arc<Remote>,
    String,
    Server,
    String,
    Server,
    providers::RefreshClient,
) {
    let scratch =
        Scratch(std::env::temp_dir().join(format!("hrrdarr-torrent-rss-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&scratch.0).unwrap();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let state = Arc::new(Remote::default());
    let (remote_base, remote_server) = serve(
        axum::Router::new()
            .fallback(remote)
            .with_state(state.clone()),
    )
    .await;
    *state.endpoint.lock().unwrap() = remote_base.clone();
    let (base, server, client) = app(db.clone()).await;
    (
        scratch,
        db,
        state,
        remote_base,
        remote_server,
        base,
        server,
        client,
    )
}
fn feed_hits(state: &Remote) -> usize {
    state.feed_requests.lock().unwrap().len()
}

#[tokio::test]
async fn configuration_contract_search_flags_and_secret_handling() {
    let (_scratch, db, _state, remote_base, _remote, base, _server, _client) = harness().await;
    let created = create_feed(&base, &remote_base, Some(scope(Some(5))), Some(scope(None))).await;
    assert_eq!(created["settings"]["implementation"], "torrentrss");
    assert_eq!(
        created["settings"]["tv"],
        json!({"enable_rss":true,"minimum_seeders":5})
    );
    assert_eq!(
        created["settings"]["movies"],
        json!({"enable_rss":true,"minimum_seeders":null})
    );
    assert_eq!(created["has_credentials"], true);
    // Search flags and a client binding do not exist for feeds: they are rejected, not ignored.
    for extra in [
        json!({"enable_rss":true,"enable_automatic_search":true}),
        json!({"enable_rss":true,"enable_interactive_search":false}),
        json!({"enable_rss":true,"download_client_id":"11111111-1111-4111-8111-111111111111"}),
        json!({"enable_rss":true,"categories":[5030]}),
        json!({"enable_rss":true,"minimum_seeders":-1}),
        json!({"enable_rss":true,"minimum_seeders":1000001}),
        json!({"enable_rss":true,"minimum_seeders":"5"}),
        json!({"enable_rss":"yes"}),
    ] {
        let (code, error) = request(
            &base,
            "POST",
            "/api/v1/providers",
            feed_body(&remote_base, Some(extra.clone()), None),
        )
        .await;
        assert!(code == 400 || code == 422, "{extra}: {code} {error}");
    }
    // Defaults: RSS is on and the minimum is optional.
    let (code, defaults) = request(
        &base,
        "POST",
        "/api/v1/providers",
        feed_body(
            &remote_base,
            Some(json!({})),
            Some(json!({"enable_rss":false})),
        ),
    )
    .await;
    assert_eq!(code, 201, "{defaults}");
    assert_eq!(
        defaults["settings"]["tv"],
        json!({"enable_rss":true,"minimum_seeders":null})
    );
    assert_eq!(defaults["settings"]["movies"]["enable_rss"], false);
    // Scope presence is the domain opt-in: at least one is required; boundaries are inclusive.
    for (tv, movies) in [(None, None)] {
        let (code, _) = request(
            &base,
            "POST",
            "/api/v1/providers",
            feed_body(&remote_base, tv, movies),
        )
        .await;
        assert_eq!(code, 400);
    }
    let (code, edge) = request(
        &base,
        "POST",
        "/api/v1/providers",
        feed_body(&remote_base, None, Some(scope(Some(1_000_000)))),
    )
    .await;
    assert_eq!(code, 201, "{edge}");
    // Endpoint rules are the shared Torznab rules: no query (secrets live in credentials), no
    // userinfo, http(s) only.
    for endpoint in [
        format!("{remote_base}/feed?passkey=PRIVATE_PASSKEY"),
        "ftp://tracker.example/feed".to_string(),
        "https://user:pw@tracker.example/feed".to_string(),
        format!("{remote_base}/feed#fragment"),
        " https://tracker.example/feed".to_string(),
        "not a url".to_string(),
    ] {
        let mut body = feed_body(&remote_base, Some(scope(None)), None);
        body["settings"]["endpoint"] = json!(endpoint);
        let (code, error) = request(&base, "POST", "/api/v1/providers", body).await;
        assert_eq!(code, 400, "{endpoint}: {error}");
    }
    // Credentials: only the feed kind is accepted for a feed, and a feed kind only for a feed.
    for bad in [
        json!({"kind":"api_key","api_key":"PRIVATE_PASSKEY"}),
        json!({"kind":"username_password","username":"u","password":"PRIVATE_PASSKEY"}),
        json!({"kind":"indexer","api_key":"PRIVATE_PASSKEY"}),
        json!({"kind":"feed"}),
        json!({"kind":"feed","cookie":"a\r\nX-Injected: 1"}),
        json!({"kind":"feed","cookie":"caf\u{e9}=1"}),
        json!({"kind":"feed","cookie":""}),
        json!({"kind":"feed","cookie":"x".repeat(4097)}),
        json!({"kind":"feed","tv_parameters":[{"name":"a","value":"1"},{"name":"A","value":"2"}]}),
        json!({"kind":"feed","tv_parameters":[{"name":"1bad","value":"1"}]}),
        json!({"kind":"feed","tv_parameters":(0..17).map(|n| json!({"name":format!("p{n}"),"value":"v"})).collect::<Vec<_>>()}),
        json!({"kind":"feed","movie_parameters":[{"name":"p","value":"line\nbreak"}]}),
        json!({"kind":"feed","cookie":"c","unknown":1}),
    ] {
        let mut body = feed_body(&remote_base, Some(scope(None)), None);
        body["credentials"] = bad.clone();
        let (code, error) = request(&base, "POST", "/api/v1/providers", body).await;
        assert!(code == 400 || code == 422, "{bad}: {code} {error}");
    }
    let (code, wrong_owner) = request(
        &base,
        "POST",
        "/api/v1/providers",
        json!({"name":"client","enabled":true,"priority":1,"settings":{"implementation":"qbittorrent","endpoint":remote_base,
            "tv":{"category":"tv","imported_category":null,"recent_priority":0,"older_priority":0},"movies":null},"credentials":feed_credentials()}),
    )
    .await;
    assert!(
        code == 400 || code == 422,
        "a feed credential cannot be attached to a client: {wrong_owner}"
    );
    // Even reserved Torznab names are fine as feed parameters (no Torznab vocabulary to protect).
    let mut body = feed_body(&remote_base, Some(scope(None)), None);
    body["credentials"] = json!({"kind":"feed","tv_parameters":[{"name":"cat","value":"5"},{"name":"t","value":"x"}]});
    assert_eq!(
        request(&base, "POST", "/api/v1/providers", body).await.0,
        201
    );

    // Secrets never appear in responses (asserted inside `request`), nor in storage in plaintext.
    let id = created["id"].as_str().unwrap();
    let (_, detail) = request(
        &base,
        "GET",
        &format!("/api/v1/providers/{id}"),
        Value::Null,
    )
    .await;
    assert_eq!(detail["has_credentials"], true);
    let (_, listed) = request(&base, "GET", "/api/v1/providers", Value::Null).await;
    assert!(listed["items"].as_array().unwrap().len() >= 4);
    let c = db.connect().await.unwrap();
    let blob: Vec<u8> = c
        .query("SELECT credentials FROM providers WHERE id=?", [id])
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    for secret in [
        COOKIE.as_bytes(),
        PASSKEY.as_bytes(),
        b"PRIVATE_COOKIE".as_slice(),
    ] {
        assert!(
            !blob.windows(secret.len()).any(|w| w == secret),
            "credential column is sealed"
        );
    }
    let stored: (
        String,
        Option<i64>,
        i64,
        i64,
        Option<String>,
        Option<String>,
    ) = {
        let row = c.query("SELECT implementation,minimum_seeders,enable_automatic_search,enable_interactive_search,categories,download_client_id FROM provider_scopes WHERE provider_id=? AND media_type='tv'", [id]).await.unwrap().next().await.unwrap().unwrap();
        (
            row.get(0).unwrap(),
            row.get(1).unwrap(),
            row.get(2).unwrap(),
            row.get(3).unwrap(),
            row.get(4).unwrap(),
            row.get(5).unwrap(),
        )
    };
    assert_eq!(
        stored,
        ("torrentrss".to_string(), Some(5), 0, 0, None, None)
    );

    // Update: revision-fenced; omitted credentials are preserved, null clears, replacement seals.
    let mut update = feed_body(&remote_base, Some(scope(Some(7))), None);
    update.as_object_mut().unwrap().remove("credentials");
    update["revision"] = created["revision"].clone();
    update["name"] = json!("feed renamed");
    let (code, updated) = request(
        &base,
        "PUT",
        &format!("/api/v1/providers/{id}"),
        update.clone(),
    )
    .await;
    assert_eq!(code, 200, "{updated}");
    assert_eq!(
        updated["has_credentials"], true,
        "an omitted credential is preserved"
    );
    assert_eq!(updated["settings"]["tv"]["minimum_seeders"], 7);
    assert!(updated["settings"]["movies"].is_null());
    assert_eq!(
        request(
            &base,
            "PUT",
            &format!("/api/v1/providers/{id}"),
            update.clone()
        )
        .await
        .0,
        409,
        "stale revision"
    );
    // A feed cannot be turned into another implementation by an update.
    let mut switched = json!({"revision":updated["revision"],"name":"x","enabled":true,"priority":1,
        "settings":{"implementation":"torznab","endpoint":remote_base,"tv":{"categories":[5030],"anime_categories":[]},"movies":null}});
    assert_eq!(
        request(
            &base,
            "PUT",
            &format!("/api/v1/providers/{id}"),
            switched.clone()
        )
        .await
        .0,
        409
    );
    switched["settings"] = updated["settings"].clone();
    switched["credentials"] = Value::Null;
    let (code, cleared) = request(&base, "PUT", &format!("/api/v1/providers/{id}"), switched).await;
    assert_eq!(code, 200, "{cleared}");
    assert_eq!(cleared["has_credentials"], false);
    // Deleting removes scopes with it.
    let (code, _) = request(
        &base,
        "DELETE",
        &format!("/api/v1/providers/{id}?revision={}", cleared["revision"]),
        Value::Null,
    )
    .await;
    assert_eq!(code, 204);
    let left: i64 = c
        .query(
            "SELECT count(*) FROM provider_scopes WHERE provider_id=?",
            [id],
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(left, 0);
    // The schema endpoint advertises the template with feed defaults and no search flags.
    let (code, schema) = request(
        &base,
        "GET",
        "/api/v1/providers/schema?media_type=tv&kind=indexer",
        Value::Null,
    )
    .await;
    assert_eq!(code, 200, "{schema}");
    let template = schema["templates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["implementation"] == "torrentrss")
        .expect("torrentrss template");
    assert_eq!(template["defaults"]["kind"], "feed");
    assert_eq!(
        template["defaults"]["tv"],
        json!({"enable_rss":true,"minimum_seeders":null})
    );
}

#[tokio::test]
async fn test_route_reports_stats_and_typed_failures_bound_to_the_revision() {
    let (_scratch, _db, state, remote_base, _remote, base, _server, _client) = harness().await;
    let feed = create_feed(&base, &remote_base, Some(scope(None)), Some(scope(None))).await;
    let id = feed["id"].as_str().unwrap();
    let path = format!("/api/v1/providers/{id}/test");
    let (code, ok) = request(&base, "POST", &path, Value::Null).await;
    assert_eq!(code, 200, "{ok}");
    assert_eq!(ok["revision"], 1);
    assert_eq!(
        ok["result"]["domains"],
        json!([{"media_type":"tv","parsed":1,"rejected":0,"below_minimum_seeders":0},{"media_type":"movies","parsed":1,"rejected":0,"below_minimum_seeders":0}])
    );
    // The mock saw one authenticated request per domain with its own private parameters and cookie.
    {
        let seen = state.feed_requests.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(
            seen[0].0.contains("kind=tv") && seen[1].0.contains("kind=movies"),
            "{seen:?}"
        );
        assert!(
            seen.iter()
                .all(|(uri, cookie)| uri.contains("passkey=PRIVATE_PASSKEY")
                    && cookie.as_deref() == Some(COOKIE))
        );
    }
    let (_, read) = request(
        &base,
        "GET",
        &format!("/api/v1/providers/{id}"),
        Value::Null,
    )
    .await;
    assert_eq!(
        (
            read["test_status"].as_str(),
            read["last_test"]["revision"].as_i64()
        ),
        (Some("success"), Some(1))
    );
    // A draft (unsaved) configuration is tested through the same probe and records no observation.
    let (code, draft) = request(
        &base,
        "POST",
        "/api/v1/providers/test-draft",
        json!({"source":null,"config":feed_body(&remote_base, Some(scope(None)), None)}),
    )
    .await;
    assert_eq!(code, 200, "{draft}");
    assert_eq!(
        draft["result"]["domains"],
        json!([{"media_type":"tv","parsed":1,"rejected":0,"below_minimum_seeders":0}])
    );
    assert_eq!(feed_hits(&state), 3);
    // Items that are malformed are counted, not fatal, when the feed itself is usable.
    state.feed_mode.store(MALFORMED_ITEM, Ordering::SeqCst);
    let (code, mixed) = request(&base, "POST", &path, Value::Null).await;
    assert_eq!(code, 200, "{mixed}");
    assert_eq!(
        mixed["result"]["domains"][0],
        json!({"media_type":"tv","parsed":1,"rejected":2,"below_minimum_seeders":0})
    );
    // An empty but well-formed feed is valid evidence of access.
    state.feed_mode.store(EMPTY, Ordering::SeqCst);
    let (code, empty) = request(&base, "POST", &path, Value::Null).await;
    assert_eq!(code, 200, "{empty}");
    assert_eq!(empty["result"]["domains"][1]["parsed"], 0);
    // Typed failures, each persisted against the tested revision.
    for (mode, status, expected) in [
        (OVERSIZED, 502, "response_too_large"),
        (DTD, 502, "invalid_response"),
        (HTML, 502, "invalid_response"),
        (SERVER_ERROR, 502, "transport_error"),
        (REDIRECT, 502, "redirect_rejected"),
    ] {
        state.feed_mode.store(mode, Ordering::SeqCst);
        let (code, error) = request(&base, "POST", &path, Value::Null).await;
        assert_eq!(
            (code, error["error"]["code"].as_str()),
            (status, Some(expected)),
            "mode {mode}"
        );
        let (_, read) = request(
            &base,
            "GET",
            &format!("/api/v1/providers/{id}"),
            Value::Null,
        )
        .await;
        assert_eq!(read["test_status"], "failure");
        assert_eq!(read["last_test"]["error_code"], expected);
        assert_eq!(read["last_test"]["revision"], 1);
    }
    state.feed_mode.store(RATE_LIMITED, Ordering::SeqCst);
    assert_eq!(request(&base, "POST", &path, Value::Null).await.0, 429);
    // The rate-limit cooldown is shared provider state: a normal feed is not even requested yet.
    state.feed_mode.store(NORMAL, Ordering::SeqCst);
    let hits = feed_hits(&state);
    let (code, cooling) = request(&base, "POST", &path, Value::Null).await;
    assert_eq!(code, 429, "{cooling}");
    assert_eq!(feed_hits(&state), hits, "no hammering during cooldown");
    // A configuration change invalidates the observation (revision-bound).
    let mut update = feed_body(&remote_base, Some(scope(Some(3))), Some(scope(None)));
    update.as_object_mut().unwrap().remove("credentials");
    update["revision"] = feed["revision"].clone();
    let (code, updated) = request(&base, "PUT", &format!("/api/v1/providers/{id}"), update).await;
    assert_eq!(code, 200, "{updated}");
    assert_eq!(updated["test_status"], "never_tested");
    assert!(updated["last_test"].is_null());
}

#[tokio::test]
async fn authentication_failures_are_typed_and_secret_free() {
    let (_scratch, _db, state, remote_base, _remote, base, _server, _client) = harness().await;
    // Wrong cookie: the mock answers 403 (and echoes the secret name in its body).
    let mut body = feed_body(&remote_base, Some(scope(None)), None);
    body["credentials"] = json!({"kind":"feed","cookie":"uid=7; pass=wrong","tv_parameters":[{"name":"kind","value":"tv"},{"name":"passkey","value":PASSKEY}]});
    let (code, feed) = request(&base, "POST", "/api/v1/providers", body).await;
    assert_eq!(code, 201, "{feed}");
    let path = format!("/api/v1/providers/{}/test", feed["id"].as_str().unwrap());
    let (code, error) = request(&base, "POST", &path, Value::Null).await;
    assert_eq!(
        (code, error["error"]["code"].as_str()),
        (502, Some("authentication")),
        "{error}"
    );
    // No credentials at all: still a typed authentication failure from the same mock.
    let mut anonymous = feed_body(&remote_base, Some(scope(None)), None);
    anonymous.as_object_mut().unwrap().remove("credentials");
    let (_, bare) = request(&base, "POST", "/api/v1/providers", anonymous).await;
    let (code, error) = request(
        &base,
        "POST",
        &format!("/api/v1/providers/{}/test", bare["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_eq!(
        (code, error["error"]["code"].as_str()),
        (502, Some("authentication")),
        "{error}"
    );
    assert_eq!(feed_hits(&state), 2);
    assert!(
        state.feed_requests.lock().unwrap()[1].1.is_none(),
        "no ambient cookie is sent"
    );
}

#[tokio::test]
async fn search_route_answers_rss_only_and_honours_the_minimum_seeders() {
    let (_scratch, _db, state, remote_base, _remote, base, _server, _client) = harness().await;
    state.feed_mode.store(SEEDERS, Ordering::SeqCst);
    let feed = create_feed(&base, &remote_base, Some(scope(Some(5))), Some(scope(None))).await;
    let id = feed["id"].as_str().unwrap();
    let search = format!("/api/v1/providers/{id}/search");
    let (code, page) = request(
        &base,
        "POST",
        &search,
        json!({"kind":"rss","media_type":"tv","limit":10}),
    )
    .await;
    assert_eq!(code, 200, "{page}");
    let titles: Vec<_> = page["page"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["title"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        titles,
        ["High.Seeds.S01E01.1080p", "Unknown.Seeds.S01E01.1080p"],
        "unknown seeders are kept, low ones skipped"
    );
    assert_eq!(page["page"]["total"], 2);
    assert_eq!(page["page"]["next_offset"], Value::Null);
    // Movies has no minimum: everything passes.
    let (_, movies) = request(
        &base,
        "POST",
        &search,
        json!({"kind":"rss","media_type":"movies","limit":10}),
    )
    .await;
    assert_eq!(movies["page"]["items"].as_array().unwrap().len(), 3);
    // Local paging over the single document.
    let (_, first) = request(
        &base,
        "POST",
        &search,
        json!({"kind":"rss","media_type":"movies","limit":2}),
    )
    .await;
    assert_eq!(
        (
            first["page"]["items"].as_array().unwrap().len(),
            first["page"]["next_offset"].as_u64()
        ),
        (2, Some(2))
    );
    let (_, second) = request(
        &base,
        "POST",
        &search,
        json!({"kind":"rss","media_type":"movies","limit":2,"offset":2}),
    )
    .await;
    assert_eq!(
        (
            second["page"]["items"].as_array().unwrap().len(),
            second["page"]["next_offset"].clone()
        ),
        (1, Value::Null)
    );
    // Feeds cannot search: the stored flags are zero, so interactive/automatic requests are refused
    // before any network call.
    let hits = feed_hits(&state);
    for kind in [
        json!({"kind":"tv","title":"Harbor","numbering":{"kind":"episode","season":1,"episode":1}}),
        json!({"kind":"movie","title":"Harbor","year":2020}),
    ] {
        let (code, error) = request(&base, "POST", &search, kind).await;
        assert_eq!(
            (code, error["error"]["code"].as_str()),
            (409, Some("provider_unavailable")),
            "{error}"
        );
    }
    assert_eq!(feed_hits(&state), hits);
    // A domain without a scope is out of scope for RSS too.
    let tv_only = create_feed(&base, &remote_base, Some(scope(None)), None).await;
    let (code, error) = request(
        &base,
        "POST",
        &format!(
            "/api/v1/providers/{}/search",
            tv_only["id"].as_str().unwrap()
        ),
        json!({"kind":"rss","media_type":"movies"}),
    )
    .await;
    assert_eq!(
        (code, error["error"]["code"].as_str()),
        (409, Some("provider_unavailable")),
        "{error}"
    );
    // A scope with RSS disabled is unavailable as well.
    let disabled = create_feed(&base, &remote_base, Some(json!({"enable_rss":false})), None).await;
    let (code, _) = request(
        &base,
        "POST",
        &format!(
            "/api/v1/providers/{}/search",
            disabled["id"].as_str().unwrap()
        ),
        json!({"kind":"rss","media_type":"tv"}),
    )
    .await;
    assert_eq!(code, 409);
    // Oversized pages are refused and bounded.
    assert_eq!(
        request(
            &base,
            "POST",
            &search,
            json!({"kind":"rss","media_type":"tv","limit":501})
        )
        .await
        .0,
        400
    );
}

#[tokio::test]
async fn rss_sync_grabs_both_domains_with_colliding_ids_and_replay_does_not_duplicate() {
    let (scratch, db, state, remote_base, _remote, base, _server, client) = harness().await;
    seed(&db).await;
    let _keep = &scratch;
    let feed = create_feed(&base, &remote_base, Some(scope(None)), Some(scope(None))).await;
    let download = create_client(&base, &remote_base).await;
    let runtime = commands::start(db.clone(), client).await.unwrap();
    // A malformed item in an otherwise valid feed does not fail the sync (unlike native Torznab).
    state.feed_mode.store(MALFORMED_ITEM, Ordering::SeqCst);
    for media in ["tv", "movies"] {
        let command = enqueue(&base, target(&feed, &download, media)).await;
        let done = until(&base, &command["id"]).await;
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
        assert_eq!(done["status"], "succeeded", "{done}");
        assert_eq!(done["observed"], 1, "{done} {receipts}");
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
        let replay = enqueue(&base, target(&feed, &download, media)).await;
        assert_eq!(until(&base, &replay["id"]).await["status"], "succeeded");
    }
    assert_eq!(
        state.adds.lock().unwrap().as_slice(),
        [TV, MOVIE],
        "replay performs no second external grab"
    );
    {
        let seen = state.feed_requests.lock().unwrap();
        assert_eq!(seen.len(), 4, "one authenticated fetch per command");
        assert!(
            seen.iter()
                .all(|(uri, cookie)| uri.contains("passkey=PRIVATE_PASSKEY")
                    && cookie.as_deref() == Some(COOKIE))
        );
        assert_eq!(
            seen.iter()
                .filter(|(uri, _)| uri.contains("kind=tv"))
                .count(),
            2
        );
    }
    // A recurring schedule is accepted for a feed, per domain.
    let (code, schedule) = request(
        &base,
        "POST",
        "/api/v1/rss/schedules",
        json!({"target":target(&feed,&download,"tv"),"interval_seconds":600,"enabled":true}),
    )
    .await;
    assert_eq!(code, 200, "{schedule}");

    // Policy: nothing is polled for a disabled provider, a disabled or absent scope, or a stale revision.
    let hits = feed_hits(&state);
    let id = feed["id"].as_str().unwrap();
    let refuse = |value: Value| {
        let base = base.clone();
        async move {
            let (code, v) = request(
                &base,
                "POST",
                "/api/v1/rss/commands",
                json!({"target":value,"priority":"normal"}),
            )
            .await;
            assert_eq!(code, 409, "{v}");
        }
    };
    let mut update = feed_body(&remote_base, Some(scope(None)), None);
    update.as_object_mut().unwrap().remove("credentials");
    update["revision"] = feed["revision"].clone();
    let (code, tv_only) = request(&base, "PUT", &format!("/api/v1/providers/{id}"), update).await;
    assert_eq!(code, 200, "{tv_only}");
    refuse(target(&feed, &download, "tv")).await; // stale revision
    refuse(target(&tv_only, &download, "movies")).await; // absent scope
    let mut off = feed_body(&remote_base, Some(json!({"enable_rss":false})), None);
    off.as_object_mut().unwrap().remove("credentials");
    off["revision"] = tv_only["revision"].clone();
    let (code, rss_off) = request(&base, "PUT", &format!("/api/v1/providers/{id}"), off).await;
    assert_eq!(code, 200, "{rss_off}");
    refuse(target(&rss_off, &download, "tv")).await; // scope RSS disabled
    let mut disabled = feed_body(&remote_base, Some(scope(None)), None);
    disabled.as_object_mut().unwrap().remove("credentials");
    disabled["enabled"] = json!(false);
    disabled["revision"] = rss_off["revision"].clone();
    let (code, provider_off) =
        request(&base, "PUT", &format!("/api/v1/providers/{id}"), disabled).await;
    assert_eq!(code, 200, "{provider_off}");
    refuse(target(&provider_off, &download, "tv")).await; // provider disabled
    // The earlier schedule is fenced by the revision change: storage disabled it, so it cannot fire.
    let conn = db.connect().await.unwrap();
    let row = conn
        .query("SELECT enabled,error_code FROM rss_schedules", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .expect("the schedule exists");
    assert_eq!(
        (row.get::<i64>(0).unwrap(), row.get::<String>(1).unwrap()),
        (0, "provider_changed".to_string())
    );
    drop(row);
    conn.execute("UPDATE rss_schedules SET next_run_at=0", ())
        .await
        .expect("a disabled schedule can still be nudged");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        feed_hits(&state),
        hits,
        "no feed request for any refused or fenced target"
    );
    assert_eq!(state.adds.lock().unwrap().as_slice(), [TV, MOVIE]);
    runtime.shutdown().await;
}

#[tokio::test]
async fn sync_failures_surface_typed_errors_without_grabbing() {
    let (scratch, db, state, remote_base, _remote, base, _server, client) = harness().await;
    seed(&db).await;
    let _keep = &scratch;
    let feed = create_feed(&base, &remote_base, Some(scope(None)), None).await;
    let download = create_client(&base, &remote_base).await;
    let runtime = commands::start(db.clone(), client).await.unwrap();
    // A feed with items but none usable (invalid whole feed) fails the command, as do hostile bodies.
    for mode in [DTD, HTML] {
        state.feed_mode.store(mode, Ordering::SeqCst);
        let command = enqueue(&base, target(&feed, &download, "tv")).await;
        // A failed fetch is retried later (30 s back-off), never grabbed and never silently dropped.
        let done = tokio::time::timeout(Duration::from_secs(18), async {
            loop {
                let (_, v) = request(
                    &base,
                    "GET",
                    &format!("/api/v1/rss/commands/{}", command["id"].as_str().unwrap()),
                    Value::Null,
                )
                .await;
                if matches!(v["status"].as_str(), Some("retry_wait" | "failed")) {
                    return v;
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .expect("failure settles");
        assert_eq!(done["error_code"], "refresh_failed", "mode {mode}: {done}");
        assert_eq!(done["fetched"], 0);
        assert!(state.adds.lock().unwrap().is_empty());
        let (code, _) = request(
            &base,
            "POST",
            &format!(
                "/api/v1/rss/commands/{}/cancel",
                command["id"].as_str().unwrap()
            ),
            Value::Null,
        )
        .await;
        assert!(code == 200 || code == 204 || code == 202, "{code}");
    }
    runtime.shutdown().await;
}

#[tokio::test]
async fn health_detectors_tolerate_feeds_next_to_clients() {
    use hrrdarr::{api::MediaDomain, health_detectors};
    let (_scratch, db, state, remote_base, _remote, base, _server, client) = harness().await;
    let _feed = create_feed(&base, &remote_base, Some(scope(None)), Some(scope(None))).await;
    let _download = create_client(&base, &remote_base).await;
    let hits = feed_hits(&state);
    for domain in [MediaDomain::Tv, MediaDomain::Movies] {
        // Client-oriented checks select clients positively: an indexer-like row is never mistaken for
        // an unsupported client (which would make the whole check unevaluable).
        health_detectors::evaluate_current(&db, &client, domain)
            .await
            .expect("completed download check evaluates");
        health_detectors::evaluate_communication(&db, &client, domain)
            .await
            .expect("communication check evaluates");
        assert!(
            health_detectors::evaluate_indexer_client(&db, domain)
                .await
                .expect("binding check evaluates")
                .is_none(),
            "feeds are never bound to a client"
        );
    }
    assert_eq!(feed_hits(&state), hits, "health checks never poll a feed");
}

#[tokio::test]
async fn enclosure_only_item_is_downloaded_by_the_client_transport_and_grabbed_once() {
    let (scratch, db, state, remote_base, _remote, base, _server, client) = harness().await;
    seed(&db).await;
    let _keep = &scratch;
    state.feed_mode.store(ENCLOSURE_ONLY, Ordering::SeqCst);
    let feed = create_feed(&base, &remote_base, Some(scope(None)), None).await;
    let download = create_client(&base, &remote_base).await;
    let runtime = commands::start(db.clone(), client).await.unwrap();
    for _ in 0..2 {
        let command = enqueue(&base, target(&feed, &download, "tv")).await;
        let done = until(&base, &command["id"]).await;
        assert_eq!(done["status"], "succeeded", "{done}");
    }
    assert_eq!(
        state.adds.lock().unwrap().as_slice(),
        [FILE_HASH],
        "the .torrent bytes were uploaded exactly once across a replay"
    );
    // The download link keeps its private query, but it is fetched by the client's lane without the
    // feed cookie: cookie-only download links are a documented limitation of this slice.
    let seen = state.torrent_requests.lock().unwrap();
    // Reasoning: the existing grab pipeline prepares a candidate twice (once to journal the submission identity,
    // once immediately before submitting); the replay command fetched nothing, so two requests in total.
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert!(
        seen.iter()
            .all(|(uri, _)| uri.contains("passkey=PRIVATE_RSS_LOCATOR"))
    );
    assert!(seen.iter().all(|(_, cookie)| cookie.is_none()));
    drop(seen);
    runtime.shutdown().await;
}

#[tokio::test]
async fn bulk_administration_and_test_all_treat_feeds_as_indexers() {
    let (_scratch, _db, state, remote_base, _remote, base, _server, _client) = harness().await;
    let first = create_feed(&base, &remote_base, Some(scope(None)), None).await;
    let second = create_feed(&base, &remote_base, Some(scope(None)), Some(scope(None))).await;
    let client = create_client(&base, &remote_base).await;
    let items = |values: &[&Value]| -> Vec<Value> {
        values
            .iter()
            .map(|v| json!({"id":v["id"],"revision":v["revision"]}))
            .collect()
    };
    // test-all selects enabled indexers of the domain, feeds included, and reports typed outcomes.
    let (code, batch) = request(
        &base,
        "POST",
        "/api/v1/providers/testall",
        json!({"media_type":"tv","kind":"indexer"}),
    )
    .await;
    assert_eq!(code, 200, "{batch}");
    assert_eq!(batch["items"].as_array().unwrap().len(), 2, "{batch}");
    assert!(
        batch["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["outcome"]["status"] == "success"),
        "{batch}"
    );
    let (_, movies_only) = request(
        &base,
        "POST",
        "/api/v1/providers/testall",
        json!({"media_type":"movies","kind":"indexer"}),
    )
    .await;
    assert_eq!(
        movies_only["items"].as_array().unwrap().len(),
        1,
        "only the feed with a movie scope"
    );
    // Reasoning: test-all fetches every configured scope of every selected feed: 1 (tv-only feed) + 2 (tv and
    // movies feed) for the TV batch and 2 more for the movies batch.
    assert_eq!(feed_hits(&state), 5);
    // A feed is an indexer, never a download client, for selection and bulk edits.
    let (code, _) = request(&base, "PUT", "/api/v1/providers/bulk", json!({"media_type":"tv","kind":"download_client","items":items(&[&first]),"changes":{"enabled":false}})).await;
    assert_eq!(code, 400);
    let (code, _) = request(&base, "PUT", "/api/v1/providers/bulk", json!({"media_type":"movies","kind":"indexer","items":items(&[&first]),"changes":{"enabled":false}})).await;
    assert_eq!(code, 400, "the first feed has no movie scope");
    let (code, updated) = request(&base, "PUT", "/api/v1/providers/bulk", json!({"media_type":"tv","kind":"indexer","items":items(&[&first, &second]),"changes":{"enabled":false,"priority":7}})).await;
    assert_eq!(code, 200, "{updated}");
    for item in updated["items"].as_array().unwrap() {
        assert_eq!(
            (item["enabled"].clone(), item["priority"].clone()),
            (json!(false), json!(7))
        );
        assert_eq!(item["settings"]["implementation"], "torrentrss");
        assert_eq!(item["revision"], 2);
    }
    // Disabled feeds drop out of test-all; the bulk delete honours the same selection rules.
    let (_, none) = request(
        &base,
        "POST",
        "/api/v1/providers/testall",
        json!({"media_type":"tv","kind":"indexer"}),
    )
    .await;
    assert_eq!(none["items"], json!([]));
    let (code, _) = request(
        &base,
        "DELETE",
        "/api/v1/providers/bulk",
        json!({"media_type":"tv","kind":"indexer","items":items(&[&client])}),
    )
    .await;
    assert_eq!(code, 400, "a client is not an indexer");
    let refreshed = |v: &Value| json!({"id":v["id"],"revision":2});
    let (code, _) = request(
        &base,
        "DELETE",
        "/api/v1/providers/bulk",
        json!({"media_type":"tv","kind":"indexer","items":[refreshed(&first),refreshed(&second)]}),
    )
    .await;
    assert_eq!(code, 204);
    let (_, listed) = request(&base, "GET", "/api/v1/providers", Value::Null).await;
    assert_eq!(
        listed["items"].as_array().unwrap().len(),
        1,
        "only the client remains"
    );
}
