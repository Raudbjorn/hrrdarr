//! Classifier coverage is distinct from the real qBittorrent probe tests below.
use hrrdarr::{
    api::MediaDomain,
    health_detectors::{ClientKind, ClientObservation, classify},
    providers::qbittorrent::EndpointLocality,
};

#[test]
fn both_domain_cdh_contract_matrix() {
    use ClientKind::*;
    use EndpointLocality::*;
    let cases = [
        (vec![], "cdh_undefined_sabnzbd"),
        (vec![(Sabnzbd, Loopback)], "cdh_undefined_sabnzbd"),
        (vec![(Nzbget, Loopback)], "cdh_undefined_nzbget"),
        (vec![(Other, Loopback)], "cdh_undefined"),
        (
            vec![(Sabnzbd, Loopback), (Nzbget, Loopback)],
            "cdh_undefined",
        ),
        (vec![(Sabnzbd, Unknown)], "cdh_undefined_nonlocal"),
        (
            vec![(Nzbget, Loopback), (Other, Unknown)],
            "cdh_undefined_nonlocal",
        ),
    ];
    for (clients, reason) in cases {
        let clients: Vec<_> = clients
            .into_iter()
            .map(|(kind, locality)| ClientObservation { kind, locality })
            .collect();
        for enabled in [false, true] {
            let issue = classify(MediaDomain::Tv, false, enabled, Ok(&clients))
                .unwrap()
                .unwrap();
            assert_eq!(issue.reason, reason);
            assert_eq!(issue.compatibility_type, "ImportMechanismCheck");
            assert_eq!(
                issue.wiki_url,
                "https://wiki.servarr.com/sonarr/system#completedfailed-download-handling"
            );
            let defined = classify(MediaDomain::Tv, true, enabled, Ok(&clients)).unwrap();
            assert_eq!(defined.is_none(), enabled);
            if let Some(issue) = defined {
                assert_eq!(issue.reason, "cdh_disabled");
            }
        }
    }
    for domain in [MediaDomain::Tv, MediaDomain::Movies] {
        for defined in [false, true] {
            for enabled in [false, true] {
                for error in ["check_failed", "check_timeout", "storage_error"] {
                    let actual = classify(domain, defined, enabled, Err(error));
                    if domain == MediaDomain::Tv {
                        assert_eq!(
                            actual,
                            Err(error),
                            "TV requires current status even when settings seem conclusive"
                        );
                    } else {
                        let issue = actual.unwrap();
                        assert_eq!(
                            issue.is_none(),
                            enabled,
                            "movies have no invented status prerequisite"
                        );
                        if let Some(issue) = issue {
                            assert_eq!(issue.reason, "cdh_disabled");
                            assert_eq!(
                                issue.wiki_url,
                                "https://wiki.servarr.com/radarr/system#completed-download-handling-is-disabled"
                            );
                        }
                    }
                }
            }
        }
    }
}

