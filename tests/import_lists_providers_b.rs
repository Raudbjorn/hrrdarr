//! Protocol tests for the Plex watchlist, MyAnimeList, AniList, Simkl and Sonarr/Radarr import-list
//! providers against the in-process fake list source: exact requests (path, query, headers, body),
//! paging and its bounds, parsing, failure classes, secret hygiene and end-to-end syncs. No real
//! network service is contacted.
mod import_lists_support;
use import_lists_support::Harness;
use serde_json::{Value, json};

const TEST_URL: &str = "/api/v1/importlist/test";
const MAX_REQUESTS: usize = 25;

fn draft(h: &Harness, imp: &str, media: &str, settings: Value, secrets: Value) -> Value {
    let mut v = h.input(
        &format!("{imp}-{}", uuid::Uuid::new_v4()),
        imp,
        media,
        settings,
    );
    v["secrets"] = secrets;
    v
}
/// The test endpoint's result for a draft.
async fn probe(h: &Harness, imp: &str, media: &str, settings: Value, secrets: Value) -> Value {
    let (status, body) = h
        .call(
            "POST",
            TEST_URL,
            json!({"draft": draft(h, imp, media, settings, secrets)}),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    body
}
async fn probe_error(
    h: &Harness,
    imp: &str,
    media: &str,
    settings: Value,
    secrets: Value,
) -> String {
    let r = probe(h, imp, media, settings, secrets).await;
    assert_eq!(r["ok"], false, "{r}");
    r["error"].as_str().unwrap().to_owned()
}
/// Syncs a list that never adds anything and returns its parsed items as
/// `[title, year, tvdb_id, tmdb_id, imdb_id]` rows in list order.
async fn snapshot(
    h: &Harness,
    imp: &str,
    media: &str,
    settings: Value,
    secrets: Value,
) -> Vec<Value> {
    let mut d = draft(h, imp, media, settings, secrets);
    d["enable_automatic_add"] = json!(false);
    let id = h.create(d).await;
    let run = h.sync(&id).await;
    assert_eq!(run["status"], "succeeded", "{run}");
    h.items(&id, "pending")
        .await
        .iter()
        .map(|i| {
            json!([
                i["title"],
                i["year"],
                i["tvdb_id"],
                i["tmdb_id"],
                i["imdb_id"]
            ])
        })
        .collect()
}
async fn titles(
    h: &Harness,
    imp: &str,
    media: &str,
    settings: Value,
    secrets: Value,
) -> Vec<String> {
    snapshot(h, imp, media, settings, secrets)
        .await
        .iter()
        .map(|r| r[0].as_str().unwrap().to_owned())
        .collect()
}

const PLEX_PATH: &str = "/library/sections/watchlist/all";
fn plex_target(start: usize) -> String {
    format!("{PLEX_PATH}?includeGuids=1&X-Plex-Container-Start={start}&X-Plex-Container-Size=100")
}
fn plex_page(total: i64, rows: Value) -> Value {
    json!({"MediaContainer": {"totalSize": total, "size": rows.as_array().unwrap().len(), "Metadata": rows}})
}

/// One representative, valid configuration per provider/domain with a body that yields one item.
struct Case {
    imp: &'static str,
    media: &'static str,
    settings: Value,
    secrets: Value,
    route: String,
    ok_body: Value,
    headers: Vec<(&'static str, &'static str)>,
    secret_values: Vec<&'static str>,
}
fn cases(h: &Harness) -> Vec<Case> {
    let base = h.feed_base.clone();
    vec![
        Case {
            imp: "plex",
            media: "tv",
            settings: json!({}),
            secrets: json!({"token": "SECRET-plex-token"}),
            route: PLEX_PATH.into(),
            ok_body: plex_page(
                1,
                json!([{"type":"show","title":"X","Guid":[{"id":"tvdb://5"}]}]),
            ),
            headers: vec![("x-plex-token", "SECRET-plex-token")],
            secret_values: vec!["SECRET-plex-token"],
        },
        Case {
            imp: "plex",
            media: "movies",
            settings: json!({}),
            secrets: json!({"token": "SECRET-plex-token"}),
            route: PLEX_PATH.into(),
            ok_body: plex_page(
                1,
                json!([{"type":"movie","title":"X","Guid":[{"id":"tmdb://5"}]}]),
            ),
            headers: vec![("x-plex-token", "SECRET-plex-token")],
            secret_values: vec!["SECRET-plex-token"],
        },
        Case {
            imp: "mal",
            media: "tv",
            settings: json!({"username": "alice"}),
            secrets: json!({"client_id": "SECRET-mal-client"}),
            route: "/v2/users/alice/animelist".into(),
            ok_body: json!({"data":[{"node":{"id":1,"title":"X"}}],"paging":{}}),
            headers: vec![("x-mal-client-id", "SECRET-mal-client")],
            secret_values: vec!["SECRET-mal-client"],
        },
        Case {
            imp: "anilist",
            media: "tv",
            settings: json!({"username": "alice"}),
            secrets: json!({}),
            route: "/".into(),
            ok_body: json!({"data":{"MediaListCollection":{"lists":[{"entries":[{"media":{"title":{"romaji":"X","english":null}}}]}]}}}),
            headers: vec![],
            secret_values: vec![],
        },
        Case {
            imp: "simkl",
            media: "tv",
            settings: json!({}),
            secrets: json!({"client_id": "SECRET-simkl-client", "access_token": "SECRET-simkl-access"}),
            route: "/sync/all-items/shows/plantowatch".into(),
            ok_body: json!({"shows":[{"show":{"title":"X","ids":{"tvdb":5}}}]}),
            headers: vec![
                ("simkl-api-key", "SECRET-simkl-client"),
                ("authorization", "Bearer SECRET-simkl-access"),
            ],
            secret_values: vec!["SECRET-simkl-client", "SECRET-simkl-access"],
        },
        Case {
            imp: "simkl",
            media: "movies",
            settings: json!({}),
            secrets: json!({"client_id": "SECRET-simkl-client", "access_token": "SECRET-simkl-access"}),
            route: "/sync/all-items/movies/plantowatch".into(),
            ok_body: json!({"movies":[{"movie":{"title":"X","ids":{"tmdb":5}}}]}),
            headers: vec![
                ("simkl-api-key", "SECRET-simkl-client"),
                ("authorization", "Bearer SECRET-simkl-access"),
            ],
            secret_values: vec!["SECRET-simkl-client", "SECRET-simkl-access"],
        },
        Case {
            imp: "sonarr",
            media: "tv",
            settings: json!({"url": base.clone()}),
            secrets: json!({"api_key": "SECRET-sonarr-key"}),
            route: "/api/v3/series".into(),
            ok_body: json!([{"tvdbId":5,"title":"X"}]),
            headers: vec![("x-api-key", "SECRET-sonarr-key")],
            secret_values: vec!["SECRET-sonarr-key"],
        },
        Case {
            imp: "radarr",
            media: "movies",
            settings: json!({"url": base}),
            secrets: json!({"api_key": "SECRET-radarr-key"}),
            route: "/api/v3/movie".into(),
            ok_body: json!([{"tmdbId":5,"title":"X"}]),
            headers: vec![("x-api-key", "SECRET-radarr-key")],
            secret_values: vec!["SECRET-radarr-key"],
        },
    ]
}
impl Case {
    fn label(&self) -> String {
        format!("{}/{}", self.imp, self.media)
    }
    async fn probe(&self, h: &Harness) -> Value {
        probe(
            h,
            self.imp,
            self.media,
            self.settings.clone(),
            self.secrets.clone(),
        )
        .await
    }
}

#[tokio::test]
async fn schema_declares_domains_secrets_defaults_and_intervals() {
    let h = Harness::new().await;
    let (_, schema) = h
        .call("GET", "/api/v1/importlist/schema", Value::Null)
        .await;
    let items = schema["items"].as_array().unwrap();
    let get = |name: &str| {
        items
            .iter()
            .find(|i| i["implementation"] == name)
            .unwrap_or_else(|| panic!("{name} missing"))
    };
    let field = |imp: &Value, name: &str| {
        imp["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["name"] == name)
            .unwrap_or_else(|| panic!("{name} missing"))
            .clone()
    };
    for (name, domains) in [
        ("plex", json!(["tv", "movies"])),
        ("mal", json!(["tv"])),
        ("anilist", json!(["tv"])),
        ("simkl", json!(["tv", "movies"])),
        ("sonarr", json!(["tv"])),
        ("radarr", json!(["movies"])),
    ] {
        assert_eq!(get(name)["domains"], domains, "{name}");
        assert_eq!(get(name)["default_interval_minutes"], 360, "{name}");
    }
    for (imp, name) in [
        ("plex", "token"),
        ("mal", "client_id"),
        ("simkl", "client_id"),
        ("simkl", "access_token"),
        ("sonarr", "api_key"),
        ("radarr", "api_key"),
    ] {
        let f = field(get(imp), name);
        assert_eq!(
            (f["secret"].clone(), f["required"].clone()),
            (json!(true), json!(true)),
            "{imp}.{name}"
        );
    }
    assert!(
        get("anilist")["fields"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["secret"] == false),
        "anilist has no secret"
    );
    assert_eq!(field(get("mal"), "status")["default"], "watching");
    assert_eq!(
        field(get("mal"), "status")["options"],
        json!([
            "all",
            "watching",
            "completed",
            "on_hold",
            "dropped",
            "plan_to_watch"
        ])
    );
    assert_eq!(field(get("anilist"), "status")["default"], "current");
    assert_eq!(field(get("simkl"), "list_status")["default"], "plantowatch");
    assert_eq!(
        (
            get("sonarr")["min_interval_minutes"].clone(),
            get("radarr")["min_interval_minutes"].clone()
        ),
        (json!(5), json!(15))
    );
    assert_eq!(get("sonarr")["label"], "Sonarr (another instance)");
    assert_eq!(get("radarr")["label"], "Radarr (another instance)");

    // Domains are enforced on create; intervals follow each implementation's minimum.
    let url = json!({"url": h.feed_base});
    let key = json!({"api_key": "k"});
    for (imp, media, settings, secrets) in [
        (
            "mal",
            "movies",
            json!({"username": "a"}),
            json!({"client_id": "c"}),
        ),
        ("anilist", "movies", json!({"username": "a"}), json!({})),
        ("sonarr", "movies", url.clone(), key.clone()),
        ("radarr", "tv", url.clone(), key.clone()),
    ] {
        let (status, body) = h
            .call(
                "POST",
                "/api/v1/importlist",
                draft(&h, imp, media, settings, secrets),
            )
            .await;
        assert_eq!(status, 400, "{imp}/{media}: {body}");
    }
    for (imp, media, interval, expected) in [
        ("sonarr", "tv", 4, 400),
        ("sonarr", "tv", 5, 201),
        ("radarr", "movies", 14, 400),
        ("radarr", "movies", 15, 201),
    ] {
        let mut d = draft(&h, imp, media, url.clone(), key.clone());
        d["interval_minutes"] = json!(interval);
        assert_eq!(
            h.call("POST", "/api/v1/importlist", d).await.0,
            expected,
            "{imp} {interval}"
        );
    }
    // Required secrets and settings, and select options, are validated.
    let missing_secret = draft(&h, "plex", "tv", json!({}), json!({}));
    assert_eq!(
        h.call("POST", "/api/v1/importlist", missing_secret).await.0,
        400
    );
    let missing_user = draft(&h, "mal", "tv", json!({}), json!({"client_id": "c"}));
    assert_eq!(
        h.call("POST", "/api/v1/importlist", missing_user).await.0,
        400
    );
    let bad_status = draft(
        &h,
        "mal",
        "tv",
        json!({"username": "a", "status": "nope"}),
        json!({"client_id": "c"}),
    );
    assert_eq!(
        h.call("POST", "/api/v1/importlist", bad_status).await.0,
        400
    );
    let bad_simkl = draft(
        &h,
        "simkl",
        "tv",
        json!({"list_status": "watchlist"}),
        json!({"client_id": "c", "access_token": "t"}),
    );
    assert_eq!(h.call("POST", "/api/v1/importlist", bad_simkl).await.0, 400);
    // The defaults are stored when the select is omitted.
    let created = h
        .create(draft(
            &h,
            "mal",
            "tv",
            json!({"username": "zed"}),
            json!({"client_id": "c"}),
        ))
        .await;
    let (_, list) = h
        .call("GET", &format!("/api/v1/importlist/{created}"), Value::Null)
        .await;
    assert_eq!(list["settings"]["status"], "watching");
}

#[tokio::test]
async fn every_provider_maps_http_failures_and_malformed_documents_to_stable_classes() {
    let h = Harness::new().await;
    for case in cases(&h) {
        let label = case.label();
        for (status, code) in [
            (401, "authentication"),
            (403, "authentication"),
            (429, "rate_limited"),
            (500, "server_error"),
            (503, "server_error"),
            (404, "rejected"),
            (302, "redirect"),
        ] {
            h.feed
                .set(&case.route, status, "text/plain", "remote said no");
            let r = case.probe(&h).await;
            assert_eq!(
                (r["ok"].clone(), r["error"].clone()),
                (json!(false), json!(code)),
                "{label} {status}"
            );
        }
        for (what, body) in [
            ("html", "<html>"),
            ("json string", "\"hello\""),
            ("scalar array", "[1]"),
            ("truncated", "{\"data\":["),
        ] {
            h.feed.set(&case.route, 200, "application/json", body);
            assert_eq!(
                case.probe(&h).await["error"],
                "invalid_response",
                "{label} {what}"
            );
        }
        // A body above the response cap fails rather than being truncated.
        h.feed.set(
            &case.route,
            200,
            "application/json",
            vec![b' '; 5 * 1024 * 1024],
        );
        assert_eq!(
            case.probe(&h).await["error"],
            "resource_limit",
            "{label} oversized"
        );
        // The valid document is accepted afterwards (no sticky state).
        h.feed.json(&case.route, case.ok_body.clone());
        let r = case.probe(&h).await;
        assert_eq!(
            (r["ok"].clone(), r["item_count"].clone()),
            (json!(true), json!(1)),
            "{label}: {r}"
        );
    }
    // Rate limiting is retryable: the run is requeued with the stable class.
    let case = &cases(&h)[2];
    h.feed.set(&case.route, 429, "text/plain", "slow down");
    let id = h
        .create(draft(
            &h,
            case.imp,
            case.media,
            case.settings.clone(),
            case.secrets.clone(),
        ))
        .await;
    h.call(
        "POST",
        &format!("/api/v1/importlist/{id}/sync"),
        Value::Null,
    )
    .await;
    h.engine.drain().await.unwrap();
    let (_, runs) = h
        .call("GET", &format!("/api/v1/importlist/{id}/runs"), Value::Null)
        .await;
    assert_eq!(
        (
            runs["items"][0]["status"].as_str(),
            runs["items"][0]["error_code"].as_str()
        ),
        (Some("queued"), Some("rate_limited"))
    );
}

#[tokio::test]
async fn secrets_travel_only_in_headers_and_never_reach_storage_responses_or_errors() {
    let h = Harness::new().await;
    for case in cases(&h) {
        let label = case.label();
        h.feed.json(&case.route, case.ok_body.clone());
        h.feed.clear();
        let mut input = draft(
            &h,
            case.imp,
            case.media,
            case.settings.clone(),
            case.secrets.clone(),
        );
        let (status, created) = h.call("POST", "/api/v1/importlist", input.clone()).await;
        assert_eq!(status, 201, "{label}: {created}");
        let id = created["id"].as_str().unwrap().to_owned();
        let mut configured: Vec<String> =
            case.secrets.as_object().unwrap().keys().cloned().collect();
        configured.sort();
        assert_eq!(created["secrets_configured"], json!(configured), "{label}");
        let (_, listed) = h.call("GET", "/api/v1/importlist", Value::Null).await;
        let (_, detail) = h
            .call("GET", &format!("/api/v1/importlist/{id}"), Value::Null)
            .await;
        // A draft test works and reports a count only.
        input["name"] = json!(format!("{label} draft"));
        let (_, tested) = h.call("POST", TEST_URL, json!({"draft": input})).await;
        assert_eq!(
            (tested["ok"].clone(), tested["item_count"].clone()),
            (json!(true), json!(1)),
            "{label}: {tested}"
        );
        let (_, saved) = h
            .call(
                "POST",
                &format!("/api/v1/importlist/{id}/test"),
                Value::Null,
            )
            .await;
        assert_eq!(saved["ok"], true, "{label}: {saved}");
        // A failing remote that echoes the secrets back must not leak them through any response.
        let echo = format!("echo {} {}", case.secret_values.join(" "), h.feed_base);
        h.feed.set(&case.route, 500, "text/plain", echo);
        let failed = case.probe(&h).await;
        let run = h.sync(&id).await;
        let (_, after) = h
            .call("GET", &format!("/api/v1/importlist/{id}"), Value::Null)
            .await;
        let wire = [
            &created, &listed, &detail, &tested, &saved, &failed, &run, &after,
        ]
        .map(Value::to_string)
        .join("\n");
        for secret in &case.secret_values {
            assert!(!wire.contains(secret), "{label}: {secret} leaked");
            assert_eq!(
                h.count(&format!(
                    "SELECT count(*) FROM import_lists WHERE settings_json LIKE '%{secret}%'"
                ))
                .await,
                0,
                "{label}"
            );
            assert_eq!(
                h.count(&format!(
                    "SELECT count(*) FROM import_lists WHERE instr(credentials,'{secret}')>0"
                ))
                .await,
                0,
                "{label}: sealed"
            );
            assert_eq!(h.count(&format!("SELECT count(*) FROM import_list_runs WHERE instr(coalesce(error_code,''),'{secret}')>0")).await, 0, "{label}");
        }
        assert!(
            !wire.contains(&h.feed_base) || case.imp == "sonarr" || case.imp == "radarr",
            "{label}: remote URL echoed"
        );
        // On the wire the secrets appear in headers only, never in the URL or body.
        let seen = h.feed.requests();
        assert!(!seen.is_empty(), "{label}");
        for request in &seen {
            for secret in &case.secret_values {
                assert!(
                    !request.target.contains(secret) && !request.body.contains(secret),
                    "{label}: {secret} in URL or body"
                );
            }
            for (name, value) in &case.headers {
                assert_eq!(request.header(name), Some(*value), "{label}: {name}");
            }
        }
    }
}

// ---------------------------------------------------------------------------------------- Plex

#[tokio::test]
async fn plex_pages_by_container_start_and_keeps_the_lists_domain() {
    let h = Harness::new().await;
    h.feed.json(
        &plex_target(0),
        plex_page(
            4,
            json!([
                {"title": "Alpha", "year": 2019, "type": "show", "Guid": [{"id": "imdb://tt0000001"}, {"id": "tmdb://11"}, {"id": "tvdb://101"}, {"id": "plex://show/5d77"}]},
                {"title": "Film", "year": 2020, "type": "movie", "Guid": [{"id": "imdb://tt0000002"}, {"id": "tmdb://21"}, {"id": "tvdb://999"}]},
            ]),
        ),
    );
    h.feed.json(
        &plex_target(2),
        plex_page(
            4,
            json!([
                {"title": "Beta", "type": "show", "Guid": [{"id": "tvdb://102"}]},
                {"title": "Episode Thing", "type": "episode", "Guid": [{"id": "tvdb://5"}]},
            ]),
        ),
    );
    h.meta.show(101, "Alpha", Some(2019));
    h.meta.show(102, "Beta", None);
    h.meta.movie(21, "Film", Some(2020), Some("tt0000002"));
    let secrets = json!({"token": "tok-plex-1"});

    let rows = snapshot(&h, "plex", "tv", json!({}), secrets.clone()).await;
    assert_eq!(
        rows,
        vec![
            json!(["Alpha", 2019, 101, null, "tt0000001"]),
            json!(["Beta", null, 102, null, null])
        ]
    );
    let seen = h.feed.requests();
    assert_eq!(
        seen.iter().map(|r| r.target.as_str()).collect::<Vec<_>>(),
        vec![plex_target(0), plex_target(2)]
    );
    for r in &seen {
        assert_eq!(r.method, "GET");
        assert_eq!(r.header("x-plex-token"), Some("tok-plex-1"));
        assert_eq!(r.header("accept"), Some("application/json"));
        assert_eq!(r.header("x-plex-client-identifier"), Some("hrrdarr"));
        assert_eq!(r.header("x-plex-product"), Some("hrrdarr"));
        assert!(!r.target.contains("tok-plex-1") && r.body.is_empty());
    }
    // The movie list sees only movies, keeps the TMDb id and drops the TVDB one.
    let rows = snapshot(&h, "plex", "movies", json!({}), secrets.clone()).await;
    assert_eq!(rows, vec![json!(["Film", 2020, null, 21, "tt0000002"])]);

    // End to end: the shows are added to the library through the shared add path.
    let tv = h
        .create(draft(&h, "plex", "tv", json!({}), secrets.clone()))
        .await;
    let run = h.sync(&tv).await;
    assert_eq!(
        (
            run["status"].as_str(),
            run["items_fetched"].as_i64(),
            run["added"].as_i64()
        ),
        (Some("succeeded"), Some(2), Some(2)),
        "{run}"
    );
    assert_eq!(h.count("SELECT count(*) FROM series").await, 2);
    let movies = h
        .create(draft(&h, "plex", "movies", json!({}), secrets))
        .await;
    let run = h.sync(&movies).await;
    assert_eq!(run["added"], 1, "{run}");
    assert_eq!(h.count("SELECT count(*) FROM movies m JOIN movie_metadata d ON d.id=m.metadata_id WHERE d.tmdb_id=21").await, 1);
}

#[tokio::test]
async fn plex_paging_stops_on_total_empty_page_or_the_request_bound() {
    let h = Harness::new().await;
    let secrets = json!({"token": "t"});
    // No Metadata at all is a valid, empty watchlist (one request).
    h.feed.json(
        &plex_target(0),
        json!({"MediaContainer": {"totalSize": 0, "size": 0}}),
    );
    let r = probe(&h, "plex", "tv", json!({}), secrets.clone()).await;
    assert_eq!(
        (r["ok"].clone(), r["item_count"].clone()),
        (json!(true), json!(0))
    );
    assert_eq!(h.feed.requests().len(), 1);
    // Without totalSize the walk ends at the first empty page.
    h.feed.clear();
    h.feed.json(&plex_target(0), json!({"MediaContainer": {"size": 1, "Metadata": [{"type": "show", "title": "One", "Guid": [{"id": "tvdb://1"}]}]}}));
    h.feed
        .json(&plex_target(1), json!({"MediaContainer": {"size": 0}}));
    let r = probe(&h, "plex", "tv", json!({}), secrets.clone()).await;
    assert_eq!(r["item_count"], 1);
    assert_eq!(h.feed.requests().len(), 2);
    // A non-ending source (always more to read) fails with resource_limit after exactly MAX_REQUESTS.
    let h = Harness::new().await;
    h.feed.json(
        PLEX_PATH,
        plex_page(
            1_000_000,
            json!([{"type": "show", "title": "Loop", "Guid": [{"id": "tvdb://7"}]}]),
        ),
    );
    assert_eq!(
        probe_error(&h, "plex", "tv", json!({}), secrets.clone()).await,
        "resource_limit"
    );
    let seen = h.feed.requests();
    assert_eq!(seen.len(), MAX_REQUESTS);
    assert_eq!(seen[MAX_REQUESTS - 1].target, plex_target(MAX_REQUESTS - 1));
    // A failure on a later page fails the whole fetch: no partial list is synced.
    h.feed.json(
        &plex_target(0),
        plex_page(
            3,
            json!([{"type": "show", "title": "A", "Guid": [{"id": "tvdb://1"}]}]),
        ),
    );
    h.feed.set(&plex_target(1), 503, "text/plain", "");
    let id = h.create(draft(&h, "plex", "tv", json!({}), secrets)).await;
    h.call(
        "POST",
        &format!("/api/v1/importlist/{id}/sync"),
        Value::Null,
    )
    .await;
    h.engine.drain().await.unwrap();
    assert_eq!(h.count("SELECT count(*) FROM import_list_items").await, 0);
}

#[tokio::test]
async fn plex_ignores_unknown_guid_schemes_but_rejects_malformed_ids_and_shapes() {
    let h = Harness::new().await;
    let secrets = json!({"token": "t"});
    let row = |guid: Value| plex_page(1, json!([{"type": "show", "title": "X", "Guid": guid}]));
    // Unknown schemes, a GUID without a scheme and a missing Guid list are all fine.
    for ok in [
        json!([{"id": "plex://show/abc"}, {"id": "tvdb://5"}]),
        json!([{"id": "nonsense"}, {"id": "tvdb://5"}]),
        json!([{"id": "tvdb://5"}, {"id": "imdb://tt1234567"}]),
    ] {
        h.feed.json(PLEX_PATH, row(ok.clone()));
        assert_eq!(
            probe(&h, "plex", "tv", json!({}), secrets.clone()).await["item_count"],
            1,
            "{ok}"
        );
    }
    h.feed.json(
        PLEX_PATH,
        plex_page(1, json!([{"type": "show", "title": "No Guid"}])),
    );
    assert_eq!(
        probe(&h, "plex", "tv", json!({}), secrets.clone()).await["item_count"],
        1,
        "title-only item"
    );
    for (what, body) in [
        ("non-numeric tvdb", row(json!([{"id": "tvdb://abc"}]))),
        ("zero tvdb", row(json!([{"id": "tvdb://0"}]))),
        ("negative tmdb", row(json!([{"id": "tmdb://-4"}]))),
        ("bad imdb", row(json!([{"id": "imdb://nm0000001"}]))),
        ("guid without id", row(json!([{"name": "x"}]))),
        ("guid not an array", row(json!({"id": "tvdb://1"}))),
        ("no container", json!({"Metadata": []})),
        (
            "Metadata an object",
            json!({"MediaContainer": {"Metadata": {"title": "x"}}}),
        ),
        (
            "row not an object",
            json!({"MediaContainer": {"Metadata": ["x"]}}),
        ),
        (
            "totalSize a string",
            json!({"MediaContainer": {"totalSize": "3", "Metadata": []}}),
        ),
        (
            "negative totalSize",
            json!({"MediaContainer": {"totalSize": -1, "Metadata": []}}),
        ),
        (
            "year not a number",
            json!({"MediaContainer": {"Metadata": [{"type": "show", "title": "x", "year": "soon"}]}}),
        ),
    ] {
        h.feed.json(PLEX_PATH, body);
        assert_eq!(
            probe_error(&h, "plex", "tv", json!({}), secrets.clone()).await,
            "invalid_response",
            "{what}"
        );
    }
    // A malformed row of the other kind is still a corrupt document only when it is of this kind.
    h.feed.json(
        PLEX_PATH,
        plex_page(
            1,
            json!([{"type": "movie", "title": "M", "Guid": [{"id": "tmdb://abc"}]}]),
        ),
    );
    assert_eq!(
        probe_error(&h, "plex", "movies", json!({}), secrets.clone()).await,
        "invalid_response"
    );
    assert_eq!(
        probe(&h, "plex", "tv", json!({}), secrets).await["item_count"],
        0,
        "movie rows are skipped on a TV list"
    );
}

// ----------------------------------------------------------------------------------------- MAL

fn mal_target(user: &str, offset: usize, status: Option<&str>) -> String {
    let mut t = format!("/v2/users/{user}/animelist?limit=100&offset={offset}&fields=start_season");
    if let Some(s) = status {
        t.push_str(&format!("&status={s}"));
    }
    t
}

#[tokio::test]
async fn mal_follows_paging_next_by_offset_and_resolves_titles_end_to_end() {
    let h = Harness::new().await;
    h.feed.json(
        &mal_target("alice", 0, Some("watching")),
        json!({"data": [
            {"node": {"id": 1, "title": "Cowboy Bebop", "start_season": {"year": 1998, "season": "spring"}}},
            {"node": {"id": 2, "title": "No Season"}},
        ], "paging": {"next": "https://api.myanimelist.net/v2/users/alice/animelist?offset=999&limit=100"}}),
    );
    h.feed.json(
        &mal_target("alice", 2, Some("watching")),
        json!({"data": [{"node": {"id": 3, "title": "Trigun", "start_season": null}, "list_status": {"status": "watching"}}], "paging": {}}),
    );
    let settings = json!({"username": "alice"});
    let secrets = json!({"client_id": "mal-client"});
    let rows = snapshot(&h, "mal", "tv", settings.clone(), secrets.clone()).await;
    assert_eq!(
        rows,
        vec![
            json!(["Cowboy Bebop", 1998, null, null, null]),
            json!(["No Season", null, null, null, null]),
            json!(["Trigun", null, null, null, null])
        ]
    );
    let seen = h.feed.requests();
    // The next link MAL announces is never fetched; the offset advances by the rows received.
    assert_eq!(
        seen.iter().map(|r| r.target.as_str()).collect::<Vec<_>>(),
        vec![
            mal_target("alice", 0, Some("watching")),
            mal_target("alice", 2, Some("watching"))
        ]
    );
    for r in &seen {
        assert_eq!(
            (
                r.method.as_str(),
                r.header("x-mal-client-id"),
                r.header("accept")
            ),
            ("GET", Some("mal-client"), Some("application/json"))
        );
        assert!(!r.target.contains("mal-client") && r.header("authorization").is_none());
    }

    // End to end: MAL ids are not TVDB ids, so the shows are found by exact title.
    h.meta.show(301, "Cowboy Bebop", Some(1998));
    h.meta.show(302, "No Season", Some(2001));
    h.meta.show(303, "Trigun", Some(1998));
    let id = h.create(draft(&h, "mal", "tv", settings, secrets)).await;
    let run = h.sync(&id).await;
    assert_eq!(
        (
            run["status"].as_str(),
            run["items_fetched"].as_i64(),
            run["added"].as_i64(),
            run["unresolved"].as_i64()
        ),
        (Some("succeeded"), Some(3), Some(3), Some(0)),
        "{run}"
    );
    assert_eq!(h.count("SELECT count(*) FROM series").await, 3);
}

#[tokio::test]
async fn mal_status_filter_username_encoding_and_empty_pages() {
    let h = Harness::new().await;
    let secrets = json!({"client_id": "c"});
    let empty = json!({"data": [], "paging": {}});
    // `all` omits the status parameter; named statuses are sent verbatim.
    h.feed.json(&mal_target("alice", 0, None), empty.clone());
    let r = probe(
        &h,
        "mal",
        "tv",
        json!({"username": "alice", "status": "all"}),
        secrets.clone(),
    )
    .await;
    assert_eq!(
        (r["ok"].clone(), r["item_count"].clone()),
        (json!(true), json!(0))
    );
    for status in ["completed", "on_hold", "dropped", "plan_to_watch"] {
        h.feed
            .json(&mal_target("alice", 0, Some(status)), empty.clone());
        assert_eq!(
            probe(
                &h,
                "mal",
                "tv",
                json!({"username": "alice", "status": status}),
                secrets.clone()
            )
            .await["ok"],
            true,
            "{status}"
        );
    }
    assert_eq!(h.feed.requests().len(), 5);
    assert!(
        h.feed.requests()[0].target.ends_with("fields=start_season")
            && !h.feed.requests()[0].target.contains("status")
    );
    // The user name is one percent-encoded path segment: it cannot add path or query components.
    h.feed.clear();
    let r = probe(
        &h,
        "mal",
        "tv",
        json!({"username": "a b/../c?x=1#"}),
        secrets.clone(),
    )
    .await;
    assert_eq!(r["error"], "rejected", "unknown route answers 404: {r}");
    let target = &h.feed.requests()[0].target;
    assert_eq!(
        target,
        "/v2/users/a%20b%2F..%2Fc%3Fx=1%23/animelist?limit=100&offset=0&fields=start_season&status=watching"
    );
    h.feed.clear();
    assert_eq!(
        probe_error(&h, "mal", "tv", json!({"username": ".."}), secrets.clone()).await,
        "invalid_settings"
    );
    assert!(
        h.feed.requests().is_empty(),
        "nothing is sent for an invalid user name"
    );
    // An entry without a title or with a null id is not an identity; the fetch still succeeds.
    h.feed.json(
        &mal_target("bob", 0, Some("watching")),
        json!({"data": [{"node": {"id": 5}}, {"node": {"id": null, "title": "Kept"}}]}),
    );
    assert_eq!(
        probe(&h, "mal", "tv", json!({"username": "bob"}), secrets).await["item_count"],
        1
    );
}

#[tokio::test]
async fn mal_rejects_malformed_documents_and_bounds_paging() {
    let h = Harness::new().await;
    let secrets = json!({"client_id": "c"});
    let settings = json!({"username": "alice"});
    let route = "/v2/users/alice/animelist";
    for (what, body) in [
        ("array", json!([])),
        ("no data", json!({"paging": {}})),
        ("data an object", json!({"data": {}})),
        ("entry without node", json!({"data": [{"list_status": {}}]})),
        ("node a string", json!({"data": [{"node": "x"}]})),
        (
            "id a string",
            json!({"data": [{"node": {"id": "abc", "title": "x"}}]}),
        ),
        (
            "id negative",
            json!({"data": [{"node": {"id": -1, "title": "x"}}]}),
        ),
        (
            "start_season a string",
            json!({"data": [{"node": {"id": 1, "title": "x", "start_season": "2020"}}]}),
        ),
        (
            "year zero",
            json!({"data": [{"node": {"id": 1, "title": "x", "start_season": {"year": 0}}}]}),
        ),
        ("paging a string", json!({"data": [], "paging": "next"})),
        ("next a number", json!({"data": [], "paging": {"next": 5}})),
        (
            "next after an empty page",
            json!({"data": [], "paging": {"next": "https://example.test/x"}}),
        ),
    ] {
        h.feed.json(route, body);
        assert_eq!(
            probe_error(&h, "mal", "tv", settings.clone(), secrets.clone()).await,
            "invalid_response",
            "{what}"
        );
    }
    // Always another page: stop after MAX_REQUESTS with resource_limit, not a truncated list.
    h.feed.clear();
    h.feed.json(route, json!({"data": [{"node": {"id": 1, "title": "Loop"}}], "paging": {"next": "https://example.test/x"}}));
    assert_eq!(
        probe_error(&h, "mal", "tv", settings, secrets).await,
        "resource_limit"
    );
    let seen = h.feed.requests();
    assert_eq!(seen.len(), MAX_REQUESTS);
    assert_eq!(
        seen[MAX_REQUESTS - 1].target,
        mal_target("alice", MAX_REQUESTS - 1, Some("watching"))
    );
}

// ------------------------------------------------------------------------------------- AniList

fn anilist_entry(romaji: &str, english: Value, year: Value, format: &str) -> Value {
    json!({"media": {"title": {"romaji": romaji, "english": english}, "startDate": {"year": year}, "format": format}})
}
fn anilist_doc(lists: Value) -> Value {
    json!({"data": {"MediaListCollection": {"lists": lists}}})
}

#[tokio::test]
async fn anilist_posts_a_graphql_query_prefers_english_titles_and_dedupes_lists() {
    let h = Harness::new().await;
    h.feed.json(
        "/",
        anilist_doc(json!([
            {"entries": [
                anilist_entry("Shingeki no Kyojin", json!("Attack on Titan"), json!(2013), "TV"),
                anilist_entry("Only Romaji", Value::Null, Value::Null, "TV_SHORT"),
                anilist_entry("A Movie", json!("A Movie"), json!(2016), "MOVIE"),
                anilist_entry("A Song", Value::Null, json!(2016), "MUSIC"),
                json!({"media": null}),
            ]},
            {"entries": [anilist_entry("Shingeki no Kyojin", json!("Attack on Titan"), json!(2013), "TV")]},
        ])),
    );
    let settings = json!({"username": "alice"});
    let rows = snapshot(&h, "anilist", "tv", settings.clone(), json!({})).await;
    assert_eq!(
        rows,
        vec![
            json!(["Attack on Titan", 2013, null, null, null]),
            json!(["Only Romaji", null, null, null, null])
        ]
    );
    let seen = h.feed.requests();
    assert_eq!(seen.len(), 1);
    let r = &seen[0];
    assert_eq!((r.method.as_str(), r.target.as_str()), ("POST", "/"));
    assert_eq!(r.header("accept"), Some("application/json"));
    assert!(
        r.header("content-type")
            .unwrap()
            .starts_with("application/json")
    );
    assert!(
        r.header("authorization").is_none() && r.header("x-api-key").is_none(),
        "no authentication is sent"
    );
    let body = r.json();
    assert_eq!(
        body["variables"],
        json!({"userName": "alice", "status": "CURRENT"})
    );
    let query = body["query"].as_str().unwrap();
    assert!(
        query.contains("MediaListCollection(userName: $userName, type: ANIME, status: $status)"),
        "{query}"
    );
    assert!(
        query.contains("$status: MediaListStatus")
            && query.contains("title { romaji english }")
            && query.contains("startDate { year }"),
        "{query}"
    );
    assert_eq!(body.as_object().unwrap().len(), 2);

    // `all` uses the variant without the status variable; other statuses map to the enum value.
    h.feed.clear();
    probe(
        &h,
        "anilist",
        "tv",
        json!({"username": "alice", "status": "all"}),
        json!({}),
    )
    .await;
    probe(
        &h,
        "anilist",
        "tv",
        json!({"username": "alice", "status": "repeating"}),
        json!({}),
    )
    .await;
    probe(
        &h,
        "anilist",
        "tv",
        json!({"username": "alice", "status": "planning"}),
        json!({}),
    )
    .await;
    let seen = h.feed.requests();
    let all = seen[0].json();
    assert_eq!(all["variables"], json!({"userName": "alice"}));
    assert!(!all["query"].as_str().unwrap().contains("status"));
    assert!(
        all["query"]
            .as_str()
            .unwrap()
            .contains("MediaListCollection(userName: $userName, type: ANIME)")
    );
    assert_eq!(seen[1].json()["variables"]["status"], "REPEATING");
    assert_eq!(seen[2].json()["variables"]["status"], "PLANNING");

    // End to end: resolved by exact (English) title.
    h.meta.show(401, "Attack on Titan", Some(2013));
    h.meta.show(402, "Only Romaji", None);
    let id = h
        .create(draft(&h, "anilist", "tv", settings, json!({})))
        .await;
    let run = h.sync(&id).await;
    assert_eq!(
        (
            run["status"].as_str(),
            run["items_fetched"].as_i64(),
            run["added"].as_i64()
        ),
        (Some("succeeded"), Some(2), Some(2)),
        "{run}"
    );
    assert_eq!(h.count("SELECT count(*) FROM series").await, 2);
}

#[tokio::test]
async fn anilist_graphql_errors_unknown_users_shapes_and_caps() {
    let h = Harness::new().await;
    let settings = json!({"username": "ghost"});
    let run = |body: Value| {
        h.feed.json("/", body);
        probe_error(&h, "anilist", "tv", settings.clone(), json!({}))
    };
    // A GraphQL error, a null collection (unknown or private user) and a missing data member are refusals.
    assert_eq!(run(json!({"errors": [{"message": "User not found", "status": 404}], "data": {"MediaListCollection": null}})).await, "rejected");
    assert_eq!(
        run(json!({"data": {"MediaListCollection": null}})).await,
        "rejected"
    );
    assert_eq!(run(json!({"data": null})).await, "rejected");
    assert_eq!(
        run(
            json!({"errors": [{"message": "boom"}], "data": {"MediaListCollection": {"lists": []}}})
        )
        .await,
        "rejected"
    );
    for (what, body) in [
        ("array", json!([])),
        ("data a string", json!({"data": "x"})),
        (
            "collection an array",
            json!({"data": {"MediaListCollection": []}}),
        ),
        (
            "lists an object",
            json!({"data": {"MediaListCollection": {"lists": {}}}}),
        ),
        ("entries an object", anilist_doc(json!([{"entries": {}}]))),
        (
            "media a string",
            anilist_doc(json!([{"entries": [{"media": "x"}]}])),
        ),
        (
            "entry without media",
            anilist_doc(json!([{"entries": [{}]}])),
        ),
        (
            "title a string",
            anilist_doc(json!([{"entries": [{"media": {"title": "x"}}]}])),
        ),
        (
            "year a string",
            anilist_doc(
                json!([{"entries": [anilist_entry("A", Value::Null, json!("abc"), "TV")]}]),
            ),
        ),
        (
            "year negative",
            anilist_doc(json!([{"entries": [anilist_entry("A", Value::Null, json!(-1), "TV")]}])),
        ),
        (
            "format a number",
            anilist_doc(json!([{"entries": [{"media": {"title": {"romaji": "A"}, "format": 3}}]}])),
        ),
        (
            "startDate a string",
            anilist_doc(
                json!([{"entries": [{"media": {"title": {"romaji": "A"}, "startDate": "2020"}}]}]),
            ),
        ),
    ] {
        assert_eq!(run(body).await, "invalid_response", "{what}");
    }
    // Empty and sparse collections are valid.
    h.feed.json("/", anilist_doc(json!([])));
    assert_eq!(
        probe(&h, "anilist", "tv", settings.clone(), json!({})).await["item_count"],
        0
    );
    h.feed.json(
        "/",
        json!({"data": {"MediaListCollection": {"lists": [{"entries": null}, {}]}}}),
    );
    assert_eq!(
        probe(&h, "anilist", "tv", settings.clone(), json!({})).await["item_count"],
        0
    );
    // Defensive caps on list and entry counts fail the fetch instead of truncating.
    assert_eq!(
        run(anilist_doc(Value::Array(
            (0..65).map(|_| json!({"entries": []})).collect()
        )))
        .await,
        "resource_limit"
    );
    let many = Value::Array((0..20_001).map(|_| json!({"media": null})).collect());
    assert_eq!(
        run(anilist_doc(json!([{"entries": many}]))).await,
        "resource_limit"
    );
    // More distinct items than a list may hold also fails (no silent truncation).
    let entries = Value::Array(
        (0..5001)
            .map(|i| json!({"media": {"title": {"romaji": format!("T{i}")}}}))
            .collect(),
    );
    assert_eq!(
        run(anilist_doc(json!([{"entries": entries}]))).await,
        "resource_limit"
    );
}

// -------------------------------------------------------------------------------------- Simkl

const SIMKL_SECRETS: fn() -> Value =
    || json!({"client_id": "simkl-client", "access_token": "simkl-access"});

#[tokio::test]
async fn simkl_requests_each_domain_with_client_id_and_bearer_token() {
    let h = Harness::new().await;
    h.feed.json(
        "/sync/all-items/shows/plantowatch",
        json!({"shows": [
            {"show": {"title": "Dark", "year": 2017, "ids": {"simkl": 1, "tvdb": 334, "imdb": "tt5753856", "tmdb": 70523}}, "status": "plantowatch"},
            {"show": {"title": "No Ids", "year": 2020}},
            {"show": {"title": "Dark again", "ids": {"tvdb": "334"}}},
            {"show": {"title": "Null Ids", "ids": null}},
        ]}),
    );
    let rows = snapshot(&h, "simkl", "tv", json!({}), SIMKL_SECRETS()).await;
    assert_eq!(
        rows,
        vec![
            json!(["Dark", 2017, 334, null, "tt5753856"]),
            json!(["No Ids", 2020, null, null, null]),
            json!(["Null Ids", null, null, null, null])
        ]
    );
    let seen = h.feed.requests();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        (seen[0].method.as_str(), seen[0].target.as_str()),
        ("GET", "/sync/all-items/shows/plantowatch")
    );
    assert_eq!(seen[0].header("simkl-api-key"), Some("simkl-client"));
    assert_eq!(seen[0].header("authorization"), Some("Bearer simkl-access"));
    assert_eq!(seen[0].header("accept"), Some("application/json"));

    h.feed.json(
        "/sync/all-items/movies/completed",
        json!({"movies": [{"movie": {"title": "Heat", "year": 1995, "ids": {"simkl": 9, "tmdb": 949, "imdb": "tt0113277", "tvdb": 12}}}]}),
    );
    let rows = snapshot(
        &h,
        "simkl",
        "movies",
        json!({"list_status": "completed"}),
        SIMKL_SECRETS(),
    )
    .await;
    assert_eq!(rows, vec![json!(["Heat", 1995, null, 949, "tt0113277"])]);
    assert_eq!(
        h.feed.requests().last().unwrap().target,
        "/sync/all-items/movies/completed"
    );
    for status in ["watching", "hold", "dropped"] {
        h.feed.json(
            &format!("/sync/all-items/shows/{status}"),
            json!({"shows": []}),
        );
        probe(
            &h,
            "simkl",
            "tv",
            json!({"list_status": status}),
            SIMKL_SECRETS(),
        )
        .await;
        assert_eq!(
            h.feed.requests().last().unwrap().target,
            format!("/sync/all-items/shows/{status}")
        );
    }

    // End to end for both domains.
    h.meta.show(334, "Dark", Some(2017));
    h.meta.show(777, "No Ids", Some(2020));
    h.meta.show(778, "Null Ids", None);
    h.meta.movie(949, "Heat", Some(1995), Some("tt0113277"));
    let tv = h
        .create(draft(&h, "simkl", "tv", json!({}), SIMKL_SECRETS()))
        .await;
    let run = h.sync(&tv).await;
    assert_eq!(
        (run["status"].as_str(), run["added"].as_i64()),
        (Some("succeeded"), Some(3)),
        "{run}"
    );
    let movies = h
        .create(draft(
            &h,
            "simkl",
            "movies",
            json!({"list_status": "completed"}),
            SIMKL_SECRETS(),
        ))
        .await;
    assert_eq!(h.sync(&movies).await["added"], 1);
    assert_eq!(
        (
            h.count("SELECT count(*) FROM series").await,
            h.count("SELECT count(*) FROM movies").await
        ),
        (3, 1)
    );
}

#[tokio::test]
async fn simkl_empty_lists_are_successful_and_malformed_documents_are_not() {
    let h = Harness::new().await;
    for (media, route, key) in [
        ("tv", "/sync/all-items/shows/plantowatch", "shows"),
        ("movies", "/sync/all-items/movies/plantowatch", "movies"),
    ] {
        for (what, body) in [
            ("null", "null".to_owned()),
            ("empty object", "{}".to_owned()),
            ("empty body", String::new()),
            ("whitespace", " \n".to_owned()),
            ("null member", format!("{{\"{key}\":null}}")),
            ("empty array", format!("{{\"{key}\":[]}}")),
        ] {
            h.feed.set(route, 200, "application/json", body);
            let r = probe(&h, "simkl", media, json!({}), SIMKL_SECRETS()).await;
            assert_eq!(
                (r["ok"].clone(), r["item_count"].clone()),
                (json!(true), json!(0)),
                "{media} {what}: {r}"
            );
        }
    }
    let inner = |key: &str, value: Value| json!({key: [value]});
    for (what, body) in [
        ("top-level array", json!([])),
        ("top-level number", json!(3)),
        ("shows an object", json!({"shows": {}})),
        ("row not an object", json!({"shows": [1]})),
        (
            "row without show",
            inner("shows", json!({"movie": {"title": "x"}})),
        ),
        ("show a string", inner("shows", json!({"show": "x"}))),
        (
            "ids a string",
            inner("shows", json!({"show": {"title": "x", "ids": "1"}})),
        ),
        (
            "tvdb not numeric",
            inner(
                "shows",
                json!({"show": {"title": "x", "ids": {"tvdb": "abc"}}}),
            ),
        ),
        (
            "tvdb an object",
            inner(
                "shows",
                json!({"show": {"title": "x", "ids": {"tvdb": {}}}}),
            ),
        ),
        (
            "bad imdb",
            inner(
                "shows",
                json!({"show": {"title": "x", "ids": {"imdb": "nm1"}}}),
            ),
        ),
        (
            "negative year",
            inner("shows", json!({"show": {"title": "x", "year": -3}})),
        ),
    ] {
        h.feed.json("/sync/all-items/shows/plantowatch", body);
        assert_eq!(
            probe_error(&h, "simkl", "tv", json!({}), SIMKL_SECRETS()).await,
            "invalid_response",
            "{what}"
        );
    }
    h.feed.json(
        "/sync/all-items/movies/plantowatch",
        json!({"movies": [{"show": {"title": "x"}}]}),
    );
    assert_eq!(
        probe_error(&h, "simkl", "movies", json!({}), SIMKL_SECRETS()).await,
        "invalid_response",
        "wrong inner key for the domain"
    );
    h.feed.set(
        "/sync/all-items/shows/plantowatch",
        200,
        "application/json",
        "not json",
    );
    assert_eq!(
        probe_error(&h, "simkl", "tv", json!({}), SIMKL_SECRETS()).await,
        "invalid_response"
    );
}

// --------------------------------------------------------------------------- Sonarr and Radarr

fn sonarr_rows() -> Value {
    json!([
        {"id": 1, "tvdbId": 101, "title": "A", "year": 2019, "qualityProfileId": 4, "tags": [1, 2], "rootFolderPath": "/tv/Anime", "monitored": false},
        {"id": 2, "tvdbId": 102, "title": "B", "year": 2020, "qualityProfileId": 5, "tags": [3], "rootFolderPath": "/TV/Kids"},
        {"id": 3, "tvdbId": 103, "title": "C", "year": 2021, "qualityProfileId": 4, "tags": [], "rootFolderPath": "/tv/kids"},
        {"id": 4, "tvdbId": 0, "title": "D", "year": 0, "qualityProfileId": 4, "tags": [1], "rootFolderPath": "/tv/anime"},
    ])
}
fn arr_settings(h: &Harness, extra: Value) -> Value {
    let mut s = json!({"url": format!("{}/sonarr/", h.feed_base)});
    for (k, v) in extra.as_object().unwrap() {
        s[k] = v.clone();
    }
    s
}
const KEY: fn() -> Value = || json!({"api_key": "remote-key"});

#[tokio::test]
async fn sonarr_requests_the_series_endpoint_with_api_key_and_keeps_the_url_base() {
    let h = Harness::new().await;
    h.feed.json("/sonarr/api/v3/series", sonarr_rows());
    let rows = snapshot(&h, "sonarr", "tv", arr_settings(&h, json!({})), KEY()).await;
    // The remote row id (1..4) is never an identity; tvdbId 0 / year 0 mean "unknown".
    assert_eq!(
        rows,
        vec![
            json!(["A", 2019, 101, null, null]),
            json!(["B", 2020, 102, null, null]),
            json!(["C", 2021, 103, null, null]),
            json!(["D", null, null, null, null])
        ]
    );
    let seen = h.feed.requests();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        (seen[0].method.as_str(), seen[0].target.as_str()),
        ("GET", "/sonarr/api/v3/series")
    );
    assert_eq!(seen[0].header("x-api-key"), Some("remote-key"));
    assert_eq!(seen[0].header("accept"), Some("application/json"));
    // Without the trailing slash the same request is made.
    h.feed.clear();
    probe(
        &h,
        "sonarr",
        "tv",
        json!({"url": format!("{}/sonarr", h.feed_base)}),
        KEY(),
    )
    .await;
    assert_eq!(h.feed.requests()[0].target, "/sonarr/api/v3/series");
    // An empty remote library is a valid empty list.
    h.feed.json("/sonarr/api/v3/series", json!([]));
    assert_eq!(
        probe(&h, "sonarr", "tv", arr_settings(&h, json!({})), KEY()).await["item_count"],
        0
    );

    // End to end: TVDB ids from the other instance become library series; the untitled-id row resolves by title.
    h.feed.json("/sonarr/api/v3/series", sonarr_rows());
    h.meta.show(101, "A", Some(2019));
    h.meta.show(102, "B", Some(2020));
    h.meta.show(103, "C", Some(2021));
    h.meta.show(900, "D", None);
    let id = h
        .create(draft(
            &h,
            "sonarr",
            "tv",
            arr_settings(&h, json!({})),
            KEY(),
        ))
        .await;
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
    assert_eq!(h.count("SELECT count(*) FROM series").await, 4);
    assert_eq!(
        h.count("SELECT count(*) FROM series WHERE tvdb_id IN (1,2,3,4)")
            .await,
        0,
        "remote row ids are not identities"
    );
}

