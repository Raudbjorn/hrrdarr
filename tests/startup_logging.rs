//! Capture the actual executable: response redaction alone cannot prove log secrecy.
use axum::{Router, extract::State, http::Uri, routing::post};
use serde_json::{Value, json};
use std::{
    io::Read,
    process::Command,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const SECRET: &str = "sentinel-upstream-api-key-and-password";

async fn hrana(
    State((seen, version)): State<(Arc<Mutex<Vec<String>>>, &'static str)>,
    uri: Uri,
    body: String,
) -> String {
    let body: Value = serde_json::from_str(&body).unwrap();
    let cursor = uri.path().ends_with("cursor");
    if body["requests"][0]["type"] == "describe" {
        return json!({"baton":null,"base_url":null,"results":[{"type":"ok","response":{"type":"describe","result":{"params":[],"cols":[],"is_explain":false,"is_readonly":true}}}]}).to_string();
    }
    let sql = if cursor {
        &body["batch"]["steps"][0]["stmt"]["sql"]
    } else if body["requests"][0]["type"] == "batch" {
        &body["requests"][0]["batch"]["steps"][0]["stmt"]["sql"]
    } else {
        &body["requests"][0]["stmt"]["sql"]
    };
    let sql = sql
        .as_str()
        .unwrap_or_else(|| panic!("fixture unexpected {body}"));
    seen.lock().unwrap().push(sql.into());
    let (cols, row) = if sql.contains("sqlite_version") {
        (
            json!([{"name":"version"},{"name":"source"}]),
            json!([{"type":"text","value":version},{"type":"text","value":"abcdef"}]),
        )
    } else if sql.contains("json_valid") {
        (
            json!([{"name":"supported"}]),
            json!([{"type":"integer","value":"1"}]),
        )
    } else {
        let error = json!({"code":"SQLITE_ERROR","message":SECRET});
        return if cursor {
            format!(
                "{}\n{}\n",
                json!({"baton":null,"base_url":null}),
                json!({"type":"step_error","step":0,"error":error})
            )
        } else {
            json!({"baton":null,"base_url":null,"results":[{"type":"error","error":error}]})
                .to_string()
        };
    };
    assert!(cursor);
    [
        json!({"baton":null,"base_url":null}),
        json!({"type":"step_begin","step":0,"cols":cols}),
        json!({"type":"row","row":row}),
        json!({"type":"step_end","affected_row_count":0,"last_inserted_rowid":null}),
    ]
    .into_iter()
    .map(|v| format!("{v}\n"))
    .collect()
}

struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
fn run(env: &[(&str, &str)]) -> (std::process::ExitStatus, String) {
    let dir = Scratch(std::env::temp_dir().join(format!("hrrdarr-log-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&dir.0).unwrap();
    let path = dir.0.join("output");
    let env: Vec<_> = env
        .iter()
        .map(|(key, value)| {
            (
                *key,
                if *key == "HRRDARR_DATABASE_PATH" {
                    dir.0.join(value).to_str().unwrap().to_owned()
                } else {
                    (*value).to_owned()
                },
            )
        })
        .collect();
    let output = std::fs::File::create(&path).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_hrrdarr"))
        .env_clear()
        .env("HRRDARR_DATABASE_PATH", dir.0.join("db"))
        .env("HRRDARR_BIND", "127.0.0.1:0")
        .envs(env.iter().map(|(key, value)| (*key, value)))
        .current_dir(&dir.0)
        .stdout(output.try_clone().unwrap())
        .stderr(output)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline || std::fs::metadata(&path).unwrap().len() > 64 * 1024 {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("startup subprocess exceeded time/output bound");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .unwrap()
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(bytes.len() <= 64 * 1024);
    (status, String::from_utf8(bytes).unwrap())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_failure_after_engine_preflight_never_logs_upstream_secrets() {
    for version in ["3.45.1", "3.9.0"] {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/v3/cursor", post(hrana))
            .route("/v3/pipeline", post(hrana))
            .with_state((seen.clone(), version));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let (status, output) = run(&[
            ("TURSO_DATABASE_URL", &format!("http://{addr}")),
            ("TURSO_AUTH_TOKEN", "sentinel-auth-token"),
        ]);
        server.abort();
        let _ = server.await;
        assert!(!status.success());
        let seen = seen.lock().unwrap();
        if version == "3.9.0" {
            let event: Value = serde_json::from_str(output.trim()).unwrap();
            assert_eq!(event["error_class"], "storage_compatibility");
            assert_eq!(
                event["storage"]["capability"],
                "SQLite SQL baseline >= 3.38.0"
            );
            assert_eq!(event["storage"]["sqlite_version"], version);
            assert_eq!(event["storage"]["sqlite_source_id"], "abcdef");
            assert!(!seen.iter().any(|s| s.contains("foreign_keys")));
            continue;
        }
        assert!(seen.iter().any(|s| s.contains("json_valid")));
        assert!(
            seen.iter().any(|s| s.contains("foreign_keys")),
            "must reach past sanitized engine preflight"
        );
        assert!(
            !output.contains(SECRET),
            "upstream secret reached process diagnostics"
        );
        assert!(!output.contains("sentinel-auth-token"));
        assert!(output.contains("database_open"), "{output}");
    }
}

#[test]
fn invalid_environment_values_have_safe_phase_diagnostics() {
    for (key, value, phase) in [
        (
            "HRRDARR_PROVIDER_KEY",
            "sentinel-invalid-provider-key",
            "provider_key",
        ),
        // HostConfig now validates bind/list together before opening the database.
        // Assert that intentional preflight phase; failure and secret checks stay strict.
        (
            "HRRDARR_BIND",
            "sentinel-invalid-bind",
            "host_configuration",
        ),
        (
            "HRRDARR_ALLOWED_HOSTS",
            "https://sentinel-invalid-host",
            "host_configuration",
        ),
        (
            "HRRDARR_DATABASE_PATH",
            "sentinel-missing-directory/db",
            "database_open",
        ),
    ] {
        let (status, output) = run(&[(key, value)]);
        assert!(!status.success());
        let event: Value = serde_json::from_str(output.trim()).unwrap();
        assert_eq!(event["event"], "process_failed");
        assert_eq!(event["phase"], phase);
        assert!(event["error_class"].is_string());
        assert!(!output.contains("sentinel"));
    }
}
