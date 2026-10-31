//! Real list adds, durable search admission and the command worker against owned HTTP fixtures.
mod import_lists_support;
use axum::{
    extract::{Query, State},
    response::IntoResponse,
};
use hrrdarr::{commands, import_lists, providers};
use import_lists_support::{Harness, KEY_HEX};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn serve(router: axum::Router) -> (String, Server) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    (
        url,
        Server(tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap()
        })),
    )
}
#[derive(Clone, Default)]
struct Indexer {
    searches: Arc<Mutex<Vec<String>>>,
}
async fn indexer(
    State(state): State<Indexer>,
    Query(q): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    if q.get("t").is_some_and(|t| t == "caps") {
        return r#"<caps><limits max="100" default="100"/><searching><search available="yes" supportedParams="q"/><tv-search available="yes" supportedParams="q,tvdbid,season,ep"/><movie-search available="yes" supportedParams="q,tmdbid"/></searching><categories><category id="5000"><subcat id="5030"/></category><category id="2000"><subcat id="2030"/></category></categories></caps>"#.to_owned();
    }
    let tv = q.get("t").is_some_and(|t| t == "tvsearch");
    state
        .searches
        .lock()
        .unwrap()
        .push(if tv { "tv" } else { "movies" }.into());
    let (title, category) = if tv {
        ("Show.S01E01.1080p.WEB-DL", 5030)
    } else {
        ("Film.2020.1080p.WEB-DL", 2030)
    };
    let published = chrono::Utc::now().to_rfc2822();
    format!(
        r#"<rss xmlns:torznab="http://torznab.com/schemas/2015/feed"><channel><item><title>{title}</title><guid>fixture-{category}</guid><pubDate>{published}</pubDate><link>https://fixture.invalid/file.torrent</link><torznab:attr name="category" value="{category}"/><torznab:attr name="size" value="1073741824"/><torznab:attr name="seeders" value="10"/></item></channel></rss>"#
    )
}
async fn provider_api(h: &Harness) -> (String, Server, providers::RefreshClient) {
    let (router, client) = providers::router_with_refresh(
        h.db.clone(),
        Some(Arc::new(
            providers::CredentialKey::from_hex(KEY_HEX).unwrap(),
        )),
    );
    let (base, server) = serve(router).await;
    (base, server, client)
}
async fn create_indexer(base: &str, endpoint: &str, name: &str) {
    let response = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().unwrap()
        .post(format!("{base}/api/v1/providers"))
        .header("content-type", "application/json")
        .body(json!({"name":name,"enabled":true,"priority":1,"settings":{"implementation":"torznab","endpoint":endpoint,
            "tv":{"categories":[5030],"anime_categories":[],"enable_rss":false},
            "movies":{"categories":[2030],"enable_rss":false}}}).to_string())
        .send().await.unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    assert_eq!(status.as_u16(), 201, "{body}");
}
async fn list(h: &Harness, media: &str, search: bool, monitor: &str) -> String {
    let tv = media == "tv";
    let path = format!("/{media}-{search}-{monitor}.json");
    h.feed.json(
        &path,
        if tv {
            json!([{"tvdb_id":101,"title":"Show","year":2020}])
        } else {
            json!([{"tmdb_id":201,"title":"Film","year":2020}])
        },
    );
    let mut input = h.input(
        &format!("{media}-{search}-{monitor}"),
        "custom",
        media,
        json!({"url":format!("{}{path}",h.feed_base)}),
    );
    input["search_on_add"] = json!(search);
    input["monitor"] = json!(monitor);
    h.create(input).await
}
fn restarted(h: &Harness) -> import_lists::sync::Engine {
    import_lists::engine(
        h.db.clone(),
        Some(Arc::new(
            providers::CredentialKey::from_hex(KEY_HEX).unwrap(),
        )),
        h.deps.clone(),
    )
}

