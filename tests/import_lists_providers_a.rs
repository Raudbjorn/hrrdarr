//! Trakt, TMDb, IMDb-list and StevenLu import-list providers against the in-process fake list source:
//! exact requests (path, query, headers), pagination and its bounds, parsing, domain handling, failure
//! classes, secret hygiene and end-to-end syncs into the library. No real third-party service is used.
mod import_lists_support;
use hrrdarr::import_lists::providers::{MAX_REQUESTS, imdb::parse_csv};
use import_lists_support::Harness;
use serde_json::{Value, json};

const TRAKT_ID: &str = "trakt-CLIENT-SECRET-1";
const TRAKT_TOKEN: &str = "trakt-TOKEN-SECRET-2";
const TMDB_TOKEN: &str = "tmdb-TOKEN-SECRET-3";

// ---------------------------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------------------------

fn with_secrets(mut input: Value, secrets: Value) -> Value {
    input["secrets"] = secrets;
    input
}
async fn probe(h: &Harness, draft: Value) -> Value {
    let (status, body) = h
        .call("POST", "/api/v1/importlist/test", json!({"draft": draft}))
        .await;
    assert_eq!(status, 200, "{body}");
    body
}
async fn error_of(h: &Harness, draft: &Value) -> String {
    let body = probe(h, draft.clone()).await;
    assert_eq!(body["ok"], false, "{body}");
    body["error"].as_str().unwrap_or_default().to_owned()
}
async fn count_of(h: &Harness, draft: &Value) -> i64 {
    let body = probe(h, draft.clone()).await;
    assert_eq!(body["ok"], true, "{body}");
    body["item_count"].as_i64().unwrap()
}
fn targets(h: &Harness) -> Vec<String> {
    h.feed.requests().iter().map(|r| r.target.clone()).collect()
}
/// Every failing status class, and no remote text, URL or secret in the answer.
async fn assert_status_classes(h: &Harness, route: &str, draft: &Value, secrets: &[&str]) {
    for (status, code) in [
        (401, "authentication"),
        (403, "authentication"),
        (429, "rate_limited"),
        (500, "server_error"),
        (503, "server_error"),
        (404, "rejected"),
    ] {
        h.feed.set(route, status, "text/plain", "REMOTE-BODY-TEXT");
        let body = probe(h, draft.clone()).await;
        assert_eq!(
            (body["ok"].clone(), body["error"].clone()),
            (json!(false), json!(code)),
            "status {status}"
        );
        let text = body.to_string();
        assert!(
            !text.contains("REMOTE-BODY-TEXT") && !text.contains(route),
            "{text}"
        );
        for secret in secrets {
            assert!(!text.contains(secret), "{text}");
        }
    }
}
async fn assert_invalid_bodies(h: &Harness, route: &str, draft: &Value, bodies: &[&str]) {
    for body in bodies {
        h.feed.set(route, 200, "application/json", body.to_string());
        assert_eq!(error_of(h, draft).await, "invalid_response", "body: {body}");
    }
}
/// The secrets are write-only: not in any API response, stored setting, credential blob or request URL.
async fn assert_secret_hygiene(h: &Harness, id: &str, secrets: &[&str], configured: &[&str]) {
    let (_, one) = h
        .call("GET", &format!("/api/v1/importlist/{id}"), Value::Null)
        .await;
    let (_, all) = h.call("GET", "/api/v1/importlist", Value::Null).await;
    let (_, runs) = h
        .call("GET", &format!("/api/v1/importlist/{id}/runs"), Value::Null)
        .await;
    let (_, schema) = h
        .call("GET", "/api/v1/importlist/schema", Value::Null)
        .await;
    let mut names: Vec<&str> = one["secrets_configured"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, configured);
    for secret in secrets {
        for text in [
            one.to_string(),
            all.to_string(),
            runs.to_string(),
            schema.to_string(),
        ] {
            assert!(!text.contains(secret), "{text}");
        }
        assert_eq!(
            h.count(&format!("SELECT count(*) FROM import_lists WHERE instr(settings_json,'{secret}')>0 OR instr(credentials,'{secret}')>0")).await,
            0
        );
        assert!(
            h.feed.requests().iter().all(|r| !r.target.contains(secret)),
            "secret leaked into a URL"
        );
    }
    assert_eq!(
        h.count("SELECT count(*) FROM import_lists WHERE credentials IS NOT NULL")
            .await,
        1
    );
}
async fn all_items(h: &Harness, id: &str) -> Vec<Value> {
    let (status, page) = h
        .call(
            "GET",
            &format!("/api/v1/importlist/{id}/items?limit=500"),
            Value::Null,
        )
        .await;
    assert_eq!(status, 200, "{page}");
    page["items"].as_array().unwrap().clone()
}

// ---------------------------------------------------------------------------------------------
// Trakt
// ---------------------------------------------------------------------------------------------

fn trakt_ids(i: i64) -> Value {
    json!({"trakt": i, "slug": format!("item-{i}"), "imdb": format!("tt{i:07}"), "tvdb": 1000 + i, "tmdb": 5000 + i})
}
fn trakt_show(i: i64) -> Value {
    json!({"title": format!("Show {i}"), "year": 2000 + i % 20, "ids": trakt_ids(i)})
}
fn trakt_movie(i: i64) -> Value {
    json!({"title": format!("Movie {i}"), "year": 2000 + i % 20, "ids": trakt_ids(i)})
}
fn trakt_list(h: &Harness, media: &str, settings: Value) -> Value {
    with_secrets(
        h.input("Trakt", "trakt", media, settings),
        json!({"client_id": TRAKT_ID}),
    )
}

