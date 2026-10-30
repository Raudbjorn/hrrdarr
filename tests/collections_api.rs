//! Collections end to end: metadata adoption on add, REST policy, and the RefreshCollections command
//! running in the real worker against a mock metadata peer on 127.0.0.1:0.
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::get,
};
use hrrdarr::{
    autotagging, collections, commands,
    db::{Database, Error},
    library,
    metadata::MetadataClient,
    providers, tags,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
type Peer = Arc<Mutex<Value>>; // {"movie":{id:body}, "collection":{id:body|null}}
async fn movie(State(p): State<Peer>, Path(id): Path<i64>) -> (StatusCode, Json<Value>) {
    match p.lock().unwrap()["movie"][id.to_string()].clone() {
        Value::Null => (StatusCode::NOT_FOUND, Json(Value::Null)),
        v => (StatusCode::OK, Json(v)),
    }
}
async fn collection(State(p): State<Peer>, Path(id): Path<i64>) -> (StatusCode, Json<Value>) {
    match p.lock().unwrap()["collection"][id.to_string()].clone() {
        Value::Null => (StatusCode::NOT_FOUND, Json(Value::Null)),
        v => (StatusCode::OK, Json(v)),
    }
}
async fn server(app: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    (
        format!("http://{address}"),
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() }),
    )
}
fn film(id: i64, title: &str, year: i64, collection: Value) -> Value {
    json!({"tmdbId":id,"title":title,"year":year,"runtime":100,"collection":collection})
}
struct H {
    _files: Scratch,
    db: Arc<Database>,
    peer: Peer,
    client: Arc<MetadataClient>,
    api: String,
    http: reqwest::Client,
    worker: Option<commands::Runtime>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}
