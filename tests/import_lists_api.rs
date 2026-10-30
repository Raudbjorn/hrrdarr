//! Import-list registry, API validation, secret handling and the synchronisation engine, against
//! in-process fake list sources and a fake metadata catalog. No real third-party service is used.
mod import_lists_support;
use import_lists_support::Harness;
use serde_json::{Value, json};

fn feed_items(n: usize) -> Value {
    Value::Array(
        (1..=n)
            .map(|i| json!({"tvdb_id": 1000 + i, "title": format!("Show {i}")}))
            .collect(),
    )
}

#[tokio::test]
async fn registry_crud_secrets_and_validation() {
    let h = Harness::new().await;
    let (status, schema) = h
        .call("GET", "/api/v1/importlist/schema", Value::Null)
        .await;
    assert_eq!(status, 200);
    let names: Vec<&str> = schema["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["implementation"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"custom") && names.contains(&"rss"),
        "{names:?}"
    );
    let custom = schema["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["implementation"] == "custom")
        .unwrap();
    assert!(
        custom["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["name"] == "bearer_token" && f["secret"] == true)
    );

    let url = format!("{}/list.json", h.feed_base);
    let mut input = h.input("Mine", "custom", "tv", json!({"url": url}));
    input["secrets"] = json!({"bearer_token": "tok-SECRET-123"});
    input["tag_ids"] = json!([]);
    let (status, created) = h.call("POST", "/api/v1/importlist", input.clone()).await;
    assert_eq!(status, 201, "{created}");
    assert_eq!(created["secrets_configured"], json!(["bearer_token"]));
    assert_eq!(created["revision"], 1);
    assert_eq!(created["series_type"], "standard");
    assert_eq!(created["season_folder"], true);
    assert!(created["minimum_availability"].is_null());
    assert!(!created.to_string().contains("tok-SECRET-123"));
    let id = created["id"].as_str().unwrap().to_owned();
    // Secrets are sealed: not in settings_json and not recoverable from the stored blob.
    assert_eq!(
        h.count("SELECT count(*) FROM import_lists WHERE settings_json LIKE '%SECRET%'")
            .await,
        0
    );
    assert_eq!(
        h.count("SELECT count(*) FROM import_lists WHERE instr(credentials,'tok-SECRET-123')>0")
            .await,
        0
    );
    assert_eq!(
        h.count("SELECT count(*) FROM import_lists WHERE credentials IS NOT NULL")
            .await,
        1
    );
    let (_, list) = h.call("GET", "/api/v1/importlist", Value::Null).await;
    assert!(!list.to_string().contains("tok-SECRET-123"));

    // Same name: conflict in the domain, allowed in the other.
    assert_eq!(
        h.call("POST", "/api/v1/importlist", input.clone()).await.0,
        409
    );
    let mut movie = h.input("mine", "custom", "movies", json!({"url": url}));
    movie["minimum_availability"] = json!("in_cinemas");
    movie["monitor"] = json!("none");
    let (status, body) = h.call("POST", "/api/v1/importlist", movie.clone()).await;
    assert_eq!(status, 201, "{body}");
    assert_eq!(body["minimum_availability"], "in_cinemas");
    assert_eq!(body["monitor"], "none");
    assert!(body["series_type"].is_null() && body["season_folder"].is_null());

    // Validation is domain-scoped and explicit.
    let bad = |mut v: Value, key: &str, value: Value| {
        v[key] = value;
        v
    };
    for (label, body) in [
        (
            "unknown implementation",
            bad(h.input("x", "nope", "tv", json!({})), "name", json!("x2")),
        ),
        (
            "movie monitor first_season",
            bad(
                h.input("m2", "custom", "movies", json!({"url": url})),
                "monitor",
                json!("first_season"),
            ),
        ),
        (
            "tv with minimum availability",
            bad(
                h.input("t2", "custom", "tv", json!({"url": url})),
                "minimum_availability",
                json!("released"),
            ),
        ),
        (
            "movie with series type",
            bad(
                h.input("m3", "custom", "movies", json!({"url": url})),
                "series_type",
                json!("anime"),
            ),
        ),
        (
            "profile from other domain",
            bad(
                h.input("t3", "custom", "tv", json!({"url": url})),
                "quality_profile_id",
                json!(h.movie_profile),
            ),
        ),
        (
            "root from other domain",
            bad(
                h.input("t4", "custom", "tv", json!({"url": url})),
                "root_folder_id",
                json!(h.movie_root),
            ),
        ),
        (
            "unknown tag",
            bad(
                h.input("t5", "custom", "tv", json!({"url": url})),
                "tag_ids",
                json!([999]),
            ),
        ),
        ("missing url", h.input("t6", "custom", "tv", json!({}))),
        (
            "link-local url",
            h.input(
                "t7",
                "custom",
                "tv",
                json!({"url": "http://169.254.169.254/latest"}),
            ),
        ),
        (
            "credentials in url",
            h.input(
                "t8",
                "custom",
                "tv",
                json!({"url": "http://u:p@127.0.0.1/x"}),
            ),
        ),
        (
            "secret in settings",
            h.input(
                "t9",
                "custom",
                "tv",
                json!({"url": url, "bearer_token": "x"}),
            ),
        ),
        (
            "interval too small",
            bad(
                h.input("t10", "custom", "tv", json!({"url": url})),
                "interval_minutes",
                json!(1),
            ),
        ),
        (
            "unknown field",
            bad(
                h.input("t11", "custom", "tv", json!({"url": url})),
                "bogus",
                json!(1),
            ),
        ),
    ] {
        let (status, response) = h.call("POST", "/api/v1/importlist", body).await;
        assert_eq!(status, 400, "{label}: {response}");
    }
    // Automatic add needs a profile and a root; turning it off does not.
    let mut no_defaults = h.input("t12", "custom", "tv", json!({"url": url}));
    no_defaults
        .as_object_mut()
        .unwrap()
        .remove("quality_profile_id");
    assert_eq!(
        h.call("POST", "/api/v1/importlist", no_defaults.clone())
            .await
            .0,
        400
    );
    no_defaults["enable_automatic_add"] = json!(false);
    assert_eq!(
        h.call("POST", "/api/v1/importlist", no_defaults).await.0,
        201
    );

    // Update: revision-guarded; absent secret kept, null clears; identity immutable.
    let mut update = input.clone();
    update["revision"] = json!(1);
    update.as_object_mut().unwrap().remove("secrets");
    update["monitor"] = json!("first_season");
    let (status, updated) = h
        .call("PUT", &format!("/api/v1/importlist/{id}"), update.clone())
        .await;
    assert_eq!(status, 200, "{updated}");
    assert_eq!(updated["revision"], 2);
    assert_eq!(updated["secrets_configured"], json!(["bearer_token"]));
    assert_eq!(
        h.call("PUT", &format!("/api/v1/importlist/{id}"), update.clone())
            .await
            .0,
        409,
        "stale revision"
    );
    let mut clear = update.clone();
    clear["revision"] = json!(2);
    clear["secrets"] = json!({"bearer_token": null});
    let (_, cleared) = h
        .call("PUT", &format!("/api/v1/importlist/{id}"), clear.clone())
        .await;
    assert_eq!(cleared["secrets_configured"], json!([]));
    assert_eq!(
        h.count("SELECT count(*) FROM import_lists WHERE id!='' AND credentials IS NOT NULL")
            .await,
        0
    );
    let mut swap = clear.clone();
    swap["revision"] = json!(3);
    swap["media_type"] = json!("movies");
    assert_eq!(
        h.call("PUT", &format!("/api/v1/importlist/{id}"), swap)
            .await
            .0,
        400
    );
    swap = clear;
    swap["revision"] = json!(3);
    swap["implementation"] = json!("rss");
    assert_eq!(
        h.call("PUT", &format!("/api/v1/importlist/{id}"), swap)
            .await
            .0,
        400
    );

    assert_eq!(
        h.call("DELETE", &format!("/api/v1/importlist/{id}"), Value::Null)
            .await
            .0,
        204
    );
    assert_eq!(
        h.call("GET", &format!("/api/v1/importlist/{id}"), Value::Null)
            .await
            .0,
        404
    );
    assert_eq!(
        h.call("GET", "/api/v1/importlist/not-a-uuid", Value::Null)
            .await
            .0,
        400
    );
}

#[tokio::test]
async fn tags_profile_and_root_deletion_follow_the_collection_default_policy() {
    let h = Harness::new().await;
    h.sql("INSERT INTO tags(id,media_type,label) VALUES(7,'tv','kids'),(8,'movies','kids')")
        .await;
    let mut input = h.input(
        "Tagged",
        "custom",
        "tv",
        json!({"url": format!("{}/x", h.feed_base)}),
    );
    input["tag_ids"] = json!([7]);
    let id = h.create(input).await;
    assert_eq!(
        h.call("GET", &format!("/api/v1/importlist/{id}"), Value::Null)
            .await
            .1["tag_ids"],
        json!([7])
    );
    // Deleting the profile disables adding rather than leaving a dangling reference.
    h.sql(&format!(
        "DELETE FROM quality_profiles WHERE id={} AND media_type='tv'",
        h.tv_profile
    ))
    .await;
    let (_, list) = h
        .call("GET", &format!("/api/v1/importlist/{id}"), Value::Null)
        .await;
    assert!(list["quality_profile_id"].is_null());
    assert_eq!(list["enable_automatic_add"], false);
    // Deleting a tag simply leaves the default set.
    h.sql("DELETE FROM tags WHERE id=7").await;
    assert_eq!(
        h.call("GET", &format!("/api/v1/importlist/{id}"), Value::Null)
            .await
            .1["tag_ids"],
        json!([])
    );
    // A root folder deletion does the same for the other lists.
    let id2 = h
        .create(h.input(
            "Rooted",
            "custom",
            "movies",
            json!({"url": format!("{}/y", h.feed_base)}),
        ))
        .await;
    h.sql(&format!(
        "DELETE FROM root_folders WHERE id={}",
        h.movie_root
    ))
    .await;
    let (_, list) = h
        .call("GET", &format!("/api/v1/importlist/{id2}"), Value::Null)
        .await;
    assert!(list["root_folder_id"].is_null());
    assert_eq!(list["enable_automatic_add"], false);
}

#[tokio::test]
async fn custom_list_sync_adds_series_with_defaults_tags_provenance_and_monitoring() {
    let h = Harness::new().await;
    h.meta.show(2001, "Alpha", Some(2019));
    h.meta.show(2002, "Beta", Some(2020));
    h.meta.show(2003, "Gamma", Some(2021));
    h.sql("INSERT INTO tags(id,media_type,label) VALUES(7,'tv','from-list')")
        .await;
    h.feed.json(
        "/tv.json",
        json!([
            {"tvdb_id": 2001, "title": "Alpha", "year": 2019},
            {"tvdbId": 2002, "title": "Beta"},
            {"tvdb_id": 2002, "title": "Beta again"},
            {"tvdb_id": 2003, "title": "Gamma: Part/1"}
        ]),
    );
    let mut input = h.input(
        "Feed",
        "custom",
        "tv",
        json!({"url": format!("{}/tv.json", h.feed_base)}),
    );
    input["monitor"] = json!("first_season");
    input["series_type"] = json!("anime");
    input["season_folder"] = json!(false);
    input["tag_ids"] = json!([7]);
    input["search_on_add"] = json!(true);
    let id = h.create(input).await;
    // An exclusion applies even when the list carries the item.
    h.call(
        "POST",
        "/api/v1/importlist/exclusions",
        json!({"media_type":"tv","external_id":2003,"title":"Gamma"}),
    )
    .await;

    let run = h.sync(&id).await;
    assert_eq!(run["status"], "succeeded", "{run}");
    assert_eq!(
        (
            run["items_fetched"].as_i64(),
            run["added"].as_i64(),
            run["excluded"].as_i64()
        ),
        (Some(3), Some(2), Some(1))
    );
    assert_eq!(run["origin"], "manual");
    assert_eq!(run["pending"], 0);

    assert_eq!(h.count("SELECT count(*) FROM series").await, 2);
    // Defaults reached the library through the shared add path.
    assert_eq!(h.count(&format!("SELECT count(*) FROM library_settings l JOIN series s ON s.id=l.series_id WHERE l.quality_profile_id={} AND l.series_type='anime' AND l.season_folder=0", h.tv_profile)).await, 2);
    assert_eq!(
        h.count("SELECT count(*) FROM series_tags WHERE tag_id=7")
            .await,
        2
    );
    assert_eq!(
        h.count("SELECT count(*) FROM series WHERE monitored=1")
            .await,
        2
    );
    // first_season: season 1 monitored, specials and later seasons not.
    assert_eq!(
        h.count("SELECT count(*) FROM seasons WHERE number=1 AND monitored=1")
            .await,
        2
    );
    assert_eq!(
        h.count("SELECT count(*) FROM seasons WHERE number!=1 AND monitored=1")
            .await,
        0
    );
    assert_eq!(
        h.count("SELECT count(*) FROM episodes WHERE season=2 AND monitored=1")
            .await,
        0
    );
    let root = h.dir.join("tv");
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM series WHERE path IN ('{0}/Alpha (2019)','{0}/Beta')",
            root.display()
        ))
        .await,
        2
    );
    // Provenance: list, run and search intent recorded in the same transaction as the add.
    assert_eq!(h.count(&format!("SELECT count(*) FROM import_list_additions WHERE list_id='{id}' AND list_name='Feed' AND run_id='{}' AND search_requested=1 AND media_type='tv' AND series_id IS NOT NULL", run["id"].as_str().unwrap())).await, 2);
    let states: Vec<String> = h
        .items(&id, "added")
        .await
        .iter()
        .map(|i| i["key"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(states, vec!["tvdb:2001", "tvdb:2002"]);
    assert_eq!(h.items(&id, "excluded").await.len(), 1);

    // A second sync is idempotent: nothing is added again and the exclusion is rechecked.
    let again = h.sync(&id).await;
    assert_eq!(
        (again["status"].as_str(), again["added"].as_i64()),
        (Some("succeeded"), Some(0))
    );
    assert_eq!(h.count("SELECT count(*) FROM series").await, 2);
    // Removing the exclusion lets the next sync add the item.
    let (_, page) = h
        .call("GET", "/api/v1/importlist/exclusions", Value::Null)
        .await;
    let ex_id = page["items"][0]["id"].as_i64().unwrap();
    assert_eq!(
        h.call(
            "DELETE",
            &format!("/api/v1/importlist/exclusions/{ex_id}"),
            Value::Null
        )
        .await
        .0,
        204
    );
    let third = h.sync(&id).await;
    assert_eq!(third["added"], 1, "{third}");
    assert_eq!(h.count("SELECT count(*) FROM series").await, 3);
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM series WHERE path='{}/Gamma- Part-1'",
            root.display()
        ))
        .await,
        1,
        "path segment is sanitised"
    );

    // Deleting the list keeps the library records and their provenance name.
    assert_eq!(
        h.call("DELETE", &format!("/api/v1/importlist/{id}"), Value::Null)
            .await
            .0,
        204
    );
    assert_eq!(h.count("SELECT count(*) FROM series").await, 3);
    assert_eq!(
        h.count(
            "SELECT count(*) FROM import_list_additions WHERE list_id IS NULL AND list_name='Feed'"
        )
        .await,
        3
    );
    assert_eq!(h.count("SELECT count(*) FROM import_list_items").await, 0);
}

