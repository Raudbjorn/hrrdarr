use hrrdarr::{db::Database, naming};
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
            std::env::temp_dir().join(format!("hrrdarr-naming-api-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
async fn open_db() -> (Sandbox, Arc<Database>) {
    let scratch = Sandbox::new();
    let path = scratch.0.join("db");
    let db = Arc::new(Database::open_local(&path).await.unwrap());
    (scratch, db)
}
async fn serve(db: Arc<Database>) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = naming::router(db);
    (
        address,
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        }),
    )
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
                serde_json::from_str(body).expect("naming response must be JSON")
            },
        )
    })
    .await
    .unwrap()
}
/// Percent-encodes a query value so token syntax like `{Series Title}` survives
/// as one path segment on the raw HTTP request line this harness writes by hand.
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

const TV_ROUTE: &str = "/api/v1/tv/config/naming";
const MOVIES_ROUTE: &str = "/api/v1/movies/config/naming";

fn fresh_tv_body(revision: i64) -> Value {
    json!({
        "revision": revision,
        "rename_enabled": false,
        "replace_illegal_characters": true,
        "colon_replacement": "smart",
        "custom_colon_replacement": null,
        "standard_episode_format": null,
        "daily_episode_format": null,
        "anime_episode_format": null,
        "series_folder_format": null,
        "season_folder_format": null,
        "specials_folder_format": null,
        "multi_episode_style": null
    })
}
fn fresh_movie_body(revision: i64) -> Value {
    json!({
        "revision": revision,
        "rename_enabled": false,
        "replace_illegal_characters": true,
        "colon_replacement": "smart",
        "custom_colon_replacement": null,
        "standard_movie_format": null,
        "movie_folder_format": null
    })
}

