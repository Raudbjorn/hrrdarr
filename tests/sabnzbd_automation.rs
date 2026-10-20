//! HTTP + worker end-to-end flow through an owned Newznab + SABnzbd mock on 127.0.0.1:0:
//! release → grab → addurl → queue/history refresh → completed handoff → import, for one TV
//! episode and one movie that share native ID 1. No live services or real media are used.
use axum::{
    extract::{OriginalUri, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use hrrdarr::{commands, db::Database, providers};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::Duration,
};

const KEY: &str = "fedcba9876543210fedcba9876543210";
const FILE_BYTES: usize = 1_048_576;

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
#[derive(Default)]
struct Remote {
    origin: Mutex<String>,
    /// Local directory that the `/remote` SABnzbd root maps to, per category.
    local_roots: Mutex<HashMap<String, PathBuf>>,
    queue: Mutex<Vec<Value>>,
    history: Mutex<Vec<Value>>,
    adds: Mutex<Vec<HashMap<String, String>>>,
    lose_add_response: AtomicBool,
    next_id: AtomicU32,
}
fn caps() -> &'static str {
    r#"<caps><limits max="100" default="100"/><searching><search available="yes" supportedParams="q"/><tv-search available="yes" supportedParams="q,tvdbid,season,ep"/><movie-search available="yes" supportedParams="q,tmdbid"/></searching><categories><category id="5000"><subcat id="5030"/></category><category id="2000"><subcat id="2030"/></category></categories></caps>"#
}
fn title(tv: bool) -> &'static str {
    if tv {
        "Harbor.S01E01.1080p.WEB-DL"
    } else {
        "Harbor.2020.1080p.WEB-DL"
    }
}
fn matches(slot: &Value, q: &HashMap<String, String>, category_key: &str, name_key: &str) -> bool {
    q.get("cat").is_none_or(|c| {
        slot[category_key]
            .as_str()
            .is_some_and(|v| v.eq_ignore_ascii_case(c))
    }) && q.get("search").is_none_or(|s| {
        slot[name_key]
            .as_str()
            .is_some_and(|n| n.to_lowercase().contains(&s.to_lowercase()))
    }) && q
        .get("nzo_ids")
        .is_none_or(|ids| ids.split(',').any(|id| slot["nzo_id"] == id))
}
fn sabnzbd(s: &Remote, q: &HashMap<String, String>) -> Response {
    let mode = q.get("mode").map(String::as_str).unwrap_or("");
    if mode == "version" {
        return axum::Json(json!({"version":"4.5.1"})).into_response();
    }
    if q.get("apikey").map(String::as_str) != Some(KEY) {
        return "error: API Key Incorrect\n".into_response();
    }
    match mode {
        "get_cats" => axum::Json(json!({"categories":["*","movies","tv"]})).into_response(),
        "get_config" => axum::Json(json!({"config":{"misc":{"complete_dir":"/remote","history_retention_option":"all","enable_tv_sorting":0,"enable_movie_sorting":0,"enable_date_sorting":0},"categories":[{"name":"*","dir":""},{"name":"tv","dir":"tv"},{"name":"movies","dir":"movies"}],"sorters":[]}})).into_response(),
        "queue" => {
            // A general scoped read (not a submission postcondition or marker lookup) finds the
            // job finished: SABnzbd moves it from the queue into history as Completed.
            if q.get("nzo_ids").is_none() && q.get("search").is_none() {
                let mut queue = s.queue.lock().unwrap();
                let category = q.get("cat").cloned().unwrap_or_default();
                let (done, kept): (Vec<_>, Vec<_>) = queue.drain(..).partition(|slot| slot["cat"] == category.as_str());
                *queue = kept;
                for slot in done {
                    let name = slot["filename"].as_str().unwrap();
                    s.history.lock().unwrap().push(json!({"fail_message":"","loaded":false,"size":"1 MB","category":category,"pp":"D","retry":0,"script":"None","nzb_name":format!("{name}.nzb"),"download_time":1,"storage":format!("/remote/{category}/{name}"),"status":"Completed","completed":1700000000,"nzo_id":slot["nzo_id"],"downloaded":FILE_BYTES,"report":"","password":"","path":"/incomplete/x","postproc_time":1,"name":name,"url":"","md5sum":"","archive":false,"bytes":FILE_BYTES,"url_info":"","stage_log":[]}));
                }
            }
            let slots: Vec<_> = s.queue.lock().unwrap().iter().filter(|v| matches(v, q, "cat", "filename")).cloned().collect();
            axum::Json(json!({"queue":{"status":"Downloading","paused":false,"noofslots":slots.len(),"slots":slots}})).into_response()
        }
        "history" => {
            let slots: Vec<_> = s.history.lock().unwrap().iter().filter(|v| matches(v, q, "category", "name")).cloned().collect();
            axum::Json(json!({"history":{"noofslots":slots.len(),"slots":slots}})).into_response()
        }
        "addurl" => {
            assert!(q["name"].contains("PRIVATE_RSS_LOCATOR"), "the release locator is passed through");
            let category = q["cat"].clone();
            let name = q["nzbname"].clone();
            let (stem, _) = name.rsplit_once(".hrrdarr-").expect("job name carries the marker");
            assert_eq!(stem, title(category == "tv"));
            // The client "downloads" into its category folder, which the remote path mapping exposes locally.
            let folder = s.local_roots.lock().unwrap()[&category].join(&category).join(&name);
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(folder.join(format!("{stem}.mkv")), vec![if category == "tv" { 1 } else { 2 }; FILE_BYTES]).unwrap();
            let nzo = format!("SABnzbd_nzo_{:06}", s.next_id.fetch_add(1, Ordering::SeqCst) + 1);
            s.queue.lock().unwrap().push(json!({"status":"Downloading","index":0,"password":"","avg_age":"1d","script":"None","mb":"1.00","mbleft":"0.50","mbmissing":"0.0","size":"1 MB","sizeleft":"0.5 MB","filename":name,"labels":[],"priority":"Normal","cat":category,"timeleft":"0:00:05","percentage":"50","nzo_id":nzo,"unpackopts":"3"}));
            s.adds.lock().unwrap().push(q.clone());
            if s.lose_add_response.load(Ordering::SeqCst) {
                return StatusCode::BAD_GATEWAY.into_response();
            }
            axum::Json(json!({"status":true,"nzo_ids":[nzo]})).into_response()
        }
        _ => axum::Json(json!({"status":false,"error":"unexpected mode"})).into_response(),
    }
}
async fn remote(State(s): State<Arc<Remote>>, OriginalUri(uri): OriginalUri) -> Response {
    let q: HashMap<String, String> =
        url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();
    match uri.path() {
        "/shows/en/101" => axum::Json(json!({"tvdbId":101,"title":"Harbor","seasons":[{"seasonNumber":1}],"episodes":[{"tvdbId":501,"seasonNumber":1,"episodeNumber":1,"title":"Pilot","runtime":45,"airDate":"2020-01-01","airDateUtc":"2020-01-01T00:00:00Z","absoluteEpisodeNumber":1}]})).into_response(),
        "/movie/201" => axum::Json(json!({"tmdbId":201,"title":"Harbor","year":2020,"runtime":100,"digitalRelease":"2020-01-01T00:00:00Z"})).into_response(),
        "/sabnzbd/api" => {
            assert_eq!(q.get("output").map(String::as_str), Some("json"));
            sabnzbd(&s, &q)
        }
        // Newznab endpoints are addressed at their configured base (here the origin root).
        "/" | "/api" => {
            if q.get("t").is_some_and(|t| t == "caps") {
                return caps().into_response();
            }
            let tv = q.get("cat").is_some_and(|v| v.contains("5030"));
            let (title, category, id) = if tv { (title(true), 5030, 1) } else { (title(false), 2030, 2) };
            let link = format!("{}/getnzb/{id}.nzb?apikey=PRIVATE_RSS_LOCATOR", s.origin.lock().unwrap());
            let link = link.replace('&', "&amp;");
            format!(r#"<rss version="2.0" xmlns:newznab="http://www.newznab.com/DTD/2010/feeds/attributes/"><channel><item><title>{title}</title><guid isPermaLink="false">PRIVATE_RSS_GUID-{id}</guid><pubDate>Mon, 01 Jan 2024 12:00:00 +0000</pubDate><link>{link}</link><enclosure url="{link}" length="1073741824" type="application/x-nzb"/><newznab:attr name="category" value="{category}"/><newznab:attr name="size" value="1073741824"/></item></channel></rss>"#).into_response()
        }
        _ => (StatusCode::NOT_FOUND, "unexpected owned mock request").into_response(),
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
    for private in ["PRIVATE_RSS_GUID", "PRIVATE_RSS_LOCATOR", KEY] {
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
    sab: Value,
    qbit: Value,
    roots: Vec<PathBuf>,
    _servers: (Server, Server),
}
async fn app(label: &str) -> App {
    let scratch =
        Scratch(std::env::temp_dir().join(format!("hrrdarr-sab-{label}-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&scratch.0).unwrap();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let state = Arc::new(Remote::default());
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
    let key = Arc::new(providers::CredentialKey::from_hex(&"22".repeat(32)).unwrap());
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
            json!({"implementation":"newznab","endpoint":origin,"tv":{"categories":[5030],"anime_categories":[]},"movies":{"categories":[2030]}}),
            Value::Null,
        ),
        // Distinct per-domain categories and priorities on one shared client.
        (
            json!({"implementation":"sabnzbd","endpoint":format!("{origin}/sabnzbd"),"tv":{"category":"tv","recent_priority":"high","older_priority":"low"},"movies":{"category":"movies","recent_priority":"force","older_priority":"normal"}}),
            json!({"kind":"api_key","api_key":KEY}),
        ),
        (
            json!({"implementation":"qbittorrent","endpoint":origin,"tv":{"category":"tv","imported_category":null,"recent_priority":0,"older_priority":0},"movies":{"category":"movies","imported_category":null,"recent_priority":0,"older_priority":0}}),
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
            .insert(media.into(), source.clone());
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
    let sab = created.remove(1);
    let qbit = created.remove(1);
    App {
        _scratch: scratch,
        db,
        base,
        state,
        client,
        metadata,
        indexer: created.remove(0),
        sab,
        qbit,
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
async fn newznab_to_sabnzbd_grab_refresh_and_import_for_both_domains() {
    let mut app = app("flow").await;
    let base = &app.base.clone();
    // The SABnzbd connection test validates key, version and both domain categories.
    let (code, tested) = request(
        base,
        "POST",
        &format!("/api/v1/providers/{}/test", app.sab["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_eq!(code, 200, "{tested}");
    assert_eq!(tested["result"]["sabnzbd_version"], "4.5.1");
    assert_eq!(tested["result"]["domains"], json!(["tv", "movies"]));

    // A Newznab release can never be sent to a torrent client: explicit choices are rejected visibly.
    for media in ["tv", "movies"] {
        let (code, v) = enqueue(base, target(&app.indexer, &app.qbit, media)).await;
        assert_eq!(
            (code, v["error"]["code"].as_str()),
            (409, Some("protocol_mismatch")),
            "{v}"
        );
    }
    let (code, v) = request(base, "POST", "/api/v1/search/commands", json!({"request_id":uuid::Uuid::new_v4(),"mode":"automatic","target":{"media_type":"movie","id":1},"indexer_id":app.indexer["id"],"indexer_revision":app.indexer["revision"],"client_id":app.qbit["id"],"client_revision":app.qbit["revision"],"priority":"high"})).await;
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (409, Some("protocol_mismatch")),
        "{v}"
    );
    // Bind both domains to the usenet client; cross-protocol preferences are covered below.
    let path = format!("/api/v1/providers/{}", app.indexer["id"].as_str().unwrap());
    let (code, indexer) = request(base, "GET", &path, Value::Null).await;
    assert_eq!(code, 200);
    let mut bound = json!({"revision":indexer["revision"],"name":indexer["name"],"enabled":true,"priority":1,"settings":indexer["settings"]});
    bound["settings"]["tv"]["download_client_id"] = app.sab["id"].clone();
    bound["settings"]["movies"]["download_client_id"] = app.sab["id"].clone();
    let (code, v) = request(base, "PUT", &path, bound).await;
    assert_eq!(code, 200, "{v}");
    app.indexer = v;

    let mut pending = Vec::new();
    for media in ["tv", "movies"] {
        let (code, v) = enqueue(base, target(&app.indexer, &app.sab, media)).await;
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
    .expect("scheduled SABnzbd observation must hand both completed jobs to import");

    let adds = app.state.adds.lock().unwrap().clone();
    assert_eq!(adds.len(), 2);
    let by_cat = |c: &str| adds.iter().find(|a| a["cat"] == c).unwrap().clone();
    // Both targets aired/released long ago, so each domain's older priority applies.
    assert_eq!(by_cat("tv")["priority"], "-1");
    assert_eq!(by_cat("movies")["priority"], "0");
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
            "each domain imports only its own job"
        );
    }
    let (code, episode) = request(base, "GET", "/api/v1/episodes/1", Value::Null).await;
    assert_eq!(code, 200, "{episode}");
    assert_eq!(episode["has_file"], true);
    assert_eq!(
        episode["file_path"],
        app.roots[0]
            .join("Harbor.S01E01.1080p.WEB-DL.mkv")
            .to_str()
            .unwrap()
    );
    let (code, movies) =
        request(base, "GET", "/api/v1/movies/files?movie_ids=1", Value::Null).await;
    assert_eq!(code, 200, "{movies}");
    assert_eq!(movies["total"], 1);
    assert_eq!(
        movies["items"][0]["path"],
        app.roots[1]
            .join("Harbor.2020.1080p.WEB-DL.mkv")
            .to_str()
            .unwrap()
    );
    // Queue projection for each domain shows only its own category, by remote identity.
    for media in ["tv", "movies"] {
        let (code, page) = request(
            base,
            "GET",
            &format!(
                "/api/v1/queue?provider_id={}&media_type={media}",
                app.sab["id"].as_str().unwrap()
            ),
            Value::Null,
        )
        .await;
        assert_eq!(code, 200, "{page}");
        // The reconciliation marker names the on-disk job folder (see original_file_path) but is
        // never projected as the job name.
        assert!(!page.to_string().contains(".hrrdarr-"), "{page}");
        let items = page["items"].as_array().unwrap();
        assert_eq!(items.len(), 1, "{page}");
        let download = &items[0]["download"];
        assert_eq!(
            (
                download["category"].as_str(),
                download["status"].as_str(),
                download["completed"].as_bool()
            ),
            (Some(media), Some("completed"), Some(true)),
            "{page}"
        );
        assert_eq!(
            download["name"],
            if media == "tv" {
                "Harbor.S01E01.1080p.WEB-DL"
            } else {
                "Harbor.2020.1080p.WEB-DL"
            }
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

    // Restarting the worker observes again without resubmitting either job.
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
    assert_eq!(
        count(&app.db, "SELECT count(*) FROM import_history").await,
        2
    );
}

#[tokio::test]
async fn lost_addurl_response_is_reconciled_without_a_second_submission() {
    let app = app("lost").await;
    let base = &app.base;
    app.state.lose_add_response.store(true, Ordering::SeqCst);
    let (code, command) = enqueue(base, target(&app.indexer, &app.sab, "tv")).await;
    assert_eq!(code, 202, "{command}");
    let runtime =
        commands::start_with_metadata(app.db.clone(), app.client.clone(), app.metadata.clone())
            .await
            .unwrap();
    // Reconciliation runs after the bounded uncertainty delay (about 30 s), never immediately.
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
    .await;
    let receipt = match receipt {
        Ok(receipt) => receipt,
        Err(_) => {
            let (_, v) = request(base, "GET", "/api/v1/rss/candidates", Value::Null).await;
            let (_, c) = request(
                base,
                "GET",
                &format!("/api/v1/rss/commands/{}", command["id"].as_str().unwrap()),
                Value::Null,
            )
            .await;
            panic!(
                "an unconfirmed add must surface for attention: {v} {c} adds={}",
                app.state.adds.lock().unwrap().len()
            )
        }
    };
    // The job was found by its marker, but an uncertain add is never silently promoted.
    assert_eq!(receipt["error_code"], "presence_unconfirmed");
    assert_eq!(receipt["target"]["media_type"], "tv");
    assert_eq!(app.state.adds.lock().unwrap().len(), 1);
    // A later RSS pass over the same release does not submit it again.
    app.state.lose_add_response.store(false, Ordering::SeqCst);
    let (code, again) = enqueue(base, target(&app.indexer, &app.sab, "tv")).await;
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
async fn provider_api_validates_and_tests_sabnzbd_configuration() {
    let app = app("config").await;
    let base = &app.base;
    let origin = app.state.origin.lock().unwrap().clone();
    let create = |settings: Value, credentials: Value| {
        let mut body = json!({"name":"sab","enabled":true,"priority":1,"settings":settings});
        if !credentials.is_null() {
            body["credentials"] = credentials;
        }
        async move { request(base, "POST", "/api/v1/providers", body).await }
    };
    let sab = |tv: &str, movies: &str| json!({"implementation":"sabnzbd","endpoint":format!("{origin}/sabnzbd"),"tv":{"category":tv},"movies":{"category":movies}});
    // Shared categories would let one domain import the other's downloads.
    let (code, v) = create(sab("tv", "TV"), Value::Null).await;
    assert_eq!(code, 400, "{v}");
    // SABnzbd authenticates by API key only.
    let (code, v) = create(
        sab("tv", "movies"),
        json!({"kind":"username_password","username":"u","password":"p"}),
    )
    .await;
    assert_eq!(code, 400, "{v}");
    // Out-of-range priority strings are rejected by the closed enum.
    let (code, v) = create(json!({"implementation":"sabnzbd","endpoint":format!("{origin}/sabnzbd"),"tv":{"category":"tv","recent_priority":"urgent"},"movies":null}), Value::Null).await;
    assert_eq!(code, 400, "{v}");

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
        sab("tv", "movies"),
        json!({"kind":"api_key","api_key":"not-the-key"}),
    )
    .await;
    assert_eq!(code, 201, "{wrong}");
    let (code, v) = test(wrong["id"].clone()).await;
    // Same contract as other providers: the upstream rejected authentication (502, code "authentication").
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (502, Some("authentication")),
        "{v}"
    );
    assert!(!v.to_string().contains("not-the-key"));
    let (_, saved) = request(
        base,
        "GET",
        &format!("/api/v1/providers/{}", wrong["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_eq!(saved["test_status"], "failure", "{saved}");
    assert_eq!(
        saved["last_test"]["error_code"], "authentication",
        "{saved}"
    );
    let bad_key_code = saved["last_test"]["error_code"].clone();

    let (code, missing) = create(
        sab("anime", "movies"),
        json!({"kind":"api_key","api_key":KEY}),
    )
    .await;
    assert_eq!(code, 201, "{missing}");
    let (code, v) = test(missing["id"].clone()).await;
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (422, Some("sabnzbd_category_missing")),
        "{v}"
    );
    let (_, saved) = request(
        base,
        "GET",
        &format!("/api/v1/providers/{}", missing["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_ne!(
        saved["last_test"]["error_code"], bad_key_code,
        "bad key and missing category are distinguishable"
    );
    // Unsaved drafts use the same SABnzbd probe without persisting anything.
    let (code, v) = request(base, "POST", "/api/v1/providers/test-draft", json!({"source":null,"config":{"name":"draft","enabled":false,"priority":1,"settings":sab("tv","movies"),"credentials":{"kind":"api_key","api_key":KEY}}})).await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["result"]["sabnzbd_version"], "4.5.1");
    assert_eq!(v["result"]["domains"], json!(["tv", "movies"]));
}

#[tokio::test]
async fn stored_cross_protocol_binding_is_rejected_at_dispatch_without_submission() {
    let app = app("binding").await;
    let base = &app.base;
    // A Newznab preference for the torrent client is stored, not erased (like a disabled client).
    let path = format!("/api/v1/providers/{}", app.indexer["id"].as_str().unwrap());
    let mut bound = json!({"revision":app.indexer["revision"],"name":app.indexer["name"],"enabled":true,"priority":1,"settings":app.indexer["settings"]});
    bound["settings"]["tv"]["download_client_id"] = app.qbit["id"].clone();
    let (code, indexer) = request(base, "PUT", &path, bound).await;
    assert_eq!(code, 200, "{indexer}");
    assert_eq!(
        indexer["settings"]["tv"]["download_client_id"],
        app.qbit["id"]
    );
    // Default selection honours the preference only if it can dispatch; here it cannot.
    let (code, command) = request(base, "POST", "/api/v1/rss/commands", json!({"target":{"media_type":"tv","indexer_id":indexer["id"],"indexer_revision":indexer["revision"],"client_selection":{"kind":"default"}},"priority":"high"})).await;
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
    .await;
    let Ok(receipt) = receipt else {
        let (_, v) = request(base, "GET", "/api/v1/rss/candidates", Value::Null).await;
        let (_, c) = request(
            base,
            "GET",
            &format!("/api/v1/rss/commands/{}", command["id"].as_str().unwrap()),
            Value::Null,
        )
        .await;
        panic!("the cross-protocol preference must be a visible rejection: {v} {c}");
    };
    // Durable candidate codes are a closed vocabulary: the mismatch is stored as unsupported_target.
    assert_eq!(receipt["status"], "rejected", "{receipt}");
    assert_eq!(receipt["error_code"], "unsupported_target", "{receipt}");
    runtime.shutdown().await;
    assert!(
        app.state.adds.lock().unwrap().is_empty(),
        "nothing is sent to either client"
    );
}