#[tokio::test]
async fn movie_list_applies_availability_monitoring_and_exclusions_by_domain() {
    let h = Harness::new().await;
    h.meta
        .movie(3001, "Film One", Some(2020), Some("tt1000001"));
    h.meta
        .movie(3002, "Film Two", Some(2021), Some("tt1000002"));
    h.meta.show(3001, "Same number show", Some(2020));
    h.feed.json(
        "/movies.json",
        json!([{"tmdb_id":3001,"title":"Film One"},{"tmdb_id":3002,"title":"Film Two"}]),
    );
    let mut input = h.input(
        "Movies",
        "custom",
        "movies",
        json!({"url": format!("{}/movies.json", h.feed_base)}),
    );
    input["minimum_availability"] = json!("announced");
    input["monitor"] = json!("none");
    let id = h.create(input).await;
    // A TV exclusion with the same number must not block the movie: domains are distinct.
    h.call(
        "POST",
        "/api/v1/importlist/exclusions",
        json!({"media_type":"tv","external_id":3001,"title":"Same number show"}),
    )
    .await;
    h.call(
        "POST",
        "/api/v1/importlist/exclusions",
        json!({"media_type":"movies","external_id":3002,"title":"Film Two","year":2021}),
    )
    .await;
    let run = h.sync(&id).await;
    assert_eq!(
        (run["added"].as_i64(), run["excluded"].as_i64()),
        (Some(1), Some(1)),
        "{run}"
    );
    assert_eq!(h.count("SELECT count(*) FROM movies").await, 1);
    assert_eq!(
        h.count("SELECT count(*) FROM movies WHERE monitored=0")
            .await,
        1
    );
    assert_eq!(h.count(&format!("SELECT count(*) FROM library_settings WHERE movie_id IS NOT NULL AND minimum_availability='announced' AND quality_profile_id={}", h.movie_profile)).await, 1);
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM movies WHERE path='{}/Film One'",
            h.dir.join("movies").display()
        ))
        .await,
        1
    );
    assert_eq!(h.count("SELECT count(*) FROM import_list_additions WHERE media_type='movies' AND movie_id IS NOT NULL AND search_requested=0").await, 1);
    // Exclusion API: idempotent add, domain filter, bulk delete, validation.
    let (status, again) = h
        .call(
            "POST",
            "/api/v1/importlist/exclusions",
            json!({"media_type":"movies","external_id":3002,"title":"Renamed"}),
        )
        .await;
    assert_eq!((status, again["title"].as_str()), (200, Some("Film Two")));
    let (_, movies) = h
        .call(
            "GET",
            "/api/v1/importlist/exclusions?media_type=movies",
            Value::Null,
        )
        .await;
    assert_eq!(movies["total"], 1);
    for bad in [
        json!({"media_type":"movies","external_id":0,"title":"x"}),
        json!({"media_type":"movies","external_id":5,"title":""}),
        json!({"media_type":"anime","external_id":5,"title":"x"}),
    ] {
        assert_eq!(
            h.call("POST", "/api/v1/importlist/exclusions", bad).await.0,
            400
        );
    }
    let (_, all) = h
        .call("GET", "/api/v1/importlist/exclusions", Value::Null)
        .await;
    let ids: Vec<i64> = all["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_i64().unwrap())
        .collect();
    assert_eq!(
        h.call(
            "DELETE",
            "/api/v1/importlist/exclusions/bulk",
            json!({"ids":[ids[0], 99999]})
        )
        .await
        .0,
        404
    );
    assert_eq!(
        h.call("GET", "/api/v1/importlist/exclusions", Value::Null)
            .await
            .1["total"],
        2,
        "bulk delete is atomic"
    );
    assert_eq!(
        h.call(
            "DELETE",
            "/api/v1/importlist/exclusions/bulk",
            json!({"ids":ids})
        )
        .await
        .0,
        204
    );
    assert_eq!(
        h.call("GET", "/api/v1/importlist/exclusions", Value::Null)
            .await
            .1["total"],
        0
    );
}

