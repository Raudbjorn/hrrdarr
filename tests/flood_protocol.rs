//! Flood REST wire contract against an owned mock on 127.0.0.1:0 (never a live Flood or its
//! default port 3000). Paths, JSON shapes and the `jwt` cookie follow Flood's published REST API
//! as understood from its documentation; the mock is not recorded from a running Flood.
use axum::{
    Router,
    body::Bytes,
    extract::{OriginalUri, State},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
};
use hrrdarr::{
    api::MediaDomain,
    db::MediaTarget,
    providers::{
        Credentials, ProviderSettings, TorrentLabelScope, TorrentQueuePriority,
        http::{HttpClient, HttpError},
        label_client,
        qbittorrent::{
            AddOutcome, AddSource, Control, DownloadDiagnostic, DownloadQuery, DownloadStatus,
            QbitError, QbitOptions, Reconciliation, SubmissionIdentity,
        },
    },
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

const USER: &str = "flood-user";
const PASSWORD: &str = "PRIVATE_FLOOD_PASSWORD";
const JWT: &str = "PRIVATE_FLOOD_JWT";
const DEFAULT_DIR: &str = "/downloads";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
/// Independent RFC 4648 encoder for the uploaded `.torrent`.
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
fn torrent(name: &str) -> (Vec<u8>, String) {
    let info = format!(
        "d6:lengthi1048576e4:name{}:{name}12:piece lengthi262144e6:pieces20:{}e",
        name.len(),
        "a".repeat(20)
    );
    let hash = hex(
        ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, info.as_bytes()).as_ref(),
    );
    (
        format!("d8:announce17:http://t.invalid/4:info{info}e").into_bytes(),
        hash,
    )
}

