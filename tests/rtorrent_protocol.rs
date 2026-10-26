//! rTorrent XML-RPC wire contract against an owned mock on 127.0.0.1:0 (never a live rTorrent or
//! its SCGI socket). Method names, argument shapes and `methodResponse`/`fault` envelopes follow
//! rTorrent's published command reference (`system.client_version`, `directory.default`,
//! `d.multicall2`, `f.multicall`, `load.*`, `d.*`); the mock is written from that reference, not
//! recorded from a running rTorrent.
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
use std::sync::{Arc, Mutex};
use uuid::Uuid;

const USER: &str = "rt-user";
const PASSWORD: &str = "PRIVATE_RTORRENT_PASSWORD";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
/// Independent RFC 4648 encoder for asserting the uploaded `.torrent` and the basic-auth value.
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

#[derive(Clone, Debug, PartialEq)]
enum Arg {
    S(String),
    I(i64),
}
impl Arg {
    fn s(&self) -> &str {
        match self {
            Arg::S(s) => s,
            Arg::I(_) => panic!("string expected"),
        }
    }
}
#[derive(Clone)]
struct Item {
    hash: String,
    name: String,
    custom1: String,
    directory: String,
    multi: bool,
    size: i64,
    done: i64,
    state: i64,
    active: i64,
    complete: i64,
    hashing: i64,
    message: String,
    ratio: i64,
    /// (path, size, completed_chunks, size_chunks, priority)
    files: Vec<(String, i64, i64, i64, i64)>,
}
fn item(hash: &str, name: &str, label: &str) -> Item {
    Item {
        hash: hash.to_uppercase(),
        name: name.into(),
        custom1: label.into(),
        directory: "/downloads".into(),
        multi: false,
        size: 1048576,
        done: 524288,
        state: 1,
        active: 1,
        complete: 0,
        hashing: 0,
        message: String::new(),
        ratio: 250,
        files: vec![(name.into(), 1048576, 2, 4, 1)],
    }
}
struct Mock {
    version: String,
    auth: bool,
    torrents: Vec<Item>,
    calls: Vec<(String, Vec<Arg>)>,
    /// Loaded torrents become visible after this many listings (rTorrent loads asynchronously).
    delay_listings: u32,
    pending: Vec<(u32, Item)>,
    lose_load_response: bool,
    metainfo: Vec<(String, String)>,
    raw_reply: Option<(u16, String)>,
}
#[derive(Clone)]
struct Fixture(Arc<Mutex<Mock>>);
impl Fixture {
    fn calls(&self, method: &str) -> Vec<Vec<Arg>> {
        self.0
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, a)| a.clone())
            .collect()
    }
    fn methods(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .calls
            .iter()
            .map(|(m, _)| m.clone())
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
fn parse_call(body: &[u8]) -> (String, Vec<Arg>) {
    let text = std::str::from_utf8(body).unwrap();
    let doc = roxmltree::Document::parse(text).unwrap();
    let root = doc.root_element();
    assert_eq!(root.tag_name().name(), "methodCall");
    let method = root
        .children()
        .find(|n| n.has_tag_name("methodName"))
        .unwrap()
        .text()
        .unwrap()
        .to_owned();
    let mut args = Vec::new();
    if let Some(params) = root.children().find(|n| n.has_tag_name("params")) {
        for param in params.children().filter(|n| n.is_element()) {
            let value = param.children().find(|n| n.has_tag_name("value")).unwrap();
            let inner = value.children().find(|n| n.is_element()).unwrap();
            args.push(match inner.tag_name().name() {
                "string" | "base64" => Arg::S(inner.text().unwrap_or("").to_owned()),
                "i4" | "i8" | "int" => Arg::I(inner.text().unwrap().parse().unwrap()),
                other => panic!("unexpected type {other}"),
            });
        }
    }
    (method, args)
}
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
enum V {
    S(String),
    I(i64),
    A(Vec<V>),
}
fn enc(v: &V) -> String {
    match v {
        V::S(s) => format!("<value><string>{}</string></value>", esc(s)),
        V::I(n) => format!("<value><i8>{n}</i8></value>"),
        V::A(items) => format!(
            "<value><array><data>{}</data></array></value>",
            items.iter().map(enc).collect::<String>()
        ),
    }
}
fn response(v: V) -> Response {
    (
        [("content-type", "text/xml")],
        format!(
            "<?xml version=\"1.0\"?><methodResponse><params><param>{}</param></params></methodResponse>",
            enc(&v)
        ),
    )
        .into_response()
}
fn fault() -> Response {
    "<?xml version=\"1.0\"?><methodResponse><fault><value><struct><member><name>faultCode</name><value><i4>-501</i4></value></member><member><name>faultString</name><value><string>Could not find info-hash.</string></value></member></struct></value></fault></methodResponse>".into_response()
}
fn field(item: &Item, name: &str) -> V {
    match name {
        "d.hash=" => V::S(item.hash.clone()),
        "d.name=" => V::S(item.name.clone()),
        "d.custom1=" => V::S(item.custom1.clone()),
        "d.directory=" => V::S(item.directory.clone()),
        "d.is_multi_file=" => V::I(item.multi as i64),
        "d.size_bytes=" => V::I(item.size),
        "d.completed_bytes=" => V::I(item.done),
        "d.state=" => V::I(item.state),
        "d.is_active=" => V::I(item.active),
        "d.complete=" => V::I(item.complete),
        "d.hashing=" => V::I(item.hashing),
        "d.message=" => V::S(item.message.clone()),
        "d.down.rate=" => V::I(2048),
        "d.up.rate=" => V::I(64),
        "d.ratio=" => V::I(item.ratio),
        other => panic!("unknown field {other}"),
    }
}
async fn handler(State(f): State<Fixture>, uri: Uri, headers: HeaderMap, body: Bytes) -> Response {
    let mut m = f.0.lock().unwrap();
    if uri.path() != "/RPC2" {
        return StatusCode::NOT_FOUND.into_response();
    }
    if m.auth {
        let expected = format!("Basic {}", b64(format!("{USER}:{PASSWORD}").as_bytes()));
        if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some(expected.as_str()) {
            return StatusCode::UNAUTHORIZED.into_response();
        }
    }
    assert_eq!(headers.get("content-type").unwrap(), "text/xml");
    if let Some((status, text)) = m.raw_reply.clone() {
        return (StatusCode::from_u16(status).unwrap(), text).into_response();
    }
    let (method, args) = parse_call(&body);
    m.calls.push((method.clone(), args.clone()));
    let find = |m: &Mock, hash: &str| {
        m.torrents
            .iter()
            .position(|t| t.hash.eq_ignore_ascii_case(hash))
    };
    match method.as_str() {
        "system.client_version" => response(V::S(m.version.clone())),
        "directory.default" => response(V::S("/downloads".into())),
        "d.multicall2" => {
            assert_eq!((args[0].s(), args[1].s()), ("", "main"));
            // Delayed loads surface after enough listings.
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
            let rows = m
                .torrents
                .iter()
                .map(|t| V::A(args[2..].iter().map(|a| field(t, a.s())).collect()))
                .collect();
            response(V::A(rows))
        }
        "d.name" | "d.is_multi_file" => match find(&m, args[0].s()) {
            Some(i) => {
                let t = &m.torrents[i];
                response(if method == "d.name" {
                    V::S(t.name.clone())
                } else {
                    V::I(t.multi as i64)
                })
            }
            None => fault(),
        },
        "f.multicall" => match find(&m, args[0].s()) {
            Some(i) => {
                let t = &m.torrents[i];
                response(V::A(
                    t.files
                        .iter()
                        .map(|(p, size, done, chunks, prio)| {
                            V::A(
                                args[2..]
                                    .iter()
                                    .map(|a| match a.s() {
                                        "f.path=" => V::S(p.clone()),
                                        "f.size_bytes=" => V::I(*size),
                                        "f.completed_chunks=" => V::I(*done),
                                        "f.size_chunks=" => V::I(*chunks),
                                        "f.priority=" => V::I(*prio),
                                        other => panic!("unknown file field {other}"),
                                    })
                                    .collect(),
                            )
                        })
                        .collect(),
                ))
            }
            None => fault(),
        },
        "load.start" | "load.normal" | "load.raw" | "load.raw_start" => {
            let raw = method.starts_with("load.raw");
            let hash = if raw {
                let encoded = args[1].s();
                m.metainfo
                    .iter()
                    .find(|(b, _)| b == encoded)
                    .map(|(_, h)| h.clone())
                    .expect("unknown torrent upload")
            } else {
                args[1].s().split("urn:btih:").nth(1).unwrap()[..40].to_lowercase()
            };
            let started = method.ends_with("start");
            let mut t = item(&hash, "Added", "");
            t.state = started as i64;
            t.active = started as i64;
            for command in &args[2..] {
                let command = command.s();
                if let Some(label) = command.strip_prefix("d.custom1.set=") {
                    t.custom1 = label.into();
                } else if command.starts_with("d.priority.set=") {
                    // Priority is set at load; the mock does not model it further.
                } else {
                    panic!("unexpected load command {command}");
                }
            }
            // rTorrent silently ignores a load for a hash it already holds.
            if find(&m, &hash).is_none() && !m.pending.iter().any(|(_, p)| p.hash == t.hash) {
                if m.delay_listings > 0 {
                    let n = m.delay_listings;
                    m.pending.push((n, t));
                } else {
                    m.torrents.push(t);
                }
            }
            if m.lose_load_response {
                return StatusCode::BAD_GATEWAY.into_response();
            }
            response(V::I(0))
        }
        "d.stop" | "d.start" | "d.erase" | "d.priority.set" | "d.custom1.set" => {
            match find(&m, args[0].s()) {
                Some(i) => {
                    match method.as_str() {
                        "d.stop" => {
                            m.torrents[i].state = 0;
                            m.torrents[i].active = 0;
                        }
                        "d.start" => {
                            m.torrents[i].state = 1;
                            m.torrents[i].active = 1;
                        }
                        "d.erase" => {
                            m.torrents.remove(i);
                        }
                        "d.custom1.set" => m.torrents[i].custom1 = args[1].s().into(),
                        _ => {}
                    }
                    response(V::I(0))
                }
                None => fault(),
            }
        }
        _ => fault(),
    }
}
async fn fixture(path: &str) -> (Fixture, String, tokio::task::JoinHandle<()>) {
    let fixture = Fixture(Arc::new(Mutex::new(Mock {
        version: "0.9.8".into(),
        auth: true,
        torrents: vec![],
        calls: vec![],
        delay_listings: 0,
        pending: vec![],
        lose_load_response: false,
        metainfo: vec![],
        raw_reply: None,
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
    ProviderSettings::Rtorrent {
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
async fn auth_version_gate_url_and_response_hardening() {
    let (f, endpoint, task) = fixture("/RPC2").await;
    let settings = settings(&endpoint);
    let client = HttpClient::new().unwrap();
    let operation = client.operation(Uuid::new_v4()).unwrap();
    let result = label_client::test_connection(&operation, &settings, Some(&login()))
        .await
        .unwrap();
    assert_eq!(result.client_version, "0.9.8");
    assert_eq!(result.domains, vec![MediaDomain::Tv, MediaDomain::Movies]);
    assert!(
        f.methods()
            .iter()
            .all(|m| !m.starts_with("load.") && !m.starts_with("d.erase")),
        "Test never mutates the client: {:?}",
        f.methods()
    );
    // Only username/password credentials (or none, when the proxy allows it) are meaningful.
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
    assert_eq!(
        label_client::test_connection(&operation, &settings, None)
            .await
            .unwrap_err()
            .error,
        QbitError::Http(HttpError::Authentication)
    );
    f.0.lock().unwrap().auth = false;
    label_client::test_connection(&operation, &settings, None)
        .await
        .unwrap();
    f.0.lock().unwrap().auth = true;
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
    // Version gate.
    f.0.lock().unwrap().version = "0.8.9".into();
    assert_eq!(
        label_client::test_connection(&operation, &settings, Some(&login()))
            .await
            .unwrap_err()
            .error,
        QbitError::UnsupportedVersion
    );
    f.0.lock().unwrap().version = "0.9.8".into();
    // A wrong path is a connection failure; an HTML page or a DTD-bearing document is not
    // accepted as a response; a fault is a rejection that is not reflected.
    let wrong_path = ProviderSettings::Rtorrent {
        endpoint: endpoint.replace("/RPC2", "/elsewhere"),
        tv: Some(scope("tv-hrr", TorrentQueuePriority::Last, false)),
        movies: None,
    };
    assert_eq!(
        label_client::test_connection(&operation, &wrong_path, Some(&login()))
            .await
            .unwrap_err()
            .error,
        QbitError::Http(HttpError::Transport)
    );
    for body in [
        "<html><body>Welcome to nginx</body></html>".to_owned(),
        "<?xml version=\"1.0\"?><!DOCTYPE r [<!ENTITY a \"aaaa\">]><methodResponse><params><param><value><string>&a;</string></value></param></params></methodResponse>".to_owned(),
    ] {
        f.0.lock().unwrap().raw_reply = Some((200, body));
        assert_eq!(
            label_client::test_connection(&operation, &settings, Some(&login()))
                .await
                .unwrap_err()
                .error,
            QbitError::InvalidResponse
        );
    }
    f.0.lock().unwrap().raw_reply = Some((200, "<?xml version=\"1.0\"?><methodResponse><fault><value><string>boom</string></value></fault></methodResponse>".into()));
    assert_eq!(
        label_client::test_connection(&operation, &settings, Some(&login()))
            .await
            .unwrap_err()
            .error,
        QbitError::Rejected
    );
    task.abort();
}

#[tokio::test]
async fn adds_magnet_and_torrent_per_domain_with_labels_priority_and_async_visibility() {
    let (f, endpoint, task) = fixture("/RPC2").await;
    let settings = settings(&endpoint);
    let client = HttpClient::new().unwrap();
    let operation = client.operation(Uuid::new_v4()).unwrap();
    let tv_hash = "0123456789abcdef0123456789abcdef01234567";
    let (bytes, movie_hash) = torrent("Film.2020.1080p");
    f.0.lock()
        .unwrap()
        .metainfo
        .push((b64(&bytes), movie_hash.clone()));
    // rTorrent lists a loaded torrent only after a short delay.
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
    // The label and priority are load-time commands; started loads are `load.start`/`load.raw_start`.
    assert_eq!(
        f.calls("load.start"),
        vec![vec![
            Arg::S(String::new()),
            Arg::S(magnet(tv_hash)),
            Arg::S("d.custom1.set=tv-hrr".into()),
            Arg::S("d.priority.set=3".into()),
        ]]
    );
    assert_eq!(
        f.calls("load.raw"),
        vec![vec![
            Arg::S(String::new()),
            Arg::S(b64(&bytes)),
            Arg::S("d.custom1.set=movies-hrr".into()),
        ]],
        "the paused movie is loaded without starting"
    );
    assert!(f.calls("load.raw_start").is_empty() && f.calls("load.normal").is_empty());
    assert_eq!(f.get(&movie_hash).state, 0, "movie added paused");
    assert_eq!(f.get(tv_hash).custom1, "tv-hrr");
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
    // The password never reaches a projected name.
    let mut leak = item(
        "5555555555555555555555555555555555555555",
        &format!("leak.{PASSWORD}.{USER}"),
        "tv-hrr",
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
    assert!(!text.contains(PASSWORD) && !text.contains(USER));
    task.abort();
}

#[tokio::test]
async fn duplicates_and_lost_responses_reconcile_by_hash_without_a_second_load() {
    let (f, endpoint, task) = fixture("/RPC2").await;
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
    assert_eq!(f.calls("load.start").len(), 1);
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
    assert_eq!(f.calls("load.start").len(), 1);

    // Lost response: the load ran (labelled at load time) but the reply never arrived.
    let lost_hash = "2222222222222222222222222222222222222222";
    f.0.lock().unwrap().lose_load_response = true;
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
    f.0.lock().unwrap().lose_load_response = false;
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
    assert_eq!(f.calls("load.start").len(), 2, "never a second load");

    // An unlabelled torrent with this hash (someone else's) is a visible conflict, never adopted
    // by a fresh grab.
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
    assert_eq!(f.calls("load.start").len(), 2);
    // Authentication failure before the load is definitely not a submission.
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
    assert_eq!(f.calls("load.start").len(), 2);
    task.abort();
}

#[tokio::test]
async fn status_mapping_isolation_details_and_controls() {
    let (f, endpoint, task) = fixture("/RPC2").await;
    let settings = settings(&endpoint);
    let client = HttpClient::new().unwrap();
    let operation = client.operation(Uuid::new_v4()).unwrap();
    let h = |n: u8| format!("{:040}", n);
    f.put(item(&h(1), "Downloading", "tv-hrr"));
    let mut seeding = item(&h(2), "Seeding", "tv-hrr");
    (seeding.complete, seeding.done) = (1, 1048576);
    f.put(seeding);
    let mut paused = item(&h(3), "Paused", "tv-hrr");
    paused.state = 0;
    paused.active = 0;
    f.put(paused);
    let mut queued = item(&h(4), "Queued", "tv-hrr");
    queued.active = 0;
    f.put(queued);
    let mut errored = item(&h(5), "Errored", "tv-hrr");
    errored.message =
        "Download registered as completed, but hash check returned unfinished chunks.".into();
    f.put(errored);
    let mut tracker = item(&h(6), "TrackerNote", "tv-hrr");
    tracker.message = "Tracker: [Failure reason \"unregistered torrent\"]".into();
    f.put(tracker);
    let mut hashing = item(&h(7), "Checking", "tv-hrr");
    hashing.hashing = 1;
    f.put(hashing);
    let mut paused_done = item(&h(8), "PausedDone", "tv-hrr");
    (
        paused_done.state,
        paused_done.active,
        paused_done.complete,
        paused_done.done,
    ) = (0, 0, 1, 1048576);
    f.put(paused_done);
    f.put(item(&h(9), "Foreign", "other"));
    f.put(item(&h(10), "MovieOne", "movies-hrr"));
    f.put(item(&h(11), "Unlabelled", ""));
    let mut magnet_item = item(&h(12), "Magnet", "tv-hrr");
    magnet_item.size = 0;
    f.put(magnet_item);
    // ruTorrent stores labels URL-encoded; the decoded label is compared.
    f.put(item(&h(13), "Encoded", "tv-hrr%2Dx"));
    let mut multi = item(&h(14), "Multi.Pack", "tv-hrr");
    (multi.multi, multi.directory, multi.complete, multi.done) =
        (true, "/downloads/Multi.Pack".into(), 1, 1048576);
    multi.files = vec![
        ("Multi.Pack.S01E01.mkv".into(), 1048000, 4, 4, 1),
        ("sample.mkv".into(), 576, 0, 1, 0),
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
    assert_eq!(
        status(6),
        (&DownloadStatus::Downloading, &None, false),
        "a tracker note is not a torrent error"
    );
    assert_eq!(status(7), (&DownloadStatus::Queued, &None, false));
    assert_eq!(status(8), (&DownloadStatus::Completed, &None, true));
    assert_eq!(
        status(12),
        (
            &DownloadStatus::Queued,
            &Some(DownloadDiagnostic::Metadata),
            false
        )
    );
    assert!(
        page.items
            .iter()
            .all(|i| ![h(9), h(10), h(11), h(13)].contains(&i.hash))
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

    // Details: a multi-file torrent's save path is the folder that contains its own directory.
    let details = label_client::details(
        &operation,
        &settings,
        Some(&login()),
        &MediaTarget::Episode(1),
        &h(14),
    )
    .await
    .unwrap();
    assert_eq!(details.save_path, "/downloads");
    assert_eq!(
        details
            .files
            .iter()
            .map(|file| (file.name.as_str(), file.priority))
            .collect::<Vec<_>>(),
        vec![
            ("Multi.Pack/Multi.Pack.S01E01.mkv", 1),
            ("Multi.Pack/sample.mkv", 0)
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
            &h(11)
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

    // Controls.
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
    assert_eq!(f.get(&h(1)).state, 0);
    assert_eq!(
        control(MediaTarget::Episode(1), h(1), Control::Resume).await,
        Ok(())
    );
    assert_eq!(f.get(&h(1)).state, 1);
    assert_eq!(
        control(
            MediaTarget::Episode(1),
            h(1),
            Control::Priority { first: true }
        )
        .await,
        Ok(())
    );
    assert_eq!(
        f.calls("d.priority.set").last(),
        Some(&vec![Arg::S(h(1)), Arg::I(3)])
    );
    assert_eq!(
        control(MediaTarget::Movie(1), h(1), Control::Pause).await,
        Err(QbitError::ScopeConflict)
    );
    // rTorrent cannot delete data over XML-RPC; that is an explicit refusal, nothing is erased.
    assert_eq!(
        control(
            MediaTarget::Episode(1),
            h(2),
            Control::Remove { delete_files: true }
        )
        .await,
        Err(QbitError::UnsupportedFeature)
    );
    assert!(f.calls("d.erase").is_empty());
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
    assert_eq!(f.calls("d.erase").len(), 1);
    task.abort();
}
