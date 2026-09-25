use hrrdarr::{db::Database, quality_profiles};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("hrrdarr-profile-policy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
async fn request(
    client: &reqwest::Client,
    base: &str,
    method: &str,
    route: &str,
    body: Value,
) -> (u16, Value) {
    let response = client
        .request(method.parse().unwrap(), format!("{base}/api/v1{route}"))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    let body = response.text().await.unwrap();
    (
        status,
        serde_json::from_str(&body).unwrap_or_else(|_| panic!("{status}: {body}")),
    )
}
fn leaf(id: i64, allowed: bool) -> Value {
    json!({"kind":"quality","quality_id":id,"allowed":allowed})
}
fn graph() -> Value {
    json!({"name":"Policy fixture","items":[leaf(1,true),{"kind":"group","name":"HD","allowed":true,"items":[{"quality_id":3,"allowed":true},{"quality_id":7,"allowed":false}]}]})
}
fn policy(media: &str) -> Value {
    json!({"upgrade_allowed":false,"cutoff":{"kind":"group","position":1},"min_format_score":-10,"cutoff_format_score":-20,"min_upgrade_format_score":1,"language_id":if media=="movies" {json!(-2)} else {Value::Null},"format_items":[]})
}

#[tokio::test]
async fn native_policy_is_scoped_complete_and_atomic() {
    let scratch = Scratch::new();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let c = db.connect().await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = quality_profiles::router(db.clone())
        .merge(hrrdarr::library::router(db.clone()))
        .merge(hrrdarr::qualities::router(db.clone()))
        .merge(hrrdarr::media_files::router(db.clone()));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let mut retained = Vec::new();
    for media in ["tv", "movies"] {
        let collection = format!("/{media}/quality-profiles");
        let (code, legacy) = request(&client, &base, "POST", &collection, graph()).await;
        assert_eq!(code, 201, "{legacy}");
        assert!(legacy["policy"].is_null());
        let route = format!("{collection}/{}", legacy["id"]);
        let library_collection = if media == "tv" {
            "/tv/series"
        } else {
            "/movies"
        };
        let mut library_input = json!({"title":"Assigned library","path":scratch.0.join(media),"settings":{"quality_profile_id":legacy["id"]}});
        library_input[if media == "tv" { "tvdb_id" } else { "tmdb_id" }] = json!(123);
        let (code, library) =
            request(&client, &base, "POST", library_collection, library_input).await;
        assert_eq!(code, 201, "{library}");
        let library_route = format!("{library_collection}/{}", library["id"]);
        // These sibling routes previously worked alone but were shadowed by static library
        // prefixes when mounted together. Verify real reads and mutations in the merged router.
        let definition = format!("/{media}/quality-definitions/20");
        assert_eq!(
            request(&client, &base, "GET", &definition, Value::Null)
                .await
                .0,
            200
        );
        let (code, changed) = request(&client,&base,"PUT",&definition,json!({"id":20,"title":"Merged definition","min_size":5,"preferred_size":10,"max_size":20})).await;
        assert_eq!(code, 200, "{changed}");
        assert_eq!(changed["title"], "Merged definition");
        assert_eq!(
            request(&client, &base, "GET", &definition, Value::Null)
                .await
                .1,
            changed
        );
        let table = if media == "tv" {
            "episode_files"
        } else {
            "movie_files"
        };
        let owner = if media == "tv" {
            "series_id"
        } else {
            "movie_id"
        };
        c.execute(
            &format!("INSERT INTO {table}(id,{owner},path) VALUES(1,?,?)"),
            libsql::params![
                library["id"].as_i64().unwrap(),
                scratch.0.join(media).join("file.mkv").to_str().unwrap()
            ],
        )
        .await
        .unwrap();
        let files = format!("/{media}/files");
        let (code, page) = request(
            &client,
            &base,
            "GET",
            &format!("{files}?file_ids=1"),
            Value::Null,
        )
        .await;
        assert_eq!(code, 200, "{page}");
        assert_eq!(page["total"], 1);
        let (code, file) = request(
            &client,
            &base,
            "PUT",
            &format!("{files}/1"),
            json!({"release_group":"Single edit"}),
        )
        .await;
        assert_eq!(code, 200, "{file}");
        assert_eq!(file["release_group"], "Single edit");
        let (code, bulk) = request(
            &client,
            &base,
            "PUT",
            &format!("{files}/bulk"),
            json!({"files":[{"id":1,"release_group":"Bulk edit"}]}),
        )
        .await;
        assert_eq!(code, 200, "{bulk}");
        assert_eq!(
            request(&client, &base, "GET", &format!("{files}/1"), Value::Null)
                .await
                .1["release_group"],
            "Bulk edit"
        );
        for explicit_null in [false, true] {
            let mut input = graph();
            if explicit_null {
                input["policy"] = Value::Null;
            }
            let (code, result) = request(&client, &base, "PUT", &route, input).await;
            assert_eq!(code, 200, "{result}");
            assert!(result["policy"].is_null());
        }
        let mut input = graph();
        input["policy"] = policy(media);
        let (code, configured) = request(&client, &base, "PUT", &route, input.clone()).await;
        assert_eq!(code, 200, "{configured}");
        assert_eq!(configured["policy"], input["policy"]);
        // A disabled upgrade flag does not waive cutoff validity, and negative cutoff scores
        // need not be greater than minimum scores in either source contract.
        assert_eq!(configured["policy"]["upgrade_allowed"], false);
        for clear in [false, true] {
            let mut missing = graph();
            if clear {
                missing["policy"] = Value::Null;
            }
            assert_eq!(request(&client, &base, "PUT", &route, missing).await.0, 400);
            assert_eq!(
                request(&client, &base, "GET", &route, Value::Null).await.1,
                configured
            );
        }
        let mut cases = Vec::new();
        for cutoff in [
            json!({"kind":"quality","quality_id":3}),
            json!({"kind":"quality","quality_id":999999}),
            json!({"kind":"group","position":0}),
            json!({"kind":"group","position":99}),
            json!({"kind":"group","position":-1}),
        ] {
            let mut bad = input.clone();
            bad["policy"]["cutoff"] = cutoff;
            cases.push(bad);
        }
        let mut bad = input.clone();
        bad["items"][1]["allowed"] = json!(false);
        cases.push(bad);
        for (field, value) in [
            ("min_format_score", json!(1)),
            ("min_upgrade_format_score", json!(0)),
            ("min_upgrade_format_score", json!(2147483648_i64)),
            ("cutoff_format_score", json!(-2147483649_i64)),
            ("format_items", json!([{"format":1,"score":10}])),
            ("upgrade_allowed", Value::Null),
            ("unknown", json!(true)),
        ] {
            let mut bad = input.clone();
            bad["policy"][field] = value;
            cases.push(bad);
        }
        for language in if media == "tv" {
            vec![json!(0), json!(-2)]
        } else {
            vec![Value::Null, json!(-3), json!(58)]
        } {
            let mut bad = input.clone();
            bad["policy"]["language_id"] = language;
            cases.push(bad);
        }
        for bad in cases {
            let (code, error) = request(&client, &base, "PUT", &route, bad.clone()).await;
            assert_eq!(code, 400, "{bad}: {error}");
            assert_eq!(
                request(&client, &base, "GET", &route, Value::Null).await.1,
                configured
            );
        }
        if media == "movies" {
            for language in [-2, -1, 0, 1, 57] {
                input["policy"]["language_id"] = json!(language);
                let (code, result) = request(&client, &base, "PUT", &route, input.clone()).await;
                assert_eq!(code, 200, "{result}");
                assert_eq!(result["policy"]["language_id"], language);
            }
        }
        // Position is an input graph reference, never a global quality/group integer identity.
        input["items"].as_array_mut().unwrap().swap(0, 1);
        input["items"][0]["name"] = json!("Reordered HD");
        input["policy"]["cutoff"] = json!({"kind":"group","position":0});
        input["policy"]["min_format_score"] = json!(i32::MIN);
        input["policy"]["cutoff_format_score"] = json!(i32::MAX);
        input["policy"]["min_upgrade_format_score"] = json!(i32::MAX);
        let (code, reordered) = request(&client, &base, "PUT", &route, input.clone()).await;
        assert_eq!(code, 200, "{reordered}");
        assert_eq!(reordered["policy"], input["policy"]);
        assert_eq!(
            request(&client, &base, "GET", &library_route, Value::Null)
                .await
                .1["settings"]["quality_profile_id"],
            legacy["id"]
        );
        let row=c.query("SELECT g.profile_id,g.position,g.name FROM quality_profile_policies p JOIN quality_profile_groups g ON g.id=p.cutoff_group_id WHERE p.profile_id=?",[legacy["id"].as_i64().unwrap()]).await.unwrap().next().await.unwrap().unwrap();
        assert_eq!(row.get::<i64>(0).unwrap(), legacy["id"].as_i64().unwrap());
        assert_eq!(row.get::<i64>(1).unwrap(), 0);
        assert_eq!(row.get::<String>(2).unwrap(), "Reordered HD");
        drop(row);
        // Failure occurs after replacement removes old graph/policy rows and rebuilds the graph.
        c.execute_batch("CREATE TRIGGER reject_fixture_policy BEFORE INSERT ON quality_profile_policies BEGIN SELECT RAISE(ABORT,'late policy fixture'); END;").await.unwrap();
        let mut failed = input.clone();
        failed["name"] = json!("Must roll back");
        failed["items"][0]["name"] = json!("Must disappear");
        assert_eq!(request(&client, &base, "PUT", &route, failed).await.0, 500);
        assert_eq!(
            request(&client, &base, "GET", &route, Value::Null).await.1,
            reordered
        );
        assert_eq!(
            request(&client, &base, "GET", &library_route, Value::Null)
                .await
                .1["settings"]["quality_profile_id"],
            legacy["id"]
        );
        c.execute_batch("DROP TRIGGER reject_fixture_policy;")
            .await
            .unwrap();
        input["policy"]["cutoff"] = json!({"kind":"quality","quality_id":1});
        let (code, leaf_cutoff) = request(&client, &base, "PUT", &route, input).await;
        assert_eq!(code, 200, "{leaf_cutoff}");
        assert_eq!(
            leaf_cutoff["policy"]["cutoff"],
            json!({"kind":"quality","quality_id":1})
        );
        let opposite = if media == "tv" { "movies" } else { "tv" };
        assert_eq!(
            request(
                &client,
                &base,
                "GET",
                &format!("/{opposite}/quality-profiles/{}", legacy["id"]),
                Value::Null
            )
            .await
            .0,
            404
        );
        retained.push((route, leaf_cutoff));
    }
    server.abort();
    let _ = server.await;
    drop(c);
    drop(db);
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = quality_profiles::router(db.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    for (route, expected) in retained {
        assert_eq!(
            request(&client, &base, "GET", &route, Value::Null).await.1,
            expected
        );
    }
    server.abort();
    let _ = server.await;
    drop(db);
}
