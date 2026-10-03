//! Fixture-only evidence for movie credits: mock HTTP on 127.0.0.1:0 and a temp database.
use axum::{Json, Router, extract::State, http::StatusCode, routing::get};
use hrrdarr::{
    credits,
    db::{Database, Error},
    library::{self, refresh},
    metadata::MetadataClient,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

struct Scratch(std::path::PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("hrrdarr-credits-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
type Upstream = Arc<Mutex<(u16, Value)>>;
async fn facts(State(value): State<Upstream>) -> (StatusCode, Json<Value>) {
    let (status, body) = value.lock().unwrap().clone();
    (StatusCode::from_u16(status).unwrap(), Json(body))
}
async fn server(app: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    (
        format!("http://{address}"),
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() }),
    )
}
async fn count(c: &libsql::Connection, sql: &str) -> i64 {
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
async fn dump(c: &libsql::Connection, sql: &str) -> String {
    let mut rows = c.query(sql, ()).await.unwrap();
    let mut out = String::new();
    while let Some(row) = rows.next().await.unwrap() {
        for i in 0..row.column_count() {
            out.push_str(&format!("{:?}|", row.get_value(i).unwrap()));
        }
        out.push('\n');
    }
    out
}
fn credit(id: &str, person: i64, name: &str, kind: &str, order: i64) -> Value {
    json!({"creditId":id,"personTmdbId":person,"name":name,"type":kind,"order":order,
        "character": if kind=="cast" {json!("Role")} else {Value::Null},
        "department": if kind=="crew" {json!("Directing")} else {Value::Null},
        "job": if kind=="crew" {json!("Director")} else {Value::Null},
        "images":[{"coverType":"headshot","url":format!("https://image.tmdb.org/t/p/original/{id}.jpg")}]})
}
fn movie(credits: Value) -> Value {
    json!({"tmdbId":101,"title":"Film","year":2020,"imdbId":"tt1234567","runtime":120,"credits":credits})
}
struct Harness {
    _files: Scratch,
    db: Arc<Database>,
    upstream: Upstream,
    client: Arc<MetadataClient>,
    api: String,
    http: reqwest::Client,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}
impl Drop for Harness {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}
async fn harness(initial: Value) -> Result<Harness, Error> {
    let files = Scratch::new();
    let upstream: Upstream = Arc::new(Mutex::new((200, initial)));
    let (origin, up) = server(
        Router::new()
            .route("/movie/{id}", get(facts))
            .with_state(upstream.clone()),
    )
    .await;
    let client = Arc::new(MetadataClient::with_origins(
        &format!("{origin}/"),
        &format!("{origin}/"),
    )?);
    let db = Arc::new(Database::open_local(files.0.join("db")).await?);
    let (api, app) = server(
        library::metadata_router(db.clone(), client.clone())
            .merge(library::router(db.clone()))
            .merge(credits::router(db.clone())),
    )
    .await;
    Ok(Harness {
        _files: files,
        db,
        upstream,
        client,
        api,
        http: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()?,
        tasks: vec![up, app],
    })
}
impl Harness {
    async fn add(&self) -> reqwest::StatusCode {
        self.http
            .post(format!("{}/api/v1/movies/lookup", self.api))
            .header("content-type", "application/json")
            .body(json!({"tmdb_id":101,"path":"/movies/Film"}).to_string())
            .send()
            .await
            .unwrap()
            .status()
    }
    async fn get(&self, path: &str) -> (u16, Value) {
        let r = self
            .http
            .get(format!("{}{path}", self.api))
            .send()
            .await
            .unwrap();
        let status = r.status().as_u16();
        let body = r.bytes().await.unwrap();
        (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
    }
    fn set(&self, status: u16, body: Value) {
        *self.upstream.lock().unwrap() = (status, body);
    }
    /// One refresh: fetch first (no transaction), then apply inside a transaction.
    async fn refresh(&self) -> Result<u16, String> {
        let detail = self.client.movie(101).await.map_err(|e| e.to_string())?;
        let c = self.db.connect().await.unwrap();
        let target = refresh::capture(&c, refresh::Target::Movies { movie_id: 1 })
            .await
            .map_err(|e| e.to_string())?;
        let tx = c.transaction().await.unwrap();
        match refresh::apply(&tx, &target, refresh::Details::Movie(detail)).await {
            Ok(n) => {
                tx.commit().await.unwrap();
                Ok(n)
            }
            Err(e) => {
                tx.rollback().await.unwrap();
                Err(e.to_string())
            }
        }
    }
}

#[tokio::test]
async fn migration_0047_fresh_and_upgrade_from_0046_preserve_data_and_enforce_schema()
-> Result<(), Error> {
    let files = Scratch::new();
    let path = files.0.join("db");
    let db = Database::open_local(&path).await?;
    let c = db.connect().await?;
    assert_eq!(
        count(&c, "SELECT max(version) FROM schema_migrations").await,
        48 // Reasoning: latest migration is now 0048 indexer operation policy; 0047 movie credits remains verified by the version=47 row below.
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM schema_migrations WHERE version=47 AND name='movie_credits'"
        )
        .await,
        1
    );
    // Foreign keys: only catalog metadata, cascade on delete; no TV table is referenced.
    assert_eq!(
        dump(
            &c,
            "SELECT \"table\",\"on_delete\" FROM pragma_foreign_key_list('movie_credits')"
        )
        .await,
        "Text(\"movie_metadata\")|Text(\"CASCADE\")|\n"
    );
    c.execute_batch("INSERT INTO movie_metadata(id,tmdb_id,title) VALUES(1,10,'Film'),(2,20,'Other'); INSERT INTO movies(id,metadata_id,path) VALUES(1,1,'/m/Film'); INSERT INTO movie_credits(metadata_id,credit_tmdb_id,person_tmdb_id,person_name,credit_order,credit_type) VALUES(1,'c1',5,'A',0,'cast');").await?;
    // Uniqueness is per (metadata, credit id); the same provider id is allowed under other metadata.
    assert!(c.execute("INSERT INTO movie_credits(metadata_id,credit_tmdb_id,person_tmdb_id,person_name,credit_order,credit_type) VALUES(1,'c1',6,'B',1,'cast')", ()).await.is_err());
    c.execute("INSERT INTO movie_credits(metadata_id,credit_tmdb_id,person_tmdb_id,person_name,credit_order,credit_type) VALUES(2,'c1',6,'B',1,'crew')", ()).await?;
    // FK, type, id charset, order, image JSON and per-metadata count bounds.
    for bad in [
        "INSERT INTO movie_credits(metadata_id,credit_tmdb_id,person_tmdb_id,person_name,credit_order,credit_type) VALUES(99,'x',1,'N',0,'cast')",
        "INSERT INTO movie_credits(metadata_id,credit_tmdb_id,person_tmdb_id,person_name,credit_order,credit_type) VALUES(2,'x',1,'N',0,'guest')",
        "INSERT INTO movie_credits(metadata_id,credit_tmdb_id,person_tmdb_id,person_name,credit_order,credit_type) VALUES(2,'a b',1,'N',0,'cast')",
        "INSERT INTO movie_credits(metadata_id,credit_tmdb_id,person_tmdb_id,person_name,credit_order,credit_type) VALUES(2,'x',0,'N',0,'cast')",
        "INSERT INTO movie_credits(metadata_id,credit_tmdb_id,person_tmdb_id,person_name,credit_order,credit_type) VALUES(2,'x',1,'N',-1,'cast')",
        "INSERT INTO movie_credits(metadata_id,credit_tmdb_id,person_tmdb_id,person_name,credit_order,credit_type,images_json) VALUES(2,'x',1,'N',0,'cast','{}')",
        "INSERT INTO movie_credits(metadata_id,credit_tmdb_id,person_tmdb_id,person_name,credit_order,credit_type) VALUES(2,'x',1,'',0,'cast')",
    ] {
        assert!(c.execute(bad, ()).await.is_err(), "accepted {bad}");
    }
    c.execute_batch("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<499) INSERT INTO movie_credits(metadata_id,credit_tmdb_id,person_tmdb_id,person_name,credit_order,credit_type) SELECT 2,'g'||i,i,'N',i,'cast' FROM n;").await?;
    assert_eq!(
        count(&c, "SELECT count(*) FROM movie_credits WHERE metadata_id=2").await,
        500
    );
    assert!(c.execute("INSERT INTO movie_credits(metadata_id,credit_tmdb_id,person_tmdb_id,person_name,credit_order,credit_type) VALUES(2,'over',1,'N',0,'cast')", ()).await.is_err());
    c.execute("DELETE FROM movie_credits WHERE metadata_id=2", ())
        .await?;
    // Upgrade from 0046: drop the 0047 objects and history row, then reopen through the real runner.
    // Reasoning: 0048 now sits above 0047, so a faithful 0046 state must also undo 0048 (its three provider_scopes
    // columns, four indexer health rows and history row); otherwise max(version) stays 48 and the runner would
    // re-run ALTER ADD COLUMN against columns that already exist.
    c.execute_batch("DROP TABLE movie_credits; ALTER TABLE provider_scopes DROP COLUMN enable_rss; ALTER TABLE provider_scopes DROP COLUMN enable_automatic_search; ALTER TABLE provider_scopes DROP COLUMN enable_interactive_search; DELETE FROM health_checks WHERE check_key IN ('indexer_search','indexer_rss'); DELETE FROM schema_migrations WHERE version IN (47,48);")
        .await?;
    assert_eq!(
        count(&c, "SELECT max(version) FROM schema_migrations").await,
        46
    );
    drop(c);
    drop(db);
    let db = Database::open_local(&path).await?;
    assert!(
        db.migration_backup().is_some(),
        "upgrade must take a pre-migration backup"
    );
    let c = db.connect().await?;
    assert_eq!(
        count(&c, "SELECT max(version) FROM schema_migrations").await,
        48 // Reasoning: latest migration is now 0048 indexer operation policy (was 47).
    );
    assert_eq!(count(&c, "SELECT count(*) FROM movie_credits").await, 0);
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM movies WHERE id=1 AND path='/m/Film'"
        )
        .await,
        1
    );
    assert_eq!(count(&c, "SELECT count(*) FROM movie_metadata").await, 2);
    // Deleting metadata cascades its credits.
    c.execute("INSERT INTO movie_credits(metadata_id,credit_tmdb_id,person_tmdb_id,person_name,credit_order,credit_type) VALUES(2,'c1',6,'B',1,'crew')", ()).await?;
    c.execute("DELETE FROM movie_metadata WHERE id=2", ())
        .await?;
    assert_eq!(count(&c, "SELECT count(*) FROM movie_credits").await, 0);
    Ok(())
}

