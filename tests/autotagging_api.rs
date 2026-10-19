//! AutoTagging end to end over the real HTTP routers: mock metadata peer on 127.0.0.1:0 + temp DB.
use axum::{Json, Router, extract::State, routing::get};
use hrrdarr::{
    autotagging,
    db::{Database, Error},
    library::{self, refresh},
    metadata::MetadataClient,
    tags,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
type Upstream = Arc<Mutex<(Value, Value)>>; // (show, movie)
async fn show(State(v): State<Upstream>) -> Json<Value> {
    Json(v.lock().unwrap().0.clone())
}
async fn film(State(v): State<Upstream>) -> Json<Value> {
    Json(v.lock().unwrap().1.clone())
}
async fn server(app: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    (
        format!("http://{address}"),
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() }),
    )
}
fn tv_body(genres: Value, network: &str) -> Value {
    json!({"tvdbId":201,"title":"Show","firstAired":"2020-01-01","network":network,"genres":genres,
        "status":"Continuing","runtime":50,"originalCountry":"usa",
        "seasons":[{"seasonNumber":1}],"episodes":[{"tvdbId":1,"seasonNumber":1,"episodeNumber":1,"title":"E"}]})
}
fn movie_body(genres: Value, studio: &str) -> Value {
    json!({"tmdbId":101,"title":"Film","year":2020,"imdbId":"tt1234567","runtime":120,"genres":genres,"studio":studio})
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
        for t in &self.tasks {
            t.abort();
        }
    }
}
async fn harness() -> Result<Harness, Error> {
    let dir = std::env::temp_dir().join(format!("hrrdarr-autotag-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let upstream: Upstream = Arc::new(Mutex::new((
        tv_body(json!(["Drama"]), "HBO"),
        movie_body(json!(["Drama"]), "A24"),
    )));
    let (origin, up) = server(
        Router::new()
            .route("/shows/en/{id}", get(show))
            .route("/movie/{id}", get(film))
            .with_state(upstream.clone()),
    )
    .await;
    let client = Arc::new(MetadataClient::with_origins(
        &format!("{origin}/"),
        &format!("{origin}/"),
    )?);
    let db = Arc::new(Database::open_local(dir.join("db")).await?);
    let (api, app) = server(
        library::metadata_router(db.clone(), client.clone())
            .merge(library::router(db.clone()))
            .merge(tags::router(db.clone()))
            .merge(autotagging::router(db.clone())),
    )
    .await;
    Ok(Harness {
        _files: Scratch(dir),
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
    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let m = reqwest::Method::from_bytes(method.as_bytes()).unwrap();
        let mut r = self.http.request(m, format!("{}{path}", self.api));
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
    async fn tag(&self, domain: &str, label: &str) -> i64 {
        let (s, b) = self
            .call(
                "POST",
                &format!("/api/v1/{domain}/tags"),
                Some(json!({"label":label})),
            )
            .await;
        assert_eq!(s, 201, "{b}");
        b["id"].as_i64().unwrap()
    }
    async fn rule(&self, domain: &str, body: Value) -> (u16, Value) {
        self.call("POST", &format!("/api/v1/{domain}/autotagging"), Some(body))
            .await
    }
    async fn add_movie(&self) -> Value {
        let (s, b) = self
            .call(
                "POST",
                "/api/v1/movies/lookup",
                Some(json!({"tmdb_id":101,"path":"/movies/Film"})),
            )
            .await;
        assert_eq!(s, 201, "{b}");
        b
    }
    async fn add_series(&self) -> Value {
        let (s, b) = self
            .call(
                "POST",
                "/api/v1/tv/series/lookup",
                Some(json!({"tvdb_id":201,"path":"/tv/Show"})),
            )
            .await;
        assert_eq!(s, 201, "{b}");
        b
    }
    async fn refresh_movie(&self) {
        let detail = self.client.movie(101).await.unwrap();
        let c = self.db.connect().await.unwrap();
        let target = refresh::capture(&c, refresh::Target::Movies { movie_id: 1 })
            .await
            .unwrap();
        let tx = c.transaction().await.unwrap();
        refresh::apply(&tx, &target, refresh::Details::Movie(detail))
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
}
fn genre_rule(name: &str, genre: &str, tags: &[i64], remove: bool) -> Value {
    json!({"name":name,"remove_tags_automatically":remove,"tag_ids":tags,"specifications":[
        {"name":"g","negate":false,"required":true,"condition":{"implementation":"genre","values":[genre]}}]})
}

#[tokio::test]
async fn crud_validation_schema_and_tag_protection() -> Result<(), Error> {
    let h = harness().await?;
    let (s, schema) = h
        .call("GET", "/api/v1/movies/autotagging/schema", None)
        .await;
    assert_eq!(s, 200);
    let names: Vec<_> = schema
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["implementation"].as_str().unwrap().to_owned())
        .collect();
    assert!(names.contains(&"studio".into()) && !names.contains(&"network".into()));
    let (_, tv_schema) = h.call("GET", "/api/v1/tv/autotagging/schema", None).await;
    assert!(
        tv_schema
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["implementation"] == "network")
    );

    let tv_tag = h.tag("tv", "drama").await;
    let movie_tag = h.tag("movies", "drama").await;
    let (s, rule) = h
        .rule(
            "movies",
            genre_rule("Drama movies", "drama", &[movie_tag], true),
        )
        .await;
    assert_eq!(s, 201, "{rule}");
    assert_eq!(rule["tag_ids"], json!([movie_tag]));
    assert_eq!(
        rule["specifications"][0]["condition"]["values"],
        json!(["drama"])
    );
    let id = rule["id"].as_i64().unwrap();
    // Duplicate name, cross-domain tag, domain-invalid and unknown conditions, unknown fields.
    assert_eq!(
        h.rule(
            "movies",
            genre_rule("Drama movies", "x", &[movie_tag], false)
        )
        .await
        .0,
        409
    );
    assert_eq!(
        h.rule("movies", genre_rule("Other", "x", &[tv_tag], false))
            .await
            .0,
        400
    );
    let network = json!({"name":"N","remove_tags_automatically":false,"tag_ids":[movie_tag],"specifications":[
        {"name":"n","negate":false,"required":true,"condition":{"implementation":"network","values":["HBO"]}}]});
    assert_eq!(h.rule("movies", network).await.0, 400);
    let unknown = json!({"name":"U","remove_tags_automatically":false,"tag_ids":[movie_tag],"specifications":[
        {"name":"n","negate":false,"required":true,"condition":{"implementation":"certification","values":["R"]}}]});
    assert_eq!(h.rule("movies", unknown).await.0, 400);
    let mut extra = genre_rule("E", "x", &[movie_tag], false);
    extra["surprise"] = json!(1);
    assert_eq!(h.rule("movies", extra).await.0, 400);
    let mut none = genre_rule("NoSpecs", "x", &[movie_tag], false);
    none["specifications"] = json!([]);
    assert_eq!(h.rule("movies", none).await.0, 400);
    // The same id in the other domain does not exist.
    assert_eq!(
        h.call("GET", &format!("/api/v1/tv/autotagging/{id}"), None)
            .await
            .0,
        404
    );
    // Referenced tag cannot be deleted; usage lists the rule.
    let (s, detail) = h
        .call(
            "GET",
            &format!("/api/v1/movies/tags/detail/{movie_tag}"),
            None,
        )
        .await;
    assert_eq!((s, &detail["auto_tagging_ids"]), (200, &json!([id])));
    assert_eq!(
        h.call("DELETE", &format!("/api/v1/movies/tags/{movie_tag}"), None)
            .await
            .0,
        409
    );
    // PUT replaces, then DELETE removes the rule and releases the tag.
    let (s, updated) = h
        .call(
            "PUT",
            &format!("/api/v1/movies/autotagging/{id}"),
            Some(genre_rule("Renamed", "Horror", &[movie_tag], false)),
        )
        .await;
    assert_eq!(s, 200, "{updated}");
    assert_eq!(updated["name"], "Renamed");
    assert_eq!(
        h.call("GET", "/api/v1/movies/autotagging", None)
            .await
            .1
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        h.call("DELETE", &format!("/api/v1/movies/autotagging/{id}"), None)
            .await
            .0,
        204
    );
    assert_eq!(
        h.call("DELETE", &format!("/api/v1/movies/autotagging/{id}"), None)
            .await
            .0,
        404
    );
    assert_eq!(
        h.call("DELETE", &format!("/api/v1/movies/tags/{movie_tag}"), None)
            .await
            .0,
        204
    );
    Ok(())
}

