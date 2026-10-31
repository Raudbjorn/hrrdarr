//! Deluge Web UI JSON-RPC wire contract against an owned mock on 127.0.0.1:0 (never port 8112 or a
//! live daemon). Method names, argument shapes and the `{result,error,id}` envelope follow
//! Deluge's published web/core/label RPC; the mock is written from that interface, not recorded
//! from a running Deluge.
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
        label_client::{self, LabelProblem},
        qbittorrent::{
            AddOutcome, AddSource, Control, DownloadDiagnostic, DownloadQuery, DownloadStatus,
            QbitError, QbitOptions, Reconciliation, SubmissionIdentity,
        },
    },
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

const PASSWORD: &str = "PRIVATE_DELUGE_PASSWORD";
const COOKIE: &str = "_session_id=PRIVATE_SESSION_TOKEN";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
/// Independent RFC 4648 encoder for asserting the uploaded `.torrent` filedump.
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

struct Mock {
    version: String,
    default_dir: String,
    label_plugin: bool,
    connected: bool,
    hosts: Vec<Value>,
    labels: Vec<String>,
    /// hash -> status record (the fields get_torrents_status projects).
    torrents: Vec<(String, Value)>,
    calls: Vec<(String, Value, bool)>,
    lose_add_response: bool,
    race_add: bool,
    metainfo: Vec<(String, String)>,
    queue: Vec<String>,
}
#[derive(Clone)]
struct Fixture(Arc<Mutex<Mock>>);
impl Fixture {
    fn calls(&self, method: &str) -> Vec<Value> {
        self.0
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|(m, _, _)| m == method)
            .map(|(_, a, _)| a.clone())
            .collect()
    }
    fn methods(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .calls
            .iter()
            .map(|(m, _, _)| m.clone())
            .collect()
    }
    fn clear(&self) {
        self.0.lock().unwrap().calls.clear();
    }
    fn torrent(&self, hash: &str) -> Value {
        self.0
            .lock()
            .unwrap()
            .torrents
            .iter()
            .find(|(h, _)| h == hash)
            .map(|(_, v)| v.clone())
            .unwrap()
    }
    fn put(&self, hash: &str, name: &str, label: &str, state: &str, done: u64) {
        self.0
            .lock()
            .unwrap()
            .torrents
            .push((hash.into(), record(name, label, state, done)));
    }
}
fn record(name: &str, label: &str, state: &str, done: u64) -> Value {
    json!({"name":name,"state":state,"progress":done as f64 / 1048576.0 * 100.0,
        "total_wanted":1048576,"total_done":done,"save_path":"/downloads",
        "download_payload_rate":2048,"upload_payload_rate":64,"eta":42,"ratio":0.25,
        "seeding_time":0,"label":label,"is_finished":done==1048576,
        "files":[{"index":0,"path":format!("{name}/{name}.mkv"),"size":1048576,"offset":0},
                 {"index":1,"path":format!("{name}/sample.mkv"),"size":10,"offset":1048576}],
        "file_progress":[done as f64 / 1048576.0, 0.0],"file_priorities":[1,0]})
}
fn project(record: &Value, keys: &[Value]) -> Value {
    let mut out = serde_json::Map::new();
    for key in keys {
        let key = key.as_str().unwrap();
        if let Some(v) = record.get(key) {
            out.insert(key.into(), v.clone());
        }
    }
    Value::Object(out)
}
async fn handler(State(f): State<Fixture>, uri: Uri, headers: HeaderMap, body: Bytes) -> Response {
    let mut m = f.0.lock().unwrap();
    if uri.path() != "/deluge/json" {
        return StatusCode::NOT_FOUND.into_response();
    }
    let request: Value = serde_json::from_slice(&body).unwrap();
    let method = request["method"].as_str().unwrap().to_owned();
    let params = request["params"].clone();
    let cookie = headers.get("cookie").and_then(|v| v.to_str().ok()) == Some(COOKIE);
    m.calls.push((method.clone(), params.clone(), cookie));
    let id = request["id"].clone();
    let ok =
        |result: Value| axum::Json(json!({"result":result,"error":null,"id":id})).into_response();
    let err = |message: &str, code: i64| {
        axum::Json(json!({"result":null,"error":{"message":message,"code":code},"id":id}))
            .into_response()
    };
    if method == "auth.login" {
        let good = params[0] == PASSWORD;
        let mut response = ok(json!(good));
        if good {
            response.headers_mut().insert(
                "set-cookie",
                format!("{COOKIE}; Path=/; HttpOnly").parse().unwrap(),
            );
        }
        return response;
    }
    if !cookie {
        return err("Not authenticated", 1);
    }
    match method.as_str() {
        "web.connected" => ok(json!(m.connected)),
        "web.get_hosts" => ok(json!(m.hosts)),
        "web.connect" => {
            m.connected = true;
            ok(json!([]))
        }
        "daemon.get_version" => ok(json!(m.version)),
        "core.get_config_value" => ok(json!(m.default_dir)),
        "label.get_labels" | "label.add" | "label.set_torrent" if !m.label_plugin => {
            err("Unknown method", 3)
        }
        "label.get_labels" => ok(json!(m.labels)),
        "label.add" => {
            let label = params[0].as_str().unwrap().to_owned();
            if m.labels.contains(&label) {
                return err("Label already exists", 4);
            }
            m.labels.push(label);
            ok(Value::Null)
        }
        "label.set_torrent" => {
            let hash = params[0].as_str().unwrap().to_owned();
            let label = params[1].clone();
            if !m.labels.iter().any(|l| label == l.as_str()) {
                return err("Unknown Label", 4);
            }
            for (h, t) in m.torrents.iter_mut() {
                if *h == hash {
                    t["label"] = label.clone();
                }
            }
            ok(Value::Null)
        }
        "core.get_torrents_status" => {
            let filter = &params[0];
            let keys = params[1].as_array().unwrap().clone();
            let mut out = serde_json::Map::new();
            for (hash, t) in &m.torrents {
                if let Some(ids) = filter.get("id").and_then(Value::as_array) {
                    if !ids.iter().any(|i| i == hash.as_str()) {
                        continue;
                    }
                }
                if let Some(label) = filter.get("label").and_then(Value::as_str) {
                    if t["label"] != label {
                        continue;
                    }
                }
                out.insert(hash.clone(), project(t, &keys));
            }
            ok(Value::Object(out))
        }
        "core.get_torrent_status" => {
            let hash = params[0].as_str().unwrap();
            let keys = params[1].as_array().unwrap().clone();
            match m.torrents.iter().find(|(h, _)| h == hash) {
                Some((_, t)) => ok(project(t, &keys)),
                None => ok(json!({})),
            }
        }
        "core.add_torrent_magnet" | "core.add_torrent_file" => {
            let (hash, options) = if method == "core.add_torrent_magnet" {
                (
                    params[0]
                        .as_str()
                        .unwrap()
                        .split("urn:btih:")
                        .nth(1)
                        .unwrap()[..40]
                        .to_lowercase(),
                    params[1].clone(),
                )
            } else {
                let encoded = params[1].as_str().unwrap();
                (
                    m.metainfo
                        .iter()
                        .find(|(b, _)| b == encoded)
                        .map(|(_, h)| h.clone())
                        .expect("unknown filedump"),
                    params[2].clone(),
                )
            };
            if m.race_add {
                // Another agent adds the same torrent unlabelled just before this request lands.
                m.torrents
                    .push((hash.clone(), record("raced", "", "Downloading", 0)));
            }
            if m.torrents.iter().any(|(h, _)| *h == hash) {
                return err("Torrent already in session", 5);
            }
            let paused = options["add_paused"].as_bool().unwrap();
            m.torrents.push((
                hash.clone(),
                record(
                    "Added",
                    "",
                    if paused { "Paused" } else { "Downloading" },
                    524288,
                ),
            ));
            m.queue.push(hash.clone());
            if m.lose_add_response {
                return StatusCode::BAD_GATEWAY.into_response();
            }
            ok(json!(hash))
        }
        "core.queue_top" => {
            let hash = params[0][0].as_str().unwrap().to_owned();
            m.queue.retain(|h| *h != hash);
            m.queue.insert(0, hash);
            ok(Value::Null)
        }
        "core.queue_bottom" => ok(Value::Null),
        "core.pause_torrent" | "core.resume_torrent" => {
            let hash = params[0][0].as_str().unwrap().to_owned();
            for (h, t) in m.torrents.iter_mut() {
                if *h == hash {
                    t["state"] = json!(if method == "core.pause_torrent" {
                        "Paused"
                    } else {
                        "Downloading"
                    });
                }
            }
            ok(Value::Null)
        }
        "core.remove_torrent" => {
            let hash = params[0].as_str().unwrap().to_owned();
            m.torrents.retain(|(h, _)| *h != hash);
            ok(json!(true))
        }
        _ => err("Unknown method", 3),
    }
}
async fn fixture(base: &str) -> (Fixture, String, tokio::task::JoinHandle<()>) {
    let fixture = Fixture(Arc::new(Mutex::new(Mock {
        version: "2.1.1".into(),
        default_dir: "/downloads".into(),
        label_plugin: true,
        connected: true,
        hosts: vec![json!(["host-one", "127.0.0.1", 58846, "Online"])],
        labels: vec![],
        torrents: vec![],
        calls: vec![],
        lose_add_response: false,
        race_add: false,
        metainfo: vec![],
        queue: vec![],
    })));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    assert_ne!(listener.local_addr().unwrap().port(), 8112);
    let endpoint = format!("http://{}{base}", listener.local_addr().unwrap());
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
    ProviderSettings::Deluge {
        endpoint: endpoint.into(),
        tv: Some(scope("tv-hrr", TorrentQueuePriority::First, false)),
        movies: Some(scope("movies-hrr", TorrentQueuePriority::Last, true)),
    }
}
fn login() -> Credentials {
    Credentials::ApiKey {
        api_key: PASSWORD.into(),
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
async fn login_daemon_attach_version_gate_and_label_plugin() {
    let (f, endpoint, task) = fixture("/deluge").await;
    let settings = settings(&endpoint);
    let client = HttpClient::new().unwrap();
    let operation = client.operation(Uuid::new_v4()).unwrap();
    let result = label_client::test_connection(&operation, &settings, Some(&login()))
        .await
        .unwrap();
    assert_eq!(result.client_version, "2.1.1");
    assert_eq!(result.domains, vec![MediaDomain::Tv, MediaDomain::Movies]);
    // The session cookie is replayed after login and never before it.
    let calls = f.0.lock().unwrap().calls.clone();
    assert_eq!(calls[0].0, "auth.login");
    assert!(!calls[0].2);
    assert!(calls[1..].iter().all(|(_, _, cookie)| *cookie));
    assert!(
        f.methods()
            .iter()
            .all(|m| !m.starts_with("core.add") && m != "label.add"),
        "Test never mutates the client: {:?}",
        f.methods()
    );
    // The configured password is the only credential kind.
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
        QbitError::InvalidRequest
    );
    // A wrong password is an authentication failure, not a rejection.
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
    // A Web UI that is not attached to a daemon is attached to the first known host.
    f.0.lock().unwrap().connected = false;
    f.clear();
    label_client::test_connection(&operation, &settings, Some(&login()))
        .await
        .unwrap();
    assert_eq!(f.calls("web.connect"), vec![json!(["host-one"])]);
    // No hosts at all is a connection failure.
    {
        let mut m = f.0.lock().unwrap();
        m.connected = false;
        m.hosts.clear();
    }
    assert_eq!(
        label_client::test_connection(&operation, &settings, Some(&login()))
            .await
            .unwrap_err()
            .error,
        QbitError::Http(HttpError::Transport)
    );
    {
        let mut m = f.0.lock().unwrap();
        m.connected = true;
        m.version = "1.2.9".into();
    }
    assert_eq!(
        label_client::test_connection(&operation, &settings, Some(&login()))
            .await
            .unwrap_err()
            .error,
        QbitError::UnsupportedVersion
    );
    {
        let mut m = f.0.lock().unwrap();
        m.version = "1.3.15".into();
        m.label_plugin = false;
    }
    let failure = label_client::test_connection(&operation, &settings, Some(&login()))
        .await
        .unwrap_err();
    assert_eq!(failure.problem, Some(LabelProblem::LabelsUnavailable));
    // A wrong URL base is a connection failure.
    let wrong_base = ProviderSettings::Deluge {
        endpoint: endpoint.replace("/deluge", "/elsewhere"),
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
    task.abort();
}

#[tokio::test]
async fn adds_magnet_and_torrent_per_domain_with_labels_and_queue_placement() {
    let (f, endpoint, task) = fixture("/deluge").await;
    let settings = settings(&endpoint);
    let client = HttpClient::new().unwrap();
    let operation = client.operation(Uuid::new_v4()).unwrap();
    let tv_hash = "0123456789abcdef0123456789abcdef01234567";
    let (bytes, movie_hash) = torrent("Film.2020.1080p");
    f.0.lock()
        .unwrap()
        .metainfo
        .push((b64(&bytes), movie_hash.clone()));
    // Episode 7 and movie 7 share a native ID but are distinct typed targets on distinct labels.
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
    assert_eq!(tv.identity().hashes(), [tv_hash.to_owned()]);
    assert_eq!(movie.identity().hashes(), [movie_hash.clone()]);
    assert!(
        !serde_json::to_string(tv.identity())
            .unwrap()
            .contains("tracker.invalid"),
        "the locator never enters the journal"
    );
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
    // Labels are declared once each, before the add that uses them.
    assert_eq!(
        f.calls("label.add"),
        vec![json!(["tv-hrr"]), json!(["movies-hrr"])]
    );
    let methods = f.methods();
    let position = |name: &str, nth: usize| {
        methods
            .iter()
            .enumerate()
            .filter(|(_, m)| *m == name)
            .nth(nth)
            .unwrap()
            .0
    };
    assert!(position("label.add", 0) < position("core.add_torrent_magnet", 0));
    assert!(position("core.add_torrent_magnet", 0) < position("label.set_torrent", 0));
    let magnets = f.calls("core.add_torrent_magnet");
    assert_eq!(
        magnets,
        vec![json!([magnet(tv_hash), {"add_paused":false}])]
    );
    let files = f.calls("core.add_torrent_file");
    assert_eq!(
        files,
        vec![json!(["film.torrent", b64(&bytes), {"add_paused":true}])]
    );
    assert_eq!(
        f.calls("label.set_torrent"),
        vec![
            json!([tv_hash, "tv-hrr"]),
            json!([movie_hash, "movies-hrr"])
        ]
    );
    // Only the recent TV target with priority "first" moves to the queue top.
    assert_eq!(f.calls("core.queue_top"), vec![json!([[tv_hash]])]);
    assert_eq!(
        f.torrent(&movie_hash)["state"],
        "Paused",
        "movie added paused"
    );

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
    assert_eq!(movie_page.items.len(), 1);
    assert_eq!(
        (
            movie_page.items[0].hash.as_str(),
            &movie_page.items[0].status,
            movie_page.items[0].domain
        ),
        (
            movie_hash.as_str(),
            &DownloadStatus::Paused,
            MediaDomain::Movies
        )
    );
    // The same hash through the other domain's target is a conflict, never a cross-domain read.
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
    // A v2-only release cannot be represented by a v1 info-hash client.
    let v2 = format!("magnet:?xt=urn:btmh:1220{}", "ab".repeat(32));
    assert_eq!(
        label_client::prepare(
            &operation,
            &settings,
            MediaTarget::Episode(8),
            AddSource::Magnet(v2),
            true,
            QbitOptions::default()
        )
        .await
        .err(),
        Some(QbitError::UnsupportedFeature)
    );
    // The password never reaches a projected name.
    f.put(
        "5555555555555555555555555555555555555555",
        &format!("leak.{PASSWORD}"),
        "tv-hrr",
        "Downloading",
        1,
    );
    let page = label_client::query(
        &operation,
        &settings,
        Some(&login()),
        &query(MediaDomain::Tv),
    )
    .await
    .unwrap();
    assert!(!serde_json::to_string(&page).unwrap().contains(PASSWORD));
    task.abort();
}

#[tokio::test]
async fn duplicates_and_lost_responses_reconcile_by_hash_without_a_second_add() {
    let (f, endpoint, task) = fixture("/deluge").await;
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
    // A retried grab for the same target finds the torrent by info hash.
    let again = prepare(MediaTarget::Episode(1), hash).await;
    assert_eq!(reparse(again.identity()).hashes(), identity.hashes());
    assert_eq!(
        label_client::submit(&operation, &settings, Some(&login()), again)
            .await
            .unwrap(),
        AddOutcome::AlreadyPresent { hash: hash.into() }
    );
    assert_eq!(f.calls("core.add_torrent_magnet").len(), 1);
    assert_eq!(
        label_client::reconcile(&operation, &settings, Some(&login()), &identity)
            .await
            .unwrap(),
        Reconciliation::Observed { hash: hash.into() }
    );
    // The same torrent for the movie domain is a scope conflict, not a second download.
    let foreign = prepare(MediaTarget::Movie(1), hash).await;
    assert_eq!(
        label_client::submit(&operation, &settings, Some(&login()), foreign)
            .await
            .unwrap_err(),
        QbitError::ScopeConflict
    );
    assert_eq!(f.calls("core.add_torrent_magnet").len(), 1);

    // Lost response: the client accepted the torrent but the reply never arrived.
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
    assert_eq!(f.calls("core.add_torrent_magnet").len(), 2);
    f.0.lock().unwrap().lose_add_response = false;
    // The add landed but its reply was lost: the engine labelled the stranded torrent, so a later
    // reconcile observes it in this domain instead of reporting absence.
    assert_eq!(
        f.calls("label.set_torrent").last(),
        Some(&json!([lost_hash, "tv-hrr"]))
    );
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
        f.calls("core.add_torrent_magnet").len(),
        2,
        "never a second add"
    );

    // A concurrent add of the same hash elsewhere is not ours: a conflict, nothing mutated.
    f.0.lock().unwrap().race_add = true;
    f.clear();
    let raced = prepare(
        MediaTarget::Episode(3),
        "3333333333333333333333333333333333333333",
    )
    .await;
    assert_eq!(
        label_client::submit(&operation, &settings, Some(&login()), raced)
            .await
            .unwrap_err(),
        QbitError::ScopeConflict
    );
    assert!(f.calls("label.set_torrent").is_empty() && f.calls("core.queue_top").is_empty());
    f.0.lock().unwrap().race_add = false;
    f.clear();

    // An authentication failure before add is definitely not a submission.
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
    assert!(f.calls("core.add_torrent_magnet").is_empty());

    // Changing remote-affecting settings invalidates a journaled identity.
    let mut changed = settings.clone();
    if let ProviderSettings::Deluge {
        tv: Some(scope), ..
    } = &mut changed
    {
        scope.category = "series".into();
    }
    assert_eq!(
        label_client::reconcile(&operation, &changed, Some(&login()), &identity)
            .await
            .unwrap_err(),
        QbitError::ScopeConflict
    );
    // Eligibility tags do not change remote behaviour, so they leave the identity valid.
    let mut tagged = settings.clone();
    if let ProviderSettings::Deluge {
        tv: Some(scope), ..
    } = &mut tagged
    {
        scope.tags = Some(vec![]);
    }
    assert!(
        label_client::reconcile(&operation, &tagged, Some(&login()), &identity)
            .await
            .is_ok()
    );
    task.abort();
}

#[tokio::test]
async fn status_mapping_isolation_details_and_controls() {
    let (f, endpoint, task) = fixture("/deluge").await;
    let settings = settings(&endpoint);
    let client = HttpClient::new().unwrap();
    let operation = client.operation(Uuid::new_v4()).unwrap();
    f.0.lock().unwrap().labels = vec!["tv-hrr".into(), "movies-hrr".into(), "other".into()];
    let h = |n: u8| format!("{:040}", n);
    f.put(&h(1), "Downloading", "tv-hrr", "Downloading", 524288);
    f.put(&h(2), "Seeding", "tv-hrr", "Seeding", 1048576);
    f.put(&h(3), "Paused", "tv-hrr", "Paused", 262144);
    f.put(&h(4), "Queued", "tv-hrr", "Queued", 0);
    f.put(&h(5), "Errored", "tv-hrr", "Error", 10);
    f.put(&h(6), "Checking", "tv-hrr", "Checking", 10);
    f.put(&h(7), "PausedDone", "tv-hrr", "Paused", 1048576);
    f.put(&h(8), "Foreign", "other", "Seeding", 1048576);
    f.put(&h(9), "MovieOne", "movies-hrr", "Seeding", 1048576);
    f.put(&h(10), "Unlabelled", "", "Seeding", 1048576);
    // A magnet without metadata has no size yet.
    f.0.lock().unwrap().torrents.push((h(11), {
        let mut t = record("Magnet", "tv-hrr", "Downloading", 0);
        t["total_wanted"] = json!(0);
        t
    }));
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
            .all(|i| ![h(8), h(9), h(10)].contains(&i.hash)),
        "foreign, other-domain and unlabelled torrents are invisible to TV"
    );
    // The movies label belongs to the movies domain only.
    let movies = label_client::query(
        &operation,
        &settings,
        Some(&login()),
        &query(MediaDomain::Movies),
    )
    .await
    .unwrap();
    assert_eq!(movies.items.len(), 1);
    assert_eq!(movies.items[0].hash, h(9));

    // Details: remote paths and per-file facts; unwanted files carry priority 0.
    let details = label_client::details(
        &operation,
        &settings,
        Some(&login()),
        &MediaTarget::Episode(1),
        &h(2),
    )
    .await
    .unwrap();
    assert_eq!(details.save_path, "/downloads");
    assert!(details.item.completed);
    assert_eq!(
        details
            .files
            .iter()
            .map(|file| (file.name.as_str(), file.priority))
            .collect::<Vec<_>>(),
        vec![("Seeding/Seeding.mkv", 1), ("Seeding/sample.mkv", 0)]
    );
    // The private import path keeps the original remote name even when a credential is a
    // substring of it. Only the public torrent projection is redacted.
    let raw_name = format!("Seeding/{PASSWORD}.mkv");
    {
        let mut m = f.0.lock().unwrap();
        let torrent = m
            .torrents
            .iter_mut()
            .find(|(hash, _)| *hash == h(2))
            .unwrap();
        torrent.1["name"] = json!(format!("Seeding.{PASSWORD}"));
        torrent.1["files"][0]["path"] = json!(raw_name);
    }
    let details = label_client::details(
        &operation,
        &settings,
        Some(&login()),
        &MediaTarget::Episode(1),
        &h(2),
    )
    .await
    .unwrap();
    assert_eq!(details.files[0].name, raw_name);
    assert!(!details.item.name.contains(PASSWORD));
    assert!(details.item.name.contains("[redacted]"));
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

    // Controls act only inside the domain and are confirmed by re-reading the torrent.
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
    assert_eq!(f.torrent(&h(1))["state"], "Paused");
    assert_eq!(
        control(MediaTarget::Episode(1), h(1), Control::Resume).await,
        Ok(())
    );
    assert_eq!(f.torrent(&h(1))["state"], "Downloading");
    assert_eq!(
        control(
            MediaTarget::Episode(1),
            h(1),
            Control::Priority { first: true }
        )
        .await,
        Ok(())
    );
    assert_eq!(f.calls("core.queue_top").last(), Some(&json!([[h(1)]])));
    assert_eq!(
        control(MediaTarget::Movie(1), h(1), Control::Pause).await,
        Err(QbitError::ScopeConflict)
    );
    assert_eq!(
        control(MediaTarget::Episode(1), h(8), Control::Pause).await,
        Err(QbitError::ScopeConflict)
    );
    assert_eq!(
        control(MediaTarget::Episode(1), h(1), Control::MarkImported).await,
        Err(QbitError::UnsupportedFeature)
    );
    assert_eq!(
        control(
            MediaTarget::Episode(1),
            h(2),
            Control::Remove { delete_files: true }
        )
        .await,
        Ok(())
    );
    assert_eq!(
        f.calls("core.remove_torrent").last(),
        Some(&json!([h(2), true]))
    );
    assert!(
        f.0.lock()
            .unwrap()
            .torrents
            .iter()
            .all(|(hash, _)| *hash != h(2))
    );
    // Pagination is stable by hash.
    let small = label_client::query(
        &operation,
        &settings,
        Some(&login()),
        &DownloadQuery {
            domain: MediaDomain::Tv,
            offset: 0,
            limit: 2,
            imported: false,
        },
    )
    .await
    .unwrap();
    assert_eq!((small.items.len(), small.next_offset), (2, Some(2)));
    task.abort();
}