#[derive(Clone)]
struct Tr {
    hash: String,
    name: String,
    directory: String,
    tags: Vec<String>,
    status: Vec<&'static str>,
    size: u64,
    done: u64,
    /// (path, size, percent, priority)
    files: Vec<(String, u64, f64, i64)>,
}
fn tr(hash: &str, name: &str, directory: &str, tags: &[&str], status: &[&'static str]) -> Tr {
    Tr {
        hash: hash.to_uppercase(),
        name: name.into(),
        directory: directory.into(),
        tags: tags.iter().map(|t| (*t).into()).collect(),
        status: status.to_vec(),
        size: 1048576,
        done: 524288,
        files: vec![(name.into(), 1048576, 50.0, 1)],
    }
}
struct Mock {
    auth: bool,
    connected: bool,
    torrents: Vec<Tr>,
    /// (method, path, body)
    calls: Vec<(String, String, Value)>,
    lose_add_response: bool,
    metainfo: Vec<(String, String)>,
}
#[derive(Clone)]
struct Fixture(Arc<Mutex<Mock>>);
impl Fixture {
    fn posts(&self, path: &str) -> Vec<Value> {
        self.0
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|(m, p, _)| m == "POST" && p == path)
            .map(|(_, _, b)| b.clone())
            .collect()
    }
    fn paths(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .calls
            .iter()
            .map(|(m, p, _)| format!("{m} {p}"))
            .collect()
    }
    fn clear(&self) {
        self.0.lock().unwrap().calls.clear();
    }
    fn by_hash(&self, hash: &str) -> Option<Tr> {
        self.0
            .lock()
            .unwrap()
            .torrents
            .iter()
            .find(|t| t.hash.eq_ignore_ascii_case(hash))
            .cloned()
    }
    fn put(&self, t: Tr) {
        self.0.lock().unwrap().torrents.push(t);
    }
}
fn view(t: &Tr) -> Value {
    json!({"hash":t.hash,"name":t.name,"directory":t.directory,"tags":t.tags,"status":t.status,
        "sizeBytes":t.size,"bytesDone":t.done,"percentComplete":t.done as f64*100.0/t.size.max(1) as f64,
        "downRate":2048,"upRate":64,"eta":30,"ratio":0.5})
}
async fn handler(
    State(f): State<Fixture>,
    method: Method,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let mut m = f.0.lock().unwrap();
    let path = uri.path().to_owned();
    let json_body: Value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap()
    };
    m.calls
        .push((method.to_string(), path.clone(), json_body.clone()));
    let ok = |v: Value| axum::Json(v).into_response();
    if path == "/api/auth/authenticate" {
        if json_body["username"] == USER && json_body["password"] == PASSWORD {
            return (
                [("set-cookie", format!("jwt={JWT}; Path=/; HttpOnly"))],
                ok(json!({"success":true,"username":USER,"level":10})),
            )
                .into_response();
        }
        return (
            StatusCode::UNAUTHORIZED,
            ok(json!({"message":"Failed login."})),
        )
            .into_response();
    }
    if m.auth && headers.get("cookie").and_then(|v| v.to_str().ok()) != Some(&format!("jwt={JWT}"))
    {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }
    match (method.as_str(), path.as_str()) {
        ("GET", "/api/client/connection-test") => ok(json!({"isConnected":m.connected})),
        ("GET", "/api/client/settings") => ok(json!({"directoryDefault":DEFAULT_DIR})),
        ("GET", "/api/torrents") => {
            let map: serde_json::Map<String, Value> = m
                .torrents
                .iter()
                .map(|t| (t.hash.clone(), view(t)))
                .collect();
            ok(json!({"id":1,"torrents":map}))
        }
        ("GET", p) if p.starts_with("/api/torrents/") && p.ends_with("/contents") => {
            let hash = p
                .trim_start_matches("/api/torrents/")
                .trim_end_matches("/contents");
            match m.torrents.iter().find(|t| t.hash == hash) {
                Some(t) => ok(json!(t
                    .files
                    .iter()
                    .enumerate()
                    .map(|(i, (p, s, pc, pr))| json!({"index":i,"path":p,"filename":p.rsplit('/').next(),
                        "percentComplete":pc,"priority":pr,"sizeBytes":s}))
                    .collect::<Vec<_>>())),
                None => (StatusCode::NOT_FOUND, "no torrent").into_response(),
            }
        }
        ("POST", "/api/torrents/add-urls") | ("POST", "/api/torrents/add-files") => {
            let hash = if path.ends_with("add-urls") {
                json_body["urls"][0]
                    .as_str()
                    .unwrap()
                    .split("urn:btih:")
                    .nth(1)
                    .unwrap()[..40]
                    .to_lowercase()
            } else {
                let encoded = json_body["files"][0].as_str().unwrap();
                m.metainfo
                    .iter()
                    .find(|(b, _)| b == encoded)
                    .map(|(_, h)| h.clone())
                    .expect("unknown torrent")
            };
            if m.torrents
                .iter()
                .any(|t| t.hash.eq_ignore_ascii_case(&hash))
            {
                return (StatusCode::INTERNAL_SERVER_ERROR, "duplicate").into_response();
            }
            let tags: Vec<&str> = json_body["tags"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t.as_str().unwrap())
                .collect();
            let started = json_body["start"] == true;
            let mut t = tr(
                &hash,
                "Added",
                &format!("{DEFAULT_DIR}/Added"),
                &tags,
                if started {
                    &["downloading", "active"]
                } else {
                    &["stopped", "inactive"]
                },
            );
            t.done = 0;
            m.torrents.push(t);
            if m.lose_add_response {
                return StatusCode::BAD_GATEWAY.into_response();
            }
            ok(json!({}))
        }
        ("POST", "/api/torrents/start")
        | ("POST", "/api/torrents/stop")
        | ("POST", "/api/torrents/delete") => {
            let hashes: Vec<String> = json_body["hashes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|h| h.as_str().unwrap().to_owned())
                .collect();
            for h in hashes {
                if path.ends_with("delete") {
                    m.torrents.retain(|t| t.hash != h);
                } else if let Some(t) = m.torrents.iter_mut().find(|t| t.hash == h) {
                    t.status = if path.ends_with("start") {
                        vec!["downloading", "active"]
                    } else {
                        vec!["stopped", "inactive"]
                    };
                }
            }
            ok(json!({}))
        }
        _ => (StatusCode::NOT_FOUND, "unknown").into_response(),
    }
}
async fn fixture() -> (Fixture, String, tokio::task::JoinHandle<()>) {
    let fixture = Fixture(Arc::new(Mutex::new(Mock {
        auth: true,
        connected: true,
        torrents: vec![],
        calls: vec![],
        lose_add_response: false,
        metainfo: vec![],
    })));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    assert_ne!(listener.local_addr().unwrap().port(), 3000);
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new().fallback(handler).with_state(fixture.clone());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (fixture, endpoint, task)
}
fn scope(label: &str, recent: TorrentQueuePriority, paused: bool) -> TorrentLabelScope {
    TorrentLabelScope {
        category: label.into(),
        recent_priority: recent,
        older_priority: TorrentQueuePriority::Last,
        add_paused: paused,
        tags: None,
    }
}
fn settings(endpoint: &str) -> ProviderSettings {
    ProviderSettings::Flood {
        endpoint: endpoint.into(),
        tv: Some(scope("tv-hrr", TorrentQueuePriority::Last, false)),
        movies: Some(scope("movies-hrr", TorrentQueuePriority::Last, true)),
    }
}
fn login() -> Credentials {
    Credentials::UsernamePassword {
        username: USER.into(),
        password: PASSWORD.into(),
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
fn magnet(hash: &str) -> String {
    format!("magnet:?xt=urn:btih:{hash}&dn=release&tr=http%3A%2F%2Ftracker.invalid%2Fannounce")
}
fn reparse(identity: &SubmissionIdentity) -> SubmissionIdentity {
    serde_json::from_value(serde_json::to_value(identity).unwrap()).unwrap()
}

#[tokio::test]
async fn login_cookie_connection_check_and_no_auth_mode() {
    let (f, endpoint, task) = fixture().await;
    let settings = settings(&endpoint);
    let client = HttpClient::new().unwrap();
    let operation = client.operation(Uuid::new_v4()).unwrap();
    let result = label_client::test_connection(&operation, &settings, Some(&login()))
        .await
        .unwrap();
    assert_eq!(result.client_version, "Flood REST API");
    assert_eq!(result.domains, vec![MediaDomain::Tv, MediaDomain::Movies]);
    assert!(
        f.paths()
            .iter()
            .all(|p| p.starts_with("GET ") || p == "POST /api/auth/authenticate"),
        "Test never mutates the client: {:?}",
        f.paths()
    );
    let wrong = Credentials::UsernamePassword {
        username: USER.into(),
        password: "nope".into(),
    };
    assert_eq!(
        label_client::test_connection(&operation, &settings, Some(&wrong))
            .await
            .unwrap_err()
            .error,
        QbitError::Http(HttpError::Authentication)
    );
    // Without credentials the protected routes refuse; an API key is not a Flood credential.
    assert_eq!(
        label_client::test_connection(&operation, &settings, None)
            .await
            .unwrap_err()
            .error,
        QbitError::Http(HttpError::Authentication)
    );
    assert_eq!(
        label_client::test_connection(
            &operation,
            &settings,
            Some(&Credentials::ApiKey {
                api_key: "k".into()
            })
        )
        .await
        .unwrap_err()
        .error,
        QbitError::InvalidRequest
    );
    // Flood with authentication disabled needs no credentials at all.
    f.0.lock().unwrap().auth = false;
    label_client::test_connection(&operation, &settings, None)
        .await
        .unwrap();
    // Flood is up but cannot reach its torrent client.
    f.0.lock().unwrap().connected = false;
    assert_eq!(
        label_client::test_connection(&operation, &settings, None)
            .await
            .unwrap_err()
            .error,
        QbitError::Http(HttpError::Transport)
    );
    task.abort();
}

#[tokio::test]
async fn adds_magnet_and_torrent_with_the_domain_tag() {
    let (f, endpoint, task) = fixture().await;
    let settings = settings(&endpoint);
    let client = HttpClient::new().unwrap();
    let operation = client.operation(Uuid::new_v4()).unwrap();
    let tv_hash = "0123456789abcdef0123456789abcdef01234567";
    let (bytes, movie_hash) = torrent("Film.2020.1080p");
    f.0.lock()
        .unwrap()
        .metainfo
        .push((b64(&bytes), movie_hash.clone()));
    let tv = label_client::prepare(
        &operation,
        &settings,
        MediaTarget::Episode(7),
        AddSource::Magnet(magnet(tv_hash)),
        true,
        QbitOptions::default(),
    )
    .await
    .unwrap();
    let movie = label_client::prepare(
        &operation,
        &settings,
        MediaTarget::Movie(7),
        AddSource::Torrent {
            filename: "film.torrent".into(),
            bytes: bytes.clone(),
        },
        false,
        QbitOptions::default(),
    )
    .await
    .unwrap();
    f.clear();
    assert_eq!(
        label_client::submit(&operation, &settings, Some(&login()), tv)
            .await
            .unwrap(),
        AddOutcome::Observed {
            hash: tv_hash.into()
        }
    );
    assert_eq!(
        label_client::submit(&operation, &settings, Some(&login()), movie)
            .await
            .unwrap(),
        AddOutcome::Observed {
            hash: movie_hash.clone()
        }
    );
    assert_eq!(
        f.posts("/api/torrents/add-urls"),
        vec![json!({"urls":[magnet(tv_hash)],"tags":["tv-hrr"],"start":true})]
    );
    assert_eq!(
        f.posts("/api/torrents/add-files"),
        vec![json!({"files":[b64(&bytes)],"tags":["movies-hrr"],"start":false})]
    );
    assert!(f.by_hash(&movie_hash).unwrap().status.contains(&"stopped"));
    // Each domain observes only its own tag.
    let tv_page = label_client::query(
        &operation,
        &settings,
        Some(&login()),
        &query(MediaDomain::Tv),
    )
    .await
    .unwrap();
    assert_eq!(
        tv_page
            .items
            .iter()
            .map(|i| (i.hash.as_str(), i.domain, i.category.as_str()))
            .collect::<Vec<_>>(),
        vec![(tv_hash, MediaDomain::Tv, "tv-hrr")]
    );
    let movie_page = label_client::query(
        &operation,
        &settings,
        Some(&login()),
        &query(MediaDomain::Movies),
    )
    .await
    .unwrap();
    assert_eq!(
        (
            movie_page.items[0].hash.as_str(),
            &movie_page.items[0].status
        ),
        (movie_hash.as_str(), &DownloadStatus::Paused)
    );
    assert_eq!(
        label_client::details(
            &operation,
            &settings,
            Some(&login()),
            &MediaTarget::Movie(7),
            tv_hash
        )
        .await
        .err(),
        Some(QbitError::ScopeConflict)
    );
    // The password and session token never reach a projected name.
    let mut leak = tr(
        "5555555555555555555555555555555555555555",
        &format!("leak.{PASSWORD}.{JWT}"),
        "/downloads",
        &["tv-hrr"],
        &["downloading"],
    );
    leak.size = 10;
    f.put(leak);
    let page = label_client::query(
        &operation,
        &settings,
        Some(&login()),
        &query(MediaDomain::Tv),
    )
    .await
    .unwrap();
    let text = serde_json::to_string(&page).unwrap();
    assert!(!text.contains(PASSWORD) && !text.contains(JWT), "{text}");
    task.abort();
}

#[tokio::test]
async fn duplicates_and_lost_responses_reconcile_by_hash_without_a_second_add() {
    let (f, endpoint, task) = fixture().await;
    let settings = settings(&endpoint);
    let client = HttpClient::new().unwrap();
    let operation = client.operation(Uuid::new_v4()).unwrap();
    let prepare = |target: MediaTarget, hash: &str| {
        let (operation, settings) = (&operation, &settings);
        let source = AddSource::Magnet(magnet(hash));
        async move {
            label_client::prepare(
                operation,
                settings,
                target,
                source,
                false,
                QbitOptions::default(),
            )
            .await
            .unwrap()
        }
    };
    let hash = "1111111111111111111111111111111111111111";
    let first = prepare(MediaTarget::Episode(1), hash).await;
    let identity = reparse(first.identity());
    assert!(matches!(
        label_client::submit(&operation, &settings, Some(&login()), first)
            .await
            .unwrap(),
        AddOutcome::Observed { .. }
    ));
    let again = prepare(MediaTarget::Episode(1), hash).await;
    assert_eq!(
        label_client::submit(&operation, &settings, Some(&login()), again)
            .await
            .unwrap(),
        AddOutcome::AlreadyPresent { hash: hash.into() }
    );
    assert_eq!(f.posts("/api/torrents/add-urls").len(), 1);
    assert_eq!(
        label_client::reconcile(&operation, &settings, Some(&login()), &identity)
            .await
            .unwrap(),
        Reconciliation::Observed { hash: hash.into() }
    );
    let foreign = prepare(MediaTarget::Movie(1), hash).await;
    assert_eq!(
        label_client::submit(&operation, &settings, Some(&login()), foreign)
            .await
            .unwrap_err(),
        QbitError::ScopeConflict
    );

    // Lost response: Flood accepted the torrent (tagged at add time) but the reply never arrived.
    let lost_hash = "2222222222222222222222222222222222222222";
    f.0.lock().unwrap().lose_add_response = true;
    let lost = prepare(MediaTarget::Episode(2), lost_hash).await;
    let lost_identity = reparse(lost.identity());
    assert_eq!(
        label_client::reconcile(&operation, &settings, Some(&login()), &lost_identity)
            .await
            .unwrap(),
        Reconciliation::NotObserved
    );
    assert_eq!(
        label_client::submit(&operation, &settings, Some(&login()), lost)
            .await
            .unwrap_err(),
        QbitError::MutationUnknown
    );
    f.0.lock().unwrap().lose_add_response = false;
    assert_eq!(
        label_client::reconcile(&operation, &settings, Some(&login()), &lost_identity)
            .await
            .unwrap(),
        Reconciliation::Observed {
            hash: lost_hash.into()
        }
    );
    let retry = prepare(MediaTarget::Episode(2), lost_hash).await;
    assert_eq!(
        label_client::submit(&operation, &settings, Some(&login()), retry)
            .await
            .unwrap(),
        AddOutcome::AlreadyPresent {
            hash: lost_hash.into()
        }
    );
    assert_eq!(
        f.posts("/api/torrents/add-urls").len(),
        2,
        "never a second add"
    );
    // A hash present untagged is a visible conflict.
    f.put(tr(
        "3333333333333333333333333333333333333333",
        "Mine.Not",
        "/elsewhere",
        &[],
        &["downloading"],
    ));
    let others = prepare(
        MediaTarget::Episode(3),
        "3333333333333333333333333333333333333333",
    )
    .await;
    assert_eq!(
        label_client::submit(&operation, &settings, Some(&login()), others)
            .await
            .unwrap_err(),
        QbitError::ScopeConflict
    );
    let denied = prepare(
        MediaTarget::Episode(4),
        "4444444444444444444444444444444444444444",
    )
    .await;
    let wrong = Credentials::UsernamePassword {
        username: USER.into(),
        password: "nope".into(),
    };
    assert_eq!(
        label_client::submit(&operation, &settings, Some(&wrong), denied)
            .await
            .unwrap_err(),
        QbitError::Http(HttpError::Authentication)
    );
    assert_eq!(f.posts("/api/torrents/add-urls").len(), 2);
    task.abort();
}

#[tokio::test]
async fn status_mapping_tag_isolation_details_and_controls() {
    let (f, endpoint, task) = fixture().await;
    let settings = settings(&endpoint);
    let client = HttpClient::new().unwrap();
    let operation = client.operation(Uuid::new_v4()).unwrap();
    let h = |n: u8| format!("{:040}", n);
    f.put(tr(
        &h(1),
        "Downloading",
        "/downloads",
        &["tv-hrr"],
        &["downloading", "active"],
    ));
    let mut seeding = tr(
        &h(2),
        "Seeding",
        "/downloads",
        &["tv-hrr"],
        &["complete", "seeding", "active"],
    );
    seeding.done = seeding.size;
    f.put(seeding);
    f.put(tr(
        &h(3),
        "Stopped",
        "/downloads",
        &["tv-hrr"],
        &["stopped", "inactive"],
    ));
    f.put(tr(&h(4), "Idle", "/downloads", &["tv-hrr"], &["inactive"]));
    f.put(tr(&h(5), "Errored", "/downloads", &["tv-hrr"], &["error"]));
    f.put(tr(
        &h(6),
        "Checking",
        "/downloads",
        &["tv-hrr"],
        &["checking"],
    ));
    // Several tags: the configured domain tag wins whatever its position.
    f.put(tr(
        &h(7),
        "Multi.Tag",
        "/downloads",
        &["archive", "TV-HRR", "other"],
        &["downloading"],
    ));
    f.put(tr(
        &h(8),
        "Foreign",
        "/downloads",
        &["other-app"],
        &["downloading"],
    ));
    f.put(tr(&h(9), "Untagged", "/downloads", &[], &["downloading"]));
    f.put(tr(
        &h(10),
        "MovieOne",
        "/downloads",
        &["movies-hrr"],
        &["downloading"],
    ));
    let mut meta = tr(&h(11), "", "/downloads", &["tv-hrr"], &["downloading"]);
    (meta.size, meta.done, meta.files) = (0, 0, vec![]);
    f.put(meta);
    // A multi-file torrent: its directory is its own folder.
    let mut multi = tr(
        &h(12),
        "Multi.Pack",
        "/downloads/Multi.Pack",
        &["tv-hrr"],
        &["complete", "seeding"],
    );
    multi.done = multi.size;
    multi.files = vec![
        ("Multi.Pack/Multi.Pack.S01E01.mkv".into(), 1048000, 100.0, 1),
        ("Multi.Pack/sample.mkv".into(), 576, 0.0, 0),
    ];
    f.put(multi);

    let page = label_client::query(
        &operation,
        &settings,
        Some(&login()),
        &query(MediaDomain::Tv),
    )
    .await
    .unwrap();
    let status = |n: u8| {
        page.items
            .iter()
            .find(|i| i.hash == h(n))
            .map(|i| (&i.status, &i.diagnostic, i.completed))
            .unwrap()
    };
    assert_eq!(status(1), (&DownloadStatus::Downloading, &None, false));
    assert_eq!(status(2), (&DownloadStatus::Completed, &None, true));
    assert_eq!(status(3), (&DownloadStatus::Paused, &None, false));
    assert_eq!(
        status(5),
        (
            &DownloadStatus::Warning,
            &Some(DownloadDiagnostic::Error),
            false
        )
    );
    assert_eq!(
        status(11),
        (
            &DownloadStatus::Queued,
            &Some(DownloadDiagnostic::Metadata),
            false
        )
    );
    assert!(page.items.iter().any(|i| i.hash == h(4)) && page.items.iter().any(|i| i.hash == h(6)));
    assert_eq!(
        page.items.iter().find(|i| i.hash == h(7)).unwrap().category,
        "TV-HRR"
    );
    assert!(
        page.items
            .iter()
            .all(|i| ![h(8), h(9), h(10)].contains(&i.hash))
    );
    let movies = label_client::query(
        &operation,
        &settings,
        Some(&login()),
        &query(MediaDomain::Movies),
    )
    .await
    .unwrap();
    assert_eq!(
        movies
            .items
            .iter()
            .map(|i| i.hash.clone())
            .collect::<Vec<_>>(),
        vec![h(10)]
    );

    let details = label_client::details(
        &operation,
        &settings,
        Some(&login()),
        &MediaTarget::Episode(1),
        &h(12),
    )
    .await
    .unwrap();
    assert_eq!(details.save_path, "/downloads");
    assert_eq!(
        details
            .files
            .iter()
            .map(|f| (f.name.as_str(), f.priority))
            .collect::<Vec<_>>(),
        vec![
            ("Multi.Pack/Multi.Pack.S01E01.mkv", 1),
            ("Multi.Pack/sample.mkv", 0)
        ]
    );
    assert_eq!(
        label_client::details(
            &operation,
            &settings,
            Some(&login()),
            &MediaTarget::Episode(1),
            &h(8)
        )
        .await
        .err(),
        Some(QbitError::ScopeConflict)
    );

    let control = |target: MediaTarget, hash: String, action: Control| {
        let (operation, settings) = (&operation, &settings);
        async move {
            label_client::control(operation, settings, Some(&login()), &target, &hash, action).await
        }
    };
    assert_eq!(
        control(MediaTarget::Episode(1), h(1), Control::Pause).await,
        Ok(())
    );
    assert!(f.by_hash(&h(1)).unwrap().status.contains(&"stopped"));
    assert_eq!(
        control(MediaTarget::Episode(1), h(1), Control::Resume).await,
        Ok(())
    );
    assert!(f.by_hash(&h(1)).unwrap().status.contains(&"downloading"));
    assert_eq!(
        control(MediaTarget::Movie(1), h(1), Control::Pause).await,
        Err(QbitError::ScopeConflict)
    );
    // Queue order needs PATCH, which the transport never sends: explicit refusal, no request.
    f.clear();
    assert_eq!(
        control(
            MediaTarget::Episode(1),
            h(1),
            Control::Priority { first: true }
        )
        .await,
        Err(QbitError::UnsupportedFeature)
    );
    assert!(f.posts("/api/torrents/stop").is_empty());
    assert_eq!(
        control(
            MediaTarget::Episode(1),
            h(1),
            Control::Remove { delete_files: true }
        )
        .await,
        Ok(())
    );
    assert_eq!(
        f.posts("/api/torrents/delete").last().unwrap()["deleteData"],
        true
    );
    assert!(f.by_hash(&h(1)).is_none());
    assert_eq!(
        control(
            MediaTarget::Episode(1),
            h(2),
            Control::Remove {
                delete_files: false
            }
        )
        .await,
        Ok(())
    );
    assert_eq!(
        f.posts("/api/torrents/delete").last().unwrap()["deleteData"],
        false
    );
    task.abort();
}