#[tokio::test]
async fn trakt_popular_paginates_with_exact_requests_and_stops_on_limit_or_short_page() {
    let h = Harness::new().await;
    let draft = trakt_list(&h, "tv", json!({"limit": 150}));
    h.feed.json(
        "/shows/popular?page=1&limit=100",
        Value::Array((1..=100).map(trakt_show).collect()),
    );
    h.feed.json(
        "/shows/popular?page=2&limit=100",
        Value::Array((101..=150).map(trakt_show).collect()),
    );
    assert_eq!(count_of(&h, &draft).await, 150);
    assert_eq!(
        targets(&h),
        [
            "/shows/popular?page=1&limit=100",
            "/shows/popular?page=2&limit=100"
        ]
    );
    let first = &h.feed.requests()[0];
    assert_eq!(first.method, "GET");
    assert_eq!(first.header("trakt-api-key"), Some(TRAKT_ID));
    assert_eq!(first.header("trakt-api-version"), Some("2"));
    assert_eq!(first.header("accept"), Some("application/json"));
    assert_eq!(
        first.header("authorization"),
        None,
        "no token configured, none sent"
    );

    // The limit stops a full page without asking for another one, and truncates the surplus.
    h.feed.clear();
    let draft = trakt_list(&h, "tv", json!({"limit": 100}));
    assert_eq!(count_of(&h, &draft).await, 100);
    assert_eq!(h.feed.requests().len(), 1);
    h.feed.clear();
    h.feed.json(
        "/shows/popular?page=1&limit=30",
        Value::Array((1..=30).map(trakt_show).collect()),
    );
    h.feed.json(
        "/shows/popular?page=1&limit=7",
        Value::Array((1..=7).map(trakt_show).collect()),
    );
    assert_eq!(
        count_of(&h, &trakt_list(&h, "tv", json!({"limit": 7}))).await,
        7
    );
    assert_eq!(
        targets(&h),
        ["/shows/popular?page=1&limit=7"],
        "page size is min(limit, 100)"
    );

    // A full page that does not reach the limit continues; an empty page ends the list.
    h.feed.clear();
    h.feed.json("/shows/popular?page=2&limit=100", json!([]));
    assert_eq!(
        count_of(&h, &trakt_list(&h, "tv", json!({"limit": 250}))).await,
        100
    );
    assert_eq!(h.feed.requests().len(), 2);
    // An empty list is a successful empty list.
    h.feed.json("/shows/popular?page=1&limit=100", json!([]));
    h.feed.clear();
    assert_eq!(count_of(&h, &trakt_list(&h, "tv", json!({}))).await, 0);
    assert_eq!(
        targets(&h),
        ["/shows/popular?page=1&limit=100"],
        "defaults: popular, limit 100"
    );
}

#[tokio::test]
async fn trakt_tv_popular_sync_creates_series_by_tvdb_id_and_by_title_when_the_id_is_missing() {
    let h = Harness::new().await;
    h.meta.show(1001, "Alpha", Some(2010));
    h.meta.show(1002, "Beta", Some(2011));
    h.meta.show(1003, "Gamma", Some(2021));
    h.feed.json(
        "/shows/popular?page=1&limit=3",
        json!([
            {"title": "Alpha", "year": 2010, "ids": {"trakt": 1, "slug": "alpha", "tvdb": 1001, "imdb": "tt0000001", "tmdb": null}},
            {"title": "Beta", "year": 2011, "ids": {"tvdb": 1002}},
            {"title": "Gamma", "year": 2021, "ids": {"trakt": 3, "tvdb": null, "imdb": null}}
        ]),
    );
    let id = h.create(trakt_list(&h, "tv", json!({"limit": 3}))).await;
    let run = h.sync(&id).await;
    assert_eq!(run["status"], "succeeded", "{run}");
    assert_eq!(
        (
            run["items_fetched"].as_i64(),
            run["added"].as_i64(),
            run["unresolved"].as_i64()
        ),
        (Some(3), Some(3), Some(0))
    );
    assert_eq!(h.count("SELECT count(*) FROM series").await, 3);
    let items = all_items(&h, &id).await;
    let keys: Vec<&str> = items.iter().map(|i| i["key"].as_str().unwrap()).collect();
    assert_eq!(keys[..2], ["tvdb:1001", "tvdb:1002"]);
    assert_eq!(
        items[0]["imdb_id"], "tt0000001",
        "imdb is carried alongside the tvdb identity"
    );
    assert!(
        items[0]["tmdb_id"].is_null(),
        "a TV item's identity is the tvdb id"
    );
    assert_secret_hygiene(&h, &id, &[TRAKT_ID], &["client_id"]).await;
}

#[tokio::test]
async fn trakt_movie_trending_unwraps_entries_skips_the_other_kind_and_sends_the_bearer_token() {
    let h = Harness::new().await;
    for i in 1..=3 {
        h.meta.movie(
            5000 + i,
            &format!("Movie {i}"),
            Some(2000 + i),
            Some(&format!("tt{i:07}")),
        );
    }
    h.feed.json(
        "/movies/trending?page=1&limit=4",
        json!([
            {"watchers": 90, "movie": trakt_movie(1)},
            {"watchers": 80, "show": trakt_show(9)},
            {"watchers": 70, "movie": trakt_movie(2)},
            {"watchers": 60, "movie": trakt_movie(3)}
        ]),
    );
    h.feed.json("/movies/trending?page=2&limit=4", json!([]));
    let draft = with_secrets(
        h.input(
            "Trend",
            "trakt",
            "movies",
            json!({"list_type": "trending", "limit": 4}),
        ),
        json!({"client_id": TRAKT_ID, "access_token": TRAKT_TOKEN}),
    );
    let body = probe(&h, draft.clone()).await;
    assert_eq!(
        (body["ok"].clone(), body["item_count"].clone()),
        (json!(true), json!(3)),
        "{body}"
    );
    assert_eq!(
        targets(&h),
        [
            "/movies/trending?page=1&limit=4",
            "/movies/trending?page=2&limit=4"
        ],
        "a skipped entry does not end paging"
    );
    let req = &h.feed.requests()[0];
    assert_eq!(
        req.header("authorization"),
        Some(format!("Bearer {TRAKT_TOKEN}").as_str())
    );
    assert_eq!(req.header("trakt-api-key"), Some(TRAKT_ID));
    assert_eq!(req.header("trakt-api-version"), Some("2"));

    let id = h.create(draft).await;
    let run = h.sync(&id).await;
    assert_eq!(
        (run["status"].as_str(), run["added"].as_i64()),
        (Some("succeeded"), Some(3)),
        "{run}"
    );
    assert_eq!(h.count("SELECT count(*) FROM movies").await, 3);
    let items = all_items(&h, &id).await;
    assert_eq!(items[0]["key"], "tmdb:5001");
    assert_eq!(items[0]["imdb_id"], "tt0000001");
    assert!(
        items[0]["tvdb_id"].is_null(),
        "a movie's identity is the tmdb id"
    );
    assert_secret_hygiene(
        &h,
        &id,
        &[TRAKT_ID, TRAKT_TOKEN],
        &["access_token", "client_id"],
    )
    .await;
}

