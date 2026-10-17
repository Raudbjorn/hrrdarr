//! Real native endpoint checks; no source snapshot or filesystem import effects.
use hrrdarr::{db::Database, import_policy};
use serde_json::{Value, json};
use std::sync::Arc;

struct Scratch(std::path::PathBuf);
impl Scratch {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("hrrdarr-import-policy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
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
async fn server(db: Arc<Database>) -> (String, Server) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = import_policy::router(db);
    (
        base,
        Server(tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap()
        })),
    )
}
async fn raw_request(base: &str, method: &str, path: &str, body: String) -> (u16, Value) {
    let response = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap()
        .request(method.parse().unwrap(), format!("{base}{path}"))
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    let bytes = response.bytes().await.unwrap();
    assert!(bytes.len() <= 32 * 1024);
    (
        status,
        if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| panic!("non-JSON policy response, status {status}"))
        },
    )
}
async fn request(base: &str, method: &str, path: &str, body: Value) -> (u16, Value) {
    raw_request(base, method, path, body.to_string()).await
}
fn input(revision: i64) -> Value {
    json!({"revision":revision,"skip_free_space_check_when_importing":false,"minimum_free_space_mib":100,"download_client_working_folders":"_UNPACK_|_FAILED_"})
}
async fn get(base: &str, media: &str) -> Value {
    let (status, value) = request(
        base,
        "GET",
        &format!("/api/v1/{media}/import-policy"),
        Value::Null,
    )
    .await;
    assert_eq!(status, 200);
    value
}
async fn local(c: &libsql::Connection, media: &str) -> i64 {
    c.query(
        "SELECT locally_edited FROM import_policies WHERE media_type=?",
        [media],
    )
    .await
    .unwrap()
    .next()
    .await
    .unwrap()
    .unwrap()
    .get(0)
    .unwrap()
}