#[tokio::test]
async fn fresh_install_returns_seeded_defaults_for_both_domains() {
    let (_scratch, db) = open_db().await;
    let (address, server) = serve(db).await;
    let (status, tv) = request(address, "GET", TV_ROUTE, "").await;
    assert_eq!(status, 200, "{tv}");
    assert_eq!(tv, fresh_tv_body(1));
    let (status, movies) = request(address, "GET", MOVIES_ROUTE, "").await;
    assert_eq!(status, 200, "{movies}");
    assert_eq!(movies, fresh_movie_body(1));
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn tv_put_round_trip_updates_and_bumps_revision() {
    let (_scratch, db) = open_db().await;
    let (address, server) = serve(db).await;
    let mut body = fresh_tv_body(1);
    body["rename_enabled"] = json!(true);
    body["standard_episode_format"] = json!("{Series Title} - S{season:00}E{episode:00}");
    let (status, updated) = request(address, "PUT", TV_ROUTE, &body.to_string()).await;
    assert_eq!(status, 200, "{updated}");
    assert_eq!(updated["revision"], 2);
    assert_eq!(updated["rename_enabled"], true);
    assert_eq!(
        updated["standard_episode_format"],
        "{Series Title} - S{season:00}E{episode:00}"
    );
    let (status, fetched) = request(address, "GET", TV_ROUTE, "").await;
    assert_eq!(status, 200);
    assert_eq!(fetched, updated);
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn tv_put_stale_revision_conflicts_and_leaves_state_unchanged() {
    let (_scratch, db) = open_db().await;
    let (address, server) = serve(db).await;
    let mut first = fresh_tv_body(1);
    first["rename_enabled"] = json!(true);
    let (status, _) = request(address, "PUT", TV_ROUTE, &first.to_string()).await;
    assert_eq!(status, 200);
    let mut stale = fresh_tv_body(1);
    stale["standard_episode_format"] = json!("{Series Title}");
    let (status, error) = request(address, "PUT", TV_ROUTE, &stale.to_string()).await;
    assert_eq!(status, 409, "{error}");
    assert_eq!(error["error"]["code"], "naming_revision_conflict");
    let (_, fetched) = request(address, "GET", TV_ROUTE, "").await;
    assert_eq!(fetched["revision"], 2);
    assert_eq!(fetched["rename_enabled"], true);
    assert_eq!(fetched["standard_episode_format"], Value::Null);
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn tv_put_omitting_nullable_field_is_rejected() {
    let (_scratch, db) = open_db().await;
    let (address, server) = serve(db).await;
    let base = fresh_tv_body(1);
    for key in [
        "standard_episode_format",
        "daily_episode_format",
        "anime_episode_format",
        "series_folder_format",
        "season_folder_format",
        "specials_folder_format",
        "multi_episode_style",
        "custom_colon_replacement",
    ] {
        let mut missing = base.clone();
        missing.as_object_mut().unwrap().remove(key);
        let (status, error) = request(address, "PUT", TV_ROUTE, &missing.to_string()).await;
        assert_eq!(status, 400, "omitting {key} should be a hard 400: {error}");
        assert_eq!(error["error"]["code"], "invalid_naming_config");
    }
    let (_, fetched) = request(address, "GET", TV_ROUTE, "").await;
    assert_eq!(
        fetched["revision"], 1,
        "no rejected attempt may have written"
    );
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn tv_put_rejects_unknown_token_naming_it() {
    let (_scratch, db) = open_db().await;
    let (address, server) = serve(db).await;
    let mut body = fresh_tv_body(1);
    body["standard_episode_format"] = json!("{Not A Real Token}");
    let (status, error) = request(address, "PUT", TV_ROUTE, &body.to_string()).await;
    assert_eq!(status, 400, "{error}");
    assert_eq!(error["error"]["code"], "unknown_naming_token");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("{Not A Real Token}")
    );
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn tv_put_rejects_movie_token_as_cross_domain() {
    let (_scratch, db) = open_db().await;
    let (address, server) = serve(db).await;
    let mut body = fresh_tv_body(1);
    body["standard_episode_format"] = json!("{Movie Title}");
    let (status, error) = request(address, "PUT", TV_ROUTE, &body.to_string()).await;
    assert_eq!(status, 400, "{error}");
    assert_eq!(error["error"]["code"], "cross_domain_naming_token");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("{Movie Title}")
    );
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn tv_put_rejects_token_not_allowed_in_field() {
    let (_scratch, db) = open_db().await;
    let (address, server) = serve(db).await;
    let mut body = fresh_tv_body(1);
    body["series_folder_format"] = json!("{Series Title} [{Quality Title}]");
    let (status, error) = request(address, "PUT", TV_ROUTE, &body.to_string()).await;
    assert_eq!(status, 400, "{error}");
    assert_eq!(error["error"]["code"], "naming_token_not_allowed_in_field");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("{Quality Title}")
    );
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn put_rejects_inconsistent_colon_replacement_and_custom_text() {
    let (_scratch, db) = open_db().await;
    let (address, server) = serve(db).await;
    let mut text_without_custom_mode = fresh_tv_body(1);
    text_without_custom_mode["colon_replacement"] = json!("dash");
    text_without_custom_mode["custom_colon_replacement"] = json!("~");
    let (status, error) = request(
        address,
        "PUT",
        TV_ROUTE,
        &text_without_custom_mode.to_string(),
    )
    .await;
    assert_eq!(status, 400, "{error}");
    assert_eq!(error["error"]["code"], "invalid_naming_config");

    let mut custom_mode_without_text = fresh_tv_body(1);
    custom_mode_without_text["colon_replacement"] = json!("custom");
    let (status, error) = request(
        address,
        "PUT",
        TV_ROUTE,
        &custom_mode_without_text.to_string(),
    )
    .await;
    assert_eq!(status, 400, "{error}");
    assert_eq!(error["error"]["code"], "invalid_naming_config");

    let (_, fetched) = request(address, "GET", TV_ROUTE, "").await;
    assert_eq!(
        fetched["revision"], 1,
        "no rejected attempt may have written"
    );
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn examples_render_configured_formats_against_fixed_facts() {
    let (_scratch, db) = open_db().await;
    let (address, server) = serve(db).await;

    let mut tv_body = fresh_tv_body(1);
    tv_body["colon_replacement"] = json!("dash");
    tv_body["standard_episode_format"] =
        json!("{Series Title} - S{season:00}E{episode:00} - {Episode Title} [{Quality Title}]");
    let (status, _) = request(address, "PUT", TV_ROUTE, &tv_body.to_string()).await;
    assert_eq!(status, 200);
    let (status, tv_examples) = request(address, "GET", &format!("{TV_ROUTE}/examples"), "").await;
    assert_eq!(status, 200, "{tv_examples}");
    assert_eq!(
        tv_examples["standard_episode_format"],
        "Halcyon Vale - S03E07 - The Long Dark [WEBDL-1080p]"
    );

    let mut movie_body = fresh_movie_body(1);
    movie_body["colon_replacement"] = json!("dash");
    movie_body["standard_movie_format"] =
        json!("{Movie Title} ({Release Year}) {Edition Tags} [{Quality Title}]");
    let (status, _) = request(address, "PUT", MOVIES_ROUTE, &movie_body.to_string()).await;
    assert_eq!(status, 200);
    let (status, movie_examples) =
        request(address, "GET", &format!("{MOVIES_ROUTE}/examples"), "").await;
    assert_eq!(status, 200, "{movie_examples}");
    assert_eq!(
        movie_examples["standard_movie_format"],
        "The Wandering Harbor (2023) Director's Cut [Bluray-1080p]"
    );
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn examples_query_override_previews_without_persisting() {
    let (_scratch, db) = open_db().await;
    let (address, server) = serve(db).await;
    let mut body = fresh_tv_body(1);
    body["colon_replacement"] = json!("dash");
    body["standard_episode_format"] = json!("{Series Title}");
    let (status, _) = request(address, "PUT", TV_ROUTE, &body.to_string()).await;
    assert_eq!(status, 200);

    let overridden = format!(
        "{TV_ROUTE}/examples?standard_episode_format={}",
        encode_query_value("{Series Title} S{season:00}E{episode:00}")
    );
    let (status, preview) = request(address, "GET", &overridden, "").await;
    assert_eq!(status, 200, "{preview}");
    assert_eq!(preview["standard_episode_format"], "Halcyon Vale S03E07");

    let (_, stored) = request(address, "GET", TV_ROUTE, "").await;
    assert_eq!(
        stored["standard_episode_format"], "{Series Title}",
        "the query override must not have been persisted"
    );
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn examples_override_away_from_custom_colon_does_not_require_stored_custom_text() {
    let (_scratch, db) = open_db().await;
    let (address, server) = serve(db).await;
    let mut body = fresh_tv_body(1);
    body["colon_replacement"] = json!("custom");
    body["custom_colon_replacement"] = json!("~");
    body["standard_episode_format"] = json!("{Episode Title}");
    let (status, _) = request(address, "PUT", TV_ROUTE, &body.to_string()).await;
    assert_eq!(status, 200);

    // Overriding colon_replacement away from the stored `custom` mode, with no
    // custom_colon_replacement override, must not fall back to the stored "~" and
    // fail validate_common (which rejects custom text outside custom mode).
    let overridden = format!("{TV_ROUTE}/examples?colon_replacement=smart");
    let (status, preview) = request(address, "GET", &overridden, "").await;
    assert_eq!(status, 200, "{preview}");
    assert_eq!(preview["standard_episode_format"], "The Long Dark");
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn examples_rejects_wrong_domain_query_parameter() {
    let (_scratch, db) = open_db().await;
    let (address, server) = serve(db).await;
    let (status, error) = request(
        address,
        "GET",
        &format!("{TV_ROUTE}/examples?standard_movie_format=X"),
        "",
    )
    .await;
    assert_eq!(status, 400, "{error}");
    let (status, error) = request(
        address,
        "GET",
        &format!("{MOVIES_ROUTE}/examples?standard_episode_format=X"),
        "",
    )
    .await;
    assert_eq!(status, 400, "{error}");
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn put_tv_config_does_not_affect_movie_config_and_vice_versa() {
    let (_scratch, db) = open_db().await;
    let (address, server) = serve(db).await;

    let mut tv_body = fresh_tv_body(1);
    tv_body["rename_enabled"] = json!(true);
    tv_body["standard_episode_format"] = json!("{Series Title}");
    let (status, _) = request(address, "PUT", TV_ROUTE, &tv_body.to_string()).await;
    assert_eq!(status, 200);
    let (_, movies_after_tv_put) = request(address, "GET", MOVIES_ROUTE, "").await;
    assert_eq!(
        movies_after_tv_put,
        fresh_movie_body(1),
        "a TV write must not touch the movie singleton row"
    );

    let mut movie_body = fresh_movie_body(1);
    movie_body["rename_enabled"] = json!(true);
    movie_body["standard_movie_format"] = json!("{Movie Title}");
    let (status, _) = request(address, "PUT", MOVIES_ROUTE, &movie_body.to_string()).await;
    assert_eq!(status, 200);
    let (_, tv_after_movie_put) = request(address, "GET", TV_ROUTE, "").await;
    assert_eq!(tv_after_movie_put["revision"], 2);
    assert_eq!(tv_after_movie_put["rename_enabled"], true);
    assert_eq!(
        tv_after_movie_put["standard_episode_format"], "{Series Title}",
        "a movie write must not touch the TV singleton row"
    );
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn examples_renders_episode_title_only_template_against_fixture_with_title() {
    let (_scratch, db) = open_db().await;
    let (address, server) = serve(db).await;
    let mut body = fresh_tv_body(1);
    body["standard_episode_format"] = json!("{Episode Title}");
    let (status, _) = request(address, "PUT", TV_ROUTE, &body.to_string()).await;
    assert_eq!(status, 200);
    let (status, preview) = request(address, "GET", &format!("{TV_ROUTE}/examples"), "").await;
    assert_eq!(status, 200, "{preview}");
    assert_eq!(preview["standard_episode_format"], "The Long Dark");
    // Residual risk (docs/naming-api.md): the fixed example episode always has a
    // title, so this only proves the happy path renders. `PUT` accepts
    // `standard_episode_format = "{Episode Title}"` alone without checking that it
    // renders for an episode with no title, and this API exposes no way to inject
    // a no-title fixture into `/examples` to exercise that failure over HTTP.
    server.abort();
    let _ = server.await;
}