#[tokio::test]
async fn rules_apply_on_add_refresh_and_update_per_domain() -> Result<(), Error> {
    let h = harness().await?;
    let movie_tag = h.tag("movies", "drama").await;
    let studio_tag = h.tag("movies", "a24").await;
    let tv_tag = h.tag("tv", "hbo").await;
    h.rule("movies", genre_rule("Drama", "drama", &[movie_tag], true))
        .await;
    let studio = json!({"name":"Studio","remove_tags_automatically":false,"tag_ids":[studio_tag],"specifications":[
        {"name":"s","negate":false,"required":true,"condition":{"implementation":"studio","values":["a24"]}},
        {"name":"y","negate":false,"required":true,"condition":{"implementation":"year","min":2010,"max":2030}}]});
    assert_eq!(h.rule("movies", studio).await.0, 201);
    let network = json!({"name":"Network","remove_tags_automatically":false,"tag_ids":[tv_tag],"specifications":[
        {"name":"n","negate":false,"required":false,"condition":{"implementation":"network","values":["hbo","amc"]}},
        {"name":"r","negate":false,"required":false,"condition":{"implementation":"runtime","min":90}}]});
    assert_eq!(h.rule("tv", network).await.0, 201);

    // Add: rule tags are present in the creation response and persisted.
    let movie = h.add_movie().await;
    assert_eq!(movie["tag_ids"], json!([movie_tag, studio_tag]));
    let series = h.add_series().await;
    assert_eq!(series["tag_ids"], json!([tv_tag]));
    // Domain isolation: the TV series (id 1) did not receive movie rules and vice versa.
    let (_, listed) = h.call("GET", "/api/v1/movies/1", None).await;
    assert_eq!(listed["tag_ids"], json!([movie_tag, studio_tag]));

    // Refresh: genre disappears -> remove_tags_automatically removes only that rule's tag.
    h.upstream.lock().unwrap().1 = movie_body(json!(["Horror"]), "A24");
    h.refresh_movie().await;
    let (_, after) = h.call("GET", "/api/v1/movies/1", None).await;
    assert_eq!(after["tag_ids"], json!([studio_tag]));
    // Manual tags survive; genre returns -> tag returns.
    h.upstream.lock().unwrap().1 = movie_body(json!(["Drama"]), "A24");
    h.refresh_movie().await;
    let (_, again) = h.call("GET", "/api/v1/movies/1", None).await;
    assert_eq!(again["tag_ids"], json!([movie_tag, studio_tag]));

    // Update: a non-tag edit re-evaluates (removing the tag manually would be re-added), whereas an
    // explicit tag assignment in the request is respected for that request.
    let (s, kept) = h
        .call(
            "PUT",
            "/api/v1/movies/1",
            Some(json!({"tags":{"mode":"replace","ids":[]}})),
        )
        .await;
    assert_eq!((s, &kept["tag_ids"]), (200, &json!([])));
    let (s, reapplied) = h
        .call("PUT", "/api/v1/movies/1", Some(json!({"monitored":false})))
        .await;
    assert_eq!(
        (s, &reapplied["tag_ids"]),
        (200, &json!([movie_tag, studio_tag]))
    );
    Ok(())
}

