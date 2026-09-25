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
use axum::{
    extract::{OriginalUri, State},
    response::{IntoResponse, Response},
};
use std::sync::Mutex;
const SECRET: &str = "PRESET_MOCK_API_KEY";
#[derive(Default)]
struct Remote {
    queries: Mutex<Vec<String>>,
}
async fn remote(State(state): State<Arc<Remote>>, OriginalUri(uri): OriginalUri) -> Response {
    let query = uri.query().unwrap_or("");
    state.queries.lock().unwrap().push(query.into());
    let args: std::collections::HashMap<_, _> = url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect();
    assert_eq!(args.get("apikey").unwrap(), SECRET);
    let body = if args.get("t").is_some_and(|t| t == "caps") {
        r#"<caps><limits max="100" default="10"/><searching><search available="yes" supportedParams="q"/><tv-search available="yes" supportedParams="q,season,ep"/><movie-search available="yes" supportedParams="q,imdbid"/></searching><categories><category id="5000" name="TV"><subcat id="5030" name="SD"/><subcat id="5040" name="HD"/><subcat id="5045" name="UHD"/><subcat id="5070" name="Anime"/></category><category id="2000" name="Movies"><subcat id="2010" name="Foreign"/><subcat id="2020" name="Other"/><subcat id="2030" name="SD"/><subcat id="2040" name="HD"/><subcat id="2045" name="UHD"/><subcat id="2050" name="BluRay"/><subcat id="2060" name="3D"/><subcat id="2070" name="DVD"/></category></categories></caps>"#
    } else {
        "<rss><channel/></rss>"
    };
    ([("content-type", "application/xml")], body).into_response()
}
async fn schema(address: SocketAddr, media: &str, kind: &str) -> Value {
    let (code, result) = request(
        address,
        "GET",
        &format!("/api/v1/providers/schema?media_type={media}&kind={kind}"),
        "",
    )
    .await;
    assert_eq!(code, 200);
    result
}
fn template<'a>(schema: &'a Value, implementation: &str) -> &'a Value {
    schema["templates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["implementation"] == implementation)
        .unwrap()
}
#[tokio::test]
async fn historical_presets_are_static_and_apply_only_the_requested_scope() -> Result<(), Error> {
    let scratch = Sandbox::new();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await?);
    let conn = db.connect().await?;
    let key = Arc::new(providers::CredentialKey::from_hex(&"12".repeat(32)).unwrap());
    let (address, server) = serve(db.clone(), Some(key)).await;
    let state = Arc::new(Remote::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/owned-api", listener.local_addr().unwrap());
    let app = axum::Router::new()
        .fallback(remote)
        .with_state(state.clone());
    let upstream = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let tv = schema(address, "tv", "indexer").await;
    let movies = schema(address, "movies", "indexer").await;
    let entries = [
        ("newznab-dognzb", "DOGnzb", "https://api.dognzb.cr/api"),
        (
            "newznab-drunkenslug",
            "DrunkenSlug",
            "https://drunkenslug.com/api",
        ),
        ("newznab-nzb-life", "Nzb.life", "https://api.nzb.life/api"),
        ("newznab-nzb-su", "Nzb.su", "https://api.nzb.su/api"),
        ("newznab-nzbcat", "NZBCat", "https://nzb.cat/api"),
        (
            "newznab-nzbfinder",
            "NZBFinder.ws",
            "https://nzbfinder.ws/api",
        ),
        ("newznab-nzbgeek", "NZBgeek", "https://api.nzbgeek.info/api"),
        (
            "newznab-nzbplanet",
            "nzbplanet.net",
            "https://api.nzbplanet.net/api",
        ),
        (
            "newznab-simplynzbs",
            "SimplyNZBs",
            "https://simplynzbs.com/api",
        ),
        (
            "newznab-tabula-rasa",
            "Tabula Rasa",
            "https://www.tabula-rasa.pw/api/v1/api",
        ),
        (
            "newznab-usenet-crawler",
            "Usenet Crawler",
            "https://www.usenet-crawler.com/api",
        ),
    ];
    for (media, catalog) in [("tv", &tv), ("movies", &movies)] {
        let presets = template(catalog, "newznab")["presets"].as_array().unwrap();
        assert_eq!(presets.len(), if media == "tv" { 9 } else { 10 });
        let expected: Vec<_> = entries
            .iter()
            .filter(|(key, _, _)| {
                if media == "tv" {
                    !matches!(*key, "newznab-nzb-su" | "newznab-usenet-crawler")
                } else {
                    *key != "newznab-nzb-life"
                }
            })
            .collect();
        for (preset, (key, name, url)) in presets.iter().zip(expected) {
            assert_eq!(preset["key"], *key);
            assert_eq!(preset["name"], *name);
            assert_eq!(preset["endpoint"], *url);
            assert_eq!(preset["enabled"], false);
            assert_eq!(preset["implementation"], "newznab");
            assert_eq!(preset["defaults"]["media_type"], media);
            assert!(preset["endpoint_hint"].is_null());
            for field in ["credentials", "id", "revision", "test_status", "last_test"] {
                assert!(preset.get(field).is_none());
            }
            let expected = if media == "tv" {
                if *key == "newznab-nzbfinder" {
                    json!([5030, 5040, 5045])
                } else {
                    json!([5030, 5040])
                }
            } else if *key == "newznab-nzbfinder" {
                json!([2030, 2040, 2045, 2050, 2060, 2070])
            } else {
                json!([2000, 2010, 2020, 2030, 2040, 2045, 2050, 2060])
            };
            assert_eq!(preset["defaults"]["settings"]["categories"], expected);
            if media == "tv" {
                assert_eq!(
                    preset["defaults"]["settings"]["anime_categories"],
                    json!([])
                );
                assert_eq!(
                    preset["defaults"]["settings"]["anime_standard_format_search"],
                    false
                );
            } else {
                assert_eq!(preset["defaults"]["settings"]["remove_year"], false);
            }
        }
        assert_eq!(
            template(catalog, "newznab")["supported_media"],
            json!(["tv", "movies"]),
            "Catalog listing differences do not limit provider capabilities"
        );
        assert_eq!(
            template(catalog, "newznab")["defaults"]["tv"]["categories"],
            json!([]),
            "Base schema still requires user selection; named preset categories are separate"
        );
        let clients = schema(address, media, "download_client").await;
        assert_eq!(template(&clients, "qbittorrent")["presets"], json!([]));
    }
    assert_eq!(template(&tv, "torznab")["presets"], json!([]));
    let jackett = &template(&movies, "torznab")["presets"][0];
    assert_eq!(
        template(&movies, "torznab")["presets"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(jackett["key"], "torznab-jackett");
    assert_eq!(jackett["name"], "Jackett");
    assert!(jackett["endpoint"].is_null());
    assert!(
        jackett["endpoint_hint"]
            .as_str()
            .unwrap()
            .contains("YOURINDEXER")
    );
    assert_eq!(
        jackett["defaults"]["settings"]["categories"],
        json!([2000, 2010, 2020, 2030, 2040, 2045, 2050, 2060])
    );
    assert!(state.queries.lock().unwrap().is_empty());
    for table in ["providers", "provider_tests", "provider_scopes"] {
        assert_eq!(
            conn.query(&format!("SELECT count(*) FROM {table}"), ())
                .await?
                .next()
                .await?
                .unwrap()
                .get::<i64>(0)?,
            0
        );
    }
    assert_eq!(
        schema(address, "tv", "indexer").await,
        tv,
        "Static catalog output is deterministic"
    );
    for (media, implementation, key, catalog) in [
        ("tv", "newznab", "newznab-nzbfinder", &tv),
        ("movies", "newznab", "newznab-nzbfinder", &movies),
        ("movies", "torznab", "torznab-jackett", &movies),
    ] {
        let preset = template(catalog, implementation)["presets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["key"] == key)
            .unwrap();
        let baseline = json!({"name":"Before preset","enabled":false,"priority":1,"settings":{"implementation":implementation,"endpoint":endpoint,"tv":{"categories":[5000],"anime_categories":[5070],"anime_standard_format_search":true},"movies":{"categories":[2000],"remove_year":true}},"credentials":{"kind":"api_key","api_key":SECRET}});
        let (code, saved) =
            request(address, "POST", "/api/v1/providers", &baseline.to_string()).await;
        assert_eq!(code, 201);
        let id = saved["id"].as_str().unwrap();
        let route = format!("/api/v1/providers/{id}");
        let cipher = conn
            .query("SELECT credentials FROM providers WHERE id=?", [id])
            .await?
            .next()
            .await?
            .unwrap()
            .get::<Vec<u8>>(0)?;
        // Replace historical endpoint metadata with an owned listener BEFORE any discovery, test or save request.
        let mut proposed = baseline.clone();
        proposed.as_object_mut().unwrap().remove("credentials");
        proposed["name"] = preset["name"].clone();
        proposed["enabled"] = preset["enabled"].clone();
        proposed["settings"]["endpoint"] = json!(endpoint);
        proposed["settings"][media] = preset["defaults"]["settings"].clone();
        assert!(
            proposed["settings"]["endpoint"]
                .as_str()
                .unwrap()
                .starts_with("http://127.0.0.1:")
        );
        let source = json!({"id":id,"revision":1});
        let discovery = json!({"media_type":media,"source":source,"connection":{"implementation":implementation,"endpoint":endpoint}});
        let (code, discovered) = request(
            address,
            "POST",
            "/api/v1/providers/indexer-categories",
            &discovery.to_string(),
        )
        .await;
        assert_eq!(code, 200);
        assert_eq!(discovered["origin"], "advertised");
        assert!(
            discovered["discovery_error"].is_null(),
            "A 200 fallback alone is not evidence the preset connection works"
        );
        tokio::time::sleep(Duration::from_millis(110)).await;
        let draft = json!({"source":source,"config":proposed});
        let (code, tested) = request(
            address,
            "POST",
            "/api/v1/providers/test-draft",
            &draft.to_string(),
        )
        .await;
        assert_eq!(code, 200, "{tested}");
        assert_eq!(tested["result"]["domains"], json!(["tv", "movies"]));
        assert_eq!(
            request(address, "GET", &route, "").await.1,
            saved,
            "Discovery and draft test must not apply the preset implicitly"
        );
        let count = state.queries.lock().unwrap().len();
        proposed["revision"] = json!(1);
        let (code, updated) = request(address, "PUT", &route, &proposed.to_string()).await;
        assert_eq!(code, 200);
        assert_eq!(updated["revision"], 2);
        assert_eq!(updated["enabled"], false);
        assert_eq!(updated["settings"][media], preset["defaults"]["settings"]);
        let other = if media == "tv" { "movies" } else { "tv" };
        assert_eq!(updated["settings"][other], saved["settings"][other]);
        assert_eq!(
            state.queries.lock().unwrap().len(),
            count,
            "Manual save has no hidden network test"
        );
        assert_eq!(
            conn.query("SELECT credentials FROM providers WHERE id=?", [id])
                .await?
                .next()
                .await?
                .unwrap()
                .get::<Vec<u8>>(0)?,
            cipher
        );
        assert_eq!(updated["test_status"], "never_tested");
    }
    server.abort();
    upstream.abort();
    let _ = server.await;
    let _ = upstream.await;
    Ok(())
}