#[tokio::test]
async fn sonarr_filters_are_and_across_filters_and_or_within_one() {
    let h = Harness::new().await;
    h.feed.json("/sonarr/api/v3/series", sonarr_rows());
    let run = |extra: Value| {
        let (h, settings) = (&h, arr_settings(&h, extra));
        async move { titles(h, "sonarr", "tv", settings, KEY()).await }
    };
    assert_eq!(run(json!({})).await, ["A", "B", "C", "D"]);
    assert_eq!(
        run(json!({"quality_profile_ids": "4"})).await,
        ["A", "C", "D"]
    );
    assert_eq!(
        run(json!({"quality_profile_ids": "5, 4"})).await,
        ["A", "B", "C", "D"]
    );
    assert_eq!(run(json!({"tag_ids": "1"})).await, ["A", "D"]);
    assert_eq!(run(json!({"tag_ids": "3 9"})).await, ["B"]);
    assert_eq!(
        run(json!({"root_folder_filter": "kids"})).await,
        ["B", "C"],
        "case-insensitive substring on rootFolderPath"
    );
    assert_eq!(
        run(json!({"root_folder_filter": "ANIME, zzz"})).await,
        ["A", "D"]
    );
    assert_eq!(
        run(json!({"root_folder_filter": "/tv/"})).await,
        ["A", "B", "C", "D"]
    );
    assert_eq!(
        run(json!({"quality_profile_ids": "4", "root_folder_filter": "kids"})).await,
        ["C"]
    );
    assert_eq!(
        run(json!({"quality_profile_ids": "4,5", "tag_ids": "3"})).await,
        ["B"]
    );
    assert_eq!(
        run(json!({"quality_profile_ids": "4", "tag_ids": "1", "root_folder_filter": "anime"}))
            .await,
        ["A", "D"]
    );
    assert!(
        run(json!({"tag_ids": "99"})).await.is_empty(),
        "no match is a successful empty list"
    );
    assert!(
        run(json!({"quality_profile_ids": "5", "tag_ids": "1"}))
            .await
            .is_empty()
    );

    // Malformed filter settings fail before any request is made.
    h.feed.clear();
    for (key, value) in [
        ("quality_profile_ids", "four"),
        ("tag_ids", "1;2"),
        ("tag_ids", "-1"),
    ] {
        assert_eq!(
            probe_error(
                &h,
                "sonarr",
                "tv",
                arr_settings(&h, json!({key: value})),
                KEY()
            )
            .await,
            "invalid_settings",
            "{key}={value}"
        );
    }
    assert!(h.feed.requests().is_empty());
}