#[tokio::test]
async fn add_persists_credits_and_native_read_routes_filter_page_and_404() -> Result<(), Error> {
    let credits = json!([
        credit("crew1", 11, "Dir", "crew", 0),
        credit("cast2", 12, "Second", "cast", 1),
        credit("cast1", 13, "First", "cast", 0),
    ]);
    let h = harness(movie(credits)).await?;
    assert_eq!(h.add().await, 201);
    let c = h.db.connect().await?;
    assert_eq!(
        count(&c, "SELECT count(*) FROM movie_credits WHERE metadata_id=1").await,
        3
    );
    // Filter by movie id and metadata id agree; cast precedes crew, then provider order.
    let (status, by_movie) = h.get("/api/v1/movies/credits?movie_id=1").await;
    assert_eq!(status, 200);
    let (_, by_meta) = h.get("/api/v1/movies/credits?metadata_id=1").await;
    let (_, all) = h.get("/api/v1/movies/credits").await;
    assert_eq!(by_movie, by_meta);
    assert_eq!(by_movie, all);
    assert_eq!(by_movie["total"], 3);
    assert_eq!(by_movie["limit"], 100);
    let ids: Vec<&str> = by_movie["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["credit_tmdb_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["cast1", "cast2", "crew1"]);
    let first = &by_movie["items"][0];
    assert_eq!(first["person_name"], "First");
    assert_eq!(first["person_tmdb_id"], 13);
    assert_eq!(first["metadata_id"], 1);
    assert_eq!(first["type"], "cast");
    assert_eq!(first["character"], "Role");
    assert_eq!(first["order"], 0);
    assert_eq!(
        first["images"],
        json!([{"cover_type":"headshot","url":"https://image.tmdb.org/t/p/original/cast1.jpg"}])
    );
    let crew = &by_movie["items"][2];
    assert_eq!(
        (
            crew["department"].as_str(),
            crew["job"].as_str(),
            crew["character"].is_null()
        ),
        (Some("Directing"), Some("Director"), true)
    );
    // Pagination.
    let (_, page) = h.get("/api/v1/movies/credits?limit=1&offset=1").await;
    assert_eq!(
        (
            page["total"].clone(),
            page["limit"].clone(),
            page["offset"].clone(),
            page["items"].as_array().unwrap().len()
        ),
        (json!(3), json!(1), json!(1), 1)
    );
    assert_eq!(page["items"][0]["credit_tmdb_id"], "cast2");
    // Get by id.
    let id = first["id"].as_i64().unwrap();
    let (status, one) = h.get(&format!("/api/v1/movies/credits/{id}")).await;
    assert_eq!((status, &one), (200, first));
    // Typed errors: unknown ids are 404; malformed or unbounded parameters are 400.
    for path in [
        "/api/v1/movies/credits/9999",
        "/api/v1/movies/credits?movie_id=9999",
        "/api/v1/movies/credits?metadata_id=9999",
    ] {
        let (status, body) = h.get(path).await;
        assert_eq!(
            (status, body["error"]["code"].as_str()),
            (404, Some("credit_not_found")),
            "{path}"
        );
    }
    for path in [
        "/api/v1/movies/credits/abc",
        "/api/v1/movies/credits/0",
        "/api/v1/movies/credits?limit=0",
        "/api/v1/movies/credits?limit=501",
        "/api/v1/movies/credits?limit=-1",
        "/api/v1/movies/credits?offset=-1",
        "/api/v1/movies/credits?movie_id=0",
        "/api/v1/movies/credits?movie_id=x",
        "/api/v1/movies/credits?movie_id=1&metadata_id=1",
        "/api/v1/movies/credits?bogus=1",
    ] {
        let (status, body) = h.get(path).await;
        assert_eq!(
            (status, body["error"]["code"].as_str()),
            (400, Some("invalid_request")),
            "{path}"
        );
    }
    // Read-only: no mutation verbs exist on the credit routes.
    for request in [
        h.http.post(format!("{}/api/v1/movies/credits", h.api)),
        h.http
            .delete(format!("{}/api/v1/movies/credits/{id}", h.api)),
        h.http.put(format!("{}/api/v1/movies/credits/{id}", h.api)),
    ] {
        assert_eq!(request.send().await?.status(), 405);
    }
    // The library detail route is unaffected by the static credits segment.
    assert_eq!(h.get("/api/v1/movies/1").await.0, 200);
    Ok(())
}

