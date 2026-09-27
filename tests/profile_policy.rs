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
        if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&body).unwrap_or_else(|_| panic!("{status}: {body}"))
        },
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

async fn scalar(c: &libsql::Connection, sql: &str) -> i64 {
    c.query(sql, ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}
fn ordered_qualities(input: &Value) -> Vec<Vec<i64>> {
    input["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| {
            if node["kind"] == "quality" {
                vec![node["quality_id"].as_i64().unwrap()]
            } else {
                node["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|leaf| leaf["quality_id"].as_i64().unwrap())
                    .collect()
            }
        })
        .collect()
}
fn allowed_qualities(input: &Value) -> Vec<i64> {
    input["items"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|node| {
            if node["kind"] == "quality" {
                if node["allowed"] == true {
                    vec![node["quality_id"].as_i64().unwrap()]
                } else {
                    vec![]
                }
            } else {
                node["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|leaf| leaf["allowed"] == true)
                    .map(|leaf| {
                        assert_eq!(node["allowed"], true);
                        leaf["quality_id"].as_i64().unwrap()
                    })
                    .collect()
            }
        })
        .collect()
}
#[tokio::test]
async fn profile_drafts_defaults_and_deletion_are_scoped_atomic_and_preserve_settings() {
    let scratch = Scratch::new();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let c = db.connect().await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = quality_profiles::router(db.clone())
        .merge(hrrdarr::library::router(db.clone()))
        .merge(hrrdarr::custom_formats::router(db.clone()));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let specs=json!([{"name":"English","negate":false,"required":true,"condition":{"kind":"language","value":1,"except_language":false}}]).to_string();
    c.execute("INSERT INTO custom_formats(media_type,name,include_when_renaming,specifications_json) VALUES('tv','Zero',0,?)",[specs]).await.unwrap();
    // Draft sizes use immutable defaults even when the global editable size settings changed.
    c.execute("UPDATE quality_definitions SET min_size=7,max_size=20,preferred_size=15 WHERE media_type='tv' AND quality_id=1",()).await.unwrap();
    let tv_order = vec![
        vec![0],
        vec![1],
        vec![12, 8],
        vec![2],
        vec![13],
        vec![22],
        vec![4],
        vec![9],
        vec![10],
        vec![14, 5],
        vec![6],
        vec![15, 3],
        vec![7],
        vec![20],
        vec![16],
        vec![17, 18],
        vec![19],
        vec![21],
    ];
    let movie_order = vec![
        vec![0],
        vec![24],
        vec![25],
        vec![26],
        vec![27],
        vec![29],
        vec![28],
        vec![1],
        vec![2],
        vec![23],
        vec![8, 12],
        vec![20],
        vec![21],
        vec![4],
        vec![5, 14],
        vec![6],
        vec![9],
        vec![3, 15],
        vec![7],
        vec![30],
        vec![16],
        vec![18, 17],
        vec![19],
        vec![31],
        vec![22],
        vec![10],
    ];
    for media in ["tv", "movies"] {
        let path = format!("/{media}/quality-profiles");
        let (code, mut draft) = request(
            &client,
            &base,
            "GET",
            &format!("{path}/schema"),
            Value::Null,
        )
        .await;
        assert_eq!(code, 200);
        assert_eq!(draft["name"], "");
        assert!(allowed_qualities(&draft).is_empty());
        assert_eq!(
            ordered_qualities(&draft),
            if media == "tv" {
                tv_order.clone()
            } else {
                movie_order.clone()
            }
        );
        assert_eq!(
            draft["policy"]["cutoff"],
            json!({"kind":"quality","quality_id":0})
        );
        assert_eq!(
            draft["policy"]["language_id"],
            if media == "tv" {
                Value::Null
            } else {
                json!(-2)
            }
        );
        assert_eq!(draft["policy"]["upgrade_allowed"], false);
        assert_eq!(draft["policy"]["min_format_score"], 0);
        assert_eq!(draft["policy"]["cutoff_format_score"], 0);
        assert_eq!(draft["policy"]["min_upgrade_format_score"], 1);
        assert_eq!(
            draft["policy"]["format_items"].as_array().unwrap().len(),
            usize::from(media == "tv")
        );
        assert_eq!(scalar(&c, "SELECT count(*) FROM quality_profiles").await, 0);
        assert_eq!(
            request(&client, &base, "POST", &path, draft.clone())
                .await
                .0,
            400
        );
        let node = draft["items"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|item| item["quality_id"] == 1)
            .unwrap();
        if media == "tv" {
            assert_eq!(node["min_size"], 2.0);
            assert_eq!(node["max_size"], 100.0);
            assert_eq!(node["preferred_size"], 95.0)
        } else {
            assert!(node["min_size"].is_null());
            assert!(node["max_size"].is_null());
            assert!(node["preferred_size"].is_null())
        }
        node["allowed"] = json!(true);
        draft["name"] = json!("Edited draft");
        draft["policy"]["cutoff"] = json!({"kind":"quality","quality_id":1});
        let (code, profile) = request(&client, &base, "POST", &path, draft).await;
        assert_eq!(code, 201, "{profile}");
        let route = format!("{path}/{}", profile["id"]);
        let opposite = if media == "tv" { "movies" } else { "tv" };
        assert_eq!(
            request(
                &client,
                &base,
                "DELETE",
                &format!("/{opposite}/quality-profiles/{}", profile["id"]),
                Value::Null
            )
            .await
            .0,
            404
        );
        assert_eq!(
            request(&client, &base, "GET", &route, Value::Null).await.1,
            profile
        );
        let mut library = json!({"title":"Kept library","path":scratch.0.join(media),"settings":{"quality_profile_id":profile["id"]}});
        library[if media == "tv" { "tvdb_id" } else { "tmdb_id" }] = json!(321);
        let library_path = if media == "tv" {
            "/tv/series"
        } else {
            "/movies"
        };
        let (code, _) = request(&client, &base, "POST", library_path, library).await;
        assert_eq!(code, 201);
        let (code, error) = request(&client, &base, "DELETE", &route, Value::Null).await;
        assert_eq!(code, 409);
        assert_eq!(error["error"]["code"], "profile_in_use");
        c.execute(
            "UPDATE library_settings SET quality_profile_id=NULL WHERE media_type=?",
            [media],
        )
        .await
        .unwrap();
        c.execute_batch("CREATE TRIGGER fail_profile_delete BEFORE DELETE ON quality_profiles BEGIN SELECT RAISE(ABORT,'injected');END").await.unwrap();
        assert_eq!(
            request(&client, &base, "DELETE", &route, Value::Null)
                .await
                .0,
            500
        );
        assert_eq!(
            request(&client, &base, "GET", &route, Value::Null).await.1,
            profile
        );
        c.execute_batch("DROP TRIGGER fail_profile_delete")
            .await
            .unwrap();
        assert_eq!(
            request(&client, &base, "DELETE", &route, Value::Null).await,
            (204, Value::Null)
        );
        assert_eq!(
            request(&client, &base, "GET", &route, Value::Null).await.0,
            404
        );
        for table in [
            "quality_profiles",
            "quality_profile_groups",
            "quality_profile_items",
            "quality_profile_policies",
            "quality_profile_format_scores",
        ] {
            assert_eq!(
                scalar(&c, &format!("SELECT count(*) FROM {table}")).await,
                0,
                "{table}"
            )
        }
    }
    assert_eq!(scalar(&c, "SELECT count(*) FROM custom_formats").await, 1);
    c.execute_batch("CREATE TRIGGER fail_profile_seed BEFORE INSERT ON quality_profiles WHEN NEW.media_type='movies' AND NEW.name='SD' BEGIN SELECT RAISE(ABORT,'injected');END").await.unwrap();
    assert!(quality_profiles::initialize_defaults(&db).await.is_err());
    assert_eq!(scalar(&c, "SELECT count(*) FROM quality_profiles").await, 0);
    c.execute_batch("DROP TRIGGER fail_profile_seed")
        .await
        .unwrap();
    quality_profiles::initialize_defaults(&db).await.unwrap();
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM quality_profiles").await,
        12
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM library_settings WHERE quality_profile_id IS NULL"
        )
        .await,
        2
    );
    for media in ["tv", "movies"] {
        let path = format!("/{media}/quality-profiles");
        let page = request(&client, &base, "GET", &path, Value::Null).await.1;
        let expected_names = [
            "Any",
            "SD",
            "HD-720p",
            "HD-1080p",
            "Ultra-HD",
            "HD - 720p/1080p",
        ];
        // Query persisted allocation order; API list intentionally sorts names.
        let mut rows = c
            .query(
                "SELECT id,name FROM quality_profiles WHERE media_type=? ORDER BY id",
                [media],
            )
            .await
            .unwrap();
        let mut persisted = vec![];
        while let Some(row) = rows.next().await.unwrap() {
            persisted.push((row.get::<i64>(0).unwrap(), row.get::<String>(1).unwrap()))
        }
        drop(rows);
        assert_eq!(
            persisted
                .iter()
                .map(|(_, name)| name.as_str())
                .collect::<Vec<_>>(),
            expected_names
        );
        assert_eq!(page["total"], 6);
        let expected: Vec<Vec<i64>> = if media == "tv" {
            vec![
                vec![1, 12, 8, 2, 13, 22, 4, 9, 14, 5, 6, 15, 3, 7],
                vec![1, 12, 8, 2, 13, 22],
                vec![4, 14, 5, 6],
                vec![9, 15, 3, 7],
                vec![16, 17, 18, 19],
                vec![4, 9, 14, 5, 6, 15, 3, 7],
            ]
        } else {
            vec![
                vec![
                    24, 25, 26, 27, 29, 28, 1, 2, 23, 8, 12, 20, 21, 4, 5, 14, 6, 9, 3, 15, 7, 30,
                    16, 18, 17, 19, 31, 22,
                ],
                vec![24, 25, 26, 27, 29, 28, 1, 2, 8, 12, 20, 21],
                vec![4, 5, 14, 6],
                vec![9, 3, 15, 7, 30],
                vec![16, 18, 17, 19, 31],
                vec![4, 5, 14, 6, 9, 3, 15, 7, 30],
            ]
        };
        let cutoffs = if media == "tv" {
            [1, 1, 4, 9, 16, 4]
        } else {
            [20, 20, 6, 7, 31, 6]
        };
        for (index, (id, _)) in persisted.iter().enumerate() {
            let p = request(&client, &base, "GET", &format!("{path}/{id}"), Value::Null)
                .await
                .1;
            assert_eq!(
                ordered_qualities(&p),
                if media == "tv" {
                    tv_order.clone()
                } else {
                    movie_order.clone()
                }
            );
            assert_eq!(allowed_qualities(&p), expected[index]);
            assert_eq!(p["policy"]["cutoff"]["quality_id"], cutoffs[index]);
            assert_eq!(p["policy"]["upgrade_allowed"], false);
            assert_eq!(p["policy"]["min_format_score"], 0);
            assert_eq!(p["policy"]["cutoff_format_score"], 0);
            assert_eq!(p["policy"]["min_upgrade_format_score"], 1);
        }
    }
    // A partially populated domain is configured; missing names are not repaired.
    c.execute(
        "DELETE FROM quality_profiles WHERE media_type='tv' AND name!='SD'",
        (),
    )
    .await
    .unwrap();
    c.execute(
        "UPDATE quality_profiles SET name='Local preserved' WHERE media_type='tv'",
        (),
    )
    .await
    .unwrap();
    quality_profiles::initialize_defaults(&db).await.unwrap();
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM quality_profiles WHERE media_type='tv'"
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM quality_profiles WHERE name='Local preserved'"
        )
        .await,
        1
    );
    c.execute("DELETE FROM quality_profiles WHERE media_type='movies'", ())
        .await
        .unwrap();
    quality_profiles::initialize_defaults(&db).await.unwrap();
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM quality_profiles WHERE media_type='movies'"
        )
        .await,
        6
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM quality_profiles WHERE media_type='tv'"
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM library_settings WHERE quality_profile_id IS NULL"
        )
        .await,
        2
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM pragma_foreign_key_check").await,
        0
    );

    // Historical IDs are global per entity table, including mappings from the opposite domain.
    c.execute("INSERT INTO snapshot_imports(application,fingerprint,schema_version) VALUES('sonarr','identity-limit',233)",()).await.unwrap();
    for maximum in [9_007_199_254_740_991i64, i64::MAX] {
        for table in ["quality_profiles", "custom_formats"] {
            c.execute("INSERT INTO snapshot_mappings(application,fingerprint,destination_table,source_id,destination_id) VALUES('sonarr','identity-limit',?,1,?) ON CONFLICT DO UPDATE SET destination_id=excluded.destination_id",libsql::params![table,maximum]).await.unwrap();
        }
        let (code, error) =
            request(&client, &base, "POST", "/movies/quality-profiles", graph()).await;
        assert_eq!(code, 409);
        assert_eq!(error["error"]["code"], "profile_id_exhausted");
        let definition = json!({"name":"Exhausted","include_when_renaming":false,"specifications":[{"name":"English","negate":false,"required":false,"condition":{"kind":"language","value":1,"except_language":false}}]});
        let (code, error) =
            request(&client, &base, "POST", "/movies/custom-formats", definition).await;
        assert_eq!(code, 409);
        assert_eq!(error["error"]["code"], "custom_format_id_exhausted");
    }
    server.abort();
    let _ = server.await;
}

