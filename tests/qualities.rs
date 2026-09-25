use hrrdarr::{
    db::{Database, Error},
    qualities,
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
        let path = std::env::temp_dir().join(format!("hrrdarr-quality-{}", uuid::Uuid::new_v4()));
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
        stream.take(1024 * 1024).read_to_end(&mut bytes).unwrap();
        let response = String::from_utf8(bytes).unwrap();
        let (headers, body) = response.split_once("\r\n\r\n").unwrap();
        let status = headers.split_whitespace().nth(1).unwrap().parse().unwrap();
        (
            status,
            serde_json::from_str(body).expect("quality response must be JSON"),
        )
    })
    .await
    .unwrap()
}
fn edit(id: i64, title: &str, min: Value, max: Value, preferred: Value) -> Value {
    json!({"id":id,"title":title,"min_size":min,"max_size":max,"preferred_size":preferred})
}
fn route(media: &str, suffix: &str) -> String {
    format!("/api/v1/{media}/quality-definitions{suffix}")
}

#[tokio::test]
async fn real_http_quality_contract_both_domains_atomicity_and_persistence() -> Result<(), Error> {
    let files = Sandbox::new();
    let path = files.0.join("library.db");
    let db = Arc::new(Database::open_local(&path).await?);
    let conn = db.connect().await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let app = qualities::router(db.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut catalogs = Vec::new();
    for (media, expected, max) in [("tv", 22, 1000), ("movies", 30, 2000)] {
        let (code, catalog) = request(address, "GET", &route(media, ""), "").await;
        assert_eq!(code, 200);
        assert_eq!(catalog.as_array().unwrap().len(), expected);
        let rows = catalog.as_array().unwrap();
        assert!(
            rows.windows(2)
                .all(|p| (p[0]["weight"].as_i64(), p[0]["id"].as_i64())
                    <= (p[1]["weight"].as_i64(), p[1]["id"].as_i64()))
        );
        assert_eq!(
            request(address, "GET", &route(media, "/defaults"), "")
                .await
                .1,
            catalog
        );
        assert_eq!(
            request(address, "GET", &route(media, "/limits"), "").await,
            (200, json!({"min":0,"max":max,"unit":"MiB/minute"}))
        );
        assert_eq!(
            request(address, "GET", &route(media, "/11"), "").await.0,
            404
        ); // No invented removed identity.
        assert_eq!(
            request(address, "GET", &route(media, "/bad"), "").await.0,
            400
        );
        let detail = request(address, "GET", &route(media, "/20"), "").await.1;
        assert_eq!(
            detail["quality"]["name"],
            if media == "tv" {
                "Bluray-1080p Remux"
            } else {
                "Bluray-480p"
            }
        );
        let movie = media == "movies";
        assert_eq!(
            detail["min_size"],
            if movie { json!(0.) } else { json!(35.) }
        );
        assert_eq!(
            detail["max_size"],
            if movie { json!(100.) } else { Value::Null }
        );
        assert_eq!(
            detail["quality"]["modifier"],
            if movie { json!("none") } else { Value::Null }
        );
        assert_eq!(
            request(address, "GET", &route(media, "/0"), "").await.1["quality"]["name"],
            "Unknown"
        );
        let data = edit(20, "Custom", json!(5), json!(150), json!(100));
        let (status, result) =
            request(address, "PUT", &route(media, "/20"), &data.to_string()).await;
        assert_eq!(status, 200);
        assert_eq!(result["title"], "Custom");
        assert_eq!(result["min_size"], 5.);
        assert_eq!(
            request(address, "GET", &route(media, "/defaults"), "")
                .await
                .1,
            catalog
        );
        catalogs.push(catalog);
    }
    // Same quality id deliberately means different qualities in each domain.
    let movie_before = request(address, "GET", &route("movies", "/20"), "").await.1;
    request(
        address,
        "PUT",
        &route("tv", "/20"),
        &edit(20, "TV only", json!(6), json!(180), json!(110)).to_string(),
    )
    .await;
    assert_eq!(
        request(address, "GET", &route("movies", "/20"), "").await.1,
        movie_before
    );
    for (media, limit) in [("tv", 1000.), ("movies", 2000.)] {
        let base = route(media, "/20");
        for payload in [
            edit(20, "", json!(0), json!(100), json!(95)),
            edit(20, "Bad", json!(-1), json!(100), json!(95)),
            edit(20, "Bad", json!(0), json!(limit + 0.1), json!(95)),
            edit(20, "Bad", json!(101), json!(100), Value::Null),
            edit(20, "Bad", json!(50), json!(100), json!(40)),
            edit(20, "Bad", json!(0), json!(100), json!(101)),
            edit(21, "Mismatch", json!(0), json!(100), json!(95)),
            json!({"id":20,"title":"Attempt identity edit","weight":999}),
            json!({"id":20,"title":"Attempt identity edit","quality":{"id":1}}),
        ] {
            let before = request(address, "GET", &base, "").await.1;
            assert_eq!(
                request(address, "PUT", &base, &payload.to_string()).await.0,
                400
            );
            assert_eq!(request(address, "GET", &base, "").await.1, before);
        }
        for body in [
            r#"{"id":20,"title":"Bad","min_size":NaN}"#,
            r#"{"id":20,"title":"Bad","min_size":1e999}"#,
            "null",
        ] {
            assert_eq!(request(address, "PUT", &base, body).await.0, 400);
        }
        let unbounded = edit(20, "Null bounds", Value::Null, Value::Null, Value::Null);
        assert_eq!(
            request(address, "PUT", &base, &unbounded.to_string())
                .await
                .0,
            200
        );
        let boundary = edit(20, "Boundary", json!(limit), json!(limit), json!(limit));
        assert_eq!(
            request(address, "PUT", &base, &boundary.to_string())
                .await
                .0,
            200
        );
        let before = request(address, "GET", &route(media, ""), "").await.1;
        let good = edit(0, "Atomic", json!(1), json!(10), json!(5));
        for body in [
            json!([]),
            json!([good, good]),
            json!([good, edit(11, "Missing", json!(1), json!(10), json!(5))]),
            json!([good, edit(20, "Invalid", json!(11), json!(10), json!(5))]),
        ] {
            assert_ne!(
                request(address, "PUT", &route(media, "/bulk"), &body.to_string())
                    .await
                    .0,
                200
            );
            assert_eq!(
                request(address, "GET", &route(media, ""), "").await.1,
                before
            );
        }
        let batch = json!([good, edit(20, "Updated", json!(2), json!(20), json!(10))]);
        let response = request(address, "PUT", &route(media, "/bulk"), &batch.to_string()).await;
        assert_eq!(response.0, 200);
        assert!(
            response
                .1
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["id"] == 20 && r["min_size"] == 2.)
        );
        let current = request(address, "GET", &route(media, "/20"), "").await.1;
        assert_eq!(
            request(address, "POST", &route(media, "/reset"), "{}")
                .await
                .0,
            200
        );
        let reset = request(address, "GET", &route(media, "/20"), "").await.1;
        assert_eq!(reset["title"], "Updated");
        assert_eq!(
            reset["min_size"],
            if media == "tv" {
                current["min_size"].clone()
            } else {
                json!(0.)
            }
        );
        assert_eq!(
            request(
                address,
                "POST",
                &route(media, "/reset"),
                r#"{"reset_titles":true}"#
            )
            .await
            .0,
            200
        );
        let titled = request(address, "GET", &route(media, "/20"), "").await.1;
        assert_eq!(titled["title"], titled["quality"]["name"]);
        assert_eq!(titled["min_size"], reset["min_size"]);
    }
    assert_eq!(
        request(address, "PUT", &route("tv", "/20"), "null").await.1["error"]["code"],
        "invalid_request"
    );
    assert_eq!(
        request(address, "GET", &route("music", ""), "").await.0,
        404
    );
    assert_eq!(
        request(address, "PUT", &route("tv", "/20"), &"x".repeat(33000))
            .await
            .0,
        413
    );
    assert_eq!(
        request(
            address,
            "PUT",
            &route("tv", "/bulk"),
            &serde_json::to_string(&vec![
                edit(
                    0,
                    "Repeated",
                    Value::Null,
                    Value::Null,
                    Value::Null
                );
                65
            ])?
        )
        .await
        .0,
        400
    );
    // Late database failure verifies rollback beyond request prevalidation. Error values stay private.
    conn.execute_batch("CREATE TRIGGER quality_failure BEFORE UPDATE OF title ON quality_definitions WHEN NEW.quality_id=20 BEGIN SELECT RAISE(ABORT,'SENTINEL_PRIVATE_SQL'); END;").await?;
    let before = request(address, "GET", &route("movies", ""), "").await.1;
    let batch = json!([
        edit(0, "Should roll back", json!(1), json!(10), json!(5)),
        edit(20, "Fail", json!(1), json!(10), json!(5))
    ]);
    let (status, error) = request(
        address,
        "PUT",
        &route("movies", "/bulk"),
        &batch.to_string(),
    )
    .await;
    assert_eq!(status, 500);
    assert_eq!(error["error"]["code"], "database_error");
    assert!(!error.to_string().contains("SENTINEL_PRIVATE_SQL"));
    assert_eq!(
        request(address, "GET", &route("movies", ""), "").await.1,
        before
    );
    conn.execute("DROP TRIGGER quality_failure", ()).await?;
    assert!(
        conn.execute(
            "UPDATE quality_definitions SET quality_id=99 WHERE media_type='tv' AND quality_id=20",
            ()
        )
        .await
        .is_err()
    );
    assert!(conn.execute("UPDATE quality_definitions SET min_size=2001 WHERE media_type='movies' AND quality_id=20",()).await.is_err());
    assert!(conn.execute("UPDATE quality_definitions SET min_size=100,max_size=1,preferred_size=NULL WHERE media_type='tv' AND quality_id=20",()).await.is_err());
    server.abort();
    let _ = server.await;
    drop(conn);
    drop(db);
    let reopened = Database::open_local(&path).await?;
    let c = reopened.connect().await?;
    let row=c.query("SELECT min_size,title FROM quality_definitions WHERE media_type='tv' AND quality_id=20",()).await?.next().await?.unwrap();
    assert_eq!(row.get::<f64>(0)?, 2.);
    assert_eq!(row.get::<String>(1)?, "Bluray-1080p Remux");
    assert_eq!(
        c.query("SELECT count(*) FROM quality_definitions", ())
            .await?
            .next()
            .await?
            .unwrap()
            .get::<i64>(0)?,
        52
    );
    // New migration rolls back table, immutable trigger and all seed rows together.
    let raw = libsql::Builder::new_local(":memory:").build().await?;
    let c = raw.connect()?;
    let tx = c.transaction().await?;
    tx.execute_batch(include_str!("../migrations/0004_quality_definitions.sql"))
        .await?;
    assert!(
        tx.execute("UPDATE quality_definitions SET weight=99", ())
            .await
            .is_err()
    );
    tx.rollback().await?;
    assert_eq!(
        c.query("SELECT count(*) FROM sqlite_schema", ())
            .await?
            .next()
            .await?
            .unwrap()
            .get::<i64>(0)?,
        0
    );
    Ok(())
}