impl Drop for H {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}
async fn harness() -> Result<H, Error> {
    let dir = std::env::temp_dir().join(format!("hrrdarr-collections-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let peer: Peer = Arc::new(Mutex::new(json!({"movie":{},"collection":{}})));
    let (origin, up) = server(
        Router::new()
            .route("/movie/{id}", get(movie))
            .route("/movie/collection/{id}", get(collection))
            .with_state(peer.clone()),
    )
    .await;
    let client = Arc::new(MetadataClient::with_origins(
        &format!("{origin}/"),
        &format!("{origin}/"),
    )?);
    let db = Arc::new(Database::open_local(dir.join("db")).await?);
    let (_, refresh_client) = providers::router_with_refresh(db.clone(), None);
    let worker = commands::start_with_metadata(db.clone(), refresh_client, client.clone())
        .await
        .unwrap();
    let (api, app) = server(
        library::metadata_router(db.clone(), client.clone())
            .merge(library::router(db.clone()))
            .merge(tags::router(db.clone()))
            .merge(autotagging::router(db.clone()))
            .merge(collections::router(db.clone()))
            .merge(commands::router(db.clone())),
    )
    .await;
    Ok(H {
        _files: Scratch(dir),
        db,
        peer,
        client,
        api,
        http: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()?,
        worker: Some(worker),
        tasks: vec![up, app],
    })
}
impl H {
    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut r = self.http.request(
            reqwest::Method::from_bytes(method.as_bytes()).unwrap(),
            format!("{}{path}", self.api),
        );
        if let Some(b) = body {
            r = r
                .header("content-type", "application/json")
                .body(b.to_string());
        }
        let resp = r.send().await.unwrap();
        let status = resp.status().as_u16();
        let bytes = resp.bytes().await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
    async fn scalar(&self, sql: &str) -> i64 {
        let c = self.db.connect().await.unwrap();
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
    async fn finish(mut self) {
        if let Some(w) = self.worker.take() {
            w.shutdown().await;
        }
    }
}

#[tokio::test]
async fn adoption_policy_validation_and_refresh_command() -> Result<(), Error> {
    let h = harness().await?;
    let saga = json!({"tmdbId":900,"name":"Saga"});
    {
        let mut p = h.peer.lock().unwrap();
        p["movie"]["101"] = film(101, "Saga One", 2001, saga.clone());
        p["collection"]["900"] = json!({"tmdbId":900,"name":"Saga","overview":"Three films","parts":[
            {"tmdbId":101,"title":"Saga One","year":2001},
            {"tmdbId":102,"title":"Saga: Two?","year":2003,"imdbId":"tt0000102"},
            {"tmdbId":103,"title":"Saga Three","year":2005}]});
    }
    let c = h.db.connect().await?;
    c.execute(
        "INSERT INTO root_folders(media_type,path) VALUES('movies','/library/movies')",
        (),
    )
    .await?;
    let root_id = h.scalar("SELECT id FROM root_folders").await;
    c.execute("INSERT INTO quality_profiles VALUES(2,'movies','HD')", ())
        .await?;
    let profile = h
        .scalar("SELECT id FROM quality_profiles WHERE media_type='movies' ORDER BY id LIMIT 1")
        .await;

    // Adoption from metadata on add.
    let (s, added) = h
        .call(
            "POST",
            "/api/v1/movies/lookup",
            Some(json!({"tmdb_id":101,"path":"/library/movies/Saga One (2001)"})),
        )
        .await;
    assert_eq!(s, 201, "{added}");
    let (s, page) = h.call("GET", "/api/v1/movies/collections", None).await;
    assert_eq!(s, 200);
    assert_eq!(page["total"], 1);
    let id = page["items"][0]["id"].as_i64().unwrap();
    assert_eq!(page["items"][0]["tmdb_id"], 900);
    assert_eq!(
        (
            page["items"][0]["movie_count"].as_i64(),
            page["items"][0]["library_movie_count"].as_i64()
        ),
        (Some(1), Some(1))
    );
    assert_eq!(page["items"][0]["monitored"], false);
    assert_eq!(
        h.call("GET", "/api/v1/movies/collections?monitored=true", None)
            .await
            .1["total"],
        0
    );
    assert_eq!(
        h.call("GET", "/api/v1/movies/collections?q=sag", None)
            .await
            .1["total"],
        1
    );
    assert_eq!(
        h.call("GET", "/api/v1/movies/collections?q=%25", None)
            .await
            .1["total"],
        0
    );

    // Policy validation: cannot monitor without a complete policy; foreign ids and unknown fields rejected.
    let path = format!("/api/v1/movies/collections/{id}");
    assert_eq!(
        h.call("PUT", &path, Some(json!({"monitored":true})))
            .await
            .0,
        409
    );
    assert_eq!(
        h.call("PUT", &path, Some(json!({"root_folder_id":9999})))
            .await
            .0,
        400
    );
    assert_eq!(
        h.call("PUT", &path, Some(json!({"tag_ids":[1234]})))
            .await
            .0,
        400
    );
    assert_eq!(
        h.call("PUT", &path, Some(json!({"surprise":1}))).await.0,
        400
    );
    assert_eq!(
        h.call("GET", "/api/v1/movies/collections/9999", None)
            .await
            .0,
        404
    );

    // Tags + full policy; the collection tag set protects the tag from deletion.
    let (_, tag) = h
        .call("POST", "/api/v1/movies/tags", Some(json!({"label":"saga"})))
        .await;
    let tag_id = tag["id"].as_i64().unwrap();
    let (s, configured) = h
        .call(
            "PUT",
            &path,
            Some(
                json!({"quality_profile_id":profile,"root_folder_id":root_id,
        "minimum_availability":"released","search_on_add":true,"tag_ids":[tag_id]}),
            ),
        )
        .await;
    assert_eq!(s, 200, "{configured}");
    assert_eq!(configured["root_folder_path"], "/library/movies");
    assert_eq!(configured["tag_ids"], json!([tag_id]));
    assert_eq!(
        h.call("DELETE", &format!("/api/v1/movies/tags/{tag_id}"), None)
            .await
            .0,
        409
    );
    // Nothing is added by merely configuring an unmonitored collection.
    assert_eq!(h.scalar("SELECT count(*) FROM movies").await, 1);
    // Clearing the policy of a monitored collection is rejected; so is a bulk patch that would.
    assert_eq!(
        h.call(
            "PUT",
            &path,
            Some(json!({"monitored":true,"root_folder_id":null}))
        )
        .await
        .0,
        409
    );

    // RefreshCollections via the real worker adds missing members only after monitoring.
    let (s, cmd) = h
        .call(
            "POST",
            "/api/v1/movies/collections/refresh-commands",
            Some(json!({})),
        )
        .await;
    assert_eq!(s, 202, "{cmd}");
    let command_id = cmd["id"].as_str().unwrap().to_owned();
    let mut settled = Value::Null;
    for _ in 0..100 {
        let (_, v) = h
            .call(
                "GET",
                &format!("/api/v1/movies/collections/refresh-commands/{command_id}"),
                None,
            )
            .await;
        if v["status"] == "succeeded" || v["status"] == "failed" {
            settled = v;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(settled["status"], "succeeded", "{settled}");
    assert_eq!(
        (
            settled["collections_checked"].as_i64(),
            settled["movies_added"].as_i64()
        ),
        (Some(1), Some(0))
    );
    let (_, detail) = h.call("GET", &path, None).await;
    assert_eq!(
        (
            detail["movie_count"].as_i64(),
            detail["library_movie_count"].as_i64(),
            detail["missing_movie_count"].as_i64()
        ),
        (Some(3), Some(1), Some(2))
    );
    assert_eq!(detail["overview"], "Three films");
    assert_eq!(detail["movies"].as_array().unwrap().len(), 3);
    assert!(detail["last_info_sync"].is_string());
    assert_eq!(h.scalar("SELECT count(*) FROM movies").await, 1);

    // Monitoring adopts the already-known missing members with the configured policy.
    let (s, mon) = h.call("PUT", &path, Some(json!({"monitored":true}))).await;
    assert_eq!(s, 200, "{mon}");
    assert_eq!(mon["library_movie_count"], 3);
    assert_eq!(
        h.scalar("SELECT count(*) FROM movies WHERE path='/library/movies/Saga Two (2003)'")
            .await,
        1
    );
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM movie_tags WHERE tag_id={tag_id}"
        ))
        .await,
        2
    );
    assert_eq!(h.scalar(&format!("SELECT count(*) FROM library_settings WHERE quality_profile_id={profile} AND minimum_availability='released'")).await, 2);

    // A later refresh that removes a member from the graph removes it from the collection only.
    {
        let mut p = h.peer.lock().unwrap();
        p["collection"]["900"]["parts"] = json!([{"tmdbId":101,"title":"Saga One","year":2001},{"tmdbId":102,"title":"Saga: Two?","year":2003},{"tmdbId":104,"title":"Saga Four","year":2010}]);
    }
    let (_, cmd) = h
        .call(
            "POST",
            "/api/v1/movies/collections/refresh-commands",
            Some(json!({})),
        )
        .await;
    let command_id = cmd["id"].as_str().unwrap().to_owned();
    for _ in 0..100 {
        let (_, v) = h
            .call(
                "GET",
                &format!("/api/v1/movies/collections/refresh-commands/{command_id}"),
                None,
            )
            .await;
        if v["status"] == "succeeded" {
            assert_eq!(v["movies_added"], 1);
            assert_eq!(v["searches_pending"], 1);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let (_, after) = h.call("GET", &path, None).await;
    assert_eq!(after["movie_count"], 3);
    assert_eq!(
        h.scalar("SELECT count(*) FROM movies").await,
        4,
        "library movies are never deleted"
    );

    // monitor_movies bulk toggles monitoring of library members only.
    let (s, _) = h
        .call("PUT", &path, Some(json!({"monitor_movies":false})))
        .await;
    assert_eq!(s, 200);
    assert_eq!(
        h.scalar("SELECT count(*) FROM movies WHERE monitored=1")
            .await,
        1
    );
    // Bulk endpoint, and unmonitoring is always allowed.
    let (s, bulk) = h
        .call(
            "PUT",
            "/api/v1/movies/collections",
            Some(json!({"ids":[id],"patch":{"monitored":false}})),
        )
        .await;
    assert_eq!((s, &bulk[0]["monitored"]), (200, &json!(false)));
    assert_eq!(
        h.call(
            "PUT",
            "/api/v1/movies/collections",
            Some(json!({"ids":[id,id],"patch":{}}))
        )
        .await
        .0,
        400
    );
    h.finish().await;
    Ok(())
}

#[tokio::test]
async fn clear_removes_membership_and_unavailable_peer_fails_the_command() -> Result<(), Error> {
    let h = harness().await?;
    {
        let mut p = h.peer.lock().unwrap();
        p["movie"]["101"] = film(101, "Saga One", 2001, json!({"tmdbId":900,"name":"Saga"}));
    }
    let (s, _) = h
        .call(
            "POST",
            "/api/v1/movies/lookup",
            Some(json!({"tmdb_id":101,"path":"/m/Saga One"})),
        )
        .await;
    assert_eq!(s, 201);
    assert_eq!(
        h.scalar("SELECT count(*) FROM movie_collection_members")
            .await,
        1
    );
    // The collection endpoint 404s: the command records the failure per collection, not a crash.
    let (_, cmd) = h
        .call(
            "POST",
            "/api/v1/movies/collections/refresh-commands",
            Some(json!({})),
        )
        .await;
    let command_id = cmd["id"].as_str().unwrap().to_owned();
    let mut settled = Value::Null;
    for _ in 0..100 {
        let (_, v) = h
            .call(
                "GET",
                &format!("/api/v1/movies/collections/refresh-commands/{command_id}"),
                None,
            )
            .await;
        if v["status"] == "succeeded" || v["status"] == "failed" {
            settled = v;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(settled["status"], "succeeded", "{settled}");
    assert_eq!(
        (
            settled["collections_checked"].as_i64(),
            settled["collections_failed"].as_i64()
        ),
        (Some(0), Some(1))
    );
    // Explicit null on a refresh clears membership but keeps the collection and the movie.
    h.peer.lock().unwrap()["movie"]["101"] = film(101, "Saga One", 2001, Value::Null);
    let c = h.db.connect().await?;
    let detail = h.client.movie(101).await.unwrap();
    let target = library::refresh::capture(&c, library::refresh::Target::Movies { movie_id: 1 })
        .await
        .unwrap();
    let tx = c.transaction().await.unwrap();
    library::refresh::apply(&tx, &target, library::refresh::Details::Movie(detail))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        h.scalar("SELECT count(*) FROM movie_collection_members")
            .await,
        0
    );
    assert_eq!(h.scalar("SELECT count(*) FROM movie_collections").await, 1);
    assert_eq!(h.scalar("SELECT count(*) FROM movies").await, 1);
    h.finish().await;
    Ok(())
}

#[tokio::test]
async fn collection_discovery_preserves_removed_and_local_edit_intents() -> Result<(), Error> {
    let mut h = harness().await?;
    h.worker.take().unwrap().shutdown().await;
    let c = h.db.connect().await?;
    c.execute_batch("INSERT INTO movie_collection_intents(tmdb_id,local_edit,removed,removal_reason) VALUES(900,1,1,'user'),(901,1,1,'metadata_missing');").await?;
    {
        let mut p = h.peer.lock().unwrap();
        p["movie"]["101"] = film(
            101,
            "Removed",
            2001,
            json!({"tmdbId":900,"name":"Removed Collection"}),
        );
        p["movie"]["102"] = film(
            102,
            "Restored",
            2002,
            json!({"tmdbId":901,"name":"Restored Collection"}),
        );
    }
    for id in [101, 102] {
        let (status, body) = h
            .call(
                "POST",
                "/api/v1/movies/lookup",
                Some(json!({"tmdb_id":id,"path":format!("/library/movies/{id}")})),
            )
            .await;
        assert_eq!(status, 201, "{body}");
    }
    assert_eq!(
        h.scalar("SELECT count(*) FROM movie_collections WHERE tmdb_id=900")
            .await,
        0
    );
    assert_eq!(h.scalar("SELECT count(*) FROM movie_collection_intents WHERE tmdb_id=900 AND removed=1 AND local_edit=1 AND removal_reason='user' AND revision=1").await, 1);
    assert_eq!(h.scalar("SELECT count(*) FROM movie_collection_members m JOIN movie_collections c ON c.id=m.collection_id WHERE c.tmdb_id=901").await, 1);
    // Restoration clears lifecycle removal, but the independent snapshot fence must survive.
    assert_eq!(h.scalar("SELECT count(*) FROM movie_collection_intents WHERE tmdb_id=901 AND removed=0 AND local_edit=1 AND removal_reason IS NULL AND revision=2").await, 1);
    // A rediscovered removed collection must also detach an outdated association.
    h.peer.lock().unwrap()["movie"]["102"] = film(
        102,
        "Restored",
        2002,
        json!({"tmdbId":900,"name":"Removed Collection"}),
    );
    let movie_id = h.scalar("SELECT m.id FROM movies m JOIN movie_metadata d ON d.id=m.metadata_id WHERE d.tmdb_id=102").await;
    let target = library::refresh::capture(&c, library::refresh::Target::Movies { movie_id })
        .await
        .unwrap();
    let detail = h.client.movie(102).await.unwrap();
    let tx = c.transaction().await?;
    library::refresh::apply(&tx, &target, library::refresh::Details::Movie(detail))
        .await
        .unwrap();
    tx.commit().await?;
    assert_eq!(
        h.scalar("SELECT count(*) FROM movie_collection_members")
            .await,
        0
    );
    assert_eq!(
        h.scalar("SELECT count(*) FROM movie_collections WHERE tmdb_id=900")
            .await,
        0
    );
    h.finish().await;
    Ok(())
}