#[tokio::test]
async fn import_policy_defaults_domain_isolation_and_cas() {
    let scratch = Scratch::new();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let c = db.connect().await.unwrap();
    let (base, _server) = server(db).await;
    for media in ["tv", "movies"] {
        let value = get(&base, media).await;
        assert_eq!(
            value,
            json!({"media_type":media,"revision":1,"skip_free_space_check_when_importing":false,"minimum_free_space_mib":100,"download_client_working_folders":"_UNPACK_|_FAILED_"})
        );
        assert_eq!(local(&c, media).await, 0);
    }
    let mut tv = input(1);
    tv["skip_free_space_check_when_importing"] = json!(true);
    tv["minimum_free_space_mib"] = json!(250);
    tv["download_client_working_folders"] = json!(" custom||prefix ");
    let (status, saved) = request(&base, "PUT", "/api/v1/tv/import-policy", tv.clone()).await;
    assert_eq!(status, 200);
    assert_eq!(saved["revision"], 2);
    assert_eq!(saved["download_client_working_folders"], " custom||prefix ");
    assert_eq!(local(&c, "tv").await, 1);
    assert_eq!(local(&c, "movies").await, 0);
    assert_eq!(get(&base, "movies").await["revision"], 1);
    assert_eq!(
        request(&base, "PUT", "/api/v1/tv/import-policy", tv)
            .await
            .0,
        409
    );
    assert_eq!(get(&base, "tv").await, saved);
    let (left, right) = tokio::join!(
        request(&base, "PUT", "/api/v1/movies/import-policy", input(1)),
        request(&base, "PUT", "/api/v1/movies/import-policy", input(1))
    );
    let mut statuses = [left.0, right.0];
    statuses.sort();
    assert_eq!(statuses, [200, 409]);
    assert_eq!(get(&base, "movies").await["revision"], 2);
    // No-op saves still establish explicit local intent and advance the CAS token.
    assert_eq!(local(&c, "movies").await, 1);
    _server.stop().await;
}
#[tokio::test]
async fn import_policy_strict_http_bounds_and_canonical_empty() {
    let scratch = Scratch::new();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let (base, _server) = server(db).await;
    let path = "/api/v1/tv/import-policy";
    let initial = get(&base, "tv").await;
    for (field, value) in [
        ("minimum_free_space_mib", json!(99)),
        ("minimum_free_space_mib", json!(2147483648_i64)),
        ("minimum_free_space_mib", json!(100.5)),
        ("revision", json!(0)),
        ("revision", json!(9007199254740992_i64)),
        ("skip_free_space_check_when_importing", json!("false")),
        ("download_client_working_folders", json!("é".repeat(2049))),
        ("download_client_working_folders", json!(null)),
        ("unknown", json!(true)),
        ("media_type", json!("movies")),
    ] {
        let mut body = input(1);
        body[field] = value;
        assert_eq!(request(&base, "PUT", path, body).await.0, 400, "{field}");
        assert_eq!(get(&base, "tv").await, initial);
    }
    for method in ["GET", "PUT"] {
        assert_eq!(
            request(&base, method, "/api/v1/tv/import-policy?extra=1", input(1))
                .await
                .0,
            400
        );
    }
    assert_eq!(raw_request(&base, "PUT", path, "{".into()).await.0, 400);
    assert_eq!(
        raw_request(&base, "PUT", path, " ".repeat(32 * 1024 + 1))
            .await
            .0,
        400
    );
    let mut maximum = input(1);
    maximum["minimum_free_space_mib"] = json!(2147483647_i64);
    maximum["download_client_working_folders"] = json!("é".repeat(2048));
    let (status, value) = request(&base, "PUT", path, maximum).await;
    assert_eq!(status, 200);
    assert_eq!(
        value["download_client_working_folders"]
            .as_str()
            .unwrap()
            .len(),
        4096
    );
    // Legal maximum decoded text must remain accepted when escaped at six bytes per character.
    let escaped = format!(
        "{{\"revision\":2,\"skip_free_space_check_when_importing\":false,\"minimum_free_space_mib\":100,\"download_client_working_folders\":\"{}\"}}",
        "\\u0061".repeat(4096)
    );
    assert_eq!(raw_request(&base, "PUT", path, escaped).await.0, 200);
    let mut empty = input(3);
    empty["download_client_working_folders"] = json!("");
    let (status, value) = request(&base, "PUT", path, empty).await;
    assert_eq!(status, 200);
    assert_eq!(
        value["download_client_working_folders"],
        "_UNPACK_|_FAILED_"
    );
    // Preserve exact literal source intent, including whitespace and inert NUL.
    let mut literal = input(4);
    literal["download_client_working_folders"] = json!(" a\t|\n|\0 ");
    let (status, value) = request(&base, "PUT", path, literal).await;
    assert_eq!(status, 200);
    assert_eq!(value["download_client_working_folders"], " a\t|\n|\0 ");
    _server.stop().await;
}
#[tokio::test]
async fn import_policy_storage_failure_rolls_back_and_read_corruption_is_explicit() {
    let scratch = Scratch::new();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let c = db.connect().await.unwrap();
    let (base, _server) = server(db).await;
    let path = "/api/v1/movies/import-policy";
    c.execute_batch("CREATE TABLE policy_test_audit(value INTEGER);CREATE TRIGGER policy_test_fail AFTER UPDATE ON import_policies WHEN NEW.minimum_free_space_mib=777 BEGIN INSERT INTO policy_test_audit VALUES(1);SELECT RAISE(ABORT,'fixture rollback');END;").await.unwrap();
    let initial = get(&base, "movies").await;
    let mut fail = input(1);
    fail["minimum_free_space_mib"] = json!(777);
    let (status, error) = request(&base, "PUT", path, fail).await;
    assert_eq!(status, 503);
    assert_eq!(error["error"]["code"], "import_policy_storage_error");
    assert_eq!(get(&base, "movies").await, initial);
    assert_eq!(local(&c, "movies").await, 0);
    assert_eq!(
        c.query("SELECT count(*) FROM policy_test_audit", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<i64>(0)
            .unwrap(),
        0
    );
    c.execute_batch("DROP TRIGGER policy_test_fail;PRAGMA ignore_check_constraints=ON;UPDATE import_policies SET download_client_working_folders=printf('%5000s','x') WHERE media_type='movies';PRAGMA ignore_check_constraints=OFF;").await.unwrap();
    let (status, error) = request(&base, "GET", path, Value::Null).await;
    assert_eq!(status, 503);
    assert_eq!(error["error"]["code"], "import_policy_invalid");
    assert_eq!(get(&base, "tv").await["revision"], 1);
    _server.stop().await;
}