struct OwnedProcess(std::process::Child);
impl Drop for OwnedProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
#[tokio::test]
async fn actual_startup_seeds_empty_domains_and_restart_preserves_configured_profiles() {
    let scratch = Scratch::new();
    let path = scratch.0.join("startup.db");
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    for pass in 0..2 {
        let log = scratch.0.join(format!("startup-{pass}.log"));
        let out = std::fs::File::create(&log).unwrap();
        let child = std::process::Command::new(env!("CARGO_BIN_EXE_hrrdarr"))
            .env_clear()
            .env("HRRDARR_DATABASE_PATH", &path)
            .env("HRRDARR_BIND", "127.0.0.1:0")
            .current_dir(&scratch.0)
            .stdout(out.try_clone().unwrap())
            .stderr(out)
            .spawn()
            .unwrap();
        let mut child = OwnedProcess(child);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        let base = loop {
            let output = std::fs::read_to_string(&log).unwrap();
            if let Some(base) = output
                .lines()
                .find_map(|line| line.strip_prefix("hrrdarr listening on "))
            {
                break base.to_owned();
            }
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "startup exited: {output}"
            );
            assert!(
                tokio::time::Instant::now() < deadline,
                "startup timed out: {output}"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        };
        let tv = request(&client, &base, "GET", "/tv/quality-profiles", Value::Null).await;
        let movies = request(
            &client,
            &base,
            "GET",
            "/movies/quality-profiles",
            Value::Null,
        )
        .await;
        assert_eq!(tv.0, 200);
        assert_eq!(movies.0, 200);
        assert_eq!(movies.1["total"], 6);
        assert_eq!(tv.1["total"], if pass == 0 { 6 } else { 1 });
        if pass == 1 {
            assert_eq!(tv.1["items"][0]["name"], "Kept on restart")
        }
        drop(child);
        let db = Database::open_local(&path).await.unwrap();
        let c = db.connect().await.unwrap();
        if pass == 0 {
            c.execute_batch("DELETE FROM quality_profiles WHERE media_type='tv' AND name!='SD';UPDATE quality_profiles SET name='Kept on restart' WHERE media_type='tv';DELETE FROM quality_profiles WHERE media_type='movies';").await.unwrap();
        }
    }
}
