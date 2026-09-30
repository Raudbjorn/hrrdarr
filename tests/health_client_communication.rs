//! Owned HTTP observations; no operator services or network fixtures.
use axum::{
    Router,
    extract::{OriginalUri, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use hrrdarr::{
    api::MediaDomain, db::Database, health::HealthSeverity,
    health_detectors::evaluate_communication, providers,
};
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
    mode: Arc<AtomicUsize>,
    items: Arc<AtomicUsize>,
    tests: Arc<AtomicUsize>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}
async fn peer(State(p): State<Peer>, OriginalUri(uri): OriginalUri) -> Response {
    let mode = p.mode.load(Ordering::SeqCst);
    match uri.path() {
        "/api/v2/app/webapiVersion" => "2.8.0".into_response(),
        "/api/v2/app/version" => {
            p.tests.fetch_add(1, Ordering::SeqCst);
            if mode == 8 {
                StatusCode::FORBIDDEN.into_response()
            } else {
                "4.5.0".into_response()
            }
        }
        "/api/v2/app/preferences" => axum::Json(json!({"queueing_enabled":true,"save_path":"/private","max_ratio_enabled":false,"max_seeding_time_enabled":false,"max_seeding_time":-1,"max_ratio_act":0})).into_response(),
        "/api/v2/torrents/categories" => axum::Json(
            json!({"tv":{"name":"tv","savePath":""},"movies":{"name":"movies","savePath":""}}),
        )
        .into_response(),
        "/api/v2/torrents/info" => {
            p.items.fetch_add(1, Ordering::SeqCst);
            if mode == 5 || mode == 6 || (mode == 9 && uri.query().unwrap_or("").contains("limit=500")) {
                p.entered.notify_one();
                p.release.notified().await;
            }
            if mode == 12 { tokio::time::sleep(Duration::from_secs(6)).await; }
            match mode {
                1 | 6 => (StatusCode::BAD_GATEWAY, "private-response-sentinel").into_response(),
                2 => StatusCode::FORBIDDEN.into_response(),
                3 => (
                    StatusCode::TOO_MANY_REQUESTS,
                    [("retry-after", "60")],
                    "private-limit-sentinel",
                )
                    .into_response(),
                4 => "not json private-malformed-sentinel".into_response(),
                7 | 10 => {
                    let domain = if uri.query().unwrap_or("").contains("category=movies") {
                        "movies"
                    } else {
                        "tv"
                    };
                    let item = json!({"hash":"1111111111111111111111111111111111111111","infohash_v1":"1111111111111111111111111111111111111111","infohash_v2":"","category":domain,"name":"private-item-sentinel","state":"downloading","progress":0.5,"size":100,"amount_left":50,"dlspeed":2,"upspeed":1,"eta":25,"ratio":0.2,"seeding_time":0,"save_path":"/private","content_path":"/private/item","priority":5,"force_start":false,"file_priority":1,"ratio_limit":-2.0,"seeding_time_limit":-2,"inactive_seeding_time_limit":-1,"seq_dl":false,"f_l_piece_prio":false,"auto_tmm":false,"tags":""});
                    if mode == 7 { axum::Json(json!([item])).into_response() } else {
                        let offset = url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes()).find(|(key,_)|key=="offset").map(|(_,value)|value.parse::<usize>().unwrap()).unwrap_or(0);
                        let items:Vec<_>=(offset..(offset+500).min(1001)).map(|n| { let mut row=item.clone(); let hash=format!("{:040x}",n+1);row["hash"]=json!(hash);row["infohash_v1"]=row["hash"].clone();row }).collect();
                        axum::Json(json!(items)).into_response()
                    }
                }
                11 => vec![b'x'; providers::http::MAX_BODY_BYTES+1].into_response(),
                _ => axum::Json(json!([])).into_response(),
            }
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}
async fn request(base: &str, method: &str, path: &str, body: Value) -> (u16, Value) {
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
    let status = response.status().as_u16();
    let text = response.text().await.unwrap();
    (
        status,
        if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap()
        },
    )
}
fn config(endpoint: &str, tv: bool, movies: bool, enabled: bool) -> Value {
    let scope = |category| json!({"category":category,"imported_category":null,"recent_priority":0,"older_priority":0});
    json!({"name":"Communication fixture","enabled":enabled,"priority":1,"settings":{"implementation":"qbittorrent","endpoint":endpoint,"tv":if tv{scope("tv")}else{Value::Null},"movies":if movies{scope("movies")}else{Value::Null}}})
}
async fn generations(db: &Database) -> Vec<(String, String, i64, Option<i64>)> {
    let c = db.connect().await.unwrap();
    let mut rows = c
        .query(
            "SELECT scope,check_key,generation,due_at FROM health_checks ORDER BY scope,check_key",
            (),
        )
        .await
        .unwrap();
    let mut result = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        result.push((
            r.get(0).unwrap(),
            r.get(1).unwrap(),
            r.get(2).unwrap(),
            r.get(3).unwrap(),
        ));
    }
    result
}
async fn fixture() -> (
    Scratch,
    Arc<Database>,
    String,
    Server,
    providers::RefreshClient,
    Peer,
    String,
    Server,
) {
    let path = std::env::temp_dir().join(format!(
        "hrrdarr-health-communication-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir(&path).unwrap();
    let scratch = Scratch(path);
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let (router, client) = providers::router_with_refresh(db.clone(), None);
    let (api, api_task) = serve(router).await;
    let p = Peer::default();
    let (endpoint, peer_task) =
        serve(Router::new().fallback(get(peer)).with_state(p.clone())).await;
    (scratch, db, api, api_task, client, p, endpoint, peer_task)
}
#[tokio::test]
async fn scoped_items_remote_failures_and_cached_cooldown_are_distinct() {
    let (_scratch, db, api, api_task, client, p, endpoint, peer_task) = fixture().await;
    for domain in [MediaDomain::Tv, MediaDomain::Movies] {
        let issue = evaluate_communication(&db, &client, domain)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(issue.severity, HealthSeverity::Warning);
        assert_eq!(issue.reason, "download_client_none_available");
    }
    let (status, mut provider) = request(
        &api,
        "POST",
        "/api/v1/providers",
        config(&endpoint, true, false, false),
    )
    .await;
    assert_eq!(status, 201);
    assert_eq!(
        evaluate_communication(&db, &client, MediaDomain::Tv)
            .await
            .unwrap()
            .unwrap()
            .severity,
        HealthSeverity::Warning
    );
    assert_eq!(p.items.load(Ordering::SeqCst), 0);
    let id = provider["id"].as_str().unwrap().to_owned();
    let mut update = config(&endpoint, true, false, true);
    update["revision"] = provider["revision"].clone();
    let (status, value) = request(&api, "PUT", &format!("/api/v1/providers/{id}"), update).await;
    assert_eq!(status, 200);
    provider = value;
    assert_eq!(
        evaluate_communication(&db, &client, MediaDomain::Movies)
            .await
            .unwrap()
            .unwrap()
            .severity,
        HealthSeverity::Warning
    );
    assert!(
        evaluate_communication(&db, &client, MediaDomain::Tv)
            .await
            .unwrap()
            .is_none()
    );
    assert!(p.items.load(Ordering::SeqCst) > 0);
    let mut update = config(&endpoint, true, true, true);
    update["revision"] = provider["revision"].clone();
    assert_eq!(
        request(&api, "PUT", &format!("/api/v1/providers/{id}"), update)
            .await
            .0,
        200
    );
    for mode in [0, 7, 1, 2, 4] {
        p.mode.store(mode, Ordering::SeqCst);
        for domain in [MediaDomain::Tv, MediaDomain::Movies] {
            let before = generations(&db).await;
            let issue = evaluate_communication(&db, &client, domain).await.unwrap();
            assert_eq!(
                generations(&db).await,
                before,
                "direct detector cannot invalidate itself"
            );
            if mode == 0 || mode == 7 {
                assert!(issue.is_none());
            } else {
                let issue = issue.unwrap();
                assert_eq!(issue.severity, HealthSeverity::Error);
                assert_eq!(issue.compatibility_type, "DownloadClientCheck");
                assert!(!format!("{issue:?}").contains("private-"));
            }
        }
    }
    p.mode.store(3, Ordering::SeqCst);
    assert_eq!(
        evaluate_communication(&db, &client, MediaDomain::Tv)
            .await
            .unwrap()
            .unwrap()
            .severity,
        HealthSeverity::Error
    );
    let before = p.items.load(Ordering::SeqCst);
    assert_eq!(
        evaluate_communication(&db, &client, MediaDomain::Tv).await,
        Err("check_failed")
    );
    assert_eq!(
        p.items.load(Ordering::SeqCst),
        before,
        "cached cooldown is not a new failed observation"
    );
    api_task.stop().await;
    peer_task.stop().await;
}
#[tokio::test]
async fn current_revision_fences_success_and_failure_and_cancellation_releases_lane() {
    let (_scratch, db, api, api_task, client, p, endpoint, peer_task) = fixture().await;
    let (status, created) = request(
        &api,
        "POST",
        "/api/v1/providers",
        config(&endpoint, true, true, true),
    )
    .await;
    assert_eq!(status, 201);
    let id = created["id"].as_str().unwrap();
    for mode in [5, 6] {
        p.mode.store(mode, Ordering::SeqCst);
        let checking = evaluate_communication(&db, &client, MediaDomain::Tv);
        let change = async {
            tokio::time::timeout(Duration::from_secs(5), p.entered.notified())
                .await
                .unwrap();
            let (_, current) =
                request(&api, "GET", &format!("/api/v1/providers/{id}"), Value::Null).await;
            let mut update = config(&endpoint, mode != 6, true, true);
            update["revision"] = current["revision"].clone();
            update["name"] = json!(format!("Changed {mode}"));
            assert_eq!(
                request(&api, "PUT", &format!("/api/v1/providers/{id}"), update)
                    .await
                    .0,
                200
            );
            p.release.notify_one();
        };
        let (result, ()) = tokio::join!(checking, change);
        assert_eq!(result, Err("check_failed"));
    }
    let (_, current) = request(&api, "GET", &format!("/api/v1/providers/{id}"), Value::Null).await;
    let mut update = config(&endpoint, true, true, true);
    update["revision"] = current["revision"].clone();
    assert_eq!(
        request(&api, "PUT", &format!("/api/v1/providers/{id}"), update)
            .await
            .0,
        200
    );
    p.mode.store(5, Ordering::SeqCst);
    {
        let check = evaluate_communication(&db, &client, MediaDomain::Tv);
        tokio::pin!(check);
        tokio::select! {result=&mut check=>panic!("unexpected early result {result:?}"),entered=tokio::time::timeout(Duration::from_secs(5),p.entered.notified())=>entered.unwrap(),}
        assert_eq!(
            evaluate_communication(&db, &client, MediaDomain::Tv).await,
            Err("check_failed"),
            "held lane is local capacity, not remote failure"
        );
    }
    p.release.notify_one();
    p.mode.store(0, Ordering::SeqCst);
    assert!(
        evaluate_communication(&db, &client, MediaDomain::Tv)
            .await
            .unwrap()
            .is_none()
    );
    p.mode.store(6, Ordering::SeqCst);
    let checking = evaluate_communication(&db, &client, MediaDomain::Tv);
    let deleting = async {
        tokio::time::timeout(Duration::from_secs(5), p.entered.notified())
            .await
            .unwrap();
        let (_, current) =
            request(&api, "GET", &format!("/api/v1/providers/{id}"), Value::Null).await;
        assert_eq!(
            request(
                &api,
                "DELETE",
                &format!("/api/v1/providers/{id}?revision={}", current["revision"]),
                Value::Null
            )
            .await
            .0,
            204
        );
        p.release.notify_one();
    };
    let (result, ()) = tokio::join!(checking, deleting);
    assert_eq!(
        result,
        Err("check_failed"),
        "deleted revision cannot publish a failed observation"
    );
    api_task.stop().await;
    peer_task.stop().await;
}
#[tokio::test]
async fn provider_test_markers_deduplicate_normalized_outcomes_and_rollback_atomically() {
    let (_scratch, db, api, api_task, _client, p, endpoint, peer_task) = fixture().await;
    let (status, created) = request(
        &api,
        "POST",
        "/api/v1/providers",
        config(&endpoint, true, true, true),
    )
    .await;
    assert_eq!(status, 201);
    let id = created["id"].as_str().unwrap();
    let test_path = format!("/api/v1/providers/{id}/test");
    let before = generations(&db).await;
    assert_eq!(request(&api, "POST", &test_path, Value::Null).await.0, 200);
    let first = generations(&db).await;
    for (old, new) in before.iter().zip(&first) {
        assert_eq!(
            new.2,
            old.2
                + if old.1 == "download_client_communication" {
                    1
                } else {
                    0
                }
        );
    }
    assert_eq!(request(&api, "POST", &test_path, Value::Null).await.0, 200);
    assert_eq!(
        generations(&db).await,
        first,
        "timestamp-only success cannot invalidate or postpone health"
    );
    p.mode.store(8, Ordering::SeqCst);
    assert_eq!(request(&api, "POST", &test_path, Value::Null).await.0, 502);
    let failed = generations(&db).await;
    for (old, new) in first.iter().zip(&failed) {
        if old.1 == "download_client_communication" {
            assert_eq!(new.2, old.2 + 1);
            assert!(new.3 <= old.3);
        }
    }
    assert_eq!(request(&api, "POST", &test_path, Value::Null).await.0, 502);
    assert_eq!(generations(&db).await, failed);
    let c = db.connect().await.unwrap();
    c.execute_batch("CREATE TRIGGER reject_health_marker BEFORE UPDATE OF generation ON health_checks BEGIN SELECT RAISE(ABORT,'marker rejected'); END;").await.unwrap();
    p.mode.store(0, Ordering::SeqCst);
    assert!(!matches!(
        request(&api, "POST", &test_path, Value::Null).await.0,
        200
    ));
    assert_eq!(generations(&db).await, failed);
    let status: String = c
        .query(
            "SELECT status FROM provider_tests WHERE provider_id=?",
            [id],
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(status, "failure", "outcome and marker roll back together");
    c.execute_batch("DROP TRIGGER reject_health_marker;")
        .await
        .unwrap();
    assert_eq!(request(&api, "POST", &test_path, Value::Null).await.0, 200);
    api_task.stop().await;
    peer_task.stop().await;
}

#[tokio::test]
async fn unreadable_credentials_invalid_configuration_and_provider_bound_are_unevaluated() {
    let (_scratch, db, api, api_task, client, p, endpoint, peer_task) = fixture().await;
    let indexer = json!({"name":"Not a download client","enabled":true,"priority":1,"settings":{"implementation":"torznab","endpoint":endpoint,"tv":{"categories":[5000],"anime_categories":[]},"movies":null}});
    let (status, indexer) = request(&api, "POST", "/api/v1/providers", indexer).await;
    assert_eq!(status, 201);
    let before = generations(&db).await;
    assert_ne!(
        request(
            &api,
            "POST",
            &format!("/api/v1/providers/{}/test", indexer["id"].as_str().unwrap()),
            Value::Null
        )
        .await
        .0,
        200
    );
    assert_eq!(
        generations(&db).await,
        before,
        "indexer test outcome cannot dirty download-client checks"
    );
    assert_eq!(
        evaluate_communication(&db, &client, MediaDomain::Tv)
            .await
            .unwrap()
            .unwrap()
            .reason,
        "download_client_none_available"
    );
    let (status, created) = request(
        &api,
        "POST",
        "/api/v1/providers",
        config(&endpoint, true, true, true),
    )
    .await;
    assert_eq!(status, 201);
    let id = created["id"].as_str().unwrap();
    let c = db.connect().await.unwrap();
    c.execute(
        "UPDATE providers SET credentials=zeroblob(29),revision=revision+1 WHERE id=?",
        [id],
    )
    .await
    .unwrap();
    assert_eq!(
        evaluate_communication(&db, &client, MediaDomain::Tv).await,
        Err("check_failed")
    );
    assert_eq!(p.items.load(Ordering::SeqCst), 0);
    c.execute(
        "UPDATE providers SET credentials=NULL,endpoint='not-a-url',revision=revision+1 WHERE id=?",
        [id],
    )
    .await
    .unwrap();
    assert_eq!(
        evaluate_communication(&db, &client, MediaDomain::Tv).await,
        Err("check_failed")
    );
    assert_eq!(p.items.load(Ordering::SeqCst), 0);
    c.execute(
        "UPDATE providers SET endpoint=?,revision=revision+1 WHERE id=?",
        libsql::params![endpoint.clone(), id],
    )
    .await
    .unwrap();
    let tx = c.transaction().await.unwrap();
    for _ in 0..256 {
        let key = uuid::Uuid::new_v4().to_string();
        tx.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'qbittorrent','Bound fixture',1,1,1,1,?)",libsql::params![key.clone(),endpoint.clone()]).await.unwrap();
        for (domain, category) in [("tv", "tv"), ("movies", "movies")] {
            // Migration 0012 requires explicit client options; unlike the API,
            // raw fixture inserts do not apply the validated scope defaults.
            tx.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags) VALUES(?,'qbittorrent',?,?,0,0,'started','default',0,0,0)",libsql::params![key.clone(),domain,category]).await.unwrap();
        }
    }
    tx.commit().await.unwrap();
    for domain in [MediaDomain::Tv, MediaDomain::Movies] {
        assert_eq!(
            evaluate_communication(&db, &client, domain).await,
            Err("check_failed")
        );
    }
    assert_eq!(
        p.items.load(Ordering::SeqCst),
        0,
        "overflow detected before any bounded-prefix success can masquerade as full coverage"
    );
    api_task.stop().await;
    peer_task.stop().await;
}

#[tokio::test]
async fn identical_tests_do_not_invalidate_blocked_probe_but_changed_outcomes_do() {
    let (_scratch, db, api, api_task, client, p, endpoint, peer_task) = fixture().await;
    let (status, created) = request(
        &api,
        "POST",
        "/api/v1/providers",
        config(&endpoint, true, true, true),
    )
    .await;
    assert_eq!(status, 201);
    let (status, second) = request(
        &api,
        "POST",
        "/api/v1/providers",
        config(&endpoint, true, true, true),
    )
    .await;
    assert_eq!(status, 201);
    // Health probes in ID order. Observe the later provider via its independent
    // shared-transport lane while the first provider's item request is blocked.
    let later = std::cmp::max(
        created["id"].as_str().unwrap(),
        second["id"].as_str().unwrap(),
    );
    let path = format!("/api/v1/providers/{later}/test");
    assert_eq!(request(&api, "POST", &path, Value::Null).await.0, 200);
    p.mode.store(9, Ordering::SeqCst);
    let check = evaluate_communication(&db, &client, MediaDomain::Tv);
    let testing = async {
        tokio::time::timeout(Duration::from_secs(5), p.entered.notified())
            .await
            .unwrap();
        let before = generations(&db).await;
        for _ in 0..2 {
            assert_eq!(request(&api, "POST", &path, Value::Null).await.0, 200);
        }
        assert_eq!(generations(&db).await, before);
        p.mode.store(8, Ordering::SeqCst);
        assert_eq!(request(&api, "POST", &path, Value::Null).await.0, 502);
        let changed = generations(&db).await;
        for (old, new) in before.iter().zip(&changed) {
            assert_eq!(
                new.2,
                old.2
                    + if old.1 == "download_client_communication" {
                        1
                    } else {
                        0
                    }
            );
        }
        p.release.notify_one();
    };
    let (result, ()) = tokio::join!(check, testing);
    assert!(
        result.unwrap().is_none(),
        "engine generation fence owns publication; direct probe only returns observations"
    );
    api_task.stop().await;
    peer_task.stop().await;
}

#[tokio::test]
async fn exhausted_markers_preserve_authoritative_test_outcome_with_diagnostic() {
    let (_scratch, db, api, api_task, _client, _p, endpoint, peer_task) = fixture().await;
    let (status, created) = request(
        &api,
        "POST",
        "/api/v1/providers",
        config(&endpoint, true, true, true),
    )
    .await;
    assert_eq!(status, 201);
    let id = created["id"].as_str().unwrap();
    let c = db.connect().await.unwrap();
    // No commands/members exist in this no-worker fixture. Reinsert exhausted
    // registry rows rather than weakening their one-step generation constraint.
    c.execute_batch("DELETE FROM health_checks WHERE check_key='download_client_communication';INSERT INTO health_checks(scope,check_key,startup,scheduled,generation,compatibility_type) VALUES('tv','download_client_communication',1,1,9007199254740991,'DownloadClientCheck'),('movies','download_client_communication',1,1,9007199254740991,'DownloadClientCheck');").await.unwrap();
    assert_eq!(
        request(
            &api,
            "POST",
            &format!("/api/v1/providers/{id}/test"),
            Value::Null
        )
        .await
        .0,
        200
    );
    let status: String = c
        .query(
            "SELECT status FROM provider_tests WHERE provider_id=?",
            [id],
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(status, "success");
    let error: String = c
        .query("SELECT schedule_error FROM health_lifecycle WHERE id=1", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(error, "health_invariant");
    assert!(
        generations(&db)
            .await
            .iter()
            .filter(|r| r.1 == "download_client_communication")
            .all(|r| r.2 == 9_007_199_254_740_991)
    );
    api_task.stop().await;
    peer_task.stop().await;
}

#[tokio::test]
async fn item_and_response_limits_are_not_healthy_or_remote_failures() {
    let (_scratch, db, api, api_task, client, p, endpoint, peer_task) = fixture().await;
    assert_eq!(
        request(
            &api,
            "POST",
            "/api/v1/providers",
            config(&endpoint, true, true, true)
        )
        .await
        .0,
        201
    );
    for mode in [10, 11] {
        p.mode.store(mode, Ordering::SeqCst);
        for domain in [MediaDomain::Tv, MediaDomain::Movies] {
            assert_eq!(
                evaluate_communication(&db, &client, domain).await,
                Err("check_failed")
            );
        }
    }
    api_task.stop().await;
    peer_task.stop().await;
}

#[tokio::test]
async fn connection_refusal_is_an_evaluated_communication_issue() {
    let (_scratch, db, api, api_task, client, _p, _endpoint, peer_task) = fixture().await;
    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let closed = format!("http://{}", socket.local_addr().unwrap());
    drop(socket);
    assert_eq!(
        request(
            &api,
            "POST",
            "/api/v1/providers",
            config(&closed, true, true, true)
        )
        .await
        .0,
        201
    );
    for domain in [MediaDomain::Tv, MediaDomain::Movies] {
        assert_eq!(
            evaluate_communication(&db, &client, domain)
                .await
                .unwrap()
                .unwrap()
                .severity,
            HealthSeverity::Error
        );
    }
    api_task.stop().await;
    peer_task.stop().await;
}

#[tokio::test]
async fn actual_item_request_timeout_is_an_evaluated_remote_failure() {
    let (_scratch, db, api, api_task, client, p, endpoint, peer_task) = fixture().await;
    assert_eq!(
        request(
            &api,
            "POST",
            "/api/v1/providers",
            config(&endpoint, true, true, true)
        )
        .await
        .0,
        201
    );
    p.mode.store(12, Ordering::SeqCst);
    for domain in [MediaDomain::Tv, MediaDomain::Movies] {
        let before = p.items.load(Ordering::SeqCst);
        assert_eq!(
            evaluate_communication(&db, &client, domain)
                .await
                .unwrap()
                .unwrap()
                .severity,
            HealthSeverity::Error
        );
        assert_eq!(
            p.items.load(Ordering::SeqCst),
            before + 1,
            "items handler entered before network read timed out"
        );
    }
    api_task.stop().await;
    peer_task.stop().await;
}

#[tokio::test]
async fn communication_issue_identifies_failing_configured_client_without_remote_payloads() {
    let (_scratch, db, api, api_task, client, p, endpoint, peer_task) = fixture().await;
    let good = Peer::default();
    let (good_endpoint, good_task) =
        serve(Router::new().fallback(get(peer)).with_state(good)).await;
    let mut healthy = config(&good_endpoint, true, true, true);
    healthy["name"] = json!("Healthy configured client");
    assert_eq!(
        request(&api, "POST", "/api/v1/providers", healthy).await.0,
        201
    );
    let mut failed = config(&endpoint, true, true, true);
    failed["name"] = json!("Broken configured client <tag>");
    let (status, failed) = request(&api, "POST", "/api/v1/providers", failed).await;
    assert_eq!(status, 201);
    p.mode.store(1, Ordering::SeqCst);
    for domain in [MediaDomain::Tv, MediaDomain::Movies] {
        let issue = evaluate_communication(&db, &client, domain)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(issue.severity, HealthSeverity::Error);
        assert!(issue.message.contains("Broken configured client <tag>"));
        assert!(issue.message.contains(failed["id"].as_str().unwrap()));
        assert!(!issue.message.contains("Healthy configured client"));
        assert!(!issue.message.contains("private-"));
        assert!(!issue.message.contains(&endpoint));
        assert!(issue.message.len() < 512);
    }
    good_task.stop().await;
    api_task.stop().await;
    peer_task.stop().await;
}
