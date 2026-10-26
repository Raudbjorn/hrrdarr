//! NZBGet wire contract against an owned JSON-RPC mock on 127.0.0.1:0. Method names, parameter
//! order and the queue/history field names (`NZBID`, `FileSizeLo/Hi`, `ParStatus`, ...) are
//! written from NZBGet's published API reference. No live NZBGet is contacted.
use axum::{
    Router, body::Bytes, extract::State, http::HeaderMap, http::StatusCode, response::IntoResponse,
};
use hrrdarr::{
    api::MediaDomain,
    db::MediaTarget,
    providers::{
        Credentials, NzbgetPriority, NzbgetScope, ProviderSettings,
        http::{HttpClient, HttpError},
        nzbget::{self, NzbgetProblem, NzbgetTestWarning},
        qbittorrent::{
            AddOutcome, Control, DownloadDiagnostic, DownloadQuery, DownloadStatus, QbitError,
            Reconciliation,
        },
    },
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

const NZB_URL: &str = "https://indexer.invalid/getnzb/abc.nzb?apikey=PRIVATE_INDEXER_KEY";
const USER: &str = "nzbget";
const PASS: &str = "tegbzn6789";

struct Mock {
    version: String,
    config: Vec<(String, String)>,
    groups: Vec<Value>,
    history: Vec<Value>,
    download_paused: bool,
    rate: u64,
    calls: Vec<(String, Value, Option<String>)>,
    lose_add_response: bool,
    refuse_add: bool,
    ignore_category: bool,
    next_id: i64,
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
            .filter(|c| c.0 == method)
            .map(|c| c.1.clone())
            .collect()
    }
    fn authorizations(&self) -> Vec<Option<String>> {
        self.0
            .lock()
            .unwrap()
            .calls
            .iter()
            .map(|c| c.2.clone())
            .collect()
    }
}
fn cfg() -> Vec<(String, String)> {
    [
        ("MainDir", "/data"),
        ("DestDir", "${MainDir}/dst"),
        ("AppendCategoryDir", "yes"),
        ("KeepHistory", "7"),
        ("Category1.Name", "tv"),
        ("Category2.Name", "Movies"),
        ("Category2.DestDir", "C:\\films"),
    ]
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .to_vec()
}
fn group(id: i64, name: &str, cat: &str, status: &str, remaining: u64, paused: u64) -> Value {
    json!({"NZBID":id,"NZBName":name,"NZBFilename":format!("{name}.nzb"),"Category":cat,"Status":status,
        "FileSizeLo":2_000_000u64,"FileSizeHi":0,"RemainingSizeLo":remaining,"RemainingSizeHi":0,
        "PausedSizeLo":paused,"PausedSizeHi":0,"ActiveDownloads":if status=="DOWNLOADING"{2}else{0},"Parameters":[]})
}
#[allow(clippy::too_many_arguments)]
fn done(
    id: i64,
    name: &str,
    cat: &str,
    par: &str,
    unpack: &str,
    mv: &str,
    script: &str,
    delete: &str,
    mark: &str,
) -> Value {
    json!({"NZBID":id,"Name":name,"Category":cat,"Kind":"NZB","FileSizeLo":2_000_000u64,"FileSizeHi":0,
        "ParStatus":par,"UnpackStatus":unpack,"MoveStatus":mv,"ScriptStatus":script,"DeleteStatus":delete,"MarkStatus":mark,
        "Status":"SUCCESS/ALL","DestDir":format!("/data/dst/{cat}/{name}"),"FinalDir":""})
}
fn ok_done(id: i64, name: &str, cat: &str) -> Value {
    done(
        id, name, cat, "SUCCESS", "SUCCESS", "SUCCESS", "NONE", "NONE", "NONE",
    )
}
async fn handler(
    State(f): State<Fixture>,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let request: Value = serde_json::from_slice(&body).unwrap();
    let method = request["method"].as_str().unwrap().to_owned();
    let params = request["params"].clone();
    let id = request["id"].clone();
    let mut m = f.0.lock().unwrap();
    m.calls.push((method.clone(), params.clone(), auth.clone()));
    let expected = format!("Basic {}", hrrdarr_base64(&format!("{USER}:{PASS}")));
    if auth.as_deref() != Some(expected.as_str()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let reply = |result: Value| {
        axum::Json(json!({"version":"1.1","result":result,"id":id})).into_response()
    };
    match method.as_str() {
        "version" => reply(json!(m.version)),
        "config" => reply(Value::Array(m.config.iter().map(|(k, v)| json!({"Name":k,"Value":v})).collect())),
        "status" => reply(json!({"DownloadPaused":m.download_paused,"DownloadRate":m.rate})),
        "listgroups" => reply(Value::Array(m.groups.clone())),
        "history" => reply(Value::Array(m.history.clone())),
        "append" => {
            let p = params.as_array().unwrap();
            assert_eq!(p.len(), 9, "nine-argument append");
            assert!(p[0].as_str().unwrap().ends_with(".nzb"));
            if m.refuse_add {
                return reply(json!(0));
            }
            m.next_id += 1;
            let nzb = m.next_id;
            let name = p[0].as_str().unwrap().trim_end_matches(".nzb").to_owned();
            let category = if m.ignore_category { "".to_owned() } else { p[2].as_str().unwrap().to_owned() };
            let mut g = group(nzb, &name, &category, "QUEUED", 2_000_000, 0);
            if p[5] == json!(true) {
                g["Status"] = json!("PAUSED");
                g["PausedSizeLo"] = json!(2_000_000);
            }
            m.groups.push(g);
            if m.lose_add_response {
                return StatusCode::BAD_GATEWAY.into_response();
            }
            reply(json!(nzb))
        }
        "editqueue" => {
            let p = params.as_array().unwrap();
            let (cmd, ids) = (p[0].as_str().unwrap().to_owned(), p[3].as_array().unwrap().clone());
            let target = ids[0].as_i64().unwrap();
            match cmd.as_str() {
                "GroupPause" => m.groups.iter_mut().filter(|g| g["NZBID"] == target).for_each(|g| {
                    g["Status"] = json!("PAUSED");
                    g["PausedSizeLo"] = g["RemainingSizeLo"].clone();
                }),
                "GroupResume" => m.groups.iter_mut().filter(|g| g["NZBID"] == target).for_each(|g| {
                    g["Status"] = json!("QUEUED");
                    g["PausedSizeLo"] = json!(0);
                }),
                "GroupMoveTop" => {
                    if let Some(i) = m.groups.iter().position(|g| g["NZBID"] == target) {
                        let g = m.groups.remove(i);
                        m.groups.insert(0, g);
                    }
                }
                "GroupFinalDelete" => m.groups.retain(|g| g["NZBID"] != target),
                "HistoryDelete" => m.history.retain(|g| g["NZBID"] != target),
                _ => return reply(json!(false)),
            }
            reply(json!(true))
        }
        _ => axum::Json(json!({"version":"1.1","error":{"name":"JSONRPCError","code":1,"message":"Invalid procedure"},"id":id})).into_response(),
    }
}
fn hrrdarr_base64(text: &str) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in text.as_bytes().chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for (i, s) in [18, 12, 6, 0].into_iter().enumerate() {
            out.push(if i <= c.len() {
                T[((n >> s) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    out
}
async fn fixture() -> (Fixture, String, tokio::task::JoinHandle<()>) {
    let f = Fixture(Arc::new(Mutex::new(Mock {
        version: "21.1".into(),
        config: cfg(),
        groups: vec![],
        history: vec![],
        download_paused: false,
        rate: 0,
        calls: vec![],
        lose_add_response: false,
        refuse_add: false,
        ignore_category: false,
        next_id: 100,
    })));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/nzbget/", listener.local_addr().unwrap());
    let app = Router::new().fallback(handler).with_state(f.clone());
    (
        f,
        endpoint,
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() }),
    )
}
fn settings(endpoint: String) -> ProviderSettings {
    ProviderSettings::Nzbget {
        endpoint,
        tv: Some(NzbgetScope {
            category: "tv".into(),
            recent_priority: NzbgetPriority::High,
            older_priority: NzbgetPriority::Low,
            add_paused: false,
            tags: None,
        }),
        movies: Some(NzbgetScope {
            category: "movies".into(),
            recent_priority: NzbgetPriority::Force,
            older_priority: NzbgetPriority::Normal,
            add_paused: true,
            tags: None,
        }),
    }
}
fn creds() -> Credentials {
    Credentials::UsernamePassword {
        username: USER.into(),
        password: PASS.into(),
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
async fn test_connection_checks_auth_version_categories_and_history() {
    let (f, url, task) = fixture().await;
    let s = settings(url);
    let client = HttpClient::new().unwrap();
    let t = nzbget::test_connection(
        &client.operation(Uuid::new_v4()).unwrap(),
        &s,
        Some(&creds()),
    )
    .await
    .unwrap();
    assert_eq!(t.nzbget_version, "21.1");
    assert_eq!(t.domains, vec![MediaDomain::Tv, MediaDomain::Movies]);
    assert!(t.warnings.is_empty());
    assert!(
        f.authorizations().iter().all(|a| a.is_some()),
        "every call is authenticated"
    );
    // Wrong or missing credentials are an authentication failure, never reflected.
    let bad = Credentials::UsernamePassword {
        username: USER.into(),
        password: "nope".into(),
    };
    for c in [Some(&bad), None] {
        let e = nzbget::test_connection(&client.operation(Uuid::new_v4()).unwrap(), &s, c)
            .await
            .unwrap_err();
        assert_eq!(e.error, QbitError::Http(HttpError::Authentication));
    }
    // API-key credentials do not exist for NZBGet.
    let e = nzbget::test_connection(
        &client.operation(Uuid::new_v4()).unwrap(),
        &s,
        Some(&Credentials::ApiKey {
            api_key: "k".repeat(16),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(e.error, QbitError::InvalidRequest);
    {
        let mut m = f.0.lock().unwrap();
        m.version = "12.0".into();
    }
    let e = nzbget::test_connection(
        &client.operation(Uuid::new_v4()).unwrap(),
        &s,
        Some(&creds()),
    )
    .await
    .unwrap_err();
    assert_eq!(e.error, QbitError::UnsupportedVersion);
    {
        let mut m = f.0.lock().unwrap();
        m.version = "21.0-testing-r2345".into();
        m.config.retain(|(k, _)| k != "Category2.Name");
        m.config.retain(|(k, _)| k != "Category2.DestDir");
    }
    let e = nzbget::test_connection(
        &client.operation(Uuid::new_v4()).unwrap(),
        &s,
        Some(&creds()),
    )
    .await
    .unwrap_err();
    assert_eq!(e.problem, Some(NzbgetProblem::CategoryMissing));
    for (value, expected) in [
        ("0", NzbgetProblem::KeepHistoryZero),
        ("30000", NzbgetProblem::KeepHistoryOverMax),
    ] {
        let mut m = f.0.lock().unwrap();
        m.config = cfg();
        m.config
            .iter_mut()
            .find(|(k, _)| k == "KeepHistory")
            .unwrap()
            .1 = value.into();
        drop(m);
        let e = nzbget::test_connection(
            &client.operation(Uuid::new_v4()).unwrap(),
            &s,
            Some(&creds()),
        )
        .await
        .unwrap_err();
        assert_eq!(e.problem, Some(expected));
    }
    {
        let mut m = f.0.lock().unwrap();
        m.config = cfg();
        m.config
            .iter_mut()
            .find(|(k, _)| k == "KeepHistory")
            .unwrap()
            .1 = "1".into();
        m.version = "testing-r2345".into();
    }
    let t = nzbget::test_connection(
        &client.operation(Uuid::new_v4()).unwrap(),
        &s,
        Some(&creds()),
    )
    .await
    .unwrap();
    assert_eq!(
        t.warnings,
        vec![
            NzbgetTestWarning::TestingVersion,
            NzbgetTestWarning::ShortHistoryRetention
        ]
    );
    // A JSON-RPC error object is a rejection.
    f.0.lock().unwrap().config = vec![];
    let e = nzbget::test_connection(
        &client.operation(Uuid::new_v4()).unwrap(),
        &s,
        Some(&creds()),
    )
    .await
    .unwrap_err();
    assert_eq!(e.problem, Some(NzbgetProblem::CategoryMissing));
    task.abort();
}

#[tokio::test]
async fn queue_and_history_map_status_per_domain_category() {
    let (f, url, task) = fixture().await;
    let s = settings(url);
    let client = HttpClient::new().unwrap();
    {
        let mut m = f.0.lock().unwrap();
        m.rate = 1_000_000;
        m.groups = vec![
            group(1, "Show.S01E01", "tv", "DOWNLOADING", 1_000_000, 0),
            group(2, "Show.S01E02", "TV", "QUEUED", 2_000_000, 0),
            group(3, "Show.S01E03", "tv", "PAUSED", 2_000_000, 2_000_000),
            group(4, "Show.S01E04", "tv", "UNPACKING", 0, 0),
            group(5, "Show.S01E05", "tv", "REPAIRING", 0, 0),
            group(6, "Show.S01E06", "tv", "SOMETHING_NEW", 0, 0),
            group(7, "Film.2020", "movies", "DOWNLOADING", 500_000, 0),
            group(8, "Unrelated", "other", "DOWNLOADING", 500_000, 0),
            group(
                9,
                "http://indexer.invalid/x?apikey=PRIVATE",
                "tv",
                "FETCHING",
                2_000_000,
                0,
            ),
            group(31, "Show.S01E07", "tv", "POST_UNPACK_RENAMING", 0, 0),
            group(32, "Show.S01E08", "tv", "POST_DOWNLOAD_RENAMING", 0, 0),
        ];
        m.history = vec![
            ok_done(11, "Show.Done", "tv"),
            done(
                12,
                "Show.ParFail",
                "tv",
                "FAILURE",
                "NONE",
                "NONE",
                "NONE",
                "NONE",
                "NONE",
            ),
            done(
                13,
                "Show.NoSpace",
                "tv",
                "SUCCESS",
                "SPACE",
                "SUCCESS",
                "NONE",
                "NONE",
                "NONE",
            ),
            done(
                14,
                "Show.MoveFail",
                "tv",
                "SUCCESS",
                "SUCCESS",
                "FAILURE",
                "NONE",
                "NONE",
                "NONE",
            ),
            done(
                22,
                "Show.ParManual",
                "tv",
                "MANUAL",
                "SUCCESS",
                "SUCCESS",
                "NONE",
                "NONE",
                "NONE",
            ),
            done(
                23,
                "Show.ParRepairable",
                "tv",
                "REPAIR_POSSIBLE",
                "SUCCESS",
                "SUCCESS",
                "NONE",
                "NONE",
                "NONE",
            ),
            done(
                24,
                "Show.UnpackPassword",
                "tv",
                "SUCCESS",
                "PASSWORD",
                "SUCCESS",
                "NONE",
                "NONE",
                "NONE",
            ),
            done(
                25,
                "Show.ScriptFail",
                "tv",
                "SUCCESS",
                "SUCCESS",
                "SUCCESS",
                "FAILURE",
                "NONE",
                "NONE",
            ),
            done(
                15,
                "Show.Health",
                "tv",
                "NONE",
                "NONE",
                "NONE",
                "NONE",
                "HEALTH",
                "NONE",
            ),
            done(
                16,
                "Show.UserDeleted",
                "tv",
                "NONE",
                "NONE",
                "NONE",
                "NONE",
                "MANUAL",
                "NONE",
            ),
            done(
                17,
                "Show.MarkedBad",
                "tv",
                "NONE",
                "NONE",
                "NONE",
                "NONE",
                "MANUAL",
                "BAD",
            ),
            done(
                26,
                "Show.BadMarked",
                "tv",
                "SUCCESS",
                "SUCCESS",
                "SUCCESS",
                "NONE",
                "NONE",
                "BAD",
            ),
            json!({
                "NZBID":21,
                "Name":"Show.FetchFailed",
                "Category":"tv",
                "Kind":"URL",
                "ParStatus":"NONE",
                "UnpackStatus":"NONE",
                "MoveStatus":"NONE",
                "ScriptStatus":"NONE",
                "DeleteStatus":"NONE",
                "MarkStatus":"NONE",
                "UrlStatus":"FAILURE",
                "Status":"FAILURE/FETCH",
                "FileSizeLo":0,
                "FileSizeHi":0,
                "DestDir":"",
                "FinalDir":""
            }),
            ok_done(18, "Film.Done", "movies"),
            ok_done(19, "Other.Done", "other"),
            json!({"NZBID":20,"Name":"dup","Category":"tv","Kind":"DUP","FileSizeLo":1,"FileSizeHi":0}),
        ];
    }
    let tv = nzbget::refresh_pages(
        &client.operation(Uuid::new_v4()).unwrap(),
        &s,
        Some(&creds()),
        MediaDomain::Tv,
    )
    .await
    .unwrap();
    let by = |name: &str| {
        tv.iter().find(|i| i.name == name).unwrap_or_else(|| {
            panic!(
                "{name} in {:?}",
                tv.iter().map(|i| &i.name).collect::<Vec<_>>()
            )
        })
    };
    assert_eq!(
        tv.len(),
        21,
        "tv queue 9 + history 12 (user-deleted and DUP records omitted)"
    );
    assert!(
        tv.iter()
            .all(|i| i.domain == MediaDomain::Tv && i.category.eq_ignore_ascii_case("tv"))
    );
    assert_eq!(by("Show.S01E01").status, DownloadStatus::Downloading);
    assert_eq!(by("Show.S01E01").eta_seconds, Some(1));
    assert_eq!(by("Show.S01E01").progress, 0.5);
    assert_eq!(by("Show.S01E02").status, DownloadStatus::Queued);
    assert_eq!(
        by("Show.S01E02").eta_seconds,
        Some(3),
        "estimate covers jobs ahead of it"
    );
    assert_eq!(by("Show.S01E03").status, DownloadStatus::Paused);
    assert_eq!(by("Show.S01E03").eta_seconds, None);
    assert_eq!(
        by("Show.S01E04").diagnostic,
        Some(DownloadDiagnostic::Extracting)
    );
    assert_eq!(
        by("Show.S01E05").diagnostic,
        Some(DownloadDiagnostic::Repairing)
    );
    assert_eq!(by("Show.S01E06").status, DownloadStatus::Unknown);
    // Reasoning: the documented listgroups statuses POST_UNPACK_RENAMING and POST_DOWNLOAD_RENAMING
    // are active post-processing phases, so they are not "unknown".
    for name in ["Show.S01E07", "Show.S01E08"] {
        assert_eq!(by(name).status, DownloadStatus::Downloading, "{name}");
        assert_eq!(
            by(name).diagnostic,
            Some(DownloadDiagnostic::PostProcessing),
            "{name}"
        );
    }
    assert_eq!(by("(fetching NZB)").status, DownloadStatus::Downloading);
    assert_eq!(by("(fetching NZB)").eta_seconds, Some(6));
    assert!(!serde_json::to_string(&tv).unwrap().contains("PRIVATE"));
    let failed_fetch = by("Show.FetchFailed");
    assert_eq!(failed_fetch.status, DownloadStatus::Failed);
    assert!(!failed_fetch.completed);
    let completed = by("Show.Done");
    assert!(
        completed.completed
            && completed.status == DownloadStatus::Completed
            && completed.progress == 1.0
    );
    assert_eq!(by("Show.ParFail").status, DownloadStatus::Failed);
    assert_eq!(
        (
            &by("Show.NoSpace").status,
            by("Show.NoSpace").diagnostic.as_ref()
        ),
        (
            &DownloadStatus::Warning,
            Some(&DownloadDiagnostic::DiskFull)
        )
    );
    assert_eq!(by("Show.MoveFail").status, DownloadStatus::Failed);
    assert!(!by("Show.MoveFail").completed);
    // NZBGet grades these as warnings (WARNING/DAMAGED, WARNING/REPAIRABLE,
    // WARNING/PASSWORD, WARNING/SCRIPT) even though the detail fields are not successes.
    for name in [
        "Show.ParManual",
        "Show.ParRepairable",
        "Show.UnpackPassword",
        "Show.ScriptFail",
    ] {
        assert_eq!(by(name).status, DownloadStatus::Warning, "{name}");
        assert!(!by(name).completed, "{name}");
    }
    assert_eq!(by("Show.Health").status, DownloadStatus::Failed);
    assert_eq!(by("Show.MarkedBad").status, DownloadStatus::Failed);
    assert_eq!(by("Show.BadMarked").status, DownloadStatus::Failed);
    assert!(!tv.iter().any(|i| i.name == "Show.UserDeleted"));
    // Global pause pauses every active job; the sizes use the Lo/Hi halves.
    f.0.lock().unwrap().download_paused = true;
    let paused = nzbget::refresh_pages(
        &client.operation(Uuid::new_v4()).unwrap(),
        &s,
        Some(&creds()),
        MediaDomain::Tv,
    )
    .await
    .unwrap();
    assert!(
        paused
            .iter()
            .filter(|i| !i.completed && i.diagnostic.is_none() && i.remaining_bytes != Some(0))
            .all(|i| i.status == DownloadStatus::Paused)
    );
    let movies = nzbget::query(
        &client.operation(Uuid::new_v4()).unwrap(),
        &s,
        Some(&creds()),
        &query(MediaDomain::Movies),
    )
    .await
    .unwrap();
    let names: Vec<_> = movies.items.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, vec!["Film.2020", "Film.Done"]);
    // Imported categories do not exist for NZBGet.
    let mut imported = query(MediaDomain::Tv);
    imported.imported = true;
    assert!(
        nzbget::query(
            &client.operation(Uuid::new_v4()).unwrap(),
            &s,
            Some(&creds()),
            &imported
        )
        .await
        .is_err()
    );
    // Remote ids are stable digests of the job id, distinct per job.
    assert_eq!(by("Show.S01E01").hash, nzbget::remote_id(1));
    task.abort();
}

#[tokio::test]
async fn append_is_idempotent_per_target_and_lost_or_refused_responses_never_double_submit() {
    let (f, url, task) = fixture().await;
    let s = settings(url);
    let client = HttpClient::new().unwrap();
    let provider = Uuid::new_v4();
    let tv = nzbget::prepare(
        provider,
        &s,
        MediaTarget::Episode(7),
        NZB_URL.into(),
        "Show.S01E01.1080p.WEB-DL",
        true,
    )
    .unwrap();
    let movie = nzbget::prepare(
        provider,
        &s,
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
    let op = || client.operation(Uuid::new_v4()).unwrap();
    assert_eq!(
        nzbget::submit(&op(), &s, Some(&creds()), tv).await.unwrap(),
        AddOutcome::Observed {
            hash: nzbget::remote_id(101)
        }
    );
    assert_eq!(
        nzbget::submit(&op(), &s, Some(&creds()), movie)
            .await
            .unwrap(),
        AddOutcome::Observed {
            hash: nzbget::remote_id(102)
        }
    );
    let adds = f.calls("append");
    assert_eq!(adds.len(), 2);
    // Recent TV uses High (50), no add-paused; an older movie uses Normal and is added paused.
    assert_eq!(
        (
            adds[0][2].as_str(),
            adds[0][3].as_i64(),
            adds[0][5].as_bool()
        ),
        (Some("tv"), Some(50), Some(false))
    );
    assert_eq!(
        (
            adds[1][2].as_str(),
            adds[1][3].as_i64(),
            adds[1][5].as_bool()
        ),
        (Some("movies"), Some(0), Some(true))
    );
    assert_eq!(adds[0][1], json!(NZB_URL));
    // Re-preparing the same release yields the same identity and a second submit is a no-op.
    let again = nzbget::prepare(
        provider,
        &s,
        MediaTarget::Episode(7),
        NZB_URL.into(),
        "Show.S01E01.1080p.WEB-DL",
        true,
    )
    .unwrap();
    assert_eq!(again.identity().hashes()[0], tv_token);
    assert_eq!(
        nzbget::submit(&op(), &s, Some(&creds()), again)
            .await
            .unwrap(),
        AddOutcome::AlreadyPresent {
            hash: nzbget::remote_id(101)
        }
    );
    assert_eq!(f.calls("append").len(), 2);
    // Lost response: the mutation is unknown, never retried, and reconciliation finds the job.
    f.0.lock().unwrap().lose_add_response = true;
    let lost = nzbget::prepare(
        provider,
        &s,
        MediaTarget::Episode(8),
        NZB_URL.into(),
        "Show.S01E02",
        true,
    )
    .unwrap();
    let identity_after = nzbget::prepare(
        provider,
        &s,
        MediaTarget::Episode(8),
        NZB_URL.into(),
        "Show.S01E02",
        true,
    )
    .unwrap();
    let e = nzbget::submit_classified_for_tests(&op(), &s, Some(&creds()), lost)
        .await
        .unwrap_err();
    assert_eq!(e.0, QbitError::MutationUnknown);
    assert!(!e.1);
    assert_eq!(f.calls("append").len(), 3);
    assert_eq!(
        nzbget::reconcile(&op(), &s, Some(&creds()), identity_after.identity())
            .await
            .unwrap(),
        Reconciliation::Observed {
            hash: nzbget::remote_id(103)
        }
    );
    assert_eq!(f.calls("append").len(), 3, "reconciliation reads only");
    let unseen = nzbget::prepare(
        provider,
        &s,
        MediaTarget::Episode(99),
        NZB_URL.into(),
        "Never.Sent",
        true,
    )
    .unwrap();
    assert_eq!(
        nzbget::reconcile(&op(), &s, Some(&creds()), unseen.identity())
            .await
            .unwrap(),
        Reconciliation::NotObserved
    );
    // An explicit refusal (result 0) is a definite non-submission.
    {
        let mut m = f.0.lock().unwrap();
        m.lose_add_response = false;
        m.refuse_add = true;
    }
    let refused = nzbget::prepare(
        provider,
        &s,
        MediaTarget::Episode(9),
        NZB_URL.into(),
        "Refused",
        true,
    )
    .unwrap();
    let e = nzbget::submit_classified_for_tests(&op(), &s, Some(&creds()), refused)
        .await
        .unwrap_err();
    assert_eq!(e.0, QbitError::Rejected);
    assert!(e.1);
    // A server that files the job under another category cannot be treated as isolated.
    {
        let mut m = f.0.lock().unwrap();
        m.refuse_add = false;
        m.ignore_category = true;
    }
    let misfiled = nzbget::prepare(
        provider,
        &s,
        MediaTarget::Episode(10),
        NZB_URL.into(),
        "Misfiled",
        true,
    )
    .unwrap();
    let e = nzbget::submit_classified_for_tests(&op(), &s, Some(&creds()), misfiled)
        .await
        .unwrap_err();
    assert_eq!(e.0, QbitError::MutationUnknown);
    // A job of the same token in a foreign category is a visible scope conflict, not absence.
    let foreign = nzbget::prepare(
        provider,
        &s,
        MediaTarget::Episode(10),
        NZB_URL.into(),
        "Misfiled",
        true,
    )
    .unwrap();
    assert_eq!(
        nzbget::reconcile(&op(), &s, Some(&creds()), foreign.identity())
            .await
            .unwrap_err(),
        QbitError::ScopeConflict
    );
    // An unknown category is refused before anything is sent.
    f.0.lock()
        .unwrap()
        .config
        .retain(|(k, _)| k != "Category1.Name");
    let before = f.calls("append").len();
    let late = nzbget::prepare(
        provider,
        &s,
        MediaTarget::Episode(11),
        NZB_URL.into(),
        "Late",
        true,
    )
    .unwrap();
    let e = nzbget::submit_classified_for_tests(&op(), &s, Some(&creds()), late)
        .await
        .unwrap_err();
    assert!(e.1);
    assert_eq!(f.calls("append").len(), before);
    task.abort();
}

#[tokio::test]
async fn completed_job_storage_status_snapshot_and_controls() {
    let (f, url, task) = fixture().await;
    let s = settings(url);
    let client = HttpClient::new().unwrap();
    let op = || client.operation(Uuid::new_v4()).unwrap();
    {
        let mut m = f.0.lock().unwrap();
        m.groups = vec![
            group(1, "A", "tv", "QUEUED", 2_000_000, 0),
            group(2, "B", "tv", "QUEUED", 2_000_000, 0),
        ];
        let mut finished = ok_done(5, "Show.Done", "tv");
        finished["FinalDir"] = json!("/data/dst/tv/Show.Done");
        m.history = vec![finished];
    }
    let job = nzbget::job(
        &op(),
        &s,
        Some(&creds()),
        &MediaTarget::Episode(1),
        &nzbget::remote_id(5),
    )
    .await
    .unwrap();
    assert_eq!(job.storage.as_deref(), Some("/data/dst/tv/Show.Done"));
    assert!(job.item.completed);
    assert_eq!(
        nzbget::job(
            &op(),
            &s,
            Some(&creds()),
            &MediaTarget::Movie(1),
            &nzbget::remote_id(5)
        )
        .await
        .err(),
        Some(QbitError::NotFound),
        "other domain's scope does not see it"
    );
    // Output roots come from the category (derived or explicit) with `${MainDir}` expanded.
    let tv = nzbget::client_status_snapshot_for_tests(&op(), &s, Some(&creds()), MediaDomain::Tv)
        .await
        .unwrap();
    assert_eq!(tv, ("/data/dst/tv".to_owned(), false, false));
    let movies =
        nzbget::client_status_snapshot_for_tests(&op(), &s, Some(&creds()), MediaDomain::Movies)
            .await
            .unwrap();
    assert_eq!(movies, ("C:\\films".to_owned(), true, false));
    // Controls are verified by a follow-up read.
    let tv_a = nzbget::remote_id(1);
    let tv_b = nzbget::remote_id(2);
    let target = MediaTarget::Episode(1);
    nzbget::control(&op(), &s, Some(&creds()), &target, &tv_a, Control::Pause)
        .await
        .unwrap();
    let paused = nzbget::refresh_pages(&op(), &s, Some(&creds()), MediaDomain::Tv)
        .await
        .unwrap();
    assert_eq!(
        paused.iter().find(|i| i.hash == tv_a).unwrap().status,
        DownloadStatus::Paused
    );
    nzbget::control(&op(), &s, Some(&creds()), &target, &tv_a, Control::Resume)
        .await
        .unwrap();
    nzbget::control(
        &op(),
        &s,
        Some(&creds()),
        &target,
        &tv_b,
        Control::Priority { first: true },
    )
    .await
    .unwrap();
    assert_eq!(f.0.lock().unwrap().groups[0]["NZBID"], 2);
    nzbget::control(
        &op(),
        &s,
        Some(&creds()),
        &target,
        &tv_a,
        Control::Remove {
            delete_files: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(f.0.lock().unwrap().groups.len(), 1);
    nzbget::control(
        &op(),
        &s,
        Some(&creds()),
        &target,
        &nzbget::remote_id(5),
        Control::Remove {
            delete_files: false,
        },
    )
    .await
    .unwrap();
    assert!(f.0.lock().unwrap().history.is_empty());
    // Files of finished jobs cannot be removed through the API, and unknown jobs are not found.
    assert_eq!(
        nzbget::control(
            &op(),
            &s,
            Some(&creds()),
            &target,
            &tv_b,
            Control::MarkImported
        )
        .await
        .unwrap_err(),
        QbitError::UnsupportedFeature
    );
    assert_eq!(
        nzbget::control(
            &op(),
            &s,
            Some(&creds()),
            &target,
            &nzbget::remote_id(77),
            Control::Pause
        )
        .await
        .unwrap_err(),
        QbitError::NotFound
    );
    task.abort();
}

#[test]
fn settings_reject_shared_or_unsafe_categories_and_priorities() {
    let parse = |v: Value| serde_json::from_value::<ProviderSettings>(v);
    let ok = parse(
        json!({"implementation":"nzbget","endpoint":"http://h:6789","tv":{"category":"tv","recent_priority":"very_high","older_priority":"force","add_paused":true},"movies":null}),
    );
    assert!(ok.is_ok());
    assert!(parse(json!({"implementation":"nzbget","endpoint":"http://h:6789","tv":{"category":"tv","recent_priority":"urgent"},"movies":null})).is_err());
    assert!(parse(json!({"implementation":"nzbget","endpoint":"http://h:6789","tv":{"category":"tv","bogus":1},"movies":null})).is_err());
}
