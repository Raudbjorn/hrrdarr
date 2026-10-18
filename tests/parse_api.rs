use hrrdarr::{db::Database, parse};
use serde_json::Value;
use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("hrrdarr-parse-api-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
async fn serve(db: Arc<Database>) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = parse::router(db);
    (
        address,
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        }),
    )
}
/// Percent-encodes a query value; the `reqwest` dependency here has no `query` feature
/// enabled (that's `Cargo.toml`, outside this module's ownership), so the query string is
/// built by hand, the same approach `tests/naming_api.rs` uses for its raw-TCP requests.
fn encode_query_value(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}
async fn get(client: &reqwest::Client, addr: SocketAddr, path: &str, title: &str) -> (u16, Value) {
    let response = client
        .get(format!(
            "http://{addr}{path}?title={}",
            encode_query_value(title)
        ))
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

#[tokio::test]
async fn tv_route_parses_matches_series_and_episode_over_real_http() {
    let sandbox = Sandbox::new();
    let db = Arc::new(Database::open_local(sandbox.0.join("db")).await.unwrap());
    let c = db.connect().await.unwrap();
    c.execute_batch("INSERT INTO series(id,title,path) VALUES(1,'Harbor','/tv/harbor');INSERT INTO seasons(series_id,number) VALUES(1,1);INSERT INTO episodes(id,series_id,season,number,title) VALUES(1,1,1,2,'Two');").await.unwrap();
    let (addr, server) = serve(db).await;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let (status, body) = get(
        &client,
        addr,
        "/api/v1/tv/parse",
        "Harbor.S01E02.1080p.WEB-DL",
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["title"], "Harbor.S01E02.1080p.WEB-DL");
    assert_eq!(body["parsed"]["title"], "Harbor");
    assert_eq!(body["series"]["id"], 1);
    assert_eq!(body["series"]["title"], "Harbor");
    assert_eq!(body["episodes"].as_array().unwrap().len(), 1);
    assert_eq!(body["episodes"][0]["id"], 1);
    assert_eq!(body["episodes"][0]["season"], 1);
    assert_eq!(body["episodes"][0]["number"], 2);
    assert_eq!(
        body["parsed"]["revision"],
        serde_json::json!({"version":1,"real":0,"is_repack":false})
    );
    assert_eq!(body["parsed"]["revision_marker"], false);
    let (status, body) = get(
        &client,
        addr,
        "/api/v1/tv/parse",
        "Harbor.S01E02v3.1080p.WEB-DL.REAL",
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["parsed"]["revision"],
        serde_json::json!({"version":3,"real":1,"is_repack":false})
    );
    assert_eq!(body["parsed"]["revision_marker"], true);
    assert_eq!(body["episodes"][0]["id"], 1);

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn tv_route_reports_no_match_and_parse_failure_without_erroring() {
    let sandbox = Sandbox::new();
    let db = Arc::new(Database::open_local(sandbox.0.join("db")).await.unwrap());
    let (addr, server) = serve(db).await;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let (status, body) = get(
        &client,
        addr,
        "/api/v1/tv/parse",
        "Harbor.S01E02.1080p.WEB-DL",
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert!(body["parsed"].is_object());
    assert!(body["series"].is_null());
    assert_eq!(body["episodes"].as_array().unwrap().len(), 0);
    let (status, body) = get(&client, addr, "/api/v1/tv/parse", "Harbor 1080p WEB-DL").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["title"], "Harbor 1080p WEB-DL");
    assert!(body["parsed"].is_null());
    assert!(body["series"].is_null());
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn movie_route_disambiguates_by_year_over_real_http() {
    let sandbox = Sandbox::new();
    let db = Arc::new(Database::open_local(sandbox.0.join("db")).await.unwrap());
    let c = db.connect().await.unwrap();
    c.execute_batch("INSERT INTO movie_metadata(id,title,year) VALUES(1,'Harbor',1982),(2,'Harbor',2011);INSERT INTO movies(id,metadata_id,path) VALUES(1,1,'/movies/harbor-1982'),(2,2,'/movies/harbor-2011');").await.unwrap();
    let (addr, server) = serve(db).await;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let (status, body) = get(
        &client,
        addr,
        "/api/v1/movies/parse",
        "Harbor.1982.1080p.Bluray",
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["movie"]["id"], 1);
    assert_eq!(body["movie"]["year"], 1982);
    let (status, body) = get(
        &client,
        addr,
        "/api/v1/movies/parse",
        "Harbor.1982.1080p.Bluray.RERIP2.REAL",
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["parsed"]["revision"],
        serde_json::json!({"version":3,"real":1,"is_repack":true})
    );
    assert_eq!(body["movie"]["id"], 1);

    let (status, body) = get(
        &client,
        addr,
        "/api/v1/movies/parse",
        "Harbor.2011.Extended.2160p.WEB-DL",
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["movie"]["id"], 2);
    // A parsed year matching neither candidate must stay unresolved, not an arbitrary pick.
    let (status, body) = get(
        &client,
        addr,
        "/api/v1/movies/parse",
        "Harbor.1999.1080p.Bluray",
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert!(body["movie"].is_null());
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn empty_title_is_a_clean_400_on_both_routes_over_real_http() {
    let sandbox = Sandbox::new();
    let db = Arc::new(Database::open_local(sandbox.0.join("db")).await.unwrap());
    let (addr, server) = serve(db).await;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    for path in ["/api/v1/tv/parse", "/api/v1/movies/parse"] {
        let (status, body) = get(&client, addr, path, "").await;
        assert_eq!(status, 400, "{path}: {body}");
        assert!(body["error"]["code"].is_string(), "{path}: {body}");
    }
    let response = client
        .get(format!("http://{addr}/api/v1/tv/parse"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 400, "missing title entirely");
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn hostile_oversized_title_parses_cleanly_to_no_match_over_real_http() {
    let sandbox = Sandbox::new();
    let db = Arc::new(Database::open_local(sandbox.0.join("db")).await.unwrap());
    let (addr, server) = serve(db).await;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let oversized = "x".repeat(2000);
    let (status, body) = get(&client, addr, "/api/v1/tv/parse", &oversized).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["title"], oversized);
    assert!(body["parsed"].is_null());
    server.abort();
    let _ = server.await;
}