use axum::{
    Router,
    extract::{OriginalUri, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use hrrdarr::{db::Database, health_detectors::evaluate_current, providers};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
struct Server(tokio::task::JoinHandle<()>);
impl Server {
    async fn stop(mut self) {
        self.0.abort();
        let _ = (&mut self.0).await;
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn serve(app: Router) -> (String, Server) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    (
        base,
        Server(tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        })),
    )
}
#[derive(Clone, Default)]
struct Peer {
    calls: Arc<AtomicUsize>,
    mode: Arc<AtomicUsize>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}
async fn probe(State(peer): State<Peer>, OriginalUri(uri): OriginalUri) -> Response {
    peer.calls.fetch_add(1, Ordering::SeqCst);
    match peer.mode.load(Ordering::SeqCst) {
        1 => return (StatusCode::UNAUTHORIZED, "secret-sentinel").into_response(),
        2 if uri.path().ends_with("preferences") => {
            peer.entered.notify_one();
            peer.release.notified().await;
        }
        _ => (),
    }
    match uri.path() {
        "/api/v2/app/webapiVersion" => "2.8.0".into_response(),
        "/api/v2/app/preferences" => axum::Json(json!({"save_path":"/private-downloads", "max_ratio_enabled":false,"max_seeding_time_enabled":false,"max_seeding_time":-1,"max_ratio_act":0})).into_response(),
        "/api/v2/torrents/categories" => axum::Json(json!({"tv":{"name":"tv","savePath":""},"movies":{"name":"movies","savePath":""}})).into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}
async fn request(base: &str, method: &str, path: &str, body: Value) -> Value {
    let response = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
        .request(method.parse().unwrap(), format!("{base}{path}"))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    let status = response.status();
    let text = response.text().await.unwrap();
    assert!(status.is_success(), "{status}: {text}");
    if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap()
    }
}
fn provider_input(endpoint: &str, tv: bool) -> Value {
    let scope = json!({"category":if tv{"tv"}else{"movies"},"imported_category":null,"recent_priority":0,"older_priority":0});
    json!({"name":"Health owned peer","enabled":true,"priority":1,"settings":{"implementation":"qbittorrent","endpoint":endpoint,"tv":if tv{scope.clone()}else{Value::Null},"movies":if tv{Value::Null}else{scope}}})
}

#[tokio::test]
async fn real_status_first_movie_independence_and_revision_fence() {
    let path =
        std::env::temp_dir().join(format!("hrrdarr-health-detectors-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&path).unwrap();
    let scratch = Scratch(path);
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let (routes, client) = providers::router_with_refresh(db.clone(), None);
    let (api, server) =
        serve(routes.merge(hrrdarr::completed_download_handling::router(db.clone()))).await;
    let peer = Peer::default();
    let (endpoint, peer_server) =
        serve(Router::new().fallback(get(probe)).with_state(peer.clone())).await;
    assert_eq!(
        evaluate_current(&db, &client, MediaDomain::Tv)
            .await
            .unwrap()
            .unwrap()
            .reason,
        "cdh_undefined_sabnzbd"
    );
    assert!(
        evaluate_current(&db, &client, MediaDomain::Movies)
            .await
            .unwrap()
            .is_none()
    );
    // A movie-only client must not be touched by either movie or TV's scoped probe.
    request(
        &api,
        "POST",
        "/api/v1/providers",
        provider_input(&endpoint, false),
    )
    .await;
    assert_eq!(
        evaluate_current(&db, &client, MediaDomain::Tv)
            .await
            .unwrap()
            .unwrap()
            .reason,
        "cdh_undefined_sabnzbd"
    );
    assert!(
        evaluate_current(&db, &client, MediaDomain::Movies)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(peer.calls.load(Ordering::SeqCst), 0);
    let created = request(
        &api,
        "POST",
        "/api/v1/providers",
        provider_input(&endpoint, true),
    )
    .await;
    let id = created["id"].as_str().unwrap();
    let issue = evaluate_current(&db, &client, MediaDomain::Tv)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(issue.reason, "cdh_undefined");
    assert!(peer.calls.load(Ordering::SeqCst) >= 3);
    let settings = request(
        &api,
        "GET",
        "/api/v1/tv/completed-download-handling",
        Value::Null,
    )
    .await;
    request(
        &api,
        "PUT",
        "/api/v1/tv/completed-download-handling",
        json!({"enabled":true,"revision":settings["revision"]}),
    )
    .await;
    assert!(
        evaluate_current(&db, &client, MediaDomain::Tv)
            .await
            .unwrap()
            .is_none(),
        "equal true save resolves undefined warning"
    );
    peer.mode.store(1, Ordering::SeqCst);
    assert_eq!(
        evaluate_current(&db, &client, MediaDomain::Tv).await,
        Err("check_failed"),
        "defined+true cannot bypass failed status"
    );
    let before = peer.calls.load(Ordering::SeqCst);
    assert!(
        evaluate_current(&db, &client, MediaDomain::Movies)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        peer.calls.load(Ordering::SeqCst),
        before,
        "movie detector never probes clients"
    );
    let settings = request(
        &api,
        "GET",
        "/api/v1/tv/completed-download-handling",
        Value::Null,
    )
    .await;
    request(
        &api,
        "PUT",
        "/api/v1/tv/completed-download-handling",
        json!({"enabled":false,"revision":settings["revision"]}),
    )
    .await;
    assert_eq!(
        evaluate_current(&db, &client, MediaDomain::Tv).await,
        Err("check_failed"),
        "defined+false also requires status first"
    );
    peer.mode.store(0, Ordering::SeqCst);
    assert_eq!(
        evaluate_current(&db, &client, MediaDomain::Tv)
            .await
            .unwrap()
            .unwrap()
            .reason,
        "cdh_disabled"
    );
    // Block the real HTTP observation, then edit its input before releasing it.
    peer.mode.store(2, Ordering::SeqCst);
    let checking = evaluate_current(&db, &client, MediaDomain::Tv);
    let changing = async {
        tokio::time::timeout(Duration::from_secs(5), peer.entered.notified())
            .await
            .unwrap();
        let mut update = provider_input(&endpoint, true);
        update["revision"] = created["revision"].clone();
        update["name"] = json!("Changed during probe");
        request(&api, "PUT", &format!("/api/v1/providers/{id}"), update).await;
        peer.release.notify_one();
    };
    let (outcome, ()) = tokio::join!(checking, changing);
    assert_eq!(
        outcome,
        Err("check_failed"),
        "stale status cannot become a successful observation"
    );
    peer.mode.store(0, Ordering::SeqCst);
    assert_eq!(
        evaluate_current(&db, &client, MediaDomain::Tv)
            .await
            .unwrap()
            .unwrap()
            .reason,
        "cdh_disabled"
    );
    // Caller cancellation drops the owned probe/permit; the next evaluation can run.
    peer.mode.store(2, Ordering::SeqCst);
    {
        let checking = evaluate_current(&db, &client, MediaDomain::Tv);
        tokio::pin!(checking);
        tokio::select! {
            result = &mut checking => panic!("probe finished before cancellation: {result:?}"),
            entered = tokio::time::timeout(Duration::from_secs(5), peer.entered.notified()) => entered.unwrap(),
        }
        // The HTTP preference handler has entered; dropping this future cancels real work.
    }
    peer.release.notify_one();
    peer.mode.store(0, Ordering::SeqCst);
    assert_eq!(
        evaluate_current(&db, &client, MediaDomain::Tv)
            .await
            .unwrap()
            .unwrap()
            .reason,
        "cdh_disabled"
    );
    peer.mode.store(2, Ordering::SeqCst);
    let checking = evaluate_current(&db, &client, MediaDomain::Tv);
    let deleting = async {
        tokio::time::timeout(Duration::from_secs(5), peer.entered.notified())
            .await
            .unwrap();
        let current = request(&api, "GET", &format!("/api/v1/providers/{id}"), Value::Null).await;
        request(
            &api,
            "DELETE",
            &format!("/api/v1/providers/{id}?revision={}", current["revision"]),
            Value::Null,
        )
        .await;
        peer.release.notify_one();
    };
    let (outcome, ()) = tokio::join!(checking, deleting);
    assert_eq!(
        outcome,
        Err("check_failed"),
        "deleted provider cannot publish a status"
    );
    let other_db = Database::open_local(scratch.0.join("other-db"))
        .await
        .unwrap();
    assert_eq!(
        evaluate_current(&other_db, &client, MediaDomain::Movies).await,
        Err("check_failed"),
        "settings and status client must own the same database"
    );
    server.stop().await;
    peer_server.stop().await;
}