#[tokio::test]
async fn refresh_replaces_credits_atomically_and_preserves_settings_files_and_ids()
-> Result<(), Error> {
    let h = harness(movie(json!([
        credit("keep", 1, "Keep", "cast", 0),
        credit("drop", 2, "Drop", "crew", 0)
    ])))
    .await?;
    assert_eq!(h.add().await, 201);
    let c = h.db.connect().await?;
    c.execute_batch("INSERT INTO tags(id,media_type,label) VALUES(1,'movies','keep'); INSERT INTO movie_tags(movie_id,tag_id) VALUES(1,1); INSERT INTO movie_files(id,movie_id,path) VALUES(1,1,'/movies/Film/original.mkv'); UPDATE movies SET monitored=0 WHERE id=1;").await?;
    let user_state = || async {
        format!(
            "{}{}{}{}",
            dump(&c, "SELECT * FROM movies ORDER BY id").await,
            dump(&c, "SELECT * FROM library_settings ORDER BY 1,2").await,
            dump(&c, "SELECT * FROM movie_files").await,
            dump(&c, "SELECT * FROM movie_tags").await
        )
    };
    let before_state = user_state().await;
    let keep_id = count(
        &c,
        "SELECT id FROM movie_credits WHERE credit_tmdb_id='keep'",
    )
    .await;
    // Identical payload: no change, row ids untouched.
    assert_eq!(h.refresh().await, Ok(0));
    // Changed set: update one, drop one, add one.
    h.set(
        200,
        movie(json!([
            credit("keep", 1, "Keep Renamed", "cast", 3),
            credit("new", 3, "New", "cast", 1)
        ])),
    );
    assert_eq!(h.refresh().await, Ok(1));
    assert_eq!(count(&c, "SELECT count(*) FROM movie_credits").await, 2);
    assert_eq!(count(&c, &format!("SELECT count(*) FROM movie_credits WHERE id={keep_id} AND credit_tmdb_id='keep' AND person_name='Keep Renamed' AND credit_order=3")).await, 1);
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM movie_credits WHERE credit_tmdb_id='drop'"
        )
        .await,
        0
    );
    assert_eq!(
        user_state().await,
        before_state,
        "refresh must not touch user settings, files or tags"
    );
    // Absent credits preserve; an explicit empty array clears.
    h.set(
        200,
        json!({"tmdbId":101,"title":"Film","year":2020,"imdbId":"tt1234567","runtime":120}),
    );
    h.refresh().await.unwrap();
    assert_eq!(count(&c, "SELECT count(*) FROM movie_credits").await, 2);
    h.set(200, movie(json!([])));
    assert_eq!(h.refresh().await, Ok(1));
    assert_eq!(count(&c, "SELECT count(*) FROM movie_credits").await, 0);
    assert_eq!(user_state().await, before_state);
    Ok(())
}