#[tokio::test]
async fn items_without_a_catalog_identity_resolve_by_exact_title_or_stay_unresolved() {
    let h = Harness::new().await;
    h.meta
        .movie(4001, "Exact Match", Some(2020), Some("tt2000001"));
    h.meta.movie(4002, "Twin", Some(2000), None);
    h.meta.movie(4003, "Twin", Some(2010), None);
    h.meta
        .movie(4004, "By Imdb Only", Some(2015), Some("tt2000004"));
    h.feed.json(
        "/m.json",
        json!([
            {"title": "Exact Match", "year": 2020},
            {"title": "Twin"},
            {"title": "Twin", "year": 2010},
            {"imdb_id": "tt2000004", "title": "Whatever the list calls it"},
            {"title": "Nothing Like It"}
        ]),
    );
    let id = h
        .create(h.input(
            "Titles",
            "custom",
            "movies",
            json!({"url": format!("{}/m.json", h.feed_base)}),
        ))
        .await;
    let run = h.sync(&id).await;
    // "Twin" without a year is ambiguous; with a year it is unique. Unknown titles are unresolved.
    assert_eq!(
        (
            run["added"].as_i64(),
            run["unresolved"].as_i64(),
            run["failed"].as_i64()
        ),
        (Some(3), Some(2), Some(0)),
        "{run}"
    );
    assert_eq!(h.count("SELECT count(*) FROM movies").await, 3);
    let unresolved: Vec<String> = h
        .items(&id, "unresolved")
        .await
        .iter()
        .map(|i| i["title"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(unresolved, vec!["Twin", "Nothing Like It"]);
    // The metadata service going away is a retryable item failure, not an add and not "unresolved".
    h.meta.down.store(true, std::sync::atomic::Ordering::SeqCst);
    h.feed.json(
        "/m.json",
        json!([{"title": "Exact Match", "year": 2020}, {"title": "Later Film", "year": 2022}]),
    );
    h.meta.movie(4005, "Later Film", Some(2022), None);
    let run = h.sync(&id).await;
    assert_eq!(run["status"], "succeeded");
    assert_eq!(run["failed"], 1, "{run}");
    assert_eq!(
        h.items(&id, "failed").await[0]["error_code"],
        "metadata_unavailable"
    );
    assert_eq!(h.count("SELECT count(*) FROM movies").await, 3);
}

#[tokio::test]
async fn failed_fetch_keeps_the_previous_snapshot_and_never_adds_from_a_corrupt_document() {
    let h = Harness::new().await;
    h.meta.show(5001, "Stable", None);
    h.feed
        .json("/list.json", json!([{"tvdb_id":5001,"title":"Stable"}]));
    let id = h
        .create(h.input(
            "Flaky",
            "custom",
            "tv",
            json!({"url": format!("{}/list.json", h.feed_base)}),
        ))
        .await;
    assert_eq!(h.sync(&id).await["added"], 1);
    for (label, status, content_type, body, code) in [
        (
            "object instead of array",
            200,
            "application/json",
            r#"{"items":[]}"#,
            "invalid_response",
        ),
        ("not json", 200, "text/plain", "<html>", "invalid_response"),
        (
            "element is not an object",
            200,
            "application/json",
            r#"[1]"#,
            "invalid_response",
        ),
        (
            "id of the wrong type",
            200,
            "application/json",
            r#"[{"tvdb_id":{"x":1},"title":"A"}]"#,
            "invalid_response",
        ),
        (
            "negative id",
            200,
            "application/json",
            r#"[{"tvdb_id":-5,"title":"A"}]"#,
            "invalid_response",
        ),
        (
            "bad imdb id",
            200,
            "application/json",
            r#"[{"imdb_id":"nm0000001","title":"A"}]"#,
            "invalid_response",
        ),
        (
            "unauthorised",
            401,
            "application/json",
            "[]",
            "authentication",
        ),
        (
            "redirect is not followed",
            302,
            "text/plain",
            "",
            "redirect",
        ),
        ("client error", 404, "text/plain", "", "rejected"),
    ] {
        h.feed.set("/list.json", status, content_type, body);
        let run = h.sync(&id).await;
        assert_eq!(
            (run["status"].as_str(), run["error_code"].as_str()),
            (Some("failed"), Some(code)),
            "{label}: {run}"
        );
        assert_eq!(
            h.items(&id, "added").await.len(),
            1,
            "{label}: previous snapshot retained"
        );
        let (_, list) = h
            .call("GET", &format!("/api/v1/importlist/{id}"), Value::Null)
            .await;
        assert_eq!(list["last_error"], code);
        assert!(list["last_success_at"].is_i64());
        assert_eq!(
            h.count("SELECT count(*) FROM series").await,
            1,
            "{label}: nothing added"
        );
    }
    // A valid empty array is a complete successful list: membership is replaced, the library is untouched.
    h.feed.json("/list.json", json!([]));
    let run = h.sync(&id).await;
    assert_eq!(
        (run["status"].as_str(), run["items_fetched"].as_i64()),
        (Some("succeeded"), Some(0))
    );
    assert_eq!(h.items(&id, "added").await.len(), 0);
    assert_eq!(
        h.count("SELECT count(*) FROM series").await,
        1,
        "removal from a list never deletes library items"
    );
    let (_, list) = h
        .call("GET", &format!("/api/v1/importlist/{id}"), Value::Null)
        .await;
    assert!(list["last_error"].is_null());
}

#[tokio::test]
async fn retryable_failures_back_off_and_exhaust_while_oversized_lists_fail_without_truncation() {
    let h = Harness::new().await;
    h.feed.set("/down.json", 503, "text/plain", "x");
    let id = h
        .create(h.input(
            "Down",
            "custom",
            "tv",
            json!({"url": format!("{}/down.json", h.feed_base)}),
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
    let run = &runs["items"][0];
    // One attempt made; the retry is scheduled in the future so the drain stops.
    assert_eq!(
        (
            run["status"].as_str(),
            run["attempts"].as_i64(),
            run["error_code"].as_str()
        ),
        (Some("queued"), Some(1), Some("server_error")),
        "{run}"
    );
    assert_eq!(h.feed.requests().len(), 1);
    // Make it due now: second and third attempts, then the run fails with the stable class.
    for _ in 0..2 {
        h.sql("UPDATE import_list_runs SET next_attempt_at=0 WHERE status='queued'")
            .await;
        h.engine.drain().await.unwrap();
    }
    let (_, runs) = h
        .call("GET", &format!("/api/v1/importlist/{id}/runs"), Value::Null)
        .await;
    assert_eq!(
        (
            runs["items"][0]["status"].as_str(),
            runs["items"][0]["attempts"].as_i64()
        ),
        (Some("failed"), Some(3))
    );
    assert_eq!(h.feed.requests().len(), 3);

    // 5000 items fit; 5001 fail with resource_limit and write nothing (no silent truncation).
    h.feed.json("/big.json", feed_items(5000));
    let big = h
        .create(h.input(
            "Big",
            "custom",
            "tv",
            json!({"url": format!("{}/big.json", h.feed_base)}),
        ))
        .await;
    h.call("PUT", &format!("/api/v1/importlist/{big}"), {
        let mut v = h.input(
            "Big",
            "custom",
            "tv",
            json!({"url": format!("{}/big.json", h.feed_base)}),
        );
        v["enable_automatic_add"] = json!(false);
        v["revision"] = json!(1);
        v
    })
    .await;
    let run = h.sync(&big).await;
    assert_eq!(
        (run["status"].as_str(), run["items_fetched"].as_i64()),
        (Some("succeeded"), Some(5000)),
        "{run}"
    );
    assert_eq!(
        h.count("SELECT count(*) FROM import_list_items").await,
        5000
    );
    assert_eq!(
        h.items(&big, "pending").await.len(),
        500,
        "pages are bounded"
    );
    h.feed.json("/big.json", feed_items(5001));
    let run = h.sync(&big).await;
    assert_eq!(
        (run["status"].as_str(), run["error_code"].as_str()),
        (Some("failed"), Some("resource_limit"))
    );
    assert_eq!(
        h.count("SELECT count(*) FROM import_list_items").await,
        5000,
        "previous snapshot retained"
    );
    // A body above the response cap is rejected as well.
    h.feed.set(
        "/huge.json",
        200,
        "application/json",
        vec![b' '; 5 * 1024 * 1024],
    );
    let huge = h
        .create(h.input(
            "Huge",
            "custom",
            "tv",
            json!({"url": format!("{}/huge.json", h.feed_base)}),
        ))
        .await;
    let run = h.sync(&huge).await;
    assert_eq!(run["error_code"], "resource_limit", "{run}");
}

#[tokio::test]
async fn sync_is_resumable_across_passes_and_a_crashed_claim() {
    let h = Harness::new().await;
    for i in 1..=30 {
        h.meta.show(6000 + i, &format!("Resume {i}"), None);
    }
    h.feed.json(
        "/r.json",
        Value::Array(
            (1..=30)
                .map(|i| json!({"tvdb_id": 6000 + i, "title": format!("Resume {i}")}))
                .collect(),
        ),
    );
    let id = h
        .create(h.input(
            "Resume",
            "custom",
            "tv",
            json!({"url": format!("{}/r.json", h.feed_base)}),
        ))
        .await;
    h.call(
        "POST",
        &format!("/api/v1/importlist/{id}/sync"),
        Value::Null,
    )
    .await;
    // One claimed pass: fetch + the first 25 items; the run goes back to the queue with progress kept.
    assert_eq!(h.engine.tick(1).await.unwrap(), 1);
    let (_, runs) = h
        .call(
            "GET",
            &format!("/api/v1/importlist/{id}/runs?limit=1"),
            Value::Null,
        )
        .await;
    let run = runs["items"][0].clone();
    assert_eq!(
        (
            run["status"].as_str(),
            run["fetched"].as_bool(),
            run["added"].as_i64(),
            run["pending"].as_i64()
        ),
        (Some("queued"), Some(true), Some(25), Some(5)),
        "{run}"
    );
    assert_eq!(h.feed.requests().len(), 1);
    // The source disappears: resuming must not refetch it.
    h.feed.set("/r.json", 500, "text/plain", "gone");
    // Simulate a crash during the next claim: running with an expired lease.
    h.sql("UPDATE import_list_runs SET status='running',lease_until=1")
        .await;
    assert_eq!(h.engine.tick(1).await.unwrap(), 1);
    let (_, runs) = h
        .call(
            "GET",
            &format!("/api/v1/importlist/{id}/runs?limit=1"),
            Value::Null,
        )
        .await;
    let run = &runs["items"][0];
    assert_eq!(
        (
            run["status"].as_str(),
            run["added"].as_i64(),
            run["pending"].as_i64()
        ),
        (Some("succeeded"), Some(30), Some(0)),
        "{run}"
    );
    assert_eq!(
        h.feed.requests().len(),
        1,
        "resumed from the stored snapshot without refetching"
    );
    assert_eq!(h.count("SELECT count(*) FROM series").await, 30);
    // A live lease is not stolen.
    h.call(
        "POST",
        &format!("/api/v1/importlist/{id}/sync"),
        Value::Null,
    )
    .await;
    h.sql(
        "UPDATE import_list_runs SET status='running',lease_until=9999999999 WHERE status='queued'",
    )
    .await;
    assert_eq!(h.engine.tick(1).await.unwrap(), 0);
}

#[tokio::test]
async fn manual_sync_is_idempotent_while_a_run_is_active_and_sync_all_skips_disabled_lists() {
    let h = Harness::new().await;
    h.feed.json("/a.json", json!([]));
    let a = h
        .create(h.input(
            "A",
            "custom",
            "tv",
            json!({"url": format!("{}/a.json", h.feed_base)}),
        ))
        .await;
    let mut off = h.input(
        "B",
        "custom",
        "movies",
        json!({"url": format!("{}/a.json", h.feed_base)}),
    );
    off["enabled"] = json!(false);
    h.create(off).await;
    let (s1, r1) = h
        .call("POST", &format!("/api/v1/importlist/{a}/sync"), Value::Null)
        .await;
    let (s2, r2) = h
        .call("POST", &format!("/api/v1/importlist/{a}/sync"), Value::Null)
        .await;
    assert_eq!(
        (s1, s2, r1["id"].clone()),
        (202, 202, r2["id"].clone()),
        "the active run is returned"
    );
    assert_eq!(h.count("SELECT count(*) FROM import_list_runs").await, 1);
    let (status, all) = h.call("POST", "/api/v1/importlist/sync", Value::Null).await;
    assert_eq!(status, 202);
    assert_eq!(
        (
            all["runs"].as_array().unwrap().len(),
            all["skipped"].as_i64()
        ),
        (0, Some(2))
    );
    assert_eq!(
        h.call(
            "POST",
            "/api/v1/importlist/00000000-0000-4000-8000-000000000000/sync",
            Value::Null
        )
        .await
        .0,
        404
    );
    let (_, lists) = h.call("GET", "/api/v1/importlist", Value::Null).await;
    let off_id = lists["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["name"] == "B")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        h.call(
            "POST",
            &format!("/api/v1/importlist/{off_id}/sync"),
            Value::Null
        )
        .await
        .0,
        409,
        "a disabled list is not synced"
    );
    h.engine.drain().await.unwrap();
    assert_eq!(
        h.call(
            "GET",
            &format!("/api/v1/importlist/{a}/runs?limit=0"),
            Value::Null
        )
        .await
        .0,
        400
    );
}

#[tokio::test]
async fn scheduler_queues_due_enabled_lists_and_respects_intervals_and_failure_cooldown() {
    let h = Harness::new().await;
    h.feed.json("/s.json", json!([]));
    let mut input = h.input(
        "Sched",
        "custom",
        "tv",
        json!({"url": format!("{}/s.json", h.feed_base)}),
    );
    input["interval_minutes"] = json!(60);
    let id = h.create(input).await;
    let mut disabled = h.input(
        "Off",
        "custom",
        "tv",
        json!({"url": format!("{}/s.json", h.feed_base)}),
    );
    disabled["enabled"] = json!(false);
    h.create(disabled).await;
    // Never synced: due now; the disabled list is not scheduled.
    assert_eq!(h.engine.schedule_due().await.unwrap(), 1);
    assert_eq!(
        h.engine.schedule_due().await.unwrap(),
        0,
        "an active run blocks another"
    );
    h.engine.drain().await.unwrap();
    let (_, runs) = h
        .call("GET", &format!("/api/v1/importlist/{id}/runs"), Value::Null)
        .await;
    assert_eq!(
        (
            runs["items"][0]["origin"].as_str(),
            runs["items"][0]["status"].as_str()
        ),
        (Some("scheduled"), Some("succeeded"))
    );
    // Not due again until the interval elapses.
    assert_eq!(h.engine.schedule_due().await.unwrap(), 0);
    h.sql("UPDATE import_lists SET last_sync_at=last_sync_at-3500")
        .await;
    assert_eq!(h.engine.schedule_due().await.unwrap(), 0);
    h.sql("UPDATE import_lists SET last_sync_at=last_sync_at-200")
        .await;
    assert_eq!(h.engine.schedule_due().await.unwrap(), 1);
    h.engine.drain().await.unwrap();
    // After a failure the list is retried sooner than its (long) interval, but not immediately.
    h.feed.set("/s.json", 400, "text/plain", "no");
    h.sql("UPDATE import_lists SET interval_minutes=1440,last_sync_at=0")
        .await;
    assert_eq!(h.engine.schedule_due().await.unwrap(), 1);
    h.engine.drain().await.unwrap();
    assert_eq!(h.engine.schedule_due().await.unwrap(), 0);
    h.sql("UPDATE import_lists SET last_sync_at=last_sync_at-1900")
        .await;
    assert_eq!(h.engine.schedule_due().await.unwrap(), 1);
}

#[tokio::test]
async fn test_endpoint_fetches_like_a_sync_without_writing_and_reports_only_stable_classes() {
    let h = Harness::new().await;
    h.feed.json(
        "/t.json",
        json!([{"tvdb_id":1,"title":"A"},{"tvdb_id":2,"title":"B"}]),
    );
    let draft = h.input(
        "Draft",
        "custom",
        "tv",
        json!({"url": format!("{}/t.json", h.feed_base)}),
    );
    let (status, ok) = h
        .call(
            "POST",
            "/api/v1/importlist/test",
            json!({"draft": draft.clone()}),
        )
        .await;
    assert_eq!(
        (status, ok["ok"].clone(), ok["item_count"].clone()),
        (200, json!(true), json!(2)),
        "{ok}"
    );
    assert_eq!(
        h.count("SELECT count(*) FROM import_lists").await
            + h.count("SELECT count(*) FROM import_list_items").await
            + h.count("SELECT count(*) FROM series").await,
        0
    );
    // A failing source yields a class only: no URL, body or token echoed back.
    h.feed.set("/bad.json", 500, "text/plain", "SECRET-BODY");
    let mut bad = h.input(
        "Bad",
        "custom",
        "tv",
        json!({"url": format!("{}/bad.json", h.feed_base)}),
    );
    bad["secrets"] = json!({"bearer_token": "tok-XYZ"});
    let (_, failed) = h
        .call(
            "POST",
            "/api/v1/importlist/test",
            json!({"draft": bad.clone()}),
        )
        .await;
    assert_eq!(
        (failed["ok"].clone(), failed["error"].clone()),
        (json!(false), json!("server_error"))
    );
    assert!(
        !failed.to_string().contains("SECRET-BODY")
            && !failed.to_string().contains("tok-XYZ")
            && !failed.to_string().contains("bad.json")
    );
    assert_eq!(
        h.feed.requests().last().unwrap().header("authorization"),
        Some("Bearer tok-XYZ")
    );
    // A saved list is tested with its stored secret; a draft may reuse it by id.
    let id = h.create(bad.clone()).await;
    let (_, saved) = h
        .call(
            "POST",
            &format!("/api/v1/importlist/{id}/test"),
            Value::Null,
        )
        .await;
    assert_eq!(saved["error"], "server_error");
    assert_eq!(
        h.feed.requests().last().unwrap().header("authorization"),
        Some("Bearer tok-XYZ")
    );
    let mut reuse = bad.clone();
    reuse.as_object_mut().unwrap().remove("secrets");
    h.call(
        "POST",
        "/api/v1/importlist/test",
        json!({"draft": reuse, "existing_id": id}),
    )
    .await;
    assert_eq!(
        h.feed.requests().last().unwrap().header("authorization"),
        Some("Bearer tok-XYZ")
    );
    // A draft is still validated.
    assert_eq!(
        h.call(
            "POST",
            "/api/v1/importlist/test",
            json!({"draft": h.input("x", "custom", "tv", json!({}))})
        )
        .await
        .0,
        400
    );
}

#[tokio::test]
async fn rss_list_parses_release_titles_years_and_imdb_ids_and_rejects_non_feeds() {
    let h = Harness::new().await;
    h.meta
        .movie(7001, "Some Movie", Some(2019), Some("tt1234567"));
    h.meta.movie(7002, "Plain Title", Some(2021), None);
    h.meta.show(7101, "Some Show", Some(2018));
    h.feed.set(
        "/movies.xml",
        200,
        "application/rss+xml",
        r#"<?xml version="1.0"?><rss version="2.0"><channel><title>t</title>
        <item><title>Some.Movie.2019.1080p.BluRay.x264-GRP</title><link>https://example.test/title/tt1234567/</link></item>
        <item><title>Plain Title (2021)</title></item>
        <item><title>   </title></item>
        <item><description>no title</description></item></channel></rss>"#,
    );
    let id = h
        .create(h.input(
            "RSS movies",
            "rss",
            "movies",
            json!({"url": format!("{}/movies.xml", h.feed_base)}),
        ))
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
    let items = h.items(&id, "added").await;
    assert_eq!(items[0]["imdb_id"], "tt1234567");
    assert_eq!(items[0]["title"], "Some Movie");
    assert_eq!(items[0]["year"], 2019);
    assert_eq!(
        (items[1]["title"].clone(), items[1]["year"].clone()),
        (json!("Plain Title"), json!(2021))
    );

    h.feed.set(
        "/shows.atom",
        200,
        "application/atom+xml",
        r#"<feed xmlns="http://www.w3.org/2005/Atom"><entry><title>Some.Show.S02E03.720p.HDTV.x264-GRP</title><link href="https://example.test/x"/></entry></feed>"#,
    );
    let tv = h
        .create(h.input(
            "Atom shows",
            "rss",
            "tv",
            json!({"url": format!("{}/shows.atom", h.feed_base)}),
        ))
        .await;
    let run = h.sync(&tv).await;
    assert_eq!(
        (run["status"].as_str(), run["added"].as_i64()),
        (Some("succeeded"), Some(1)),
        "{run}"
    );
    assert_eq!(h.items(&tv, "added").await[0]["title"], "Some Show");

    for (label, body) in [
        ("html", "<html><body>no</body></html>"),
        ("truncated xml", "<rss><channel><item><title>x"),
        (
            "doctype entity bomb",
            "<!DOCTYPE r [<!ENTITY a \"aaaa\">]><rss><channel><item><title>&a;</title></item></channel></rss>",
        ),
        (
            "deep nesting",
            &format!("<rss>{}{}</rss>", "<a>".repeat(200), "</a>".repeat(200)),
        ),
    ] {
        h.feed.set("/bad.xml", 200, "application/xml", body);
        let bad = h
            .create(h.input(
                &format!("bad {label}"),
                "rss",
                "tv",
                json!({"url": format!("{}/bad.xml", h.feed_base)}),
            ))
            .await;
        let run = h.sync(&bad).await;
        assert_eq!(
            (run["status"].as_str(), run["error_code"].as_str()),
            (Some("failed"), Some("invalid_response")),
            "{label}: {run}"
        );
    }
}

#[tokio::test]
async fn background_scheduler_syncs_due_lists_without_a_manual_trigger_and_shuts_down() {
    let h = Harness::new().await;
    h.meta.show(8001, "Background", None);
    h.feed.json(
        "/bg.json",
        json!([{"tvdb_id": 8001, "title": "Background"}]),
    );
    let id = h
        .create(h.input(
            "BG",
            "custom",
            "tv",
            json!({"url": format!("{}/bg.json", h.feed_base)}),
        ))
        .await;
    let key = Some(std::sync::Arc::new(
        hrrdarr::providers::CredentialKey::from_hex(import_lists_support::KEY_HEX).unwrap(),
    ));
    let worker = hrrdarr::import_lists::start(h.db.clone(), key, h.deps.clone());
    let mut done = false;
    for _ in 0..60 {
        if h.count("SELECT count(*) FROM series").await == 1
            && h.count("SELECT count(*) FROM import_list_runs WHERE status='succeeded'")
                .await
                == 1
        {
            done = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    worker.shutdown().await;
    assert!(done, "the scheduler synced the new list on its own");
    let (_, runs) = h
        .call("GET", &format!("/api/v1/importlist/{id}/runs"), Value::Null)
        .await;
    assert_eq!(runs["items"][0]["origin"], "scheduled");
    assert_eq!(
        h.call("GET", &format!("/api/v1/importlist/{id}"), Value::Null)
            .await
            .1["last_error"],
        Value::Null
    );
}

#[tokio::test]
async fn unfinished_list_pass_yields_to_another_queued_sync() {
    let h = Harness::new().await;
    for i in 1..=30 {
        h.meta.show(1000 + i, &format!("Show {i}"), None);
    }
    h.feed.json("/large.json", feed_items(30));
    h.feed.json("/small.json", json!([]));
    let large = h
        .create(h.input(
            "Large",
            "custom",
            "tv",
            json!({"url": format!("{}/large.json", h.feed_base)}),
        ))
        .await;
    let small = h
        .create(h.input(
            "Small",
            "custom",
            "tv",
            json!({"url": format!("{}/small.json", h.feed_base)}),
        ))
        .await;
    for id in [&large, &small] {
        let (status, _) = h
            .call(
                "POST",
                &format!("/api/v1/importlist/{id}/sync"),
                Value::Null,
            )
            .await;
        assert_eq!(status, 202);
    }
    // Equal enqueue times exercise the tie that otherwise lets the oldest row monopolize passes.
    h.sql("UPDATE import_list_runs SET created_at=1,next_attempt_at=1")
        .await;
    assert_eq!(h.engine.tick(1).await.unwrap(), 1);
    assert_eq!(
        h.count("SELECT count(*) FROM import_list_runs WHERE status='queued' AND added=25")
            .await,
        1
    );
    assert_eq!(h.engine.tick(1).await.unwrap(), 1);
    let (_, runs) = h
        .call(
            "GET",
            &format!("/api/v1/importlist/{small}/runs"),
            Value::Null,
        )
        .await;
    assert_eq!(runs["items"][0]["status"], "succeeded");
    let (_, runs) = h
        .call(
            "GET",
            &format!("/api/v1/importlist/{large}/runs"),
            Value::Null,
        )
        .await;
    assert_eq!(runs["items"][0]["added"], 25);
    assert_eq!(runs["items"][0]["pending"], 5);
    assert_eq!(
        h.feed.requests().len(),
        2,
        "one fetch per source; continuation retains its snapshot"
    );
}