fn profile_route(media: &str, id: Option<i64>) -> String {
    format!(
        "/api/v1/{media}/quality-profiles{}",
        id.map(|id| format!("/{id}")).unwrap_or_default()
    )
}
fn leaf(id: i64, allowed: bool) -> Value {
    // Use the API's floating-point representation when comparing complete response objects.
    json!({"quality_id":id,"allowed":allowed,"min_size":1.0,"max_size":10.0,"preferred_size":5.0})
}
fn leaves(profile: &Value) -> std::collections::BTreeMap<i64, Value> {
    let mut result = std::collections::BTreeMap::new();
    for node in profile["items"].as_array().unwrap() {
        if node["kind"] == "quality" {
            result.insert(node["quality_id"].as_i64().unwrap(), node.clone());
        } else {
            for item in node["items"].as_array().unwrap() {
                result.insert(item["quality_id"].as_i64().unwrap(), item.clone());
            }
        }
    }
    result
}
fn with_direct_kind(mut value: Value) -> Value {
    value["kind"] = json!("quality");
    value
}

#[tokio::test]
async fn tv_definition_propagates_atomically_to_persisted_direct_and_grouped_profiles()
-> Result<(), Error> {
    let files = Sandbox::new();
    let path = files.0.join("profiles.db");
    let db = Arc::new(Database::open_local(&path).await?);
    let conn = db.connect().await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let app = qualities::router(db.clone()).merge(hrrdarr::quality_profiles::router(db.clone()));
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let a = json!({"name":"A","items":[with_direct_kind(leaf(20,true)),{"kind":"group","name":"Web","allowed":true,"items":[leaf(3,false)]}]});
    let b = json!({"name":"B","items":[{"kind":"group","name":"Remux","allowed":false,"items":[leaf(20,false)]},with_direct_kind(leaf(3,true))]});
    let mut profiles = Vec::new();
    for (media, input) in [("tv", &a), ("tv", &b), ("movies", &a)] {
        let (status, created) = request(
            address,
            "POST",
            &profile_route(media, None),
            &input.to_string(),
        )
        .await;
        assert_eq!(status, 201, "{created}");
        assert_eq!(created["items"], input["items"]);
        let id = created["id"].as_i64().unwrap();
        assert_eq!(
            request(address, "GET", &profile_route(media, Some(id)), "")
                .await
                .1,
            created
        );
        profiles.push((media, id, created));
    }
    let (_, page) = request(
        address,
        "GET",
        &format!("{}?limit=1&offset=1", profile_route("tv", None)),
        "",
    )
    .await;
    for query in ["?limit=0", "?limit=101", "?offset=-1", "?offset=bad"] {
        let (code, error) = request(
            address,
            "GET",
            &format!("{}{query}", profile_route("tv", None)),
            "",
        )
        .await;
        assert_eq!(code, 400);
        assert_eq!(error["error"]["code"], "invalid_request");
    }
    assert_eq!(page["total"], 2);
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    assert_eq!(page["items"][0]["id"], profiles[1].1);
    assert_eq!(page["items"][0]["group_count"], 1);
    assert_eq!(page["items"][0]["item_count"], 2);
    assert_eq!(
        request(
            address,
            "GET",
            &profile_route("movies", Some(profiles[0].1)),
            ""
        )
        .await
        .0,
        404
    );
    assert_eq!(
        request(address, "POST", &profile_route("tv", None), &a.to_string())
            .await
            .0,
        409
    );
    for bad in [
        json!({"name":"Duplicate","items":[with_direct_kind(leaf(20,true)),{"kind":"group","name":"Again","allowed":true,"items":[leaf(20,false)]}]}),
        json!({"name":"Unknown","items":[with_direct_kind(leaf(11,true))]}),
        json!({"name":"Empty","items":[]}),
        json!({"name":"Empty group","items":[{"kind":"group","name":"Empty","allowed":true,"items":[]}]}),
        json!({"name":"Unknown shape","items":[{"kind":"quality","quality_id":20,"allowed":true,"unexpected":true}]}),
        json!({"name":"Nested","items":[{"kind":"group","name":"Nested","allowed":true,"items":[{"kind":"group","items":[]}]}]}),
    ] {
        assert_eq!(
            request(
                address,
                "POST",
                &profile_route("tv", None),
                &bad.to_string()
            )
            .await
            .0,
            400
        );
    }
    // Both nullable values must overwrite leaf overrides when any size is present.
    let partial = edit(20, "Edited", Value::Null, Value::Null, json!(50));
    assert_eq!(
        request(address, "PUT", &route("tv", "/20"), &partial.to_string())
            .await
            .0,
        200
    );
    let mut after = Vec::new();
    for (media, id, original) in &profiles {
        let updated = request(address, "GET", &profile_route(media, Some(*id)), "")
            .await
            .1;
        let indexed = leaves(&updated);
        if *media == "tv" {
            assert_eq!(indexed[&20]["min_size"], Value::Null);
            assert_eq!(indexed[&20]["max_size"], Value::Null);
            assert_eq!(indexed[&20]["preferred_size"], 50.);
            // Only the matching leaf sizes change, never order/group metadata/allowed flags/other leaves.
            let mut expected = original.clone();
            for node in expected["items"].as_array_mut().unwrap() {
                if node["kind"] == "quality" && node["quality_id"] == 20 {
                    node["min_size"] = Value::Null;
                    node["max_size"] = Value::Null;
                    node["preferred_size"] = json!(50.);
                }
                if node["kind"] == "group" {
                    for l in node["items"].as_array_mut().unwrap() {
                        if l["quality_id"] == 20 {
                            l["min_size"] = Value::Null;
                            l["max_size"] = Value::Null;
                            l["preferred_size"] = json!(50.);
                        }
                    }
                }
            }
            assert_eq!(updated, expected);
        } else {
            assert_eq!(&updated, original);
        }
        after.push(updated);
    }
    let all_null = edit(20, "All null", Value::Null, Value::Null, Value::Null);
    assert_eq!(
        request(address, "PUT", &route("tv", "/20"), &all_null.to_string())
            .await
            .0,
        200
    );
    assert_eq!(
        request(
            address,
            "PUT",
            &route("movies", "/20"),
            &partial.to_string()
        )
        .await
        .0,
        200
    );
    for ((media, id, _), expected) in profiles.iter().zip(&after) {
        assert_eq!(
            request(address, "GET", &profile_route(media, Some(*id)), "")
                .await
                .1,
            *expected
        );
    }
    // Bulk independently filters all-null definitions but propagates every field for the others.
    let batch = json!([all_null, edit(3, "Web", Value::Null, json!(88), json!(66))]);
    assert_eq!(
        request(address, "PUT", &route("tv", "/bulk"), &batch.to_string())
            .await
            .0,
        200
    );
    for (media, id, _) in &profiles {
        let updated = request(address, "GET", &profile_route(media, Some(*id)), "")
            .await
            .1;
        if *media == "tv" {
            let indexed = leaves(&updated);
            assert_eq!(indexed[&20]["preferred_size"], 50.);
            assert_eq!(indexed[&3]["min_size"], Value::Null);
            assert_eq!(indexed[&3]["max_size"], 88.);
            assert_eq!(indexed[&3]["preferred_size"], 66.);
        } else {
            assert_eq!(updated, after[2]);
        }
    }
    let mut stable = Vec::new();
    for (media, id, _) in &profiles {
        stable.push(
            request(address, "GET", &profile_route(media, Some(*id)), "")
                .await
                .1,
        );
    }
    for media in ["tv", "movies"] {
        assert_eq!(
            request(
                address,
                "POST",
                &route(media, "/reset"),
                r#"{"reset_titles":true}"#
            )
            .await
            .0,
            200
        );
    }
    for ((media, id, _), expected) in profiles.iter().zip(&stable) {
        assert_eq!(
            request(address, "GET", &profile_route(media, Some(*id)), "")
                .await
                .1,
            *expected
        );
    }
    // A late profile write failure must roll back both definition edits and earlier leaf writes.
    conn.execute_batch(&format!("CREATE TRIGGER propagation_failure BEFORE UPDATE ON quality_profile_items WHEN NEW.profile_id={} AND NEW.quality_id=20 BEGIN SELECT RAISE(ABORT,'PRIVATE_PROFILE_FAILURE'); END;",profiles[1].1)).await?;
    let definitions = request(address, "GET", &route("tv", ""), "").await.1;
    let failing = json!([
        edit(3, "Earlier definition", json!(2), json!(90), json!(50)),
        edit(20, "Fails later", json!(3), json!(90), json!(60))
    ]);
    let (status, error) =
        request(address, "PUT", &route("tv", "/bulk"), &failing.to_string()).await;
    assert_eq!(status, 500);
    assert_eq!(error["error"]["code"], "database_error");
    assert!(!error.to_string().contains("PRIVATE_PROFILE_FAILURE"));
    assert_eq!(
        request(address, "GET", &route("tv", ""), "").await.1,
        definitions
    );
    for ((media, id, _), expected) in profiles.iter().zip(&stable) {
        assert_eq!(
            request(address, "GET", &profile_route(media, Some(*id)), "")
                .await
                .1,
            *expected
        );
    }
    conn.execute("DROP TRIGGER propagation_failure", ()).await?;
    // Replacement deletes/rebuilds ordering in one transaction; failed rebuild restores all old state.
    conn.execute_batch("CREATE TRIGGER replacement_failure BEFORE INSERT ON quality_profile_items WHEN NEW.quality_id=3 BEGIN SELECT RAISE(ABORT,'PRIVATE_REPLACEMENT_FAILURE'); END;").await?;
    let replacement = json!({"name":"Renamed","items":[{"kind":"group","name":"Changed","allowed":true,"items":[leaf(20,true),leaf(3,false)]}]});
    assert_eq!(
        request(
            address,
            "PUT",
            &profile_route("tv", Some(profiles[0].1)),
            &replacement.to_string()
        )
        .await
        .0,
        500
    );
    assert_eq!(
        request(
            address,
            "GET",
            &profile_route("tv", Some(profiles[0].1)),
            ""
        )
        .await
        .1,
        stable[0]
    );
    conn.execute("DROP TRIGGER replacement_failure", ()).await?;
    let (status, replaced) = request(
        address,
        "PUT",
        &profile_route("tv", Some(profiles[0].1)),
        &replacement.to_string(),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(replaced["name"], "Renamed");
    assert_eq!(replaced["items"], replacement["items"]);
    assert!(
        conn.execute(
            "UPDATE quality_profile_items SET media_type='movies' WHERE profile_id=?1",
            libsql::params![profiles[0].1]
        )
        .await
        .is_err()
    );
    assert!(conn.execute("UPDATE quality_profile_items SET group_id=(SELECT id FROM quality_profile_groups WHERE profile_id=?1 LIMIT 1) WHERE profile_id=?2",libsql::params![profiles[1].1,profiles[0].1]).await.is_err());
    // Cross-table position collisions are diagnosed instead of silently hiding a root item.
    conn.execute(
        "UPDATE quality_profile_groups SET position=1 WHERE profile_id=?1",
        libsql::params![profiles[1].1],
    )
    .await?;
    let (code, error) = request(
        address,
        "GET",
        &profile_route("tv", Some(profiles[1].1)),
        "",
    )
    .await;
    assert_eq!(code, 500);
    assert_eq!(error["error"]["code"], "profile_integrity");
    conn.execute(
        "UPDATE quality_profile_groups SET position=0 WHERE profile_id=?1",
        libsql::params![profiles[1].1],
    )
    .await?;
    server.abort();
    let _ = server.await;
    drop(conn);
    drop(db);
    let reopened = Database::open_local(&path).await?;
    let c = reopened.connect().await?;
    let row=c.query("SELECT min_size,max_size,preferred_size,allowed FROM quality_profile_items WHERE profile_id=?1 AND quality_id=20",libsql::params![profiles[1].1]).await?.next().await?.unwrap();
    assert_eq!(row.get::<Option<f64>>(0)?, None);
    assert_eq!(row.get::<Option<f64>>(1)?, None);
    assert_eq!(row.get::<f64>(2)?, 50.);
    assert_eq!(row.get::<i64>(3)?, 0);
    assert_eq!(
        c.query(
            "SELECT name FROM quality_profiles WHERE id=?1",
            libsql::params![profiles[0].1]
        )
        .await?
        .next()
        .await?
        .unwrap()
        .get::<String>(0)?,
        "Renamed"
    );
    Ok(())
}
