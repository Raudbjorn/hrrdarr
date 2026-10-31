//! Transmission RPC wire contract against an owned mock on 127.0.0.1:0 (never port 9091 or a
//! live daemon). Envelope, 409 session handshake, method and field names follow Transmission's
//! published `docs/rpc-spec.md`; the mock is written from that specification, not recorded from
//! a running Transmission.
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
        Credentials, ProviderSettings, TorrentPlacement, TorrentQueuePriority, TransmissionScope,
        http::{HttpClient, HttpError},
        qbittorrent::{
            AddOutcome, AddSource, Control, DownloadDiagnostic, DownloadQuery, DownloadStatus,
            QbitError, QbitOptions, Reconciliation, SubmissionIdentity,
        },
        transmission::{self, TransmissionProblem, TransmissionTestWarning},
    },
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

const USER: &str = "transmission-user";
const PASSWORD: &str = "PRIVATE_TRANSMISSION_PASSWORD";
const TRACKER_SECRET: &str = "PRIVATE_TRACKER_PASSKEY";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
/// Independent RFC 4648 encoder for asserting the `metainfo` and basic-auth wire values.
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
/// A minimal valid single-file v1 `.torrent` and its info hash (SHA-1 of the bencoded info dict).
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
    session: String,
    rpc_version: u64,
    rpc_minimum: u64,
    version: String,
    download_dir: String,
    dht: bool,
    queue: bool,
    auth: bool,
    torrents: Vec<Value>,
    calls: Vec<(String, Value, bool)>,
    paths: Vec<String>,
    lose_add_response: bool,
    race_add: bool,
    metainfo: Vec<(String, String)>,
    next_id: u64,
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
            .find(|t| t["hashString"] == hash)
            .cloned()
            .unwrap()
    }
}
fn record(hash: &str, name: &str, dir: &str, labels: &[&str], status: u64) -> Value {
    json!({"id":0,"hashString":hash,"name":name,"downloadDir":dir,"labels":labels,"status":status,
        "totalSize":1048576,"sizeWhenDone":1048576,"leftUntilDone":524288,"percentDone":0.5,
        "metadataPercentComplete":1.0,"isFinished":false,"isStalled":false,"eta":42,"error":0,"errorString":"",
        "rateDownload":2048,"rateUpload":64,"uploadRatio":0.25,"secondsSeeding":0,"queuePosition":0,
        "seedRatioLimit":2.0,"seedRatioMode":0,"seedIdleLimit":30,"seedIdleMode":0,
        "files":[{"name":format!("{name}/{name}.mkv"),"length":1048576,"bytesCompleted":524288},{"name":format!("{name}/sample.mkv"),"length":10,"bytesCompleted":0}],
        "fileStats":[{"bytesCompleted":524288,"wanted":true,"priority":0},{"bytesCompleted":0,"wanted":false,"priority":0}]})
}
fn project(torrent: &Value, fields: &[Value]) -> Value {
    let mut out = serde_json::Map::new();
    for field in fields {
        let field = field.as_str().unwrap();
        // Fields an older server does not know are simply absent from its reply.
        if let Some(value) = torrent.get(field) {
            out.insert(field.into(), value.clone());
        }
    }
    Value::Object(out)
}
async fn handler(State(f): State<Fixture>, uri: Uri, headers: HeaderMap, body: Bytes) -> Response {
    let mut m = f.0.lock().unwrap();
    m.paths.push(uri.path().to_owned());
    if m.auth {
        let expected = format!("Basic {}", b64(format!("{USER}:{PASSWORD}").as_bytes()));
        if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some(expected.as_str()) {
            return (StatusCode::UNAUTHORIZED, "<h1>401: Unauthorized</h1>").into_response();
        }
    }
    let request: Value = serde_json::from_slice(&body).unwrap();
    let method = request["method"].as_str().unwrap().to_owned();
    let arguments = request["arguments"].clone();
    let had_session = headers
        .get("x-transmission-session-id")
        .and_then(|v| v.to_str().ok())
        == Some(m.session.as_str());
    m.calls
        .push((method.clone(), arguments.clone(), had_session));
    if !had_session {
        let mut response = (StatusCode::CONFLICT, "<h1>409: Conflict</h1>").into_response();
        response
            .headers_mut()
            .insert("X-Transmission-Session-Id", m.session.parse().unwrap());
        return response;
    }
    let tag = request["tag"].clone();
    let ok = |arguments: Value| {
        axum::Json(json!({"result":"success","arguments":arguments,"tag":tag})).into_response()
    };
    let ids: Vec<String> = arguments["ids"]
        .as_array()
        .map(|ids| ids.iter().map(|v| v.as_str().unwrap().to_owned()).collect())
        .unwrap_or_default();
    let selected = |t: &Value| {
        arguments.get("ids").is_none() || ids.iter().any(|id| t["hashString"] == id.as_str())
    };
    match method.as_str() {
        "session-get" => ok(
            json!({"rpc-version":m.rpc_version,"rpc-version-minimum":m.rpc_minimum,"version":m.version,"download-dir":m.download_dir,"dht-enabled":m.dht,"download-queue-enabled":m.queue}),
        ),
        "torrent-get" => {
            let fields = arguments["fields"].as_array().unwrap().clone();
            let list: Vec<Value> = m
                .torrents
                .iter()
                .filter(|t| selected(t))
                .map(|t| project(t, &fields))
                .collect();
            ok(json!({"torrents":list}))
        }
        "torrent-add" => {
            let hash = if let Some(magnet) = arguments["filename"].as_str() {
                magnet.split("urn:btih:").nth(1).unwrap()[..40].to_lowercase()
            } else {
                let encoded = arguments["metainfo"].as_str().unwrap();
                m.metainfo
                    .iter()
                    .find(|(b, _)| b == encoded)
                    .map(|(_, h)| h.clone())
                    .expect("unknown metainfo")
            };
            if m.race_add {
                // Another agent adds the same torrent elsewhere just before this request lands.
                let dir = m.download_dir.clone();
                m.torrents.push(record(&hash, "raced", &dir, &[], 4));
            }
            if let Some(existing) = m.torrents.iter().find(|t| t["hashString"] == hash.as_str()) {
                let existing =
                    project(existing, &[json!("id"), json!("name"), json!("hashString")]);
                return ok(json!({"torrent-duplicate":existing}));
            }
            m.next_id += 1;
            let labels: Vec<String> = arguments["labels"]
                .as_array()
                .map(|l| l.iter().map(|v| v.as_str().unwrap().to_owned()).collect())
                .unwrap_or_default();
            let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
            let paused = arguments["paused"].as_bool().unwrap();
            let mut item = record(
                &hash,
                &format!("Added.{}", m.next_id),
                arguments["download-dir"].as_str().unwrap(),
                &labels,
                if paused { 0 } else { 4 },
            );
            item["id"] = json!(m.next_id);
            item["queuePosition"] = json!(m.torrents.len());
            m.torrents.push(item);
            if m.lose_add_response {
                return StatusCode::BAD_GATEWAY.into_response();
            }
            ok(json!({"torrent-added":{"id":m.next_id,"name":"Added","hashString":hash}}))
        }
        "torrent-set" => {
            for t in m.torrents.iter_mut().filter(|t| selected(t)) {
                for key in [
                    "seedRatioLimit",
                    "seedRatioMode",
                    "seedIdleLimit",
                    "seedIdleMode",
                    "labels",
                ] {
                    if let Some(v) = arguments.get(key) {
                        t[key] = v.clone();
                    }
                }
            }
            ok(json!({}))
        }
        "queue-move-top" => {
            for t in m.torrents.iter_mut() {
                let position = t["queuePosition"].as_u64().unwrap();
                t["queuePosition"] = json!(if selected(t) { 0 } else { position + 1 });
            }
            ok(json!({}))
        }
        "queue-move-bottom" => ok(json!({})),
        "torrent-stop" | "torrent-start" => {
            for t in m.torrents.iter_mut().filter(|t| selected(t)) {
                t["status"] = json!(if method == "torrent-stop" { 0 } else { 4 });
            }
            ok(json!({}))
        }
        "torrent-remove" => {
            m.torrents.retain(|t| !selected(t));
            ok(json!({}))
        }
        _ => axum::Json(json!({"result":"method name not recognized","arguments":{},"tag":tag}))
            .into_response(),
    }
}
async fn fixture(base: &str) -> (Fixture, String, tokio::task::JoinHandle<()>) {
    let fixture = Fixture(Arc::new(Mutex::new(Mock {
        session: "session-one".into(),
        rpc_version: 17,
        rpc_minimum: 14,
        version: "4.0.6 (38c164933e)".into(),
        download_dir: "/downloads".into(),
        dht: true,
        queue: true,
        auth: true,
        torrents: vec![],
        calls: vec![],
        paths: vec![],
        lose_add_response: false,
        race_add: false,
        metainfo: vec![],
        next_id: 0,
    })));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    assert_ne!(listener.local_addr().unwrap().port(), 9091);
    let endpoint = format!("http://{}{base}", listener.local_addr().unwrap());
    let app = Router::new().fallback(handler).with_state(fixture.clone());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (fixture, endpoint, task)
}
fn category(name: &str) -> TransmissionScope {
    TransmissionScope {
        placement: TorrentPlacement::Category {
            category: name.into(),
        },
        recent_priority: TorrentQueuePriority::First,
        older_priority: TorrentQueuePriority::Last,
        add_paused: false,
        tags: None,
    }
}
fn directory(path: &str) -> TransmissionScope {
    TransmissionScope {
        placement: TorrentPlacement::Directory {
            directory: path.into(),
        },
        recent_priority: TorrentQueuePriority::Last,
        older_priority: TorrentQueuePriority::Last,
        add_paused: true,
        tags: None,
    }
}
fn settings(endpoint: &str) -> ProviderSettings {
    ProviderSettings::Transmission {
        endpoint: endpoint.into(),
        tv: Some(category("tv-sonarr")),
        movies: Some(directory("/srv/movies")),
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
async fn handshake_authentication_url_base_and_version_gate() {
    let (f, endpoint, task) = fixture("/transmission").await;
    let settings = settings(&endpoint);
    let client = HttpClient::new().unwrap();
    let operation = client.operation(Uuid::new_v4()).unwrap();
    let result = transmission::test_connection(&operation, &settings, Some(&login()))
        .await
        .unwrap();
    assert_eq!(result.transmission_version, "4.0.6 (38c164933e)");
    assert_eq!((result.rpc_version, result.labels_supported), (17, true));
    assert_eq!(result.domains, vec![MediaDomain::Tv, MediaDomain::Movies]);
    assert!(result.warnings.is_empty());
    {
        let m = f.0.lock().unwrap();
        // The first request learns the session id from the 409 and is resent once with it.
        assert_eq!(
            (m.calls[0].0.as_str(), m.calls[0].2),
            ("session-get", false)
        );
        assert_eq!((m.calls[1].0.as_str(), m.calls[1].2), ("session-get", true));
        assert!(
            m.calls[2..].iter().all(|(_, _, session)| *session),
            "later calls reuse the id"
        );
        // The URL base is preserved and the RPC path appended.
        assert!(
            m.paths.iter().all(|p| p == "/transmission/rpc"),
            "{:?}",
            m.paths
        );
        // A connection test reads torrents with identity/placement fields only.
        let listed = &m
            .calls
            .iter()
            .find(|(m, _, _)| m == "torrent-get")
            .unwrap()
            .1;
        assert_eq!(
            listed["fields"],
            json!(["id", "hashString", "downloadDir", "labels"])
        );
        assert!(listed.get("ids").is_none());
    }
    // A rotated session id mid-operation is re-learned; the method is executed once.
    f.0.lock().unwrap().session = "session-two".into();
    f.clear();
    transmission::test_connection(&operation, &settings, Some(&login()))
        .await
        .unwrap();
    let methods = f.methods();
    assert_eq!(&methods[..2], ["session-get", "session-get"]);

    // Wrong or absent basic credentials are an authentication failure; secrets never surface.
    for credentials in [
        Some(Credentials::UsernamePassword {
            username: USER.into(),
            password: "wrong".into(),
        }),
        None,
    ] {
        let failure = transmission::test_connection(&operation, &settings, credentials.as_ref())
            .await
            .unwrap_err();
        assert_eq!(failure.error, QbitError::Http(HttpError::Authentication));
        assert!(!format!("{failure:?}").contains(PASSWORD));
    }
    // An API key is not a Transmission credential.
    let failure = transmission::test_connection(
        &operation,
        &settings,
        Some(&Credentials::ApiKey {
            api_key: "k".into(),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.error, QbitError::InvalidRequest);

    // Version gate: RPC 14 (Transmission 2.40) is the minimum; a server that dropped the legacy
    // protocol level this client speaks is also refused.
    f.0.lock().unwrap().rpc_version = 13;
    assert_eq!(
        transmission::test_connection(&operation, &settings, Some(&login()))
            .await
            .unwrap_err()
            .error,
        QbitError::UnsupportedVersion
    );
    f.0.lock().unwrap().rpc_version = 19;
    f.0.lock().unwrap().rpc_minimum = 18;
    assert_eq!(
        transmission::test_connection(&operation, &settings, Some(&login()))
            .await
            .unwrap_err()
            .error,
        QbitError::UnsupportedVersion
    );
    // Older servers work without labels; the test warns, and labels are neither requested nor sent.
    {
        let mut m = f.0.lock().unwrap();
        m.rpc_version = 15;
        m.rpc_minimum = 1;
        m.queue = false;
    }
    f.clear();
    let result = transmission::test_connection(&operation, &settings, Some(&login()))
        .await
        .unwrap();
    assert!(!result.labels_supported);
    assert_eq!(
        result.warnings,
        vec![
            TransmissionTestWarning::LabelsUnsupported,
            TransmissionTestWarning::QueueDisabled
        ]
    );
    assert_eq!(
        f.calls("torrent-get")[0]["fields"],
        json!(["id", "hashString", "downloadDir"])
    );
    {
        let mut m = f.0.lock().unwrap();
        m.rpc_version = 17;
        m.queue = true;
    }
    // A category scope needs a usable client download directory.
    f.0.lock().unwrap().download_dir = "relative/downloads".into();
    let failure = transmission::test_connection(&operation, &settings, Some(&login()))
        .await
        .unwrap_err();
    assert_eq!(
        failure.problem,
        Some(TransmissionProblem::DownloadDirInvalid)
    );
    // The effective TV subfolder may not nest inside (or contain) the movie directory.
    f.0.lock().unwrap().download_dir = "/srv".into();
    let overlapping = ProviderSettings::Transmission {
        endpoint: endpoint.clone(),
        tv: Some(category("movies")),
        movies: Some(directory("/srv/movies/films")),
    };
    let failure = transmission::test_connection(&operation, &overlapping, Some(&login()))
        .await
        .unwrap_err();
    assert_eq!(failure.problem, Some(TransmissionProblem::DomainsOverlap));
    f.0.lock().unwrap().download_dir = "/downloads".into();
    // A torrent claimed by both domains (TV label inside the movie directory) is surfaced too.
    f.0.lock().unwrap().torrents.push(record(
        &"c".repeat(40),
        "Both",
        "/srv/movies",
        &["tv-sonarr"],
        4,
    ));
    let failure = transmission::test_connection(&operation, &settings, Some(&login()))
        .await
        .unwrap_err();
    assert_eq!(failure.problem, Some(TransmissionProblem::DomainsOverlap));
    task.abort();

    // A custom URL base is honoured verbatim.
    let (f, endpoint, task) = fixture("/custom/base").await;
    f.0.lock().unwrap().auth = false;
    transmission::test_connection(&operation, &settings_at(&endpoint), None)
        .await
        .unwrap();
    assert!(
        f.0.lock()
            .unwrap()
            .paths
            .iter()
            .all(|p| p == "/custom/base/rpc")
    );
    task.abort();
}
fn settings_at(endpoint: &str) -> ProviderSettings {
    settings(endpoint)
}

#[tokio::test]
async fn adds_magnet_and_torrent_per_domain_with_colliding_ids() {
    let (f, endpoint, task) = fixture("/transmission").await;
    let settings = settings(&endpoint);
    let client = HttpClient::new().unwrap();
    let operation = client.operation(Uuid::new_v4()).unwrap();
    let tv_hash = "0123456789abcdef0123456789abcdef01234567";
    let (bytes, movie_hash) = torrent("Film.2020.1080p");
    f.0.lock()
        .unwrap()
        .metainfo
        .push((b64(&bytes), movie_hash.clone()));
    let options = QbitOptions {
        ratio_limit: Some(1.5),
        seeding_minutes: Some(60),
        ..Default::default()
    };
    // Episode 7 and movie 7 share a native ID but are distinct typed targets on distinct placements.
    let tv = transmission::prepare(
        &operation,
        &settings,
        MediaTarget::Episode(7),
        AddSource::Magnet(magnet(tv_hash)),
        true,
        options,
    )
    .await
    .unwrap();
    let movie = transmission::prepare(
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
    assert_eq!(
        transmission::submit(&operation, &settings, Some(&login()), tv)
            .await
            .unwrap(),
        AddOutcome::Observed {
            hash: tv_hash.into()
        }
    );
    assert_eq!(
        transmission::submit(&operation, &settings, Some(&login()), movie)
            .await
            .unwrap(),
        AddOutcome::Observed {
            hash: movie_hash.clone()
        }
    );
    let adds = f.calls("torrent-add");
    assert_eq!(adds.len(), 2);
    // TV: magnet into the category subfolder of the client download dir, labelled, started.
    assert_eq!(adds[0]["filename"], magnet(tv_hash));
    assert_eq!(adds[0]["download-dir"], "/downloads/tv-sonarr");
    assert_eq!(adds[0]["labels"], json!(["tv-sonarr"]));
    assert_eq!(adds[0]["paused"], false);
    assert!(adds[0].get("metainfo").is_none());
    // Movie: metainfo upload into the explicit directory, unlabelled, added paused.
    assert_eq!(adds[1]["metainfo"], b64(&bytes));
    assert_eq!(adds[1]["download-dir"], "/srv/movies");
    assert!(adds[1].get("labels").is_none());
    assert_eq!(adds[1]["paused"], true);
    assert!(adds[1].get("filename").is_none());
    // Seed limits (ratio, and seed time mapped onto the idle limit) apply only to the TV torrent;
    // the recent TV target with priority "first" moves to the queue top, the older movie does not.
    let sets = f.calls("torrent-set");
    assert_eq!(
        sets,
        vec![
            json!({"ids":[tv_hash],"seedRatioLimit":1.5,"seedRatioMode":1,"seedIdleLimit":60,"seedIdleMode":1})
        ]
    );
    assert_eq!(f.calls("queue-move-top"), vec![json!({"ids":[tv_hash]})]);
    let stored = f.torrent(tv_hash);
    assert_eq!(
        (
            stored["queuePosition"].as_u64(),
            stored["seedRatioMode"].as_u64()
        ),
        (Some(0), Some(1))
    );
    assert_eq!(f.torrent(&movie_hash)["status"], 0, "movie added paused");

    // Each domain observes only its own placement.
    let tv_page = transmission::query(
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
        vec![(tv_hash, MediaDomain::Tv, "tv-sonarr")]
    );
    let movie_page = transmission::query(
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
        transmission::details(
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
    // A v2-only release cannot be represented by Transmission.
    let v2 = format!("magnet:?xt=urn:btmh:1220{}", "ab".repeat(32));
    assert_eq!(
        transmission::prepare(
            &operation,
            &settings,
            MediaTarget::Episode(8),
            AddSource::Magnet(v2),
            true,
            QbitOptions::default()
        )
        .await
        .err()
        .map(|e| e),
        Some(QbitError::UnsupportedFeature)
    );
    // A trackerless magnet is refused before add when the client has DHT disabled.
    f.0.lock().unwrap().dht = false;
    let trackerless = transmission::prepare(
        &operation,
        &settings,
        MediaTarget::Episode(9),
        AddSource::Magnet(format!("magnet:?xt=urn:btih:{}", "9".repeat(40))),
        true,
        QbitOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        transmission::submit(&operation, &settings, Some(&login()), trackerless)
            .await
            .unwrap_err(),
        QbitError::Rejected
    );
    assert_eq!(f.calls("torrent-add").len(), 2);
    task.abort();
}

#[tokio::test]
async fn duplicates_and_lost_responses_reconcile_by_hash_without_a_second_add() {
    let (f, endpoint, task) = fixture("/transmission").await;
    let settings = settings(&endpoint);
    let client = HttpClient::new().unwrap();
    let operation = client.operation(Uuid::new_v4()).unwrap();
    let hash = "1111111111111111111111111111111111111111";
    let prepare = |target: MediaTarget, hash: &str| {
        let operation = &operation;
        let settings = &settings;
        let source = AddSource::Magnet(magnet(hash));
        async move {
            transmission::prepare(
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
        transmission::submit(&operation, &settings, Some(&login()), first)
            .await
            .unwrap(),
        AddOutcome::Observed { .. }
    ));
    // A retried grab for the same target finds the torrent by info hash.
    let again = prepare(MediaTarget::Episode(1), hash).await;
    assert_eq!(
        reparse(again.identity()).hashes(),
        identity.hashes(),
        "preparation is deterministic"
    );
    assert_eq!(
        transmission::submit(&operation, &settings, Some(&login()), again)
            .await
            .unwrap(),
        AddOutcome::AlreadyPresent { hash: hash.into() }
    );
    assert_eq!(f.calls("torrent-add").len(), 1);
    assert_eq!(
        transmission::reconcile(&operation, &settings, Some(&login()), &identity)
            .await
            .unwrap(),
        Reconciliation::Observed { hash: hash.into() }
    );
    // The same torrent for the movie domain is a scope conflict, not a second download.
    let foreign = prepare(MediaTarget::Movie(1), hash).await;
    assert_eq!(
        transmission::submit(&operation, &settings, Some(&login()), foreign)
            .await
            .unwrap_err(),
        QbitError::ScopeConflict
    );
    assert_eq!(f.calls("torrent-add").len(), 1);

    // Lost response: the client accepted the torrent but the reply never arrived.
    let lost_hash = "2222222222222222222222222222222222222222";
    f.0.lock().unwrap().lose_add_response = true;
    let lost = prepare(MediaTarget::Episode(2), lost_hash).await;
    let lost_identity = reparse(lost.identity());
    assert_eq!(
        transmission::reconcile(&operation, &settings, Some(&login()), &lost_identity)
            .await
            .unwrap(),
        Reconciliation::NotObserved
    );
    assert_eq!(
        transmission::submit(&operation, &settings, Some(&login()), lost)
            .await
            .unwrap_err(),
        QbitError::MutationUnknown
    );
    assert_eq!(f.calls("torrent-add").len(), 2);
    f.0.lock().unwrap().lose_add_response = false;
    // Reconciliation from the journaled identity observes it; resubmission never adds again.
    assert_eq!(
        transmission::reconcile(&operation, &settings, Some(&login()), &lost_identity)
            .await
            .unwrap(),
        Reconciliation::Observed {
            hash: lost_hash.into()
        }
    );
    let retry = prepare(MediaTarget::Episode(2), lost_hash).await;
    assert_eq!(
        transmission::submit(&operation, &settings, Some(&login()), retry)
            .await
            .unwrap(),
        AddOutcome::AlreadyPresent {
            hash: lost_hash.into()
        }
    );
    assert_eq!(f.calls("torrent-add").len(), 2);

    // A concurrent add of the same hash elsewhere answers torrent-duplicate: it is not ours, so
    // no follow-up mutates it and the outcome is a conflict.
    f.0.lock().unwrap().race_add = true;
    f.clear();
    let raced = prepare(
        MediaTarget::Episode(3),
        "3333333333333333333333333333333333333333",
    )
    .await;
    assert_eq!(
        transmission::submit(&operation, &settings, Some(&login()), raced)
            .await
            .unwrap_err(),
        QbitError::ScopeConflict
    );
    assert!(f.calls("torrent-set").is_empty() && f.calls("queue-move-top").is_empty());
    f.0.lock().unwrap().race_add = false;
    f.clear();

    // An authentication failure before add is definitely not a submission.
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
        transmission::submit(&operation, &settings, Some(&wrong), denied)
            .await
            .unwrap_err(),
        QbitError::Http(HttpError::Authentication)
    );
    assert_eq!(f.calls("torrent-add").len(), 0);

    // Changing remote-affecting settings invalidates a journaled identity.
    let mut changed = settings.clone();
    if let ProviderSettings::Transmission {
        tv: Some(scope), ..
    } = &mut changed
    {
        scope.placement = TorrentPlacement::Category {
            category: "series".into(),
        };
    }
    assert_eq!(
        transmission::reconcile(&operation, &changed, Some(&login()), &identity)
            .await
            .unwrap_err(),
        QbitError::ScopeConflict
    );
    task.abort();
}

#[tokio::test]
async fn status_mapping_isolation_details_and_controls() {
    let (f, endpoint, task) = fixture("/transmission").await;
    let settings = settings(&endpoint);
    let client = HttpClient::new().unwrap();
    let operation = client.operation(Uuid::new_v4()).unwrap();
    let h = |c: char| c.to_string().repeat(40);
    {
        let mut m = f.0.lock().unwrap();
        let mut add = |hash: String, dir: &str, labels: &[&str], status: u64, patch: Value| {
            let mut t = record(&hash, &format!("T-{}", &hash[..1]), dir, labels, status);
            for (k, v) in patch.as_object().unwrap() {
                t[k] = v.clone();
            }
            m.torrents.push(t);
        };
        let done = json!({"leftUntilDone":0,"percentDone":1.0,"eta":-1});
        add(h('a'), "/downloads/tv-sonarr", &["tv-sonarr"], 4, json!({}));
        add(h('b'), "/downloads/tv-sonarr", &["tv-sonarr"], 3, json!({}));
        add(h('c'), "/downloads/tv-sonarr", &["tv-sonarr"], 2, json!({}));
        add(
            h('d'),
            "/downloads/tv-sonarr",
            &["tv-sonarr"],
            0,
            json!({"eta":-1}),
        );
        add(
            h('e'),
            "/downloads/tv-sonarr",
            &["tv-sonarr"],
            6,
            done.clone(),
        );
        add(
            h('f'),
            "/downloads/tv-sonarr",
            &["tv-sonarr"],
            0,
            done.clone(),
        );
        add(
            h('1'),
            "/downloads/tv-sonarr",
            &["tv-sonarr"],
            4,
            json!({"error":3,"errorString":"No data found! Ensure your drives are connected"}),
        );
        add(
            h('2'),
            "/downloads/tv-sonarr",
            &["tv-sonarr"],
            4,
            json!({"error":2,"errorString":format!("Tracker gave HTTP 403 http://t.invalid/announce?passkey={TRACKER_SECRET}")}),
        );
        let mut seeded = done.clone();
        seeded["error"] = json!(2);
        seeded["errorString"] = json!(format!("unregistered passkey={TRACKER_SECRET}"));
        add(h('3'), "/downloads/tv-sonarr", &["tv-sonarr"], 6, seeded);
        add(
            h('4'),
            "/downloads/tv-sonarr",
            &["tv-sonarr"],
            4,
            json!({"error":1,"errorString":"Could not connect to tracker"}),
        );
        add(
            h('5'),
            "/downloads/tv-sonarr",
            &["tv-sonarr"],
            4,
            json!({"metadataPercentComplete":0.3,"totalSize":0,"sizeWhenDone":0}),
        );
        add(
            h('6'),
            "/downloads/tv-sonarr",
            &["tv-sonarr"],
            4,
            json!({"isStalled":true,"uploadRatio":-1}),
        );
        // An unlabelled torrent in the TV subfolder is TV on servers without labels in use.
        add(h('7'), "/downloads/tv-sonarr/Show", &[], 4, json!({}));
        // Not TV: another app's label, or another directory with no labels.
        add(h('8'), "/downloads/tv-sonarr", &["radarr"], 4, json!({}));
        add(h('9'), "/downloads/other", &[], 4, json!({}));
        // Movies by directory, regardless of labels.
        add(h('0'), "/srv/movies/Film", &["anything"], 6, done);
    }
    f.clear();
    let tv = transmission::refresh_pages(&operation, &settings, Some(&login()), MediaDomain::Tv)
        .await
        .unwrap();
    // Two-phase bounded read: identities/placement for all, full fields only for TV hashes.
    let gets = f.calls("torrent-get");
    assert_eq!(gets.len(), 2);
    assert_eq!(
        gets[0]["fields"],
        json!(["id", "hashString", "downloadDir", "labels"])
    );
    let requested: Vec<&str> = gets[1]["ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(requested.len(), 13);
    assert!(!requested.contains(&h('8').as_str()) && !requested.contains(&h('0').as_str()));
    assert!(
        !gets[1]["fields"]
            .as_array()
            .unwrap()
            .contains(&json!("files")),
        "refresh never reads file lists"
    );
    let by = |c: char| {
        tv.iter()
            .find(|i| i.hash == h(c))
            .unwrap_or_else(|| panic!("{c}"))
    };
    let state = |c: char| (&by(c).status, by(c).diagnostic, by(c).completed);
    assert_eq!(state('a'), (&DownloadStatus::Downloading, None, false));
    assert_eq!(
        (
            by('a').eta_seconds,
            by('a').progress,
            by('a').remaining_bytes,
            by('a').download_bytes_per_second
        ),
        (Some(42), 0.5, Some(524288), 2048)
    );
    assert_eq!(state('b'), (&DownloadStatus::Queued, None, false));
    assert_eq!(
        state('c'),
        (&DownloadStatus::Queued, None, false),
        "checking"
    );
    assert_eq!(state('d'), (&DownloadStatus::Paused, None, false));
    assert_eq!(
        state('e'),
        (&DownloadStatus::Completed, None, true),
        "seeding"
    );
    assert_eq!(by('e').eta_seconds, Some(0));
    assert_eq!(
        state('f'),
        (&DownloadStatus::Completed, None, true),
        "stopped after completion"
    );
    assert_eq!(
        state('1'),
        (
            &DownloadStatus::Warning,
            Some(DownloadDiagnostic::Error),
            false
        ),
        "local error"
    );
    assert_eq!(
        state('2'),
        (
            &DownloadStatus::Warning,
            Some(DownloadDiagnostic::Error),
            false
        ),
        "tracker error while incomplete"
    );
    assert_eq!(
        state('3'),
        (&DownloadStatus::Completed, None, true),
        "tracker error after the data completed"
    );
    assert_eq!(
        state('4'),
        (&DownloadStatus::Downloading, None, false),
        "tracker warnings do not change state"
    );
    assert_eq!(
        state('5'),
        (
            &DownloadStatus::Queued,
            Some(DownloadDiagnostic::Metadata),
            false
        )
    );
    assert_eq!(
        state('6'),
        (
            &DownloadStatus::Warning,
            Some(DownloadDiagnostic::Stalled),
            false
        )
    );
    assert_eq!(by('6').ratio, 0.0, "unavailable ratio is not a measurement");
    assert_eq!(by('7').domain, MediaDomain::Tv);
    // Error text (which can carry tracker passkeys) is never projected.
    assert!(!serde_json::to_string(&tv).unwrap().contains(TRACKER_SECRET));
    let movies =
        transmission::refresh_pages(&operation, &settings, Some(&login()), MediaDomain::Movies)
            .await
            .unwrap();
    assert_eq!(
        movies
            .iter()
            .map(|i| (i.hash.clone(), i.category.clone()))
            .collect::<Vec<_>>(),
        vec![(h('0'), String::new())]
    );

    // Completed-download facts: files relative to the download directory, unwanted files skipped.
    let details = transmission::details(
        &operation,
        &settings,
        Some(&login()),
        &MediaTarget::Movie(1),
        &h('0'),
    )
    .await
    .unwrap();
    assert_eq!(details.save_path, "/srv/movies/Film");
    assert!(details.item.completed);
    assert_eq!(
        details
            .files
            .iter()
            .map(|f| (f.name.as_str(), f.priority, f.progress))
            .collect::<Vec<_>>(),
        vec![("T-0/T-0.mkv", 1, 0.5), ("T-0/sample.mkv", 0, 0.0)]
    );
    // The private import path keeps the original remote name even when a credential is a
    // substring of it. Only the public torrent projection is redacted.
    let raw_name = format!("T-0/{PASSWORD}.mkv");
    {
        let mut m = f.0.lock().unwrap();
        let torrent = m
            .torrents
            .iter_mut()
            .find(|t| t["hashString"] == h('0'))
            .unwrap();
        torrent["name"] = json!(format!("T-0.{PASSWORD}"));
        torrent["files"][0]["name"] = json!(raw_name);
    }
    let details = transmission::details(
        &operation,
        &settings,
        Some(&login()),
        &MediaTarget::Movie(1),
        &h('0'),
    )
    .await
    .unwrap();
    assert_eq!(details.files[0].name, raw_name);
    assert!(!details.item.name.contains(PASSWORD));
    assert!(details.item.name.contains("[redacted]"));
    assert_eq!(
        transmission::details(
            &operation,
            &settings,
            Some(&login()),
            &MediaTarget::Episode(1),
            &h('0')
        )
        .await
        .err(),
        Some(QbitError::ScopeConflict)
    );
    assert_eq!(
        transmission::details(
            &operation,
            &settings,
            Some(&login()),
            &MediaTarget::Episode(1),
            &h('e')
        )
        .await
        .ok()
        .map(|d| d.item.completed),
        Some(true)
    );
    assert_eq!(
        transmission::details(
            &operation,
            &settings,
            Some(&login()),
            &MediaTarget::Episode(1),
            "abababababababababababababababababababab"
        )
        .await
        .err(),
        Some(QbitError::NotFound)
    );
    // Health root facts: the TV subfolder of the client directory and the explicit movie directory.
    let tv_root =
        transmission::client_status(&operation, &settings, Some(&login()), MediaDomain::Tv)
            .await
            .unwrap();
    assert_eq!(
        (
            tv_root.remote_root.as_str(),
            tv_root.removes_completed_downloads
        ),
        ("/downloads/tv-sonarr", false)
    );
    assert_eq!(
        transmission::client_status(&operation, &settings, Some(&login()), MediaDomain::Movies)
            .await
            .unwrap()
            .remote_root,
        "/srv/movies"
    );

    // Controls are domain-checked and verified after the mutation.
    let target = MediaTarget::Episode(1);
    transmission::control(
        &operation,
        &settings,
        Some(&login()),
        &target,
        &h('a'),
        Control::Pause,
    )
    .await
    .unwrap();
    assert_eq!(f.torrent(&h('a'))["status"], 0);
    transmission::control(
        &operation,
        &settings,
        Some(&login()),
        &target,
        &h('a'),
        Control::Resume,
    )
    .await
    .unwrap();
    transmission::control(
        &operation,
        &settings,
        Some(&login()),
        &target,
        &h('b'),
        Control::Priority { first: true },
    )
    .await
    .unwrap();
    assert_eq!(f.torrent(&h('b'))["queuePosition"], 0);
    assert_eq!(
        transmission::control(
            &operation,
            &settings,
            Some(&login()),
            &target,
            &h('0'),
            Control::Remove {
                delete_files: false
            }
        )
        .await
        .unwrap_err(),
        QbitError::ScopeConflict
    );
    assert_eq!(
        transmission::control(
            &operation,
            &settings,
            Some(&login()),
            &target,
            &h('a'),
            Control::MarkImported
        )
        .await
        .unwrap_err(),
        QbitError::UnsupportedFeature
    );
    f.clear();
    transmission::control(
        &operation,
        &settings,
        Some(&login()),
        &target,
        &h('a'),
        Control::Remove { delete_files: true },
    )
    .await
    .unwrap();
    assert_eq!(
        f.calls("torrent-remove"),
        vec![json!({"ids":[h('a')],"delete-local-data":true})]
    );
    assert!(
        f.0.lock()
            .unwrap()
            .torrents
            .iter()
            .all(|t| t["hashString"] != h('a').as_str())
    );

    // A torrent claimed by both domains fails the observation closed.
    f.0.lock().unwrap().torrents.push(record(
        &"ab".repeat(20),
        "Both",
        "/srv/movies",
        &["tv-sonarr"],
        4,
    ));
    assert_eq!(
        transmission::refresh_pages(&operation, &settings, Some(&login()), MediaDomain::Tv)
            .await
            .err(),
        Some(QbitError::ScopeConflict)
    );
    task.abort();
}

#[test]
fn settings_reject_unsafe_or_shared_placements() {
    let make = |tv: TransmissionScope, movies: TransmissionScope| ProviderSettings::Transmission {
        endpoint: "http://127.0.0.1:1/transmission".into(),
        tv: Some(tv),
        movies: Some(movies),
    };
    let parse = |value: Value| serde_json::from_value::<ProviderSettings>(value);
    // Wire shape: tagged placement, closed priorities, required nullable scopes.
    assert!(parse(json!({"implementation":"transmission","endpoint":"http://h:9091/transmission","tv":{"placement":{"strategy":"category","category":"tv"}},"movies":null})).is_ok());
    assert!(parse(json!({"implementation":"transmission","endpoint":"http://h:9091/transmission","tv":{"placement":{"strategy":"category","category":"tv"},"recent_priority":"top"},"movies":null})).is_err());
    assert!(parse(json!({"implementation":"transmission","endpoint":"http://h:9091/transmission","tv":{"placement":{"strategy":"label","category":"tv"}},"movies":null})).is_err());
    assert!(parse(json!({"implementation":"transmission","endpoint":"http://h:9091/transmission","tv":{"placement":{"strategy":"category","category":"tv"}}})).is_err());
    let client = HttpClient::new().unwrap();
    let valid = |settings: &ProviderSettings| {
        let operation = client.operation(Uuid::new_v4()).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            transmission::prepare(
                &operation,
                settings,
                MediaTarget::Episode(1),
                AddSource::Magnet(magnet(&"a".repeat(40))),
                true,
                QbitOptions::default(),
            )
            .await
            .is_ok()
        })
    };
    assert!(valid(&make(category("tv"), category("movies"))));
    assert!(valid(&make(category("tv"), directory("/srv/movies"))));
    assert!(valid(&make(
        directory("C:\\Downloads\\TV"),
        directory("C:/Downloads/Movies")
    )));
    for (tv, movies) in [
        (category("tv"), category("TV")),
        (category("tv/x"), category("movies")),
        (category("tv,hd"), category("movies")),
        (category(".."), category("movies")),
        (category(""), category("movies")),
        (directory("/srv/media"), directory("/srv/media/movies")),
        (directory("/srv/Media"), directory("/srv/media")),
        (directory("relative/tv"), category("movies")),
        (directory("/"), category("movies")),
        (directory("/srv/tv/"), category("movies")),
        (directory("/srv/../tv"), category("movies")),
    ] {
        assert!(
            !valid(&make(tv.clone(), movies.clone())),
            "{:?}/{:?}",
            tv.placement,
            movies.placement
        );
    }
}
