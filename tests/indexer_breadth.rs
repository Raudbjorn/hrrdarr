//! Protocol tests for the nine tracker indexers (Nyaa, Fanzub, IPTorrents, Torrentleech,
//! BroadcastheNet, FileList, HDBits, PassThePopcorn, TorrentPotato) against one in-process fake
//! server. The response documents follow each tracker's documented format as remembered while
//! implementing; no real tracker was contacted, so they prove the implementation against these
//! fixtures, not against the live services.
use axum::{
    body::Bytes,
    extract::{OriginalUri, State},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
};
use hrrdarr::{commands, db::Database, providers};
use serde_json::{Value, json};
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
struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}

// Every one of these is a credential or a server-side locator secret: none may appear in any API
// response, stored plaintext column or error.
const COOKIE: &str = "uid=7; pass=PRIVATE_COOKIE";
const PASSKEY: &str = "PRIVATE_PASSKEY";
const APIKEY: &str = "PRIVATE_APIKEY";
const RSSKEY: &str = "PRIVATE_RSSKEY";
const SECRETS: [&str; 6] = [
    "PRIVATE_COOKIE",
    "PRIVATE_PASSKEY",
    "PRIVATE_APIKEY",
    "PRIVATE_RSSKEY",
    "PRIVATE_LOCATOR",
    "PRIVATE_AUTH",
];
const TORRENT_FILE: &str =
    "d4:infod6:lengthi1e4:name1:x12:piece lengthi16384e6:pieces20:aaaaaaaaaaaaaaaaaaaaee";
// SHA-1 of the exact bencoded info dictionary served above.
const FILE_HASH: &str = "fd1ecef9f83ef3d11c63557b271320a134a6add7";
// A second torrent (served by the HDBits download endpoint) so both grabs are distinct downloads.
const TORRENT_FILE_2: &str =
    "d4:infod6:lengthi1e4:name1:y12:piece lengthi16384e6:pieces20:aaaaaaaaaaaaaaaaaaaaee";
const FILE_HASH_2: &str = "af2b014f845a800410e7b00cf888c38b8eecef47";

const NORMAL: u8 = 0;
const AUTH: u8 = 1;
const RATE_LIMITED: u8 = 2;
const SERVER_ERROR: u8 = 3;
const REDIRECT: u8 = 4;
const GARBAGE: u8 = 5;
const OVERSIZED: u8 = 6;
const EMPTY: u8 = 7;
const PAGED: u8 = 8;