#[tokio::test]
async fn list_searches_execute_for_both_domains_and_keep_background_policy_after_restart() {
    let h = Harness::new().await;
    h.meta.show(101, "Show", Some(2020));
    h.meta.movie(201, "Film", Some(2020), None);
    let remote = Indexer::default();
    let (endpoint, _remote) = serve(
        axum::Router::new()
            .fallback(indexer)
            .with_state(remote.clone()),
    )
    .await;
    let (base, _api, client) = provider_api(&h).await;
    create_indexer(&base, &endpoint, "search").await;
    let tv = list(&h, "tv", true, "first_season").await;
    let movie = list(&h, "movies", true, "all").await;
    assert_eq!(h.sync(&tv).await["status"], "succeeded");
    assert_eq!(h.sync(&movie).await["status"], "succeeded");
    assert_eq!(h.count("SELECT count(*) FROM search_commands").await, 2);
    assert_eq!(h.count("SELECT count(*) FROM search_commands WHERE media_type='tv' AND requested_episode_id IN (SELECT id FROM episodes WHERE season=1)").await,1);
    assert_eq!(h.count("SELECT count(*) FROM search_commands WHERE media_type='movies' AND requested_movie_id=1").await,1);
    let items = h.items(&tv, "added").await;
    assert_eq!(items[0]["search_status"], "admitted");

    // The same IDs belong to different domains, with ordinary background availability/delay.
    h.sql("UPDATE episodes SET runtime=45; UPDATE movie_metadata SET runtime=100,digital_release='2999-01-01T00:00:00Z',physical_release='2999-01-01T00:00:00Z',in_cinemas='2999-01-01T00:00:00Z'; UPDATE quality_definitions SET min_size=0; UPDATE quality_profile_items SET allowed=1; INSERT OR REPLACE INTO release_delay_policies VALUES('tv',60,60,0),('movies',0,0,0)").await;
    let driver = commands::start_driven(h.db.clone(), client.clone())
        .await
        .unwrap();
    for _ in 0..12 {
        driver.step().await.unwrap();
        if h.count("SELECT count(*) FROM search_commands WHERE fetch_complete=1")
            .await
            == 2
        {
            break;
        }
    }
    assert_eq!(
        h.count("SELECT count(*) FROM search_commands WHERE fetch_complete=1")
            .await,
        2
    );
    // List searches record real dispatch for Wanted ordering while retaining background authority.
    assert_eq!(
        h.count("SELECT count(*) FROM search_commands WHERE dispatch_completed_at IS NOT NULL")
            .await,
        2
    );
    assert_eq!(
        h.count("SELECT count(*) FROM episodes WHERE native_last_search_at IS NOT NULL")
            .await,
        1
    );
    assert_eq!(
        h.count("SELECT count(*) FROM movies WHERE native_last_search_at IS NOT NULL")
            .await,
        1
    );
    assert_eq!(h.count("SELECT count(*) FROM search_results s JOIN search_commands c ON c.id=s.command_id WHERE c.media_type='movies' AND s.decision_json LIKE '%movie_unavailable%'").await,1);
    assert_eq!(
        h.count("SELECT count(*) FROM rss_candidates WHERE media_type='movies'")
            .await,
        0
    );
    assert_eq!(
        h.count("SELECT count(*) FROM rss_candidates WHERE media_type='tv' AND status='pending'")
            .await,
        1
    );

    // Removing provenance must not grant the pending candidate the user-search delay bypass.
    let c = h.db.connect().await.unwrap();
    assert!(
        c.execute("DELETE FROM import_list_search_commands", ())
            .await
            .is_err()
    );
    h.sql("DELETE FROM import_list_additions WHERE media_type='tv'")
        .await;
    assert_eq!(
        h.count("SELECT count(*) FROM import_list_search_commands WHERE addition_id IS NULL")
            .await,
        1
    );
    drop(driver);
    let driver = commands::start_driven(h.db.clone(), client).await.unwrap();
    for _ in 0..12 {
        driver.step().await.unwrap();
        if h.count(
            "SELECT count(*) FROM rss_candidates WHERE media_type='tv' AND not_before>unixepoch()",
        )
        .await
            == 1
        {
            break;
        }
    }
    assert_eq!(h.count("SELECT count(*) FROM rss_candidates WHERE media_type='tv' AND status='pending' AND not_before>unixepoch() AND decision_reasons_json LIKE '%configured_delay%'").await,1);
    assert_eq!(
        h.count("SELECT count(*) FROM rss_client_attempts").await,
        0,
        "no early submission or client selection while delayed"
    );
    drop(driver);
    restarted(&h).tick(0).await.unwrap();
    h.sync(&movie).await;
    assert_eq!(
        h.count("SELECT count(*) FROM search_commands").await,
        2,
        "completed searches are not re-admitted by sync or engine restart"
    );
    let requests = remote.searches.lock().unwrap();
    assert!(
        requests.iter().any(|s| s == "tv") && requests.iter().any(|s| s == "movies"),
        "{requests:?}"
    );
}