#[tokio::test]
async fn root_folder_negation_and_unknown_facts() -> Result<(), Error> {
    let h = harness().await?;
    let c = h.db.connect().await?;
    c.execute(
        "INSERT INTO root_folders(media_type,path) VALUES('movies','/movies')",
        (),
    )
    .await?;
    let in_root = h.tag("movies", "in-root").await;
    let not_horror = h.tag("movies", "not-horror").await;
    let unknown_cert = h.tag("movies", "keywords").await;
    let root = json!({"name":"Root","remove_tags_automatically":false,"tag_ids":[in_root],"specifications":[
        {"name":"r","negate":false,"required":true,"condition":{"implementation":"root_folder","path":"/movies/"}}]});
    assert_eq!(h.rule("movies", root).await.0, 201);
    let neg = json!({"name":"Neg","remove_tags_automatically":false,"tag_ids":[not_horror],"specifications":[
        {"name":"g","negate":true,"required":true,"condition":{"implementation":"genre","values":["horror"]}}]});
    assert_eq!(h.rule("movies", neg).await.0, 201);
    // Keyword facts are never supplied by this peer: unknown must not satisfy a negated keyword rule.
    let kw = json!({"name":"Kw","remove_tags_automatically":false,"tag_ids":[unknown_cert],"specifications":[
        {"name":"k","negate":true,"required":true,"condition":{"implementation":"keyword","values":["gore"]}}]});
    assert_eq!(h.rule("movies", kw).await.0, 201);
    let movie = h.add_movie().await;
    assert_eq!(movie["tag_ids"], json!([in_root, not_horror]));
    Ok(())
}