#[tokio::test]
async fn radarr_requests_the_movie_endpoint_and_maps_tmdb_and_imdb_ids() {
    let h = Harness::new().await;
    let route = "/radarr/api/v3/movie?excludeLocalCovers=true";
    h.feed.json(
        route,
        json!([
            {"id": 9, "tmdbId": 603, "imdbId": "tt0133093", "title": "The Matrix", "year": 1999, "qualityProfileId": 1, "tags": [7], "path": "/movies/The Matrix (1999)"},
            {"id": 10, "tmdbId": 604, "title": "The Matrix Reloaded", "year": 2003, "qualityProfileId": 2, "tags": [], "path": "/data/MOVIES/Reloaded"},
            {"id": 11, "tmdbId": 0, "imdbId": "", "title": "Unknown Film", "year": 0, "qualityProfileId": 1, "path": "/movies/Unknown"},
        ]),
    );
    let settings = |extra: Value| {
        let mut s = json!({"url": format!("{}/radarr", h.feed_base)});
        for (k, v) in extra.as_object().unwrap() {
            s[k] = v.clone();
        }
        s
    };
    let rows = snapshot(&h, "radarr", "movies", settings(json!({})), KEY()).await;
    assert_eq!(
        rows,
        vec![
            json!(["The Matrix", 1999, null, 603, "tt0133093"]),
            json!(["The Matrix Reloaded", 2003, null, 604, null]),
            json!(["Unknown Film", null, null, null, null])
        ]
    );
    let seen = h.feed.requests();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        (seen[0].method.as_str(), seen[0].target.as_str()),
        ("GET", route)
    );
    assert_eq!(seen[0].header("x-api-key"), Some("remote-key"));
    // Filters use the movie path (case-insensitive) for the folder filter.
    assert_eq!(
        titles(
            &h,
            "radarr",
            "movies",
            settings(json!({"root_folder_filter": "movies/the"})),
            KEY()
        )
        .await,
        ["The Matrix"]
    );
    assert_eq!(
        titles(
            &h,
            "radarr",
            "movies",
            settings(json!({"root_folder_filter": "/data/movies"})),
            KEY()
        )
        .await,
        ["The Matrix Reloaded"]
    );
    assert_eq!(
        titles(
            &h,
            "radarr",
            "movies",
            settings(json!({"quality_profile_ids": "1", "tag_ids": "7"})),
            KEY()
        )
        .await,
        ["The Matrix"]
    );
    assert_eq!(
        titles(
            &h,
            "radarr",
            "movies",
            settings(json!({"quality_profile_ids": "2", "tag_ids": "7"})),
            KEY()
        )
        .await
        .len(),
        0
    );

    // End to end: the TMDb id is the identity; the id-less row falls back to an exact title match.
    h.meta
        .movie(603, "The Matrix", Some(1999), Some("tt0133093"));
    h.meta.movie(604, "The Matrix Reloaded", Some(2003), None);
    h.meta.movie(605, "Unknown Film", None, None);
    let id = h
        .create(draft(&h, "radarr", "movies", settings(json!({})), KEY()))
        .await;
    let run = h.sync(&id).await;
    assert_eq!(
        (run["status"].as_str(), run["added"].as_i64()),
        (Some("succeeded"), Some(3)),
        "{run}"
    );
    assert_eq!(h.count("SELECT count(*) FROM movies m JOIN movie_metadata d ON d.id=m.metadata_id WHERE d.tmdb_id IN (603,604,605)").await, 3);
    assert_eq!(h.count("SELECT count(*) FROM movies m JOIN movie_metadata d ON d.id=m.metadata_id WHERE d.tmdb_id IN (9,10,11)").await, 0, "remote row ids are not identities");
}

