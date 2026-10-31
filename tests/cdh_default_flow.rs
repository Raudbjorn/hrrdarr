// Owned protocol fixture adapted from download_processing.rs; no real endpoints/media.
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
    time::{Duration, Instant},
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
    completed: AtomicU8,
    detail_reads: AtomicU8,
    endpoint: Mutex<String>,
    // scn.002: overrides the tv search-feed title and the completed-download filename below
    // (stem only, no extension) so a daily/anime-shaped release can be exercised end to end
    // without disturbing the fixed "Harbor.S01E01.1080p.WEB-DL" title every other tv scenario
    // in this file relies on. The wire hash stays the plain `TV` constant either way -- only
    // the advertised/observed filename changes.
    release_override: Mutex<Option<String>>,
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
    body: Bytes,
) -> Response {
    let q: std::collections::HashMap<_, _> =
        url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();
    if uri.path() == "/shows/en/101" {
        // airDate/absoluteEpisodeNumber are additive: every existing standard-typed scenario
        // ignores both columns, but a daily/anime-typed series (scn.002) needs a real air date
        // and absolute number on this same fixture episode to match end to end.
        return axum::Json(json!({"tvdbId":101,"title":"Harbor","seasons":[{"seasonNumber":1}],"episodes":[{"tvdbId":501,"seasonNumber":1,"episodeNumber":1,"title":"Pilot","runtime":45,"airDate":"2020-01-01","airDateUtc":"2020-01-01T00:00:00Z","absoluteEpisodeNumber":1}]})).into_response();
    }
    if uri.path() == "/movie/201" {
        return axum::Json(json!({"tmdbId":201,"title":"Harbor","year":2020,"runtime":100,"digitalRelease":"2020-01-01T00:00:00Z"})).into_response();
    }
    if uri.path().ends_with("torrents/files") {
        s.detail_reads.fetch_add(1, Ordering::SeqCst);
        if s.mode.load(Ordering::SeqCst) == 11 {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        let hash = q.get("hash").unwrap();
        let tv = hash == TV || hash.starts_with('3');
        let upgrade = hash.starts_with('3') || hash.starts_with('4');
        let mut name = if let Some(stem) = s.release_override.lock().unwrap().clone() {
            format!("{stem}.mkv")
        } else if upgrade {
            if tv {
                "Harbor.S01E01.1080p.Bluray.mkv"
            } else {
                "Harbor.2020.1080p.Bluray.mkv"
            }
            .to_string()
        } else if tv {
            "Harbor.S01E01.1080p.WEB-DL.mkv".to_string()
        } else {
            "Harbor.2020.1080p.WEB-DL.mkv".to_string()
        };
        if s.mode.load(Ordering::SeqCst) == 9 {
            name = if tv {
                "Harbor.S01E02.1080p.WEB-DL.mkv"
            } else {
                "Harbor.2021.1080p.WEB-DL.mkv"
            }
            .into()
        }
        if tv {
            name = format!("batch/{name}")
        }
        let mut files =
            vec![json!({"index":0,"name":name,"size":1048576,"progress":1.0,"priority":1})];
        if s.mode.load(Ordering::SeqCst) == 10 {
            let mut duplicate = files[0].clone();
            duplicate["index"] = json!(1);
            duplicate["name"] = json!(format!("duplicate/{name}"));
            files.push(duplicate);
        }
        files.push(json!({"index":2,"name":if tv{"Harbor.S01E01.sample.1080p.WEB-DL.mkv"}else{"Harbor.2020.sample.1080p.WEB-DL.mkv"},"size":1,"progress":1.0,"priority":1}));
        return axum::Json(json!(files)).into_response();
    }
    if uri.path().ends_with("torrents/properties") {
        return axum::Json(json!({"save_path":"/remote","total_size":1048576,"addition_date":1,"completion_date":2,"seeding_time":0})).into_response();
    }
    if uri.path() == "/api" || uri.path() == "/" {
        if q.get("t").is_some_and(|v| v == "caps") {
            return r#"<caps><limits max="100" default="100"/><searching><search available="yes" supportedParams="q"/><tv-search available="yes" supportedParams="q,tvdbid,season,ep"/><movie-search available="yes" supportedParams="q,tmdbid"/></searching><categories><category id="5000"><subcat id="5030"/></category><category id="2000"><subcat id="2030"/></category></categories></caps>"#.into_response();
        }
        let tv = q.get("cat").is_some_and(|v| v.contains("5030"));
        let (title, hash, cat) = if tv {
            (
                s.release_override
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or_else(|| "Harbor.S01E01.1080p.WEB-DL".to_string()),
                TV,
                5030,
            )
        } else {
            (
                s.release_override
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or_else(|| "Harbor.2020.1080p.WEB-DL".to_string()),
                MOVIE,
                2030,
            )
        };
        let hash = match s.mode.load(Ordering::SeqCst) {
            6 => {
                if tv {
                    "3333333333333333333333333333333333333333"
                } else {
                    "4444444444444444444444444444444444444444"
                }
            }
            _ => hash,
        };
        let title = if s.mode.load(Ordering::SeqCst) == 6 {
            if tv {
                "Harbor.S01E01.1080p.Bluray".to_string()
            } else {
                "Harbor.2020.1080p.Bluray".to_string()
            }
        } else {
            title
        };
        let date = "Mon, 01 Jan 2024 12:00:00 +0000";
        let link = format!(
            "{}/torrent?passkey=PRIVATE_RSS_LOCATOR",
            s.endpoint.lock().unwrap()
        );
        let magnet =
            format!(r#"<torznab:attr name="magneturl" value="magnet:?xt=urn:btih:{hash}"/>"#);
        // Link-only magnets exercise the shared locator normalizer through a real RSS grab.
        let link = if tv {
            link
        } else {
            format!("magnet:?xt=urn:btih:{hash}")
        };
        let magnet = if tv { magnet } else { String::new() };
        return format!(r#"<rss xmlns:torznab="http://torznab.com/schemas/2015/feed"><channel><item><title>{title}</title><guid>PRIVATE_RSS_GUID-{hash}</guid><pubDate>{date}</pubDate><link>{link}</link>{magnet}<torznab:attr name="language" value="English"/><torznab:attr name="downloadvolumefactor" value="0"/><torznab:attr name="category" value="{cat}"/><torznab:attr name="size" value="1073741824"/></item></channel></rss>"#).into_response();
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
        let items: Vec<_> = items
            .into_iter()
            .map(|mut item| {
                if s.completed.load(Ordering::SeqCst) == 1 {
                    let tv = item["category"] == "tv";
                    item["state"] = json!("uploading");
                    item["progress"] = json!(1.0);
                    item["amount_left"] = json!(0);
                    item["size"] = json!(1048576);
                    item["save_path"] = json!("/remote");
                    item["content_path"] = json!(if tv {
                        "/remote/Harbor.S01E01.1080p.WEB-DL.mkv"
                    } else {
                        "/remote/Harbor.2020.1080p.WEB-DL.mkv"
                    });
                }
                item
            })
            .collect();
        return axum::Json(items).into_response();
    }
    if uri.path().ends_with("torrents/add") {
        assert_eq!(method, Method::POST);
        let pairs: std::collections::HashMap<_, _> =
            url::form_urlencoded::parse(&body).into_owned().collect();
        let category = pairs.get("category").unwrap();
        let hash = if s.mode.load(Ordering::SeqCst) == 6 {
            if category == "tv" {
                "3333333333333333333333333333333333333333"
            } else {
                "4444444444444444444444444444444444444444"
            }
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
        .unwrap_or_else(|e| {
            panic!(
                "{method} {path}: request failed (timeout={}, connect={}); {}",
                e.is_timeout(),
                e.is_connect(),
                phase_report()
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
            serde_json::from_str(&text)
                .unwrap_or_else(|_| panic!("{method} {path}: {status} {text}"))
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
        json!({"target":target,"priority":"high"}),
    )
    .await;
    assert_eq!(code, 202, "{v}");
    v
}
// Real-timer scenario budgets. The inherited 60 s schedule makes each variant take roughly 90 s
// of wall time on an idle host (two ~60 s waits overlap the import and restart phases); both
// variants together were observed at ~187 s serially. A variant that exceeds FLOW_BUDGET failed
// or stalled, and the panic names the phase instead of waiting for an outer harness timeout.
const FLOW_BUDGET: Duration = Duration::from_secs(300);
const IMPORT_BUDGET: Duration = Duration::from_secs(90);
const RESTART_BUDGET: Duration = Duration::from_secs(90);
const COMMAND_BUDGET: Duration = Duration::from_secs(30);
const TEARDOWN_BUDGET: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(50);
static PHASE: Mutex<(&str, Option<Instant>)> = Mutex::new(("not_started", None));
/// Starts a named phase and reports how long the previous one took (stderr; `--nocapture`).
fn phase(name: &'static str) {
    let mut current = PHASE.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(since) = current.1 {
        eprintln!(
            "{}",
            json!({"event":"cdh_phase","phase":current.0,"elapsed_ms":since.elapsed().as_millis()})
        );
    }
    *current = (name, Some(Instant::now()));
}
fn phase_report() -> String {
    let current = PHASE.lock().unwrap_or_else(|p| p.into_inner());
    format!(
        "phase={} in_phase_ms={}",
        current.0,
        current.1.map_or(0, |since| since.elapsed().as_millis())
    )
}
/// Polls `step` until it yields a value. On budget exhaustion it panics with the phase, elapsed
/// time and the last observed state, so a stall is attributable without a debugger. States must be
/// built from whitelisted fields (`pick`), never raw payloads.
async fn poll_until<T, Fut: std::future::Future<Output = Result<T, String>>>(
    what: &str,
    budget: Duration,
    mut step: impl FnMut() -> Fut,
) -> T {
    let started = Instant::now();
    loop {
        match step().await {
            Ok(value) => return value,
            Err(state) if started.elapsed() >= budget => panic!(
                "{what}: unmet after {} ms (budget {} ms); {}; last state: {state}",
                started.elapsed().as_millis(),
                budget.as_millis(),
                phase_report()
            ),
            Err(_) => tokio::time::sleep(POLL_INTERVAL).await,
        }
    }
}
/// Keeps only fields that name a state or reason, never ids, paths or release text.
fn pick(row: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for key in [
        "status",
        "error_code",
        "resume_requested",
        "reasons",
        "media_type",
    ] {
        if let Some(value) = row.get(key) {
            out.insert(key.into(), value.clone());
        }
    }
    Value::Object(out)
}
fn processing_state(v: &Value) -> String {
    let rows: Vec<Value> = v["items"]
        .as_array()
        .map(|rows| rows.iter().map(pick).collect())
        .unwrap_or_default();
    format!("download-processing {}", Value::Array(rows))
}
async fn command_states(db: &hrrdarr::db::Database) -> String {
    let mut out = Vec::new();
    for table in ["rss_commands", "search_commands"] {
        let Ok(c) = db.connect().await else { break };
        let Ok(mut rows) = c
            .query(
                &format!("SELECT status,count(*) FROM {table} GROUP BY status"),
                (),
            )
            .await
        else {
            continue;
        };
        while let Ok(Some(row)) = rows.next().await {
            out.push(format!(
                "{table}.{}={}",
                row.get::<String>(0).unwrap_or_default(),
                row.get::<i64>(1).unwrap_or(-1)
            ));
        }
    }
    out.join(",")
}
async fn wait_status(base: &str, path: &str, status: &str) -> Value {
    let (base, path, status) = (base.to_owned(), path.to_owned(), status.to_owned());
    poll_until("command status", COMMAND_BUDGET, || async {
        let (code, v) = request(&base, "GET", &path, Value::Null).await;
        assert_eq!(code, 200, "{v}");
        if v["status"] == status.as_str() {
            return Ok(v);
        }
        if v["status"] == "blocked"
            || v["status"] == "failed"
            || (v["status"] == "importing"
                && !v["error_code"].is_null()
                && v["resume_requested"] != true)
        {
            panic!("unexpected terminal: {v}")
        }
        Err(format!("wanted {status}; {}", pick(&v)))
    })
    .await
}
#[test]
fn default_cdh_rss_and_search_both_domains_without_policy_or_schedule_writes() {
    // An explicit runtime so teardown (aborted workers, blocking filesystem jobs) has its own
    // finite boundary instead of the implicit unbounded drop of `#[tokio::test]`.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runtime.block_on(async {
            // One serial lease owner, as in the existing import integration fixtures.
            for search in [false, true] {
                let started = Instant::now();
                tokio::time::timeout(FLOW_BUDGET, default_flow(search))
                    .await
                    .unwrap_or_else(|_| {
                        panic!(
                            "default flow (search={search}) exceeded {} s; {}",
                            FLOW_BUDGET.as_secs(),
                            phase_report()
                        )
                    });
                phase("variant_done");
                eprintln!(
                    "{}",
                    json!({"event":"cdh_variant","search":search,"elapsed_ms":started.elapsed().as_millis()})
                );
            }
        })
    }));
    let teardown = Instant::now();
    runtime.shutdown_timeout(TEARDOWN_BUDGET);
    assert!(
        teardown.elapsed() < TEARDOWN_BUDGET,
        "runtime teardown did not drain blocking work within {} s; {}",
        TEARDOWN_BUDGET.as_secs(),
        phase_report()
    );
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
async fn default_flow(search: bool) {
    let scratch =
        Scratch(std::env::temp_dir().join(format!("hrrdarr-cdh-flow-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&scratch.0).unwrap();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let state = Arc::new(Remote::default());
    let (origin, _upstream) = serve(
        axum::Router::new()
            .fallback(remote)
            .with_state(state.clone()),
    )
    .await;
    *state.endpoint.lock().unwrap() = origin.clone();
    let metadata = Arc::new(
        hrrdarr::metadata::MetadataClient::with_origins(
            &format!("{origin}/"),
            &format!("{origin}/"),
        )
        .unwrap(),
    );
    let key = Arc::new(providers::CredentialKey::from_hex(&"11".repeat(32)).unwrap());
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
        .merge(hrrdarr::naming::router(db.clone()))
        .merge(hrrdarr::history::router(db.clone()));
    let (base, _api) = serve(router).await;
    let c = db.connect().await.unwrap();
    // Profiles are configuration fixtures; all catalog targets, receipts, import journals and history use real producers.
    // Any (-1) keeps this fixture language-unrestricted; Original (-2) requires audio matching.
    c.execute_batch("UPDATE quality_definitions SET min_size=0; INSERT INTO quality_profiles VALUES(1,'tv','HD'),(2,'movies','HD'); INSERT INTO quality_profile_items(profile_id,media_type,quality_id,position,allowed)VALUES(1,'tv',3,0,1),(2,'movies',3,0,1),(1,'tv',7,1,1),(2,'movies',7,1,1); INSERT INTO quality_profile_policies(profile_id,media_type,upgrade_allowed,cutoff_quality_id,min_format_score,cutoff_format_score,min_upgrade_format_score,language_id)VALUES(1,'tv',1,7,0,0,1,NULL),(2,'movies',1,7,0,0,1,-1); INSERT INTO release_delay_policies VALUES('tv',0,0,0),('movies',0,0,0);").await.unwrap();
    // Both actual RSS→grab→download→import paths require facts unavailable in the
    // language-free filenames: provider audio language plus the freeleech flag.
    let cf = json!([{"name":"English","required":true,"negate":false,"condition":{"kind":"language","value":1,"except_language":false}},{"name":"Freeleech","required":true,"negate":false,"condition":{"kind":"indexer_flag","value":1}}]);
    for (id, domain) in [(1, "tv"), (2, "movies")] {
        c.execute("INSERT INTO custom_formats(id,media_type,name,include_when_renaming,specification_version,specifications_json)VALUES(?,?,'Provider facts',0,1,?)",libsql::params![id,domain,cf.to_string()]).await.unwrap();
        c.execute(
            "INSERT INTO quality_profile_format_scores VALUES(?,?,?,10)",
            libsql::params![id, id, domain],
        )
        .await
        .unwrap();
    }
    c.execute_batch("UPDATE quality_profile_policies SET min_format_score=10;")
        .await
        .unwrap();

    // Fresh-install naming defaults must be disabled with every format unset; a naming
    // config that is *configured but disabled* must not perturb any basename assertion
    // below, since `rename_enabled` is the only automated-path gate that matters.
    let (code, tv_defaults) = request(&base, "GET", "/api/v1/tv/config/naming", Value::Null).await;
    assert_eq!(code, 200, "{tv_defaults}");
    assert_eq!(tv_defaults["rename_enabled"], false);
    assert_eq!(tv_defaults["revision"], 1);
    assert!(tv_defaults["standard_episode_format"].is_null());
    let (code, movie_defaults) =
        request(&base, "GET", "/api/v1/movies/config/naming", Value::Null).await;
    assert_eq!(code, 200, "{movie_defaults}");
    assert_eq!(movie_defaults["rename_enabled"], false);
    assert_eq!(movie_defaults["revision"], 1);
    assert!(movie_defaults["standard_movie_format"].is_null());
    let(code,v)=request(&base,"PUT","/api/v1/tv/config/naming",json!({"revision":1,"rename_enabled":false,"replace_illegal_characters":true,"colon_replacement":"smart","custom_colon_replacement":null,"standard_episode_format":"DISABLED-{Series Title}","daily_episode_format":null,"anime_episode_format":null,"series_folder_format":null,"season_folder_format":null,"specials_folder_format":null,"multi_episode_style":null})).await;
    assert_eq!(code, 200, "{v}");
    let(code,v)=request(&base,"PUT","/api/v1/movies/config/naming",json!({"revision":1,"rename_enabled":false,"replace_illegal_characters":true,"colon_replacement":"smart","custom_colon_replacement":null,"standard_movie_format":"DISABLED-{Movie Title}","movie_folder_format":null})).await;
    assert_eq!(code, 200, "{v}");
    let (indexer, download) = providers(&base, &origin).await;
    let mut roots = Vec::new();
    let mut sources = Vec::new();
    for (media, profile) in [("tv", 1), ("movies", 2)] {
        let root = scratch.0.join(media);
        let source = scratch.0.join(format!("source-{media}"));
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&source).unwrap();
        let media_source = if media == "tv" {
            let path = source.join("batch");
            std::fs::create_dir(&path).unwrap();
            path
        } else {
            source.clone()
        };
        let name = if media == "tv" {
            "Harbor.S01E01.1080p.WEB-DL.mkv"
        } else {
            "Harbor.2020.1080p.WEB-DL.mkv"
        };
        std::fs::write(
            media_source.join(name),
            vec![if media == "tv" { 1 } else { 2 }; 1048576],
        )
        .unwrap();
        let route = if media == "tv" {
            "/api/v1/tv/series/lookup"
        } else {
            "/api/v1/movies/lookup"
        };
        let input = if media == "tv" {
            json!({"tvdb_id":101,"path":root,"settings":{"quality_profile_id":profile,"series_type":"standard","use_scene_numbering":false,"monitored":true}})
        } else {
            json!({"tmdb_id":201,"path":root,"settings":{"quality_profile_id":profile,"minimum_availability":"released","monitored":true}})
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
        sources.push(media_source);
    }

    phase(if search {
        "search_variant_setup"
    } else {
        "rss_variant_setup"
    });
    // Finish both client downloads in the owned mock; only scheduled observation can admit imports.
    state.completed.store(1, Ordering::SeqCst);
    let mut commands_pending = Vec::new();
    for media in ["tv", "movies"] {
        let command = if search {
            let (code,v)=request(&base,"POST","/api/v1/search/commands",json!({"request_id":uuid::Uuid::new_v4(),"mode":"automatic","target":{"media_type":if media=="tv"{"episode"}else{"movie"},"id":1},"indexer_id":indexer["id"],"indexer_revision":indexer["revision"],"client_id":download["id"],"client_revision":download["revision"],"priority":"high"})).await;
            assert_eq!(code, 202, "{v}");
            v
        } else {
            enqueue(&base, target(&indexer, &download, media)).await
        };
        commands_pending.push(command);
    }
    phase("command_worker_start_and_enqueue_drain");
    let runtime = commands::start_with_metadata(db.clone(), client.clone(), metadata.clone())
        .await
        .unwrap();
    for command in &commands_pending {
        wait_status(
            &base,
            &format!(
                "/api/v1/{}/commands/{}",
                if search { "search" } else { "rss" },
                command["id"].as_str().unwrap()
            ),
            "succeeded",
        )
        .await;
    }
    // The real inherited60s interval is retained; no deadline/row injection or manual refresh.
    phase("initial_scheduled_import");
    poll_until(
        "default scheduled imports must finish",
        IMPORT_BUDGET,
        || async {
            let (_, v) = request(&base, "GET", "/api/v1/download-processing", Value::Null).await;
            if let Some(rows) = v["items"].as_array() {
                assert!(!rows.iter().any(|r| r["status"] == "blocked"), "{v}");
                if rows.len() == 2 && rows.iter().all(|r| r["status"] == "imported") {
                    return Ok(());
                }
            }
            Err(format!(
                "{}; commands {}",
                processing_state(&v),
                command_states(&db).await
            ))
        },
    )
    .await;
    phase("post_import_assertions");
    for (i, name) in [
        "Harbor.S01E01.1080p.WEB-DL.mkv",
        "Harbor.2020.1080p.WEB-DL.mkv",
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(
            std::fs::read(roots[i].join(name)).unwrap(),
            std::fs::read(sources[i].join(name)).unwrap(),
            "copy default retains original bytes"
        );
    }
    let (code, episode) = request(&base, "GET", "/api/v1/episodes/1", Value::Null).await;
    assert_eq!(code, 200, "{episode}");
    assert_eq!(episode["series_id"], 1);
    assert_eq!(episode["has_file"], true);
    assert_eq!(
        episode["file_path"],
        roots[0]
            .join("Harbor.S01E01.1080p.WEB-DL.mkv")
            .to_str()
            .unwrap()
    );
    let (code, tvfile) = request(
        &base,
        "GET",
        &format!(
            "/api/v1/tv/files/{}",
            episode["episode_file_id"].as_i64().unwrap()
        ),
        Value::Null,
    )
    .await;
    assert_eq!(code, 200, "{tvfile}");
    assert_eq!(tvfile["series_id"], 1);
    assert_eq!(tvfile["path"], episode["file_path"]);
    let (code, movies) = request(
        &base,
        "GET",
        "/api/v1/movies/files?movie_ids=1",
        Value::Null,
    )
    .await;
    assert_eq!(code, 200, "{movies}");
    assert_eq!(movies["total"], 1);
    assert_eq!(movies["items"][0]["movie_id"], 1);
    assert_eq!(
        movies["items"][0]["path"],
        roots[1]
            .join("Harbor.2020.1080p.WEB-DL.mkv")
            .to_str()
            .unwrap()
    );
    let (_, before_imports) =
        request(&base, "GET", "/api/v1/download-processing", Value::Null).await;
    assert_eq!(state.adds.lock().unwrap().len(), 2);
    assert_eq!(c.query("SELECT count(*) FROM download_processing_policies WHERE enabled=1 AND enabled_override IS NULL",()).await.unwrap().next().await.unwrap().unwrap().get::<i64>(0).unwrap(),2);
    assert_eq!(c.query("SELECT count(*) FROM download_refresh_schedules WHERE intent='inherited' AND interval_seconds=60 AND last_run_at IS NOT NULL",()).await.unwrap().next().await.unwrap().unwrap().get::<i64>(0).unwrap(),2);
    phase("first_worker_shutdown");
    tokio::time::timeout(TEARDOWN_BUDGET, runtime.shutdown())
        .await
        .unwrap_or_else(|_| panic!("first command worker did not join; {}", phase_report()));
    // Freeze the baseline after the old worker has joined; its final refresh cannot satisfy restart evidence.
    let mut prior_observations = Vec::new();
    for media in ["tv", "movies"] {
        let (code, value) = request(
            &base,
            "GET",
            &format!(
                "/api/v1/queue?provider_id={}&media_type={media}",
                download["id"].as_str().unwrap()
            ),
            Value::Null,
        )
        .await;
        assert_eq!(code, 200, "{value}");
        prior_observations.push(value["command_id"].clone());
    }
    let restarted = commands::start_with_metadata(db.clone(), client, metadata)
        .await
        .unwrap();
    // Require new successful periodic observations in both domains after restart, not just startup.
    phase("restart_periodic_observation");
    poll_until(
        "both inherited schedules must actually refresh after restart",
        RESTART_BUDGET,
        || async {
            let mut settled = true;
            let mut seen = Vec::new();
            for (i, media) in ["tv", "movies"].iter().enumerate() {
                let (code, value) = request(
                    &base,
                    "GET",
                    &format!(
                        "/api/v1/queue?provider_id={}&media_type={media}",
                        download["id"].as_str().unwrap()
                    ),
                    Value::Null,
                )
                .await;
                assert_eq!(code, 200, "{value}");
                let advanced = value["command_id"] != prior_observations[i];
                seen.push(format!("{media}_advanced={advanced}"));
                settled &= advanced;
            }
            if settled {
                Ok(())
            } else {
                Err(format!(
                    "{}; commands {}",
                    seen.join(","),
                    command_states(&db).await
                ))
            }
        },
    )
    .await;
    phase("post_restart_assertions");
    let (_, after_imports) =
        request(&base, "GET", "/api/v1/download-processing", Value::Null).await;
    for old in before_imports["items"].as_array().unwrap() {
        let new = after_imports["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["receipt_id"] == old["receipt_id"])
            .unwrap();
        assert_eq!(new["operation_id"], old["operation_id"]);
        assert_eq!(new["status"], "imported");
    }
    assert_eq!(
        state.adds.lock().unwrap().len(),
        2,
        "restart observations must not resubmit downloads"
    );
    assert_eq!(
        c.query("SELECT count(*) FROM import_history", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<i64>(0)
            .unwrap(),
        2
    );
    phase("final_shutdown");
    tokio::time::timeout(TEARDOWN_BUDGET, async {
        restarted.shutdown().await;
        _api.stop().await;
        _upstream.stop().await;
    })
    .await
    .unwrap_or_else(|_| panic!("final shutdown did not complete; {}", phase_report()));
}
