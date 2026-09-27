//! Owned Hrana fixture checks the real remote driver/startup path, not a Turso deployment.
use axum::{Router, extract::State, http::Uri, routing::post};
use hrrdarr::db::{Database, StorageCompatibilityError};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct Fixture {
    conn: libsql::Connection,
    version: &'static str,
    source_id: &'static str,
    reject_json: bool,
    requests: Arc<Mutex<Vec<String>>>,
}

async fn respond(State(f): State<Fixture>, uri: Uri, body: String) -> String {
    let body: Value = serde_json::from_str(&body).unwrap();
    let cursor = uri.path().ends_with("cursor");
    let requests = if cursor {
        body["batch"]["steps"].as_array().unwrap()
    } else {
        body["requests"].as_array().unwrap()
    };
    let mut results = Vec::new();
    for request in requests {
        if request["type"] == "close" {
            results.push(json!({"type":"ok","response":{"type":"close"}}));
            continue;
        }
        if request["type"] == "get_autocommit" {
            results.push(
                json!({"type":"ok","response":{"type":"get_autocommit","is_autocommit":true}}),
            );
            continue;
        }
        if request["type"] == "describe" {
            f.requests
                .lock()
                .unwrap()
                .push(request["sql"].as_str().unwrap().to_owned());
            results.push(json!({"type":"ok","response":{"type":"describe","result":{
                "params":[],"cols":[],"is_explain":false,"is_readonly":true}}}));
            continue;
        }
        let batch = request["type"] == "batch";
        let statement = if batch {
            &request["batch"]["steps"][0]["stmt"]
        } else {
            &request["stmt"]
        };
        let sql = statement["sql"]
            .as_str()
            .unwrap_or_else(|| panic!("unexpected fixture request {request}"));
        f.requests.lock().unwrap().push(sql.to_owned());
        let (cols, rows) = if sql.contains("sqlite_version") {
            (
                json!([{"name":"sqlite_version()"},{"name":"sqlite_source_id()"}]),
                vec![
                    json!([{"type":"text","value":f.version},{"type":"text","value":f.source_id}]),
                ],
            )
        } else if f.reject_json && sql.contains("json_valid") {
            let error = json!({"message":"secret-server-error-token","code":"SQLITE_ERROR"});
            assert!(cursor);
            return format!(
                "{}\n{}\n",
                json!({"baton":null,"base_url":null}),
                json!({"type":"step_error","step":0,"error":error})
            );
        } else {
            let mut result = f.conn.query(sql, ()).await.unwrap();
            let cols: Vec<_> = (0..result.column_count())
                .map(|i| json!({"name":result.column_name(i)}))
                .collect();
            let mut rows = Vec::new();
            while let Some(row) = result.next().await.unwrap() {
                let values: Vec<_> = (0..result.column_count())
                    .map(|i| match row.get_value(i).unwrap() {
                        libsql::Value::Null => json!({"type":"null"}),
                        libsql::Value::Integer(n) => {
                            json!({"type":"integer","value":n.to_string()})
                        }
                        libsql::Value::Text(s) => json!({"type":"text","value":s}),
                        other => panic!("unexpected fixture value {other:?}"),
                    })
                    .collect();
                rows.push(json!(values));
            }
            (json!(cols), rows)
        };
        if cursor {
            let mut lines = vec![
                json!({"baton":null,"base_url":null}),
                json!({"type":"step_begin","step":0,"cols":cols}),
            ];
            lines.extend(rows.into_iter().map(|row| json!({"type":"row","row":row})));
            lines
                .push(json!({"type":"step_end","affected_row_count":0,"last_inserted_rowid":null}));
            return lines.into_iter().map(|v| format!("{v}\n")).collect();
        }
        let result =
            json!({"cols":cols,"rows":rows,"affected_row_count":0,"last_insert_rowid":null});
        let response = if batch {
            json!({"type":"batch","result":{"step_results":[result],"step_errors":[null]}})
        } else {
            json!({"type":"execute","result":result})
        };
        results.push(json!({"type":"ok","response":response}));
    }
    json!({"baton":null,"base_url":null,"results":results}).to_string()
}

#[tokio::test]
async fn engine_preflight_local_and_remote_rejection_before_migration() {
    let dir = std::env::temp_dir().join(format!("hrrdarr-engine-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let local = Database::open_local(dir.join("db")).await.unwrap();
    let conn = local.connect().await.unwrap();
    let identity = conn
        .query("SELECT sqlite_version(), sqlite_source_id()", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        local.storage_engine().sqlite_version,
        identity.get::<String>(0).unwrap()
    );
    assert_eq!(
        local.storage_engine().sqlite_source_id,
        identity.get::<String>(1).unwrap()
    );
    let before = conn
        .query("SELECT count(*) FROM schema_migrations", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<i64>(0)
        .unwrap();
    for (version, source_id, reject_json, capability) in [
        ("3.45.1", "2024-01-30 16:01:20 abcdefalt1", false, None),
        ("3.45.1", "unsafe-source-secret", false, None),
        (
            "3.9.0",
            "abcdef",
            false,
            Some("SQLite SQL baseline >= 3.38.0"),
        ),
        ("3.38.0", "abcdef", false, None),
        (
            "garbage\nsecret-token",
            "abcdef",
            false,
            Some("engine identity"),
        ),
        (
            "3.45.1",
            "abcdef",
            true,
            Some("structural JSON and UTC date functions"),
        ),
    ] {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let fixture = Fixture {
            conn: conn.clone(),
            version,
            source_id,
            reject_json,
            requests: requests.clone(),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/v3/cursor", post(respond))
            .route("/v3/pipeline", post(respond))
            .with_state(fixture);
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            Database::open_remote(format!("http://{addr}"), "fixture-token".into()),
        )
        .await
        .unwrap();
        server.abort();
        let _ = server.await;
        if let Some(capability) = capability {
            let error = match result {
                Ok(_) => panic!("unsupported engine accepted"),
                Err(e) => e,
            };
            let error = error.downcast_ref::<StorageCompatibilityError>().unwrap();
            assert_eq!(error.capability, capability);
            assert_eq!(
                error
                    .engine
                    .as_ref()
                    .map(|engine| engine.sqlite_version.as_str()),
                (capability != "engine identity").then_some(version)
            );
            assert!(!format!("{error:?} {error}").contains("secret"));
            let queries = requests.lock().unwrap();
            assert!(!queries.is_empty());
            // Rejection precedes connection setup, integrity scans, backup or schema writes.
            assert!(
                queries
                    .iter()
                    .all(|s| s.contains("sqlite_version") || s.contains("json_valid")),
                "{queries:?}"
            );
        } else {
            let remote = result.unwrap();
            assert_eq!(remote.storage_engine().sqlite_version, version);
            assert_eq!(
                remote.storage_engine().sqlite_source_id,
                if source_id == "unsafe-source-secret" {
                    "unavailable"
                } else {
                    source_id
                }
            );
            assert!(!remote.permits_local_imports());
            assert!(!remote.permits_private_snapshots());
            assert!(
                requests
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|s| s.contains("schema_migrations"))
            );
        }
    }
    let after = conn
        .query("SELECT count(*) FROM schema_migrations", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<i64>(0)
        .unwrap();
    assert_eq!(before, after);
    drop(conn);
    drop(local);
    std::fs::remove_dir_all(dir).unwrap();
}
