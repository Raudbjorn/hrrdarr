use hrrdarr::{
    db::Database,
    snapshots::{self, Application},
};
use serde_json::{Value, json};
use std::sync::Arc;
struct Scratch(std::path::PathBuf);
impl Scratch {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("hrrdarr-tags-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn request(base: &str, method: &str, path: &str, v: Value) -> (u16, Value) {
    let r = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap()
        .request(method.parse().unwrap(), format!("{base}{path}"))
        .header("content-type", "application/json")
        .body(v.to_string())
        .send()
        .await
        .unwrap();
    let status = r.status().as_u16();
    let text = r.text().await.unwrap();
    (
        status,
        if status == 204 {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap()
        },
    )
}
async fn server(db: Arc<Database>) -> (String, Server) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let app = hrrdarr::tags::router(db.clone()).merge(hrrdarr::library::router(db));
    (
        base,
        Server(tokio::spawn(
            async move { axum::serve(l, app).await.unwrap() },
        )),
    )
}
#[tokio::test]
async fn tags_http_both_domains_bulk_and_assignment_transactions() {
    let s = Scratch::new();
    let db = Arc::new(Database::open_local(s.0.join("db")).await.unwrap());
    let c = db.connect().await.unwrap();
    c.execute_batch("INSERT INTO series(id,title,path)VALUES(1,'TV','/tv/1'),(2,'Other TV','/tv/2');INSERT INTO movie_metadata(id,title)VALUES(1,'Movie'),(2,'Other Movie');INSERT INTO movies(id,metadata_id,path)VALUES(1,1,'/movie/1'),(2,2,'/movie/2');").await.unwrap();
    let (base, _server) = server(db).await;
    let mut domain_tags = vec![];
    for (media, library) in [("tv", "/api/v1/tv/series"), ("movies", "/api/v1/movies")] {
        let p = format!("/api/v1/{media}/tags");
        let (code, t) = request(&base, "POST", &p, json!({"label":"Sci-FI"})).await;
        assert_eq!(code, 201, "{t}");
        assert_eq!(t["label"], "sci-fi");
        let id = t["id"].as_i64().unwrap();
        domain_tags.push(id);
        assert_eq!(
            request(&base, "POST", &p, json!({"label":"SCI-fi"}))
                .await
                .1["id"],
            id
        );
        for bad in ["", "space here", "a/b", "á"] {
            assert_eq!(
                request(&base, "POST", &p, json!({"label":bad})).await.0,
                400
            )
        }
        assert_eq!(
            request(&base, "POST", &p, json!({"label":"x".repeat(129)}))
                .await
                .0,
            400
        );
        for assignment in [
            json!({"mode":"add","ids":[id,id]}),
            json!({"mode":"replace","ids":[0]}),
            json!({"mode":"replace","ids":(1..=201).collect::<Vec<i64>>()}),
            json!({"mode":"unknown","ids":[]}),
        ] {
            assert_eq!(
                request(
                    &base,
                    "PUT",
                    &format!("{library}/1"),
                    json!({"tags":assignment})
                )
                .await
                .0,
                400
            );
        }
        let patch = json!({"tags":{"mode":"add","ids":[id]},"monitored":false});
        let (code, r) = request(
            &base,
            "PUT",
            &format!("{library}/editor"),
            json!({"ids":[1,2],"patch":patch}),
        )
        .await;
        assert_eq!(code, 200, "{r}");
        assert_eq!(
            request(&base, "GET", &format!("{library}/1"), Value::Null)
                .await
                .1["tag_ids"],
            json!([id])
        );
        assert_eq!(
            request(&base, "DELETE", &format!("{p}/{id}"), Value::Null)
                .await
                .0,
            409
        );
        let detail = request(&base, "GET", &format!("{p}/detail/{id}"), Value::Null).await;
        assert_eq!(detail.0, 200);
        assert_eq!(detail.1["owner_count"], 2);
        assert_eq!(
            request(
                &base,
                "GET",
                &format!("{p}/{id}/owners?limit=1&offset=1"),
                Value::Null
            )
            .await
            .1["ids"],
            json!([2])
        );
        // A later bad ID must roll back both monitoring and assignments of the first owner.
        assert_eq!(request(&base,"PUT",&format!("{library}/bulk"),json!({"items":[{"id":1,"patch":{"monitored":true,"tags":{"mode":"replace","ids":[]}}},{"id":2,"patch":{"tags":{"mode":"add","ids":[999999]}}}]})).await.0,400);
        let saved = request(&base, "GET", &format!("{library}/1"), Value::Null)
            .await
            .1;
        assert_eq!(saved["monitored"], false);
        assert_eq!(saved["tag_ids"], json!([id]));
        assert_eq!(
            request(&base, "PUT", &format!("{library}/1"), json!({"tags":null}))
                .await
                .0,
            400
        );
        assert_eq!(
            request(
                &base,
                "PUT",
                &format!("{p}/{id}"),
                json!({"label":"Renamed"})
            )
            .await
            .0,
            200
        );
        assert_eq!(
            request(&base, "GET", &p, Value::Null).await.1[0]["label"],
            "renamed"
        );
        assert_eq!(
            request(
                &base,
                "PUT",
                &format!("{library}/editor"),
                json!({"ids":[1,2],"patch":{"tags":{"mode":"remove","ids":[id]}}})
            )
            .await
            .0,
            200
        );
        assert_eq!(
            request(&base, "DELETE", &format!("{p}/{id}"), Value::Null)
                .await
                .0,
            204
        );
        let (_, tag) = request(&base, "POST", &p, json!({"label":"new"})).await;
        let new = tag["id"].as_i64().unwrap();
        let mut create = json!({"title":"Created","path":format!("/scratch/{media}/created"),"settings":{"tags":{"mode":"replace","ids":[new]}}});
        create[if media == "tv" { "tvdb_id" } else { "tmdb_id" }] = json!(5432);
        let (code, created) = request(&base, "POST", library, create).await;
        assert_eq!(code, 201, "{created}");
        assert_eq!(created["tag_ids"], json!([new]));
    }
    // Existing same-number owners remain distinct; cross-domain tag IDs cannot be assigned.
    let tv = request(&base, "GET", "/api/v1/tv/tags", Value::Null)
        .await
        .1[0]["id"]
        .as_i64()
        .unwrap();
    assert_eq!(
        request(
            &base,
            "PUT",
            "/api/v1/movies/1",
            json!({"tags":{"mode":"add","ids":[tv]}})
        )
        .await
        .0,
        400
    );
    c.execute("INSERT INTO snapshot_imports(application,fingerprint,schema_version)VALUES('sonarr','id-limit',233)",()).await.unwrap();
    for maximum in [9_007_199_254_740_991_i64, i64::MAX] {
        c.execute("INSERT INTO snapshot_mappings(application,fingerprint,destination_table,source_id,destination_id)VALUES('sonarr','id-limit','tags',99,?) ON CONFLICT DO UPDATE SET destination_id=excluded.destination_id",[maximum]).await.unwrap();
        let response = request(
            &base,
            "POST",
            "/api/v1/tv/tags",
            json!({"label":"exhausted"}),
        )
        .await;
        assert_eq!(response.0, 409);
        assert_eq!(response.1["error"]["code"], "tag_id_exhausted");
    }
}
async fn fixture(s: &Scratch, version: i64, suffix: &str, extra: &str) -> Vec<u8> {
    let p = s.0.join(format!("source-{version}-{suffix}"));
    let db = libsql::Builder::new_local(&p).build().await.unwrap();
    let c = db.connect().unwrap();
    c.execute_batch(&format!("CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES({version});CREATE TABLE Tags(Id INTEGER,Label TEXT);INSERT INTO Tags VALUES(7,'Imported'),(8,'unused');")).await.unwrap();
    let sql = match version {
        233 => {
            "CREATE TABLE Series(Id INTEGER,TvdbId INTEGER,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,Seasons TEXT,Tags TEXT);INSERT INTO Series VALUES(1,233,'TV',2020,'/tv/imported',1,'[]','[7]');CREATE TABLE Episodes(Id INTEGER,SeriesId INTEGER,SeasonNumber INTEGER,EpisodeNumber INTEGER,Title TEXT,Monitored INTEGER,EpisodeFileId INTEGER);CREATE TABLE EpisodeFiles(Id INTEGER,SeriesId INTEGER,RelativePath TEXT);"
        }
        206 => {
            "CREATE TABLE Movies(Id INTEGER,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER,Tags TEXT);INSERT INTO Movies VALUES(1,206,NULL,'Old',2020,'/movies/old',1,0,'[7]');CREATE TABLE MovieFiles(Id INTEGER,MovieId INTEGER,RelativePath TEXT,Edition TEXT);"
        }
        _ => {
            "CREATE TABLE MovieMetadata(Id INTEGER,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER);INSERT INTO MovieMetadata VALUES(10,242,NULL,'New',2020);CREATE TABLE Movies(Id INTEGER,MovieMetadataId INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER,Tags TEXT);INSERT INTO Movies VALUES(1,10,'/movies/new',1,0,'[7]');CREATE TABLE MovieFiles(Id INTEGER,MovieId INTEGER,RelativePath TEXT,Edition TEXT);"
        }
    };
    c.execute_batch(sql).await.unwrap();
    c.execute_batch(extra).await.unwrap();
    drop(c);
    drop(db);
    std::fs::read(p).unwrap()
}
#[tokio::test]
async fn tags_snapshot_three_schemas_replay_local_edits_deletion_and_late_rollback() {
    let s = Scratch::new();
    for version in [233, 206, 242] {
        let app = if version == 233 {
            Application::Sonarr
        } else {
            Application::Radarr
        };
        let media = if version == 233 { "tv" } else { "movies" };
        let library = if version == 233 {
            "/api/v1/tv/series"
        } else {
            "/api/v1/movies"
        };
        let db = Arc::new(
            Database::open_local(s.0.join(format!("dest-{version}")))
                .await
                .unwrap(),
        );
        let c = db.connect().await.unwrap();
        let bytes = fixture(&s, version, "ok", "").await;
        let report = snapshots::import(&db, app, bytes.clone(), true)
            .await
            .unwrap();
        assert!(!report.applied);
        assert_eq!(
            c.query("SELECT count(*) FROM tags", ())
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap()
                .get::<i64>(0)
                .unwrap(),
            0
        );
        c.execute_batch("CREATE TRIGGER reject_tag_finish BEFORE UPDATE OF tag_version ON snapshot_imports BEGIN SELECT RAISE(ABORT,'injected'); END;").await.unwrap();
        assert!(
            snapshots::import(&db, app, bytes.clone(), false)
                .await
                .is_err()
        );
        assert_eq!(
            c.query("SELECT count(*) FROM tags", ())
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap()
                .get::<i64>(0)
                .unwrap(),
            0
        );
        c.execute_batch("DROP TRIGGER reject_tag_finish;")
            .await
            .unwrap();
        assert!(
            snapshots::import(&db, app, bytes.clone(), false)
                .await
                .unwrap()
                .applied
        );
        let replay = snapshots::import(&db, app, bytes.clone(), false)
            .await
            .unwrap();
        assert!(replay.applied);
        assert_eq!(replay.mapped, 0);
        let (base, _server) = server(db.clone()).await;
        let old = request(&base, "GET", &format!("{library}/1"), Value::Null)
            .await
            .1["tag_ids"][0]
            .as_i64()
            .unwrap();

        assert_eq!(
            request(
                &base,
                "PUT",
                &format!("/api/v1/{media}/tags/{old}"),
                json!({"label":"locally-renamed"})
            )
            .await
            .0,
            200
        );
        let renamed = snapshots::import(&db, app, bytes.clone(), false)
            .await
            .unwrap();
        assert!(!renamed.applied);
        assert!(renamed.conflicts > 0);
        assert_eq!(
            request(
                &base,
                "GET",
                &format!("/api/v1/{media}/tags/{old}"),
                Value::Null
            )
            .await
            .1["label"],
            "locally-renamed"
        );
        assert_eq!(
            request(
                &base,
                "PUT",
                &format!("/api/v1/{media}/tags/{old}"),
                json!({"label":"imported"})
            )
            .await
            .0,
            200
        );
        assert_eq!(
            request(
                &base,
                "PUT",
                &format!("{library}/1"),
                json!({"tags":{"mode":"replace","ids":[]}})
            )
            .await
            .0,
            200
        );
        let replay = snapshots::import(&db, app, bytes.clone(), false)
            .await
            .unwrap();
        assert!(!replay.applied);
        assert!(replay.conflicts > 0);
        assert_eq!(
            request(
                &base,
                "DELETE",
                &format!("/api/v1/{media}/tags/{old}"),
                Value::Null
            )
            .await
            .0,
            204
        );
        let recreated = request(
            &base,
            "POST",
            &format!("/api/v1/{media}/tags"),
            json!({"label":"imported"}),
        )
        .await
        .1["id"]
            .as_i64()
            .unwrap();
        assert!(recreated > old);
        assert!(
            !snapshots::import(&db, app, bytes, false)
                .await
                .unwrap()
                .applied
        );
        let unknown=fixture(&s,version,"unknown-fields","ALTER TABLE Tags ADD COLUMN FuturePolicy TEXT; UPDATE Tags SET FuturePolicy='unknown';").await;
        let isolated = Database::open_local(s.0.join(format!("{version}-unknown-dest")))
            .await
            .unwrap();
        let unsupported = snapshots::import(&isolated, app, unknown, false)
            .await
            .unwrap();
        assert!(unsupported.applied);
        assert!(unsupported.unsupported.iter().any(|u| u.table == "Tags"));
        assert_eq!(
            isolated
                .connect()
                .await
                .unwrap()
                .query("SELECT count(*) FROM tags", ())
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap()
                .get::<i64>(0)
                .unwrap(),
            0
        );
        let owner = if version == 233 { "Series" } else { "Movies" };
        let bad = fixture(
            &s,
            version,
            "bad",
            &format!("UPDATE {owner} SET Tags='[999]';"),
        )
        .await;
        assert!(snapshots::import(&db, app, bad, false).await.is_err());
        for (case, value, errors) in [
            ("malformed", "'['", true),
            ("duplicate", "'[7,7]'", true),
            ("null", "NULL", false),
            ("jsonnull", "'null'", false),
        ] {
            let bytes = fixture(
                &s,
                version,
                case,
                &format!("UPDATE {owner} SET Tags={value};"),
            )
            .await;
            let isolated = Database::open_local(s.0.join(format!("{version}-{case}-dest")))
                .await
                .unwrap();
            let result = snapshots::import(&isolated, app, bytes, false).await;
            if errors {
                assert!(result.is_err());
            } else {
                let r = result.unwrap();
                assert!(r.applied);
                assert!(
                    r.unsupported
                        .iter()
                        .any(|u| u.table == owner
                            && u.columns.iter().any(|c| c.starts_with("Tags:")))
                );
                let conn = isolated.connect().await.unwrap();
                let table = if version == 233 {
                    "series_tags"
                } else {
                    "movie_tags"
                };
                assert_eq!(
                    conn.query(&format!("SELECT count(*) FROM {table}"), ())
                        .await
                        .unwrap()
                        .next()
                        .await
                        .unwrap()
                        .unwrap()
                        .get::<i64>(0)
                        .unwrap(),
                    0
                );
            }
        }
    }
    // One destination: identical source tag/owner IDs must remap independently by application.
    let combined = Database::open_local(s.0.join("combined")).await.unwrap();
    for version in [233, 242] {
        let app = if version == 233 {
            Application::Sonarr
        } else {
            Application::Radarr
        };
        let bytes = fixture(&s, version, "combined", "").await;
        assert!(
            snapshots::import(&combined, app, bytes, false)
                .await
                .unwrap()
                .applied
        );
    }
    let conn = combined.connect().await.unwrap();
    let mut rows=conn.query("SELECT t.media_type,t.label FROM tags t JOIN series_tags a ON a.tag_id=t.id UNION ALL SELECT t.media_type,t.label FROM tags t JOIN movie_tags a ON a.tag_id=t.id ORDER BY 1",()).await.unwrap();
    assert_eq!(
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        "movies"
    );
    assert_eq!(
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        "tv"
    );
    assert!(rows.next().await.unwrap().is_none());
    assert_eq!(conn.query("SELECT count(DISTINCT destination_id) FROM snapshot_mappings WHERE destination_table='tags' AND source_id=7",()).await.unwrap().next().await.unwrap().unwrap().get::<i64>(0).unwrap(),2);
}
