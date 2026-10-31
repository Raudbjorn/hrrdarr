//! Synology Download Station wire contract against an owned mock on 127.0.0.1:0 (never a live NAS
//! or DSM's ports 5000/5001). API names, paths, parameters and error codes follow Synology's
//! Download Station Web API guide as understood from its documentation; the mock is not recorded
//! from a running DSM.
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
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

const USER: &str = "nas-user";
const PASSWORD: &str = "PRIVATE_NAS_PASSWORD";
const SID: &str = "PRIVATE_NAS_SID";
const ROOT: &str = "downloads";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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

/// (api, method, query parameters) of each wire request.
type Call = (String, String, Vec<(String, String)>);

#[derive(Clone)]
struct Task {
    id: String,
    title: String,
    status: &'static str,
    destination: String,
    uri: String,
    size: u64,
    done: u64,
    /// (filename, size, size_downloaded, priority)
    files: Vec<(String, u64, u64, &'static str)>,
    /// Emit byte counts as decimal strings, as Synology's API guide documents them.
    strings: bool,
    /// Emit an unparseable `size`.
    bad_size: bool,
}
fn mk_task(id: &str, title: &str, destination: &str, uri: &str, status: &'static str) -> Task {
    Task {
        id: id.into(),
        title: title.into(),
        status,
        destination: destination.into(),
        uri: uri.into(),
        size: 1048576,
        done: 524288,
        files: vec![(title.into(), 1048576, 524288, "normal")],
        strings: false,
        bad_size: false,
    }
}
struct Mock {
    version: &'static str,
    config_destination: Option<&'static str>,
    tasks: Vec<Task>,
    /// (api, method, params) of every call after login.
    calls: Vec<Call>,
    logins: u32,
    logouts: u32,
    next_id: u32,
    lose_add_response: bool,
    metainfo: Vec<(Vec<u8>, String)>,
    reject_sid: bool,
    /// Folders that exist, as slash-joined paths without a leading slash. The first segment of
    /// a File Station path must be a shared folder already present here.
    folders: BTreeSet<String>,
    /// Like a real NAS, task creation names a destination that must already exist (task error
    /// 403) unless this is switched off.
    strict_destinations: bool,
    /// Fail every File Station `CreateFolder` call with this Synology error code.
    folder_error: Option<i64>,
    /// Create the folder but answer with a gateway error (an ambiguous outcome).
    lose_folder_response: bool,
    /// Answer `CreateFolder` with this path instead of the requested one.
    folder_reply_path: Option<String>,
}
#[derive(Clone)]
struct Fixture(Arc<Mutex<Mock>>);
impl Fixture {
    /// Download Station task/info calls with this method name.
    fn calls(&self, method: &str) -> Vec<Vec<(String, String)>> {
        self.0
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|(a, m, _)| m == method && a.starts_with("SYNO.DownloadStation"))
            .map(|(_, _, p)| p.clone())
            .collect()
    }
    /// File Station `CreateFolder` calls.
    fn folder_calls(&self) -> Vec<Vec<(String, String)>> {
        self.0
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|(a, _, _)| a == "SYNO.FileStation.CreateFolder")
            .map(|(_, _, p)| p.clone())
            .collect()
    }
    fn has_folder(&self, path: &str) -> bool {
        self.0.lock().unwrap().folders.contains(path)
    }
    fn clear(&self) {
        self.0.lock().unwrap().calls.clear();
    }
    fn by_hash(&self, hash: &str) -> Option<Task> {
        self.0
            .lock()
            .unwrap()
            .tasks
            .iter()
            .find(|t| t.destination.ends_with(hash))
            .cloned()
    }
    fn put(&self, t: Task) {
        self.0.lock().unwrap().tasks.push(t);
    }
}
fn param<'a>(p: &'a [(String, String)], key: &str) -> &'a str {
    p.iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .unwrap_or("")
}
fn view(t: &Task, additional: &str) -> Value {
    // Numbers or decimal strings, per the task.
    let n = |v: u64| {
        if t.strings {
            json!(v.to_string())
        } else {
            json!(v)
        }
    };
    let mut add = serde_json::Map::new();
    if additional.contains("detail") {
        add.insert(
            "detail".into(),
            json!({"destination":t.destination,"uri":t.uri,"create_time":1700000000}),
        );
    }
    if additional.contains("transfer") {
        add.insert("transfer".into(), json!({"size_downloaded":n(t.done),"size_uploaded":n(524288),"speed_download":n(2048),"speed_upload":n(64)}));
    }
    if additional.contains("file") {
        add.insert("file".into(), json!(t.files.iter().map(|(name, s, d, p)| json!({"filename":name,"size":n(*s),"size_downloaded":n(*d),"priority":p})).collect::<Vec<_>>()));
    }
    let size = if t.bad_size { json!("12x") } else { n(t.size) };
    json!({"id":t.id,"title":t.title,"size":size,"status":t.status,"type":"bt","username":USER,"additional":add})
}
fn fail(code: i64) -> Response {
    axum::Json(json!({"success":false,"error":{"code":code}})).into_response()
}
fn good(data: Value) -> Response {
    axum::Json(json!({"success":true,"data":data})).into_response()
}
/// Minimal multipart reader: text fields by name, and whether a byte string was uploaded.
fn multipart_field(body: &[u8], name: &str) -> Option<String> {
    let text = String::from_utf8_lossy(body);
    let marker = format!("name=\"{name}\"\r\n\r\n");
    let start = text.find(&marker)? + marker.len();
    Some(text[start..start + text[start..].find("\r\n")?].to_owned())
}
async fn handler(
    State(f): State<Fixture>,
    method: Method,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let mut m = f.0.lock().unwrap();
    assert_eq!(
        method,
        Method::POST,
        "every call is a form POST: secrets never ride in URLs"
    );
    assert!(
        uri.query().unwrap_or("").is_empty(),
        "no query string: {uri}"
    );
    let multipart = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("multipart/form-data"));
    let mut params: Vec<(String, String)> = if multipart {
        ["api", "version", "method", "destination", "_sid"]
            .iter()
            .filter_map(|k| multipart_field(&body, k).map(|v| ((*k).to_owned(), v)))
            .collect()
    } else {
        url::form_urlencoded::parse(&body).into_owned().collect()
    };
    let api = param(&params, "api").to_owned();
    let call = param(&params, "method").to_owned();
    if api == "SYNO.API.Auth" && call == "login" {
        assert_eq!(uri.path(), "/webapi/auth.cgi");
        assert_eq!(
            (
                param(&params, "version"),
                param(&params, "session"),
                param(&params, "format")
            ),
            ("6", "DownloadStation", "sid")
        );
        if param(&params, "account") == USER && param(&params, "passwd") == PASSWORD {
            m.logins += 1;
            return good(json!({"sid":SID}));
        }
        return fail(400);
    }
    if api == "SYNO.API.Auth" && call == "logout" {
        m.logouts += 1;
        return good(Value::Null);
    }
    if param(&params, "_sid") != SID || m.reject_sid {
        return fail(119);
    }
    params.retain(|(k, _)| k != "_sid");
    m.calls.push((api.clone(), call.clone(), params.clone()));
    match (api.as_str(), call.as_str()) {
        ("SYNO.DownloadStation.Info", "getinfo") => {
            good(json!({"version":3825,"version_string":m.version,"is_manager":true}))
        }
        ("SYNO.DownloadStation.Info", "getconfig") => match m.config_destination {
            Some(d) => good(json!({"default_destination":d,"bt_max_download":0})),
            None => good(json!({})),
        },
        ("SYNO.DownloadStation.Task", "list") => {
            let additional = param(&params, "additional").to_owned();
            let tasks: Vec<Value> = m.tasks.iter().map(|t| view(t, &additional)).collect();
            good(json!({"total":tasks.len(),"offset":0,"tasks":tasks}))
        }
        ("SYNO.DownloadStation.Task", "getinfo") => {
            let ids = param(&params, "id").to_owned();
            let additional = param(&params, "additional").to_owned();
            match m.tasks.iter().find(|t| t.id == ids) {
                Some(t) => good(json!({"tasks":[view(t, &additional)]})),
                None => fail(404),
            }
        }
        ("SYNO.FileStation.CreateFolder", "create") => {
            assert_eq!(uri.path(), "/webapi/entry.cgi");
            assert_eq!(
                (param(&params, "version"), param(&params, "force_parent")),
                ("2", "true"),
                "an existing folder must be a success so a retry is harmless"
            );
            if let Some(code) = m.folder_error {
                return fail(code);
            }
            let parent = param(&params, "folder_path").to_owned();
            let name = param(&params, "name").to_owned();
            let relative = parent.trim_start_matches('/');
            let share = relative.split('/').next().unwrap_or("");
            if !parent.starts_with('/') || !m.folders.contains(share) {
                // No such shared folder: nothing may be created outside the shares.
                return fail(408);
            }
            let full = format!("{relative}/{name}");
            let mut prefix = String::new();
            for segment in full.split('/') {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(segment);
                m.folders.insert(prefix.clone());
            }
            if m.lose_folder_response {
                return StatusCode::BAD_GATEWAY.into_response();
            }
            let reply = m
                .folder_reply_path
                .clone()
                .unwrap_or_else(|| format!("{parent}/{name}"));
            good(json!({"folders":[{"isdir":true,"name":name,"path":reply}]}))
        }
        ("SYNO.DownloadStation.Task", "create") => {
            let destination = param(&params, "destination").to_owned();
            if m.strict_destinations && !m.folders.contains(destination.trim_start_matches('/')) {
                return fail(403); // destination does not exist
            }
            let hash = destination.rsplit('/').next().unwrap().to_owned();
            let (title, uri) = if multipart {
                let found = m
                    .metainfo
                    .iter()
                    .find(|(b, h)| *h == hash && body.windows(b.len()).any(|w| w == b.as_slice()));
                assert!(found.is_some(), "the .torrent is uploaded verbatim");
                ("Uploaded".to_owned(), "release.torrent".to_owned())
            } else {
                ("Added".to_owned(), param(&params, "uri").to_owned())
            };
            m.next_id += 1;
            let id = format!("dbid_{}", m.next_id);
            let mut t = mk_task(&id, &title, &destination, &uri, "downloading");
            t.done = 0;
            m.tasks.push(t);
            if m.lose_add_response {
                return StatusCode::BAD_GATEWAY.into_response();
            }
            good(json!({"task_id":[id]}))
        }
        ("SYNO.DownloadStation.Task", "pause" | "resume" | "delete") => {
            let id = param(&params, "id").to_owned();
            let Some(i) = m.tasks.iter().position(|t| t.id == id) else {
                return fail(404);
            };
            match call.as_str() {
                "pause" => m.tasks[i].status = "paused",
                "resume" => m.tasks[i].status = "downloading",
                _ => {
                    m.tasks.remove(i);
                }
            }
            good(json!([{"id":id,"error":0}]))
        }
        _ => fail(102),
    }
}
async fn fixture() -> (Fixture, String, tokio::task::JoinHandle<()>) {
    let fixture = Fixture(Arc::new(Mutex::new(Mock {
        version: "3.8.16-3825",
        config_destination: Some(ROOT),
        tasks: vec![],
        calls: vec![],
        logins: 0,
        logouts: 0,
        next_id: 0,
        lose_add_response: false,
        metainfo: vec![],
        reject_sid: false,
        folders: BTreeSet::from([ROOT.to_owned()]),
        strict_destinations: true,
        folder_error: None,
        lose_folder_response: false,
        folder_reply_path: None,
    })));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    assert!(![5000, 5001].contains(&listener.local_addr().unwrap().port()));
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new().fallback(handler).with_state(fixture.clone());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (fixture, endpoint, task)
}
fn scope(label: &str, paused: bool) -> TorrentLabelScope {
    TorrentLabelScope {
        category: label.into(),
        recent_priority: TorrentQueuePriority::Last,
        older_priority: TorrentQueuePriority::Last,
        add_paused: paused,
        tags: None,
    }
}
fn settings(endpoint: &str) -> ProviderSettings {
    ProviderSettings::Downloadstation {
        endpoint: endpoint.into(),
        tv: Some(scope("tv-hrr", false)),
        movies: Some(scope("movies-hrr", true)),
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
fn dest(label: &str, hash: &str) -> String {
    format!("{ROOT}/{label}/{hash}")
}

#[tokio::test]
async fn login_session_lifecycle_and_configuration_reads() {
    let (f, endpoint, task) = fixture().await;
    let settings = settings(&endpoint);
    let client = HttpClient::new().unwrap();
    let operation = client.operation(Uuid::new_v4()).unwrap();
    let result = label_client::test_connection(&operation, &settings, Some(&login()))
        .await
        .unwrap();
    assert_eq!(result.client_version, "3.8.16-3825");
    assert_eq!(result.domains, vec![MediaDomain::Tv, MediaDomain::Movies]);
    {
        let m = f.0.lock().unwrap();
        assert_eq!(
            (m.logins, m.logouts),
            (1, 1),
            "every operation ends by logging out"
        );
        assert!(
            m.calls
                .iter()
                .all(|(_, method, _)| matches!(method.as_str(), "getinfo" | "getconfig" | "list")),
            "Test never mutates: {:?}",
            m.calls.iter().map(|c| c.1.clone()).collect::<Vec<_>>()
        );
    }
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
    // Credentials are required, and an API key is not a DSM credential.
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
    // A session id that stops working mid-operation is an authentication failure.
    f.0.lock().unwrap().reject_sid = true;
    assert_eq!(
        label_client::test_connection(&operation, &settings, Some(&login()))
            .await
            .unwrap_err()
            .error,
        QbitError::Http(HttpError::Authentication)
    );
    f.0.lock().unwrap().reject_sid = false;
    // No default destination configured: domains cannot be isolated by folder.
    f.0.lock().unwrap().config_destination = None;
    assert_eq!(
        label_client::test_connection(&operation, &settings, Some(&login()))
            .await
            .unwrap_err()
            .error,
        QbitError::UnsupportedFeature
    );
    task.abort();
}

#[tokio::test]
async fn adds_magnet_and_torrent_into_hash_destinations() {
    let (f, endpoint, task) = fixture().await;
    let settings = settings(&endpoint);
    let client = HttpClient::new().unwrap();
    let operation = client.operation(Uuid::new_v4()).unwrap();
    let tv_hash = "0123456789abcdef0123456789abcdef01234567";
    let (bytes, movie_hash) = torrent("Film.2020.1080p");
    f.0.lock()
        .unwrap()
        .metainfo
        .push((bytes.clone(), movie_hash.clone()));
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
    let creates = f.calls("create");
    assert_eq!(creates.len(), 2);
    assert_eq!(param(&creates[0], "destination"), dest("tv-hrr", tv_hash));
    assert_eq!(param(&creates[0], "uri"), magnet(tv_hash));
    assert_eq!(
        param(&creates[1], "destination"),
        dest("movies-hrr", &movie_hash)
    );
    // The movie scope adds paused: created, then paused.
    assert_eq!(f.calls("pause").len(), 1);
    assert_eq!(f.by_hash(&movie_hash).unwrap().status, "paused");
    assert_eq!(f.by_hash(tv_hash).unwrap().status, "downloading");
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
            .map(|i| (i.hash.as_str(), i.category.as_str()))
            .collect::<Vec<_>>(),
        vec![(tv_hash, "tv-hrr")]
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
    // Neither the password nor the session id reaches a projected name.
    let mut leak = mk_task(
        "dbid_900",
        &format!("leak.{PASSWORD}.{SID}"),
        &dest("tv-hrr", "5555555555555555555555555555555555555555"),
        "",
        "downloading",
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
    assert!(!text.contains(PASSWORD) && !text.contains(SID), "{text}");
    task.abort();
}

#[tokio::test]
async fn duplicates_foreign_destinations_and_lost_responses() {
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
    assert_eq!(f.calls("create").len(), 1);
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

    // Lost response: the task exists in the right destination, so reconcile finds it by hash.
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
    assert_eq!(f.calls("create").len(), 2, "never a second add");
    // A magnet task the user added elsewhere in Download Station is recognised by its URI.
    let other = "3333333333333333333333333333333333333333";
    f.put(mk_task(
        "dbid_800",
        "Manual",
        "downloads/manual",
        &magnet(other),
        "downloading",
    ));
    let clash = prepare(MediaTarget::Episode(3), other).await;
    assert_eq!(
        label_client::submit(&operation, &settings, Some(&login()), clash)
            .await
            .unwrap_err(),
        QbitError::ScopeConflict
    );
    assert_eq!(f.calls("create").len(), 2);
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
    assert_eq!(f.calls("create").len(), 2);
    task.abort();
}

#[tokio::test]
async fn status_mapping_isolation_details_and_controls() {
    let (f, endpoint, task) = fixture().await;
    let settings = settings(&endpoint);
    let client = HttpClient::new().unwrap();
    let operation = client.operation(Uuid::new_v4()).unwrap();
    let h = |n: u8| format!("{:040}", n);
    let mk = |n: u8, name: &str, label: &str, status: &'static str| {
        mk_task(&format!("dbid_{n}"), name, &dest(label, &h(n)), "", status)
    };
    f.put(mk(1, "Downloading", "tv-hrr", "downloading"));
    let mut seeding = mk(2, "Seeding", "tv-hrr", "seeding");
    seeding.done = seeding.size;
    f.put(seeding);
    f.put(mk(3, "Paused", "tv-hrr", "paused"));
    f.put(mk(4, "Waiting", "tv-hrr", "waiting"));
    f.put(mk(5, "Errored", "tv-hrr", "error"));
    f.put(mk(6, "Checking", "tv-hrr", "hash_checking"));
    let mut finished = mk(7, "Finished", "tv-hrr", "finished");
    finished.done = finished.size;
    f.put(finished);
    f.put(mk(8, "Foreign", "other-app", "downloading"));
    f.put(mk(9, "MovieOne", "movies-hrr", "downloading"));
    let mut meta = mk(10, "Magnet", "tv-hrr", "downloading");
    (meta.size, meta.done, meta.files) = (0, 0, vec![]);
    f.put(meta);
    // A task Download Station knows without a recognisable hash is not ours.
    f.put(mk_task(
        "dbid_700",
        "Plain.http.download",
        "downloads/web",
        "https://example.invalid/file",
        "downloading",
    ));
    let mut multi = mk(11, "Multi.Pack", "tv-hrr", "finished");
    multi.done = multi.size;
    multi.files = vec![
        (
            "Multi.Pack/Multi.Pack.S01E01.mkv".into(),
            1048000,
            1048000,
            "normal",
        ),
        ("Multi.Pack/sample.mkv".into(), 576, 0, "skip"),
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
    assert_eq!(status(7), (&DownloadStatus::Completed, &None, true));
    assert_eq!(
        status(10),
        (
            &DownloadStatus::Queued,
            &Some(DownloadDiagnostic::Metadata),
            false
        )
    );
    assert!(page.items.iter().any(|i| i.hash == h(6)));
    assert!(page.items.iter().all(|i| ![h(8), h(9)].contains(&i.hash)));
    assert_eq!(page.items.len(), 9);
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

    let details = label_client::details(
        &operation,
        &settings,
        Some(&login()),
        &MediaTarget::Episode(1),
        &h(11),
    )
    .await
    .unwrap();
    assert_eq!(details.save_path, format!("/{ROOT}/tv-hrr/{}", h(11)));
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
    assert_eq!(f.by_hash(&h(1)).unwrap().status, "paused");
    assert_eq!(
        control(MediaTarget::Episode(1), h(1), Control::Resume).await,
        Ok(())
    );
    assert_eq!(f.by_hash(&h(1)).unwrap().status, "downloading");
    assert_eq!(
        control(MediaTarget::Movie(1), h(1), Control::Pause).await,
        Err(QbitError::ScopeConflict)
    );
    f.clear();
    // Neither queue order nor deleting data exists in the API: explicit refusals, no requests.
    assert_eq!(
        control(
            MediaTarget::Episode(1),
            h(1),
            Control::Priority { first: true }
        )
        .await,
        Err(QbitError::UnsupportedFeature)
    );
    assert_eq!(
        control(
            MediaTarget::Episode(1),
            h(1),
            Control::Remove { delete_files: true }
        )
        .await,
        Err(QbitError::UnsupportedFeature)
    );
    assert!(f.calls("delete").is_empty());
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
    assert!(f.by_hash(&h(1)).is_none());
    task.abort();
}

#[tokio::test]
async fn byte_counts_are_read_as_documented_strings_and_malformed_values_fail_closed() {
    let (f, endpoint, task) = fixture().await;
    let settings = settings(&endpoint);
    let client = HttpClient::new().unwrap();
    let h = |n: u8| format!("{:040}", n);
    // Reasoning: Synology's API guide types sizes and transfer counters as strings; a finished
    // task whose size parsed as zero could never satisfy the engine's completion predicate.
    let mut done = mk_task(
        "dbid_1",
        "StringsDone",
        &dest("tv-hrr", &h(1)),
        "",
        "finished",
    );
    done.strings = true;
    done.done = done.size;
    done.files = vec![("StringsDone/ep.mkv".into(), 1048576, 1048576, "normal")];
    f.put(done);
    let page = label_client::query(
        &client.operation(Uuid::new_v4()).unwrap(),
        &settings,
        Some(&login()),
        &query(MediaDomain::Tv),
    )
    .await
    .unwrap();
    let item = page.items.iter().find(|i| i.name == "StringsDone").unwrap();
    assert_eq!(item.size_bytes, 1048576);
    assert!(
        item.completed,
        "a finished task with string counts is complete"
    );
    let details = label_client::details(
        &client.operation(Uuid::new_v4()).unwrap(),
        &settings,
        Some(&login()),
        &MediaTarget::Episode(1),
        &h(1),
    )
    .await
    .unwrap();
    assert_eq!(details.files.len(), 1);
    assert_eq!(details.files[0].size_bytes, 1048576);
    // A present but unparseable size is a malformed response, not a silent zero.
    let mut bad = mk_task("dbid_2", "BadSize", &dest("tv-hrr", &h(2)), "", "finished");
    bad.bad_size = true;
    f.put(bad);
    let result = label_client::query(
        &client.operation(Uuid::new_v4()).unwrap(),
        &settings,
        Some(&login()),
        &query(MediaDomain::Tv),
    )
    .await;
    assert!(
        matches!(result, Err(QbitError::InvalidResponse)),
        "got {:?}",
        result.map(|p| p.items.len())
    );
    task.abort();
}

async fn prepared(
    operation: &hrrdarr::providers::http::HttpOperation<'_>,
    settings: &ProviderSettings,
    target: MediaTarget,
    hash: &str,
) -> label_client::PreparedTorrent {
    label_client::prepare(
        operation,
        settings,
        target,
        AddSource::Magnet(magnet(hash)),
        false,
        QbitOptions::default(),
    )
    .await
    .unwrap()
}

/// The NAS rejects a task whose destination folder is absent (task error 403), so the wire
/// must create `<root>/<label>/<hash>` first: once per submission, for both domains, with a
/// magnet and an uploaded torrent alike, and without touching any other folder.
#[tokio::test]
async fn missing_destination_folders_are_provisioned_before_exactly_one_task_each() {
    let (f, endpoint, task) = fixture().await;
    let settings = settings(&endpoint);
    let client = HttpClient::new().unwrap();
    let operation = client.operation(Uuid::new_v4()).unwrap();
    let tv_hash = "0123456789abcdef0123456789abcdef01234567";
    let (bytes, movie_hash) = torrent("Film.2020.1080p");
    f.0.lock()
        .unwrap()
        .metainfo
        .push((bytes.clone(), movie_hash.clone()));
    // Only the shared folder exists: neither the label nor the hash folder.
    assert!(!f.has_folder("downloads/tv-hrr") && !f.has_folder("downloads/movies-hrr"));
    let tv = prepared(&operation, &settings, MediaTarget::Episode(9), tv_hash).await;
    let movie = label_client::prepare(
        &operation,
        &settings,
        MediaTarget::Movie(9),
        AddSource::Torrent {
            filename: "film.torrent".into(),
            bytes,
        },
        false,
        QbitOptions::default(),
    )
    .await
    .unwrap();
    f.clear();
    for prepared in [tv, movie] {
        assert!(matches!(
            label_client::submit(&operation, &settings, Some(&login()), prepared)
                .await
                .unwrap(),
            AddOutcome::Observed { .. }
        ));
    }
    let folders = f.folder_calls();
    assert_eq!(folders.len(), 2, "one provisioning call per submission");
    assert_eq!(
        (
            param(&folders[0], "folder_path"),
            param(&folders[0], "name"),
            param(&folders[0], "force_parent")
        ),
        ("/downloads/tv-hrr", tv_hash, "true")
    );
    assert_eq!(
        (
            param(&folders[1], "folder_path"),
            param(&folders[1], "name")
        ),
        ("/downloads/movies-hrr", movie_hash.as_str())
    );
    let creates = f.calls("create");
    assert_eq!(creates.len(), 2, "exactly one task per domain");
    // Domains stay isolated: each hash folder exists only below its own label.
    assert!(
        f.has_folder(&dest("tv-hrr", tv_hash)) && f.has_folder(&dest("movies-hrr", &movie_hash))
    );
    assert!(
        !f.has_folder(&dest("movies-hrr", tv_hash)) && !f.has_folder(&dest("tv-hrr", &movie_hash))
    );
    assert_eq!(
        f.0.lock().unwrap().folders.len(),
        1 + 2 * 2,
        "the share, two label folders and two hash folders; nothing else"
    );
    // A second torrent for a label that already exists reuses it and adds only its hash folder.
    let second = "abcdefabcdefabcdefabcdefabcdefabcdefabcd";
    f.clear();
    let again = prepared(&operation, &settings, MediaTarget::Episode(10), second).await;
    label_client::submit(&operation, &settings, Some(&login()), again)
        .await
        .unwrap();
    assert_eq!(f.folder_calls().len(), 1);
    assert_eq!(f.calls("create").len(), 1);
    // Resubmitting an existing download finds it by hash first: no folder call, no second task.
    f.clear();
    let repeat = prepared(&operation, &settings, MediaTarget::Episode(10), second).await;
    assert_eq!(
        label_client::submit(&operation, &settings, Some(&login()), repeat)
            .await
            .unwrap(),
        AddOutcome::AlreadyPresent {
            hash: second.into()
        }
    );
    assert!(f.folder_calls().is_empty() && f.calls("create").is_empty());
    task.abort();
}

/// Every folder failure happens before the add is dispatched: bounded error, no task, and a
/// later retry succeeds with exactly one task.
#[tokio::test]
async fn folder_failures_never_submit_and_a_retry_creates_one_task() {
    let (f, endpoint, task) = fixture().await;
    let settings = settings(&endpoint);
    let client = HttpClient::new().unwrap();
    let operation = client.operation(Uuid::new_v4()).unwrap();
    let hash = "9999999999999999999999999999999999999999";
    for (code, expected) in [
        (102, QbitError::UnsupportedFeature), // File Station API absent on this DSM
        (103, QbitError::UnsupportedFeature),
        (104, QbitError::UnsupportedFeature),
        (105, QbitError::Http(HttpError::Authentication)), // no permission
        (119, QbitError::Http(HttpError::Authentication)), // session id rejected
        (407, QbitError::Http(HttpError::Authentication)), // operation not permitted
        (408, QbitError::InvalidResponse),
        (418, QbitError::InvalidResponse), // illegal name or path
        (1100, QbitError::InvalidResponse),
        (1101, QbitError::InvalidResponse),
    ] {
        f.0.lock().unwrap().folder_error = Some(code);
        f.clear();
        let attempt = prepared(&operation, &settings, MediaTarget::Episode(1), hash).await;
        assert_eq!(
            label_client::submit(&operation, &settings, Some(&login()), attempt)
                .await
                .unwrap_err(),
            expected,
            "folder error {code}"
        );
        assert!(f.calls("create").is_empty(), "no task after error {code}");
        assert!(f.by_hash(hash).is_none());
        assert!(!f.has_folder(&dest("tv-hrr", hash)));
    }
    // Ambiguous creation: the folder exists but the reply was lost.
    {
        let mut m = f.0.lock().unwrap();
        m.folder_error = None;
        m.lose_folder_response = true;
    }
    f.clear();
    let attempt = prepared(&operation, &settings, MediaTarget::Episode(1), hash).await;
    assert!(
        label_client::submit(&operation, &settings, Some(&login()), attempt)
            .await
            .is_err()
    );
    assert!(f.has_folder(&dest("tv-hrr", hash)));
    assert!(f.calls("create").is_empty() && f.by_hash(hash).is_none());
    // Retry: the existing folder is a success and exactly one task is created.
    f.0.lock().unwrap().lose_folder_response = false;
    f.clear();
    let retry = prepared(&operation, &settings, MediaTarget::Episode(1), hash).await;
    assert_eq!(
        label_client::submit(&operation, &settings, Some(&login()), retry)
            .await
            .unwrap(),
        AddOutcome::Observed { hash: hash.into() }
    );
    assert_eq!(f.calls("create").len(), 1);
    // A reply that names another location is refused rather than trusted.
    let other = "8888888888888888888888888888888888888888";
    f.0.lock().unwrap().folder_reply_path = Some("/downloads/elsewhere/x".into());
    f.clear();
    let attempt = prepared(&operation, &settings, MediaTarget::Episode(2), other).await;
    assert_eq!(
        label_client::submit(&operation, &settings, Some(&login()), attempt)
            .await
            .unwrap_err(),
        QbitError::InvalidResponse
    );
    assert!(f.calls("create").is_empty());
    task.abort();
}

/// The destination read from the NAS becomes part of a File Station path, so a segment that
/// could climb out of it is refused before any call is made.
#[tokio::test]
async fn unsafe_default_destinations_are_refused_before_any_folder_call() {
    let (f, endpoint, task) = fixture().await;
    let settings = settings(&endpoint);
    let client = HttpClient::new().unwrap();
    let operation = client.operation(Uuid::new_v4()).unwrap();
    for root in ["downloads/../etc", "downloads/./x", "a\\b", "a//b"] {
        f.0.lock().unwrap().config_destination = Some(root);
        f.clear();
        let attempt = prepared(
            &operation,
            &settings,
            MediaTarget::Episode(1),
            "7777777777777777777777777777777777777777",
        )
        .await;
        assert_eq!(
            label_client::submit(&operation, &settings, Some(&login()), attempt)
                .await
                .unwrap_err(),
            QbitError::InvalidRequest,
            "{root}"
        );
        assert!(f.folder_calls().is_empty() && f.calls("create").is_empty());
    }
    task.abort();
}