#[tokio::test]
async fn arr_rejects_malformed_rows_and_never_follows_a_redirect_with_the_api_key() {
    let h = Harness::new().await;
    for (imp, media, route, id_key) in [
        ("sonarr", "tv", "/api/v3/series", "tvdbId"),
        ("radarr", "movies", "/api/v3/movie", "tmdbId"),
    ] {
        let settings = json!({"url": h.feed_base});
        for (what, body) in [
            ("object", json!({"items": []})),
            ("row not an object", json!([1])),
            ("id a string", json!([{id_key: "abc", "title": "x"}])),
            ("id negative", json!([{id_key: -5, "title": "x"}])),
            ("id an object", json!([{id_key: {}, "title": "x"}])),
            ("tags not an array", json!([{id_key: 1, "tags": 4}])),
            ("tag not an integer", json!([{id_key: 1, "tags": ["x"]}])),
            (
                "profile not a number",
                json!([{id_key: 1, "qualityProfileId": "x"}]),
            ),
            (
                "path a number",
                json!([{id_key: 1, "path": 4, "rootFolderPath": 4}]),
            ),
            (
                "negative year",
                json!([{id_key: 1, "title": "x", "year": -5}]),
            ),
            (
                "year too large",
                json!([{id_key: 1, "title": "x", "year": 99999}]),
            ),
        ] {
            h.feed.json(route, body);
            assert_eq!(
                probe_error(&h, imp, media, settings.clone(), KEY()).await,
                "invalid_response",
                "{imp}: {what}"
            );
        }
        h.feed.json(route, json!([]));
        assert_eq!(
            probe(&h, imp, media, settings.clone(), KEY()).await["item_count"],
            0,
            "{imp} empty library"
        );
        // A wrong URL base answers with a redirect: surfaced as such, the key is sent once and
        // the Location target is never requested.
        h.feed.clear();
        h.feed.set(route, 301, "text/plain", "");
        let r = probe(&h, imp, media, settings.clone(), KEY()).await;
        assert_eq!(r["error"], "redirect", "{imp}: {r}");
        assert_eq!(h.feed.requests().len(), 1, "{imp}: redirect not followed");
        // Authentication failures are their own class.
        h.feed.set(
            route,
            401,
            "application/json",
            "{\"error\":\"Unauthorized\"}",
        );
        assert_eq!(
            probe_error(&h, imp, media, settings.clone(), KEY()).await,
            "authentication"
        );
    }
    // The configured URL is validated like every other list URL.
    for bad in [
        "ftp://example.test",
        "http://user:pw@example.test",
        "http://169.254.169.254/",
    ] {
        let (status, _) = h
            .call(
                "POST",
                "/api/v1/importlist",
                draft(&h, "sonarr", "tv", json!({"url": bad}), KEY()),
            )
            .await;
        assert_eq!(status, 400, "{bad}");
    }
}
