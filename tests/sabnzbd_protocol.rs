//! SABnzbd wire contract against an owned mock on 127.0.0.1:0. Response shapes for `queue`,
//! `history`, `addurl`, `get_cats` and API-key errors are recorded from the public SABnzbd API
//! reference (sabnzbd.org/wiki/configuration/4.5/api); `get_config` uses its documented
//! section/key names. No live client is contacted.
use axum::{Router, extract::State, http::StatusCode, http::Uri, response::IntoResponse};
use hrrdarr::{
    api::MediaDomain,
    db::MediaTarget,
    providers::{
        Credentials, ProviderSettings, UsenetPriority, UsenetScope,
        http::{HttpClient, HttpError},
        qbittorrent::{
            AddOutcome, DownloadDiagnostic, DownloadQuery, DownloadStatus, QbitError,
            Reconciliation,
        },
        sabnzbd::{self, SabnzbdProblem, SabnzbdTestWarning},
    },
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

const KEY: &str = "0123456789abcdef0123456789abcdef";
const NZB_URL: &str = "https://indexer.invalid/getnzb/abc.nzb?apikey=PRIVATE_INDEXER_KEY";

/// Recorded `mode=queue` sample (two slots) from the API reference, trimmed of display-only keys.
const QUEUE_SAMPLE: &str = r#"{"queue":{"status":"Downloading","speedlimit":"9","paused":false,"noofslots_total":2,"noofslots":2,"limit":10,"start":0,"timeleft":"0:16:44","speed":"1.3 M","kbpersec":"1296.02","size":"1.2 GB","sizeleft":"1.2 GB","mb":"1277.65","mbleft":"1271.58","slots":[{"status":"Downloading","index":0,"password":"","avg_age":"2895d","time_added":1469172000,"script":"None","direct_unpack":"10/30","mb":"1277.65","mbleft":"1271.59","mbmissing":"0.0","size":"1.2 GB","sizeleft":"1.2 GB","filename":"TV.Show.S04E11.720p.HDTV.x264","labels":[],"priority":"Normal","cat":"tv","timeleft":"0:16:44","percentage":"0","nzo_id":"SABnzbd_nzo_p86tgx","unpackopts":"3"},{"status":"Paused","index":1,"password":"","avg_age":"2895d","time_added":1469171000,"script":"None","direct_unpack":null,"mb":"1277.76","mbleft":"1277.76","mbmissing":"0.0","size":"1.2 GB","sizeleft":"1.2 GB","filename":"TV.Show.S04E12.720p.HDTV.x264","labels":["TOO LARGE","DUPLICATE"],"priority":"Normal","cat":"tv","timeleft":"0:00:00","percentage":"0","nzo_id":"SABnzbd_nzo_ksfai6","unpackopts":"3"}],"version":"4.5.1","finish":2,"paused_all":false,"have_quota":false}}"#;
/// Recorded `mode=history` completed slot from the API reference (Windows storage path).
const HISTORY_SAMPLE: &str = r#"{"history":{"noofslots":1,"ppslots":0,"day_size":"1.9 G","week_size":"30.4 G","month_size":"167.3 G","total_size":"678.1 G","last_history_update":1469210913,"slots":[{"action_line":"","duplicate_key":"TV.Show/4/2","meta":null,"fail_message":"","loaded":false,"size":"2.3 GB","category":"tv","pp":"D","retry":0,"script":"None","nzb_name":"TV.Show.S04E02.720p.BluRay.x264-xHD.nzb","download_time":64,"storage":"C:\\Users\\xxx\\Videos\\Complete\\TV.Show.S04E02.720p.BluRay.x264-xHD","has_rating":false,"status":"Completed","script_line":"","completed":1469172988,"time_added":1469172000,"nzo_id":"SABnzbd_nzo_sdkoun","downloaded":2436906376,"report":"","password":"","path":"\\\\?\\C:\\SABnzbd\\TV.Show.S04E02.720p.BluRay.x264-xHD","postproc_time":40,"name":"TV.Show.S04E02.720p.BluRay.x264-xHD","url":"TV.Show.S04E02.720p.BluRay.x264-xHD.nzb","md5sum":"d2c16aeecbc1b1921d04422850e93013","archive":false,"bytes":2436906376,"url_info":"","stage_log":[]}]}}"#;

struct Mock {
    version: String,
    cats: Vec<String>,
    config: Value,
    queue: Vec<Value>,
    history: Vec<Value>,
    paused: bool,
    calls: Vec<HashMap<String, String>>,
    lose_add_response: bool,
    ignore_cat: bool,
    next_id: u32,
}
#[derive(Clone)]
struct Fixture(Arc<Mutex<Mock>>);
impl Fixture {
    fn adds(&self) -> Vec<HashMap<String, String>> {
        let m = self.0.lock().unwrap();
        m.calls
            .iter()
            .filter(|c| c["mode"] == "addurl")
            .cloned()
            .collect()
    }
    fn modes(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .calls
            .iter()
            .map(|c| c["mode"].clone())
            .collect()
    }
}
fn config() -> Value {
    json!({"misc":{"complete_dir":"/data/complete","download_dir":"/data/incomplete","history_retention_option":"all","history_retention_number":1,"enable_tv_sorting":0,"tv_categories":["tv"],"enable_movie_sorting":0,"movie_categories":["movies"],"enable_date_sorting":0,"date_categories":[]},
        "categories":[{"name":"*","order":0,"pp":"3","script":"None","dir":"","priority":0},{"name":"tv","order":1,"pp":"","script":"Default","dir":"tv","priority":-100},{"name":"movies","order":2,"pp":"","script":"Default","dir":"movies","priority":-100}],
        "sorters":[]})
}
fn queue_slot(nzo: &str, name: &str, cat: &str, status: &str) -> Value {
    json!({"status":status,"index":0,"password":"","avg_age":"1d","script":"None","direct_unpack":null,"mb":"100.00","mbleft":"25.00","mbmissing":"0.0","size":"100 MB","sizeleft":"25 MB","filename":name,"labels":[],"priority":"Normal","cat":cat,"timeleft":"0:01:40","percentage":"75","nzo_id":nzo,"unpackopts":"3"})
}
fn history_slot(
    nzo: &str,
    name: &str,
    cat: &str,
    status: &str,
    storage: &str,
    fail: &str,
) -> Value {
    json!({"action_line":"","fail_message":fail,"loaded":false,"size":"100 MB","category":cat,"pp":"D","retry":0,"script":"None","nzb_name":format!("{name}.nzb"),"download_time":10,"storage":storage,"status":status,"completed":1700000000,"nzo_id":nzo,"downloaded":104857600,"report":"","password":"","path":"/data/incomplete/x","postproc_time":1,"name":name,"url":"","md5sum":"","archive":false,"bytes":104857600,"url_info":"","stage_log":[]})
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
async fn handler(State(f): State<Fixture>, uri: Uri) -> axum::response::Response {
    let q: HashMap<String, String> =
        url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();
    let mut m = f.0.lock().unwrap();
    m.calls.push(q.clone());
    if uri.path() != "/sabnzbd/api" {
        return (StatusCode::NOT_FOUND, "unexpected path").into_response();
    }
    assert_eq!(q.get("output").map(String::as_str), Some("json"));
    let mode = q.get("mode").cloned().unwrap_or_default();
    if mode == "version" {
        return axum::Json(json!({"version": m.version})).into_response();
    }
    match q.get("apikey").map(String::as_str) {
        None => return "error: API Key Required\n".into_response(),
        Some(KEY) => {}
        Some(_) => return "error: API Key Incorrect\n".into_response(),
    }
    let mut q = q;
    if m.ignore_cat {
        q.remove("cat");
    }
    let window = |items: Vec<Value>| -> Vec<Value> {
        let start = q
            .get("start")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0usize);
        let limit = q
            .get("limit")
            .and_then(|s| s.parse().ok())
            .unwrap_or(usize::MAX);
        items.into_iter().skip(start).take(limit).collect()
    };
    match mode.as_str() {
        "get_cats" => axum::Json(json!({"categories": m.cats})).into_response(),
        "get_config" => axum::Json(json!({"config": m.config})).into_response(),
        "fullstatus" => {
            axum::Json(json!({"status":{"completedir":"/data/complete"}})).into_response()
        }
        "queue" => {
            let slots = window(
                m.queue
                    .iter()
                    .filter(|s| matches(s, &q, "cat", "filename"))
                    .cloned()
                    .collect(),
            );
            axum::Json(json!({"queue":{"status":"Downloading","paused":m.paused,"noofslots":slots.len(),"noofslots_total":m.queue.len(),"start":0,"limit":0,"slots":slots,"version":m.version}})).into_response()
        }
        "history" => {
            let slots = window(
                m.history
                    .iter()
                    .filter(|s| matches(s, &q, "category", "name"))
                    .cloned()
                    .collect(),
            );
            axum::Json(json!({"history":{"noofslots":slots.len(),"ppslots":0,"slots":slots}}))
                .into_response()
        }
        "addurl" => {
            assert_eq!(
                q["name"], NZB_URL,
                "the locator is sent only as the addurl name"
            );
            m.next_id += 1;
            let nzo = format!("SABnzbd_nzo_{:06}", m.next_id);
            let slot = queue_slot(&nzo, &q["nzbname"], &q["cat"], "Queued");
            m.queue.push(slot);
            if m.lose_add_response {
                return StatusCode::BAD_GATEWAY.into_response();
            }
            axum::Json(json!({"status":true,"nzo_ids":[nzo]})).into_response()
        }
        _ => axum::Json(json!({"status":false,"error":"not implemented"})).into_response(),
    }
}
async fn fixture() -> (Fixture, String, tokio::task::JoinHandle<()>) {
    let fixture = Fixture(Arc::new(Mutex::new(Mock {
        version: "4.5.1".into(),
        cats: vec!["*".into(), "movies".into(), "tv".into()],
        config: config(),
        queue: vec![],
        history: vec![],
        paused: false,
        calls: vec![],
        lose_add_response: false,
        ignore_cat: false,
        next_id: 0,
    })));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/sabnzbd/", listener.local_addr().unwrap());
    let app = Router::new().fallback(handler).with_state(fixture.clone());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (fixture, endpoint, task)
}
fn settings(endpoint: String) -> ProviderSettings {
    ProviderSettings::Sabnzbd {
        endpoint,
        tv: Some(UsenetScope {
            category: "tv".into(),
            recent_priority: UsenetPriority::High,
            older_priority: UsenetPriority::Low,
            tags: None,
        }),
        movies: Some(UsenetScope {
            category: "movies".into(),
            recent_priority: UsenetPriority::Force,
            older_priority: UsenetPriority::Default,
            tags: None,
        }),
    }
}
fn key(value: &str) -> Credentials {
    Credentials::ApiKey {
        api_key: value.into(),
    }
}
fn query(domain: MediaDomain) -> DownloadQuery {
    DownloadQuery {
        domain,
        offset: 0,
        limit: 100,
        imported: false,
    }
}

#[tokio::test]
async fn test_connection_checks_version_categories_layout_and_key() {
    let (f, url, task) = fixture().await;
    let settings = settings(url);
    let client = HttpClient::new().unwrap();
    let operation = client.operation(Uuid::new_v4()).unwrap();
    let result = sabnzbd::test_connection(&operation, &settings, Some(&key(KEY)))
        .await
        .unwrap();
    assert_eq!(result.sabnzbd_version, "4.5.1");
    assert_eq!(result.domains, vec![MediaDomain::Tv, MediaDomain::Movies]);
    assert!(result.warnings.is_empty());
    // Every authenticated call carried the key; the version probe did not need it.
    assert!(
        f.0.lock()
            .unwrap()
            .calls
            .iter()
            .all(|c| c["mode"] == "version" || c.get("apikey").is_some_and(|k| k == KEY))
    );
    assert!(
        f.0.lock()
            .unwrap()
            .calls
            .iter()
            .any(|c| c["mode"] == "queue" && c["cat"] == "movies")
    );

    // A wrong or absent key is an authentication failure, distinct from other rejections.
    for credentials in [Some(key("wrong-key")), None] {
        let failure = sabnzbd::test_connection(&operation, &settings, credentials.as_ref())
            .await
            .unwrap_err();
        assert_eq!(failure.error, QbitError::Http(HttpError::Authentication));
        assert_eq!(failure.problem, None);
        assert!(!format!("{failure:?}").contains("wrong-key"));
    }
    // Static configuration problems are named so the API can explain them.
    let cases: [(&str, fn(&mut Value), SabnzbdProblem); 4] = [
        (
            "missing",
            |c| {
                c["categories"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|v| v["name"] != "movies")
            },
            SabnzbdProblem::CategoryMissing,
        ),
        (
            "no job folders",
            |c| c["categories"][1]["dir"] = json!("tv*"),
            SabnzbdProblem::JobFoldersDisabled,
        ),
        (
            "legacy sorting",
            |c| c["misc"]["enable_tv_sorting"] = json!(1),
            SabnzbdProblem::SortingEnabled,
        ),
        // An active sorter with an empty category list sorts every category.
        (
            "sorter",
            |c| c["sorters"] = json!([{"name":"all","is_active":1,"sort_cats":[]}]),
            SabnzbdProblem::SortingEnabled,
        ),
    ];
    for (label, mutate, problem) in cases {
        mutate(&mut f.0.lock().unwrap().config);
        let failure = sabnzbd::test_connection(&operation, &settings, Some(&key(KEY)))
            .await
            .unwrap_err();
        assert_eq!(failure.problem, Some(problem), "{label}");
        f.0.lock().unwrap().config = config();
    }
    // Sorting confined to another category does not affect these scopes.
    f.0.lock().unwrap().config["sorters"] =
        json!([{"name":"music","is_active":true,"sort_cats":["music"]}]);
    assert!(
        sabnzbd::test_connection(&operation, &settings, Some(&key(KEY)))
            .await
            .is_ok()
    );
    f.0.lock().unwrap().config = config();

    f.0.lock().unwrap().config["misc"]["history_retention_option"] = json!("number-delete");
    f.0.lock().unwrap().version = "develop".into();
    let result = sabnzbd::test_connection(&operation, &settings, Some(&key(KEY)))
        .await
        .unwrap();
    assert_eq!(
        result.warnings,
        vec![
            SabnzbdTestWarning::DevelopVersion,
            SabnzbdTestWarning::RemovesCompletedDownloads
        ]
    );
    f.0.lock().unwrap().version = "2.3.9".into();
    let failure = sabnzbd::test_connection(&operation, &settings, Some(&key(KEY)))
        .await
        .unwrap_err();
    assert_eq!(failure.error, QbitError::UnsupportedVersion);
    task.abort();
}

#[tokio::test]
async fn recorded_queue_and_history_map_status_per_domain_category() {
    let (f, url, task) = fixture().await;
    let settings = settings(url);
    let client = HttpClient::new().unwrap();
    let operation = client.operation(Uuid::new_v4()).unwrap();
    let credentials = key(KEY);
    {
        let mut m = f.0.lock().unwrap();
        let queue: Value = serde_json::from_str(QUEUE_SAMPLE).unwrap();
        let history: Value = serde_json::from_str(HISTORY_SAMPLE).unwrap();
        m.queue = queue["queue"]["slots"].as_array().unwrap().clone();
        m.history = history["history"]["slots"].as_array().unwrap().clone();
        m.queue.push(queue_slot(
            "SABnzbd_nzo_extract",
            "Show.S01E03",
            "tv",
            "Extracting",
        ));
        m.queue.push(queue_slot(
            "SABnzbd_nzo_movie1",
            "Film.2020.1080p",
            "movies",
            "Downloading",
        ));
        m.history.push(history_slot(
            "SABnzbd_nzo_fail",
            "Show.S01E04",
            "tv",
            "Failed",
            "",
            "Repair failed, not enough repair blocks",
        ));
        m.history.push(history_slot(
            "SABnzbd_nzo_full",
            "Show.S01E05",
            "tv",
            "Failed",
            "",
            "Unpacking failed, write error or disk is full?",
        ));
        m.history.push(history_slot(
            "SABnzbd_nzo_gone",
            "Show.S01E06",
            "tv",
            "Deleted",
            "",
            "",
        ));
        m.history.push(history_slot(
            "SABnzbd_nzo_done",
            "Film.2019.1080p",
            "movies",
            "Completed",
            "/data/complete/movies/Film.2019.1080p",
            "",
        ));
    }
    let tv = sabnzbd::query(
        &operation,
        &settings,
        Some(&credentials),
        &query(MediaDomain::Tv),
    )
    .await
    .unwrap();
    let by = |nzo: &str| {
        tv.items
            .iter()
            .find(|i| i.hash == sabnzbd::remote_id(nzo))
            .unwrap_or_else(|| panic!("{nzo}"))
    };
    assert_eq!(
        tv.items.len(),
        6,
        "deleted history is skipped; movies are not visible in TV"
    );
    assert!(
        tv.items
            .iter()
            .all(|i| i.category == "tv" && i.domain == MediaDomain::Tv)
    );
    let downloading = by("SABnzbd_nzo_p86tgx");
    assert_eq!(downloading.status, DownloadStatus::Downloading);
    assert_eq!(downloading.diagnostic, None);
    assert_eq!(
        downloading.size_bytes,
        (1277.65f64 * 1048576.0).round() as u64
    );
    assert_eq!(downloading.eta_seconds, Some(1004));
    assert_eq!(by("SABnzbd_nzo_ksfai6").status, DownloadStatus::Paused);
    assert_eq!(by("SABnzbd_nzo_ksfai6").eta_seconds, None);
    assert_eq!(
        by("SABnzbd_nzo_extract").diagnostic,
        Some(DownloadDiagnostic::Extracting)
    );
    let done = by("SABnzbd_nzo_sdkoun");
    assert_eq!(done.status, DownloadStatus::Completed);
    assert_eq!((done.completed, done.size_bytes), (true, 2436906376));
    assert_eq!(by("SABnzbd_nzo_fail").status, DownloadStatus::Failed);
    assert_eq!(
        by("SABnzbd_nzo_fail").diagnostic,
        Some(DownloadDiagnostic::Error)
    );
    assert_eq!(by("SABnzbd_nzo_full").status, DownloadStatus::Warning);
    assert_eq!(
        by("SABnzbd_nzo_full").diagnostic,
        Some(DownloadDiagnostic::DiskFull)
    );
    // Reads were category-filtered at the client, not only locally.
    assert!(
        f.0.lock()
            .unwrap()
            .calls
            .iter()
            .filter(|c| c["mode"] == "queue" || c["mode"] == "history")
            .all(|c| c["cat"] == "tv")
    );

    let movies = sabnzbd::query(
        &operation,
        &settings,
        Some(&credentials),
        &query(MediaDomain::Movies),
    )
    .await
    .unwrap();
    assert_eq!(movies.items.len(), 2);
    assert!(
        movies
            .items
            .iter()
            .all(|i| i.domain == MediaDomain::Movies && i.category == "movies")
    );
    // A completed job exposes its final destination as the output; the projected name is clean.
    let job = sabnzbd::job(
        &operation,
        &settings,
        Some(&credentials),
        &MediaTarget::Movie(1),
        &sabnzbd::remote_id("SABnzbd_nzo_done"),
    )
    .await
    .unwrap();
    assert_eq!(
        job.storage.as_deref(),
        Some("/data/complete/movies/Film.2019.1080p")
    );
    // Same-numbered TV target cannot see the movie job.
    assert_eq!(
        sabnzbd::job(
            &operation,
            &settings,
            Some(&credentials),
            &MediaTarget::Episode(1),
            &sabnzbd::remote_id("SABnzbd_nzo_done")
        )
        .await
        .err(),
        Some(QbitError::NotFound)
    );

    // A global pause pauses everything except forced jobs.
    f.0.lock().unwrap().paused = true;
    let tv = sabnzbd::query(
        &operation,
        &settings,
        Some(&credentials),
        &query(MediaDomain::Tv),
    )
    .await
    .unwrap();
    assert_eq!(
        tv.items
            .iter()
            .find(|i| i.hash == sabnzbd::remote_id("SABnzbd_nzo_p86tgx"))
            .unwrap()
            .status,
        DownloadStatus::Paused
    );
    f.0.lock().unwrap().paused = false;
    // Category comparison is case-insensitive, as in the client.
    f.0.lock().unwrap().queue[0]["cat"] = json!("TV");
    let tv = sabnzbd::query(
        &operation,
        &settings,
        Some(&credentials),
        &query(MediaDomain::Tv),
    )
    .await
    .unwrap();
    assert_eq!(tv.items.len(), 6);
    // A client that ignores the category filter is a scope conflict, never a cross-domain item.
    f.0.lock().unwrap().ignore_cat = true;
    assert_eq!(
        sabnzbd::query(
            &operation,
            &settings,
            Some(&credentials),
            &query(MediaDomain::Tv)
        )
        .await
        .err(),
        Some(QbitError::ScopeConflict)
    );
    task.abort();
}

#[tokio::test]
async fn addurl_is_idempotent_per_target_and_lost_responses_never_double_submit() {
    let (f, url, task) = fixture().await;
    let settings = settings(url);
    let client = HttpClient::new().unwrap();
    let operation = client.operation(Uuid::new_v4()).unwrap();
    let credentials = key(KEY);
    let provider = Uuid::new_v4();
    // Colliding native IDs: episode 7 and movie 7 are distinct targets in distinct categories.
    let tv = sabnzbd::prepare(
        provider,
        &settings,
        MediaTarget::Episode(7),
        NZB_URL.into(),
        "Show.S01E01.1080p.WEB-DL",
        true,
    )
    .unwrap();
    let movie = sabnzbd::prepare(
        provider,
        &settings,
        MediaTarget::Movie(7),
        NZB_URL.into(),
        "Film.2020.1080p.WEB-DL",
        false,
    )
    .unwrap();
    assert_ne!(tv.identity().hashes(), movie.identity().hashes());
    assert!(
        !serde_json::to_string(tv.identity())
            .unwrap()
            .contains("PRIVATE_INDEXER_KEY")
    );
    let tv_token = tv.identity().hashes()[0].clone();
    let tv_outcome = sabnzbd::submit(&operation, &settings, Some(&credentials), tv)
        .await
        .unwrap();
    let movie_outcome = sabnzbd::submit(&operation, &settings, Some(&credentials), movie)
        .await
        .unwrap();
    assert_eq!(
        tv_outcome,
        AddOutcome::Observed {
            hash: sabnzbd::remote_id("SABnzbd_nzo_000001")
        }
    );
    assert_eq!(
        movie_outcome,
        AddOutcome::Observed {
            hash: sabnzbd::remote_id("SABnzbd_nzo_000002")
        }
    );
    let adds = f.adds();
    assert_eq!(adds.len(), 2);
    assert_eq!(
        (adds[0]["cat"].as_str(), adds[0]["priority"].as_str()),
        ("tv", "1"),
        "recent TV uses High"
    );
    assert_eq!(
        (adds[1]["cat"].as_str(), adds[1]["priority"].as_str()),
        ("movies", "-100"),
        "older movie uses the category default"
    );
    assert!(adds[0]["nzbname"].starts_with("Show.S01E01.1080p.WEB-DL.hrrdarr-"));
    assert!(adds[0]["nzbname"].ends_with(&tv_token));
    // The projected item name hides the reconciliation marker.
    let page = sabnzbd::query(
        &operation,
        &settings,
        Some(&credentials),
        &query(MediaDomain::Tv),
    )
    .await
    .unwrap();
    assert_eq!(page.items[0].name, "Show.S01E01.1080p.WEB-DL");

    // Re-preparing the same grab reproduces the identity; submitting again finds the job by marker.
    let again = sabnzbd::prepare(
        provider,
        &settings,
        MediaTarget::Episode(7),
        NZB_URL.into(),
        "Show.S01E01.1080p.WEB-DL",
        true,
    )
    .unwrap();
    assert_eq!(again.identity().hashes()[0], tv_token);
    assert_eq!(
        sabnzbd::submit(&operation, &settings, Some(&credentials), again)
            .await
            .unwrap(),
        AddOutcome::AlreadyPresent {
            hash: sabnzbd::remote_id("SABnzbd_nzo_000001")
        }
    );
    assert_eq!(
        f.adds().len(),
        2,
        "a retried grab must not add a second job"
    );

    // Lost response: the client accepted the job but the reply never arrived.
    f.0.lock().unwrap().lose_add_response = true;
    let lost = sabnzbd::prepare(
        provider,
        &settings,
        MediaTarget::Episode(8),
        NZB_URL.into(),
        "Show.S01E02.1080p.WEB-DL",
        false,
    )
    .unwrap();
    let identity = serde_json::to_value(lost.identity()).unwrap();
    let identity: hrrdarr::providers::qbittorrent::SubmissionIdentity =
        serde_json::from_value(identity).unwrap();
    assert_eq!(
        sabnzbd::submit(&operation, &settings, Some(&credentials), lost)
            .await
            .unwrap_err(),
        QbitError::MutationUnknown
    );
    assert_eq!(f.adds().len(), 3);
    f.0.lock().unwrap().lose_add_response = false;
    // Reconciliation from the journaled identity observes the job; retrying never adds again.
    assert_eq!(
        sabnzbd::reconcile(&operation, &settings, Some(&credentials), &identity)
            .await
            .unwrap(),
        Reconciliation::Observed {
            hash: sabnzbd::remote_id("SABnzbd_nzo_000003")
        }
    );
    let retry = sabnzbd::prepare(
        provider,
        &settings,
        MediaTarget::Episode(8),
        NZB_URL.into(),
        "Show.S01E02.1080p.WEB-DL",
        false,
    )
    .unwrap();
    assert!(matches!(
        sabnzbd::submit(&operation, &settings, Some(&credentials), retry)
            .await
            .unwrap(),
        AddOutcome::AlreadyPresent { .. }
    ));
    assert_eq!(f.adds().len(), 3);

    // A job moved to the other domain's category is a conflict, not absence (no resubmission).
    f.0.lock().unwrap().queue[0]["cat"] = json!("movies");
    let moved = sabnzbd::prepare(
        provider,
        &settings,
        MediaTarget::Episode(7),
        NZB_URL.into(),
        "Show.S01E01.1080p.WEB-DL",
        true,
    )
    .unwrap();
    assert_eq!(
        sabnzbd::submit(&operation, &settings, Some(&credentials), moved)
            .await
            .unwrap_err(),
        QbitError::ScopeConflict
    );
    assert_eq!(f.adds().len(), 3);
    f.0.lock().unwrap().queue[0]["cat"] = json!("tv");

    // Missing category or a bad key fails before addurl is dispatched.
    f.0.lock().unwrap().cats.retain(|c| c != "movies");
    let missing = sabnzbd::prepare(
        provider,
        &settings,
        MediaTarget::Movie(9),
        NZB_URL.into(),
        "Film.2021",
        true,
    )
    .unwrap();
    assert_eq!(
        sabnzbd::submit(&operation, &settings, Some(&credentials), missing)
            .await
            .unwrap_err(),
        QbitError::UnsupportedFeature
    );
    let bad = sabnzbd::prepare(
        provider,
        &settings,
        MediaTarget::Episode(9),
        NZB_URL.into(),
        "Show.S01E09",
        true,
    )
    .unwrap();
    assert_eq!(
        sabnzbd::submit(&operation, &settings, Some(&key("nope")), bad)
            .await
            .unwrap_err(),
        QbitError::Http(HttpError::Authentication)
    );
    assert_eq!(f.adds().len(), 3);
    assert!(!f.modes().is_empty());

    // Changing remote-affecting settings invalidates a journaled identity.
    let mut changed = settings.clone();
    if let ProviderSettings::Sabnzbd {
        tv: Some(scope), ..
    } = &mut changed
    {
        scope.category = "series".into();
    }
    assert_eq!(
        sabnzbd::reconcile(&operation, &changed, Some(&credentials), &identity)
            .await
            .unwrap_err(),
        QbitError::ScopeConflict
    );
    task.abort();
}

#[test]
fn settings_reject_shared_or_unsafe_categories() {
    let base = |tv: &str, movies: &str| ProviderSettings::Sabnzbd {
        endpoint: "http://127.0.0.1:1/sabnzbd".into(),
        tv: Some(UsenetScope {
            category: tv.into(),
            ..Default::default()
        }),
        movies: Some(UsenetScope {
            category: movies.into(),
            ..Default::default()
        }),
    };
    // Preparation validates the stored settings shape before any identity is derived.
    let valid = |settings: &ProviderSettings| {
        sabnzbd::prepare(
            Uuid::nil(),
            settings,
            MediaTarget::Episode(1),
            NZB_URL.into(),
            "x",
            true,
        )
        .is_ok()
    };
    assert!(valid(&base("tv", "movies")));
    for (tv, movies) in [
        ("tv", "TV"),
        ("", "movies"),
        ("*", "movies"),
        ("tv/x", "movies"),
        ("tv,hd", "movies"),
        (" tv", "movies"),
    ] {
        assert!(!valid(&base(tv, movies)), "{tv:?}/{movies:?}");
    }
    assert!(
        sabnzbd::prepare(
            Uuid::nil(),
            &base("tv", "movies"),
            MediaTarget::Episode(1),
            "file:///etc/passwd".into(),
            "x",
            true
        )
        .is_err()
    );
    assert!(
        sabnzbd::prepare(
            Uuid::nil(),
            &base("tv", "movies"),
            MediaTarget::Episode(1),
            "magnet:?xt=urn:btih:1".into(),
            "x",
            true
        )
        .is_err()
    );
}