#[tokio::test]
async fn failed_fetch_invalid_payload_or_storage_failure_leaves_existing_credits_intact()
-> Result<(), Error> {
    let h = harness(movie(json!([
        credit("a", 1, "A", "cast", 0),
        credit("b", 2, "B", "crew", 0)
    ])))
    .await?;
    assert_eq!(h.add().await, 201);
    let c = h.db.connect().await?;
    let before = dump(&c, "SELECT * FROM movie_credits ORDER BY id").await;
    // Upstream failure.
    h.set(500, json!({}));
    assert!(h.refresh().await.is_err());
    h.set(404, json!({}));
    assert!(h.refresh().await.is_err());
    // Invalid payloads are rejected as a whole before any transaction opens.
    let many: Vec<Value> = (0..501)
        .map(|i| credit(&format!("m{i}"), 1, "N", "cast", i))
        .collect();
    let mut bad_cases: Vec<(&str, Value)> = vec![
        ("type", json!([credit("z", 1, "N", "guest", 0)])),
        (
            "duplicate",
            json!([
                credit("z", 1, "N", "cast", 0),
                credit("z", 2, "N", "cast", 1)
            ]),
        ),
        ("count", json!(many)),
        ("person", json!([credit("z", 0, "N", "cast", 0)])),
        ("order", json!([credit("z", 1, "N", "cast", -1)])),
        ("name", json!([credit("z", 1, "  ", "cast", 0)])),
        ("id charset", json!([credit("z/../", 1, "N", "cast", 0)])),
        (
            "long job",
            json!([{"creditId":"z","personTmdbId":1,"name":"N","type":"crew","job":"j".repeat(257)}]),
        ),
        (
            "cover type",
            json!([{"creditId":"z","personTmdbId":1,"name":"N","type":"cast","images":[{"coverType":"nope","url":"https://image.tmdb.org/a.jpg"}]}]),
        ),
        (
            "too many images",
            json!([{"creditId":"z","personTmdbId":1,"name":"N","type":"cast","images":(0..9).map(|i| json!({"coverType":"poster","url":format!("https://image.tmdb.org/{i}.jpg")})).collect::<Vec<_>>()}]),
        ),
    ];
    for url in [
        "http://image.tmdb.org/a.jpg",
        "javascript:alert(1)",
        "data:image/png;base64,AAAA",
        "https://127.0.0.1/a.jpg",
        "https://[::1]/a.jpg",
        "https://localhost/a.jpg",
        "https://user:pw@image.tmdb.org/a.jpg",
        "https://image.tmdb.org:8443/a.jpg",
        "https://image.tmdb.org/a.jpg#frag",
        "https://intranet/a.jpg",
        "file:///etc/passwd",
        "/local/cover.jpg",
        "https://image.tmdb.org/a b.jpg",
    ] {
        bad_cases.push((url, json!([{"creditId":"z","personTmdbId":1,"name":"N","type":"cast","images":[{"coverType":"headshot","url":url}]}])));
    }
    for (label, credits) in bad_cases {
        h.set(200, movie(credits));
        assert!(h.client.movie(101).await.is_err(), "accepted {label}");
        assert!(h.refresh().await.is_err(), "refreshed {label}");
    }
    assert_eq!(
        dump(&c, "SELECT * FROM movie_credits ORDER BY id").await,
        before
    );
    // Storage failure inside the replacement transaction rolls back every credit change.
    h.set(
        200,
        movie(json!([
            credit("a", 1, "A changed", "cast", 0),
            credit("n", 9, "N", "cast", 5)
        ])),
    );
    c.execute_batch("CREATE TRIGGER reject_credit BEFORE INSERT ON movie_credits BEGIN SELECT RAISE(ABORT,'fixture'); END;").await?;
    assert_eq!(h.refresh().await, Err("metadata storage failed".into()));
    assert_eq!(
        dump(&c, "SELECT * FROM movie_credits ORDER BY id").await,
        before
    );
    Ok(())
}

