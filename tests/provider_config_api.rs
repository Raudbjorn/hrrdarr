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
            std::env::temp_dir().join(format!("hrrdarr-provider-api-{}", uuid::Uuid::new_v4()));
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
async fn serve(
    db: Arc<Database>,
    key: Option<Arc<providers::CredentialKey>>,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = providers::router(db, key);
    (
        address,
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        }),
    )
}
#[tokio::test]
async fn provider_config_http_secrets_scopes_revisions_and_reopen() -> Result<(), Error> {
    const SECRET: &str = "PRIVATE_PROVIDER_SENTINEL_9327";
    let scratch = Sandbox::new();
    let path = scratch.0.join("db");
    let db = Arc::new(Database::open_local(&path).await?);
    let c = db.connect().await?;
    let key = Arc::new(providers::CredentialKey::from_hex(&"12".repeat(32)).unwrap());
    let (address, server) = serve(db.clone(), Some(key.clone())).await;
    let route = "/api/v1/providers";
    let base = json!({"name":"Shared indexer","enabled":true,"priority":10,"settings":{"implementation":"torznab","endpoint":"http://127.0.0.1:1/api","tv":{"categories":[5030],"anime_categories":[5070]},"movies":{"categories":[2000]}},"credentials":{"kind":"api_key","api_key":SECRET}});
    let (status, created) = request(address, "POST", route, &base.to_string()).await;
    assert_eq!(status, 201, "{created}");
    assert!(!created.to_string().contains(SECRET));
    assert_eq!(created["has_credentials"], true);
    assert_eq!(created["test_supported"], false);
    assert_eq!(created["test_status"], "never_tested");
    let id = created["id"].as_str().unwrap();
    let detail = format!("{route}/{id}");
    assert_eq!(created["revision"], 1);
    for media in ["tv", "movies"] {
        let (status, page) = request(
            address,
            "GET",
            &format!("{route}?media_type={media}&limit=1"),
            "",
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(page["total"], 1);
        assert_eq!(page["items"][0]["id"], id);
        assert!(!page.to_string().contains(SECRET));
    }
    let ciphertext = c
        .query("SELECT credentials FROM providers WHERE id=?1", [id])
        .await?
        .next()
        .await?
        .unwrap()
        .get::<Vec<u8>>(0)?;
    assert!(
        !ciphertext
            .windows(SECRET.len())
            .any(|v| v == SECRET.as_bytes())
    );
    let mut update = base.clone();
    update.as_object_mut().unwrap().remove("credentials");
    update["revision"] = json!(1);
    update["name"] = json!("Renamed");
    let (status, changed) = request(address, "PUT", &detail, &update.to_string()).await;
    assert_eq!(status, 200, "{changed}");
    assert_eq!(changed["revision"], 2);
    assert_eq!(changed["has_credentials"], true);
    assert_eq!(
        c.query("SELECT credentials FROM providers WHERE id=?1", [id])
            .await?
            .next()
            .await?
            .unwrap()
            .get::<Vec<u8>>(0)?,
        ciphertext
    );
    assert_eq!(
        request(address, "PUT", &detail, &update.to_string())
            .await
            .0,
        409
    );
    // Failure while inserting the second domain scope must roll back config, secret and revision.
    c.execute_batch("CREATE TRIGGER fail_provider_scope BEFORE INSERT ON provider_scopes WHEN NEW.media_type='movies' BEGIN SELECT RAISE(ABORT,'synthetic scope failure'); END;").await?;
    update["revision"] = json!(2);
    update["name"] = json!("Must rollback");
    update["credentials"] = json!({"kind":"api_key","api_key":"REPLACEMENT_PRIVATE_SENTINEL"});
    let (status, error) = request(address, "PUT", &detail, &update.to_string()).await;
    assert_eq!(status, 500, "{error}");
    assert!(!error.to_string().contains("REPLACEMENT_PRIVATE_SENTINEL"));
    c.execute_batch("DROP TRIGGER fail_provider_scope").await?;
    let (_, kept) = request(address, "GET", &detail, "").await;
    assert_eq!(kept["name"], "Renamed");
    assert_eq!(kept["revision"], 2);
    assert_eq!(kept["settings"]["movies"]["categories"], json!([2000]));
    assert_eq!(
        c.query("SELECT credentials FROM providers WHERE id=?1", [id])
            .await?
            .next()
            .await?
            .unwrap()
            .get::<Vec<u8>>(0)?,
        ciphertext
    );
    for key in [
        None,
        Some(Arc::new(
            providers::CredentialKey::from_hex(&"34".repeat(32)).unwrap(),
        )),
    ] {
        let (locked, task) = serve(db.clone(), key).await;
        assert_eq!(request(locked, "GET", &detail, "").await.0, 200);
        let mut omitted = update.clone();
        omitted.as_object_mut().unwrap().remove("credentials");
        assert_eq!(
            request(locked, "PUT", &detail, &omitted.to_string())
                .await
                .0,
            503
        );
        for credentials in [
            Value::Null,
            json!({"kind":"api_key","api_key":"replacement"}),
        ] {
            let mut attempt = update.clone();
            attempt["credentials"] = credentials;
            let (status, error) = request(locked, "PUT", &detail, &attempt.to_string()).await;
            assert_eq!(status, 503, "{error}");
            assert!(!error.to_string().contains(SECRET));
        }
        assert_eq!(
            request(locked, "DELETE", &format!("{detail}?revision=2"), "")
                .await
                .0,
            503
        );
        task.abort();
        let _ = task.await;
    }
    let (status, error) = request(address, "POST", &format!("{detail}/test"), "").await;
    assert_eq!(status, 501);
    assert_eq!(error["error"]["code"], "provider_test_not_implemented");
    let mut clear = update.clone();
    clear["credentials"] = Value::Null;
    clear["name"] = json!("Cleared");
    let (status, cleared) = request(address, "PUT", &detail, &clear.to_string()).await;
    assert_eq!(status, 200, "{cleared}");
    assert_eq!(cleared["has_credentials"], false);
    assert_eq!(cleared["revision"], 3);
    let scope = |category: &str| json!({"category":category,"imported_category":null,"recent_priority":1,"older_priority":0});
    let client = json!({"name":"Shared client","enabled":false,"priority":20,"settings":{"implementation":"qbittorrent","endpoint":"http://127.0.0.1:1","tv":scope("tv"),"movies":scope("movies")},"credentials":{"kind":"username_password","username":"PRIVATE_USER_SENTINEL","password":SECRET}});
    let (status, download) = request(address, "POST", route, &client.to_string()).await;
    assert_eq!(status, 201, "{download}");
    assert!(!download.to_string().contains(SECRET));
    assert!(!download.to_string().contains("PRIVATE_USER_SENTINEL"));
    assert_eq!(download["settings"]["tv"]["category"], "tv");
    assert_eq!(download["settings"]["movies"]["category"], "movies");
    let mut collision = client.clone();
    collision["settings"]["movies"] = scope("tv");
    assert_eq!(
        request(address, "POST", route, &collision.to_string())
            .await
            .0,
        400
    );
    let mut invalid_priority = client.clone();
    invalid_priority["settings"]["tv"]["recent_priority"] = json!(2);
    assert_eq!(
        request(address, "POST", route, &invalid_priority.to_string())
            .await
            .0,
        400
    );
    let mut duplicate_categories = base.clone();
    duplicate_categories["settings"]["tv"]["categories"] = json!([5030, 5030]);
    assert_eq!(
        request(address, "POST", route, &duplicate_categories.to_string())
            .await
            .0,
        400
    );
    let mut no_scopes = base.clone();
    no_scopes["settings"]["tv"] = Value::Null;
    no_scopes["settings"]["movies"] = Value::Null;
    assert_eq!(
        request(address, "POST", route, &no_scopes.to_string())
            .await
            .0,
        400
    );
    for endpoint in [
        format!("http://user:{SECRET}@localhost/api"),
        format!("http://localhost/api?apikey={SECRET}"),
        "file:///tmp/provider".into(),
    ] {
        let mut bad = base.clone();
        bad["settings"]["endpoint"] = json!(endpoint);
        let (status, error) = request(address, "POST", route, &bad.to_string()).await;
        assert_eq!(status, 400, "{error}");
        assert!(!error.to_string().contains(SECRET));
    }
    let (status, error) = request(
        address,
        "POST",
        route,
        &format!("{{\"credentials\":\"{SECRET}",),
    )
    .await;
    assert_eq!(status, 400);
    assert!(!error.to_string().contains(SECRET));
    for query in [
        "limit=0",
        "limit=101",
        "offset=-1",
        "media_type=episode",
        "unknown=1",
    ] {
        assert_eq!(
            request(address, "GET", &format!("{route}?{query}"), "")
                .await
                .0,
            400
        );
    }
    assert_eq!(
        request(address, "DELETE", &format!("{detail}?revision=2"), "")
            .await
            .0,
        409
    );
    // Every registry implementation has a real HTTP persisted roundtrip; scopes stay distinct.
    let mut newznab_ids = Vec::new();
    for media in ["tv", "movies"] {
        let settings = if media == "tv" {
            json!({"implementation":"newznab","endpoint":"http://127.0.0.1:1/api","tv":{"categories":[],"anime_categories":[5070]},"movies":null})
        } else {
            json!({"implementation":"newznab","endpoint":"http://127.0.0.1:1/api","tv":null,"movies":{"categories":[2000]}})
        };
        let body = json!({"name":format!("Newznab {media}"),"enabled":false,"priority":30,"settings":settings});
        let (status, created) = request(address, "POST", route, &body.to_string()).await;
        assert_eq!(status, 201, "{created}");
        let nid = created["id"].as_str().unwrap().to_owned();
        let (status, read) = request(address, "GET", &format!("{route}/{nid}"), "").await;
        assert_eq!(status, 200);
        assert_eq!(read["settings"], settings);
        assert_eq!(read["has_credentials"], false);
        newznab_ids.push(nid);
    }
    let mut api_key_client = client.clone();
    api_key_client["revision"] = json!(1);
    api_key_client["credentials"] = json!({"kind":"api_key","api_key":SECRET});
    let download_path = format!("{route}/{}", download["id"].as_str().unwrap());
    let (status, updated_client) =
        request(address, "PUT", &download_path, &api_key_client.to_string()).await;
    assert_eq!(status, 200, "{updated_client}");
    assert_eq!(updated_client["has_credentials"], true);
    assert!(!updated_client.to_string().contains(SECRET));
    // Removing one provider cascades only its scopes and preserves the other providers verbatim.
    let (status, body) = request(address, "DELETE", &format!("{detail}?revision=3"), "").await;
    assert_eq!(status, 204, "{body}");
    assert_eq!(request(address, "GET", &detail, "").await.0, 404);
    assert_eq!(
        c.query(
            "SELECT count(*) FROM provider_scopes WHERE provider_id=?1",
            [id]
        )
        .await?
        .next()
        .await?
        .unwrap()
        .get::<i64>(0)?,
        0
    );
    assert_eq!(
        request(address, "GET", &download_path, "").await.1,
        updated_client
    );
    server.abort();
    let _ = server.await;
    drop(c);
    drop(db);
    let reopened = Arc::new(Database::open_local(&path).await?);
    let (address, server) = serve(reopened.clone(), Some(key)).await;
    let (status, stored) = request(address, "GET", &detail, "").await;
    assert_eq!(status, 404, "{stored}");
    for nid in newznab_ids {
        assert_eq!(
            request(address, "GET", &format!("{route}/{nid}"), "")
                .await
                .0,
            200
        );
    }
    let (_, page) = request(address, "GET", route, "").await;
    assert_eq!(page["total"], 3);
    server.abort();
    let _ = server.await;
    Ok(())
}