#[derive(Clone, Debug)]
struct Seen {
    path: String,
    query: HashMap<String, String>,
    headers: HashMap<String, String>,
    body: String,
}
#[derive(Default)]
struct Remote {
    seen: Mutex<Vec<Seen>>,
    mode: AtomicU8,
    endpoint: Mutex<String>,
    adds: Mutex<Vec<String>>,
    items: Mutex<Vec<Value>>,
}
impl Remote {
    fn hits(&self, prefix: &str) -> Vec<Seen> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.path.starts_with(prefix))
            .cloned()
            .collect()
    }
    fn mode(&self, mode: u8) {
        self.mode.store(mode, Ordering::SeqCst);
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
async fn request(base: &str, method: &str, path: &str, body: Value) -> (u16, Value) {
    let response = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(8))
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
        !SECRETS.iter().any(|secret| text.contains(secret)),
        "a secret or private locator reached an API response: {text}"
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

fn rss_doc(items: &str, extra_ns: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><rss version="2.0" {extra_ns}><channel><title>feed</title>{items}</channel></rss>"#
    )
}
fn nyaa_doc(base: &str, term: Option<&str>) -> String {
    let item = |title: &str, id: u32| {
        format!(
            r#"<item><title>{title}</title><link>{base}/dl/file.torrent</link><guid isPermaLink="true">{base}/nyaa/view/{id}</guid><pubDate>Mon, 01 Jan 2024 12:00:00 -0000</pubDate><nyaa:seeders>120</nyaa:seeders><nyaa:leechers>4</nyaa:leechers><nyaa:infoHash>{FILE_HASH}</nyaa:infoHash><nyaa:size>1.4 GiB</nyaa:size></item>"#
        )
    };
    let items = match term {
        Some(term) => item(&format!("Echo.{}.1080p", term.replace(' ', ".")), 9),
        None => format!(
            "{}{}",
            item("Harbor.S01E01.1080p.WEB-DL", 1),
            item("Harbor.2020.1080p.WEB-DL", 2)
        ),
    };
    rss_doc(&items, r#"xmlns:nyaa="https://nyaa.si/xmlns/nyaa""#)
}
fn fanzub_doc(base: &str, term: Option<&str>) -> String {
    let title = match term {
        Some(term) => format!("Echo.{}.720p", term.replace(' ', ".")),
        None => "Harbor.S01E01.720p.HDTV".to_string(),
    };
    rss_doc(
        &format!(
            r#"<item><title>{title}</title><link>{base}/fanzub/browse/5</link><guid>{base}/fanzub/browse/5</guid><pubDate>Mon, 01 Jan 2024 12:00:00 +0000</pubDate><enclosure url="{base}/fanzub/nzb/5.nzb" length="367001600" type="application/x-nzb"/></item>"#
        ),
        "",
    )
}
fn private_feed_doc(base: &str, title: &str) -> String {
    rss_doc(
        &format!(
            r#"<item><title>{title}</title><link>{base}/dl/file.torrent</link><description>800.5 MB; TV/x264</description><pubDate>Mon, 01 Jan 2024 12:00:00 +0000</pubDate><guid>{base}/details/77</guid></item>"#
        ),
        "",
    )
}
fn btn_doc(base: &str) -> Value {
    json!({"id":"hrrdarr","result":{"results":"1","torrents":{"4001":{"GroupName":"S01E01","GroupID":"7","TorrentID":"4001","SeriesID":"5","Series":"Harbor","Category":"Episode",
        "Snatched":"1","Seeders":"31","Leechers":"2","Source":"WEB-DL","Container":"MKV","Codec":"H.264","Resolution":"1080p","Origin":"Scene",
        "ReleaseName":"Harbor.S01E01.1080p.WEB-DL","Size":"1610612736","Time":"1704067200","TvdbID":"101","TvrageID":"0","ImdbID":"0","InfoHash":FILE_HASH,
        "DownloadURL":format!("{base}/dl/file.torrent?authkey=PRIVATE_AUTH&torrent_pass=PRIVATE_LOCATOR")}}}})
}
fn filelist_doc(base: &str, tv: bool, term: Option<&str>) -> Value {
    let name = match term {
        Some(term) => format!("Echo.{}.1080p", term.replace(' ', ".")),
        None if tv => "Harbor.S01E01.1080p.WEB-DL".to_string(),
        None => "Harbor.2020.1080p.WEB-DL".to_string(),
    };
    json!([{"id":321,"name":name,"imdb":"tt0123456","freeleech":0,"internal":0,"moderated":1,"upload_date":"2024-07-01 12:00:00",
        "download_link":format!("{base}/dl/file.torrent?passkey=PRIVATE_LOCATOR"),"size":4294967296u64,"leechers":3,"seeders":40,"times_completed":9,"comments":0,"files":3,"category":"Filme HD"}])
}
fn hdbits_doc(tv: bool, term: Option<&str>) -> Value {
    let name = match term {
        Some(term) => format!("Echo.{}.1080p", term.replace(' ', ".")),
        None if tv => "Harbor.S01E01.1080p.WEB-DL".to_string(),
        None => "Harbor.2020.1080p.WEB-DL".to_string(),
    };
    json!({"status":0,"data":[{"id":9001,"hash":FILE_HASH_2,"leechers":1,"seeders":25,"name":name,"times_completed":3,"size":1073741824u64,
        "utadded":1704067200,"added":"2024-01-01T00:00:00+0000","type_category":if tv {2} else {1},"type_codec":1,"type_medium":3,"type_origin":0,
        "imdb":{"id":123456},"tvdb":{"id":if tv {101} else {0},"season":1,"episode":1},"freeleech":"no"}]})
}
fn ptp_doc(base: &str, term: Option<&str>) -> Value {
    let name = match term {
        Some(term) => format!("Echo.{}.1080p", term.replace(' ', ".")),
        None => "Harbor.2020.1080p.WEB-DL".to_string(),
    };
    json!({"TotalResults":"1","Page":"1","AuthKey":"PRIVATE_AUTH","PassKey":"PRIVATE_LOCATOR","Movies":[{"GroupId":"31337","Title":"Harbor","Year":"2020","ImdbId":"0123456",
        "Torrents":[{"Id":"555","ReleaseName":name,"Source":"WEB","Codec":"x264","Resolution":"1080p","Scene":false,"Size":"8589934592",
            "UploadTime":"2024-01-01 12:00:00","Seeders":"20","Leechers":"1","Snatched":"4","FreeleechType":null}]}],"_base":base})
}
fn potato_doc(base: &str, term: Option<&str>) -> Value {
    let name = match term {
        Some(term) => format!("Echo.{}.1080p", term.replace(' ', ".")),
        None => "Harbor.2020.1080p.WEB-DL".to_string(),
    };
    json!({"results":[{"release_name":name,"torrent_id":"p-1","details_url":format!("{base}/potato/details/1"),
        "download_url":format!("{base}/dl/file.torrent?passkey=PRIVATE_LOCATOR"),"freeleech":false,"type":"movie","size":1400,"leechers":2,"seeders":9,
        "publish_date":"2024-01-01T10:30:00Z","imdb_id":"tt0123456"}],"total_results":1})
}
fn basic(user: &str, secret: &str) -> String {
    // Standard base64 of "user:secret" without an extra dependency.
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = format!("{user}:{secret}").into_bytes();
    let mut out = String::from("Basic ");
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}
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
    let q: HashMap<String, String> =
        url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();
    let path = uri.path().to_string();
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    if let Some(rest) = path.strip_prefix("/qb") {
        return qbit(&s, &method, rest, &q, &headers, &body);
    }
    s.seen.lock().unwrap().push(Seen {
        path: path.clone(),
        query: q.clone(),
        headers: ["cookie", "authorization", "apiuser", "apikey"]
            .iter()
            .filter_map(|name| header(name).map(|v| (name.to_string(), v)))
            .collect(),
        body: String::from_utf8_lossy(&body).into_owned(),
    });
    if path.starts_with("/dl/") {
        return TORRENT_FILE.as_bytes().to_vec().into_response();
    }
    if path == "/download.php" {
        return TORRENT_FILE_2.as_bytes().to_vec().into_response();
    }
    match s.mode.load(Ordering::SeqCst) {
        AUTH => {
            return (
                StatusCode::FORBIDDEN,
                "denied PRIVATE_COOKIE PRIVATE_PASSKEY",
            )
                .into_response();
        }
        RATE_LIMITED => {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                [("retry-after", "120")],
                "slow down",
            )
                .into_response();
        }
        SERVER_ERROR => return (StatusCode::INTERNAL_SERVER_ERROR, "boom").into_response(),
        REDIRECT => {
            return (
                StatusCode::FOUND,
                [("location", "http://127.0.0.1:1/elsewhere")],
                "",
            )
                .into_response();
        }
        GARBAGE => {
            return "<html><body>PRIVATE_PASSKEY please log in</body></html>".into_response();
        }
        OVERSIZED => return "x".repeat(2 * 1024 * 1024).into_response(),
        _ => {}
    }
    let empty = s.mode.load(Ordering::SeqCst) == EMPTY;
    let base = s.endpoint.lock().unwrap().clone();
    let json_body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let term = q
        .get("q")
        .or(q.get("query"))
        .or(q.get("search"))
        .or(q.get("searchstr"))
        .cloned();
    let denied = |status: StatusCode| (status, "denied").into_response();
    match path.as_str() {
        "/nyaa/" => {
            if empty {
                return rss_doc("", r#"xmlns:nyaa="https://nyaa.si/xmlns/nyaa""#).into_response();
            }
            nyaa_doc(&base, term.as_deref()).into_response()
        }
        "/fanzub/rss" => {
            if empty {
                return rss_doc("", "").into_response();
            }
            fanzub_doc(&base, term.as_deref()).into_response()
        }
        "/ipt/t.rss" => {
            if header("cookie").as_deref() != Some(COOKIE)
                || q.get("u").map(String::as_str) != Some("7")
                || q.get("tp").map(String::as_str) != Some(PASSKEY)
                || !q.contains_key("48")
            {
                return denied(StatusCode::FORBIDDEN);
            }
            let title = if q.get("kind").map(String::as_str) == Some("movies") {
                "Harbor.2020.1080p.WEB-DL"
            } else {
                "Harbor.S01E01.1080p.WEB-DL"
            };
            if empty {
                return rss_doc("", "").into_response();
            }
            private_feed_doc(&base, title).into_response()
        }
        p if p.starts_with("/tl/") => {
            if p != format!("/tl/{RSSKEY}") {
                return denied(StatusCode::FORBIDDEN);
            }
            if empty {
                return rss_doc("", "").into_response();
            }
            private_feed_doc(&base, "Harbor.S01E01.1080p.WEB-DL").into_response()
        }
        "/btn/" => {
            assert_eq!(method, Method::POST);
            if json_body["params"][0] != APIKEY {
                return axum::Json(
                    json!({"id":"hrrdarr","error":{"code":-32001,"message":"Invalid API Key"}}),
                )
                .into_response();
            }
            if empty {
                return axum::Json(json!({"result":{"results":"0","torrents":[]}})).into_response();
            }
            if s.mode.load(Ordering::SeqCst) == PAGED {
                let template = btn_doc(&base)["result"]["torrents"]["4001"].clone();
                let mut torrents = serde_json::Map::new();
                for (index, id) in (70..=100)
                    .rev()
                    .enumerate()
                    .take(json_body["params"][2].as_u64().unwrap() as usize)
                {
                    let mut row = template.clone();
                    row["TorrentID"] = json!(id.to_string());
                    row["ReleaseName"] = json!(format!("Harbor.S01E01.Release{id}"));
                    row["Seeders"] = json!(if index < 2 { 0 } else { 20 });
                    torrents.insert(id.to_string(), row);
                }
                return axum::Json(json!({"result":{"torrents":torrents}})).into_response();
            }
            axum::Json(btn_doc(&base)).into_response()
        }
        "/filelist/api.php" => {
            if header("authorization").as_deref() != Some(basic("flist", PASSKEY).as_str()) {
                return denied(StatusCode::UNAUTHORIZED);
            }
            if empty {
                return axum::Json(json!([])).into_response();
            }
            let tv =
                q.get("category").is_some_and(|c| c.starts_with("21")) || q.get("season").is_some();
            axum::Json(filelist_doc(&base, tv, term.as_deref())).into_response()
        }
        "/hdbits/api/torrents" => {
            assert_eq!(method, Method::POST);
            if json_body["username"] != "hdb" || json_body["passkey"] != PASSKEY {
                return axum::Json(json!({"status":5,"message":"Invalid passkey"})).into_response();
            }
            if empty {
                return axum::Json(json!({"status":0,"data":[]})).into_response();
            }
            let tv = json_body["category"] == json!([2]);
            if s.mode.load(Ordering::SeqCst) == PAGED {
                let template = hdbits_doc(tv, None)["data"][0].clone();
                let rows: Vec<_> = (70..=100)
                    .rev()
                    .enumerate()
                    .take(json_body["limit"].as_u64().unwrap() as usize)
                    .map(|(index, id)| {
                        let mut row = template.clone();
                        row["id"] = json!(id);
                        row["name"] = json!(format!("Harbor.S01E01.Release{id}"));
                        row["seeders"] = json!(if index < 2 { 0 } else { 20 });
                        row
                    })
                    .collect();
                return axum::Json(json!({"status":0,"data":rows})).into_response();
            }
            axum::Json(hdbits_doc(tv, json_body["search"].as_str())).into_response()
        }
        "/ptp/torrents.php" => {
            if header("apiuser").as_deref() != Some("ptpuser")
                || header("apikey").as_deref() != Some(APIKEY)
            {
                return denied(StatusCode::FORBIDDEN);
            }
            if empty {
                return axum::Json(json!({"TotalResults":"0","Movies":[]})).into_response();
            }
            axum::Json(ptp_doc(&base, term.as_deref())).into_response()
        }
        "/potato/" => {
            if q.get("user").map(String::as_str) != Some("potuser")
                || q.get("passkey").map(String::as_str) != Some(PASSKEY)
            {
                return axum::Json(json!({"error":"Invalid passkey"})).into_response();
            }
            if empty {
                return axum::Json(json!({"results":[],"total_results":0})).into_response();
            }
            axum::Json(potato_doc(&base, term.as_deref())).into_response()
        }
        _ => (StatusCode::NOT_FOUND, "unexpected owned mock request").into_response(),
    }
}