#[tokio::test]
async fn credits_are_movie_domain_scoped_and_tv_ids_never_match() -> Result<(), Error> {
    let h = harness(movie(json!([credit("a", 1, "A", "cast", 0)]))).await?;
    assert_eq!(h.add().await, 201);
    let c = h.db.connect().await?;
    // Series 5 / episode id equal to the credit id and to the movie id; none of them may resolve credits.
    let credit_id = count(&c, "SELECT id FROM movie_credits").await;
    c.execute_batch(&format!("INSERT INTO series(id,tvdb_id,title,path) VALUES(77,7,'S','/tv/S'); INSERT INTO seasons(series_id,number) VALUES(77,1); INSERT INTO episodes(id,series_id,season,number,title) VALUES({credit_id},77,1,1,'E'),(500,77,1,2,'E2');")).await?;
    // movie_id names movies only: a series id or episode id is not a movie.
    for id in [77, 500] {
        let (status, _) = h
            .get(&format!("/api/v1/movies/credits?movie_id={id}"))
            .await;
        assert_eq!(status, 404, "id {id} must not resolve through TV rows");
    }
    // A credit has no TV route, and the schema references only movie_metadata.
    assert_eq!(h.get("/api/v1/tv/series/77/credits").await.0, 404);
    assert_eq!(
        h.get(&format!("/api/v1/tv/credits/{credit_id}")).await.0,
        404
    );
    assert_eq!(
        dump(
            &c,
            "SELECT \"table\" FROM pragma_foreign_key_list('movie_credits')"
        )
        .await,
        "Text(\"movie_metadata\")|\n"
    );
    // Catalog-only metadata (no library membership) still has addressable, separate credits.
    c.execute_batch("INSERT INTO movie_metadata(id,tmdb_id,title) VALUES(9,909,'Catalog'); INSERT INTO movie_credits(metadata_id,credit_tmdb_id,person_tmdb_id,person_name,credit_order,credit_type) VALUES(9,'a',2,'Other',0,'cast');").await?;
    let (_, catalog) = h.get("/api/v1/movies/credits?metadata_id=9").await;
    assert_eq!(
        (
            catalog["total"].clone(),
            catalog["items"][0]["person_name"].clone()
        ),
        (json!(1), json!("Other"))
    );
    let (_, film) = h.get("/api/v1/movies/credits?movie_id=1").await;
    assert_eq!(film["total"], 1);
    assert_eq!(film["items"][0]["person_name"], "A");
    Ok(())
}