#[tokio::test]
async fn trakt_anticipated_watched_and_wrapped_shows_parse_for_the_requested_domain() {
    let h = Harness::new().await;
    h.feed.json(
        "/shows/anticipated?page=1&limit=100",
        json!([{"list_count": 5, "show": trakt_show(1)}, {"list_count": 4, "show": trakt_show(2)}]),
    );
    let anticipated = trakt_list(&h, "tv", json!({"list_type": "anticipated"}));
    assert_eq!(count_of(&h, &anticipated).await, 2);
    h.feed.json(
        "/shows/watched?page=1&limit=100",
        json!([{"watcher_count": 5, "play_count": 9, "collected_count": 1, "show": trakt_show(3)}]),
    );
    assert_eq!(
        count_of(&h, &trakt_list(&h, "tv", json!({"list_type": "watched"}))).await,
        1
    );
    // A movie-only payload under a TV list yields nothing, not an error.
    h.feed.json(
        "/shows/watched?page=1&limit=100",
        json!([{"watchers": 1, "movie": trakt_movie(4)}]),
    );
    assert_eq!(
        count_of(&h, &trakt_list(&h, "tv", json!({"list_type": "watched"}))).await,
        0
    );
    // Duplicate entries collapse to one item.
    h.feed.json(
        "/shows/trending?page=1&limit=100",
        json!([{"watchers": 1, "show": trakt_show(5)}, {"watchers": 1, "show": trakt_show(5)}]),
    );
    assert_eq!(
        count_of(&h, &trakt_list(&h, "tv", json!({"list_type": "trending"}))).await,
        1
    );
}

#[tokio::test]
async fn trakt_user_endpoints_percent_encode_names_and_require_their_settings() {
    let h = Harness::new().await;
    h.feed.json(
        "/users/Jane%20Doe/watchlist/shows?page=1&limit=100",
        json!([{"rank": 1, "listed_at": "2020-01-01T00:00:00.000Z", "type": "show", "show": trakt_show(1)},
               {"rank": 2, "type": "movie", "movie": trakt_movie(2)},
               {"rank": 3, "type": "season", "season": {"number": 1}}]),
    );
    let watchlist = with_secrets(
        h.input(
            "WL",
            "trakt",
            "tv",
            json!({"list_type": "user_watchlist", "username": "Jane Doe"}),
        ),
        json!({"client_id": TRAKT_ID, "access_token": TRAKT_TOKEN}),
    );
    assert_eq!(
        count_of(&h, &watchlist).await,
        1,
        "entries of other types are skipped"
    );
    assert_eq!(
        targets(&h),
        ["/users/Jane%20Doe/watchlist/shows?page=1&limit=100"]
    );

    h.feed.clear();
    h.feed.json(
        "/users/jane/lists/my%2Flist%2Bx%20y/items/movies?page=1&limit=100",
        json!([{"type": "movie", "movie": trakt_movie(1)}, {"type": "show", "show": trakt_show(2)}, {"type": "movie", "movie": trakt_movie(3)}]),
    );
    let custom = trakt_list(
        &h,
        "movies",
        json!({"list_type": "user_list", "username": "jane", "list_slug": "my/list+x y"}),
    );
    assert_eq!(count_of(&h, &custom).await, 2);
    assert_eq!(
        targets(&h),
        ["/users/jane/lists/my%2Flist%2Bx%20y/items/movies?page=1&limit=100"]
    );
    assert_eq!(h.feed.requests()[0].header("authorization"), None);

    // Missing names are configuration failures and never reach the wire.
    h.feed.clear();
    for settings in [
        json!({"list_type": "user_watchlist"}),
        json!({"list_type": "user_list", "username": "jane"}),
        json!({"list_type": "user_list", "list_slug": "x"}),
    ] {
        assert_eq!(
            error_of(&h, &trakt_list(&h, "tv", settings.clone())).await,
            "invalid_settings",
            "{settings}"
        );
    }
    assert!(h.feed.requests().is_empty());
    // Over-long names are refused when the list is saved.
    let long = trakt_list(
        &h,
        "tv",
        json!({"list_type": "user_list", "username": "a".repeat(129), "list_slug": "x"}),
    );
    assert_eq!(h.call("POST", "/api/v1/importlist", long).await.0, 400);
}

