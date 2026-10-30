//! Aria2 JSON-RPC wire contract against an owned mock on 127.0.0.1:0 (never a live aria2c or its
//! default port 6800). Method names, argument shapes, string-typed numbers, the `token:` secret and
//! the `Unauthorized` error follow aria2's published RPC manual; the mock is written from that
//! manual, not recorded from a running aria2.
use axum::{
    Router,
    body::Bytes,
    extract::State,
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

const SECRET: &str = "PRIVATE_ARIA2_SECRET";
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
struct Dl {
    gid: String,
    status: String,
    hash: String,
    dir: String,
    name: String,
    total: u64,
    completed: u64,
    followed_by: Vec<String>,
    /// (absolute path, length, completed, selected)
    files: Vec<(String, u64, u64, bool)>,
}
fn dl(gid: &str, hash: &str, name: &str, dir: &str, status: &str) -> Dl {
    Dl {
        gid: gid.into(),
        status: status.into(),
        hash: hash.into(),
        dir: dir.into(),
        name: name.into(),
        total: 1048576,
        completed: 524288,
        followed_by: vec![],
        files: vec![(format!("{dir}/{name}"), 1048576, 524288, true)],
    }
}
struct Mock {
    version: String,
    secret: bool,
    downloads: Vec<Dl>,
    calls: Vec<(String, Vec<Value>)>,
    next_gid: u32,
    lose_add_response: bool,
    metainfo: Vec<(String, String)>,
}
#[derive(Clone)]
struct Fixture(Arc<Mutex<Mock>>);
impl Fixture {
    fn calls(&self, method: &str) -> Vec<Vec<Value>> {
        self.0
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, p)| p.clone())
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
    fn by_hash(&self, hash: &str) -> Dl {
        self.0
            .lock()
            .unwrap()
            .downloads
            .iter()
            .find(|d| d.hash == hash)
            .cloned()
            .unwrap()
    }
    fn put(&self, d: Dl) {
        self.0.lock().unwrap().downloads.push(d);
    }
}
fn status(d: &Dl, keys: &[Value]) -> Value {
    let mut out = serde_json::Map::new();
    for key in keys {
        let key = key.as_str().unwrap();
        let value = match key {
            "gid" => json!(d.gid),
            "status" => json!(d.status),
            "totalLength" => json!(d.total.to_string()),
            "completedLength" => json!(d.completed.to_string()),
            "uploadLength" => json!("262144"),
            "downloadSpeed" => json!("2048"),
            "uploadSpeed" => json!("64"),
            "infoHash" => json!(d.hash),
            "dir" => json!(d.dir),
            "bittorrent" => {
                if d.name.is_empty() {
                    json!({})
                } else {
                    json!({"info":{"name":d.name}})
                }
            }
            "followedBy" => json!(d.followed_by),
            "errorCode" => json!("0"),
            "seeder" => json!("false"),
            "files" => json!(
                d.files
                    .iter()
                    .map(
                        |(p, l, c, s)| json!({"index":"1","path":p,"length":l.to_string(),
                    "completedLength":c.to_string(),"selected":s.to_string()})
                    )
                    .collect::<Vec<_>>()
            ),
            other => panic!("unknown key {other}"),
        };
        out.insert(key.into(), value);
    }
    Value::Object(out)
}
async fn handler(State(f): State<Fixture>, body: Bytes) -> Response {
    let mut m = f.0.lock().unwrap();
    let request: Value = serde_json::from_slice(&body).unwrap();
    let id = request["id"].clone();
    let method = request["method"].as_str().unwrap().to_owned();
    let mut params = request["params"].as_array().unwrap().clone();
    let ok = |r: Value| axum::Json(json!({"id":id,"jsonrpc":"2.0","result":r})).into_response();
    let err = |message: &str, code: i64| {
        (
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(json!({"id":id,"jsonrpc":"2.0","error":{"code":code,"message":message}})),
        )
            .into_response()
    };
    if m.secret {
        if params.first().and_then(Value::as_str) != Some(&format!("token:{SECRET}")) {
            return err("Unauthorized", 1);
        }
        params.remove(0);
    }
    m.calls.push((method.clone(), params.clone()));
    match method.as_str() {
        "aria2.getVersion" => ok(json!({"version":m.version,"enabledFeatures":["BitTorrent"]})),
        "aria2.getGlobalOption" => ok(json!({"dir":DEFAULT_DIR,"max-concurrent-downloads":"5"})),
        "aria2.tellActive" => ok(json!(
            m.downloads
                .iter()
                .filter(|d| d.status == "active")
                .map(|d| status(d, params[0].as_array().unwrap()))
                .collect::<Vec<_>>()
        )),
        "aria2.tellWaiting" => ok(json!(
            m.downloads
                .iter()
                .filter(|d| matches!(d.status.as_str(), "waiting" | "paused"))
                .skip(params[0].as_u64().unwrap() as usize)
                .take(params[1].as_u64().unwrap() as usize)
                .map(|d| status(d, params[2].as_array().unwrap()))
                .collect::<Vec<_>>()
        )),
        "aria2.tellStopped" => ok(json!(
            m.downloads
                .iter()
                .filter(|d| matches!(d.status.as_str(), "complete" | "error" | "removed"))
                .skip(params[0].as_u64().unwrap() as usize)
                .take(params[1].as_u64().unwrap() as usize)
                .map(|d| status(d, params[2].as_array().unwrap()))
                .collect::<Vec<_>>()
        )),
        "aria2.tellStatus" => {
            let gid = params[0].as_str().unwrap();
            match m.downloads.iter().find(|d| d.gid == gid) {
                Some(d) => ok(status(d, params[1].as_array().unwrap())),
                None => err("GID not found", 1),
            }
        }
        "aria2.addUri" | "aria2.addTorrent" => {
            let (hash, options) = if method == "aria2.addUri" {
                (
                    params[0][0]
                        .as_str()
                        .unwrap()
                        .split("urn:btih:")
                        .nth(1)
                        .unwrap()[..40]
                        .to_lowercase(),
                    params[1].clone(),
                )
            } else {
                let encoded = params[0].as_str().unwrap();
                (
                    m.metainfo
                        .iter()
                        .find(|(b, _)| b == encoded)
                        .map(|(_, h)| h.clone())
                        .expect("unknown torrent"),
                    params[2].clone(),
                )
            };
            if m.downloads
                .iter()
                .any(|d| d.hash == hash && d.followed_by.is_empty())
            {
                return err("InfoHash is already registered.", 1);
            }
            m.next_gid += 1;
            let gid = format!("{:016x}", m.next_gid);
            let paused = options["pause"] == "true";
            let mut d = dl(
                &gid,
                &hash,
                "Added",
                options["dir"].as_str().unwrap(),
                if paused { "paused" } else { "active" },
            );
            d.completed = 0;
            m.downloads.push(d);
            if m.lose_add_response {
                return axum::http::StatusCode::BAD_GATEWAY.into_response();
            }
            ok(json!(gid))
        }
        "aria2.changePosition" => ok(json!(0)),
        "aria2.pause" | "aria2.unpause" | "aria2.remove" | "aria2.removeDownloadResult" => {
            let gid = params[0].as_str().unwrap().to_owned();
            let Some(i) = m.downloads.iter().position(|d| d.gid == gid) else {
                return err("GID not found", 1);
            };
            match method.as_str() {
                "aria2.pause" => m.downloads[i].status = "paused".into(),
                "aria2.unpause" => m.downloads[i].status = "active".into(),
                "aria2.remove" => {
                    if matches!(
                        m.downloads[i].status.as_str(),
                        "complete" | "error" | "removed"
                    ) {
                        return err("Active Download not found for GID", 1);
                    }
                    m.downloads[i].status = "removed".into();
                }
                _ => {
                    m.downloads.remove(i);
                }
            }
            ok(json!(gid))
        }
        _ => err("Unknown method", 1),
    }
}
async fn fixture() -> (Fixture, String, tokio::task::JoinHandle<()>) {
    let fixture = Fixture(Arc::new(Mutex::new(Mock {
        version: "1.36.0".into(),
        secret: true,
        downloads: vec![],
        calls: vec![],
        next_gid: 0,
        lose_add_response: false,
        metainfo: vec![],
    })));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    assert_ne!(listener.local_addr().unwrap().port(), 6800);
    let endpoint = format!("http://{}/jsonrpc", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/jsonrpc", axum::routing::post(handler))
        .with_state(fixture.clone());
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
    ProviderSettings::Aria2 {
        endpoint: endpoint.into(),
        tv: Some(scope("tv-hrr", TorrentQueuePriority::First, false)),
        movies: Some(scope("movies-hrr", TorrentQueuePriority::Last, true)),
    }
}
fn login() -> Credentials {
    Credentials::ApiKey {
        api_key: SECRET.into(),
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
async fn secret_version_gate_and_default_directory() {
    let (f, endpoint, task) = fixture().await;
    let settings = settings(&endpoint);
    let client = HttpClient::new().unwrap();
    let operation = client.operation(Uuid::new_v4()).unwrap();
    let result = label_client::test_connection(&operation, &settings, Some(&login()))
        .await
        .unwrap();
    assert_eq!(result.client_version, "1.36.0");
    assert_eq!(result.domains, vec![MediaDomain::Tv, MediaDomain::Movies]);
    assert!(
        f.methods()
            .iter()
            .all(|m| !m.starts_with("aria2.add") && m != "aria2.remove"),
        "Test never mutates the client: {:?}",
        f.methods()
    );
    // The secret is the leading `token:` parameter of every call; username/password is not a
    // valid credential kind.
    assert_eq!(
        label_client::test_connection(
            &operation,
            &settings,
            Some(&Credentials::UsernamePassword {
                username: "u".into(),
                password: "p".into()
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
    let wrong = Credentials::ApiKey {
        api_key: "wrong".into(),
    };
    assert_eq!(
        label_client::test_connection(&operation, &settings, Some(&wrong))
            .await
            .unwrap_err()
            .error,
        QbitError::Http(HttpError::Authentication)
    );
    // Without an RPC secret no credential is needed.
    f.0.lock().unwrap().secret = false;
    label_client::test_connection(&operation, &settings, None)
        .await
        .unwrap();
    f.0.lock().unwrap().version = "1.17.1".into();
    assert_eq!(
        label_client::test_connection(&operation, &settings, None)
            .await
            .unwrap_err()
            .error,
        QbitError::UnsupportedVersion
    );
    // A wrong path is a connection failure.
    let wrong_path = ProviderSettings::Aria2 {
        endpoint: endpoint.replace("/jsonrpc", "/elsewhere"),
        tv: Some(scope("tv-hrr", TorrentQueuePriority::Last, false)),
        movies: None,
    };
    assert_eq!(
        label_client::test_connection(&operation, &wrong_path, None)
            .await
            .unwrap_err()
            .error,
        QbitError::Http(HttpError::Transport)
    );
    task.abort();
}

#[tokio::test]
async fn adds_magnet_and_torrent_into_label_subfolders() {
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
    // Each domain downloads into its own subfolder of aria2's default directory.
    assert_eq!(
        f.calls("aria2.addUri"),
        vec![vec![
            json!([magnet(tv_hash)]),
            json!({"dir":"/downloads/tv-hrr"})
        ]]
    );
    assert_eq!(
        f.calls("aria2.addTorrent"),
        vec![vec![
            json!(b64(&bytes)),
            json!([]),
            json!({"dir":"/downloads/movies-hrr","pause":"true"})
        ]]
    );
    // Only the recent TV target with priority "first" is moved to the queue front.
    let moves = f.calls("aria2.changePosition");
    assert_eq!(moves.len(), 1);
    assert_eq!(
        (moves[0][1].clone(), moves[0][2].clone()),
        (json!(0), json!("POS_SET"))
    );
    assert_eq!(f.by_hash(&movie_hash).status, "paused");
    // Each domain observes only its own subfolder.
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
    // The secret never reaches a projected name.
    let mut leak = dl(
        "00000000000000ff",
        "5555555555555555555555555555555555555555",
        &format!("leak.{SECRET}"),
        "/downloads/tv-hrr",
        "active",
    );
    leak.total = 10;
    f.put(leak);
    let page = label_client::query(
        &operation,
        &settings,
        Some(&login()),
        &query(MediaDomain::Tv),
    )
    .await
    .unwrap();
    assert!(!serde_json::to_string(&page).unwrap().contains(SECRET));
    task.abort();
}

#[tokio::test]
async fn duplicates_and_lost_responses_reconcile_by_hash_without_a_second_add() {
    let (f, endpoint, task) = fixture().await;
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
    assert_eq!(f.calls("aria2.addUri").len(), 1);
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
    assert_eq!(f.calls("aria2.addUri").len(), 1);

    // Lost response: aria2 accepted the download (into the right subfolder) but the reply never
    // arrived, so reconciliation observes it directly.
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
    assert_eq!(f.calls("aria2.addUri").len(), 2, "never a second add");
    // The same hash downloading outside any domain subfolder is a visible conflict and aria2's
    // own duplicate refusal is never worked around.
    f.put(dl(
        "00000000000000aa",
        "3333333333333333333333333333333333333333",
        "Mine.Not",
        "/elsewhere",
        "active",
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
    assert_eq!(f.calls("aria2.addUri").len(), 2);
    let denied = prepare(
        MediaTarget::Episode(4),
        "4444444444444444444444444444444444444444",
    )
    .await;
    let wrong = Credentials::ApiKey {
        api_key: "wrong".into(),
    };
    assert_eq!(
        label_client::submit(&operation, &settings, Some(&wrong), denied)
            .await
            .unwrap_err(),
        QbitError::Http(HttpError::Authentication)
    );
    assert_eq!(f.calls("aria2.addUri").len(), 2);
    task.abort();
}

#[tokio::test]
async fn status_mapping_isolation_details_and_controls() {
    let (f, endpoint, task) = fixture().await;
    let settings = settings(&endpoint);
    let client = HttpClient::new().unwrap();
    let operation = client.operation(Uuid::new_v4()).unwrap();
    let h = |n: u8| format!("{:040}", n);
    let tv = "/downloads/tv-hrr";
    f.put(dl("0000000000000001", &h(1), "Downloading", tv, "active"));
    let mut seeding = dl("0000000000000002", &h(2), "Seeding", tv, "active");
    seeding.completed = seeding.total;
    f.put(seeding);
    f.put(dl("0000000000000003", &h(3), "Paused", tv, "paused"));
    f.put(dl("0000000000000004", &h(4), "Queued", tv, "waiting"));
    f.put(dl("0000000000000005", &h(5), "Errored", tv, "error"));
    let mut done = dl("0000000000000006", &h(6), "Finished", tv, "complete");
    done.completed = done.total;
    f.put(done);
    f.put(dl(
        "0000000000000007",
        &h(7),
        "Foreign",
        "/downloads/other",
        "active",
    ));
    f.put(dl(
        "0000000000000008",
        &h(8),
        "MovieOne",
        "/downloads/movies-hrr",
        "active",
    ));
    f.put(dl(
        "0000000000000009",
        &h(9),
        "Nested",
        "/downloads/tv-hrr/sub",
        "active",
    ));
    // A magnet still fetching its metadata has no name or size.
    let mut meta = dl("000000000000000a", &h(10), "", tv, "active");
    (meta.total, meta.completed, meta.files) = (0, 0, vec![]);
    f.put(meta);
    // A completed metadata stub is replaced by the real download with the same hash: only the
    // real one is listed.
    let mut stub = dl("000000000000000b", &h(11), "", tv, "complete");
    (stub.total, stub.completed, stub.files, stub.followed_by) =
        (0, 0, vec![], vec!["000000000000000c".into()]);
    f.put(stub);
    f.put(dl("000000000000000c", &h(11), "FromMagnet", tv, "active"));
    // A multi-file torrent: absolute file paths become paths relative to the directory.
    let mut multi = dl("000000000000000d", &h(12), "Multi.Pack", tv, "complete");
    multi.completed = multi.total;
    multi.files = vec![
        (
            format!("{tv}/Multi.Pack/Multi.Pack.S01E01.mkv"),
            1048000,
            1048000,
            true,
        ),
        (format!("{tv}/Multi.Pack/sample.mkv"), 576, 0, false),
    ];
    f.put(multi);
    // A non-BitTorrent download has no info hash and is ignored.
    let mut http = dl("000000000000000e", "", "plain.bin", tv, "active");
    http.hash = String::new();
    f.put(http);

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
    assert_eq!(status(6), (&DownloadStatus::Completed, &None, true));
    assert_eq!(
        status(10),
        (
            &DownloadStatus::Queued,
            &Some(DownloadDiagnostic::Metadata),
            false
        )
    );
    assert_eq!(
        page.items.iter().filter(|i| i.hash == h(11)).count(),
        1,
        "the stub and its replacement are one download"
    );
    assert_eq!(
        page.items.iter().find(|i| i.hash == h(11)).unwrap().name,
        "FromMagnet"
    );
    assert!(
        page.items
            .iter()
            .all(|i| ![h(7), h(8), h(9)].contains(&i.hash))
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
        vec![h(8)]
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
    assert_eq!(details.save_path, tv);
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
            &h(7)
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
    assert_eq!(f.by_hash(&h(1)).status, "paused");
    assert_eq!(
        control(MediaTarget::Episode(1), h(1), Control::Resume).await,
        Ok(())
    );
    assert_eq!(f.by_hash(&h(1)).status, "active");
    assert_eq!(
        control(
            MediaTarget::Episode(1),
            h(4),
            Control::Priority { first: true }
        )
        .await,
        Ok(())
    );
    assert_eq!(
        control(MediaTarget::Movie(1), h(1), Control::Pause).await,
        Err(QbitError::ScopeConflict)
    );
    // aria2 cannot delete data over RPC: an explicit refusal, nothing is removed.
    assert_eq!(
        control(
            MediaTarget::Episode(1),
            h(1),
            Control::Remove { delete_files: true }
        )
        .await,
        Err(QbitError::UnsupportedFeature)
    );
    assert!(f.calls("aria2.remove").is_empty());
    // Removing an active download stops it and drops its result; a finished one only its result.
    assert_eq!(
        control(
            MediaTarget::Episode(1),
            h(1),
            Control::Remove {
                delete_files: false
            }
        )
        .await,
        Ok(())
    );
    assert_eq!(
        control(
            MediaTarget::Episode(1),
            h(6),
            Control::Remove {
                delete_files: false
            }
        )
        .await,
        Ok(())
    );
    assert_eq!(f.calls("aria2.removeDownloadResult").len(), 2);
    task.abort();
}

#[tokio::test]
async fn full_inventory_windows_fail_instead_of_hiding_tracked_torrents() {
    for status in ["waiting", "complete"] {
        let (f, endpoint, task) = fixture().await;
        let settings = settings(&endpoint);
        let client = HttpClient::new().unwrap();
        let hash = "a".repeat(40);
        // Unrelated HTTP downloads consume the window before filtering by hash or domain.
        for n in 0..1000 {
            f.put(dl(
                &format!("{n:016x}"),
                "",
                "plain.bin",
                "/downloads/other",
                status,
            ));
        }
        f.put(dl(
            "000000000000ffff",
            &hash,
            "Tracked",
            "/downloads/tv-hrr",
            status,
        ));
        let operation = client.operation(Uuid::new_v4()).unwrap();
        assert_eq!(
            label_client::query(
                &operation,
                &settings,
                Some(&login()),
                &query(MediaDomain::Tv)
            )
            .await
            .err()
            .unwrap(),
            QbitError::Http(HttpError::ResponseTooLarge)
        );
        // The per-hash lookup must fail too, rather than reporting the torrent absent.
        let operation = client.operation(Uuid::new_v4()).unwrap();
        assert_eq!(
            label_client::details(
                &operation,
                &settings,
                Some(&login()),
                &MediaTarget::Episode(1),
                &hash
            )
            .await
            .err()
            .unwrap(),
            QbitError::Http(HttpError::ResponseTooLarge)
        );
        // Exactly a full window is ambiguous; below it a complete inventory is safe.
        f.0.lock().unwrap().downloads.truncate(1000);
        let operation = client.operation(Uuid::new_v4()).unwrap();
        assert_eq!(
            label_client::query(
                &operation,
                &settings,
                Some(&login()),
                &query(MediaDomain::Tv)
            )
            .await
            .err()
            .unwrap(),
            QbitError::Http(HttpError::ResponseTooLarge)
        );
        f.0.lock().unwrap().downloads.pop();
        let operation = client.operation(Uuid::new_v4()).unwrap();
        assert!(
            label_client::query(
                &operation,
                &settings,
                Some(&login()),
                &query(MediaDomain::Tv)
            )
            .await
            .unwrap()
            .items
            .is_empty()
        );
        task.abort();
    }
}
