//! Actual process capture, with owned peers and scratch state only.
use axum::{
    body::Bytes,
    extract::{OriginalUri, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use hrrdarr::db::Database;
use serde_json::{Value, json};
use std::{
    io::Read,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
const API_KEY: &str = "sentinel-indexer-api-key";
const USER: &str = "sentinel-client-user";
const PASSWORD: &str = "sentinel-client-password";
const COOKIE: &str = "sentinel-session-cookie";
const UPSTREAM: &str = "sentinel-upstream-error-body";
const TEMPLATE: &str = "sentinel-invalid-template-secret";
const TV: &str = "1111111111111111111111111111111111111111";
const MOVIE: &str = "2222222222222222222222222222222222222222";
#[derive(Default)]
struct Peer {
    items: Mutex<Vec<Value>>,
    fail: AtomicBool,
    authenticated: AtomicBool,
}
async fn peer(
    State(s): State<Arc<Peer>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let q: std::collections::HashMap<_, _> =
        url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();
    if uri.path() == "/api" || uri.path() == "/" {
        assert_eq!(q.get("apikey").map(String::as_str), Some(API_KEY));
        if s.fail.load(Ordering::SeqCst) {
            return (StatusCode::UNAUTHORIZED, UPSTREAM).into_response();
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
        return format!(r#"<rss xmlns:torznab="http://torznab.com/schemas/2015/feed"><channel><item><title>{title}</title><guid>sentinel-private-guid-{hash}</guid><pubDate>Mon, 01 Jan 2024 12:00:00 +0000</pubDate><link>magnet:?xt=urn:btih:{hash}</link><torznab:attr name="category" value="{cat}"/><torznab:attr name="size" value="1073741824"/></item></channel></rss>"#).into_response();
    }
    if uri.path().ends_with("auth/login") {
        let fields: std::collections::HashMap<_, _> =
            url::form_urlencoded::parse(&body).into_owned().collect();
        assert_eq!(fields.get("username").map(String::as_str), Some(USER));
        assert_eq!(fields.get("password").map(String::as_str), Some(PASSWORD));
        s.authenticated.store(true, Ordering::SeqCst);
        if s.fail.load(Ordering::SeqCst) {
            return (StatusCode::FORBIDDEN, UPSTREAM).into_response();
        }
        return (
            [("set-cookie", format!("SID={COOKIE}; HttpOnly; Path=/"))],
            "Ok.",
        )
            .into_response();
    }
    if !headers
        .get("cookie")
        .is_some_and(|v| v.to_str().unwrap().contains(COOKIE))
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    if s.fail.load(Ordering::SeqCst) {
        return (StatusCode::FORBIDDEN, UPSTREAM).into_response();
    }
    if uri.path().ends_with("webapiVersion") {
        return "2.8.4".into_response();
    }
    if uri.path().ends_with("app/version") {
        return "v4.6.0".into_response();
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
    if uri.path().ends_with("torrents/add") {
        let fields: std::collections::HashMap<_, _> =
            url::form_urlencoded::parse(&body).into_owned().collect();
        let cat = fields.get("category").unwrap();
        let hash = if cat == "tv" { TV } else { MOVIE };
        s.items.lock().unwrap().push(json!({"hash":hash,"infohash_v1":hash,"infohash_v2":"","category":cat,"name":"safe item","state":"uploading","progress":1.0,"size":1048576,"amount_left":0,"dlspeed":0,"upspeed":1,"eta":0,"ratio":0.2,"seeding_time":0,"priority":5,"force_start":false,"ratio_limit":-2.0,"seeding_time_limit":-2,"inactive_seeding_time_limit":-1,"seq_dl":false,"f_l_piece_prio":false,"auto_tmm":false,"tags":"","save_path":"/remote","content_path":"/remote/item"}));
        return "Ok.".into_response();
    }
    if uri.path().ends_with("torrents/info") {
        return axum::Json(json!(
            s.items
                .lock()
                .unwrap()
                .iter()
                .filter(|v| q
                    .get("category")
                    .is_none_or(|c| v["category"] == c.as_str())
                    && q.get("hashes").is_none_or(|h| v["hash"] == h.as_str()))
                .cloned()
                .collect::<Vec<_>>()
        ))
        .into_response();
    }
    if uri.path().ends_with("torrents/files") {
        let name = if q.get("hash").unwrap() == TV {
            "Harbor.S01E01.1080p.WEB-DL.mkv"
        } else {
            "Harbor.2020.1080p.WEB-DL.mkv"
        };
        return axum::Json(
            json!([{ "index":0,"name":name,"size":1048576,"progress":1.0,"priority":1}]),
        )
        .into_response();
    }
    if uri.path().ends_with("torrents/properties") {
        return axum::Json(json!({"save_path":"/remote","total_size":1048576,"addition_date":1,"completion_date":2,"seeding_time":0})).into_response();
    }
    (StatusCode::NOT_FOUND, "unexpected owned mock request").into_response()
}
struct Process {
    child: std::process::Child,
    directory: PathBuf,
}
impl Process {
    fn output(&self) -> String {
        let mut bytes = Vec::new();
        std::fs::File::open(self.directory.join("output"))
            .unwrap()
            .take(65537)
            .read_to_end(&mut bytes)
            .unwrap();
        assert!(bytes.len() <= 65536, "process log exceeded bound");
        String::from_utf8(bytes).unwrap()
    }
    async fn start(directory: PathBuf) -> (Self, String) {
        let output = std::fs::File::create(directory.join("output")).unwrap();
        let child = std::process::Command::new(env!("CARGO_BIN_EXE_hrrdarr"))
            .env_clear()
            .env("HRRDARR_DATABASE_PATH", directory.join("db"))
            .env("HRRDARR_PROVIDER_KEY", "11".repeat(32))
            .env("HRRDARR_BIND", "127.0.0.1:0")
            .current_dir(&directory)
            .stdout(output.try_clone().unwrap())
            .stderr(output)
            .spawn()
            .unwrap();
        let mut process = Self { child, directory };
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            assert!(
                process.child.try_wait().unwrap().is_none(),
                "server exited: {}",
                process.output()
            );
            if let Some(base) = process
                .output()
                .lines()
                .find_map(|l| l.strip_prefix("hrrdarr listening on ").map(str::to_owned))
            {
                return (process, base);
            }
            assert!(Instant::now() < deadline, "server startup timeout");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
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
    (
        status,
        if status == 204 {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or_else(|_| panic!("{status} {text}"))
        },
    )
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
            json!({"name":"owned","enabled":true,"priority":1,"credentials":if settings["implementation"] == "torznab" {json!({"kind":"api_key","api_key":API_KEY})}else{json!({"kind":"username_password","username":USER,"password":PASSWORD})},"settings":settings}),
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
async fn wait_status(base: &str, path: &str, status: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let (code, v) = request(base, "GET", path, Value::Null).await;
            assert_eq!(code, 200, "{v}");
            if v["status"] == status {
                return v;
            }
            if v["status"] == "blocked"
                || v["status"] == "failed"
                || (v["status"] == "importing"
                    && !v["error_code"].is_null()
                    && v["resume_requested"] != true)
            {
                panic!("unexpected terminal: {v}")
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await
    .expect("processing deadline")
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_provider_and_background_diagnostics_exclude_secret_values() {
    tokio::time::timeout(Duration::from_secs(90), exercise())
        .await
        .expect("runtime capture deadline");
}
async fn exercise() {
    let directory =
        std::env::temp_dir().join(format!("hrrdarr-runtime-log-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let _scratch = Scratch(directory.clone());
    let db = Database::open_local(directory.join("db")).await.unwrap();
    let c = db.connect().await.unwrap();
    c.execute_batch("INSERT INTO series(id,tvdb_id,title,path)VALUES(1,101,'Harbor','/fixture');INSERT INTO seasons(series_id,number)VALUES(1,1);INSERT INTO episodes(id,series_id,season,number,title,runtime,air_date_utc)VALUES(1,1,1,1,'Pilot',45,'2020-01-01 00:00:00');INSERT INTO movie_metadata(id,tmdb_id,title,year,runtime,digital_release)VALUES(1,201,'Harbor',2020,100,'2020-01-01 00:00:00');INSERT INTO movies(id,metadata_id,path)VALUES(1,1,'/fixture');UPDATE quality_definitions SET min_size=0;INSERT INTO quality_profiles VALUES(1,'tv','HD'),(2,'movies','HD');INSERT INTO quality_profile_items(profile_id,media_type,quality_id,position,allowed)VALUES(1,'tv',3,0,1),(2,'movies',3,0,1);INSERT INTO quality_profile_policies(profile_id,media_type,upgrade_allowed,cutoff_quality_id,min_format_score,cutoff_format_score,min_upgrade_format_score,language_id)VALUES(1,'tv',1,3,0,0,1,NULL),(2,'movies',1,3,0,0,1,-1);INSERT INTO library_settings(media_type,series_id,quality_profile_id,series_type,use_scene_numbering)VALUES('tv',1,1,'standard',0);INSERT INTO library_settings(media_type,movie_id,quality_profile_id,minimum_availability)VALUES('movies',1,2,'released');INSERT INTO release_delay_policies VALUES('tv',0,0,0),('movies',0,0,0);").await.unwrap();
    for (media, table, name) in [
        ("tv", "series", "Harbor.S01E01.1080p.WEB-DL.mkv"),
        ("movies", "movies", "Harbor.2020.1080p.WEB-DL.mkv"),
    ] {
        let root = directory.join(media);
        let source = directory.join(format!("source-{media}"));
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join(name), vec![1u8; 1048576]).unwrap();
        c.execute(
            &format!("UPDATE {table} SET path=? WHERE id=1"),
            [root.to_str().unwrap()],
        )
        .await
        .unwrap();
        let field = if media == "tv" {
            "standard_episode_format"
        } else {
            "standard_movie_format"
        };
        // Corruption bypasses validated naming PUT: log safety must also hold for stored inputs.
        c.execute(
            &format!("UPDATE naming_settings SET revision=revision+1,rename_enabled=1,{field}=? WHERE domain=?"),
            libsql::params![format!("{{{TEMPLATE}}}"), media],
        )
        .await
        .unwrap();
    }
    let peer_state = Arc::new(Peer::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let app = axum::Router::new()
        .fallback(peer)
        .with_state(peer_state.clone());
    let _peer = Server(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap()
    }));
    drop(c);
    drop(db);
    let (process, base) = Process::start(directory.clone()).await;
    let (indexer, client) = providers(&base, &origin).await;
    for fail in [false, true] {
        peer_state.fail.store(fail, Ordering::SeqCst);
        for provider in [&indexer, &client] {
            let (code, value) = request(
                &base,
                "POST",
                &format!(
                    "/api/v1/providers/{}/test",
                    provider["id"].as_str().unwrap()
                ),
                json!({}),
            )
            .await;
            assert_eq!(code, if fail { 502 } else { 200 }, "{value}");
        }
    }
    peer_state.fail.store(false, Ordering::SeqCst);
    for media in ["tv", "movies"] {
        let (code,value)=request(&base,"POST",&format!("/api/v1/{media}/remote-path-mappings"),json!({"host":"127.0.0.1","remote_path":"/remote","local_path":directory.join(format!("source-{media}"))})).await;
        assert_eq!(code, 201, "{value}");
        let (code,value)=request(&base,"PUT",&format!("/api/v1/download-processing/policies/{}/{media}",client["id"].as_str().unwrap()),json!({"provider_revision":client["revision"],"revision":null,"enabled":true,"mode":"copy"})).await;
        assert_eq!(code, 200, "{value}");
        let command = enqueue(&base, target(&indexer, &client, media)).await;
        let done = wait_status(
            &base,
            &format!("/api/v1/rss/commands/{}", command["id"].as_str().unwrap()),
            "succeeded",
        )
        .await;
        assert_eq!(done["observed"], 1, "{done}");
        let (_, rows) = request(
            &base,
            "GET",
            &format!(
                "/api/v1/rss/candidates?command_id={}",
                command["id"].as_str().unwrap()
            ),
            Value::Null,
        )
        .await;
        let receipt = &rows["items"][0]["id"];
        assert!(receipt.is_string(), "{rows}");
        let (code,value)=request(&base,"POST","/api/v1/download-processing",json!({"provider_id":client["id"],"provider_revision":client["revision"],"media_type":media,"receipt_ids":[receipt]})).await;
        assert_eq!(code, 202, "{value}");
        let blocked = wait_status(
            &base,
            &format!("/api/v1/download-processing/{}", receipt.as_str().unwrap()),
            "blocked",
        )
        .await;
        assert_eq!(
            blocked["reasons"],
            json!(["naming_render_failed"]),
            "{blocked}"
        );
    }
    let mut settlements = Vec::new();
    for fail in [false, true] {
        peer_state.fail.store(fail, Ordering::SeqCst);
        for media in ["tv", "movies"] {
            let (code, command)=request(&base,"POST","/api/v1/commands",json!({"name":"refresh_downloads","target":{"provider_id":client["id"],"media_type":media},"provider_revision":client["revision"],"priority":"normal"})).await;
            assert_eq!(code, 202, "{command}");
            settlements.push(format!(
                "event=command_settled command_id={} media_type={media} status={}",
                command["id"].as_str().unwrap(),
                if fail { "failed" } else { "succeeded" }
            ));
            wait_status(
                &base,
                &format!("/api/v1/commands/{}", command["id"].as_str().unwrap()),
                if fail { "failed" } else { "succeeded" },
            )
            .await;
        }
    }
    assert!(peer_state.authenticated.load(Ordering::SeqCst));
    // Settlement commits before its diagnostic is emitted; wait for the actual lines.
    let output = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let output = process.output();
            if settlements.iter().all(|event| output.contains(event)) {
                break output;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("missing terminal worker diagnostics");
    assert_eq!(
        output.matches("provider_test_completed").count(),
        4,
        "{output}"
    );
    assert_eq!(
        output.matches("naming_render_failed").count(),
        2,
        "{output}"
    );
    assert!(
        output.contains("media_type=tv") && output.contains("media_type=movies"),
        "{output}"
    );
    for event in settlements {
        assert!(output.contains(&event), "missing worker event: {event}");
    }
    for field in ["standard_episode_format", "standard_movie_format"] {
        assert!(
            output
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .any(|event| event["event"] == "naming_render_failed"
                    && event["field"] == field
                    && event["error_class"] == "invalid_template"),
            "{output}"
        );
    }
    assert!(
        !output.contains(&"11".repeat(32)),
        "provider encryption key reached logs"
    );
    for secret in [
        API_KEY,
        USER,
        PASSWORD,
        COOKIE,
        UPSTREAM,
        TEMPLATE,
        "sentinel-private-guid",
    ] {
        assert!(
            !output.contains(secret),
            "secret reached actual server diagnostics: {secret}"
        );
    }
    drop(process);
}