#[tokio::test]
async fn trakt_failure_classes_and_malformed_documents() {
    let h = Harness::new().await;
    let draft = trakt_list(&h, "tv", json!({}));
    let route = "/shows/popular?page=1&limit=100";
    assert_status_classes(&h, route, &draft, &[TRAKT_ID]).await;
    assert_invalid_bodies(
        &h,
        route,
        &draft,
        &[
            "not json",
            "",
            "{\"items\": []}",
            "\"string\"",
            "[\"x\"]",
            "[null]",
            &json!([{"title": "A", "ids": {"tvdb": "abc"}}]).to_string(),
            &json!([{"title": "A", "ids": {"tvdb": 0}}]).to_string(),
            &json!([{"title": "A", "ids": {"tvdb": -4}}]).to_string(),
            &json!([{"title": "A", "ids": {"tvdb": 1.5}}]).to_string(),
            &json!([{"title": "A", "ids": {"tvdb": [1]}}]).to_string(),
            &json!([{"title": "A", "ids": "x"}]).to_string(),
            &json!([{"title": "A", "ids": {"imdb": "nm0000001"}}]).to_string(),
            &json!([{"title": "A", "ids": {"imdb": 12}}]).to_string(),
            &json!([{"title": "A", "year": "soon"}]).to_string(),
            &json!([{"title": "A", "year": 0}]).to_string(),
        ],
    )
    .await;
    // A malformed identity anywhere fails the whole list, even behind valid entries.
    h.feed.json(
        route,
        json!([trakt_show(1), trakt_show(2), {"title": "Bad", "ids": {"tvdb": "x"}}]),
    );
    assert_eq!(error_of(&h, &draft).await, "invalid_response");
    // Wrapped lists validate the wrapper too.
    let trending = trakt_list(&h, "tv", json!({"list_type": "trending"}));
    assert_invalid_bodies(
        &h,
        "/shows/trending?page=1&limit=100",
        &trending,
        &[
            &json!([{"watchers": 1, "show": "x"}]).to_string(),
            &json!([{"watchers": 1, "show": {"title": "A", "ids": {"tvdb": "q"}}}]).to_string(),
            &json!([{"type": 7, "show": trakt_show(1)}]).to_string(),
            &json!({"show": trakt_show(1)}).to_string(),
        ],
    )
    .await;
    // A failing second page fails the whole fetch rather than returning the first page.
    h.feed
        .json(route, Value::Array((1..=100).map(trakt_show).collect()));
    h.feed
        .set("/shows/popular?page=2&limit=100", 500, "text/plain", "x");
    let long = trakt_list(&h, "tv", json!({"limit": 200}));
    assert_eq!(error_of(&h, &long).await, "server_error");

    // The failure surfaces on a sync run and the list, as a stable class.
    h.feed.set(route, 401, "text/plain", "REMOTE-BODY-TEXT");
    let id = h.create(draft).await;
    let run = h.sync(&id).await;
    assert_eq!(
        (run["status"].as_str(), run["error_code"].as_str()),
        (Some("failed"), Some("authentication")),
        "{run}"
    );
    let (_, list) = h
        .call("GET", &format!("/api/v1/importlist/{id}"), Value::Null)
        .await;
    assert_eq!(list["last_error"], "authentication");
    assert_eq!(h.count("SELECT count(*) FROM series").await, 0);
}

#[tokio::test]
async fn trakt_request_budget_is_bounded_and_fails_instead_of_truncating() {
    let h = Harness::new().await;
    // Every page is full but holds only entries of the other kind, so the limit is never reached.
    for page in 1..=MAX_REQUESTS + 1 {
        h.feed.json(
            &format!("/users/u/watchlist/movies?page={page}&limit=100"),
            Value::Array(
                (1..=100)
                    .map(|i| json!({"type": "show", "show": trakt_show(i)}))
                    .collect(),
            ),
        );
    }
    let draft = trakt_list(
        &h,
        "movies",
        json!({"list_type": "user_watchlist", "username": "u"}),
    );
    assert_eq!(error_of(&h, &draft).await, "resource_limit");
    assert_eq!(
        h.feed.requests().len(),
        MAX_REQUESTS,
        "no request past the budget"
    );
    assert_eq!(
        h.feed.requests().last().unwrap().target,
        format!("/users/u/watchlist/movies?page={MAX_REQUESTS}&limit=100")
    );
}

