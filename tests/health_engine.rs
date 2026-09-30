use hrrdarr::{commands, completed_download_handling, db::Database, health, providers};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.0) {
            eprintln!("health API scratch cleanup: {e}")
        }
    }
}
async fn request(
    client: &reqwest::Client,
    base: &str,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let mut req = client.request(method, format!("{base}{path}"));
    if let Some(body) = body {
        req = req
            .header("content-type", "application/json")
            .body(body.to_string())
    }
    let r = req.send().await.unwrap();
    let status = r.status().as_u16();
    let text = r.text().await.unwrap();
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}
async fn wait_current(client: &reqwest::Client, base: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            let (status, v) =
                request(client, base, reqwest::Method::GET, "/api/v1/health", None).await;
            assert_eq!(status, 200);
            if v["summary"]["current"] == 2 {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn real_health_api_and_equal_value_cdh_save_drive_owned_worker() {
    let path = std::env::temp_dir().join(format!("hrrdarr-health-api-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&path).unwrap();
    let _scratch = Scratch(path.clone());
    let db = Arc::new(Database::open_local(path.join("db")).await.unwrap());
    let (provider_routes, refresh) = providers::router_with_refresh(db.clone(), None);
    let app = health::router(db.clone())
        .merge(completed_download_handling::router(db.clone()))
        .merge(provider_routes);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let (status, fresh) =
        request(&client, &base, reqwest::Method::GET, "/api/v1/health", None).await;
    assert_eq!(status, 200);
    assert_eq!(fresh["summary"]["never_run"], 2);
    assert_eq!(fresh["summary"]["current"], 0);
    assert_eq!(fresh["coverage"]["registered_only"], true);
    for query in ["?limit=0", "?offset=128", "?scope=episode", "?unknown=true"] {
        assert_eq!(
            request(
                &client,
                &base,
                reqwest::Method::GET,
                &format!("/api/v1/health{query}"),
                None
            )
            .await
            .0,
            400
        )
    }
    assert_eq!(
        request(
            &client,
            &base,
            reqwest::Method::POST,
            "/api/v1/health/commands",
            Some(json!({"scope":"all"}))
        )
        .await
        .0,
        400
    );
    assert_eq!(
        request(
            &client,
            &base,
            reqwest::Method::POST,
            "/api/v1/health/commands",
            Some(json!({"scope":"system","priority":"normal"}))
        )
        .await
        .0,
        409
    );
    let (status, queued) = request(
        &client,
        &base,
        reqwest::Method::POST,
        "/api/v1/health/commands",
        Some(json!({"scope":"tv","priority":"normal"})),
    )
    .await;
    assert_eq!(status, 202);
    let id = queued["command_id"].as_str().unwrap();
    let (status, union) = request(
        &client,
        &base,
        reqwest::Method::POST,
        "/api/v1/health/commands",
        Some(json!({"scope":"movies","priority":"high"})),
    )
    .await;
    assert_eq!(status, 202);
    assert_eq!(union["outcome"], "coalesced_queued");
    assert_eq!(union["command_id"], id);
    let (_, detail) = request(
        &client,
        &base,
        reqwest::Method::GET,
        &format!("/api/v1/health/commands/{id}"),
        None,
    )
    .await;
    assert_eq!(detail["members"].as_array().unwrap().len(), 2);
    let (_, filtered) = request(
        &client,
        &base,
        reqwest::Method::GET,
        "/api/v1/health/commands?scope=movies",
        None,
    )
    .await;
    assert_eq!(filtered["total"], 1);
    assert_eq!(
        request(
            &client,
            &base,
            reqwest::Method::POST,
            &format!("/api/v1/health/commands/{id}/cancel"),
            Some(json!({}))
        )
        .await
        .0,
        400
    );
    for _ in 0..2 {
        let (status, cancelled) = request(
            &client,
            &base,
            reqwest::Method::POST,
            &format!("/api/v1/health/commands/{id}/cancel"),
            None,
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(cancelled["status"], "cancelled");
    }
    // Real provider mutations dirty the union of old/new domains, including bulk deletion.
    let c = db.connect().await.unwrap();
    let tv_before = c
        .query("SELECT generation FROM health_checks WHERE scope='tv'", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<i64>(0)
        .unwrap();
    let scope =
        json!({"category":"tv","imported_category":null,"recent_priority":0,"older_priority":0});
    let mut config = json!({"name":"Health trigger fixture","enabled":false,"priority":1,"settings":{"implementation":"qbittorrent","endpoint":"http://127.0.0.1:1","tv":scope.clone(),"movies":null}});
    let (status, provider) = request(
        &client,
        &base,
        reqwest::Method::POST,
        "/api/v1/providers",
        Some(config.clone()),
    )
    .await;
    assert_eq!(status, 201, "{provider}");
    assert_eq!(
        c.query("SELECT generation FROM health_checks WHERE scope='tv'", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<i64>(0)
            .unwrap(),
        tv_before + 1
    );
    config["settings"]["tv"] = Value::Null;
    config["settings"]["movies"] = scope;
    config["revision"] = provider["revision"].clone();
    let (status, updated) = request(
        &client,
        &base,
        reqwest::Method::PUT,
        &format!("/api/v1/providers/{}", provider["id"].as_str().unwrap()),
        Some(config),
    )
    .await;
    assert_eq!(status, 200, "{updated}");
    assert_eq!(
        c.query("SELECT generation FROM health_checks WHERE scope='tv'", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<i64>(0)
            .unwrap(),
        tv_before + 2
    );
    let movie_before = c
        .query(
            "SELECT generation FROM health_checks WHERE scope='movies'",
            (),
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<i64>(0)
        .unwrap();
    let bulk = json!({"media_type":"movies","kind":"download_client","items":[{"id":updated["id"],"revision":updated["revision"]}],"changes":{"enabled":false}});
    let (status, bulk_result) = request(
        &client,
        &base,
        reqwest::Method::PUT,
        "/api/v1/providers/bulk",
        Some(bulk),
    )
    .await;
    assert_eq!(status, 200, "{bulk_result}");
    let deletion = json!({"media_type":"movies","kind":"download_client","items":[{"id":updated["id"],"revision":bulk_result["items"][0]["revision"]}]});
    let (status, error) = request(
        &client,
        &base,
        reqwest::Method::DELETE,
        "/api/v1/providers/bulk",
        Some(deletion),
    )
    .await;
    assert_eq!(status, 204, "{error}");
    assert_eq!(
        c.query(
            "SELECT generation FROM health_checks WHERE scope='movies'",
            ()
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<i64>(0)
        .unwrap(),
        movie_before + 2
    );
    let runtime = commands::start(db.clone(), refresh.clone()).await.unwrap();
    assert!(commands::start(db.clone(), refresh).await.is_err());
    let result = wait_current(&client, &base).await;
    assert_eq!(result["issues"].as_array().unwrap().len(), 1);
    assert_eq!(result["issues"][0]["identity"]["scope"], "tv");
    let (_, settings) = request(
        &client,
        &base,
        reqwest::Method::GET,
        "/api/v1/tv/completed-download-handling",
        None,
    )
    .await;
    assert_eq!(settings["enabled"], true);
    assert_eq!(settings["defined"], false);
    let body = json!({"enabled":true,"revision":settings["revision"]});
    assert_eq!(
        request(
            &client,
            &base,
            reqwest::Method::PUT,
            "/api/v1/tv/completed-download-handling",
            Some(body.clone())
        )
        .await
        .0,
        200
    );
    let (_, pending) = request(
        &client,
        &base,
        reqwest::Method::GET,
        "/api/v1/health?scope=tv",
        None,
    )
    .await;
    assert_eq!(pending["summary"]["stale"], 1);
    let generation = pending["checks"][0]["generation"].clone();
    assert_eq!(
        request(
            &client,
            &base,
            reqwest::Method::PUT,
            "/api/v1/tv/completed-download-handling",
            Some(body)
        )
        .await
        .0,
        409
    );
    let (_, unchanged) = request(
        &client,
        &base,
        reqwest::Method::GET,
        "/api/v1/health?scope=tv",
        None,
    )
    .await;
    assert_eq!(unchanged["checks"][0]["generation"], generation);
    let healthy = wait_current(&client, &base).await;
    assert!(healthy["issues"].as_array().unwrap().is_empty());
    let (_, transitions) = request(
        &client,
        &base,
        reqwest::Method::GET,
        "/api/v1/health/transitions",
        None,
    )
    .await;
    assert_eq!(transitions["items"].as_array().unwrap().len(), 2);
    assert_eq!(transitions["items"][1]["kind"], "restored");
    assert_eq!(
        transitions["items"][1]["issue"]["compatibility_type"],
        "ImportMechanismCheck"
    );
    runtime.shutdown().await;
    let c = db.connect().await.unwrap();
    let epoch = healthy["lifecycle"]["epoch"].as_str().unwrap();
    for _ in 0..1025 {
        c.execute("INSERT INTO health_transitions(event_id,epoch,command_id,scope,check_key,kind,in_grace,created_at,severity,reason,message,wiki_url,compatibility_type) VALUES(?,?,?,'tv','completed_download_handling','issue',0,1,2,'test','test','https://example.invalid','ImportMechanismCheck')",libsql::params![uuid::Uuid::new_v4().to_string(),epoch,uuid::Uuid::new_v4().to_string()]).await.unwrap();
    }
    let (_, gap) = request(
        &client,
        &base,
        reqwest::Method::GET,
        "/api/v1/health/transitions?after_sequence=1",
        None,
    )
    .await;
    assert_eq!(gap["gap"], true);
    assert_eq!(gap["items"].as_array().unwrap().len(), 16);
    assert_eq!(gap["next_cursor"], gap["items"][15]["sequence"]);
    assert_eq!(
        request(
            &client,
            &base,
            reqwest::Method::GET,
            "/api/v1/health/transitions?after_sequence=9007199254740992",
            None
        )
        .await
        .0,
        400
    );
    // Maximum-byte issue text with worst-case JSON escaping remains a bounded real response.
    let message = "\u{0001}".repeat(4096);
    let wiki = "\u{0001}".repeat(2048);
    c.execute("UPDATE health_checks SET severity=2,reason='bounded',message=?,wiki_url=? WHERE scope='tv'",libsql::params![message,wiki]).await.unwrap();
    let response = client
        .get(format!("{base}/api/v1/health"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let bytes = response.bytes().await.unwrap();
    assert!(bytes.len() < 1024 * 1024);
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["issues"][0]["message"].as_str().unwrap().len(), 4096);
    server.abort();
    let _ = server.await;
}