#[tokio::test]
async fn search_intent_retries_missing_indexers_and_capacity_atomically_after_restart() {
    let h = Harness::new().await;
    h.meta.show(101, "Show", Some(2020));
    h.meta.movie(201, "Film", Some(2020), None);
    let tv = list(&h, "tv", true, "first_season").await;
    h.sync(&tv).await;
    assert_eq!(h.count("SELECT count(*) FROM search_commands").await, 0);
    let items = h.items(&tv, "added").await;
    assert_eq!(items[0]["search_status"], "pending");
    assert_eq!(items[0]["search_error_code"], "no_search_indexer");
    let (base, _api, _client) = provider_api(&h).await;
    create_indexer(&base, &h.feed_base, "first").await;
    h.sql("UPDATE import_list_search_requests SET next_attempt_at=0")
        .await;
    restarted(&h).tick(0).await.unwrap();
    assert_eq!(h.count("SELECT count(*) FROM search_commands").await, 1);
    // Seed the existing active-command pool using valid clones of the captured target. A second
    // request must admit no partial work, then succeed once the pool is available after restart.
    h.sql("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1023) INSERT INTO search_commands(id,mode,decision_context,media_type,requested_episode_id,requested_movie_id,captured_target_json,indexer_id,indexer_revision,client_id,client_revision,priority,status,attempts,next_attempt_at,created_at,fetched,fetch_complete,selection_kind) SELECT printf('%08x-0000-4000-8000-%012x',x,x),c.mode,c.decision_context,c.media_type,c.requested_episode_id,c.requested_movie_id,c.captured_target_json,c.indexer_id,c.indexer_revision,c.client_id,c.client_revision,0,'queued',0,0,0,0,0,c.selection_kind FROM n CROSS JOIN (SELECT * FROM search_commands LIMIT 1) c").await;
    create_indexer(&base, &h.feed_base, "second").await;
    // One slot is free, but this movie requires two indexers: admitting a prefix would lose work.
    h.sql("UPDATE search_commands SET status='cancelled',completed_at=unixepoch() WHERE id='00000001-0000-4000-8000-000000000001'").await;
    let movie = list(&h, "movies", true, "all").await;
    h.sync(&movie).await;
    assert_eq!(
        h.count("SELECT count(*) FROM search_commands WHERE media_type='movies'")
            .await,
        0
    );
    let items = h.items(&movie, "added").await;
    assert_eq!(items[0]["search_error_code"], "command_history_full");
    h.sql("UPDATE search_commands SET status='cancelled',completed_at=unixepoch() WHERE status='queued'; UPDATE import_list_search_requests SET next_attempt_at=0 WHERE status='pending'").await;
    // Requested work survives deletion of its source list and is not conditional on future syncs.
    assert_eq!(
        h.call(
            "DELETE",
            &format!("/api/v1/importlist/{movie}"),
            Value::Null
        )
        .await
        .0,
        204
    );
    restarted(&h).tick(0).await.unwrap();
    assert_eq!(
        h.count("SELECT count(*) FROM search_commands WHERE media_type='movies'")
            .await,
        2
    );
    assert_eq!(
        h.count("SELECT count(*) FROM import_list_search_requests WHERE status='pending'")
            .await,
        0
    );
    // Terminal history retention does not define idempotence: the admitted outbox marker does.
    h.sql("UPDATE search_commands SET status='cancelled',completed_at=unixepoch() WHERE status='queued'; DELETE FROM search_commands").await;
    restarted(&h).tick(0).await.unwrap();
    assert_eq!(h.count("SELECT count(*) FROM search_commands").await, 0);
}

#[tokio::test]
async fn disabled_search_and_unmonitored_adds_do_not_queue_searches() {
    let h = Harness::new().await;
    h.meta.show(101, "Show", Some(2020));
    h.meta.movie(201, "Film", Some(2020), None);
    let tv = list(&h, "tv", false, "all").await;
    let movie = list(&h, "movies", true, "none").await;
    h.sync(&tv).await;
    h.sync(&movie).await;
    assert_eq!(
        h.count("SELECT count(*) FROM import_list_additions").await,
        2
    );
    assert_eq!(
        h.count("SELECT count(*) FROM import_list_search_requests")
            .await,
        0
    );
    assert_eq!(h.count("SELECT count(*) FROM search_commands").await, 0);
}

#[tokio::test]
async fn oversized_search_capture_remains_pending_without_partial_admission() {
    let h = Harness::new().await;
    h.meta.show(101, "Show", Some(2020));
    let tv = list(&h, "tv", true, "first_season").await;
    h.sync(&tv).await;
    h.sql("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1024) INSERT INTO episodes(series_id,season,number,title,monitored) SELECT 1,1,x+1,'Extra',1 FROM n; UPDATE import_list_search_requests SET next_attempt_at=0").await;
    let (base, _api, _client) = provider_api(&h).await;
    create_indexer(&base, &h.feed_base, "first").await;
    restarted(&h).tick(0).await.unwrap();
    assert_eq!(
        h.count("SELECT count(*) FROM search_commands").await,
        0,
        "the 1025th target must not silently lose a tail after partial admission"
    );
    assert_eq!(
        h.count("SELECT count(*) FROM import_list_search_commands")
            .await,
        0
    );
    let items = h.items(&tv, "added").await;
    assert_eq!(items[0]["search_status"], "pending");
    assert_eq!(items[0]["search_error_code"], "search_on_add_target_limit");
}