#[tokio::test]
async fn trakt_schema_validation_domains_and_defaults() {
    let h = Harness::new().await;
    let (_, schema) = h
        .call("GET", "/api/v1/importlist/schema", Value::Null)
        .await;
    let trakt = schema["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["implementation"] == "trakt")
        .expect("trakt is available")
        .clone();
    assert_eq!(trakt["domains"], json!(["tv", "movies"]));
    assert!(
        trakt["description"]
            .as_str()
            .unwrap()
            .contains("not implemented"),
        "the static-token scope is stated"
    );
    let field = |name: &str| {
        trakt["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["name"] == name)
            .unwrap()
            .clone()
    };
    assert_eq!(
        (
            field("client_id")["secret"].clone(),
            field("client_id")["required"].clone()
        ),
        (json!(true), json!(true))
    );
    assert_eq!(
        (
            field("access_token")["secret"].clone(),
            field("access_token")["required"].clone()
        ),
        (json!(true), json!(false))
    );
    assert_eq!(field("list_type")["default"], "popular");
    assert_eq!(
        (
            field("limit")["min"].clone(),
            field("limit")["max"].clone(),
            field("limit")["default"].clone()
        ),
        (json!(1), json!(1000), json!(100))
    );

    let created = h
        .call(
            "POST",
            "/api/v1/importlist",
            trakt_list(&h, "tv", json!({})),
        )
        .await
        .1;
    assert_eq!(
        created["settings"],
        json!({"list_type": "popular", "limit": 100})
    );
    // client_id is required; unknown types and out-of-range limits are rejected; secrets are not settings.
    for (label, body) in [
        ("no client id", h.input("a", "trakt", "tv", json!({}))),
        (
            "bad list type",
            trakt_list(&h, "movies", json!({"list_type": "everything"})),
        ),
        ("limit 0", trakt_list(&h, "movies", json!({"limit": 0}))),
        (
            "limit 1001",
            trakt_list(&h, "movies", json!({"limit": 1001})),
        ),
        (
            "limit as text",
            trakt_list(&h, "movies", json!({"limit": "5"})),
        ),
        (
            "secret as setting",
            trakt_list(&h, "movies", json!({"client_id": "x"})),
        ),
    ] {
        let mut body = body;
        body["name"] = json!(label);
        assert_eq!(
            h.call("POST", "/api/v1/importlist", body).await.0,
            400,
            "{label}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// TMDb
// ---------------------------------------------------------------------------------------------

fn tmdb_results(range: std::ops::RangeInclusive<i64>) -> Vec<Value> {
    range
        .map(|i| json!({"id": 9000 + i, "title": format!("Film {i}"), "release_date": format!("{}-06-01", 1990 + i % 30), "poster_path": "/p.jpg", "adult": false}))
        .collect()
}
fn tmdb_page(page: i64, total_pages: i64, results: Vec<Value>) -> Value {
    json!({"page": page, "total_pages": total_pages, "total_results": total_pages * 20, "results": results})
}
fn tmdb_list(h: &Harness, settings: Value) -> Value {
    with_secrets(
        h.input("TMDb", "tmdb", "movies", settings),
        json!({"access_token": TMDB_TOKEN}),
    )
}

#[tokio::test]
async fn tmdb_popular_paginates_with_bearer_header_and_stops_on_limit_total_pages_or_empty_page() {
    let h = Harness::new().await;
    h.feed.json(
        "/3/movie/popular?page=1",
        tmdb_page(1, 5, tmdb_results(1..=20)),
    );
    h.feed.json(
        "/3/movie/popular?page=2",
        tmdb_page(2, 5, tmdb_results(21..=40)),
    );
    assert_eq!(count_of(&h, &tmdb_list(&h, json!({"limit": 30}))).await, 30);
    assert_eq!(
        targets(&h),
        ["/3/movie/popular?page=1", "/3/movie/popular?page=2"],
        "stops at the limit"
    );
    let first = &h.feed.requests()[0];
    assert_eq!(first.method, "GET");
    assert_eq!(
        first.header("authorization"),
        Some(format!("Bearer {TMDB_TOKEN}").as_str())
    );
    assert_eq!(first.header("accept"), Some("application/json"));

    // total_pages ends the list before the limit.
    h.feed.clear();
    h.feed.json(
        "/3/movie/popular?page=1",
        tmdb_page(1, 2, tmdb_results(1..=20)),
    );
    h.feed.json(
        "/3/movie/popular?page=2",
        tmdb_page(2, 2, tmdb_results(21..=40)),
    );
    assert_eq!(
        count_of(&h, &tmdb_list(&h, json!({}))).await,
        40,
        "default limit is 200"
    );
    assert_eq!(h.feed.requests().len(), 2);

    // Without total_pages an empty page ends it.
    h.feed.clear();
    h.feed.json(
        "/3/movie/popular?page=1",
        json!({"results": tmdb_results(1..=5)}),
    );
    h.feed
        .json("/3/movie/popular?page=2", json!({"results": []}));
    assert_eq!(count_of(&h, &tmdb_list(&h, json!({}))).await, 5);
    assert_eq!(h.feed.requests().len(), 2);

    // An empty list is a successful empty list.
    h.feed
        .json("/3/movie/popular?page=1", tmdb_page(1, 0, vec![]));
    assert_eq!(count_of(&h, &tmdb_list(&h, json!({}))).await, 0);
}

#[tokio::test]
async fn tmdb_list_types_use_their_endpoints_and_require_a_target_when_needed() {
    let h = Harness::new().await;
    for kind in ["top_rated", "upcoming", "now_playing"] {
        h.feed.json(
            &format!("/3/movie/{kind}?page=1"),
            tmdb_page(1, 1, tmdb_results(1..=3)),
        );
        h.feed.clear();
        assert_eq!(
            count_of(&h, &tmdb_list(&h, json!({"list_type": kind}))).await,
            3
        );
        assert_eq!(targets(&h), [format!("/3/movie/{kind}?page=1")]);
    }
    for (kind, query) in [
        ("person", "with_cast=77"),
        ("company", "with_companies=77"),
        ("keyword", "with_keywords=77"),
    ] {
        let route = format!("/3/discover/movie?{query}&page=1&sort_by=popularity.desc");
        h.feed.json(&route, tmdb_page(1, 1, tmdb_results(1..=4)));
        h.feed.clear();
        assert_eq!(
            count_of(
                &h,
                &tmdb_list(&h, json!({"list_type": kind, "target_id": 77}))
            )
            .await,
            4,
            "{kind}"
        );
        assert_eq!(targets(&h), [route]);
    }
    // A TMDb list keeps its movies under `items`; entries of another media type are skipped.
    let mut items = tmdb_results(1..=3);
    items.push(
        json!({"id": 1, "name": "A Show", "media_type": "tv", "first_air_date": "2020-01-01"}),
    );
    items[0]["media_type"] = json!("movie");
    h.feed.json(
        "/3/list/12345?page=1",
        json!({"id": "12345", "page": 1, "total_pages": 1, "items": items}),
    );
    h.feed.clear();
    assert_eq!(
        count_of(
            &h,
            &tmdb_list(&h, json!({"list_type": "list", "target_id": 12345}))
        )
        .await,
        3
    );
    assert_eq!(targets(&h), ["/3/list/12345?page=1"]);

    h.feed.clear();
    for kind in ["list", "person", "company", "keyword"] {
        assert_eq!(
            error_of(&h, &tmdb_list(&h, json!({"list_type": kind}))).await,
            "invalid_settings",
            "{kind}"
        );
    }
    assert!(h.feed.requests().is_empty());
}

#[tokio::test]
async fn tmdb_sync_adds_movies_and_records_years_from_release_dates() {
    let h = Harness::new().await;
    for (id, title) in [
        (9001, "Dated"),
        (9002, "Empty Date"),
        (9003, "No Date"),
        (9004, "Null Date"),
    ] {
        h.meta.movie(id, title, Some(2001), None);
    }
    h.feed.json(
        "/3/movie/popular?page=1",
        tmdb_page(
            1,
            1,
            vec![
                json!({"id": 9001, "title": "Dated", "release_date": "2019-05-01"}),
                json!({"id": 9002, "title": "Empty Date", "release_date": ""}),
                json!({"id": 9003, "title": "No Date"}),
                json!({"id": 9004, "title": "Null Date", "release_date": null}),
            ],
        ),
    );
    let id = h.create(tmdb_list(&h, json!({}))).await;
    let run = h.sync(&id).await;
    assert_eq!(
        (run["status"].as_str(), run["added"].as_i64()),
        (Some("succeeded"), Some(4)),
        "{run}"
    );
    assert_eq!(h.count("SELECT count(*) FROM movies").await, 4);
    let years: Vec<Value> = all_items(&h, &id)
        .await
        .iter()
        .map(|i| i["year"].clone())
        .collect();
    assert_eq!(years, [json!(2019), Value::Null, Value::Null, Value::Null]);
    let keys: Vec<String> = all_items(&h, &id)
        .await
        .iter()
        .map(|i| i["key"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(keys, ["tmdb:9001", "tmdb:9002", "tmdb:9003", "tmdb:9004"]);
    assert_secret_hygiene(&h, &id, &[TMDB_TOKEN], &["access_token"]).await;
}

#[tokio::test]
async fn tmdb_failure_classes_malformed_documents_and_request_budget() {
    let h = Harness::new().await;
    let draft = tmdb_list(&h, json!({}));
    let route = "/3/movie/popular?page=1";
    assert_status_classes(&h, route, &draft, &[TMDB_TOKEN]).await;
    assert_invalid_bodies(
        &h,
        route,
        &draft,
        &[
            "garbage",
            "[]",
            "{}",
            "{\"results\": {}}",
            "{\"results\": [1]}",
            "{\"results\": [], \"total_pages\": \"3\"}",
            "{\"results\": [], \"total_pages\": -1}",
            &json!({"results": [{"id": "abc", "title": "A"}]}).to_string(),
            &json!({"results": [{"id": 0, "title": "A"}]}).to_string(),
            &json!({"results": [{"id": 1.5, "title": "A"}]}).to_string(),
            &json!({"results": [{"id": 1, "title": "A", "release_date": "soon"}]}).to_string(),
            &json!({"results": [{"id": 1, "title": "A", "release_date": 2019}]}).to_string(),
            &json!({"results": [{"id": 1, "title": "A", "release_date": "00-1-1"}]}).to_string(),
            &json!({"results": [{"id": 1, "title": "A", "media_type": 3}]}).to_string(),
        ],
    )
    .await;

    // 25 one-movie pages of a 100-page result never reach the limit: fail, do not truncate.
    for page in 1..=MAX_REQUESTS + 1 {
        h.feed.json(
            &format!("/3/movie/popular?page={page}"),
            tmdb_page(page as i64, 100, tmdb_results(1..=1)),
        );
    }
    h.feed.clear();
    assert_eq!(
        error_of(&h, &tmdb_list(&h, json!({"limit": 500}))).await,
        "resource_limit"
    );
    assert_eq!(h.feed.requests().len(), MAX_REQUESTS);
    // The same source with a limit it can meet succeeds.
    assert_eq!(count_of(&h, &tmdb_list(&h, json!({"limit": 1}))).await, 1);
}

#[tokio::test]
async fn tmdb_schema_domain_and_validation() {
    let h = Harness::new().await;
    let (_, schema) = h
        .call("GET", "/api/v1/importlist/schema", Value::Null)
        .await;
    let tmdb = schema["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["implementation"] == "tmdb")
        .expect("tmdb is available")
        .clone();
    assert_eq!(tmdb["domains"], json!(["movies"]));
    let token = tmdb["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "access_token")
        .unwrap()
        .clone();
    assert_eq!(
        (token["secret"].clone(), token["required"].clone()),
        (json!(true), json!(true))
    );
    let created = h
        .call("POST", "/api/v1/importlist", tmdb_list(&h, json!({})))
        .await
        .1;
    assert_eq!(
        created["settings"],
        json!({"list_type": "popular", "limit": 200})
    );
    for (label, body) in [
        (
            "tv domain",
            tmdb_list(&h, json!({}))
                .as_object()
                .cloned()
                .map(|mut o| {
                    o.insert("media_type".into(), json!("tv"));
                    Value::Object(o)
                })
                .unwrap(),
        ),
        ("no token", h.input("b", "tmdb", "movies", json!({}))),
        ("bad type", tmdb_list(&h, json!({"list_type": "trending"}))),
        (
            "target 0",
            tmdb_list(&h, json!({"list_type": "list", "target_id": 0})),
        ),
        ("limit 0", tmdb_list(&h, json!({"limit": 0}))),
        ("limit over budget", tmdb_list(&h, json!({"limit": 501}))),
    ] {
        let mut body = body;
        body["name"] = json!(label);
        assert_eq!(
            h.call("POST", "/api/v1/importlist", body).await.0,
            400,
            "{label}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// IMDb list export
// ---------------------------------------------------------------------------------------------

const IMDB_HEADER: &str = "Position,Const,Created,Modified,Description,Title,URL,Title Type,IMDb Rating,Runtime (mins),Year,Genres,Num Votes,Release Date,Directors";
fn imdb_row(position: usize, imdb: &str, title: &str, kind: &str, year: &str) -> String {
    format!(
        "{position},{imdb},2020-01-01,2020-01-02,,{title},https://www.imdb.com/title/{imdb}/,{kind},7.1,100,{year},\"Drama, Crime\",1000,2000-01-01,Someone"
    )
}
fn imdb_list(h: &Harness, list_id: &str) -> Value {
    h.input("IMDb", "imdb", "movies", json!({"list_id": list_id}))
}

#[tokio::test]
async fn imdb_export_csv_is_parsed_and_synced_with_quoting_bom_crlf_and_tv_rows_skipped() {
    let h = Harness::new().await;
    h.meta
        .movie(8001, "The Film", Some(1999), Some("tt0000001"));
    h.meta
        .movie(8002, "Say \"Hello\", Bob", Some(2005), Some("tt0000002"));
    h.meta
        .movie(8005, "A TV Movie", Some(2010), Some("tt0000005"));
    h.meta
        .movie(8006, "Two Lines", Some(2012), Some("tt0000006"));
    let csv = format!(
        "\u{feff}{IMDB_HEADER}\r\n{}\r\n{}\r\n{}\r\n{}\r\n{}\r\n{}\r\n\r\n",
        imdb_row(1, "tt0000001", "The Film", "Movie", "1999"),
        imdb_row(2, "tt0000002", "\"Say \"\"Hello\"\", Bob\"", "movie", ""),
        imdb_row(3, "tt0000003", "A Sitcom", "TV Series", "2001"),
        imdb_row(4, "tt0000004", "A Miniseries", "TV Mini Series", "2002"),
        imdb_row(5, "tt0000005", "A TV Movie", "TV Movie", "2010"),
        imdb_row(6, "tt0000006", "\"Two\nLines\"", "Video", "2012"),
    );
    h.feed.set("/list/ls012345678/export", 200, "text/csv", csv);
    let draft = imdb_list(&h, "ls012345678");
    assert_eq!(count_of(&h, &draft).await, 4);
    let req = &h.feed.requests()[0];
    assert_eq!(
        (req.method.as_str(), req.target.as_str()),
        ("GET", "/list/ls012345678/export")
    );
    assert!(req.header("accept").unwrap().contains("text/csv"));
    assert_eq!(
        req.header("authorization"),
        None,
        "no credentials are sent to IMDb"
    );

    let id = h.create(draft).await;
    let run = h.sync(&id).await;
    assert_eq!(
        (
            run["status"].as_str(),
            run["items_fetched"].as_i64(),
            run["added"].as_i64()
        ),
        (Some("succeeded"), Some(4), Some(4)),
        "{run}"
    );
    assert_eq!(h.count("SELECT count(*) FROM movies").await, 4);
    let items = all_items(&h, &id).await;
    let titles: Vec<&str> = items.iter().map(|i| i["title"].as_str().unwrap()).collect();
    assert_eq!(
        titles,
        ["The Film", "Say \"Hello\", Bob", "A TV Movie", "Two Lines"]
    );
    let years: Vec<Value> = items.iter().map(|i| i["year"].clone()).collect();
    assert_eq!(years, [json!(1999), Value::Null, json!(2010), json!(2012)]);
    let keys: Vec<&str> = items.iter().map(|i| i["key"].as_str().unwrap()).collect();
    assert_eq!(
        keys,
        [
            "imdb:tt0000001",
            "imdb:tt0000002",
            "imdb:tt0000005",
            "imdb:tt0000006"
        ]
    );
}

#[tokio::test]
async fn imdb_optional_columns_header_only_documents_and_column_order() {
    let h = Harness::new().await;
    let draft = imdb_list(&h, "ls1");
    // Only Title and Const are required, in any order and case; without Title Type nothing is skipped.
    h.feed.set(
        "/list/ls1/export",
        200,
        "text/csv",
        "title,const\nFirst,tt0000001\nSecond,tt0000002\n",
    );
    assert_eq!(count_of(&h, &draft).await, 2);
    // Header only: a valid, empty list.
    h.feed.set(
        "/list/ls1/export",
        200,
        "text/csv",
        format!("{IMDB_HEADER}\n"),
    );
    assert_eq!(count_of(&h, &draft).await, 0);
    // No trailing newline, duplicate rows collapse.
    h.feed.set(
        "/list/ls1/export",
        200,
        "text/csv",
        "Const,Title,Year\ntt0000001,One,2001\ntt0000001,One again,2001",
    );
    assert_eq!(count_of(&h, &draft).await, 1);
}

#[tokio::test]
async fn imdb_rejects_bad_list_ids_without_any_request_and_non_csv_documents() {
    let h = Harness::new().await;
    for bad in [
        "123",
        "ls",
        "lsabc",
        "LS123",
        "ls12x",
        "ls12/../x",
        "ls 1",
        &format!("ls{}", "1".repeat(21)),
    ] {
        assert_eq!(
            error_of(&h, &imdb_list(&h, bad)).await,
            "invalid_settings",
            "{bad}"
        );
    }
    assert!(h.feed.requests().is_empty());

    let draft = imdb_list(&h, "ls42");
    let route = "/list/ls42/export";
    let invalid: Vec<String> = vec![
        "<!DOCTYPE html><html><head><title>IMDb</title></head><body>Sign in</body></html>".into(),
        String::new(),
        "\n\n".into(),
        "Position,Title\n1,A\n".into(),
        "Position,Const\n1,tt0000001\n".into(),
        "Const,Title\n12345,Bad const\n".into(),
        "Const,Title\ntt12x,Bad const\n".into(),
        "Const,Title\n,Empty const\n".into(),
        "Const,Title\ntt0000001\n".into(),
        "Const,Title,Year\ntt0000001,A,soon\n".into(),
        "Const,Title,Year\ntt0000001,A,0\n".into(),
        "Const,Title\ntt0000001,\"unterminated\n".into(),
        "Const,Title\ntt0000001,\"quoted\"junk\n".into(),
        // A valid row does not excuse a later bad one.
        "Const,Title\ntt0000001,Fine\nnot-an-id,Broken\n".into(),
    ];
    for body in invalid {
        h.feed.set(route, 200, "text/csv", body.clone());
        assert_eq!(
            error_of(&h, &draft).await,
            "invalid_response",
            "body: {body:?}"
        );
    }
    h.feed
        .set(route, 200, "text/csv", vec![0xff, 0xfe, 0x00, 0x41]);
    assert_eq!(error_of(&h, &draft).await, "invalid_response", "not UTF-8");
    assert_status_classes(&h, route, &draft, &[]).await;
}

#[test]
fn csv_parser_follows_rfc_4180() {
    let rows = |s: &str| parse_csv(s).unwrap();
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    assert_eq!(rows("a,b\r\nc,d\r\n"), [s(&["a", "b"]), s(&["c", "d"])]);
    assert_eq!(rows("a,b\nc,d"), [s(&["a", "b"]), s(&["c", "d"])]);
    assert_eq!(rows("\u{feff}a,b\n"), [s(&["a", "b"])]);
    assert_eq!(rows("a,\"b,c\",d\n"), [s(&["a", "b,c", "d"])]);
    assert_eq!(
        rows("\"he said \"\"hi\"\"\",x\n"),
        [s(&["he said \"hi\"", "x"])]
    );
    assert_eq!(
        rows("\"line1\r\nline2\",x\n"),
        [s(&["line1\r\nline2", "x"])]
    );
    assert_eq!(rows("a,,c\n,\n"), [s(&["a", "", "c"]), s(&["", ""])]);
    assert_eq!(rows("a,b,\n"), [s(&["a", "b", ""])]);
    assert_eq!(rows("\"\",x\n"), [s(&["", "x"])]);
    assert_eq!(
        rows("a\n\n\nb\n"),
        [s(&["a"]), s(&["b"])],
        "blank lines carry no record"
    );
    assert_eq!(
        rows("say \"hi\" now,x\n"),
        [s(&["say \"hi\" now", "x"])],
        "a quote inside an unquoted field is literal"
    );
    assert!(rows("").is_empty());
    assert!(parse_csv("\"open,x\n").is_err());
    assert!(parse_csv("\"a\"b,c\n").is_err());
    assert!(
        parse_csv(&format!("{}\n", vec!["x"; 200].join(","))).is_err(),
        "column count is bounded"
    );
}

// ---------------------------------------------------------------------------------------------
// StevenLu
// ---------------------------------------------------------------------------------------------

const STEVENLU_ROUTE: &str = "/popular-movies/movies.json";

#[tokio::test]
async fn stevenlu_default_link_resolves_items_through_the_catalog() {
    let h = Harness::new().await;
    h.meta.movie(8101, "Movie A", Some(2020), Some("tt0100001"));
    h.meta.movie(8102, "Movie B", Some(2021), Some("tt0100002"));
    h.feed.json(
        STEVENLU_ROUTE,
        json!([
            {"title": "Movie A", "imdb_id": "tt0100001", "poster_url": "https://img.example/a.jpg", "genres": ["x"]},
            {"title": "Movie B", "imdb_id": "tt0100002"},
            {"title": "Titled Only", "imdb_id": ""},
            {"title": "Null Id", "imdb_id": null},
            {"title": "", "imdb_id": ""},
            {"poster_url": "https://img.example/orphan.jpg"}
        ]),
    );
    let draft = h.input("Steven", "stevenlu", "movies", json!({}));
    assert_eq!(
        count_of(&h, &draft).await,
        4,
        "entries with neither an id nor a title are skipped"
    );
    let req = &h.feed.requests()[0];
    assert_eq!(
        (req.method.as_str(), req.target.as_str()),
        ("GET", STEVENLU_ROUTE)
    );
    assert_eq!(req.header("accept"), Some("application/json"));

    let created = h.call("POST", "/api/v1/importlist", draft).await.1;
    assert_eq!(
        created["settings"]["link"],
        "https://s3.amazonaws.com/popular-movies/movies.json"
    );
    let id = created["id"].as_str().unwrap().to_owned();
    let run = h.sync(&id).await;
    assert_eq!(
        (
            run["status"].as_str(),
            run["items_fetched"].as_i64(),
            run["added"].as_i64(),
            run["unresolved"].as_i64()
        ),
        (Some("succeeded"), Some(4), Some(2), Some(2)),
        "{run}"
    );
    assert_eq!(h.count("SELECT count(*) FROM movies").await, 2);
    let items = all_items(&h, &id).await;
    assert_eq!(items[0]["imdb_id"], "tt0100001");
    assert!(
        items[0]["tmdb_id"].is_null()
            && items[0]["tvdb_id"].is_null()
            && items[0]["year"].is_null()
    );
    assert_eq!(items[0]["state"], "added");
    assert_eq!(
        items[2]["state"], "unresolved",
        "a title-only entry that the catalog cannot match stays unresolved"
    );
}

#[tokio::test]
async fn stevenlu_link_override_failure_classes_and_malformed_documents() {
    let h = Harness::new().await;
    h.feed.json(
        "/custom/movies.json",
        json!([{"title": "Only", "imdb_id": "tt0100009"}]),
    );
    let draft = h.input(
        "Mine",
        "stevenlu",
        "movies",
        json!({"link": format!("{}/custom/movies.json", h.feed_base)}),
    );
    assert_eq!(count_of(&h, &draft).await, 1);
    assert_eq!(targets(&h), ["/custom/movies.json"]);

    let default = h.input("Steven", "stevenlu", "movies", json!({}));
    assert_status_classes(&h, STEVENLU_ROUTE, &default, &[]).await;
    assert_invalid_bodies(
        &h,
        STEVENLU_ROUTE,
        &default,
        &[
            "nope",
            "",
            "{\"movies\": []}",
            "\"x\"",
            "[1]",
            "[null]",
            "[\"tt0100001\"]",
            &json!([{"title": "A", "imdb_id": "abc"}]).to_string(),
            &json!([{"title": "A", "imdb_id": "tt12ab"}]).to_string(),
            &json!([{"title": "A", "imdb_id": 100001}]).to_string(),
            &json!([{"title": "Fine", "imdb_id": "tt0100001"}, {"title": "Bad", "imdb_id": "nm0000001"}]).to_string(),
        ],
    )
    .await;
    h.feed.json(STEVENLU_ROUTE, json!([]));
    assert_eq!(count_of(&h, &default).await, 0);
}

#[tokio::test]
async fn stevenlu_schema_domain_and_link_validation() {
    let h = Harness::new().await;
    let (_, schema) = h
        .call("GET", "/api/v1/importlist/schema", Value::Null)
        .await;
    let steven = schema["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["implementation"] == "stevenlu")
        .expect("stevenlu is available")
        .clone();
    assert_eq!(steven["domains"], json!(["movies"]));
    for (label, body) in [
        ("tv domain", h.input("a", "stevenlu", "tv", json!({}))),
        (
            "link-local",
            h.input(
                "b",
                "stevenlu",
                "movies",
                json!({"link": "http://169.254.169.254/x"}),
            ),
        ),
        (
            "not a url",
            h.input(
                "c",
                "stevenlu",
                "movies",
                json!({"link": "ftp://example.com/x"}),
            ),
        ),
        (
            "credentials",
            h.input(
                "d",
                "stevenlu",
                "movies",
                json!({"link": "http://u:p@example.com/x"}),
            ),
        ),
    ] {
        assert_eq!(
            h.call("POST", "/api/v1/importlist", body).await.0,
            400,
            "{label}"
        );
    }
    // Movie-only providers are not offered for TV.
    for implementation in ["tmdb", "imdb"] {
        let body = h.input("tv", implementation, "tv", json!({"list_id": "ls1"}));
        assert_eq!(
            h.call("POST", "/api/v1/importlist", body).await.0,
            400,
            "{implementation}"
        );
    }
    // IMDb requires its list id; its id format is checked when fetching.
    assert_eq!(
        h.call(
            "POST",
            "/api/v1/importlist",
            h.input("i", "imdb", "movies", json!({}))
        )
        .await
        .0,
        400
    );
}
