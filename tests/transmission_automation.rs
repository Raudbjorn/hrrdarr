//! HTTP + worker end-to-end flow through an owned Torznab + Transmission RPC mock on
//! 127.0.0.1:0: release → grab → torrent-add → torrent-get refresh → completed handoff → import,
//! for one TV episode (category placement, `.torrent` upload) and one movie (directory placement,
//! magnet) that share native ID 1. No live services, port 9091 or real media are used.
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
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

const USER: &str = "owned-user";
const PASSWORD: &str = "PRIVATE_TRANSMISSION_SECRET";
const SESSION: &str = "owned-session-id";
const FILE_BYTES: usize = 1_048_576;
const MOVIE_HASH: &str = "2222222222222222222222222222222222222222";

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
fn title(tv: bool) -> &'static str {
    if tv {
        "Harbor.S01E01.1080p.WEB-DL"
    } else {
        "Harbor.2020.1080p.WEB-DL"
    }
}
fn b64(bytes: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        out.push(A[(n >> 18) as usize & 63] as char);
        out.push(A[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            A[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            A[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}
/// A valid single-directory v1 `.torrent` for the TV release and its info hash.
fn tv_torrent() -> (Vec<u8>, String) {
    let name = title(true);
    let info = format!(
        "d5:filesld6:lengthi{FILE_BYTES}e4:pathl{}:{name}.mkveee4:name{}:{name}12:piece lengthi262144e6:pieces20:{}e",
        name.len() + 4,
        name.len(),
        "b".repeat(20)
    );
    let hash: String =
        ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, info.as_bytes())
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
    (
        format!("d8:announce17:http://t.invalid/4:info{info}e").into_bytes(),
        hash,
    )
}
#[derive(Default)]
struct Remote {
    origin: Mutex<String>,
    /// Local directories the `/remote` Transmission root maps to, per domain.
    local_roots: Mutex<Vec<(String, PathBuf)>>,
    torrents: Mutex<Vec<Value>>,
    adds: Mutex<Vec<Value>>,
    lose_add_response: AtomicBool,
    next_id: AtomicU64,
    password: Mutex<String>,
}
fn caps() -> &'static str {
    r#"<caps><limits max="100" default="100"/><searching><search available="yes" supportedParams="q"/><tv-search available="yes" supportedParams="q,tvdbid,season,ep"/><movie-search available="yes" supportedParams="q,tmdbid"/></searching><categories><category id="5000"><subcat id="5030"/></category><category id="2000"><subcat id="2030"/></category></categories></caps>"#
}
fn project(torrent: &Value, fields: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for field in fields.as_array().unwrap() {
        if let Some(v) = torrent.get(field.as_str().unwrap()) {
            out.insert(field.as_str().unwrap().into(), v.clone());
        }
    }
    Value::Object(out)
}
fn transmission(s: &Remote, headers: &HeaderMap, body: &[u8]) -> Response {
    let expected = format!(
        "Basic {}",
        b64(format!("{USER}:{}", s.password.lock().unwrap()).as_bytes())
    );
    if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some(expected.as_str()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if headers
        .get("x-transmission-session-id")
        .and_then(|v| v.to_str().ok())
        != Some(SESSION)
    {
        let mut response = StatusCode::CONFLICT.into_response();
        response
            .headers_mut()
            .insert("X-Transmission-Session-Id", SESSION.parse().unwrap());
        return response;
    }
    let request: Value = serde_json::from_slice(body).unwrap();
    let arguments = &request["arguments"];
    let tag = request["tag"].clone();
    let ok = |arguments: Value| {
        axum::Json(json!({"result":"success","arguments":arguments,"tag":tag})).into_response()
    };
    match request["method"].as_str().unwrap() {
        "session-get" => ok(
            json!({"rpc-version":17,"rpc-version-minimum":14,"version":"4.0.6 (owned)","download-dir":"/remote","dht-enabled":true,"download-queue-enabled":true}),
        ),
        "torrent-get" => {
            let mut torrents = s.torrents.lock().unwrap();
            // A general listing (refresh/test, not a submission lookup by hash) finds the transfer
            // finished and seeding.
            if arguments.get("ids").is_none() {
                for t in torrents.iter_mut() {
                    t["status"] = json!(6);
                    t["leftUntilDone"] = json!(0);
                    t["percentDone"] = json!(1.0);
                    t["eta"] = json!(-1);
                    t["files"][0]["bytesCompleted"] = json!(FILE_BYTES);
                    t["fileStats"][0]["bytesCompleted"] = json!(FILE_BYTES);
                }
            }
            let ids = arguments["ids"].as_array().cloned();
            let list: Vec<Value> = torrents
                .iter()
                .filter(|t| {
                    ids.as_ref()
                        .is_none_or(|ids| ids.iter().any(|id| *id == t["hashString"]))
                })
                .map(|t| project(t, &arguments["fields"]))
                .collect();
            ok(json!({ "torrents": list }))
        }
        "torrent-add" => {
            s.adds.lock().unwrap().push(arguments.clone());
            let (bytes, tv_hash) = tv_torrent();
            let (hash, tv) = if let Some(metainfo) = arguments["metainfo"].as_str() {
                assert_eq!(
                    metainfo,
                    b64(&bytes),
                    "the fetched .torrent is uploaded verbatim"
                );
                (tv_hash, true)
            } else {
                let magnet = arguments["filename"].as_str().unwrap();
                assert!(magnet.starts_with(&format!("magnet:?xt=urn:btih:{MOVIE_HASH}")));
                (MOVIE_HASH.to_owned(), false)
            };
            let dir = arguments["download-dir"].as_str().unwrap().to_owned();
            assert_eq!(
                dir,
                if tv {
                    "/remote/tv-hrrdarr"
                } else {
                    "/remote/films"
                }
            );
            assert_eq!(
                arguments.get("labels"),
                tv.then(|| json!(["tv-hrrdarr"])).as_ref(),
                "only the category placement is labelled"
            );
            // The client "downloads" beneath its download directory, which the remote path
            // mapping for that domain exposes locally.
            let name = title(tv);
            let local = s
                .local_roots
                .lock()
                .unwrap()
                .iter()
                .find(|(d, _)| d == if tv { "tv" } else { "movies" })
                .unwrap()
                .1
                .join(dir.trim_start_matches("/remote/"))
                .join(name);
            std::fs::create_dir_all(&local).unwrap();
            std::fs::write(
                local.join(format!("{name}.mkv")),
                vec![if tv { 1 } else { 2 }; FILE_BYTES],
            )
            .unwrap();
            let id = s.next_id.fetch_add(1, Ordering::SeqCst) + 1;
            s.torrents.lock().unwrap().push(json!({"id":id,"hashString":hash,"name":name,"downloadDir":dir,"labels":arguments.get("labels").cloned().unwrap_or(json!([])),
                "status":if arguments["paused"]==true {0} else {4},"totalSize":FILE_BYTES,"sizeWhenDone":FILE_BYTES,"leftUntilDone":FILE_BYTES/2,"percentDone":0.5,"metadataPercentComplete":1.0,
                "isFinished":false,"isStalled":false,"eta":30,"error":0,"errorString":"","rateDownload":1024,"rateUpload":0,"uploadRatio":0.0,"secondsSeeding":0,"queuePosition":0,
                "seedRatioLimit":2.0,"seedRatioMode":0,"seedIdleLimit":30,"seedIdleMode":0,
                "files":[{"name":format!("{name}/{name}.mkv"),"length":FILE_BYTES,"bytesCompleted":FILE_BYTES/2}],
                "fileStats":[{"bytesCompleted":FILE_BYTES/2,"wanted":true,"priority":0}]}));
            if s.lose_add_response.load(Ordering::SeqCst) {
                return StatusCode::BAD_GATEWAY.into_response();
            }
            ok(json!({"torrent-added":{"id":id,"name":name,"hashString":hash}}))
        }
        "torrent-set" | "queue-move-top" => ok(json!({})),
        _ => axum::Json(json!({"result":"method name not recognized","arguments":{},"tag":tag}))
            .into_response(),
    }
}
async fn remote(
    State(s): State<Arc<Remote>>,
    method: Method,
    headers: HeaderMap,
    OriginalUri(uri): OriginalUri,
    body: Bytes,
) -> Response {
    let q: std::collections::HashMap<String, String> =
        url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();
    match uri.path() {
        "/shows/en/101" => axum::Json(json!({"tvdbId":101,"title":"Harbor","seasons":[{"seasonNumber":1}],"episodes":[{"tvdbId":501,"seasonNumber":1,"episodeNumber":1,"title":"Pilot","runtime":45,"airDate":"2020-01-01","airDateUtc":"2020-01-01T00:00:00Z","absoluteEpisodeNumber":1}]})).into_response(),
        "/movie/201" => axum::Json(json!({"tmdbId":201,"title":"Harbor","year":2020,"runtime":100,"digitalRelease":"2020-01-01T00:00:00Z"})).into_response(),
        "/transmission/rpc" => {
            assert_eq!(method, Method::POST);
            transmission(&s, &headers, &body)
        }
        "/torrent/tv.torrent" => {
            assert_eq!(q.get("passkey").map(String::as_str), Some("PRIVATE_RSS_LOCATOR"));
            tv_torrent().0.into_response()
        }
        "/" | "/api" => {
            if q.get("t").is_some_and(|t| t == "caps") {
                return caps().into_response();
            }
            let tv = q.get("cat").is_some_and(|v| v.contains("5030"));
            let origin = s.origin.lock().unwrap().clone();
            let (link, magnet, category) = if tv {
                (format!("{origin}/torrent/tv.torrent?passkey=PRIVATE_RSS_LOCATOR"), String::new(), 5030)
            } else {
                (
                    format!("{origin}/torrent/movie.torrent?passkey=PRIVATE_RSS_LOCATOR"),
                    format!(r#"<torznab:attr name="magneturl" value="magnet:?xt=urn:btih:{MOVIE_HASH}&amp;tr=http%3A%2F%2Ft.invalid%2Fannounce"/>"#),
                    2030,
                )
            };
            let link = link.replace('&', "&amp;");
            format!(r#"<rss xmlns:torznab="http://torznab.com/schemas/2015/feed"><channel><item><title>{}</title><guid>PRIVATE_RSS_GUID-{category}</guid><pubDate>Mon, 01 Jan 2024 12:00:00 +0000</pubDate><link>{link}</link>{magnet}<torznab:attr name="category" value="{category}"/><torznab:attr name="size" value="1073741824"/><torznab:attr name="seeders" value="10"/></item></channel></rss>"#, title(tv)).into_response()
        }
        _ => (StatusCode::NOT_FOUND, "unexpected owned mock request").into_response(),
    }
}
async fn serve(app: axum::Router) -> (String, Server) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    assert_ne!(listener.local_addr().unwrap().port(), 9091);
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
    for private in ["PRIVATE_RSS_GUID", "PRIVATE_RSS_LOCATOR", PASSWORD, SESSION] {
        assert!(
            !text.contains(private),
            "{private} cannot enter API responses: {method} {path}: {text}"
        );
    }
    (
        status,
        if status == 204 {
            Value::Null
        } else {
            serde_json::from_str(&text)
                .unwrap_or_else(|_| panic!("{method} {path}: {status} {text}"))
        },
    )
}
struct App {
    _scratch: Scratch,
    db: Arc<Database>,
    base: String,
    state: Arc<Remote>,
    client: providers::RefreshClient,
    metadata: Arc<hrrdarr::metadata::MetadataClient>,
    indexer: Value,
    transmission: Value,
    sab: Value,
    roots: Vec<PathBuf>,
    _servers: (Server, Server),
}
fn transmission_settings(origin: &str) -> Value {
    // TV by category (subfolder + label), movies by explicit directory, on one shared client.
    json!({"implementation":"transmission","endpoint":format!("{origin}/transmission"),
        "tv":{"placement":{"strategy":"category","category":"tv-hrrdarr"},"recent_priority":"first","older_priority":"last"},
        "movies":{"placement":{"strategy":"directory","directory":"/remote/films"},"recent_priority":"last","older_priority":"first"}})
}
async fn app(label: &str) -> App {
    let scratch = Scratch(std::env::temp_dir().join(format!(
        "hrrdarr-transmission-{label}-{}",
        uuid::Uuid::new_v4()
    )));
    std::fs::create_dir(&scratch.0).unwrap();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let state = Arc::new(Remote::default());
    *state.password.lock().unwrap() = PASSWORD.into();
    let (origin, upstream) = serve(
        axum::Router::new()
            .fallback(remote)
            .with_state(state.clone()),
    )
    .await;
    *state.origin.lock().unwrap() = origin.clone();
    let metadata = Arc::new(
        hrrdarr::metadata::MetadataClient::with_origins(
            &format!("{origin}/"),
            &format!("{origin}/"),
        )
        .unwrap(),
    );
    let key = Arc::new(providers::CredentialKey::from_hex(&"33".repeat(32)).unwrap());
    let (router, client) = providers::router_with_refresh(db.clone(), Some(key));
    let router = router
        .merge(commands::router(db.clone()))
        .merge(hrrdarr::search::router(db.clone(), client.clone()))
        .merge(hrrdarr::library::router(db.clone()))
        .merge(hrrdarr::episodes::router(db.clone()))
        .merge(hrrdarr::media_files::router(db.clone()))
        .merge(hrrdarr::library::metadata_router(
            db.clone(),
            metadata.clone(),
        ))
        .merge(hrrdarr::remote_paths::router(db.clone()))
        .merge(hrrdarr::history::router(db.clone()));
    let (base, api) = serve(router).await;
    let c = db.connect().await.unwrap();
    // Profiles are configuration fixtures; targets, receipts, imports and history use real producers.
    c.execute_batch("UPDATE quality_definitions SET min_size=0; INSERT INTO quality_profiles VALUES(1,'tv','HD'),(2,'movies','HD'); INSERT INTO quality_profile_items(profile_id,media_type,quality_id,position,allowed)VALUES(1,'tv',3,0,1),(2,'movies',3,0,1),(1,'tv',7,1,1),(2,'movies',7,1,1); INSERT INTO quality_profile_policies(profile_id,media_type,upgrade_allowed,cutoff_quality_id,min_format_score,cutoff_format_score,min_upgrade_format_score,language_id)VALUES(1,'tv',1,7,0,0,1,NULL),(2,'movies',1,7,0,0,1,-1); INSERT INTO release_delay_policies VALUES('tv',0,0,0),('movies',0,0,0);").await.unwrap();
    let mut created = Vec::new();
    for (settings, credentials) in [
        (
            json!({"implementation":"torznab","endpoint":origin,"tv":{"categories":[5030],"anime_categories":[]},"movies":{"categories":[2030]}}),
            Value::Null,
        ),
        (
            transmission_settings(&origin),
            json!({"kind":"username_password","username":USER,"password":PASSWORD}),
        ),
        // A usenet client exists only to prove releases never cross protocols.
        (
            json!({"implementation":"sabnzbd","endpoint":format!("{origin}/sabnzbd"),"tv":{"category":"tv"},"movies":{"category":"movies"}}),
            Value::Null,
        ),
    ] {
        let mut body = json!({"name":"owned","enabled":true,"priority":1,"settings":settings});
        if !credentials.is_null() {
            body["credentials"] = credentials;
        }
        let (code, value) = request(&base, "POST", "/api/v1/providers", body).await;
        assert_eq!(code, 201, "{value}");
        created.push(value);
    }
    let mut roots = Vec::new();
    for (media, profile) in [("tv", 1), ("movies", 2)] {
        let root = scratch.0.join(media);
        let source = scratch.0.join(format!("source-{media}"));
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&source).unwrap();
        state
            .local_roots
            .lock()
            .unwrap()
            .push((media.into(), source.clone()));
        let (route, input) = if media == "tv" {
            (
                "/api/v1/tv/series/lookup",
                json!({"tvdb_id":101,"path":root,"settings":{"quality_profile_id":profile,"series_type":"standard","use_scene_numbering":false,"monitored":true}}),
            )
        } else {
            (
                "/api/v1/movies/lookup",
                json!({"tmdb_id":201,"path":root,"settings":{"quality_profile_id":profile,"minimum_availability":"released","monitored":true}}),
            )
        };
        let (code, v) = request(&base, "POST", route, input).await;
        assert_eq!(code, 201, "{v}");
        assert_eq!(v["id"], 1, "colliding native IDs must retain typed targets");
        let (code, v) = request(
            &base,
            "POST",
            &format!("/api/v1/{media}/remote-path-mappings"),
            json!({"host":"127.0.0.1","remote_path":"/remote","local_path":source}),
        )
        .await;
        assert_eq!(code, 201, "{v}");
        roots.push(root);
    }
    let sab = created.remove(2);
    let transmission = created.remove(1);
    App {
        _scratch: scratch,
        db,
        base,
        state,
        client,
        metadata,
        indexer: created.remove(0),
        transmission,
        sab,
        roots,
        _servers: (upstream, api),
    }
}
fn target(indexer: &Value, client: &Value, media: &str) -> Value {
    json!({"media_type":media,"indexer_id":indexer["id"],"indexer_revision":indexer["revision"],"client_id":client["id"],"client_revision":client["revision"]})
}
async fn enqueue(base: &str, target: Value) -> (u16, Value) {
    request(
        base,
        "POST",
        "/api/v1/rss/commands",
        json!({"target":target,"priority":"high"}),
    )
    .await
}
async fn wait_command(base: &str, id: &Value, status: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let (code, v) = request(
                base,
                "GET",
                &format!("/api/v1/rss/commands/{}", id.as_str().unwrap()),
                Value::Null,
            )
            .await;
            assert_eq!(code, 200, "{v}");
            if v["status"] == status {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await
    .expect("rss command deadline")
}
async fn count(db: &Database, sql: &str) -> i64 {
    let c = db.connect().await.unwrap();
    c.query(sql, ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<i64>(0)
        .unwrap()
}

#[tokio::test]
async fn torznab_to_transmission_grab_refresh_and_import_for_both_domains() {
    let app = app("flow").await;
    let base = &app.base.clone();
    let id = app.transmission["id"].as_str().unwrap().to_owned();
    // The saved Test performs the session handshake and validates both placements.
    let (code, tested) = request(
        base,
        "POST",
        &format!("/api/v1/providers/{id}/test"),
        Value::Null,
    )
    .await;
    assert_eq!(code, 200, "{tested}");
    assert_eq!(tested["result"]["transmission_version"], "4.0.6 (owned)");
    assert_eq!(tested["result"]["rpc_version"], 17);
    assert_eq!(tested["result"]["domains"], json!(["tv", "movies"]));
    assert_eq!(tested["result"]["labels_supported"], true);
    // The stored password is write-only.
    let (_, saved) = request(base, "GET", &format!("/api/v1/providers/{id}"), Value::Null).await;
    assert_eq!(saved["has_credentials"], true);
    // Server readback includes the defaulted add-paused flag and the (empty) eligibility tags.
    let mut expected = transmission_settings(app.state.origin.lock().unwrap().as_str());
    for media in ["tv", "movies"] {
        expected[media]["add_paused"] = json!(false);
        expected[media]["tags"] = json!([]);
    }
    assert_eq!(saved["settings"], expected);

    // A Torznab release can never be sent to the usenet client.
    for media in ["tv", "movies"] {
        let (code, v) = enqueue(base, target(&app.indexer, &app.sab, media)).await;
        assert_eq!(
            (code, v["error"]["code"].as_str()),
            (409, Some("protocol_mismatch")),
            "{v}"
        );
    }
    let mut pending = Vec::new();
    for media in ["tv", "movies"] {
        let (code, v) = enqueue(base, target(&app.indexer, &app.transmission, media)).await;
        assert_eq!(code, 202, "{v}");
        pending.push(v);
    }
    let runtime =
        commands::start_with_metadata(app.db.clone(), app.client.clone(), app.metadata.clone())
            .await
            .unwrap();
    for command in &pending {
        wait_command(base, &command["id"], "succeeded").await;
    }
    tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            let (_, v) = request(base, "GET", "/api/v1/download-processing", Value::Null).await;
            if v["items"].as_array().is_some_and(|rows| {
                rows.len() == 2 && rows.iter().all(|r| r["status"] == "imported")
            }) {
                break;
            }
            if let Some(rows) = v["items"].as_array() {
                assert!(!rows.iter().any(|r| r["status"] == "blocked"), "{v}");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("scheduled Transmission observation must hand both completed torrents to import");

    let adds = app.state.adds.lock().unwrap().clone();
    assert_eq!(adds.len(), 2, "one torrent-add per domain");
    for (i, name) in [
        "Harbor.S01E01.1080p.WEB-DL.mkv",
        "Harbor.2020.1080p.WEB-DL.mkv",
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(
            std::fs::read(app.roots[i].join(name)).unwrap(),
            vec![if i == 0 { 1 } else { 2 }; FILE_BYTES],
            "each domain imports only its own torrent"
        );
    }
    let (code, episode) = request(base, "GET", "/api/v1/episodes/1", Value::Null).await;
    assert_eq!(code, 200, "{episode}");
    assert_eq!(episode["has_file"], true);
    let (code, movies) =
        request(base, "GET", "/api/v1/movies/files?movie_ids=1", Value::Null).await;
    assert_eq!(code, 200, "{movies}");
    assert_eq!(movies["total"], 1);
    // Queue projection per domain shows only its own torrent, completed, by info hash.
    let tv_hash = tv_torrent().1;
    for (media, hash, category) in [
        ("tv", tv_hash.as_str(), "tv-hrrdarr"),
        ("movies", MOVIE_HASH, ""),
    ] {
        let (code, page) = request(
            base,
            "GET",
            &format!("/api/v1/queue?provider_id={id}&media_type={media}"),
            Value::Null,
        )
        .await;
        assert_eq!(code, 200, "{page}");
        let items = page["items"].as_array().unwrap();
        assert_eq!(items.len(), 1, "{page}");
        let download = &items[0]["download"];
        assert_eq!(
            (
                download["hash"].as_str(),
                download["category"].as_str(),
                download["status"].as_str(),
                download["completed"].as_bool()
            ),
            (Some(hash), Some(category), Some("completed"), Some(true)),
            "{page}"
        );
        assert_eq!(
            items[0]["association"]["media_type"],
            if media == "tv" { "episode" } else { "movie" },
            "{page}"
        );
    }
    assert_eq!(
        count(&app.db, "SELECT count(*) FROM import_history").await,
        2
    );
    runtime.shutdown().await;

    // Restarting the worker observes again without resubmitting either torrent.
    let restarted =
        commands::start_with_metadata(app.db.clone(), app.client.clone(), app.metadata.clone())
            .await
            .unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    restarted.shutdown().await;
    assert_eq!(
        app.state.adds.lock().unwrap().len(),
        2,
        "restart must not resubmit downloads"
    );
    // A later RSS pass over the same releases does not add them again either.
    let runtime =
        commands::start_with_metadata(app.db.clone(), app.client.clone(), app.metadata.clone())
            .await
            .unwrap();
    for media in ["tv", "movies"] {
        let (code, v) = enqueue(base, target(&app.indexer, &app.transmission, media)).await;
        assert_eq!(code, 202, "{v}");
        wait_command(base, &v["id"], "succeeded").await;
    }
    runtime.shutdown().await;
    assert_eq!(
        app.state.adds.lock().unwrap().len(),
        2,
        "repeated RSS must not resubmit downloads"
    );
    assert_eq!(
        count(&app.db, "SELECT count(*) FROM import_history").await,
        2
    );
}

#[tokio::test]
async fn lost_torrent_add_response_is_reconciled_without_a_second_submission() {
    let app = app("lost").await;
    let base = &app.base;
    app.state.lose_add_response.store(true, Ordering::SeqCst);
    let (code, command) = enqueue(base, target(&app.indexer, &app.transmission, "movies")).await;
    assert_eq!(code, 202, "{command}");
    let runtime =
        commands::start_with_metadata(app.db.clone(), app.client.clone(), app.metadata.clone())
            .await
            .unwrap();
    // Reconciliation runs after the bounded uncertainty delay, never immediately.
    let receipt = tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            let (code, v) = request(base, "GET", "/api/v1/rss/candidates", Value::Null).await;
            assert_eq!(code, 200, "{v}");
            if let Some(item) = v["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["status"] == "needs_attention")
            {
                return item.clone();
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "an unconfirmed add must surface for attention; adds={}",
            app.state.adds.lock().unwrap().len()
        )
    });
    // The torrent was found by its info hash, but an uncertain add is never silently promoted.
    assert_eq!(receipt["error_code"], "presence_unconfirmed");
    assert_eq!(receipt["target"]["media_type"], "movies");
    assert_eq!(app.state.adds.lock().unwrap().len(), 1);
    app.state.lose_add_response.store(false, Ordering::SeqCst);
    let (code, again) = enqueue(base, target(&app.indexer, &app.transmission, "movies")).await;
    assert_eq!(code, 202, "{again}");
    wait_command(base, &again["id"], "succeeded").await;
    assert_eq!(
        app.state.adds.lock().unwrap().len(),
        1,
        "retry after a lost response must not double-submit"
    );
    runtime.shutdown().await;
}

#[tokio::test]
async fn provider_api_validates_and_tests_transmission_configuration() {
    let app = app("config").await;
    let base = &app.base;
    let origin = app.state.origin.lock().unwrap().clone();
    let create = |settings: Value, credentials: Value| {
        let mut body = json!({"name":"tx","enabled":true,"priority":1,"settings":settings});
        if !credentials.is_null() {
            body["credentials"] = credentials;
        }
        async move { request(base, "POST", "/api/v1/providers", body).await }
    };
    let placement = |tv: Value, movies: Value| json!({"implementation":"transmission","endpoint":format!("{origin}/transmission"),"tv":{"placement":tv},"movies":{"placement":movies}});
    let login = json!({"kind":"username_password","username":USER,"password":PASSWORD});
    for (tv, movies) in [
        // Shared labels or nested directories would let one domain import the other's downloads.
        (
            json!({"strategy":"category","category":"media"}),
            json!({"strategy":"category","category":"MEDIA"}),
        ),
        (
            json!({"strategy":"directory","directory":"/remote"}),
            json!({"strategy":"directory","directory":"/remote/films"}),
        ),
        (
            json!({"strategy":"category","category":"tv/sub"}),
            json!({"strategy":"category","category":"movies"}),
        ),
        (
            json!({"strategy":"directory","directory":"relative"}),
            json!({"strategy":"category","category":"movies"}),
        ),
    ] {
        let (code, v) = create(placement(tv, movies), login.clone()).await;
        assert_eq!(code, 400, "{v}");
    }
    // Transmission authenticates with HTTP basic credentials only.
    let (code, v) = create(
        transmission_settings(&origin),
        json!({"kind":"api_key","api_key":"k"}),
    )
    .await;
    assert_eq!(code, 400, "{v}");
    // The schema advertises a Transmission template without contacting any client.
    let (code, schema) = request(
        base,
        "GET",
        "/api/v1/providers/schema?media_type=movies&kind=download_client",
        Value::Null,
    )
    .await;
    assert_eq!(code, 200, "{schema}");
    let template = schema["templates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["implementation"] == "transmission")
        .cloned()
        .expect("transmission template");
    assert_eq!(template["defaults"]["kind"], "transmission_client");
    assert_eq!(
        (
            template["enabled"].as_bool(),
            template["defaults"]["add_paused"].as_bool()
        ),
        (Some(false), Some(false))
    );

    let test = |id: Value| async move {
        request(
            base,
            "POST",
            &format!("/api/v1/providers/{}/test", id.as_str().unwrap()),
            Value::Null,
        )
        .await
    };
    let (code, wrong) = create(
        transmission_settings(&origin),
        json!({"kind":"username_password","username":USER,"password":"not-the-password"}),
    )
    .await;
    assert_eq!(code, 201, "{wrong}");
    let (code, v) = test(wrong["id"].clone()).await;
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (502, Some("authentication")),
        "{v}"
    );
    assert!(!v.to_string().contains("not-the-password"));
    let (_, saved) = request(
        base,
        "GET",
        &format!("/api/v1/providers/{}", wrong["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_eq!(
        (
            saved["test_status"].as_str(),
            saved["last_test"]["error_code"].as_str()
        ),
        (Some("failure"), Some("authentication")),
        "{saved}"
    );
    // Editing only the password (credential replace) keeps placements and fixes the test.
    let update = json!({"revision":saved["revision"],"name":saved["name"],"enabled":true,"priority":1,"settings":saved["settings"],"credentials":login});
    let (code, updated) = request(
        base,
        "PUT",
        &format!("/api/v1/providers/{}", wrong["id"].as_str().unwrap()),
        update.clone(),
    )
    .await;
    assert_eq!(code, 200, "{updated}");
    assert_eq!(updated["settings"], saved["settings"]);
    let (code, v) = test(wrong["id"].clone()).await;
    assert_eq!(code, 200, "{v}");
    // Unsaved drafts use the same Transmission probe without persisting anything.
    let before = count(&app.db, "SELECT count(*) FROM providers").await;
    let (code, v) = request(base, "POST", "/api/v1/providers/test-draft", json!({"source":null,"config":{"name":"draft","enabled":false,"priority":1,"settings":transmission_settings(&origin),"credentials":login}})).await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["result"]["domains"], json!(["tv", "movies"]));
    assert_eq!(
        count(&app.db, "SELECT count(*) FROM providers").await,
        before
    );
    // A TV category whose effective folder nests in the movie directory is caught by Test.
    let (code, nested) = create(
        placement(
            json!({"strategy":"category","category":"films"}),
            json!({"strategy":"directory","directory":"/remote/films/hd"}),
        ),
        login.clone(),
    )
    .await;
    assert_eq!(code, 201, "{nested}");
    let (code, v) = test(nested["id"].clone()).await;
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (422, Some("transmission_domains_overlap")),
        "{v}"
    );
}

#[tokio::test]
async fn stored_cross_protocol_binding_is_rejected_at_dispatch_without_submission() {
    let app = app("binding").await;
    let base = &app.base;
    // A Torznab preference for the usenet client is stored, not erased.
    let path = format!("/api/v1/providers/{}", app.indexer["id"].as_str().unwrap());
    let mut bound = json!({"revision":app.indexer["revision"],"name":app.indexer["name"],"enabled":true,"priority":1,"settings":app.indexer["settings"]});
    bound["settings"]["movies"]["download_client_id"] = app.sab["id"].clone();
    let (code, indexer) = request(base, "PUT", &path, bound).await;
    assert_eq!(code, 200, "{indexer}");
    let (code, command) = request(base, "POST", "/api/v1/rss/commands", json!({"target":{"media_type":"movies","indexer_id":indexer["id"],"indexer_revision":indexer["revision"],"client_selection":{"kind":"default"}},"priority":"high"})).await;
    assert_eq!(code, 202, "{command}");
    let runtime =
        commands::start_with_metadata(app.db.clone(), app.client.clone(), app.metadata.clone())
            .await
            .unwrap();
    let receipt = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let (_, v) = request(base, "GET", "/api/v1/rss/candidates", Value::Null).await;
            if let Some(item) = v["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["status"] == "rejected" || v["status"] == "needs_attention")
            {
                return item.clone();
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await
    .expect("the cross-protocol preference must be a visible rejection");
    assert_eq!(receipt["status"], "rejected", "{receipt}");
    assert_eq!(receipt["error_code"], "unsupported_target", "{receipt}");
    runtime.shutdown().await;
    assert!(
        app.state.adds.lock().unwrap().is_empty(),
        "nothing is sent to the torrent client"
    );
    // The unbound TV domain's default selection chooses Transmission (the only torrent client).
    let (code, command) = request(base, "POST", "/api/v1/rss/commands", json!({"target":{"media_type":"tv","indexer_id":indexer["id"],"indexer_revision":indexer["revision"],"client_selection":{"kind":"default"}},"priority":"high"})).await;
    assert_eq!(code, 202, "{command}");
    let runtime =
        commands::start_with_metadata(app.db.clone(), app.client.clone(), app.metadata.clone())
            .await
            .unwrap();
    wait_command(base, &command["id"], "succeeded").await;
    tokio::time::timeout(Duration::from_secs(30), async {
        while app.state.adds.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await
    .expect("default selection dispatches the torrent release to Transmission");
    runtime.shutdown().await;
    let adds = app.state.adds.lock().unwrap().clone();
    assert_eq!(adds.len(), 1);
    assert_eq!(adds[0]["download-dir"], "/remote/tv-hrrdarr");
}