fn qbit(
    s: &Remote,
    method: &Method,
    path: &str,
    q: &HashMap<String, String>,
    headers: &HeaderMap,
    body: &Bytes,
) -> Response {
    if path.ends_with("webapiVersion") {
        return "2.8.4".into_response();
    }
    if path.ends_with("preferences") {
        return axum::Json(json!({"queueing_enabled":true,"dht":true,"save_path":"/remote","max_ratio_enabled":false,"max_ratio":-1,"max_seeding_time_enabled":false,"max_seeding_time":-1,"max_ratio_act":0})).into_response();
    }
    if path.ends_with("categories") {
        return axum::Json(
            json!({"tv":{"savePath":"/remote/tv"},"movies":{"savePath":"/remote/movies"}}),
        )
        .into_response();
    }
    if path.ends_with("torrents/info") {
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
    if path.ends_with("torrents/add") {
        assert_eq!(method, Method::POST);
        let boundary = headers
            .get("content-type")
            .and_then(|header| header.to_str().ok())
            .and_then(|header| header.split("boundary=").nth(1))
            .expect("the tracker release is added as an uploaded .torrent");
        let body = std::str::from_utf8(body).unwrap();
        let hash = if body.contains(TORRENT_FILE_2) {
            FILE_HASH_2
        } else {
            assert!(body.contains(TORRENT_FILE));
            FILE_HASH
        };
        let category = body
            .split(&format!("--{boundary}"))
            .find_map(|part| {
                let (head, value) = part.split_once("\r\n\r\n")?;
                head.contains("name=\"category\"")
                    .then(|| value.trim_end_matches("\r\n").to_owned())
            })
            .expect("category field");
        s.adds.lock().unwrap().push(format!("{category}:{hash}"));
        s.items.lock().unwrap().push(item(hash, &category));
        return "Ok.".into_response();
    }
    (StatusCode::NOT_FOUND, "unexpected owned qbit request").into_response()
}

// ------------------------------------------------------------------------------------------
// Harness
// ------------------------------------------------------------------------------------------

struct Harness {
    _scratch: Scratch,
    db: Arc<Database>,
    state: Arc<Remote>,
    remote: String,
    base: String,
    client: providers::RefreshClient,
    _servers: (Server, Server),
}
async fn harness() -> Harness {
    let scratch = Scratch(
        std::env::temp_dir().join(format!("hrrdarr-indexer-breadth-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&scratch.0).unwrap();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let state = Arc::new(Remote::default());
    let (remote, remote_server) = serve(
        axum::Router::new()
            .fallback(remote)
            .with_state(state.clone()),
    )
    .await;
    *state.endpoint.lock().unwrap() = remote.clone();
    let key = Arc::new(providers::CredentialKey::from_hex(&"11".repeat(32)).unwrap());
    let (router, client) = providers::router_with_refresh(db.clone(), Some(key));
    let (base, server) = serve(
        router
            .merge(commands::router(db.clone()))
            .merge(hrrdarr::delay_profiles::router(db.clone()))
            .merge(hrrdarr::release_profiles::router(db.clone())),
    )
    .await;
    Harness {
        _scratch: scratch,
        db,
        state,
        remote,
        base,
        client,
        _servers: (remote_server, server),
    }
}
async fn seed(db: &Database) {
    db.connect().await.unwrap().execute_batch("INSERT INTO series(id,tvdb_id,title,path)VALUES(1,101,'Harbor','/owned-fictional-tv'); INSERT INTO seasons(series_id,number)VALUES(1,1); INSERT INTO episodes(id,series_id,season,number,title,runtime,air_date_utc)VALUES(1,1,1,1,'Pilot',45,'2020-01-01 00:00:00'); INSERT INTO movie_metadata(id,tmdb_id,imdb_id,title,year,runtime,digital_release)VALUES(1,201,'tt0123456','Harbor',2020,100,'2020-01-01 00:00:00'); INSERT INTO movies(id,metadata_id,path)VALUES(1,1,'/owned-fictional-movie'); INSERT INTO quality_profiles VALUES(1,'tv','HD'),(2,'movies','HD'); INSERT INTO quality_profile_items(profile_id,media_type,quality_id,position,allowed)VALUES(1,'tv',3,0,1),(2,'movies',3,0,1); INSERT INTO quality_profile_policies(profile_id,media_type,upgrade_allowed,cutoff_quality_id,min_format_score,cutoff_format_score,min_upgrade_format_score,language_id)VALUES(1,'tv',1,3,0,0,1,NULL),(2,'movies',1,3,0,0,1,-1); INSERT INTO library_settings(media_type,series_id,quality_profile_id,series_type,use_scene_numbering)VALUES('tv',1,1,'standard',0); INSERT INTO library_settings(media_type,movie_id,quality_profile_id,minimum_availability)VALUES('movies',1,2,'released'); INSERT INTO release_delay_policies VALUES('tv',0,0,0),('movies',0,0,0);").await.unwrap();
}

/// One tracker under test: its endpoint path, credentials, which domains it serves and how it behaves.
struct Spec {
    kind: &'static str,
    path: &'static str,
    credentials: Option<Value>,
    tv: bool,
    movies: bool,
    searchable: bool,
}
fn specs() -> Vec<Spec> {
    let feed_params = |kind: &str| json!([{"name":"u","value":"7"},{"name":"tp","value":PASSKEY},{"name":"48","value":""},{"name":"kind","value":kind}]);
    vec![
        Spec {
            kind: "nyaa",
            path: "/nyaa/",
            credentials: None,
            tv: true,
            movies: true,
            searchable: true,
        },
        Spec {
            kind: "fanzub",
            path: "/fanzub/rss",
            credentials: None,
            tv: true,
            movies: false,
            searchable: true,
        },
        Spec {
            kind: "iptorrents",
            path: "/ipt/t.rss",
            credentials: Some(
                json!({"kind":"feed","cookie":COOKIE,"tv_parameters":feed_params("tv"),"movie_parameters":feed_params("movies")}),
            ),
            tv: true,
            movies: true,
            searchable: false,
        },
        Spec {
            kind: "torrentleech",
            path: "/tl",
            credentials: Some(json!({"kind":"api_key","api_key":RSSKEY})),
            tv: true,
            movies: false,
            searchable: false,
        },
        Spec {
            kind: "broadcasthenet",
            path: "/btn/",
            credentials: Some(json!({"kind":"api_key","api_key":APIKEY})),
            tv: true,
            movies: false,
            searchable: true,
        },
        Spec {
            kind: "filelist",
            path: "/filelist/api.php",
            credentials: Some(
                json!({"kind":"username_password","username":"flist","password":PASSKEY}),
            ),
            tv: true,
            movies: true,
            searchable: true,
        },
        Spec {
            kind: "hdbits",
            path: "/hdbits/api/torrents",
            credentials: Some(
                json!({"kind":"username_password","username":"hdb","password":PASSKEY}),
            ),
            tv: true,
            movies: true,
            searchable: true,
        },
        Spec {
            kind: "passthepopcorn",
            path: "/ptp/torrents.php",
            credentials: Some(
                json!({"kind":"username_password","username":"ptpuser","password":APIKEY}),
            ),
            tv: false,
            movies: true,
            searchable: true,
        },
        Spec {
            kind: "torrentpotato",
            path: "/potato/",
            credentials: Some(
                json!({"kind":"username_password","username":"potuser","password":PASSKEY}),
            ),
            tv: false,
            movies: true,
            searchable: true,
        },
    ]
}
fn scopes(spec: &Spec) -> (Value, Value) {
    let categories = |tv: bool| match (spec.kind, tv) {
        ("filelist", true) => json!([21, 23, 27]),
        ("filelist", false) => json!([4, 19]),
        ("hdbits", true) => json!([2]),
        ("hdbits", false) => json!([1]),
        _ => json!([]),
    };
    let scope = |tv: bool| {
        let mut value = json!({"enable_rss":true,"categories":categories(tv)});
        if spec.searchable {
            value["enable_automatic_search"] = json!(true);
            value["enable_interactive_search"] = json!(true);
        }
        value
    };
    (
        if spec.tv { scope(true) } else { Value::Null },
        if spec.movies {
            scope(false)
        } else {
            Value::Null
        },
    )
}
fn body(h: &Harness, spec: &Spec) -> Value {
    let (tv, movies) = scopes(spec);
    let mut value = json!({"name":spec.kind,"enabled":true,"priority":1,
        "settings":{"implementation":spec.kind,"endpoint":format!("{}{}",h.remote,spec.path),"tv":tv,"movies":movies}});
    if let Some(credentials) = &spec.credentials {
        value["credentials"] = credentials.clone();
    }
    value
}
async fn create(h: &Harness, spec: &Spec) -> Value {
    let (code, value) = request(&h.base, "POST", "/api/v1/providers", body(h, spec)).await;
    assert_eq!(code, 201, "{}: {value}", spec.kind);
    value
}
async fn create_client(h: &Harness) -> Value {
    let (code, value) = request(
        &h.base,
        "POST",
        "/api/v1/providers",
        json!({"name":"client","enabled":true,"priority":1,"settings":{"implementation":"qbittorrent","endpoint":format!("{}/qb",h.remote),
            "tv":{"category":"tv","imported_category":null,"recent_priority":0,"older_priority":0},
            "movies":{"category":"movies","imported_category":null,"recent_priority":0,"older_priority":0}}}),
    )
    .await;
    assert_eq!(code, 201, "{value}");
    value
}
fn target(indexer: &Value, client: &Value, media: &str) -> Value {
    json!({"media_type":media,"indexer_id":indexer["id"],"indexer_revision":indexer["revision"],"client_id":client["id"],"client_revision":client["revision"]})
}
async fn until(base: &str, id: &Value) -> Value {
    tokio::time::timeout(Duration::from_secs(20), async {
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

// ------------------------------------------------------------------------------------------
// Tests
// ------------------------------------------------------------------------------------------

#[tokio::test]
async fn registry_templates_and_configuration_contract() {
    let h = harness().await;
    let (code, schema) = request(
        &h.base,
        "GET",
        "/api/v1/providers/schema?kind=indexer&media_type=tv",
        Value::Null,
    )
    .await;
    assert_eq!(code, 200, "{schema}");
    let templates = schema["templates"].as_array().unwrap();
    let find = |name: &str| templates.iter().find(|t| t["implementation"] == name);
    // TV-served trackers: nyaa, fanzub, iptorrents, torrentleech, broadcasthenet, filelist, hdbits
    for spec in specs() {
        let template = find(spec.kind).unwrap_or_else(|| panic!("no template for {}", spec.kind));
        assert_eq!(template["defaults"]["kind"], "tracker");
        assert!(
            template["defaults"]["endpoint_hint"]
                .as_str()
                .unwrap()
                .starts_with("http")
        );
        let media: Vec<_> = template["supported_media"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m.as_str().unwrap())
            .collect();
        assert_eq!(media.contains(&"tv"), spec.tv, "{}", spec.kind);
        assert_eq!(media.contains(&"movies"), spec.movies, "{}", spec.kind);
        let flag = &template["defaults"]["tv"]["enable_interactive_search"];
        assert_eq!(flag, &json!(spec.searchable), "{}", spec.kind);
    }
    let conn = h.db.connect().await.unwrap();
    for spec in specs() {
        let created = create(&h, &spec).await;
        assert_eq!(created["settings"]["implementation"], spec.kind);
        assert_eq!(
            created["has_credentials"],
            spec.credentials.is_some(),
            "{}",
            spec.kind
        );
        for (domain, served) in [("tv", spec.tv), ("movies", spec.movies)] {
            let scope = &created["settings"][domain];
            assert_eq!(scope.is_object(), served, "{} {domain}", spec.kind);
            if served {
                // Responses always carry the resolved search flags.
                assert_eq!(scope["enable_rss"], true);
                assert_eq!(
                    scope["enable_automatic_search"],
                    json!(spec.searchable),
                    "{} {domain}",
                    spec.kind
                );
                assert_eq!(
                    scope["enable_interactive_search"],
                    json!(spec.searchable),
                    "{} {domain}",
                    spec.kind
                );
                assert_eq!(
                    scope["categories"],
                    scopes(&spec)
                        .0
                        .get("categories")
                        .map_or(json!([]), |_| scope["categories"].clone())
                );
            }
        }
        // Stored shape: no native columns, search flags per capability, options in the side table.
        let id = created["id"].as_str().unwrap();
        let mut rows = conn
            .query(
                "SELECT s.media_type,s.enable_rss,s.enable_automatic_search,s.enable_interactive_search,s.categories,s.download_client_id,o.categories,o.minimum_seeders FROM provider_scopes s JOIN tracker_scope_options o ON o.provider_id=s.provider_id AND o.media_type=s.media_type WHERE s.provider_id=? ORDER BY s.media_type",
                [id],
            )
            .await
            .unwrap();
        let mut stored = 0;
        while let Some(row) = rows.next().await.unwrap() {
            stored += 1;
            assert_eq!(row.get::<i64>(1).unwrap(), 1);
            assert_eq!(row.get::<i64>(2).unwrap(), i64::from(spec.searchable));
            assert_eq!(row.get::<i64>(3).unwrap(), i64::from(spec.searchable));
            assert!(row.get::<Option<String>>(4).unwrap().is_none());
            assert!(row.get::<Option<String>>(5).unwrap().is_none());
            assert!(row.get::<Option<i64>>(7).unwrap().is_none());
        }
        assert_eq!(
            stored,
            usize::from(spec.tv) + usize::from(spec.movies),
            "{}",
            spec.kind
        );
        // Credentials are sealed: no marker survives in the stored blob or any plaintext column.
        let mut rows = conn
            .query(
                "SELECT credentials,endpoint,name FROM providers WHERE id=?",
                [id],
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        let blob: Option<Vec<u8>> = row.get(0).unwrap();
        let text = format!(
            "{:?}{}{}",
            blob,
            row.get::<String>(1).unwrap(),
            row.get::<String>(2).unwrap()
        );
        assert!(!SECRETS.iter().any(|s| text.contains(s)), "{}", spec.kind);
        assert_eq!(blob.is_some(), spec.credentials.is_some());
    }
    // A search flag defaults on for a searchable tracker and off for an RSS-only one.
    let h2 = &h;
    let (code, defaults) = request(
        &h2.base,
        "POST",
        "/api/v1/providers",
        json!({"name":"defaults","enabled":true,"priority":1,"settings":{"implementation":"nyaa","endpoint":format!("{}/nyaa/",h2.remote),"tv":{},"movies":null}}),
    )
    .await;
    assert_eq!(code, 201, "{defaults}");
    assert_eq!(
        defaults["settings"]["tv"],
        json!({"enable_rss":true,"enable_automatic_search":true,"enable_interactive_search":true,"minimum_seeders":null,"categories":[]})
    );
    let (code, defaults) = request(
        &h2.base,
        "POST",
        "/api/v1/providers",
        json!({"name":"defaults-ipt","enabled":true,"priority":1,"settings":{"implementation":"iptorrents","endpoint":format!("{}/ipt/t.rss",h2.remote),"tv":{},"movies":null}}),
    )
    .await;
    assert_eq!(code, 201, "{defaults}");
    assert_eq!(
        defaults["settings"]["tv"]["enable_interactive_search"],
        false
    );

    // Rejections: wrong domain, unsupported options, wrong credential kinds, bad bounds.
    let nyaa_url = format!("{}/nyaa/", h.remote);
    let try_create = |settings: Value, credentials: Option<Value>| {
        let base = h.base.clone();
        async move {
            let mut input = json!({"name":"x","enabled":true,"priority":1,"settings":settings});
            if let Some(credentials) = credentials {
                input["credentials"] = credentials;
            }
            request(&base, "POST", "/api/v1/providers", input).await.0
        }
    };
    let settings = |kind: &str, tv: Value, movies: Value| json!({"implementation":kind,"endpoint":nyaa_url,"tv":tv,"movies":movies});
    for (kind, tv, movies) in [
        ("broadcasthenet", Value::Null, json!({})), // TV only
        ("fanzub", Value::Null, json!({})),         // TV only
        ("torrentleech", Value::Null, json!({})),   // TV only
        ("passthepopcorn", json!({}), Value::Null), // movies only
        ("torrentpotato", json!({}), Value::Null),  // movies only
        ("nyaa", Value::Null, Value::Null),         // no domain at all
        ("nyaa", json!({"categories":[1]}), Value::Null), // categories only where the tracker has them
        ("hdbits", json!({"categories":[0]}), Value::Null),
        ("hdbits", json!({"categories":[2,2]}), Value::Null),
        ("filelist", json!({"minimum_seeders":1000001}), Value::Null),
        (
            "iptorrents",
            json!({"enable_interactive_search":true}),
            Value::Null,
        ), // RSS-only
        (
            "torrentleech",
            json!({"enable_automatic_search":true}),
            Value::Null,
        ),
        (
            "nyaa",
            json!({"download_client_id":"11111111-1111-4111-8111-111111111111"}),
            Value::Null,
        ),
        ("nyaa", json!({"unknown":1}), Value::Null),
    ] {
        let code = try_create(settings(kind, tv.clone(), movies.clone()), None).await;
        assert!(code == 400 || code == 422, "{kind} {tv} {movies}: {code}");
    }
    for (kind, credentials) in [
        ("nyaa", json!({"kind":"api_key","api_key":"k"})),
        ("filelist", json!({"kind":"api_key","api_key":"k"})),
        ("hdbits", json!({"kind":"feed","cookie":"a=b"})),
        (
            "broadcasthenet",
            json!({"kind":"username_password","username":"u","password":"p"}),
        ),
        ("torrentpotato", json!({"kind":"indexer","api_key":"k"})),
        (
            "iptorrents",
            json!({"kind":"feed","cookie":"bad\u{7f}cookie"}),
        ),
        (
            "iptorrents",
            json!({"kind":"feed","tv_parameters":[{"name":"","value":"x"}]}),
        ),
    ] {
        let code = try_create(
            settings(kind, json!({}), Value::Null),
            Some(credentials.clone()),
        )
        .await;
        assert!(code == 400 || code == 422, "{kind} {credentials}: {code}");
    }
    // Numeric parameter names are the IPTorrents category selectors; other trackers refuse them.
    let ipt = json!({"kind":"feed","tv_parameters":[{"name":"48","value":""}]});
    assert_eq!(
        try_create(
            settings("iptorrents", json!({}), Value::Null),
            Some(ipt.clone())
        )
        .await,
        201
    );
    assert_eq!(
        try_create(settings("nyaa", json!({}), Value::Null), Some(ipt)).await,
        400
    );
    // Read-back after reopen of the same scopes is exact (revision-bound update keeps credentials).
    let spec = &specs()[5];
    let created = create(&h, spec).await;
    let id = created["id"].as_str().unwrap();
    let mut update = body(&h, spec);
    update.as_object_mut().unwrap().remove("credentials");
    update["settings"]["tv"]["minimum_seeders"] = json!(7);
    update["settings"]["tv"]["categories"] = json!([21]);
    update["settings"]["movies"] = Value::Null;
    update["revision"] = created["revision"].clone();
    let (code, updated) = request(&h.base, "PUT", &format!("/api/v1/providers/{id}"), update).await;
    assert_eq!(code, 200, "{updated}");
    assert_eq!(updated["has_credentials"], true);
    assert_eq!(updated["settings"]["tv"]["minimum_seeders"], 7);
    assert_eq!(updated["settings"]["tv"]["categories"], json!([21]));
    assert!(updated["settings"]["movies"].is_null());
    let (_, read) = request(
        &h.base,
        "GET",
        &format!("/api/v1/providers/{id}"),
        Value::Null,
    )
    .await;
    assert_eq!(read["settings"], updated["settings"]);
}

#[tokio::test]
async fn every_tracker_tests_and_searches_with_the_documented_request_shape() {
    let h = harness().await;
    for spec in specs() {
        let created = create(&h, &spec).await;
        let id = created["id"].as_str().unwrap().to_string();
        // --- test route -----------------------------------------------------------------
        let (code, tested) = request(
            &h.base,
            "POST",
            &format!("/api/v1/providers/{id}/test"),
            Value::Null,
        )
        .await;
        assert_eq!(code, 200, "{}: {tested}", spec.kind);
        let domains = tested["result"]["domains"].as_array().unwrap();
        assert_eq!(
            domains.len(),
            usize::from(spec.tv) + usize::from(spec.movies),
            "{}",
            spec.kind
        );
        assert!(
            domains
                .iter()
                .all(|d| d["parsed"].as_u64().unwrap() >= 1 && d["rejected"] == 0),
            "{}: {tested}",
            spec.kind
        );
        let (_, read) = request(
            &h.base,
            "GET",
            &format!("/api/v1/providers/{id}"),
            Value::Null,
        )
        .await;
        assert_eq!(read["test_status"], "success", "{}", spec.kind);
        // --- RSS through the search route ----------------------------------------------------
        for (domain, served, title) in [
            ("tv", spec.tv, "Harbor.S01E01"),
            ("movies", spec.movies, "Harbor.2020"),
        ] {
            let (code, page) = request(
                &h.base,
                "POST",
                &format!("/api/v1/providers/{id}/search"),
                json!({"kind":"rss","media_type":domain,"limit":10}),
            )
            .await;
            if !served {
                assert_eq!(code, 409, "{} {domain}: {page}", spec.kind);
                continue;
            }
            assert_eq!(code, 200, "{} {domain}: {page}", spec.kind);
            let items = page["page"]["items"].as_array().unwrap();
            assert!(!items.is_empty(), "{} {domain}", spec.kind);
            assert!(
                items
                    .iter()
                    .any(|i| i["title"].as_str().unwrap().contains(title)),
                "{} {domain}: {page}",
                spec.kind
            );
            for item in items {
                assert!(
                    item["published_at"].as_str().unwrap().starts_with("2024-"),
                    "{}",
                    spec.kind
                );
                assert!(item["size_bytes"].as_u64().unwrap_or(1) > 0);
                assert!(item["categories"].as_array().unwrap().iter().all(|c| {
                    let c = c.as_u64().unwrap();
                    (domain == "tv" && (5000..6000).contains(&c))
                        || (domain == "movies" && (2000..3000).contains(&c))
                }));
            }
        }
        // --- tv / movie searches --------------------------------------------------------------
        let before = h.state.seen.lock().unwrap().len();
        let tv_search = json!({"kind":"tv","title":"Harbor","tvdb_id":101,"numbering":{"kind":"episode","season":1,"episode":1},"limit":10});
        let movie_search =
            json!({"kind":"movie","title":"Harbor","year":2020,"imdb_id":"tt0123456","limit":10});
        for (domain, served, search) in [
            ("tv", spec.tv, tv_search),
            ("movies", spec.movies, movie_search),
        ] {
            let (code, page) = request(
                &h.base,
                "POST",
                &format!("/api/v1/providers/{id}/search"),
                search,
            )
            .await;
            if served && spec.searchable {
                assert_eq!(code, 200, "{} {domain}: {page}", spec.kind);
                assert!(
                    !page["page"]["items"].as_array().unwrap().is_empty(),
                    "{} {domain}",
                    spec.kind
                );
            } else {
                // Not served or RSS-only: refused before any network call.
                assert_eq!(code, 409, "{} {domain}: {page}", spec.kind);
            }
        }
        let after = h.state.seen.lock().unwrap().len();
        let expected =
            usize::from(spec.tv && spec.searchable) + usize::from(spec.movies && spec.searchable);
        assert_eq!(
            after - before,
            expected,
            "{}: refused searches must not reach the tracker",
            spec.kind
        );
    }

    let s = &h.state;
    // Nyaa: documented RSS parameters, no credentials, and a text term for searches.
    let nyaa = s.hits("/nyaa/");
    assert!(
        nyaa.iter()
            .all(|r| r.query.get("page").map(String::as_str) == Some("rss")
                && r.query.get("f").map(String::as_str) == Some("1"))
    );
    assert!(
        nyaa.iter()
            .any(|r| r.query.get("q").map(String::as_str) == Some("Harbor S01E01")),
        "{nyaa:?}"
    );
    assert!(
        nyaa.iter()
            .any(|r| r.query.get("q").map(String::as_str) == Some("Harbor 2020"))
    );
    assert!(
        nyaa.iter().all(|r| r.headers.is_empty()),
        "no ambient credentials"
    );
    // Fanzub: anime category, NZB enclosure.
    let fanzub = s.hits("/fanzub/");
    assert!(
        fanzub
            .iter()
            .all(|r| r.query.get("cat").map(String::as_str) == Some("anime"))
    );
    assert!(
        fanzub
            .iter()
            .any(|r| r.query.get("q").map(String::as_str) == Some("Harbor S01E01"))
    );
    // IPTorrents: sealed cookie and parameters (including the numeric selector) on every request.
    let ipt = s.hits("/ipt/");
    assert!(
        !ipt.is_empty()
            && ipt.iter().all(
                |r| r.headers.get("cookie").map(String::as_str) == Some(COOKIE)
                    && r.query.contains_key("48")
                    && r.query["tp"] == PASSKEY
            )
    );
    assert!(
        ipt.iter().any(|r| r.query["kind"] == "movies")
            && ipt.iter().any(|r| r.query["kind"] == "tv")
    );
    // Torrentleech: the RSS key is a path segment, never a query value.
    let tl = s.hits("/tl/");
    assert!(
        !tl.is_empty()
            && tl
                .iter()
                .all(|r| r.path == format!("/tl/{RSSKEY}") && r.query.is_empty())
    );
    // BroadcastheNet: JSON-RPC getTorrents with the key first and criteria from the numbering.
    let btn = s.hits("/btn/");
    let rpc: Vec<Value> = btn
        .iter()
        .map(|r| serde_json::from_str(&r.body).unwrap())
        .collect();
    assert!(rpc.iter().all(|b| b["jsonrpc"] == "2.0"
        && b["method"] == "getTorrents"
        && b["params"][0] == APIKEY));
    assert!(rpc.iter().any(|b| b["params"][1] == json!({})));
    assert!(
        rpc.iter()
            .any(|b| b["params"][1] == json!({"tvdb":101,"name":"S01E01","category":"Episode"})),
        "{rpc:?}"
    );
    // FileList: HTTP basic authentication, latest vs search action, imdb / name search types.
    let fl = s.hits("/filelist/");
    assert!(
        fl.iter()
            .all(|r| r.headers.get("authorization").map(String::as_str)
                == Some(basic("flist", PASSKEY).as_str())
                && !r.query.values().any(|v| v == PASSKEY))
    );
    assert!(fl.iter().any(|r| r.query.get("action").map(String::as_str)
        == Some("latest-torrents")
        && r.query.get("category").map(String::as_str) == Some("21,23,27")));
    assert!(
        fl.iter()
            .any(|r| r.query.get("type").map(String::as_str) == Some("imdb")
                && r.query["query"] == "tt0123456")
    );
    assert!(
        fl.iter()
            .any(|r| r.query.get("season").map(String::as_str) == Some("1")
                && r.query.get("episode").map(String::as_str) == Some("1"))
    );
    // HDBits: credentials and filters travel in the JSON body only.
    let hd = s.hits("/hdbits/");
    assert!(hd.iter().all(|r| r.query.is_empty()));
    let bodies: Vec<Value> = hd
        .iter()
        .map(|r| serde_json::from_str(&r.body).unwrap())
        .collect();
    assert!(
        bodies
            .iter()
            .all(|b| b["username"] == "hdb" && b["passkey"] == PASSKEY)
    );
    assert!(
        bodies
            .iter()
            .any(|b| b["tvdb"] == json!({"id":101,"season":1,"episode":1})
                && b["category"] == json!([2]))
    );
    assert!(
        bodies
            .iter()
            .any(|b| b["imdb"] == json!({"id":123456}) && b["category"] == json!([1]))
    );
    // PassThePopcorn: API user/key headers, JSON listing, imdb search.
    let ptp = s.hits("/ptp/");
    assert!(ptp.iter().all(
        |r| r.headers.get("apiuser").map(String::as_str) == Some("ptpuser")
            && r.headers.get("apikey").map(String::as_str) == Some(APIKEY)
            && r.query["json"] == "noredirect"
    ));
    assert!(
        ptp.iter()
            .any(|r| r.query.get("searchstr").map(String::as_str) == Some("tt0123456"))
    );
    // TorrentPotato: user and passkey as query parameters.
    let potato = s.hits("/potato/");
    assert!(
        potato
            .iter()
            .all(|r| r.query["user"] == "potuser" && r.query["passkey"] == PASSKEY)
    );
    assert!(
        potato
            .iter()
            .any(|r| r.query.get("search").map(String::as_str) == Some("tt0123456"))
    );
}

#[tokio::test]
async fn search_paging_and_minimum_seeders_apply_to_the_fetched_document() {
    let h = harness().await;
    let spec = &specs()[0];
    let mut input = body(&h, spec);
    input["settings"]["tv"]["minimum_seeders"] = json!(200);
    input["settings"]["movies"]["minimum_seeders"] = json!(100);
    let (code, nyaa) = request(&h.base, "POST", "/api/v1/providers", input).await;
    assert_eq!(code, 201, "{nyaa}");
    let id = nyaa["id"].as_str().unwrap();
    let search = format!("/api/v1/providers/{id}/search");
    // Both fixture items report 120 seeders: below the TV minimum, above the movie minimum.
    let (_, tv) = request(
        &h.base,
        "POST",
        &search,
        json!({"kind":"rss","media_type":"tv","limit":10}),
    )
    .await;
    assert_eq!(tv["page"]["items"].as_array().unwrap().len(), 0);
    let (_, movies) = request(
        &h.base,
        "POST",
        &search,
        json!({"kind":"rss","media_type":"movies","limit":1}),
    )
    .await;
    assert_eq!(movies["page"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(movies["page"]["total"], 2);
    assert_eq!(movies["page"]["next_offset"], 1);
    let (_, second) = request(
        &h.base,
        "POST",
        &search,
        json!({"kind":"rss","media_type":"movies","limit":1,"offset":1}),
    )
    .await;
    assert_eq!(second["page"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(second["page"]["next_offset"], Value::Null);
    let (_, tested) = request(
        &h.base,
        "POST",
        &format!("/api/v1/providers/{id}/test"),
        Value::Null,
    )
    .await;
    assert_eq!(
        tested["result"]["domains"][0],
        json!({"media_type":"tv","parsed":2,"rejected":0,"below_minimum_seeders":2})
    );
    // Invalid paging is refused without a network call.
    let hits = h.state.hits("/nyaa/").len();
    let (code, _) = request(
        &h.base,
        "POST",
        &search,
        json!({"kind":"rss","media_type":"tv","limit":0}),
    )
    .await;
    assert!(code == 400 || code == 422);
    let (code, _) = request(
        &h.base,
        "POST",
        &search,
        json!({"kind":"rss","media_type":"tv","limit":10,"query_index":1}),
    )
    .await;
    assert!(code == 400 || code == 422);
    assert_eq!(h.state.hits("/nyaa/").len(), hits);
}

#[tokio::test]
async fn failures_are_typed_secret_free_and_feed_the_durable_backoff() {
    let h = harness().await;
    for spec in specs() {
        let created = create(&h, &spec).await;
        let id = created["id"].as_str().unwrap().to_string();
        let path = format!("/api/v1/providers/{id}/test");
        for (mode, status, expected) in [
            (SERVER_ERROR, 502, "transport_error"),
            (REDIRECT, 502, "redirect_rejected"),
            (GARBAGE, 502, "invalid_response"),
            (OVERSIZED, 502, "response_too_large"),
        ] {
            h.state.mode(mode);
            // A failed observation opens the durable cool-down for native-style backoff; clear it so the
            // next typed failure is actually requested.
            h.db.connect()
                .await
                .unwrap()
                .execute("DELETE FROM indexer_status", ())
                .await
                .unwrap();
            let (code, error) = request(&h.base, "POST", &path, Value::Null).await;
            assert_eq!(
                (code, error["error"]["code"].as_str()),
                (status, Some(expected)),
                "{} mode {mode}: {error}",
                spec.kind
            );
            let (_, read) = request(
                &h.base,
                "GET",
                &format!("/api/v1/providers/{id}"),
                Value::Null,
            )
            .await;
            assert_eq!(read["test_status"], "failure");
            assert_eq!(read["last_test"]["error_code"], expected, "{}", spec.kind);
        }
        // Wrong credentials: the mock answers 403 / a protocol error; both are `authentication`.
        h.state.mode(AUTH);
        h.db.connect()
            .await
            .unwrap()
            .execute("DELETE FROM indexer_status", ())
            .await
            .unwrap();
        let (code, error) = request(&h.base, "POST", &path, Value::Null).await;
        assert_eq!(
            (code, error["error"]["code"].as_str()),
            (502, Some("authentication")),
            "{} {error}",
            spec.kind
        );
        // The failure is durable: an indexer_status row exists for the tested revision and domain.
        let conn = h.db.connect().await.unwrap();
        let count: i64 = conn
            .query(
                "SELECT count(*) FROM indexer_status WHERE provider_id=?",
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
        assert!(count >= 1, "{}: failures reach indexer_status", spec.kind);
        // While cooling down the tracker is not requested at all (the saved-provider search admits through
        // the durable backoff; the explicit test probe deliberately bypasses it).
        h.state.mode(NORMAL);
        let before = h.state.seen.lock().unwrap().len();
        let domain = if spec.tv { "tv" } else { "movies" };
        let (code, cooling) = request(
            &h.base,
            "POST",
            &format!("/api/v1/providers/{id}/search"),
            json!({"kind":"rss","media_type":domain,"limit":5}),
        )
        .await;
        assert_eq!(code, 429, "{} {cooling}", spec.kind);
        assert_eq!(
            h.state.seen.lock().unwrap().len(),
            before,
            "{}: backoff admission sends nothing",
            spec.kind
        );
        // Success clears the status once the cool-down is released.
        conn.execute("DELETE FROM indexer_status", ())
            .await
            .unwrap();
        let (code, ok) = request(&h.base, "POST", &path, Value::Null).await;
        assert_eq!(code, 200, "{} {ok}", spec.kind);
        let count: i64 = conn
            .query(
                "SELECT count(*) FROM indexer_status WHERE provider_id=?",
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
        assert_eq!(count, 0, "{}", spec.kind);
        // Rate limiting: a 429 with Retry-After is a typed failure that opens the durable cool-down
        // for at least that long; the saved-provider search is then refused without a request.
        h.state.mode(RATE_LIMITED);
        conn.execute("DELETE FROM indexer_status", ())
            .await
            .unwrap();
        let (code, limited) = request(&h.base, "POST", &path, Value::Null).await;
        assert_eq!(code, 429, "{} {limited}", spec.kind);
        let remaining: i64 = conn
            .query(
                "SELECT max(disabled_until-last_failure_at) FROM indexer_status WHERE provider_id=?",
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
        assert!(
            remaining >= 120,
            "{}: Retry-After is honoured ({remaining}s)",
            spec.kind
        );
        h.state.mode(NORMAL);
        let hits = h.state.seen.lock().unwrap().len();
        let (code, again) = request(
            &h.base,
            "POST",
            &format!("/api/v1/providers/{id}/search"),
            json!({"kind":"rss","media_type":domain,"limit":5}),
        )
        .await;
        assert_eq!(code, 429, "{} {again}", spec.kind);
        assert_eq!(
            h.state.seen.lock().unwrap().len(),
            hits,
            "{}: no request during the cool-down",
            spec.kind
        );
        h.state.mode(NORMAL);
    }
}

#[tokio::test]
async fn missing_credentials_fail_before_any_request_and_empty_documents_are_valid() {
    let h = harness().await;
    for spec in specs().into_iter().filter(|s| {
        matches!(
            s.kind,
            "broadcasthenet"
                | "filelist"
                | "hdbits"
                | "passthepopcorn"
                | "torrentpotato"
                | "torrentleech"
        )
    }) {
        let mut input = body(&h, &spec);
        input.as_object_mut().unwrap().remove("credentials");
        let (code, created) = request(&h.base, "POST", "/api/v1/providers", input).await;
        assert_eq!(code, 201, "{created}");
        let before = h.state.seen.lock().unwrap().len();
        let (code, error) = request(
            &h.base,
            "POST",
            &format!("/api/v1/providers/{}/test", created["id"].as_str().unwrap()),
            Value::Null,
        )
        .await;
        assert_eq!(
            (code, error["error"]["code"].as_str()),
            (502, Some("authentication")),
            "{} {error}",
            spec.kind
        );
        assert_eq!(
            h.state.seen.lock().unwrap().len(),
            before,
            "{}: nothing sent without an account",
            spec.kind
        );
    }
    // An empty but well-formed document is valid evidence of access.
    h.state.mode(EMPTY);
    for spec in specs() {
        let created = create(&h, &spec).await;
        let (code, tested) = request(
            &h.base,
            "POST",
            &format!("/api/v1/providers/{}/test", created["id"].as_str().unwrap()),
            Value::Null,
        )
        .await;
        assert_eq!(code, 200, "{} {tested}", spec.kind);
        assert!(
            tested["result"]["domains"]
                .as_array()
                .unwrap()
                .iter()
                .all(|d| d["parsed"] == 0),
            "{}",
            spec.kind
        );
    }
}

#[tokio::test]
async fn draft_tests_cover_trackers_without_persisting() {
    let h = harness().await;
    for spec in specs() {
        let (code, result) = request(
            &h.base,
            "POST",
            "/api/v1/providers/test-draft",
            json!({"source":null,"config":body(&h, &spec)}),
        )
        .await;
        assert_eq!(code, 200, "{} {result}", spec.kind);
        assert_eq!(
            result["result"]["domains"].as_array().unwrap().len(),
            usize::from(spec.tv) + usize::from(spec.movies)
        );
    }
    let conn = h.db.connect().await.unwrap();
    for table in [
        "providers",
        "provider_scopes",
        "tracker_scope_options",
        "provider_tests",
        "indexer_status",
    ] {
        let count: i64 = conn
            .query(&format!("SELECT count(*) FROM {table}"), ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        assert_eq!(count, 0, "{table}");
    }
    // A draft with the wrong credentials fails typed and secret-free.
    let spec = &specs()[4];
    let mut wrong = body(&h, spec);
    wrong["credentials"] = json!({"kind":"api_key","api_key":"PRIVATE_APIKEY_WRONG"});
    let (code, error) = request(
        &h.base,
        "POST",
        "/api/v1/providers/test-draft",
        json!({"source":null,"config":wrong}),
    )
    .await;
    assert_eq!(
        (code, error["error"]["code"].as_str()),
        (502, Some("authentication")),
        "{error}"
    );
}

#[tokio::test]
async fn rss_sync_grabs_tracker_releases_for_both_domains_and_replay_is_idempotent() {
    let h = harness().await;
    seed(&h.db).await;
    let client = create_client(&h).await;
    let specs = specs();
    let nyaa = create(&h, &specs[0]).await; // TV (and movies) feed through Nyaa RSS
    let hdbits = create(&h, &specs[6]).await; // JSON API, both domains
    let runtime = commands::start(h.db.clone(), h.client.clone())
        .await
        .unwrap();
    for (indexer, media, expected_category) in [(&nyaa, "tv", "tv"), (&hdbits, "movies", "movies")]
    {
        let (code, command) = request(
            &h.base,
            "POST",
            "/api/v1/rss/commands",
            json!({"target":target(indexer, &client, media),"priority":"normal"}),
        )
        .await;
        assert_eq!(code, 202, "{command}");
        let done = until(&h.base, &command["id"]).await;
        assert_eq!(done["status"], "succeeded", "{done}");
        let (_, receipts) = request(
            &h.base,
            "GET",
            &format!(
                "/api/v1/rss/candidates?command_id={}",
                command["id"].as_str().unwrap()
            ),
            Value::Null,
        )
        .await;
        let grabbed: Vec<_> = receipts["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["target"]["media_type"] == media)
            .collect();
        assert_eq!(grabbed.len(), 1, "{media}: {receipts}");
        assert_eq!(
            grabbed[0]["target"][if media == "tv" {
                "series_id"
            } else {
                "movie_id"
            }],
            1,
            "equal numeric IDs keep their domain"
        );
        // Replay performs no second external grab.
        let (_, replay) = request(
            &h.base,
            "POST",
            "/api/v1/rss/commands",
            json!({"target":target(indexer, &client, media),"priority":"normal"}),
        )
        .await;
        assert_eq!(until(&h.base, &replay["id"]).await["status"], "succeeded");
        assert_eq!(
            h.state
                .adds
                .lock()
                .unwrap()
                .iter()
                .filter(|a| a.starts_with(expected_category))
                .count(),
            1,
            "{media}"
        );
    }
    assert_eq!(h.state.adds.lock().unwrap().len(), 2);
    // The HDBits release locator (with the passkey) was only ever used server-side by the client's
    // transport; the JSON request carried the account in the body.
    assert!(h.state.hits("/hdbits/").iter().all(|r| r.query.is_empty()));
    // A schedule is accepted for a searchable tracker too.
    let (code, schedule) = request(
        &h.base,
        "POST",
        "/api/v1/rss/schedules",
        json!({"target":target(&nyaa, &client, "tv"),"interval_seconds":600,"enabled":true}),
    )
    .await;
    assert_eq!(code, 200, "{schedule}");
    runtime.shutdown().await;
}

#[tokio::test]
async fn fanzub_is_a_usenet_indexer_and_pairs_only_with_usenet_clients() {
    let h = harness().await;
    let fanzub = create(&h, &specs()[1]).await;
    let torrent_client = create_client(&h).await;
    let (code, usenet) = request(
        &h.base,
        "POST",
        "/api/v1/providers",
        json!({"name":"sab","enabled":true,"priority":1,"credentials":{"kind":"api_key","api_key":"k"},
            "settings":{"implementation":"sabnzbd","endpoint":format!("{}/sab",h.remote),
                "tv":{"category":"tv"},"movies":null}}),
    )
    .await;
    assert_eq!(code, 201, "{usenet}");
    let enqueue = |client: Value| {
        let base = h.base.clone();
        let fanzub = fanzub.clone();
        async move {
            request(
                &base,
                "POST",
                "/api/v1/rss/commands",
                json!({"target":target(&fanzub, &client, "tv"),"priority":"normal"}),
            )
            .await
            .0
        }
    };
    assert_eq!(
        enqueue(torrent_client).await,
        409,
        "an NZB indexer cannot feed a torrent client"
    );
    assert_eq!(enqueue(usenet.clone()).await, 202);
    // The converse: a torrent tracker cannot feed the usenet client.
    let nyaa = create(&h, &specs()[0]).await;
    let (code, _) = request(
        &h.base,
        "POST",
        "/api/v1/rss/commands",
        json!({"target":target(&nyaa, &usenet, "tv"),"priority":"normal"}),
    )
    .await;
    assert_eq!(code, 409);
}

#[tokio::test]
async fn administration_treats_trackers_as_indexers() {
    let h = harness().await;
    let mut ids = Vec::new();
    for spec in specs() {
        ids.push(create(&h, &spec).await["id"].clone());
    }
    // Listing by media domain: trackers appear only in the domains they serve.
    let (code, list) = request(
        &h.base,
        "GET",
        "/api/v1/providers?media_type=movies",
        Value::Null,
    )
    .await;
    assert_eq!(code, 200, "{list}");
    let movie_kinds: std::collections::BTreeSet<String> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            p["settings"]["implementation"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    let expected: std::collections::BTreeSet<String> = specs()
        .iter()
        .filter(|s| s.movies)
        .map(|s| s.kind.to_string())
        .collect();
    assert_eq!(movie_kinds, expected);
    // Bulk test-all reaches every tracker of the domain and reports each one.
    let (code, all) = request(
        &h.base,
        "POST",
        "/api/v1/providers/testall",
        json!({"media_type":"tv","kind":"indexer"}),
    )
    .await;
    assert_eq!(code, 200, "{all}");
    let items = all["items"].as_array().unwrap();
    assert_eq!(
        items.len(),
        specs().iter().filter(|s| s.tv).count(),
        "{all}"
    );
    assert!(
        items
            .iter()
            .all(|i| i["status"] == "success" || i["result"].is_object() || i["error"].is_null()),
        "{all}"
    );
    // Tracker rows are never download clients: the client list stays empty.
    let (code, clients) = request(
        &h.base,
        "POST",
        "/api/v1/providers/testall",
        json!({"media_type":"tv","kind":"download_client"}),
    )
    .await;
    assert_eq!(code, 200, "{clients}");
    assert_eq!(clients["items"], json!([]));
}

#[tokio::test]
async fn bounded_json_pages_remain_stable_and_filter_before_local_pagination() {
    let h = harness().await;
    h.state.mode(PAGED);
    for spec in specs()
        .into_iter()
        .filter(|s| matches!(s.kind, "broadcasthenet" | "hdbits"))
    {
        let created = create(&h, &spec).await;
        let id = created["id"].as_str().unwrap();
        let path = format!("/api/v1/providers/{id}/search");
        let (code, full) = request(
            &h.base,
            "POST",
            &path,
            json!({"kind":"rss","media_type":"tv","limit":100}),
        )
        .await;
        assert_eq!(code, 200, "{full}");
        for offset in 0..4 {
            let (code, page) = request(
                &h.base,
                "POST",
                &path,
                json!({"kind":"rss","media_type":"tv","limit":1,"offset":offset}),
            )
            .await;
            assert_eq!(code, 200, "{page}");
            assert_eq!(
                page["page"]["items"][0]["title"], full["page"]["items"][offset]["title"],
                "{} offset {offset}",
                spec.kind
            );
        }
        let mut update = body(&h, &spec);
        update.as_object_mut().unwrap().remove("credentials");
        update["revision"] = created["revision"].clone();
        update["settings"]["tv"]["minimum_seeders"] = json!(10);
        let (code, updated) =
            request(&h.base, "PUT", &format!("/api/v1/providers/{id}"), update).await;
        assert_eq!(code, 200, "{updated}");
        let (code, first) = request(
            &h.base,
            "POST",
            &path,
            json!({"kind":"rss","media_type":"tv","limit":25}),
        )
        .await;
        assert_eq!(code, 200, "{first}");
        assert_eq!(first["page"]["items"].as_array().unwrap().len(), 25);
        assert_eq!(first["page"]["total"], 29);
        assert_eq!(first["page"]["next_offset"], 25);
        let (code, last) = request(
            &h.base,
            "POST",
            &path,
            json!({"kind":"rss","media_type":"tv","limit":25,"offset":25}),
        )
        .await;
        assert_eq!(code, 200, "{last}");
        assert_eq!(last["page"]["items"].as_array().unwrap().len(), 4);
        assert!(last["page"]["next_offset"].is_null());
        for hit in h.state.hits(spec.path) {
            let request: Value = serde_json::from_str(&hit.body).unwrap();
            let count = if spec.kind == "broadcasthenet" {
                &request["params"][2]
            } else {
                &request["limit"]
            };
            assert_eq!(count, 100);
        }
    }
}
