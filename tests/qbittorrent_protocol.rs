use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri},
    response::IntoResponse,
};
use hrrdarr::{
    api::MediaDomain,
    db::MediaTarget,
    providers::{
        Credentials, DownloadScope, ProviderSettings,
        http::{HttpClient, HttpError},
        qbittorrent::{
            self, AddOutcome, AddSource, Control, DownloadQuery, DownloadStatus, QbitError,
            QbitOptions,
        },
    },
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use uuid::Uuid;
const TV_HASH: &str = "1111111111111111111111111111111111111111";
const MOVIE_HASH: &str = "2222222222222222222222222222222222222222";
#[derive(Clone)]
struct Fixture(Arc<Mutex<FixtureState>>);
struct FixtureState {
    version: String,
    legacy: bool,
    login: bool,
    fail_login: bool,
    items: Vec<Value>,
    calls: Vec<(String, String, String)>,
    unknown_add: bool,
    delay_add: bool,
    uploaded_hash: Option<String>,
    uploaded_aliases: Option<(String, String)>,
    ignore_mutations: bool,
    seed_preferences: Option<Value>,
    seed_properties: Option<Value>,
    properties_fail: bool,
    category_response: Option<Value>,
    tag_fault: u8,
    added_category: Option<String>,
    ignored_paused: bool,
    missing_first_last: bool,
    scope_fault: bool,
    legacy_seed_response: bool,
    file_response: Option<Value>,
    category_create_fault: u8,
}
fn item(hash: &str, category: &str, state: &str) -> Value {
    json!({"hash":hash,"infohash_v1":hash,"infohash_v2":"","category":category,"name":"safe item","state":state,"progress":if state.ends_with("UP"){1.0}else{0.5},"size":100,"amount_left":if state.ends_with("UP"){0}else{50},"dlspeed":2,"upspeed":1,"eta":25,"ratio":0.2,"seeding_time":0,"save_path":"/remote","content_path":"/remote/item","priority":5,"force_start":false,"file_priority":1,"ratio_limit":-2.0,"seeding_time_limit":-2,"inactive_seeding_time_limit":-1,"seq_dl":false,"f_l_piece_prio":false,"auto_tmm":false,"tags":"external"})
}
async fn handler(
    State(f): State<Fixture>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let mut f = f.0.lock().unwrap();
    let labels = f.legacy && f.version.parse::<u16>().is_ok_and(|v| v < 10);
    let path = uri.path();
    let query = uri.query().unwrap_or("");
    let body = String::from_utf8_lossy(&body).into_owned();
    f.calls.push((path.into(), query.into(), body.clone()));
    if path == "/source" {
        assert!(headers.get("cookie").is_none());
        assert!(headers.get("authorization").is_none());
        return b"d4:infod6:lengthi1e4:name1:x12:piece lengthi16384e6:pieces20:12345678901234567890ee".as_slice().into_response();
    }
    if f.legacy && path.starts_with("/api/v2/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    if path.ends_with("auth/login") || path == "/login" {
        assert_eq!(method, Method::POST);
        assert!(body.contains("username=user") && body.contains("password=private-password"));
        assert!(headers.get("referer").is_some());
        if f.fail_login {
            return "Fails.".into_response();
        }
        return (
            [("set-cookie", "SID=privateSID123; path=/; HttpOnly")],
            "Ok.",
        )
            .into_response();
    }
    if f.login && headers.get("cookie").and_then(|v| v.to_str().ok()) != Some("SID=privateSID123") {
        return StatusCode::FORBIDDEN.into_response();
    }
    if path.ends_with("webapiVersion") || path == "/version/api" {
        return f.version.clone().into_response();
    }
    if path.ends_with("app/version") || path == "/version/qbittorrent" {
        return "v5.0.0-private-password-privateSID123".into_response();
    }
    if path.ends_with("preferences") {
        if let Some(value) = &f.seed_preferences {
            return axum::Json(value.clone()).into_response();
        }
        let mut prefs = json!({"queueing_enabled":true,"dht":true,"save_path":"/remote","max_ratio_enabled":false,"max_ratio":-1,"max_seeding_time_enabled":false,"max_seeding_time":-1,"max_ratio_act":0});
        if f.legacy && f.version.parse::<u16>().is_ok_and(|v| v < 16) {
            prefs
                .as_object_mut()
                .unwrap()
                .remove("max_seeding_time_enabled");
            prefs.as_object_mut().unwrap().remove("max_seeding_time");
        }
        return axum::Json(prefs).into_response();
    }
    if path.ends_with("categories") {
        if let Some(value) = &f.category_response {
            return axum::Json(value.clone()).into_response();
        }
        return axum::Json(json!({"tv":{"savePath":"/remote/tv"},"movies":{"savePath":"/remote/movies"},"tv-done":{"savePath":"/remote/tv-done"}})).into_response();
    }
    if path.ends_with("sync/maindata") {
        if let Some(value) = &f.category_response {
            return axum::Json(json!({if labels {"labels"} else {"categories"}:value}))
                .into_response();
        }
        return axum::Json(
            json!({if labels {"labels"} else {"categories"}:["tv","movies","tv-done"]}),
        )
        .into_response();
    }
    let pairs = url::form_urlencoded::parse(query.as_bytes()).collect::<Vec<_>>();
    if path.ends_with("torrents/info") || path == "/query/torrents" {
        let mut items = f.items.clone();
        for item in &mut items {
            match f.tag_fault {
                1 => {
                    item.as_object_mut().unwrap().remove("tags");
                }
                2 => item["tags"] = Value::Null,
                3 => item["tags"] = json!("external,,series tag"),
                _ => {}
            }
        }
        if f.legacy {
            for item in &mut items {
                let o = item.as_object_mut().unwrap();
                if labels {
                    let category = o.remove("category").unwrap();
                    o.insert("label".into(), category);
                }
                if f.legacy_seed_response {
                    for field in [
                        "content_path",
                        "seeding_time",
                        "seeding_time_limit",
                        "inactive_seeding_time_limit",
                        "tags",
                        "infohash_v1",
                        "infohash_v2",
                        "auto_tmm",
                        "file_priority",
                    ] {
                        o.remove(field);
                    }
                    if matches!(f.version.as_str(), "7" | "10") {
                        o.remove("amount_left");
                    }
                    continue;
                }
                if f.version
                    .parse::<u16>()
                    .is_ok_and(|v| (7..=13).contains(&v))
                {
                    for field in [
                        "content_path",
                        "seeding_time",
                        "seeding_time_limit",
                        "inactive_seeding_time_limit",
                        "tags",
                        "infohash_v1",
                        "infohash_v2",
                        "auto_tmm",
                        "file_priority",
                        "amount_left",
                        "ratio_limit",
                    ] {
                        o.remove(field);
                    }
                    if f.missing_first_last {
                        o.remove("f_l_piece_prio");
                    }
                    continue;
                }
                if matches!(f.version.as_str(), "14" | "15") {
                    for field in [
                        "content_path",
                        "seeding_time",
                        "seeding_time_limit",
                        "inactive_seeding_time_limit",
                        "tags",
                        "infohash_v1",
                        "infohash_v2",
                        "auto_tmm",
                        "file_priority",
                    ] {
                        o.remove(field);
                    }
                    o.insert("ratio_limit".into(), json!(-1)); // Legacy exposes resolved maxRatio, not modern raw inheritance.
                    if f.missing_first_last {
                        o.insert("state".into(), json!("metaDL"));
                        o.remove("f_l_piece_prio");
                    }
                    continue;
                }
                for field in ["amount_left", "content_path", "save_path", "seeding_time"] {
                    o.remove(field);
                }
            }
        }
        if f.scope_fault {
            for item in &mut items {
                if labels {
                    item["category"] = json!(if item["label"] == "tv" {
                        "movies"
                    } else {
                        "tv"
                    });
                } else {
                    let category = item.as_object_mut().unwrap().remove("category").unwrap();
                    item["label"] = category;
                }
            }
        }
        if let Some((_, v)) = pairs
            .iter()
            .find(|(k, _)| k == if labels { "label" } else { "category" })
        {
            items.retain(|i| i[if labels { "label" } else { "category" }] == v.as_ref());
        }
        if let Some((_, v)) = pairs.iter().find(|(k, _)| k == "hashes") {
            items.retain(|i| v.split('|').any(|h| i["hash"] == h));
        }
        if pairs.iter().any(|(k, v)| k == "sort" && v == "priority") {
            items.sort_by_key(|v| std::cmp::Reverse(v["priority"].as_i64().unwrap()));
        }
        let limit = pairs
            .iter()
            .find(|(k, _)| k == "limit")
            .and_then(|(_, v)| v.parse().ok())
            .unwrap_or(500);
        let offset = pairs
            .iter()
            .find(|(k, _)| k == "offset")
            .and_then(|(_, v)| v.parse().ok())
            .unwrap_or(0);
        return axum::Json(
            items
                .into_iter()
                .skip(offset)
                .take(limit)
                .collect::<Vec<_>>(),
        )
        .into_response();
    }
    if path.ends_with("torrents/files") || path.starts_with("/query/propertiesFiles/") {
        if let Some(files) = &f.file_response {
            return axum::Json(files.clone()).into_response();
        }
        let requested = pairs
            .iter()
            .find(|(k, _)| k == "hash")
            .map(|(_, v)| v.as_ref())
            .unwrap_or_else(|| path.rsplit('/').next().unwrap());
        let priority = f
            .items
            .iter()
            .find(|i| i["hash"] == requested)
            .and_then(|i| i.get("file_priority"))
            .cloned()
            .unwrap_or(json!(1));
        return axum::Json(json!([{"index":0,"name":"folder/file.mkv","size":100,"progress":0.5,"priority":priority}])).into_response();
    }
    if path.ends_with("torrents/properties") || path.starts_with("/query/propertiesGeneral/") {
        if f.properties_fail {
            return StatusCode::BAD_GATEWAY.into_response();
        }
        if let Some(value) = &f.seed_properties {
            return axum::Json(value.clone()).into_response();
        }
        return axum::Json(json!({"save_path":"/remote"})).into_response();
    }
    let pairs = if let Some(boundary) = headers
        .get("content-type")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.split("boundary=").nth(1))
    {
        // Owned fixture parser: reqwest constructs the actual multipart request.
        body.split(&format!("--{boundary}"))
            .filter_map(|part| {
                let (head, value) = part.split_once("\r\n\r\n")?;
                if head.contains("filename=") {
                    return None;
                }
                let name = head.split("name=\"").nth(1)?.split('"').next()?;
                Some((
                    std::borrow::Cow::Owned(name.to_owned()),
                    std::borrow::Cow::Owned(value.trim_end_matches("\r\n").to_owned()),
                ))
            })
            .collect::<Vec<_>>()
    } else {
        url::form_urlencoded::parse(body.as_bytes()).collect::<Vec<_>>()
    };
    if path.ends_with("torrents/add") || path == "/command/download" || path == "/command/upload" {
        assert_eq!(method, Method::POST);
        let source = pairs
            .iter()
            .find(|(k, _)| k == "urls")
            .map(|(_, v)| v.as_ref())
            .unwrap_or("");
        let h = if source.contains(MOVIE_HASH) {
            MOVIE_HASH
        } else {
            TV_HASH
        };
        let cat = pairs
            .iter()
            .find(|(k, _)| k == if labels { "label" } else { "category" })
            .map(|(_, v)| v.as_ref())
            .unwrap_or("tv");
        let h = f.uploaded_hash.clone().unwrap_or_else(|| h.to_owned());
        let mut added = item(&h, cat, "downloading");
        if let Some(category) = &f.added_category {
            added["category"] = json!(category);
        }
        if let Some((v1, v2)) = &f.uploaded_aliases {
            added["infohash_v1"] = json!(v1);
            added["infohash_v2"] = json!(v2);
        }
        for (wire, remote) in [
            ("sequentialDownload", "seq_dl"),
            ("firstLastPiecePrio", "f_l_piece_prio"),
        ] {
            added[remote] = json!(pairs.iter().any(|(k, v)| k == wire && v == "true"));
        }
        let paused = pairs
            .iter()
            .any(|(k, v)| (k == "paused" || k == "stopped") && v == "true");
        if paused != f.ignored_paused {
            added["state"] = json!(if f.legacy { "pausedDL" } else { "stoppedDL" });
        }
        for (wire, remote) in [
            ("ratioLimit", "ratio_limit"),
            ("seedingTimeLimit", "seeding_time_limit"),
            ("inactiveSeedingTimeLimit", "inactive_seeding_time_limit"),
        ] {
            if let Some((_, v)) = pairs.iter().find(|(k, _)| k == wire) {
                added[remote] = serde_json::from_str(v).unwrap();
            }
        }
        if !f.ignore_mutations {
            if let Some((_, tags)) = pairs.iter().find(|(key, _)| key == "tags") {
                added["tags"] = json!(format!("external, {tags}"));
            }
        }
        if let Some(Value::Array(registry)) = &mut f.category_response {
            if !registry.contains(&added["category"]) {
                registry.push(added["category"].clone());
            }
        }
        if !f.delay_add {
            f.items.push(added);
        }
        if f.unknown_add {
            return StatusCode::BAD_GATEWAY.into_response();
        }
        return if path == "/command/upload" { "" } else { "Ok." }.into_response();
    }
    if method == Method::POST {
        if path == "/command/addCategory" || path == "/api/v2/torrents/createCategory" {
            assert!(!labels);
            let name = pairs
                .iter()
                .find(|(key, _)| key == "category")
                .unwrap()
                .1
                .to_string();
            if f.ignore_mutations {
                return "".into_response();
            }
            if let Some(Value::Array(registry)) = &mut f.category_response {
                registry.push(json!(name));
            } else if let Some(Value::Object(registry)) = &mut f.category_response {
                registry.insert(name, json!({"savePath":""}));
            }
            match f.category_create_fault {
                1 => f.items[0]["category"] = json!("foreign"),
                2 => {
                    f.items[0]["progress"] = json!(0.5);
                    f.items[0]["amount_left"] = json!(50);
                }
                _ => {}
            }
            if f.unknown_add {
                return StatusCode::BAD_GATEWAY.into_response();
            }
            return "".into_response();
        }
        if f.ignore_mutations {
            return "".into_response();
        }
        let h = pairs
            .iter()
            .find(|(k, _)| k == "hashes" || k == "hash")
            .map(|(_, v)| v.to_string())
            .unwrap_or_default();
        assert_ne!(h, "all");
        if path.ends_with("/delete") || path.ends_with("deletePerm") {
            f.items.retain(|i| i["hash"] != h)
        }
        for item in f.items.iter_mut().filter(|i| i["hash"] == h) {
            if path.ends_with("addTags") {
                let tags = pairs.iter().find(|(k, _)| k == "tags").unwrap().1.as_ref();
                item["tags"] = json!(format!("{}, {}", item["tags"].as_str().unwrap(), tags));
            }
            if path.ends_with("topPrio") {
                item["priority"] = json!(1);
            }
            if path.ends_with("bottomPrio") {
                item["priority"] = json!(10);
            }
            if path.ends_with("setForceStart") {
                item["force_start"] = json!(pairs.iter().any(|(k, v)| k == "value" && v == "true"));
            }
            if path.ends_with("filePrio") || path.ends_with("setFilePrio") {
                item["file_priority"] =
                    serde_json::from_str(&pairs.iter().find(|(k, _)| k == "priority").unwrap().1)
                        .unwrap();
            }
            if path.ends_with("setShareLimits") {
                for (wire, remote) in [
                    ("ratioLimit", "ratio_limit"),
                    ("seedingTimeLimit", "seeding_time_limit"),
                    ("inactiveSeedingTimeLimit", "inactive_seeding_time_limit"),
                ] {
                    if let Some((_, v)) = pairs.iter().find(|(k, _)| k == wire) {
                        item[remote] = serde_json::from_str(v).unwrap();
                    }
                }
            }
            if path.ends_with("/stop") || path.ends_with("/pause") {
                item["state"] = json!("stoppedDL")
            }
            if path.ends_with("/start") || path.ends_with("/resume") {
                item["state"] = json!("downloading")
            }
            if path.ends_with("setCategory") || path.ends_with("setLabel") {
                item["category"] = json!(
                    pairs
                        .iter()
                        .find(|(k, _)| k == if labels { "label" } else { "category" })
                        .unwrap()
                        .1
                        .as_ref()
                )
            }
        }
        return "".into_response();
    }
    StatusCode::NOT_FOUND.into_response()
}
async fn fixture(legacy: bool) -> (Fixture, String, tokio::task::JoinHandle<()>) {
    let fixture = Fixture(Arc::new(Mutex::new(FixtureState {
        version: if legacy { "6" } else { "2.11.0" }.into(),
        legacy,
        login: true,
        fail_login: false,
        items: Vec::new(),
        calls: Vec::new(),
        unknown_add: false,
        delay_add: false,
        uploaded_hash: None,
        uploaded_aliases: None,
        ignore_mutations: false,
        seed_preferences: None,
        seed_properties: None,
        properties_fail: false,
        category_response: None,
        tag_fault: 0,
        added_category: None,
        ignored_paused: false,
        missing_first_last: false,
        scope_fault: false,
        legacy_seed_response: false,
        file_response: None,
        category_create_fault: 0,
    })));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new().fallback(handler).with_state(fixture.clone());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (fixture, endpoint, task)
}
fn settings(endpoint: String) -> ProviderSettings {
    let scope = |category: &str, imported: Option<&str>| DownloadScope {
        category: category.into(),
        imported_category: imported.map(str::to_owned),
        recent_priority: 0,
        older_priority: 0,
        ..Default::default()
    };
    ProviderSettings::Qbittorrent {
        endpoint,
        tv: Some(scope("tv", Some("tv-done"))),
        movies: Some(scope("movies", None)),
    }
}
fn credentials() -> Credentials {
    Credentials::UsernamePassword {
        username: "user".into(),
        password: "private-password".into(),
    }
}
async fn submit_source(
    operation: &hrrdarr::providers::http::HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
    target: &MediaTarget,
    source: AddSource,
    recent: bool,
    options: QbitOptions,
) -> Result<AddOutcome, QbitError> {
    let target = match target {
        MediaTarget::Episode(id) => MediaTarget::Episode(*id),
        MediaTarget::Movie(id) => MediaTarget::Movie(*id),
    };
    let prepared =
        qbittorrent::prepare(operation, settings, target, source, recent, options).await?;
    qbittorrent::submit(operation, settings, credentials, prepared).await
}
#[tokio::test]
async fn authenticated_modern_and_legacy_reads_keep_domains_separate() {
    for legacy in [false, true] {
        let (f, url, task) = fixture(legacy).await;
        let settings = settings(url);
        let credentials = credentials();
        f.0.lock().unwrap().items = vec![
            item(TV_HASH, "tv", "checkingUP"),
            item(MOVIE_HASH, "movies", "stoppedUP"),
        ];
        let client = HttpClient::new().unwrap();
        let operation = client.operation(Uuid::new_v4()).unwrap();
        let result = qbittorrent::test_connection(&operation, &settings, Some(&credentials))
            .await
            .unwrap();
        assert_eq!(result.domains.len(), 2);
        assert!(
            !serde_json::to_string(&result)
                .unwrap()
                .contains("private-password")
        );
        assert!(
            !serde_json::to_string(&result)
                .unwrap()
                .contains("privateSID123")
        );
        assert!(result.missing_categories.is_empty());
        let tv = qbittorrent::query(
            &operation,
            &settings,
            Some(&credentials),
            &DownloadQuery {
                domain: MediaDomain::Tv,
                offset: 0,
                limit: 10,
                imported: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(tv.items.len(), 1);
        assert_eq!(tv.items[0].status, DownloadStatus::Queued);
        assert!(!tv.items[0].completed);
        let movies = qbittorrent::query(
            &operation,
            &settings,
            Some(&credentials),
            &DownloadQuery {
                domain: MediaDomain::Movies,
                offset: 0,
                limit: 10,
                imported: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(movies.items[0].hash, MOVIE_HASH);
        assert_eq!(movies.items[0].status, DownloadStatus::Completed);
        assert!(movies.items[0].completed);
        assert_eq!(
            movies.items[0].remaining_bytes,
            if legacy { None } else { Some(0) }
        );
        let files = qbittorrent::files(
            &operation,
            &settings,
            Some(&credentials),
            &qbittorrent::DownloadFilesQuery {
                domain: MediaDomain::Tv,
                hash: TV_HASH.into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(files.files[0].name, "folder/file.mkv");
        if legacy {
            f.0.lock().unwrap().version = "5".into();
            assert!(matches!(
                qbittorrent::test_connection(&operation, &settings, Some(&credentials)).await,
                Err(QbitError::UnsupportedFeature)
            ));
            f.0.lock().unwrap().version = "6".into();
        }
        f.0.lock().unwrap().fail_login = true;
        assert!(matches!(
            qbittorrent::test_connection(&operation, &settings, Some(&credentials)).await,
            Err(QbitError::Http(HttpError::Authentication))
        ));
        task.abort();
        let _ = task.await;
    }
}
#[tokio::test]
async fn add_controls_and_ambiguous_submission_never_cross_categories_or_retry() {
    let (f, url, task) = fixture(false).await;
    let settings = settings(url);
    let credentials = credentials();
    let client = HttpClient::new().unwrap();
    let operation = client.operation(Uuid::new_v4()).unwrap();
    let tv = MediaTarget::Episode(7);
    let movie = MediaTarget::Movie(7);
    for (target, h) in [(&tv, TV_HASH), (&movie, MOVIE_HASH)] {
        let result = submit_source(
            &operation,
            &settings,
            Some(&credentials),
            target,
            AddSource::Magnet(format!("magnet:?xt=urn:btih:{h}")),
            false,
            QbitOptions::default(),
        )
        .await
        .unwrap();
        assert_eq!(result, AddOutcome::Observed { hash: h.into() });
    }
    let before = f.0.lock().unwrap().calls.len();
    assert_eq!(
        qbittorrent::control(
            &operation,
            &settings,
            Some(&credentials),
            &tv,
            MOVIE_HASH,
            Control::Remove { delete_files: true }
        )
        .await,
        Err(QbitError::ScopeConflict)
    );
    assert!(
        f.0.lock().unwrap().calls[before..]
            .iter()
            .all(|(path, _, _)| !path.ends_with("/delete"))
    );
    qbittorrent::control(
        &operation,
        &settings,
        Some(&credentials),
        &tv,
        TV_HASH,
        Control::Pause,
    )
    .await
    .unwrap();
    qbittorrent::control(
        &operation,
        &settings,
        Some(&credentials),
        &tv,
        TV_HASH,
        Control::Resume,
    )
    .await
    .unwrap();
    qbittorrent::control(
        &operation,
        &settings,
        Some(&credentials),
        &tv,
        TV_HASH,
        Control::Remove {
            delete_files: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(f.0.lock().unwrap().items.len(), 1);
    f.0.lock().unwrap().unknown_add = true;
    let before = f.0.lock().unwrap().calls.len();
    assert_eq!(
        submit_source(
            &operation,
            &settings,
            Some(&credentials),
            &tv,
            AddSource::Magnet(format!("magnet:?xt=urn:btih:{TV_HASH}")),
            false,
            QbitOptions::default()
        )
        .await,
        Err(QbitError::MutationUnknown)
    );
    assert_eq!(
        f.0.lock().unwrap().calls[before..]
            .iter()
            .filter(|(path, _, _)| path.ends_with("/add"))
            .count(),
        1
    );
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn url_upload_validates_identity_before_client_submission_and_exposes_pending() {
    let (f, url, task) = fixture(false).await;
    let source = format!("{url}/source?passkey=a%2Bb%26c");
    let settings = settings(url);
    let credentials = credentials();
    let client = HttpClient::new().unwrap();
    let operation = client.operation(Uuid::new_v4()).unwrap();
    let result = submit_source(
        &operation,
        &settings,
        Some(&credentials),
        &MediaTarget::Episode(1),
        AddSource::Url(source),
        false,
        QbitOptions::default(),
    )
    .await
    .unwrap();
    // Fixture reports an unrelated hash; accepted upload must not invent a matching download.
    assert!(matches!(result,AddOutcome::Pending{hashes} if hashes.len()==1&&hashes[0].len()==40));
    let calls = &f.0.lock().unwrap().calls;
    let source = calls.iter().find(|(path, _, _)| path == "/source").unwrap();
    assert_eq!(source.1, "passkey=a%2Bb%26c");
    let upload = calls
        .iter()
        .find(|(path, _, _)| path.ends_with("/add"))
        .unwrap();
    assert!(upload.2.contains("name=\"torrents\""));
    assert!(upload.2.contains("filename=\"download.torrent\""));
    task.abort();
}

#[tokio::test]
async fn persisted_domain_options_are_sent_with_versioned_names() {
    let (f, url, task) = fixture(false).await;
    let mut settings = settings(url);
    let credentials = credentials();
    if let ProviderSettings::Qbittorrent {
        tv: Some(scope), ..
    } = &mut settings
    {
        scope.initial_state = hrrdarr::providers::DownloadInitialState::Stopped;
        scope.content_layout = hrrdarr::providers::DownloadContentLayout::Subfolder;
        scope.sequential_order = true;
        scope.first_last_first = true;
        scope.add_tags = true;
    }
    let client = HttpClient::new().unwrap();
    let operation = client.operation(Uuid::new_v4()).unwrap();
    submit_source(
        &operation,
        &settings,
        Some(&credentials),
        &MediaTarget::Episode(1),
        AddSource::Magnet(format!("magnet:?xt=urn:btih:{TV_HASH}")),
        true,
        QbitOptions {
            tags: vec!["my tag".into()],
            ratio_limit: Some(2.0),
            seeding_minutes: Some(60),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let calls = &f.0.lock().unwrap().calls;
    let (_, _, body) = calls
        .iter()
        .find(|(path, _, _)| path.ends_with("/add"))
        .unwrap();
    let fields =
        url::form_urlencoded::parse(body.as_bytes()).collect::<std::collections::BTreeMap<_, _>>();
    for (key, value) in [
        ("category", "tv"),
        ("stopped", "true"),
        ("contentLayout", "Subfolder"),
        ("sequentialDownload", "true"),
        ("firstLastPiecePrio", "true"),
        ("tags", "my tag"),
        ("ratioLimit", "2"),
        ("seedingTimeLimit", "60"),
    ] {
        assert_eq!(fields.get(key).map(|v| v.as_ref()), Some(value));
    }
    assert!(!fields.contains_key("paused"));
    task.abort();
}

#[tokio::test]
async fn prepared_url_identity_survives_unknown_submission_and_reconciles_without_resubmission() {
    let (f, url, task) = fixture(false).await;
    let source = format!("{url}/source?passkey=private-source");
    let settings = settings(url);
    let credentials = credentials();
    let client = HttpClient::new().unwrap();
    let operation = client.operation(Uuid::new_v4()).unwrap();
    let prepared = qbittorrent::prepare(
        &operation,
        &settings,
        MediaTarget::Episode(7),
        AddSource::Url(source),
        false,
        QbitOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(prepared.identity().target(), &MediaTarget::Episode(7));
    assert_eq!(prepared.identity().hashes().len(), 1);
    assert_eq!(prepared.identity().payload_sha256().len(), 64);
    assert_eq!(
        f.0.lock().unwrap().calls.len(),
        1,
        "Preparation must not contact qBittorrent or create categories"
    );
    let record = serde_json::to_vec(prepared.identity()).unwrap();
    assert!(!String::from_utf8_lossy(&record).contains("private-source"));
    // The caller records identity before external mutation; this isolated file is not a production journal.
    struct Record(std::path::PathBuf);
    impl Drop for Record {
        fn drop(&mut self) {
            std::fs::remove_file(&self.0).unwrap();
        }
    }
    let path =
        Record(std::env::temp_dir().join(format!("hrrdarr-qbit-intent-{}.json", Uuid::new_v4())));
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path.0)
            .unwrap();
        file.write_all(&record).unwrap();
        file.sync_all().unwrap();
    }
    let hash = prepared.identity().hashes()[0].clone();
    {
        let mut fixture = f.0.lock().unwrap();
        fixture.uploaded_hash = Some(hash.clone());
        fixture.unknown_add = true;
    }
    assert_eq!(
        qbittorrent::submit(&operation, &settings, Some(&credentials), prepared).await,
        Err(QbitError::MutationUnknown)
    );
    let identity: qbittorrent::SubmissionIdentity =
        serde_json::from_slice(&std::fs::read(&path.0).unwrap()).unwrap();
    let before = f.0.lock().unwrap().calls.len();
    assert_eq!(
        qbittorrent::reconcile(&operation, &settings, Some(&credentials), &identity)
            .await
            .unwrap(),
        qbittorrent::Reconciliation::Observed { hash: hash.clone() }
    );
    let fixture = f.0.lock().unwrap();
    assert_eq!(
        fixture
            .calls
            .iter()
            .filter(|(p, _, _)| p == "/source")
            .count(),
        1,
        "Submit must use retained bytes without fetching URL again"
    );
    assert_eq!(
        fixture
            .calls
            .iter()
            .filter(|(p, _, _)| p.ends_with("/add"))
            .count(),
        1
    );
    assert!(
        fixture.calls[before..]
            .iter()
            .all(|(p, _, _)| !p.ends_with("/add") && !p.contains("Category"))
    );
    drop(fixture);
    // No observed torrent is not proof that submission failed, and does not authorize another add.
    f.0.lock().unwrap().items.clear();
    assert_eq!(
        qbittorrent::reconcile(&operation, &settings, Some(&credentials), &identity)
            .await
            .unwrap(),
        qbittorrent::Reconciliation::NotObserved
    );
    f.0.lock()
        .unwrap()
        .items
        .push(item(&hash, "movies", "downloading"));
    assert_eq!(
        qbittorrent::reconcile(&operation, &settings, Some(&credentials), &identity).await,
        Err(QbitError::ScopeConflict)
    );
    task.abort();
    let _ = task.await;
}
#[tokio::test]
async fn preparation_and_identity_binding_reject_invalid_inputs_before_side_effects() {
    let (f, url, task) = fixture(false).await;
    let settings = settings(url);
    let client = HttpClient::new().unwrap();
    let operation = client.operation(Uuid::new_v4()).unwrap();
    for source in [
        AddSource::Torrent {
            filename: "../bad".into(),
            bytes: vec![0],
        },
        AddSource::Torrent {
            filename: "bad.torrent".into(),
            bytes: vec![0],
        },
        AddSource::Magnet("magnet:?xt=urn:btih:all".into()),
    ] {
        assert!(matches!(
            qbittorrent::prepare(
                &operation,
                &settings,
                MediaTarget::Movie(7),
                source,
                false,
                QbitOptions::default()
            )
            .await,
            Err(QbitError::InvalidRequest)
        ));
    }
    assert!(f.0.lock().unwrap().calls.is_empty());
    let prepared = qbittorrent::prepare(
        &operation,
        &settings,
        MediaTarget::Movie(7),
        AddSource::Magnet(format!("magnet:?xt=urn:btih:{MOVIE_HASH}")),
        false,
        QbitOptions::default(),
    )
    .await
    .unwrap();
    let record = serde_json::to_value(prepared.identity()).unwrap();
    for field in ["endpoint", "category", "options"] {
        let mut changed = settings.clone();
        if let ProviderSettings::Qbittorrent {
            endpoint,
            movies: Some(scope),
            ..
        } = &mut changed
        {
            match field {
                "endpoint" => endpoint.push_str("/another-client"),
                "category" => scope.category = "other-movies".into(),
                _ => scope.sequential_order = true,
            }
        }
        assert_eq!(
            qbittorrent::reconcile(&operation, &changed, None, prepared.identity()).await,
            Err(QbitError::ScopeConflict)
        );
    }
    for (key, value) in [
        ("hashes", json!([])),
        ("hashes", json!([TV_HASH, MOVIE_HASH])),
        ("payload_sha256", json!("invalid")),
        ("target", json!({"media_type":"movie","id":0})),
        ("version", json!(2)),
    ] {
        let mut invalid = record.clone();
        invalid[key] = value;
        let identity: qbittorrent::SubmissionIdentity = serde_json::from_value(invalid).unwrap();
        assert_eq!(
            qbittorrent::reconcile(&operation, &settings, None, &identity).await,
            Err(QbitError::InvalidRequest)
        );
    }
    let mut changed = settings.clone();
    if let ProviderSettings::Qbittorrent { endpoint, .. } = &mut changed {
        endpoint.push_str("/changed");
    }
    assert_eq!(
        qbittorrent::submit(&operation, &changed, None, prepared).await,
        Err(QbitError::ScopeConflict)
    );
    assert!(
        f.0.lock().unwrap().calls.is_empty(),
        "All invalid bindings must fail before any client request"
    );
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn controls_require_observed_postconditions_in_each_domain() {
    let (f, url, task) = fixture(false).await;
    let settings = settings(url);
    let credentials = credentials();
    let client = HttpClient::new().unwrap();
    for (target, h, category) in [
        (MediaTarget::Episode(9), TV_HASH, "tv"),
        (MediaTarget::Movie(9), MOVIE_HASH, "movies"),
    ] {
        f.0.lock().unwrap().items = vec![item(h, category, "downloading")];
        for ignore in [false, true] {
            f.0.lock().unwrap().ignore_mutations = ignore;
            for action in [
                Control::Priority { first: true },
                Control::Priority { first: false },
                Control::ForceStart { enabled: true },
                Control::FilePriority {
                    indexes: vec![0],
                    priority: 7,
                },
                Control::ShareLimits {
                    ratio: 2.0,
                    seeding_minutes: 60,
                    inactive_seeding_minutes: None,
                },
            ] {
                // Reset to a contradictory state so a successful HTTP response alone cannot pass.
                let mut other = item(
                    if h == TV_HASH { MOVIE_HASH } else { TV_HASH },
                    if category == "tv" { "movies" } else { "tv" },
                    "downloading",
                );
                other["priority"] = json!(9);
                // A second higher-priority-number item makes an ignored bottom operation observable.
                f.0.lock().unwrap().items = vec![item(h, category, "downloading"), other];
                let operation = client.operation(Uuid::new_v4()).unwrap();
                let result = qbittorrent::control(
                    &operation,
                    &settings,
                    Some(&credentials),
                    &target,
                    h,
                    action,
                )
                .await;
                assert_eq!(
                    result,
                    if ignore {
                        Err(QbitError::MutationUnknown)
                    } else {
                        Ok(())
                    }
                );
            }
        }
    }
    let fixture = f.0.lock().unwrap();
    let (_, _, body) = fixture
        .calls
        .iter()
        .find(|(path, _, _)| path.ends_with("setShareLimits"))
        .unwrap();
    assert!(
        body.contains("inactiveSeedingTimeLimit=-1"),
        "API2.9.2+ requires the omitted axis to preserve the observed raw sentinel"
    );
    drop(fixture);
    f.0.lock().unwrap().ignore_mutations = false;
    let mut incomplete = item(TV_HASH, "tv", "uploading");
    incomplete["progress"] = json!(0.5);
    incomplete["amount_left"] = json!(50);
    f.0.lock().unwrap().items = vec![incomplete];
    let before = f.0.lock().unwrap().calls.len();
    let operation = client.operation(Uuid::new_v4()).unwrap();
    assert_eq!(
        qbittorrent::control(
            &operation,
            &settings,
            Some(&credentials),
            &MediaTarget::Episode(9),
            TV_HASH,
            Control::MarkImported
        )
        .await,
        Err(QbitError::Rejected)
    );
    assert!(
        f.0.lock().unwrap().calls[before..]
            .iter()
            .all(|(path, _, _)| !path.ends_with("setCategory"))
    );
    task.abort();
    let _ = task.await;
}
#[tokio::test]
async fn versioned_options_use_supported_wire_fields_and_reject_ignored_features() {
    for (legacy, version, layout, expect_field) in [
        (
            true,
            "16",
            hrrdarr::providers::DownloadContentLayout::Subfolder,
            "root_folder",
        ),
        (
            false,
            "2.6.1",
            hrrdarr::providers::DownloadContentLayout::Subfolder,
            "root_folder",
        ),
        (
            false,
            "2.7.0",
            hrrdarr::providers::DownloadContentLayout::Original,
            "contentLayout",
        ),
    ] {
        let (f, url, task) = fixture(legacy).await;
        f.0.lock().unwrap().version = version.into();
        let mut settings = settings(url);
        if let ProviderSettings::Qbittorrent {
            movies: Some(scope),
            ..
        } = &mut settings
        {
            scope.content_layout = layout;
            scope.sequential_order = true;
            scope.first_last_first = true;
        }
        let client = HttpClient::new().unwrap();
        let operation = client.operation(Uuid::new_v4()).unwrap();
        let result = submit_source(
            &operation,
            &settings,
            Some(&credentials()),
            &MediaTarget::Movie(1),
            AddSource::Magnet(format!("magnet:?xt=urn:btih:{MOVIE_HASH}")),
            false,
            QbitOptions::default(),
        )
        .await
        .unwrap();
        assert_eq!(
            result,
            AddOutcome::Observed {
                hash: MOVIE_HASH.into()
            }
        );
        let fixture = f.0.lock().unwrap();
        let (_, _, body) = fixture
            .calls
            .iter()
            .find(|(p, _, _)| p.ends_with("/add") || p.ends_with("/download"))
            .unwrap();
        assert!(body.contains(&format!("{expect_field}=")));
        assert!(
            body.contains("sequentialDownload=true") && body.contains("firstLastPiecePrio=true")
        );
        assert!(body.contains("paused=false"));
        assert!(!body.contains(if expect_field == "root_folder" {
            "contentLayout"
        } else {
            "root_folder"
        }));
        drop(fixture);
        task.abort();
        let _ = task.await;
    }
    for (version, kind) in [
        ("2.2.1", "tags"),
        ("2.6.1", "original"),
        ("2.9.1", "inactive"),
        ("2.0.0", "share"),
        // API7+ default submission is supported; API6 cannot scope its add request.
        ("6", "legacy"),
    ] {
        let (f, url, task) = fixture(kind == "legacy").await;
        f.0.lock().unwrap().version = version.into();
        let mut settings = settings(url);
        if let ProviderSettings::Qbittorrent {
            tv: Some(scope), ..
        } = &mut settings
        {
            scope.add_tags = kind == "tags";
            if kind == "original" {
                scope.content_layout = hrrdarr::providers::DownloadContentLayout::Original;
            }
        }
        let options = QbitOptions {
            tags: if kind == "tags" {
                vec!["tag".into()]
            } else {
                vec![]
            },
            ratio_limit: (kind == "share").then_some(2.0),
            inactive_seeding_minutes: (kind == "inactive").then_some(30),
            ..Default::default()
        };
        let client = HttpClient::new().unwrap();
        let operation = client.operation(Uuid::new_v4()).unwrap();
        assert_eq!(
            submit_source(
                &operation,
                &settings,
                Some(&credentials()),
                &MediaTarget::Episode(1),
                AddSource::Magnet(format!("magnet:?xt=urn:btih:{TV_HASH}")),
                false,
                options
            )
            .await,
            Err(QbitError::UnsupportedFeature)
        );
        assert!(
            f.0.lock()
                .unwrap()
                .calls
                .iter()
                .all(|(p, _, _)| !p.ends_with("/add")
                    && !p.ends_with("/download")
                    && !p.ends_with("createCategory"))
        );
        task.abort();
        let _ = task.await;
    }
}

#[tokio::test]
async fn detail_seed_facts_keep_domains_unknowns_and_properties_fallback_separate() {
    for legacy in [false] {
        let (f, url, task) = fixture(legacy).await;
        let settings = settings(url);
        let credentials = credentials();
        let client = HttpClient::new().unwrap();
        let mut tv = item(TV_HASH, "tv", "pausedUP");
        tv["ratio_limit"] = json!(2.0);
        tv["ratio"] = json!(2.0);
        tv["seeding_time_limit"] = json!(-1);
        tv["inactive_seeding_time_limit"] = json!(-1);
        let mut movie = item(MOVIE_HASH, "movies", "stoppedUP");
        movie["ratio_limit"] = json!(-1);
        movie["seeding_time_limit"] = json!(60);
        movie["inactive_seeding_time_limit"] = json!(-1);
        movie.as_object_mut().unwrap().remove("seeding_time");
        {
            let mut state = f.0.lock().unwrap();
            state.items = vec![tv, movie];
            state.seed_properties = Some(json!({"save_path":"/remote","seeding_time":3600}));
            state.seed_preferences = Some(
                json!({"max_ratio_enabled":false,"max_ratio":-1,"max_seeding_time_enabled":false,"max_seeding_time":-1,"max_inactive_seeding_time_enabled":false,"max_inactive_seeding_time":-1,"max_ratio_act":1}),
            );
        }
        for (target, h) in [
            (MediaTarget::Episode(1), TV_HASH),
            (MediaTarget::Movie(1), MOVIE_HASH),
        ] {
            let operation = client.operation(Uuid::new_v4()).unwrap();
            let detail =
                qbittorrent::details(&operation, &settings, Some(&credentials), &target, h)
                    .await
                    .unwrap();
            assert_eq!(detail.seed_policy.ready, Some(true));
            assert_eq!(detail.seed_policy.client_removes_on_limit(), Some(true));
            if h == MOVIE_HASH {
                assert_eq!(detail.seed_policy.elapsed_seeding_seconds, Some(3600));
                assert_eq!(
                    detail.seed_policy.seeding_minutes.raw,
                    qbittorrent::SeedLimit::Limited(60)
                );
            } else {
                assert_eq!(
                    detail.seed_policy.ratio.raw,
                    qbittorrent::SeedLimit::Limited(2.0)
                );
            }
        }
        // Missing raw fields on an older client remain unknown; do not invent inherited defaults.
        {
            let mut fixture = f.0.lock().unwrap();
            let movie = fixture
                .items
                .iter_mut()
                .find(|i| i["hash"] == MOVIE_HASH)
                .unwrap();
            movie.as_object_mut().unwrap().remove("seeding_time_limit");
            fixture.seed_properties = Some(json!({"save_path":"/remote"}));
        }
        let operation = client.operation(Uuid::new_v4()).unwrap();
        let detail = qbittorrent::details(
            &operation,
            &settings,
            Some(&credentials),
            &MediaTarget::Movie(1),
            MOVIE_HASH,
        )
        .await
        .unwrap();
        assert_eq!(detail.seed_policy.ready, None);
        assert_eq!(detail.seed_policy.elapsed_seeding_seconds, None);
        f.0.lock().unwrap().properties_fail = true;
        let operation = client.operation(Uuid::new_v4()).unwrap();
        assert!(matches!(
            qbittorrent::details(
                &operation,
                &settings,
                Some(&credentials),
                &MediaTarget::Movie(1),
                MOVIE_HASH
            )
            .await,
            Err(QbitError::Http(HttpError::Transport))
        ));
        let fixture = f.0.lock().unwrap();
        assert!(fixture.calls.iter().all(|(p, _, _)| !p.ends_with("/add")
            && !p.ends_with("/delete")
            && !p.ends_with("setCategory")));
        drop(fixture);
        task.abort();
        let _ = task.await;
    }
}

#[tokio::test]
async fn connection_test_rejects_automatic_removal_without_mutation() {
    let (f, url, task) = fixture(false).await;
    let settings = settings(url);
    let credentials = credentials();
    let client = HttpClient::new().unwrap();
    for (ratio, enabled, minutes, action, rejected) in [
        (true, false, -1, 1, true),
        (true, false, -1, 3, true),
        (false, true, 20159, 3, true),
        (false, true, -1, 3, true),
        (false, true, 20160, 3, false),
        (true, true, 0, 0, false),
        (true, true, 0, 2, false),
        (false, false, 0, 3, false),
    ] {
        f.0.lock().unwrap().seed_preferences = Some(json!({
            "queueing_enabled":true,"max_ratio_enabled":ratio,
            "max_seeding_time_enabled":enabled,"max_seeding_time":minutes,
            "max_ratio_act":action
        }));
        let operation = client.operation(Uuid::new_v4()).unwrap();
        let result = qbittorrent::test_connection(&operation, &settings, Some(&credentials)).await;
        if rejected {
            assert!(matches!(result, Err(QbitError::UnsafeRetention)));
        } else {
            assert_eq!(result.unwrap().domains.len(), 2);
        }
    }
    assert!(
        f.0.lock()
            .unwrap()
            .calls
            .iter()
            .all(|(path, _, _)| !path.ends_with("/add")
                && !path.ends_with("/delete")
                && !path.ends_with("setPreferences"))
    );
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn remote_v2_and_hybrid_aliases_require_complete_consistent_mapping() {
    for movie in [false, true] {
        let (f, url, task) = fixture(false).await;
        let settings = settings(url);
        let credentials = credentials();
        let client = HttpClient::new().unwrap();
        let category = if movie { "movies" } else { "tv" };
        let v1 = "a".repeat(40);
        let v2 = "b".repeat(64);
        let remote = "b".repeat(40);
        for hybrid in [false, true] {
            let magnet = if hybrid {
                format!("magnet:?xt=urn:btih:{v1}&xt=urn:btmh:1220{v2}")
            } else {
                format!("magnet:?xt=urn:btmh:1220{v2}")
            };
            let operation = client.operation(Uuid::new_v4()).unwrap();
            let prepared = qbittorrent::prepare(
                &operation,
                &settings,
                if movie {
                    MediaTarget::Movie(7)
                } else {
                    MediaTarget::Episode(7)
                },
                AddSource::Magnet(magnet),
                false,
                QbitOptions::default(),
            )
            .await
            .unwrap();
            let mut good = item(&remote, category, "pausedUP");
            good["infohash_v1"] = json!(if hybrid {
                v1.to_uppercase()
            } else {
                String::new()
            });
            good["infohash_v2"] = json!(v2.to_uppercase());
            f.0.lock().unwrap().items = vec![good.clone()];
            let operation = client.operation(Uuid::new_v4()).unwrap();
            assert_eq!(
                qbittorrent::reconcile(
                    &operation,
                    &settings,
                    Some(&credentials),
                    prepared.identity()
                )
                .await,
                Ok(qbittorrent::Reconciliation::Observed {
                    hash: remote.clone()
                })
            );
            // Controls take the observed API ID, never the prepared full v2 hash.
            let operation = client.operation(Uuid::new_v4()).unwrap();
            qbittorrent::control(
                &operation,
                &settings,
                Some(&credentials),
                &if movie {
                    MediaTarget::Movie(7)
                } else {
                    MediaTarget::Episode(7)
                },
                &remote,
                Control::Pause,
            )
            .await
            .unwrap();
            for case in 0..8 {
                let mut bad = good.clone();
                let expected = match case {
                    0 => {
                        bad["category"] = json!(if movie { "tv" } else { "movies" });
                        QbitError::ScopeConflict
                    }
                    1 => {
                        bad.as_object_mut().unwrap().remove("infohash_v2");
                        QbitError::UnsupportedFeature
                    }
                    2 => {
                        bad["infohash_v2"] = Value::Null;
                        QbitError::InvalidResponse
                    }
                    3 => {
                        bad["infohash_v2"] = json!("not-a-hash");
                        QbitError::InvalidResponse
                    }
                    4 => {
                        bad["infohash_v1"] = json!("c".repeat(64));
                        QbitError::InvalidResponse
                    }
                    5 => QbitError::InvalidResponse,
                    7 => {
                        bad["infohash_v1"] = json!("");
                        bad["infohash_v2"] = json!("");
                        QbitError::InvalidResponse
                    }
                    _ => QbitError::UnsupportedFeature,
                };
                f.0.lock().unwrap().items = match case {
                    5 => vec![bad.clone(), bad],
                    6 => vec![bad; 500],
                    _ => vec![bad],
                };
                let operation = client.operation(Uuid::new_v4()).unwrap();
                assert_eq!(
                    qbittorrent::reconcile(
                        &operation,
                        &settings,
                        Some(&credentials),
                        prepared.identity()
                    )
                    .await,
                    Err(expected),
                    "case {case}, hybrid {hybrid}"
                );
            }
            if hybrid {
                for conflicting_v1 in [false, true] {
                    let mut bad = good.clone();
                    bad[if conflicting_v1 {
                        "infohash_v1"
                    } else {
                        "infohash_v2"
                    }] = json!("c".repeat(if conflicting_v1 { 40 } else { 64 }));
                    f.0.lock().unwrap().items = vec![bad];
                    let operation = client.operation(Uuid::new_v4()).unwrap();
                    assert_eq!(
                        qbittorrent::reconcile(
                            &operation,
                            &settings,
                            Some(&credentials),
                            prepared.identity()
                        )
                        .await,
                        Err(QbitError::InvalidResponse)
                    );
                }
            }
            f.0.lock().unwrap().version = "2.8.3".into();
            let operation = client.operation(Uuid::new_v4()).unwrap();
            assert_eq!(
                qbittorrent::reconcile(
                    &operation,
                    &settings,
                    Some(&credentials),
                    prepared.identity()
                )
                .await,
                Err(QbitError::UnsupportedFeature)
            );
            f.0.lock().unwrap().version = "2.8.4".into();
            f.0.lock().unwrap().items = vec![good];
            let operation = client.operation(Uuid::new_v4()).unwrap();
            assert!(
                matches!(qbittorrent::submit(&operation, &settings, Some(&credentials), prepared).await,
                Ok(AddOutcome::AlreadyPresent { hash }) if hash == remote)
            );
        }
        assert!(
            f.0.lock()
                .unwrap()
                .calls
                .iter()
                .all(|(path, _, _)| !path.ends_with("/add"))
        );
        task.abort();
        let _ = task.await;
    }
}

#[tokio::test]
async fn hybrid_v1_lookup_and_lost_response_reconcile_using_observed_api_id() {
    for movie in [false, true] {
        let (f, url, task) = fixture(false).await;
        let settings = settings(url);
        let credentials = credentials();
        let client = HttpClient::new().unwrap();
        let v1 = "a".repeat(40);
        let v2 = "b".repeat(64);
        let remote = "b".repeat(40);
        let operation = client.operation(Uuid::new_v4()).unwrap();
        let prepared = qbittorrent::prepare(&operation, &settings,
            if movie { MediaTarget::Movie(7) } else { MediaTarget::Episode(7) },
            AddSource::Magnet(format!("magnet:?xt=urn:btih:{v1}&xt=urn:btmh:1220{v2}&tr=https%3A%2F%2Ftracker.invalid%2Fannounce")),
            false, QbitOptions::default()).await.unwrap();
        let identity: qbittorrent::SubmissionIdentity =
            serde_json::from_value(serde_json::to_value(prepared.identity()).unwrap()).unwrap();
        {
            let mut state = f.0.lock().unwrap();
            state.items.clear();
            state.uploaded_hash = Some(remote.clone());
            state.uploaded_aliases = Some((v1.clone(), v2.clone()));
            state.unknown_add = true;
        }
        let operation = client.operation(Uuid::new_v4()).unwrap();
        assert!(matches!(
            qbittorrent::submit(&operation, &settings, Some(&credentials), prepared).await,
            Err(QbitError::MutationUnknown)
        ));
        let operation = client.operation(Uuid::new_v4()).unwrap();
        assert_eq!(
            qbittorrent::reconcile(&operation, &settings, Some(&credentials), &identity).await,
            Ok(qbittorrent::Reconciliation::Observed {
                hash: remote.clone()
            })
        );
        let operation = client.operation(Uuid::new_v4()).unwrap();
        let v1_only = qbittorrent::prepare(
            &operation,
            &settings,
            if movie {
                MediaTarget::Movie(7)
            } else {
                MediaTarget::Episode(7)
            },
            AddSource::Magnet(format!("magnet:?xt=urn:btih:{v1}")),
            false,
            QbitOptions::default(),
        )
        .await
        .unwrap();
        assert_eq!(
            qbittorrent::reconcile(
                &operation,
                &settings,
                Some(&credentials),
                v1_only.identity()
            )
            .await,
            Ok(qbittorrent::Reconciliation::Observed {
                hash: remote.clone()
            })
        );
        // An API ID collision does not make contradictory aliases the intended torrent.
        for foreign in [false, true] {
            let mut collision = item(
                &v1,
                if movie ^ foreign { "movies" } else { "tv" },
                "downloading",
            );
            collision["infohash_v1"] = json!("");
            collision["infohash_v2"] = json!("c".repeat(64));
            f.0.lock().unwrap().items = vec![collision];
            let operation = client.operation(Uuid::new_v4()).unwrap();
            assert_eq!(
                qbittorrent::reconcile(
                    &operation,
                    &settings,
                    Some(&credentials),
                    v1_only.identity()
                )
                .await,
                Err(if foreign {
                    QbitError::ScopeConflict
                } else {
                    QbitError::InvalidResponse
                })
            );
        }
        let mut first = item(&v1, if movie { "movies" } else { "tv" }, "downloading");
        first["infohash_v2"] = json!("");
        let mut second = item(&remote, if movie { "movies" } else { "tv" }, "downloading");
        second["infohash_v1"] = json!("");
        second["infohash_v2"] = json!(v2);
        f.0.lock().unwrap().items = vec![first, second];
        let operation = client.operation(Uuid::new_v4()).unwrap();
        assert_eq!(
            qbittorrent::reconcile(&operation, &settings, Some(&credentials), &identity).await,
            Err(QbitError::InvalidResponse)
        );
        assert_eq!(
            f.0.lock()
                .unwrap()
                .calls
                .iter()
                .filter(|(p, _, _)| p.ends_with("/add"))
                .count(),
            1
        );
        task.abort();
        let _ = task.await;
    }
}

#[tokio::test]
async fn accepted_v2_pending_can_be_observed_later_without_resubmission() {
    for movie in [false, true] {
        let (f, url, task) = fixture(false).await;
        let settings = settings(url);
        let credentials = credentials();
        let client = HttpClient::new().unwrap();
        let v2 = "d".repeat(64);
        let remote = "d".repeat(40);
        let operation = client.operation(Uuid::new_v4()).unwrap();
        let prepared = qbittorrent::prepare(
            &operation,
            &settings,
            if movie {
                MediaTarget::Movie(7)
            } else {
                MediaTarget::Episode(7)
            },
            AddSource::Magnet(format!(
                "magnet:?xt=urn:btmh:1220{v2}&tr=https%3A%2F%2Ftracker.invalid%2Fannounce"
            )),
            false,
            QbitOptions::default(),
        )
        .await
        .unwrap();
        let identity: qbittorrent::SubmissionIdentity =
            serde_json::from_value(serde_json::to_value(prepared.identity()).unwrap()).unwrap();
        {
            let mut state = f.0.lock().unwrap();
            state.items.clear();
            state.delay_add = true;
        }
        let operation = client.operation(Uuid::new_v4()).unwrap();
        assert!(
            matches!(qbittorrent::submit(&operation, &settings, Some(&credentials), prepared).await,
            Ok(AddOutcome::Pending { hashes }) if hashes == vec![v2.clone()])
        );
        let operation = client.operation(Uuid::new_v4()).unwrap();
        assert_eq!(
            qbittorrent::reconcile(&operation, &settings, Some(&credentials), &identity).await,
            Ok(qbittorrent::Reconciliation::NotObserved)
        );
        let mut observed = item(&remote, if movie { "movies" } else { "tv" }, "downloading");
        observed["infohash_v1"] = json!("");
        observed["infohash_v2"] = json!(v2);
        f.0.lock().unwrap().items = vec![observed];
        let operation = client.operation(Uuid::new_v4()).unwrap();
        assert_eq!(
            qbittorrent::reconcile(&operation, &settings, Some(&credentials), &identity).await,
            Ok(qbittorrent::Reconciliation::Observed { hash: remote })
        );
        assert_eq!(
            f.0.lock()
                .unwrap()
                .calls
                .iter()
                .filter(|(p, _, _)| p.ends_with("/add"))
                .count(),
            1
        );
        task.abort();
        let _ = task.await;
    }
}

#[tokio::test]
async fn client_status_derives_only_active_domain_roots_without_filesystem_authority() {
    for (legacy, version, rooted) in [
        (true, "6", false),
        (false, "2.0.0", false),
        (false, "2.1.0", true),
        (false, "2.1.1", true),
    ] {
        let (f, url, task) = fixture(legacy).await;
        let settings = settings(url);
        let credentials = credentials();
        let client = HttpClient::new().unwrap();
        {
            let mut state = f.0.lock().unwrap();
            state.version = version.into();
            state.category_response = Some(if rooted {
                json!({"tv":{"savePath":"/shows"},"movies":{"savePath":"films/new"},"tv-done":{"savePath":"/must-not-use"}})
            } else {
                json!(["tv", "movies"])
            });
            state.seed_preferences = Some(
                json!({"save_path":"/downloads","max_ratio_enabled":true,"max_seeding_time_enabled":false,"max_seeding_time":-1,"max_ratio_act":1}),
            );
        }
        if legacy {
            let mut state = f.0.lock().unwrap();
            let prefs = state
                .seed_preferences
                .as_mut()
                .unwrap()
                .as_object_mut()
                .unwrap();
            prefs.remove("max_seeding_time_enabled");
            prefs.remove("max_seeding_time");
        }
        for domain in [MediaDomain::Tv, MediaDomain::Movies] {
            let operation = client.operation(Uuid::new_v4()).unwrap();
            let status =
                qbittorrent::client_status(&operation, &settings, Some(&credentials), domain)
                    .await
                    .unwrap();
            assert_eq!(status.domain, domain);
            assert_eq!(
                status.remote_root,
                if !rooted {
                    "/downloads"
                } else if domain == MediaDomain::Tv {
                    "/shows"
                } else {
                    "/downloads/films/new"
                }
            );
            assert_eq!(status.locality, qbittorrent::EndpointLocality::Loopback);
            assert!(status.removes_completed_downloads);
            assert_eq!(
                status.root_source,
                if rooted {
                    qbittorrent::RootSource::Category
                } else {
                    qbittorrent::RootSource::Default
                }
            );
        }
        if rooted {
            f.0.lock().unwrap().category_response = Some(
                json!({"tv":{"savePath":"//server/share/shows"},"movies":{"savePath":"films"}}),
            );
            f.0.lock().unwrap().seed_preferences.as_mut().unwrap()["save_path"] =
                json!(r"C:\downloads");
            for (domain, expected) in [
                (MediaDomain::Tv, "//server/share/shows"),
                (MediaDomain::Movies, r"C:\downloads\films"),
            ] {
                let operation = client.operation(Uuid::new_v4()).unwrap();
                assert_eq!(
                    qbittorrent::client_status(&operation, &settings, Some(&credentials), domain)
                        .await
                        .unwrap()
                        .remote_root,
                    expected
                );
            }
            f.0.lock().unwrap().seed_preferences.as_mut().unwrap()["save_path"] =
                json!("/downloads");
            let mut nested = settings.clone();
            if let ProviderSettings::Qbittorrent { tv: Some(tv), .. } = &mut nested {
                tv.category = "tv/nested".into();
            }
            f.0.lock().unwrap().category_response = Some(
                json!({"tv":{"savePath":"/parent"},"tv/nested":{"savePath":"/exact"},"tv-done":{"savePath":"/imported"}}),
            );
            let operation = client.operation(Uuid::new_v4()).unwrap();
            assert_eq!(
                qbittorrent::client_status(
                    &operation,
                    &nested,
                    Some(&credentials),
                    MediaDomain::Tv
                )
                .await
                .unwrap()
                .remote_root,
                "/exact"
            );
            f.0.lock().unwrap().category_response = Some(json!({"tv":{"savePath":"/parent"}}));
            let operation = client.operation(Uuid::new_v4()).unwrap();
            assert_eq!(
                qbittorrent::client_status(
                    &operation,
                    &nested,
                    Some(&credentials),
                    MediaDomain::Tv
                )
                .await
                .unwrap()
                .remote_root,
                "/downloads"
            );
        }
        if rooted {
            for categories in [json!({"tv":{"savePath":"  "},"movies":{}}), json!({})] {
                f.0.lock().unwrap().category_response = Some(categories);
                for domain in [MediaDomain::Tv, MediaDomain::Movies] {
                    let operation = client.operation(Uuid::new_v4()).unwrap();
                    assert_eq!(
                        qbittorrent::client_status(
                            &operation,
                            &settings,
                            Some(&credentials),
                            domain
                        )
                        .await
                        .unwrap()
                        .remote_root,
                        "/downloads"
                    );
                }
            }
            f.0.lock().unwrap().category_response = Some(json!({"tv":{"savePath":null}}));
            let operation = client.operation(Uuid::new_v4()).unwrap();
            assert!(matches!(
                qbittorrent::client_status(
                    &operation,
                    &settings,
                    Some(&credentials),
                    MediaDomain::Tv
                )
                .await,
                Err(QbitError::InvalidResponse)
            ));
        }
        let state = f.0.lock().unwrap();
        assert!(state.calls.iter().all(|(p, _, _)| !p.ends_with("/add")
            && !p.ends_with("setPreferences")
            && !p.ends_with("torrents/info")));
        drop(state);
        if version == "2.1.1" {
            // Predicted category roots never substitute for actual torrent paths, even with autoTMM off.
            f.0.lock().unwrap().items = vec![
                item(TV_HASH, "tv", "downloading"),
                item(MOVIE_HASH, "movies", "downloading"),
            ];
            for (target, hash) in [
                (MediaTarget::Episode(1), TV_HASH),
                (MediaTarget::Movie(1), MOVIE_HASH),
            ] {
                let operation = client.operation(Uuid::new_v4()).unwrap();
                let details =
                    qbittorrent::details(&operation, &settings, Some(&credentials), &target, hash)
                        .await
                        .unwrap();
                assert_eq!(details.save_path, "/remote");
                assert_eq!(details.content_path.as_deref(), Some("/remote/folder"));
            }
        }
        task.abort();
        let _ = task.await;
    }
}

#[tokio::test]
async fn tv_tags_use_versioned_additive_wire_and_observed_effects_for_both_payloads() {
    for version in ["2.2.1", "2.3.0", "2.6.1", "2.6.2"] {
        for multipart in [false, true] {
            let (f, url, task) = fixture(false).await;
            let mut settings = settings(url);
            if let ProviderSettings::Qbittorrent {
                tv: Some(scope), ..
            } = &mut settings
            {
                scope.add_tags = true;
            }
            let credentials = credentials();
            let client = HttpClient::new().unwrap();
            let operation = client.operation(Uuid::new_v4()).unwrap();
            let source = if multipart {
                AddSource::Torrent { filename: "show.torrent".into(), bytes: b"d4:infod6:lengthi1e4:name1:x12:piece lengthi16384e6:pieces20:12345678901234567890ee".to_vec() }
            } else {
                AddSource::Magnet(format!("magnet:?xt=urn:btih:{TV_HASH}"))
            };
            let prepared = qbittorrent::prepare(
                &operation,
                &settings,
                MediaTarget::Episode(1),
                source,
                false,
                QbitOptions {
                    tags: vec!["series tag".into(), "genre".into()],
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            {
                let mut state = f.0.lock().unwrap();
                state.version = version.into();
                state.items.clear();
                state.uploaded_hash = Some(prepared.identity().hashes()[0].clone());
            }
            let operation = client.operation(Uuid::new_v4()).unwrap();
            let result =
                qbittorrent::submit(&operation, &settings, Some(&credentials), prepared).await;
            let state = f.0.lock().unwrap();
            if version == "2.2.1" {
                assert!(matches!(result, Err(QbitError::UnsupportedFeature)));
                assert!(
                    state
                        .calls
                        .iter()
                        .all(|(p, _, _)| !p.ends_with("/add") && !p.ends_with("addTags"))
                );
            } else {
                assert!(matches!(result, Ok(AddOutcome::Observed { .. })));
                let tags = state.items[0]["tags"].as_str().unwrap();
                assert!(
                    tags.contains("external")
                        && tags.contains("series tag")
                        && tags.contains("genre")
                );
                let followups = state
                    .calls
                    .iter()
                    .filter(|(p, _, _)| p.ends_with("addTags"))
                    .collect::<Vec<_>>();
                assert_eq!(followups.len(), usize::from(version != "2.6.2"));
                if let Some((_, _, body)) = followups.first() {
                    assert!(body.contains("hashes=") && !body.contains("all"));
                }
                let add = state
                    .calls
                    .iter()
                    .find(|(p, _, _)| p.ends_with("/add"))
                    .unwrap();
                assert_eq!(
                    add.2
                        .contains(if multipart { "name=\"tags\"" } else { "tags=" }),
                    version == "2.6.2"
                );
                assert_eq!(add.2.contains("filename=\"show.torrent\""), multipart);
            }
            drop(state);
            task.abort();
            let _ = task.await;
        }
    }
}

#[tokio::test]
async fn tags_fail_closed_and_do_not_mutate_existing_or_movie_torrents() {
    for version in ["2.3.0", "2.6.2"] {
        for fault in 0..5 {
            let (f, url, task) = fixture(false).await;
            let mut settings = settings(url);
            if let ProviderSettings::Qbittorrent {
                tv: Some(scope), ..
            } = &mut settings
            {
                scope.add_tags = true;
            }
            let credentials = credentials();
            let client = HttpClient::new().unwrap();
            {
                let mut state = f.0.lock().unwrap();
                state.items.clear();
                state.version = version.into();
                state.ignore_mutations = fault == 0;
                state.tag_fault = fault.min(3);
                if fault == 4 {
                    state.added_category = Some("movies".into());
                }
            }
            let operation = client.operation(Uuid::new_v4()).unwrap();
            let result = submit_source(
                &operation,
                &settings,
                Some(&credentials),
                &MediaTarget::Episode(1),
                AddSource::Magnet(format!("magnet:?xt=urn:btih:{TV_HASH}")),
                false,
                QbitOptions {
                    tags: vec!["series tag".into()],
                    ..Default::default()
                },
            )
            .await;
            assert!(matches!(result, Err(QbitError::MutationUnknown)));
            let state = f.0.lock().unwrap();
            assert_eq!(
                state
                    .calls
                    .iter()
                    .filter(|(p, _, _)| p.ends_with("/add"))
                    .count(),
                1
            );
            assert_eq!(
                state
                    .calls
                    .iter()
                    .filter(|(p, _, _)| p.ends_with("addTags"))
                    .count(),
                usize::from(version == "2.3.0" && fault != 4)
            );
            drop(state);
            task.abort();
            let _ = task.await;
        }
        for existing in [false, true] {
            let (f, url, task) = fixture(false).await;
            let mut settings = settings(url);
            if let ProviderSettings::Qbittorrent {
                tv: Some(scope), ..
            } = &mut settings
            {
                scope.add_tags = true;
            }
            let credentials = credentials();
            let client = HttpClient::new().unwrap();
            {
                let mut state = f.0.lock().unwrap();
                state.version = version.into();
                state.items = if existing {
                    vec![item(TV_HASH, "tv", "downloading")]
                } else {
                    vec![]
                };
            }
            let operation = client.operation(Uuid::new_v4()).unwrap();
            let result = submit_source(
                &operation,
                &settings,
                Some(&credentials),
                &if existing {
                    MediaTarget::Episode(1)
                } else {
                    MediaTarget::Movie(1)
                },
                AddSource::Magnet(format!(
                    "magnet:?xt=urn:btih:{}",
                    if existing { TV_HASH } else { MOVIE_HASH }
                )),
                false,
                QbitOptions {
                    tags: vec!["series tag".into()],
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            assert!(if existing {
                matches!(result, AddOutcome::AlreadyPresent { .. })
            } else {
                matches!(result, AddOutcome::Observed { .. })
            });
            let state = f.0.lock().unwrap();
            assert!(
                state
                    .calls
                    .iter()
                    .all(|(p, _, body)| !p.ends_with("addTags")
                        && !(p.ends_with("/add") && body.contains("tags=")))
            );
            assert_eq!(state.items[0]["tags"], "external");
            drop(state);
            task.abort();
            let _ = task.await;
        }
    }
    let (f, url, task) = fixture(false).await;
    let settings = settings(url);
    let client = HttpClient::new().unwrap();
    let operation = client.operation(Uuid::new_v4()).unwrap();
    for tag in [" ", " leading", "trailing ", "comma,tag"] {
        assert!(matches!(
            qbittorrent::prepare(
                &operation,
                &settings,
                MediaTarget::Episode(1),
                AddSource::Magnet(format!("magnet:?xt=urn:btih:{TV_HASH}")),
                false,
                QbitOptions {
                    tags: vec![tag.into()],
                    ..Default::default()
                }
            )
            .await,
            Err(QbitError::InvalidRequest)
        ));
    }
    assert!(f.0.lock().unwrap().calls.is_empty());
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn legacy_14_15_submit_atomic_paused_scope_for_magnets_and_uploads() {
    use hrrdarr::providers::DownloadInitialState;
    for version in ["14", "15"] {
        for movie in [false, true] {
            for multipart in [false, true] {
                for initial in [
                    DownloadInitialState::Started,
                    DownloadInitialState::Stopped,
                    DownloadInitialState::Forced,
                ] {
                    let (f, url, task) = fixture(true).await;
                    let mut settings = settings(url);
                    if let ProviderSettings::Qbittorrent { tv, movies, .. } = &mut settings {
                        let scope = if movie { movies } else { tv }.as_mut().unwrap();
                        scope.initial_state = initial;
                        scope.older_priority = if initial == DownloadInitialState::Forced {
                            1
                        } else {
                            0
                        };
                    }
                    let credentials = credentials();
                    let client = HttpClient::new().unwrap();
                    let operation = client.operation(Uuid::new_v4()).unwrap();
                    let source = if multipart {
                        AddSource::Torrent {filename:"media.torrent".into(),bytes:b"d4:infod6:lengthi1e4:name1:x12:piece lengthi16384e6:pieces20:12345678901234567890ee".to_vec()}
                    } else {
                        AddSource::Magnet(format!(
                            "magnet:?xt=urn:btih:{}",
                            if movie { MOVIE_HASH } else { TV_HASH }
                        ))
                    };
                    let prepared = qbittorrent::prepare(
                        &operation,
                        &settings,
                        if movie {
                            MediaTarget::Movie(1)
                        } else {
                            MediaTarget::Episode(1)
                        },
                        source,
                        false,
                        QbitOptions::default(),
                    )
                    .await
                    .unwrap();
                    let expected = prepared.identity().hashes()[0].clone();
                    {
                        let mut state = f.0.lock().unwrap();
                        state.version = version.into();
                        state.items.clear();
                        state.uploaded_hash = Some(expected.clone());
                    }
                    let operation = client.operation(Uuid::new_v4()).unwrap();
                    assert_eq!(
                        qbittorrent::submit(&operation, &settings, Some(&credentials), prepared)
                            .await
                            .unwrap(),
                        AddOutcome::Observed { hash: expected }
                    );
                    let state = f.0.lock().unwrap();
                    let add = state
                        .calls
                        .iter()
                        .find(|(p, _, _)| {
                            p == if multipart {
                                "/command/upload"
                            } else {
                                "/command/download"
                            }
                        })
                        .unwrap();
                    for absent in [
                        "root_folder",
                        "contentLayout",
                        "sequentialDownload",
                        "firstLastPiecePrio",
                        "ratioLimit",
                        "tags",
                    ] {
                        assert!(!add.2.contains(absent));
                    }
                    assert_eq!(
                        state.items[0]["category"],
                        if movie { "movies" } else { "tv" }
                    );
                    assert_eq!(
                        state.items[0]["state"],
                        if initial == DownloadInitialState::Stopped {
                            "pausedDL"
                        } else {
                            "downloading"
                        }
                    );
                    assert_eq!(
                        state.items[0]["force_start"],
                        initial == DownloadInitialState::Forced
                    );
                    assert_eq!(
                        state
                            .calls
                            .iter()
                            .filter(|(p, _, _)| p.ends_with("topPrio"))
                            .count(),
                        usize::from(initial == DownloadInitialState::Forced)
                    );
                    assert!(
                        state
                            .calls
                            .iter()
                            .all(|(p, _, _)| !p.ends_with("/pause") && !p.ends_with("/resume"))
                    );
                    drop(state);
                    task.abort();
                    let _ = task.await;
                }
            }
        }
    }
}

#[tokio::test]
async fn legacy_14_15_options_reject_before_effects_and_unknown_adds_never_retry() {
    use hrrdarr::providers::{DownloadContentLayout, DownloadInitialState};
    for version in ["13", "14", "15"] {
        for feature in [
            "layout",
            "original",
            "sequential",
            "first_last",
            "share",
            "tags",
        ] {
            let (f, url, task) = fixture(true).await;
            let mut settings = settings(url);
            if let ProviderSettings::Qbittorrent {
                tv: Some(scope), ..
            } = &mut settings
            {
                match feature {
                    "layout" => scope.content_layout = DownloadContentLayout::Subfolder,
                    "original" => scope.content_layout = DownloadContentLayout::Original,
                    "sequential" => scope.sequential_order = true,
                    "first_last" => scope.first_last_first = true,
                    "tags" => scope.add_tags = true,
                    _ => {}
                }
            }
            f.0.lock().unwrap().version = version.into();
            let credentials = credentials();
            let client = HttpClient::new().unwrap();
            let operation = client.operation(Uuid::new_v4()).unwrap();
            let options = QbitOptions {
                ratio_limit: if feature == "share" { Some(2.0) } else { None },
                tags: if feature == "tags" {
                    vec!["show".into()]
                } else {
                    vec![]
                },
                ..Default::default()
            };
            assert!(matches!(
                submit_source(
                    &operation,
                    &settings,
                    Some(&credentials),
                    &MediaTarget::Episode(1),
                    AddSource::Magnet(format!("magnet:?xt=urn:btih:{TV_HASH}")),
                    false,
                    options
                )
                .await,
                Err(QbitError::UnsupportedFeature)
            ));
            assert!(
                f.0.lock()
                    .unwrap()
                    .calls
                    .iter()
                    .all(|(p, _, _)| !p.starts_with("/command/"))
            );
            task.abort();
            let _ = task.await;
        }
    }
    for movie in [false, true] {
        for fault in ["lost", "paused", "started", "metadata", "scope"] {
            let (f, url, task) = fixture(true).await;
            let mut settings = settings(url);
            if let ProviderSettings::Qbittorrent { tv, movies, .. } = &mut settings {
                if fault == "paused" {
                    if movie { movies } else { tv }
                        .as_mut()
                        .unwrap()
                        .initial_state = DownloadInitialState::Stopped;
                }
            }
            {
                let mut state = f.0.lock().unwrap();
                state.version = "14".into();
                state.items.clear();
                state.unknown_add = fault == "lost";
                state.ignored_paused = matches!(fault, "paused" | "started");
                state.missing_first_last = fault == "metadata";
                if fault == "scope" {
                    state.added_category = Some(if movie { "tv" } else { "movies" }.into());
                }
            }
            let credentials = credentials();
            let client = HttpClient::new().unwrap();
            let operation = client.operation(Uuid::new_v4()).unwrap();
            assert!(matches!(
                submit_source(
                    &operation,
                    &settings,
                    Some(&credentials),
                    &if movie {
                        MediaTarget::Movie(1)
                    } else {
                        MediaTarget::Episode(1)
                    },
                    AddSource::Magnet(format!(
                        "magnet:?xt=urn:btih:{}",
                        if movie { MOVIE_HASH } else { TV_HASH }
                    )),
                    false,
                    QbitOptions::default()
                )
                .await,
                Err(QbitError::MutationUnknown)
            ));
            assert_eq!(
                f.0.lock()
                    .unwrap()
                    .calls
                    .iter()
                    .filter(|(p, _, _)| p == "/command/download")
                    .count(),
                1
            );
            task.abort();
            let _ = task.await;
        }
    }
}

#[tokio::test]
async fn modern_started_submission_also_requires_observed_running_state() {
    for movie in [false, true] {
        let (f, url, task) = fixture(false).await;
        let settings = settings(url);
        let credentials = credentials();
        let client = HttpClient::new().unwrap();
        {
            let mut state = f.0.lock().unwrap();
            state.items.clear();
            state.ignored_paused = true;
        }
        let operation = client.operation(Uuid::new_v4()).unwrap();
        assert!(matches!(
            submit_source(
                &operation,
                &settings,
                Some(&credentials),
                &if movie {
                    MediaTarget::Movie(1)
                } else {
                    MediaTarget::Episode(1)
                },
                AddSource::Magnet(format!(
                    "magnet:?xt=urn:btih:{}",
                    if movie { MOVIE_HASH } else { TV_HASH }
                )),
                false,
                QbitOptions::default()
            )
            .await,
            Err(QbitError::MutationUnknown)
        ));
        assert_eq!(
            f.0.lock()
                .unwrap()
                .calls
                .iter()
                .filter(|(p, _, _)| p.ends_with("/add"))
                .count(),
            1
        );
        task.abort();
        let _ = task.await;
    }
}

#[tokio::test]
async fn earlier_legacy_wire_scopes_apply_to_submit_query_and_imported_moves() {
    for version in ["7", "9", "10", "13"] {
        for movie in [false, true] {
            for multipart in [false, true] {
                let (f, url, task) = fixture(true).await;
                let mut settings = settings(url);
                if let ProviderSettings::Qbittorrent {
                    movies: Some(scope),
                    ..
                } = &mut settings
                {
                    scope.imported_category = Some("movies-done".into());
                }
                let credentials = credentials();
                let client = HttpClient::new().unwrap();
                let operation = client.operation(Uuid::new_v4()).unwrap();
                let source = if multipart {
                    AddSource::Torrent{filename:"media.torrent".into(),bytes:b"d4:infod6:lengthi1e4:name1:x12:piece lengthi16384e6:pieces20:12345678901234567890ee".to_vec()}
                } else {
                    AddSource::Magnet(format!(
                        "magnet:?xt=urn:btih:{}",
                        if movie { MOVIE_HASH } else { TV_HASH }
                    ))
                };
                let target = if movie {
                    MediaTarget::Movie(1)
                } else {
                    MediaTarget::Episode(1)
                };
                let prepared = qbittorrent::prepare(
                    &operation,
                    &settings,
                    target,
                    source,
                    false,
                    QbitOptions::default(),
                )
                .await
                .unwrap();
                let hash = prepared.identity().hashes()[0].clone();
                {
                    let mut state = f.0.lock().unwrap();
                    state.version = version.into();
                    state.items.clear();
                    state.uploaded_hash = Some(hash.clone());
                    state.category_response = Some(json!([]));
                }
                let operation = client.operation(Uuid::new_v4()).unwrap();
                assert_eq!(
                    qbittorrent::submit(&operation, &settings, Some(&credentials), prepared)
                        .await
                        .unwrap(),
                    AddOutcome::Observed { hash: hash.clone() }
                );
                let operation = client.operation(Uuid::new_v4()).unwrap();
                let page = qbittorrent::query(
                    &operation,
                    &settings,
                    Some(&credentials),
                    &DownloadQuery {
                        domain: if movie {
                            MediaDomain::Movies
                        } else {
                            MediaDomain::Tv
                        },
                        offset: 0,
                        limit: 10,
                        imported: false,
                    },
                )
                .await
                .unwrap();
                assert_eq!(page.items.len(), 1);
                {
                    let mut state = f.0.lock().unwrap();
                    let row = &mut state.items[0];
                    row["state"] = json!("pausedUP");
                    row["progress"] = json!(1.0);
                    row["amount_left"] = json!(0);
                }
                let operation = client.operation(Uuid::new_v4()).unwrap();
                qbittorrent::control(
                    &operation,
                    &settings,
                    Some(&credentials),
                    &if movie {
                        MediaTarget::Movie(1)
                    } else {
                        MediaTarget::Episode(1)
                    },
                    &hash,
                    Control::MarkImported,
                )
                .await
                .unwrap();
                let state = f.0.lock().unwrap();
                let labels = matches!(version, "7" | "9");
                let add = state
                    .calls
                    .iter()
                    .find(|(p, _, _)| {
                        p == if multipart {
                            "/command/upload"
                        } else {
                            "/command/download"
                        }
                    })
                    .unwrap();
                assert!(!add.2.contains("paused") && !add.2.contains("sequentialDownload"));
                assert!(add.2.contains(if multipart {
                    if labels {
                        "name=\"label\""
                    } else {
                        "name=\"category\""
                    }
                } else if labels {
                    "label="
                } else {
                    "category="
                }));
                assert_eq!(
                    state
                        .calls
                        .iter()
                        .filter(|(p, _, _)| p == "/command/addCategory")
                        .count(),
                    // Active and imported categories must both be provisioned before their respective mutations.
                    2 * usize::from(!labels)
                );
                assert!(state.calls.iter().any(|(p, q, _)| p == "/query/torrents"
                    && q.contains(if labels { "label=" } else { "category=" })));
                assert!(state.calls.iter().any(|(p, _, _)| p
                    == if labels {
                        "/command/setLabel"
                    } else {
                        "/command/setCategory"
                    }));
                assert_eq!(
                    state.items[0]["category"],
                    if movie { "movies-done" } else { "tv-done" }
                );
                drop(state);
                task.abort();
                let _ = task.await;
            }
        }
    }
}

#[tokio::test]
async fn earlier_legacy_hidden_paused_defaults_and_scope_failures_are_not_retried() {
    use hrrdarr::providers::DownloadInitialState;
    for version in ["7", "13"] {
        for movie in [false, true] {
            for fault in ["stopped", "default_paused", "lost", "foreign"] {
                let (f, url, task) = fixture(true).await;
                let mut settings = settings(url);
                if fault == "stopped" {
                    if let ProviderSettings::Qbittorrent { tv, movies, .. } = &mut settings {
                        if movie { movies } else { tv }
                            .as_mut()
                            .unwrap()
                            .initial_state = DownloadInitialState::Stopped;
                    }
                }
                {
                    let mut state = f.0.lock().unwrap();
                    state.version = version.into();
                    state.items.clear();
                    state.ignored_paused = fault == "default_paused";
                    state.unknown_add = fault == "lost";
                    if fault == "foreign" {
                        state.added_category = Some(if movie { "tv" } else { "movies" }.into());
                    }
                }
                let credentials = credentials();
                let client = HttpClient::new().unwrap();
                let operation = client.operation(Uuid::new_v4()).unwrap();
                let result = submit_source(
                    &operation,
                    &settings,
                    Some(&credentials),
                    &if movie {
                        MediaTarget::Movie(1)
                    } else {
                        MediaTarget::Episode(1)
                    },
                    AddSource::Magnet(format!(
                        "magnet:?xt=urn:btih:{}",
                        if movie { MOVIE_HASH } else { TV_HASH }
                    )),
                    false,
                    QbitOptions::default(),
                )
                .await;
                assert_eq!(
                    result,
                    Err(if fault == "stopped" {
                        QbitError::UnsupportedFeature
                    } else {
                        QbitError::MutationUnknown
                    })
                );
                let state = f.0.lock().unwrap();
                assert_eq!(
                    state
                        .calls
                        .iter()
                        .filter(|(p, _, _)| p == "/command/download")
                        .count(),
                    usize::from(fault != "stopped")
                );
                assert!(
                    state
                        .calls
                        .iter()
                        .all(|(p, _, _)| !p.ends_with("/pause") && !p.ends_with("/resume"))
                );
                drop(state);
                task.abort();
                let _ = task.await;
            }
        }
    }
}

#[tokio::test]
async fn negotiated_scope_field_is_required_and_contradictions_never_authorize_mutation() {
    for legacy in [false, true] {
        for movie in [false, true] {
            let (f, url, task) = fixture(legacy).await;
            let settings = settings(url);
            {
                let mut state = f.0.lock().unwrap();
                state.version = if legacy { "7" } else { "2.11.0" }.into();
                state.scope_fault = true;
                state.items = vec![item(
                    if movie { MOVIE_HASH } else { TV_HASH },
                    if movie { "movies" } else { "tv" },
                    "downloading",
                )];
            }
            let credentials = credentials();
            let client = HttpClient::new().unwrap();
            let operation = client.operation(Uuid::new_v4()).unwrap();
            assert_eq!(
                qbittorrent::control(
                    &operation,
                    &settings,
                    Some(&credentials),
                    &if movie {
                        MediaTarget::Movie(1)
                    } else {
                        MediaTarget::Episode(1)
                    },
                    if movie { MOVIE_HASH } else { TV_HASH },
                    Control::Pause
                )
                .await,
                Err(QbitError::InvalidResponse)
            );
            assert!(
                f.0.lock()
                    .unwrap()
                    .calls
                    .iter()
                    .all(|(p, _, _)| !p.starts_with("/command/")
                        && !p.ends_with("/pause")
                        && !p.ends_with("/stop"))
            );
            task.abort();
            let _ = task.await;
        }
    }
}

#[tokio::test]
async fn earlier_legacy_forced_priority_is_observed_and_invalid_labels_reject_preflight() {
    for version in ["7", "13"] {
        for movie in [false, true] {
            let (f, url, task) = fixture(true).await;
            let mut settings = settings(url);
            if let ProviderSettings::Qbittorrent { tv, movies, .. } = &mut settings {
                let scope = if movie { movies } else { tv }.as_mut().unwrap();
                scope.initial_state = hrrdarr::providers::DownloadInitialState::Forced;
                scope.older_priority = 1;
            }
            {
                let mut state = f.0.lock().unwrap();
                state.version = version.into();
                state.items.clear();
            }
            let credentials = credentials();
            let client = HttpClient::new().unwrap();
            let operation = client.operation(Uuid::new_v4()).unwrap();
            assert!(matches!(
                submit_source(
                    &operation,
                    &settings,
                    Some(&credentials),
                    &if movie {
                        MediaTarget::Movie(1)
                    } else {
                        MediaTarget::Episode(1)
                    },
                    AddSource::Magnet(format!(
                        "magnet:?xt=urn:btih:{}",
                        if movie { MOVIE_HASH } else { TV_HASH }
                    )),
                    false,
                    QbitOptions::default()
                )
                .await,
                Ok(AddOutcome::Observed { .. })
            ));
            {
                let state = f.0.lock().unwrap();
                assert_eq!(state.items[0]["force_start"], true);
                assert_eq!(state.items[0]["priority"], 1);
            }
            task.abort();
            let _ = task.await;
        }
    }
    let (f, url, task) = fixture(true).await;
    let mut settings = settings(url);
    if let ProviderSettings::Qbittorrent {
        tv: Some(scope), ..
    } = &mut settings
    {
        scope.category = "tv/nested".into();
    }
    f.0.lock().unwrap().version = "7".into();
    let credentials = credentials();
    let client = HttpClient::new().unwrap();
    let operation = client.operation(Uuid::new_v4()).unwrap();
    assert!(matches!(
        submit_source(
            &operation,
            &settings,
            Some(&credentials),
            &MediaTarget::Episode(1),
            AddSource::Magnet(format!("magnet:?xt=urn:btih:{TV_HASH}")),
            false,
            QbitOptions::default()
        )
        .await,
        Err(QbitError::UnsupportedFeature)
    ));
    assert!(
        f.0.lock()
            .unwrap()
            .calls
            .iter()
            .all(|(p, _, _)| !p.starts_with("/command/"))
    );
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn legacy_seed_profiles_preserve_effective_ratio_and_unavailable_time_provenance() {
    use qbittorrent::{SeedLimit, SeedProvenance};
    for version in [7, 10, 15, 16, 17] {
        let (f, url, task) = fixture(true).await;
        let settings = settings(url);
        let credentials = credentials();
        let client = HttpClient::new().unwrap();
        let mut prefs = json!({"queueing_enabled":true,"dht":true,"save_path":"/remote","max_ratio_enabled":true,"max_ratio":2.0,"max_ratio_act":0});
        if version >= 16 {
            prefs["max_seeding_time_enabled"] = json!(false);
            prefs["max_seeding_time"] = json!(-1);
        }
        {
            let mut state = f.0.lock().unwrap();
            state.version = version.to_string();
            state.legacy_seed_response = true;
            state.seed_preferences = Some(prefs.clone());
            state.seed_properties = Some(json!({"save_path":"/remote","seeding_time":3600}));
            state.items = vec![
                item(TV_HASH, "tv", "pausedUP"),
                item(MOVIE_HASH, "movies", "pausedUP"),
            ];
            for row in &mut state.items {
                row["ratio"] = json!(1.6);
                if version >= 15 {
                    row["ratio_limit"] = json!(1.5);
                } else {
                    row.as_object_mut().unwrap().remove("ratio_limit");
                }
            }
        }
        let operation = client.operation(Uuid::new_v4()).unwrap();
        assert_eq!(
            qbittorrent::test_connection(&operation, &settings, Some(&credentials))
                .await
                .unwrap()
                .domains
                .len(),
            2
        );
        for (domain, target, hash) in [
            (MediaDomain::Tv, MediaTarget::Episode(1), TV_HASH),
            (MediaDomain::Movies, MediaTarget::Movie(1), MOVIE_HASH),
        ] {
            let operation = client.operation(Uuid::new_v4()).unwrap();
            let status =
                qbittorrent::client_status(&operation, &settings, Some(&credentials), domain)
                    .await
                    .unwrap();
            assert!(!status.removes_completed_downloads);
            let operation = client.operation(Uuid::new_v4()).unwrap();
            let details =
                qbittorrent::details(&operation, &settings, Some(&credentials), &target, hash)
                    .await
                    .unwrap();
            let seed = details.seed_policy;
            assert_eq!(seed.ratio.raw, SeedLimit::Unknown);
            assert_eq!(seed.ratio.global, SeedLimit::Limited(2.0));
            assert_eq!(
                seed.ratio.provenance,
                if version >= 15 {
                    SeedProvenance::ObservedEffective
                } else {
                    SeedProvenance::Unknown
                }
            );
            assert_eq!(
                seed.ratio.effective,
                if version >= 15 {
                    SeedLimit::Limited(1.5)
                } else {
                    SeedLimit::Unknown
                }
            );
            assert_eq!(seed.ready, if version >= 15 { Some(true) } else { None });
            assert_eq!(seed.elapsed_seeding_seconds, Some(3600));
            assert_eq!(
                seed.seeding_minutes.provenance,
                if version < 16 {
                    SeedProvenance::Unavailable
                } else {
                    SeedProvenance::Unknown
                }
            );
            assert_eq!(seed.inactive_minutes.raw, SeedLimit::Unavailable);
        }
        f.0.lock().unwrap().seed_preferences.as_mut().unwrap()["max_ratio_act"] = json!(1);
        let operation = client.operation(Uuid::new_v4()).unwrap();
        assert!(matches!(
            qbittorrent::test_connection(&operation, &settings, Some(&credentials)).await,
            Err(QbitError::UnsafeRetention)
        ));
        let operation = client.operation(Uuid::new_v4()).unwrap();
        assert!(
            qbittorrent::client_status(
                &operation,
                &settings,
                Some(&credentials),
                MediaDomain::Movies
            )
            .await
            .unwrap()
            .removes_completed_downloads
        );
        f.0.lock().unwrap().items[0]["ratio_limit"] = json!(-2);
        let operation = client.operation(Uuid::new_v4()).unwrap();
        assert!(matches!(
            qbittorrent::details(
                &operation,
                &settings,
                Some(&credentials),
                &MediaTarget::Episode(1),
                TV_HASH
            )
            .await,
            Err(QbitError::InvalidResponse)
        ));
        // Modern required flags cannot be fabricated on older protocols, nor omitted after the boundary.
        if version < 16 {
            prefs["max_seeding_time_enabled"] = json!(true);
            prefs["max_seeding_time"] = json!(60);
        } else {
            prefs
                .as_object_mut()
                .unwrap()
                .remove("max_seeding_time_enabled");
        }
        f.0.lock().unwrap().seed_preferences = Some(prefs);
        let operation = client.operation(Uuid::new_v4()).unwrap();
        assert!(matches!(
            qbittorrent::test_connection(&operation, &settings, Some(&credentials)).await,
            Err(QbitError::InvalidResponse)
        ));
        assert!(
            f.0.lock()
                .unwrap()
                .calls
                .iter()
                .all(|(p, _, _)| !p.starts_with("/command/"))
        );
        task.abort();
        let _ = task.await;
    }
}

#[tokio::test]
async fn layout_requests_cover_domains_payloads_and_wire_boundaries_without_effect_claims() {
    use hrrdarr::providers::DownloadContentLayout;
    for (legacy, version) in [(true, "16"), (false, "2.6.1"), (false, "2.7.0")] {
        for movie in [false, true] {
            for multipart in [false, true] {
                for layout in [
                    DownloadContentLayout::Default,
                    DownloadContentLayout::Original,
                    DownloadContentLayout::Subfolder,
                ] {
                    let (f, url, task) = fixture(legacy).await;
                    let mut settings = settings(url);
                    if let ProviderSettings::Qbittorrent { tv, movies, .. } = &mut settings {
                        if movie { movies } else { tv }
                            .as_mut()
                            .unwrap()
                            .content_layout = layout;
                    }
                    let credentials = credentials();
                    let client = HttpClient::new().unwrap();
                    let operation = client.operation(Uuid::new_v4()).unwrap();
                    let source = if multipart {
                        AddSource::Torrent{filename:"layout.torrent".into(),bytes:b"d4:infod6:lengthi1e4:name1:x12:piece lengthi16384e6:pieces20:12345678901234567890ee".to_vec()}
                    } else {
                        AddSource::Magnet(format!(
                            "magnet:?xt=urn:btih:{}",
                            if movie { MOVIE_HASH } else { TV_HASH }
                        ))
                    };
                    let prepared = qbittorrent::prepare(
                        &operation,
                        &settings,
                        if movie {
                            MediaTarget::Movie(1)
                        } else {
                            MediaTarget::Episode(1)
                        },
                        source,
                        false,
                        QbitOptions::default(),
                    )
                    .await
                    .unwrap();
                    {
                        let mut state = f.0.lock().unwrap();
                        state.version = version.into();
                        state.items.clear();
                        state.uploaded_hash = Some(prepared.identity().hashes()[0].clone());
                    }
                    let operation = client.operation(Uuid::new_v4()).unwrap();
                    let result =
                        qbittorrent::submit(&operation, &settings, Some(&credentials), prepared)
                            .await;
                    let state = f.0.lock().unwrap();
                    let unsupported =
                        layout == DownloadContentLayout::Original && version != "2.7.0";
                    if unsupported {
                        assert!(matches!(result, Err(QbitError::UnsupportedFeature)));
                        assert!(
                            state.calls.iter().all(
                                |(p, _, _)| !p.starts_with("/command/") && !p.ends_with("/add")
                            )
                        );
                    } else {
                        // The fixture deliberately does not rearrange remote files: Observed isn't a layout guarantee.
                        assert!(matches!(result, Ok(AddOutcome::Observed { .. })));
                        let add = state
                            .calls
                            .iter()
                            .find(|(p, _, _)| {
                                p.ends_with("/add")
                                    || p == "/command/download"
                                    || p == "/command/upload"
                            })
                            .unwrap();
                        let fields = if multipart {
                            add.2
                                .split("\r\n--")
                                .filter_map(|part| {
                                    let (head, value) = part.split_once("\r\n\r\n")?;
                                    if head.contains("filename=") {
                                        return None;
                                    }
                                    let name = head.split("name=\"").nth(1)?.split('"').next()?;
                                    Some((
                                        name.to_owned(),
                                        value.trim_end_matches("\r\n").to_owned(),
                                    ))
                                })
                                .collect::<std::collections::BTreeMap<_, _>>()
                        } else {
                            url::form_urlencoded::parse(add.2.as_bytes())
                                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                                .collect()
                        };
                        match layout {
                            DownloadContentLayout::Default => {
                                assert!(!fields.contains_key("root_folder"));
                                assert!(!fields.contains_key("contentLayout"));
                            }
                            DownloadContentLayout::Original => assert_eq!(
                                fields.get("contentLayout").map(String::as_str),
                                Some("Original")
                            ),
                            DownloadContentLayout::Subfolder => assert_eq!(
                                fields
                                    .get(if version == "2.7.0" {
                                        "contentLayout"
                                    } else {
                                        "root_folder"
                                    })
                                    .map(String::as_str),
                                Some(if version == "2.7.0" {
                                    "Subfolder"
                                } else {
                                    "true"
                                })
                            ),
                        }
                        assert_eq!(
                            state.items[0]["category"],
                            if movie { "movies" } else { "tv" }
                        );
                    }
                    drop(state);
                    task.abort();
                    let _ = task.await;
                }
            }
        }
    }
}

#[tokio::test]
async fn details_expose_single_multi_and_missing_metadata_paths_without_layout_inference() {
    for (legacy, version) in [(true, "15"), (false, "2.6.0"), (false, "2.6.1")] {
        for movie in [false, true] {
            for names in [
                vec!["movie.mkv"],
                vec!["folder/a.mkv", "folder/b.mkv"],
                vec!["a.mkv", "b.mkv"],
                vec![],
            ] {
                let (f, url, task) = fixture(legacy).await;
                let settings = settings(url);
                let credentials = credentials();
                let client = HttpClient::new().unwrap();
                let hash = if movie { MOVIE_HASH } else { TV_HASH };
                let category = if movie { "movies" } else { "tv" };
                {
                    let mut state = f.0.lock().unwrap();
                    state.version = version.into();
                    let mut row = item(
                        hash,
                        category,
                        if names.is_empty() {
                            "metaDL"
                        } else {
                            "downloading"
                        },
                    );
                    row["content_path"] = json!(if names.is_empty() {
                        ""
                    } else {
                        "/remote/client-reported"
                    });
                    state.items = vec![row];
                    state.file_response=Some(json!(names.iter().enumerate().map(|(i,name)|json!({"index":i,"name":name,"size":100,"progress":0.5,"priority":1})).collect::<Vec<_>>()));
                }
                let operation = client.operation(Uuid::new_v4()).unwrap();
                let details = qbittorrent::details(
                    &operation,
                    &settings,
                    Some(&credentials),
                    &if movie {
                        MediaTarget::Movie(1)
                    } else {
                        MediaTarget::Episode(1)
                    },
                    hash,
                )
                .await
                .unwrap();
                let expected = if names.is_empty() {
                    None
                } else if version == "2.6.1" {
                    Some("/remote/client-reported")
                } else if names.len() == 1 {
                    Some("/remote/movie.mkv")
                } else if names[0].starts_with("folder/") {
                    Some("/remote/folder")
                } else {
                    None
                };
                assert_eq!(details.content_path.as_deref(), expected);
                assert_eq!(details.save_path, "/remote");
                assert!(!details.item.completed);
                assert_eq!(details.files.len(), names.len());
                assert!(
                    f.0.lock()
                        .unwrap()
                        .calls
                        .iter()
                        .all(|(p, _, _)| !p.starts_with("/command/") && !p.ends_with("/add"))
                );
                task.abort();
                let _ = task.await;
            }
        }
    }
}

#[tokio::test]
async fn native_diagnostics_and_eta_preserve_unknown_facts_for_both_domains() {
    for (domain, category, hash) in [
        (MediaDomain::Tv, "tv", TV_HASH),
        (MediaDomain::Movies, "movies", MOVIE_HASH),
    ] {
        let (f, url, task) = fixture(false).await;
        let settings = settings(url);
        let credentials = credentials();
        let client = HttpClient::new().unwrap();
        for (remote, diagnostic, status) in [
            ("error", "error", "warning"),
            ("missingFiles", "missing_files", "warning"),
            ("stalledDL", "stalled", "warning"),
            ("private-remote-state", "unknown_state", "unknown"),
        ] {
            let mut row = item(hash, category, remote);
            row["error"] = json!("PRIVATE_RAW_DIAGNOSTIC");
            {
                let mut f = f.0.lock().unwrap();
                f.items = vec![row];
                f.calls.clear();
            }
            let op = client.operation(Uuid::new_v4()).unwrap();
            let page = qbittorrent::query(
                &op,
                &settings,
                Some(&credentials),
                &DownloadQuery {
                    domain,
                    offset: 0,
                    limit: 10,
                    imported: false,
                },
            )
            .await
            .unwrap();
            let public = serde_json::to_value(&page).unwrap();
            assert_eq!(public["items"][0]["diagnostic"], diagnostic);
            assert_eq!(public["items"][0]["status"], status);
            assert!(!public.to_string().contains("PRIVATE_RAW_DIAGNOSTIC"));
            assert!(!public.to_string().contains("private-remote-state"));
            assert!(
                !f.0.lock()
                    .unwrap()
                    .calls
                    .iter()
                    .any(|(p, _, _)| p.ends_with("preferences")),
                "Nonmetadata listing needs no preference request"
            );
        }
        for (prefs, expected) in [
            (json!({}), Some(("queued", "metadata"))),
            (json!({"dht":true}), Some(("queued", "metadata"))),
            (json!({"dht":false}), Some(("warning", "dht_disabled"))),
            (json!({"dht":null}), None),
            (json!({"dht":"false"}), None),
            (json!([]), None),
            (Value::Null, None),
        ] {
            {
                let mut f = f.0.lock().unwrap();
                f.items = vec![item(hash, category, "metaDL")];
                f.seed_preferences = Some(prefs);
            }
            let op = client.operation(Uuid::new_v4()).unwrap();
            let result = qbittorrent::query(
                &op,
                &settings,
                Some(&credentials),
                &DownloadQuery {
                    domain,
                    offset: 0,
                    limit: 10,
                    imported: false,
                },
            )
            .await;
            if let Some((status, diagnostic)) = expected {
                let public = serde_json::to_value(result.unwrap()).unwrap();
                assert_eq!(public["items"][0]["status"], status);
                assert_eq!(public["items"][0]["diagnostic"], diagnostic);
            } else {
                assert!(matches!(result, Err(QbitError::InvalidResponse)));
            }
            // Details already fetch preferences for seed facts; metadata diagnostics must reuse that request.
            f.0.lock().unwrap().calls.clear();
            f.0.lock().unwrap().items[0]["state"] = json!("forcedMetaDL");
            let op = client.operation(Uuid::new_v4()).unwrap();
            let target = if domain == MediaDomain::Tv {
                MediaTarget::Episode(1)
            } else {
                MediaTarget::Movie(1)
            };
            let result =
                qbittorrent::details(&op, &settings, Some(&credentials), &target, hash).await;
            if let Some((status, diagnostic)) = expected {
                let public = serde_json::to_value(result.unwrap().item).unwrap();
                assert_eq!(public["status"], status);
                assert_eq!(public["diagnostic"], diagnostic);
            } else {
                assert!(matches!(result, Err(QbitError::InvalidResponse)));
            }
            assert_eq!(
                f.0.lock()
                    .unwrap()
                    .calls
                    .iter()
                    .filter(|(p, _, _)| p.ends_with("preferences"))
                    .count(),
                1
            );
        }
        for (remote, eta, expected) in [
            ("downloading", json!(25), Some(25)),
            ("downloading", json!(8640000), None),
            ("downloading", json!(31536000), Some(31536000)),
            ("downloading", json!(31536001), None),
            ("downloading", json!(-1), None),
            ("downloading", json!("private-eta"), None),
            ("uploading", json!(8640000), Some(0)),
            ("uploading", json!(25), Some(25)),
        ] {
            let mut row = item(hash, category, remote);
            if remote == "uploading" {
                row["progress"] = json!(1.0);
                row["amount_left"] = json!(0);
            }
            if remote == "uploading" && eta == json!(25) {
                row["progress"] = json!(0.5);
                row["amount_left"] = json!(50);
            }
            row["eta"] = eta;
            f.0.lock().unwrap().items = vec![row];
            let op = client.operation(Uuid::new_v4()).unwrap();
            let page = qbittorrent::query(
                &op,
                &settings,
                Some(&credentials),
                &DownloadQuery {
                    domain,
                    offset: 0,
                    limit: 10,
                    imported: false,
                },
            )
            .await
            .unwrap();
            assert_eq!(page.items[0].eta_seconds, expected);
        }
        task.abort();
        let _ = task.await;
    }
}

#[tokio::test]
async fn imported_category_is_confirmed_before_scoped_move() {
    for movie in [false, true] {
        for fault in 0..8 {
            let (f, url, task) = fixture(fault == 7).await;
            let mut settings = settings(url);
            if let ProviderSettings::Qbittorrent {
                movies: Some(scope),
                ..
            } = &mut settings
            {
                scope.imported_category = Some("movies-done".into());
            }
            let (hash, category, target, imported) = if movie {
                (MOVIE_HASH, "movies", MediaTarget::Movie(1), "movies-done")
            } else {
                (TV_HASH, "tv", MediaTarget::Episode(1), "tv-done")
            };
            {
                let mut f = f.0.lock().unwrap();
                f.category_response = Some(if fault == 7 {
                    json!(["tv", "movies"])
                } else {
                    json!({"tv":{},"movies":{}})
                });
                if fault == 7 {
                    f.version = "16".into();
                }
                f.items = vec![item(
                    hash,
                    if fault == 3 {
                        if movie { "tv" } else { "movies" }
                    } else {
                        category
                    },
                    "pausedUP",
                )];
                if fault == 4 {
                    f.items[0]["progress"] = json!(0.5);
                    f.items[0]["amount_left"] = json!(50);
                }
                f.category_create_fault = match fault {
                    5 => 1,
                    6 => 2,
                    _ => 0,
                };
                f.ignore_mutations = fault == 1;
                f.unknown_add = fault == 2;
            }
            let credentials = credentials();
            let client = HttpClient::new().unwrap();
            let op = client.operation(Uuid::new_v4()).unwrap();
            let result = qbittorrent::control(
                &op,
                &settings,
                Some(&credentials),
                &target,
                hash,
                Control::MarkImported,
            )
            .await;
            let state = f.0.lock().unwrap();
            let moves: Vec<_> = state
                .calls
                .iter()
                .filter(|(p, _, _)| p.ends_with("setCategory"))
                .collect();
            if fault == 0 || fault == 7 {
                result.unwrap();
                assert_eq!(moves.len(), 1);
                assert!(moves[0].2.contains(&format!("category={imported}")));
                assert_eq!(state.items[0]["category"], imported);
                let create = state
                    .calls
                    .iter()
                    .position(|(p, _, _)| {
                        p.ends_with("createCategory") || p.ends_with("addCategory")
                    })
                    .unwrap();
                let moved = state
                    .calls
                    .iter()
                    .position(|(p, _, _)| p.ends_with("setCategory"))
                    .unwrap();
                assert!(
                    state.calls[create + 1..moved]
                        .iter()
                        .any(|(p, _, _)| p.ends_with("categories") || p.ends_with("maindata"))
                );
            } else {
                if fault == 1 || fault == 2 {
                    assert!(matches!(result, Err(QbitError::MutationUnknown)));
                    assert_eq!(
                        state
                            .calls
                            .iter()
                            .filter(|(p, _, _)| p.ends_with("createCategory"))
                            .count(),
                        1,
                        "An uncertain create is never replayed"
                    );
                } else {
                    assert!(result.is_err());
                }
                assert!(
                    moves.is_empty(),
                    "Unconfirmed creation or foreign scope cannot authorize a move"
                );
                if fault == 3 || fault == 4 {
                    assert!(
                        !state
                            .calls
                            .iter()
                            .any(|(p, _, _)| p.ends_with("createCategory"))
                    );
                }
            }
            drop(state);
            task.abort();
            let _ = task.await;
        }
    }
}

#[tokio::test]
async fn refresh_pages_share_auth_redaction_and_enforce_complete_bounds() {
    #[derive(Default)]
    struct Pages {
        count: usize,
        logins: usize,
        offsets: Vec<usize>,
        duplicate: bool,
        large: bool,
        denied: bool,
    }
    async fn pages(
        State(state): State<Arc<Mutex<Pages>>>,
        uri: Uri,
        headers: HeaderMap,
    ) -> axum::response::Response {
        let mut s = state.lock().unwrap();
        if uri.path().ends_with("auth/login") {
            s.logins += 1;
            let sid = if s.logins == 1 {
                "OLD_REFRESH_SID"
            } else {
                "NEW_REFRESH_SID"
            };
            return ([("set-cookie", format!("SID={sid}; HttpOnly"))], "Ok.").into_response();
        }
        if !headers.contains_key("cookie") {
            return StatusCode::FORBIDDEN.into_response();
        }
        if uri.path().ends_with("webapiVersion") {
            return "2.8.3".into_response();
        }
        assert!(uri.path().ends_with("torrents/info"));
        let args: std::collections::HashMap<_, _> =
            url::form_urlencoded::parse(uri.query().unwrap().as_bytes())
                .into_owned()
                .collect();
        let offset: usize = args["offset"].parse().unwrap();
        if offset == 500 && !s.denied {
            s.denied = true;
            return StatusCode::FORBIDDEN.into_response();
        }
        s.offsets.push(offset);
        let rows: Vec<_> = (offset..s.count.min(offset + 500))
            .map(|i| {
                let id = if s.duplicate && i == 500 { 0 } else { i };
                let mut row = item(&format!("{id:040x}"), &args["category"], "downloading");
                row["name"] = json!(if s.large {
                    "x".repeat(1024)
                } else {
                    "OLD_REFRESH_SID NEW_REFRESH_SID private-password".into()
                });
                row
            })
            .collect();
        axum::Json(rows).into_response()
    }
    for domain in [MediaDomain::Tv, MediaDomain::Movies] {
        for (count, duplicate, large) in [
            (499, false, false),
            (500, false, false),
            (1000, false, false),
            (1001, false, false),
            (501, true, false),
            (1000, false, true),
        ] {
            let state = Arc::new(Mutex::new(Pages {
                count,
                duplicate,
                large,
                ..Default::default()
            }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let app = Router::new().fallback(pages).with_state(state.clone());
            let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let client = HttpClient::new().unwrap();
            let operation = client.operation(Uuid::new_v4()).unwrap();
            let result = qbittorrent::refresh_pages(
                &operation,
                &settings(endpoint),
                Some(&credentials()),
                domain,
            )
            .await;
            if duplicate {
                assert!(matches!(result, Err(QbitError::InvalidResponse)));
            } else if count > 1000 || large {
                assert!(matches!(
                    result,
                    Err(QbitError::Http(HttpError::ResponseTooLarge))
                ));
            } else {
                let items = result.unwrap();
                assert_eq!(items.len(), count);
                let encoded = serde_json::to_string(&items).unwrap();
                assert!(!encoded.contains("OLD_REFRESH_SID"));
                // Even page-one names are redacted after the later renewal learns NEW SID.
                if count >= 500 {
                    assert!(!encoded.contains("NEW_REFRESH_SID"));
                }
                assert!(!encoded.contains("private-password"));
                let s = state.lock().unwrap();
                assert_eq!(s.logins, if count >= 500 { 2 } else { 1 });
                assert_eq!(
                    s.offsets,
                    if count == 1000 {
                        vec![0, 500, 1000]
                    } else if count == 500 {
                        vec![0, 500]
                    } else {
                        vec![0]
                    }
                );
            }
            task.abort();
            let _ = task.await;
        }
    }
}
