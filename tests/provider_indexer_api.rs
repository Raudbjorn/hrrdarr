use hrrdarr::{
    db::{Database, Error},
    providers,
};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("hrrdarr-indexer-api-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
async fn request(addr: SocketAddr, method: &str, path: &str, body: &str) -> (u16, Value) {
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    tokio::task::spawn_blocking(move || {
        let mut stream =
            std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(5)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut bytes = Vec::new();
        stream
            .take(9 * 1024 * 1024)
            .read_to_end(&mut bytes)
            .unwrap();
        let response = String::from_utf8(bytes).unwrap();
        let (headers, body) = response.split_once("\r\n\r\n").unwrap();
        let status: u16 = headers.split_whitespace().nth(1).unwrap().parse().unwrap();
        (
            status,
            if status == 204 {
                assert!(body.is_empty());
                Value::Null
            } else {
                serde_json::from_str(body).expect("provider response must be JSON")
            },
        )
    })
    .await
    .unwrap()
}
use axum::{
    extract::{OriginalUri, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use std::sync::{
    Mutex,
    atomic::{AtomicU8, Ordering},
};
const SECRET: &str = "INDEXER_PRIVATE_SENTINEL";
const CAPS: &str = r#"<caps><limits max="100" default="10"/><searching><search available="yes" supportedParams="q"/><tv-search available="yes" supportedParams="q,tvdbid,season,ep"/><movie-search available="yes" supportedParams="q,imdbid,tmdbid"/></searching><categories><category id="5000"><subcat id="5030"/></category><category id="2000"/></categories></caps>"#;
#[derive(Default)]
struct Upstream {
    mode: AtomicU8,
    queries: Mutex<Vec<String>>,
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
async fn fixture(State(state): State<Arc<Upstream>>, OriginalUri(uri): OriginalUri) -> Response {
    let query = uri.query().unwrap_or("").to_owned();
    state.queries.lock().unwrap().push(query.clone());
    let values: std::collections::HashMap<_, _> = url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect();
    let caps = values.get("t").is_some_and(|s| s == "caps");
    let mode = state.mode.load(Ordering::SeqCst);
    if mode == 2 {
        return (
            StatusCode::FOUND,
            [("location", format!("/unexpected?apikey={SECRET}"))],
            "",
        )
            .into_response();
    }
    if mode == 3 && caps {
        state.started.notify_one();
        state.release.notified().await;
    }
    if caps {
        return ([("content-type", "application/xml")], CAPS).into_response();
    }
    if mode == 1 {
        return (
            [("content-type", "application/xml")],
            format!("<error code=\"100\" description=\"{SECRET}\"/>"),
        )
            .into_response();
    }
    if mode == 4 && (values.contains_key("tmdbid") || values.contains_key("imdbid")) {
        return ([("content-type", "application/xml")], r#"<rss xmlns:x="http://www.newznab.com/DTD/2010/feeds/attributes/"><channel><x:response offset="0" total="0"/></channel></rss>"#).into_response();
    }
    let offset = values.get("offset").map(String::as_str).unwrap_or("0");
    let category = values
        .get("cat")
        .and_then(|s| s.split(',').next())
        .unwrap_or("5030");
    let namespace = if uri.path() == "/torznab" {
        "http://torznab.com/schemas/2015/feed"
    } else {
        "http://www.newznab.com/DTD/2010/feeds/attributes/"
    };
    let mime = if uri.path() == "/torznab" {
        "application/x-bittorrent"
    } else {
        "application/x-nzb"
    };
    let total = if mode == 4 { "1" } else { "20" };
    if mode == 5 {
        let xml = format!(
            r#"<rss xmlns:x="{namespace}"><channel><x:response offset="{offset}" total="10"/><item><pubDate>Tue, 01 Sep 2026 00:00:00 +0000</pubDate><enclosure url="http://localhost/download?apikey={SECRET}" type="{mime}"/><comments>http://localhost/info?token={SECRET}</comments><x:attr name="info" value="http://localhost/private?token={SECRET}"/><x:attr name="language" value="{SECRET}"/><x:attr name="unknown_private" value="{SECRET}"/></item><item><title>Invalid {SECRET}</title><guid>{SECRET}</guid><pubDate>Tue, 01 Sep 2026 00:00:00 +0000</pubDate><enclosure url="file:///{SECRET}" type="{mime}"/></item></channel></rss>"#
        );
        return ([("content-type", "application/xml")], xml).into_response();
    }
    let xml = format!(
        r#"<rss xmlns:x="{namespace}"><channel><x:response offset="{offset}" total="{total}"/><item><title>Example {SECRET}</title><guid>private-{SECRET}</guid><pubDate>Tue, 01 Sep 2026 00:00:00 +0000</pubDate><enclosure url="http://localhost/download?apikey={SECRET}" length="1234" type="{mime}"/><x:attr name="category" value="{category}"/><x:attr name="seeders" value="3"/><x:attr name="language" value="{SECRET}"/><x:attr name="unknown_private" value="{SECRET}"/></item></channel></rss>"#
    );
    ([("content-type", "application/xml")], xml).into_response()
}
#[tokio::test]
async fn indexer_api_transport_status_staleness_and_redaction() -> Result<(), Error> {
    let scratch = Sandbox::new();
    let db_path = scratch.0.join("db");
    let db = Arc::new(Database::open_local(&db_path).await?);
    let key = Arc::new(providers::CredentialKey::from_hex(&"42".repeat(32)).unwrap());
    let upstream = Arc::new(Upstream::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let upstream_address = listener.local_addr()?;
    let app = axum::Router::new()
        .route("/torznab", axum::routing::get(fixture))
        .route("/newznab", axum::routing::get(fixture))
        .route("/unexpected", axum::routing::get(fixture))
        .with_state(upstream.clone());
    let upstream_task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let app = providers::router(db.clone(), Some(key.clone()));
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut providers_created = Vec::new();
    for implementation in ["torznab", "newznab"] {
        let config = json!({"name":implementation,"enabled":true,"priority":1,"settings":{"implementation":implementation,"endpoint":format!("http://{upstream_address}/{implementation}"),"tv":{"categories":[5030],"anime_categories":[]},"movies":{"categories":[2000]}},"credentials":{"kind":"api_key","api_key":SECRET}});
        let (status, provider) =
            request(address, "POST", "/api/v1/providers", &config.to_string()).await;
        assert_eq!(status, 201, "{provider}");
        let id = provider["id"].as_str().unwrap().to_owned();
        let path = format!("/api/v1/providers/{id}");
        let (status, tested) = request(address, "POST", &format!("{path}/test"), "").await;
        assert_eq!(status, 200, "{tested}");
        assert_eq!(tested["result"]["domains"], json!(["tv", "movies"]));
        assert!(!tested.to_string().contains(SECRET));
        let (_, read) = request(address, "GET", &path, "").await;
        assert_eq!(read["test_status"], "success");
        assert_eq!(read["last_test"]["revision"], 1);
        for (query, media, category) in [
            (
                json!({"kind":"tv","title":"Show & title","tvdb_id":12,"numbering":{"kind":"episode","season":1,"episode":2},"offset":0,"limit":5}),
                "tv",
                "5030",
            ),
            (
                json!({"kind":"movie","title":"Movie","tmdb_id":34,"imdb_id":"tt1234567","offset":5,"limit":5}),
                "movies",
                "2000",
            ),
        ] {
            let (status, result) = request(
                address,
                "POST",
                &format!("{path}/search"),
                &query.to_string(),
            )
            .await;
            assert_eq!(status, 200, "{result}");
            assert_eq!(result["page"]["media_type"], media);
            assert_eq!(result["page"]["items"][0]["size_bytes"], 1234);
            assert!(!result.to_string().contains(SECRET));
            let item = &result["page"]["items"][0];
            assert!(item.get("download_url").is_none());
            assert!(item.get("guid").is_none());
            assert!(item.get("attributes").is_none());
            let last = upstream.queries.lock().unwrap().last().unwrap().clone();
            let params: std::collections::HashMap<_, _> =
                url::form_urlencoded::parse(last.as_bytes())
                    .into_owned()
                    .collect();
            assert_eq!(params.get("apikey").unwrap(), SECRET);
            assert_eq!(params.get("cat").unwrap(), category);
            assert_eq!(
                params.get("t").unwrap(),
                if media == "tv" { "tvsearch" } else { "movie" }
            );
            if media == "tv" {
                assert_eq!(params.get("tvdbid").unwrap(), "12");
                assert_eq!(params.get("season").unwrap(), "1");
                assert_eq!(params.get("ep").unwrap(), "2");
            } else {
                // Explicit supportedParams advertises aggregate ID queries: send both supported IDs.
                assert_eq!(params.get("imdbid").unwrap(), "1234567");
                assert_eq!(params.get("tmdbid").unwrap(), "34");
                assert_eq!(params.get("offset").unwrap(), "5");
            }
        }
        providers_created.push((id, config));
    }
    let (id, config) = &providers_created[0];
    let path = format!("/api/v1/providers/{id}");
    upstream.mode.store(5, Ordering::SeqCst);
    for (provider_id, _) in &providers_created {
        for media in ["tv", "movies"] {
            let query = json!({"kind":"rss","media_type":media,"offset":4,"limit":2});
            let (status, result) = request(
                address,
                "POST",
                &format!("/api/v1/providers/{provider_id}/search"),
                &query.to_string(),
            )
            .await;
            assert_eq!(status, 200, "{result}");
            let page = &result["page"];
            assert_eq!(page["media_type"], media);
            assert_eq!(page["items"].as_array().unwrap().len(), 1);
            let item = &page["items"][0];
            assert_eq!(item.get("title"), Some(&Value::Null));
            assert_eq!(item.get("size_bytes"), Some(&Value::Null));
            assert_eq!(item["categories"], json!([]));
            // Remote offsets count all received items, including the rejected second item.
            assert_eq!(page["offset"], 4);
            assert_eq!(page["total"], 10);
            assert_eq!(page["next_offset"], 6);
            assert_eq!(page["next_query"], json!({"query_index":0,"offset":6}));
            assert_eq!(page["warnings"], json!([{"index":5,"code":"invalid_item"}]));
            assert!(!result.to_string().contains(SECRET));
            for private_field in [
                "guid",
                "download_url",
                "attributes",
                "facts",
                "comments_url",
                "info_url",
            ] {
                assert!(item.get(private_field).is_none(), "{private_field}: {item}");
            }
        }
    }
    // With no ID results, movie provider fallback uses generic title/year search, not movie&q.
    // The explicit cursor then advances to the next caller-supplied alias without replaying IDs.
    upstream.mode.store(4, Ordering::SeqCst);
    let fallback = json!({"kind":"movie","title":"Original Movie","aliases":["Alternate Movie"],"year":2020,"tmdb_id":34,"imdb_id":"tt1234567","limit":5});
    let before = upstream.queries.lock().unwrap().len();
    let (status, result) = request(
        address,
        "POST",
        &format!("{path}/search"),
        &fallback.to_string(),
    )
    .await;
    assert_eq!(status, 200, "{result}");
    assert_eq!(result["page"]["query_index"], 1);
    assert_eq!(result["page"]["query_count"], 3);
    assert_eq!(
        result["page"]["next_query"],
        json!({"query_index":2,"offset":0})
    );
    let recorded = upstream.queries.lock().unwrap()[before..].to_vec();
    assert_eq!(
        recorded.len(),
        3,
        "caps, empty ID query, then title fallback"
    );
    let parsed: std::collections::HashMap<_, _> =
        url::form_urlencoded::parse(recorded[2].as_bytes())
            .into_owned()
            .collect();
    assert_eq!(parsed.get("t").unwrap(), "search");
    assert_eq!(parsed.get("q").unwrap(), "Original Movie 2020");
    let mut continuation = fallback.clone();
    continuation["query_index"] = json!(2);
    continuation["offset"] = json!(0);
    let (status, result) = request(
        address,
        "POST",
        &format!("{path}/search"),
        &continuation.to_string(),
    )
    .await;
    assert_eq!(status, 200, "{result}");
    assert_eq!(result["page"]["query_index"], 2);
    assert!(result["page"]["next_query"].is_null());
    let last = upstream.queries.lock().unwrap().last().unwrap().clone();
    let parsed: std::collections::HashMap<_, _> = url::form_urlencoded::parse(last.as_bytes())
        .into_owned()
        .collect();
    assert_eq!(parsed.get("q").unwrap(), "Alternate Movie 2020");
    assert_eq!(parsed.get("t").unwrap(), "search");
    // Public caps can succeed while authenticated RSS fails: this must persist failure.
    upstream.mode.store(1, Ordering::SeqCst);
    let (status, error) = request(address, "POST", &format!("{path}/test"), "").await;
    assert_eq!(status, 502, "{error}");
    assert_eq!(error["error"]["code"], "authentication");
    assert!(!error.to_string().contains(SECRET));
    let (_, read) = request(address, "GET", &path, "").await;
    assert_eq!(read["test_status"], "failure");
    assert_eq!(read["last_test"]["error_code"], "authentication");
    upstream.mode.store(2, Ordering::SeqCst);
    let before = upstream.queries.lock().unwrap().len();
    let (status, error) = request(address, "POST", &format!("{path}/test"), "").await;
    assert_eq!(status, 502);
    assert_eq!(error["error"]["code"], "redirect_rejected");
    assert!(!error.to_string().contains(SECRET));
    assert_eq!(upstream.queries.lock().unwrap().len(), before + 1);
    upstream.mode.store(3, Ordering::SeqCst);
    let route = format!("{path}/test");
    let in_flight = tokio::spawn(async move { request(address, "POST", &route, "").await });
    tokio::time::timeout(Duration::from_secs(3), upstream.started.notified())
        .await
        .unwrap();
    let mut update = config.clone();
    update.as_object_mut().unwrap().remove("credentials");
    update["revision"] = json!(1);
    update["name"] = json!("Changed while testing");
    let (status, updated) = request(address, "PUT", &path, &update.to_string()).await;
    assert_eq!(status, 200, "{updated}");
    assert_eq!(updated["test_status"], "never_tested");
    assert!(updated["last_test"].is_null());
    upstream.mode.store(0, Ordering::SeqCst);
    upstream.release.notify_one();
    let (status, error) = in_flight.await.unwrap();
    assert_eq!(status, 409, "{error}");
    let (_, read) = request(address, "GET", &path, "").await;
    assert_eq!(read["revision"], 2);
    assert_eq!(read["test_status"], "never_tested");
    server.abort();
    let _ = server.await;
    drop(db);
    let reopened = Arc::new(Database::open_local(&db_path).await?);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let app = providers::router(reopened, Some(key));
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let (_, read) = request(
        address,
        "GET",
        &format!("/api/v1/providers/{}", providers_created[1].0),
        "",
    )
    .await;
    assert_eq!(read["test_status"], "success");
    assert_eq!(read["last_test"]["revision"], 1);
    let (_, read) = request(address, "GET", &path, "").await;
    assert_eq!(read["test_status"], "never_tested");
    assert!(read["last_test"].is_null());
    server.abort();
    let _ = server.await;
    upstream_task.abort();
    let _ = upstream_task.await;
    Ok(())
}
