//! uTorrent Web UI wire contract against an owned mock on 127.0.0.1:0 (never a live uTorrent).
//! The `token.html` handshake, the `?token=...&list=1` torrent rows (positions, permille progress,
//! status bit field) and `action=...` calls follow uTorrent's published Web UI API; the mock is
//! written from that description, not recorded from a running uTorrent.
use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode, Uri},
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
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

const USER: &str = "ut-user";
const PASSWORD: &str = "PRIVATE_UTORRENT_PASSWORD";
const TOKEN: &str = "PRIVATE_UT_TOKEN_abc-123_XYZ";
const COOKIE: &str = "GUID=PRIVATE_UT_GUID";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
/// Independent RFC 4648 encoder for the basic-auth value.
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
struct Item {
    hash: String,
    status: i64,
    name: String,
    size: i64,
    permille: i64,
    label: String,
    path: String,
    /// (name, size, downloaded, priority)
    files: Vec<(String, i64, i64, i64)>,
}
fn item(hash: &str, name: &str, label: &str) -> Item {
    Item {
        hash: hash.to_uppercase(),
        status: 1 | 8 | 128,
        name: name.into(),
        size: 1048576,
        permille: 500,
        label: label.into(),
        path: format!("/downloads/{name}"),
        files: vec![(name.into(), 1048576, 524288, 2)],
    }
}
struct Mock {
    build: i64,
    torrents: Vec<Item>,
    /// (method + query, auth/cookie ok) of every GUI request after the handshake.
    calls: Vec<(String, HashMap<String, String>)>,
    delay_listings: u32,
    pending: Vec<(u32, Item)>,
    lose_add_response: bool,
    uploads: Vec<(Vec<u8>, String)>,
    token_html: Option<String>,
    cookie_required: bool,
}
#[derive(Clone)]
struct Fixture(Arc<Mutex<Mock>>);
impl Fixture {
    fn calls(&self, action: &str) -> Vec<HashMap<String, String>> {
        self.0
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|(_, q)| q.get("action").map(String::as_str) == Some(action))
            .map(|(_, q)| q.clone())
            .collect()
    }
    fn actions(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .calls
            .iter()
            .map(|(_, q)| q.get("action").cloned().unwrap_or_else(|| "list".into()))
            .collect()
    }
    fn clear(&self) {
        self.0.lock().unwrap().calls.clear();
    }
    fn get(&self, hash: &str) -> Item {
        self.0
            .lock()
            .unwrap()
            .torrents
            .iter()
            .find(|t| t.hash.eq_ignore_ascii_case(hash))
            .cloned()
            .unwrap()
    }
    fn put(&self, item: Item) {
        self.0.lock().unwrap().torrents.push(item);
    }
}
fn row(t: &Item) -> Value {
    let mut cells = vec![Value::Null; 27];
    cells[0] = json!(t.hash);
    cells[1] = json!(t.status);
    cells[2] = json!(t.name);
    cells[3] = json!(t.size);
    cells[4] = json!(t.permille);
    cells[5] = json!(t.size * t.permille / 1000);
    cells[6] = json!(0);
    cells[7] = json!(250);
    cells[8] = json!(64);
    cells[9] = json!(2048);
    cells[10] = json!(42);
    cells[11] = json!(t.label);
    for i in 12..=18 {
        cells[i] = json!(0);
    }
    for i in 19..=22 {
        cells[i] = json!("");
    }
    cells[23] = json!(0);
    cells[24] = json!(0);
    cells[25] = json!("");
    cells[26] = json!(t.path);
    Value::Array(cells)
}
async fn handler(State(f): State<Fixture>, uri: Uri, headers: HeaderMap, body: Bytes) -> Response {
    let mut m = f.0.lock().unwrap();
    let expected = format!("Basic {}", b64(format!("{USER}:{PASSWORD}").as_bytes()));
    if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some(expected.as_str()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if uri.path() == "/gui/token.html" {
        let html = m.token_html.clone().unwrap_or_else(|| {
            format!("<html><div id='token' style='display:none;'>{TOKEN}</div></html>")
        });
        return ([("set-cookie", format!("{COOKIE}; path=/"))], html).into_response();
    }
    if uri.path() != "/gui/" {
        return StatusCode::NOT_FOUND.into_response();
    }
    let query: HashMap<String, String> =
        url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();
    let cookie_ok = headers.get("cookie").and_then(|v| v.to_str().ok()) == Some(COOKIE);
    if query.get("token").map(String::as_str) != Some(TOKEN) || (m.cookie_required && !cookie_ok) {
        return (StatusCode::BAD_REQUEST, "invalid request").into_response();
    }
    let method = if body.is_empty() { "GET" } else { "POST" };
    m.calls.push((method.into(), query.clone()));
    let build = m.build;
    let find = |m: &Mock, hash: &str| {
        m.torrents
            .iter()
            .position(|t| t.hash.eq_ignore_ascii_case(hash))
    };
    let ok = |v: Value| axum::Json(v).into_response();
    match query.get("action").map(String::as_str) {
        None if query.get("list").map(String::as_str) == Some("1") => {
            let mut ready = Vec::new();
            m.pending
                .iter_mut()
                .for_each(|(n, _)| *n = n.saturating_sub(1));
            let mut i = 0;
            while i < m.pending.len() {
                if m.pending[i].0 == 0 {
                    ready.push(m.pending.remove(i).1);
                } else {
                    i += 1;
                }
            }
            m.torrents.extend(ready);
            ok(
                json!({"build":build,"label":[],"torrents":m.torrents.iter().map(row).collect::<Vec<_>>(),"torrentc":"1"}),
            )
        }
        Some("getsettings") => ok(json!({"build":build,"settings":[
            ["dir_active_download",2,"/downloads"],
            ["dir_completed_download",2,"/completed"],
            ["dir_completed_download_flag",1,"false"]]})),
        Some("getfiles") => {
            let hash = query.get("hash").unwrap();
            match find(&m, hash) {
                Some(i) => {
                    let t = &m.torrents[i];
                    ok(json!({"build":build,"files":[t.hash,
                        t.files.iter().map(|(n,s,d,p)| json!([n,s,d,p,0,0,0])).collect::<Vec<_>>()]}))
                }
                None => ok(json!({"build":build,"files":[hash,[]]})),
            }
        }
        Some("add-url") | Some("add-file") => {
            let hash = if query.get("action").map(String::as_str) == Some("add-url") {
                query["s"].split("urn:btih:").nth(1).unwrap()[..40].to_lowercase()
            } else {
                let text = body.to_vec();
                assert!(
                    String::from_utf8_lossy(&text).contains("name=\"torrent_file\""),
                    "the torrent travels in the torrent_file part"
                );
                m.uploads
                    .iter()
                    .find(|(bytes, _)| text.windows(bytes.len()).any(|w| w == bytes.as_slice()))
                    .map(|(_, h)| h.clone())
                    .expect("unknown upload")
            };
            if find(&m, &hash).is_none()
                && !m
                    .pending
                    .iter()
                    .any(|(_, p)| p.hash.eq_ignore_ascii_case(&hash))
            {
                let t = item(&hash, "Added", "");
                if m.delay_listings > 0 {
                    let n = m.delay_listings;
                    m.pending.push((n, t));
                } else {
                    m.torrents.push(t);
                }
            }
            if m.lose_add_response {
                return StatusCode::BAD_GATEWAY.into_response();
            }
            ok(json!({"build":build}))
        }
        Some(action) => {
            let Some(i) = find(&m, query.get("hash").map(String::as_str).unwrap_or("")) else {
                return ok(json!({"build":build}));
            };
            match action {
                "setprops" => {
                    assert_eq!(query.get("s").map(String::as_str), Some("label"));
                    m.torrents[i].label = query["v"].clone();
                }
                "stop" => m.torrents[i].status &= !1,
                "start" => m.torrents[i].status |= 1,
                "pause" => m.torrents[i].status |= 32,
                "unpause" => m.torrents[i].status &= !32,
                "remove" | "removedata" => {
                    m.torrents.remove(i);
                }
                "queuetop" | "queuebottom" => {}
                other => panic!("unexpected action {other}"),
            }
            ok(json!({"build":build}))
        }
        None => StatusCode::BAD_REQUEST.into_response(),
    }
}
async fn fixture(path: &str) -> (Fixture, String, tokio::task::JoinHandle<()>) {
    let fixture = Fixture(Arc::new(Mutex::new(Mock {
        build: 43000,
        torrents: vec![],
        calls: vec![],
        delay_listings: 0,
        pending: vec![],
        lose_add_response: false,
        uploads: vec![],
        token_html: None,
        cookie_required: true,
    })));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}{path}", listener.local_addr().unwrap());
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
    ProviderSettings::Utorrent {
        endpoint: endpoint.into(),
        tv: Some(scope("tv-hrr", TorrentQueuePriority::First, false)),
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
async fn token_cookie_credentials_and_response_hardening() {
    let (f, endpoint, task) = fixture("/gui").await;
    let settings = settings(&endpoint);
    let client = HttpClient::new().unwrap();
    let operation = client.operation(Uuid::new_v4()).unwrap();
    let result = label_client::test_connection(&operation, &settings, Some(&login()))
        .await
        .unwrap();
    assert_eq!(result.client_version, "build 43000");
    assert_eq!(result.domains, vec![MediaDomain::Tv, MediaDomain::Movies]);
    assert!(
        f.actions()
            .iter()
            .all(|a| !matches!(a.as_str(), "add-url" | "add-file" | "setprops")),
        "Test never mutates the client: {:?}",
        f.actions()
    );
    // The Web UI always needs a user name and password.
    for credentials in [
        None,
        Some(Credentials::ApiKey {
            api_key: "k".into(),
        }),
    ] {
        assert_eq!(
            label_client::test_connection(&operation, &settings, credentials.as_ref())
                .await
                .unwrap_err()
                .error,
            QbitError::InvalidRequest
        );
    }
    let wrong = Credentials::UsernamePassword {
        username: USER.into(),
        password: "wrong".into(),
    };
    assert_eq!(
        label_client::test_connection(&operation, &settings, Some(&wrong))
            .await
            .unwrap_err()
            .error,
        QbitError::Http(HttpError::Authentication)
    );
    // A token page that is not the token page is not accepted.
    for html in [
        "<html>nothing here</html>",
        "<div id='token'>bad token!</div>",
        "<div id='token'></div>",
    ] {
        f.0.lock().unwrap().token_html = Some(html.into());
        assert_eq!(
            label_client::test_connection(&operation, &settings, Some(&login()))
                .await
                .unwrap_err()
                .error,
            QbitError::InvalidResponse,
            "{html}"
        );
    }
    f.0.lock().unwrap().token_html = None;
    // A build of zero is not a usable Web UI.
    f.0.lock().unwrap().build = 0;
    assert_eq!(
        label_client::test_connection(&operation, &settings, Some(&login()))
            .await
            .unwrap_err()
            .error,
        QbitError::UnsupportedVersion
    );
    f.0.lock().unwrap().build = 43000;
    // A wrong URL base is a connection failure.
    let wrong_base = ProviderSettings::Utorrent {
        endpoint: endpoint.replace("/gui", "/elsewhere"),
        tv: Some(scope("tv-hrr", TorrentQueuePriority::Last, false)),
        movies: None,
    };
    assert_eq!(
        label_client::test_connection(&operation, &wrong_base, Some(&login()))
            .await
            .unwrap_err()
            .error,
        QbitError::Http(HttpError::Transport)
    );
    // The token and cookie are replayed on every GUI request.
    for (_, q) in f.0.lock().unwrap().calls.iter() {
        assert_eq!(q["token"], TOKEN);
    }
    task.abort();
}

#[tokio::test]
async fn adds_magnet_and_torrent_per_domain_with_labels_stop_and_queue_top() {
    let (f, endpoint, task) = fixture("/gui").await;
    let settings = settings(&endpoint);
    let client = HttpClient::new().unwrap();
    let operation = client.operation(Uuid::new_v4()).unwrap();
    let tv_hash = "0123456789abcdef0123456789abcdef01234567";
    let (bytes, movie_hash) = torrent("Film.2020.1080p");
    f.0.lock()
        .unwrap()
        .uploads
        .push((bytes.clone(), movie_hash.clone()));
    f.0.lock().unwrap().delay_listings = 3;
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
    let add_url = f.calls("add-url");
    assert_eq!(add_url.len(), 1);
    assert_eq!(add_url[0]["s"], magnet(tv_hash));
    assert_eq!(f.calls("add-file").len(), 1);
    // The label is applied once the torrent is listed; the paused movie is stopped; only the
    // recent TV target with priority "first" moves to the queue top.
    let labels: Vec<(String, String)> = f
        .calls("setprops")
        .into_iter()
        .map(|q| (q["hash"].to_lowercase(), q["v"].clone()))
        .collect();
    assert_eq!(
        labels,
        vec![
            (tv_hash.to_owned(), "tv-hrr".to_owned()),
            (movie_hash.clone(), "movies-hrr".to_owned())
        ]
    );
    assert_eq!(f.calls("stop").len(), 1);
    assert_eq!(f.calls("stop")[0]["hash"], movie_hash.to_uppercase());
    assert_eq!(f.calls("queuetop").len(), 1);
    assert_eq!(f.calls("queuetop")[0]["hash"], tv_hash.to_uppercase());
    assert_eq!(f.get(&movie_hash).status & 1, 0, "movie stopped");
    // Each domain observes only its own label.
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
    // Credentials and the session token never reach a projected name.
    f.put(item(
        "5555555555555555555555555555555555555555",
        &format!("leak.{PASSWORD}.{USER}.{TOKEN}"),
        "tv-hrr",
    ));
    let page = label_client::query(
        &operation,
        &settings,
        Some(&login()),
        &query(MediaDomain::Tv),
    )
    .await
    .unwrap();
    let text = serde_json::to_string(&page).unwrap();
    assert!(!text.contains(PASSWORD) && !text.contains(USER) && !text.contains(TOKEN));
    task.abort();
}

#[tokio::test]
async fn duplicates_and_lost_responses_reconcile_by_hash_without_a_second_add() {
    let (f, endpoint, task) = fixture("/gui").await;
    let settings = settings(&endpoint);
    let client = HttpClient::new().unwrap();
    let operation = client.operation(Uuid::new_v4()).unwrap();
    let hash = "1111111111111111111111111111111111111111";
    let prepare = |target: MediaTarget, hash: &str| {
        let operation = &operation;
        let settings = &settings;
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
    assert_eq!(f.calls("add-url").len(), 1);
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
    assert_eq!(f.calls("add-url").len(), 1);

    // Lost response: uTorrent added the torrent (unlabelled) but the reply never arrived. The
    // engine labels the stranded torrent so a later reconcile observes it in this domain.
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
    assert_eq!(f.get(lost_hash).label, "tv-hrr");
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
    assert_eq!(f.calls("add-url").len(), 2, "never a second add");
    // An unlabelled torrent with a fresh grab's hash is a visible conflict, never adopted.
    f.put(item(
        "3333333333333333333333333333333333333333",
        "Mine.Not",
        "",
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
    assert_eq!(f.calls("add-url").len(), 2);
    let denied = prepare(
        MediaTarget::Episode(4),
        "4444444444444444444444444444444444444444",
    )
    .await;
    let wrong = Credentials::UsernamePassword {
        username: USER.into(),
        password: "wrong".into(),
    };
    assert_eq!(
        label_client::submit(&operation, &settings, Some(&wrong), denied)
            .await
            .unwrap_err(),
        QbitError::Http(HttpError::Authentication)
    );
    assert_eq!(f.calls("add-url").len(), 2);
    task.abort();
}

#[tokio::test]
async fn status_mapping_isolation_details_and_controls() {
    let (f, endpoint, task) = fixture("/gui").await;
    let settings = settings(&endpoint);
    let client = HttpClient::new().unwrap();
    let operation = client.operation(Uuid::new_v4()).unwrap();
    let h = |n: u8| format!("{:040}", n);
    f.put(item(&h(1), "Downloading", "tv-hrr"));
    let mut seeding = item(&h(2), "Seeding", "tv-hrr");
    (seeding.permille, seeding.path) = (1000, "/downloads".into());
    f.put(seeding);
    let mut paused = item(&h(3), "Paused", "tv-hrr");
    paused.status |= 32;
    f.put(paused);
    let mut queued = item(&h(4), "Queued", "tv-hrr");
    queued.status = 64 | 8 | 128;
    f.put(queued);
    let mut errored = item(&h(5), "Errored", "tv-hrr");
    errored.status |= 16;
    f.put(errored);
    let mut checking = item(&h(6), "Checking", "tv-hrr");
    checking.status |= 2;
    f.put(checking);
    let mut stopped = item(&h(7), "StoppedDone", "tv-hrr");
    (stopped.status, stopped.permille) = (8 | 128, 1000);
    f.put(stopped);
    f.put(item(&h(8), "Foreign", "other"));
    f.put(item(&h(9), "MovieOne", "movies-hrr"));
    f.put(item(&h(10), "Unlabelled", ""));
    let mut magnet_item = item(&h(11), "Magnet", "tv-hrr");
    magnet_item.size = 0;
    f.put(magnet_item);
    let mut multi = item(&h(12), "Multi.Pack", "tv-hrr");
    (multi.permille, multi.path) = (1000, "/downloads/Multi.Pack".into());
    multi.files = vec![
        ("Multi.Pack.S01E01.mkv".into(), 1048000, 1048000, 2),
        ("Subs\\sample.srt".into(), 576, 0, 0),
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
    assert_eq!(status(4), (&DownloadStatus::Queued, &None, false));
    assert_eq!(
        status(5),
        (
            &DownloadStatus::Warning,
            &Some(DownloadDiagnostic::Error),
            false
        )
    );
    assert_eq!(status(6), (&DownloadStatus::Queued, &None, false));
    assert_eq!(status(7), (&DownloadStatus::Completed, &None, true));
    assert_eq!(
        status(11),
        (
            &DownloadStatus::Queued,
            &Some(DownloadDiagnostic::Metadata),
            false
        )
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
        vec![h(9)]
    );

    // Details: the root download path ends with the torrent's own name; the save path is its
    // parent, and multi-file names are prefixed with the folder. Backslashes are normalized.
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
            ("Multi.Pack/Subs/sample.srt", 0)
        ]
    );
    let single = label_client::details(
        &operation,
        &settings,
        Some(&login()),
        &MediaTarget::Episode(1),
        &h(2),
    )
    .await
    .unwrap();
    assert_eq!(single.save_path, "/downloads");
    assert_eq!(single.files[0].name, "Seeding");
    assert_eq!(
        label_client::details(
            &operation,
            &settings,
            Some(&login()),
            &MediaTarget::Episode(1),
            &h(10)
        )
        .await
        .err(),
        Some(QbitError::ScopeConflict)
    );
    assert_eq!(
        label_client::details(
            &operation,
            &settings,
            Some(&login()),
            &MediaTarget::Episode(1),
            &h(99)
        )
        .await
        .err(),
        Some(QbitError::NotFound)
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
    assert_eq!(f.get(&h(1)).status & 32, 32);
    assert_eq!(
        control(MediaTarget::Episode(1), h(1), Control::Resume).await,
        Ok(())
    );
    assert_eq!(f.get(&h(1)).status & 32, 0);
    assert_eq!(
        control(
            MediaTarget::Episode(1),
            h(1),
            Control::Priority { first: true }
        )
        .await,
        Ok(())
    );
    assert_eq!(f.calls("queuetop").last().unwrap()["hash"], h(1));
    assert_eq!(
        control(MediaTarget::Movie(1), h(1), Control::Pause).await,
        Err(QbitError::ScopeConflict)
    );
    // Data removal is supported by uTorrent: the matching action is `removedata`.
    assert_eq!(
        control(
            MediaTarget::Episode(1),
            h(2),
            Control::Remove { delete_files: true }
        )
        .await,
        Ok(())
    );
    assert_eq!(f.calls("removedata").len(), 1);
    assert_eq!(
        control(
            MediaTarget::Episode(1),
            h(3),
            Control::Remove {
                delete_files: false
            }
        )
        .await,
        Ok(())
    );
    assert_eq!(f.calls("remove").len(), 1);
    task.abort();
}
