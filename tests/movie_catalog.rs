use axum::{Json, Router, extract::State, routing::get};
use hrrdarr::{
    db::{Database, Error},
    library,
    metadata::MetadataClient,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
async fn facts(State(value): State<Arc<Mutex<Value>>>) -> Json<Value> {
    Json(value.lock().unwrap().clone())
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
#[tokio::test]
async fn real_movie_metadata_add_and_refresh_persist_eligibility_facts_atomically()
-> Result<(), Error> {
    let files = Scratch(
        std::env::temp_dir().join(format!("hrrdarr-movie-catalog-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&files.0)?;
    let movie = json!({"tmdbId":101,"title":"Film","year":2020,"imdbId":"tt1234567","runtime":120,"studio":" Studio ","genres":["Drama","drama"],"keywords":["Ý","ý"],"status":"Ignored","inCinema":"2020-02-01T02:00:00+02:00","digitalRelease":"2020-04-01T00:00:00.500000000Z","physicalRelease":"2020-05-01T00:00:00Z","premier":"2019-12-31T00:00:00Z","originalLanguage":"en","alternativeTitles":[{"title":"Other title","type":"ignored","language":"en"}]});
    let value = Arc::new(Mutex::new(movie.clone()));
    let (origin, upstream) = server(
        Router::new()
            .route("/movie/{id}", get(facts))
            .with_state(value.clone()),
    )
    .await;
    let client = Arc::new(MetadataClient::with_origins(
        &format!("{origin}/"),
        &format!("{origin}/"),
    )?);
    let db = Arc::new(Database::open_local(files.0.join("db")).await?);
    let (api, app) = server(
        library::metadata_router(db.clone(), client.clone()).merge(library::router(db.clone())),
    )
    .await;
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()?;
    let response = http
        .post(format!("{api}/api/v1/movies/lookup"))
        .header("content-type", "application/json")
        .body(json!({"tmdb_id":101,"path":"/movies/Film"}).to_string())
        .send()
        .await?;
    assert_eq!(response.status(), 201);
    let c = db.connect().await?;
    // Native lifecycle now derives released from these past validated home dates; wire status is ignored.
    assert_eq!(count(&c,"SELECT count(*) FROM movie_metadata WHERE runtime=120 AND status='released' AND studio=' Studio ' AND genres_json='[\"Drama\"]' AND keywords_json='[\"Ý\"]' AND in_cinemas='2020-02-01 00:00:00' AND digital_release='2020-04-01 00:00:00.5' AND physical_release='2020-05-01 00:00:00' AND secondary_year=2019 AND original_language=1").await,1);
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM movie_alternative_titles WHERE title='Other title'"
        )
        .await,
        1
    );
    let target =
        library::refresh::capture(&c, library::refresh::Target::Movies { movie_id: 1 }).await?;
    c.execute_batch("INSERT INTO tags(id,media_type,label) VALUES(1,'movies','keep'); INSERT INTO movie_tags(movie_id,tag_id) VALUES(1,1); INSERT INTO movie_files(id,movie_id,path) VALUES(1,1,'/movies/Film/original.mkv');").await?;
    // Explicit empty sets clear only supplied sets. Lifecycle is derived from this
    // sparse response (announced), while previously known absent dates are retained.
    *value.lock().unwrap() = json!({"tmdbId":101,"title":"Film","year":2020,"imdbId":"tt1234567","alternativeTitles":[],"genres":[],"keywords":null});
    let detail = client.movie(101).await?;
    let tx = c.transaction().await?;
    assert_eq!(
        library::refresh::apply(&tx, &target, library::refresh::Details::Movie(detail)).await?,
        1
    );
    tx.commit().await?;
    assert_eq!(
        count(&c, "SELECT count(*) FROM movie_alternative_titles").await,
        0
    );
    assert_eq!(
        count(&c, "SELECT runtime FROM movie_metadata WHERE id=1").await,
        120
    );
    assert_eq!(count(&c, "SELECT count(*) FROM movie_metadata WHERE status='announced' AND digital_release='2020-04-01 00:00:00.5' AND studio=' Studio ' AND genres_json='[]' AND keywords_json='[\"Ý\"]'").await, 1);
    let detail = client.movie(101).await?;
    let tx = c.transaction().await?;
    assert_eq!(
        library::refresh::apply(&tx, &target, library::refresh::Details::Movie(detail)).await?,
        0
    );
    tx.commit().await?;
    let mut changed = movie.clone();
    changed["runtime"] = json!(130);
    changed["alternativeTitles"] = json!([{"title":"Late"}]);
    *value.lock().unwrap() = changed;
    c.execute_batch("CREATE TRIGGER reject_alias BEFORE INSERT ON movie_alternative_titles BEGIN SELECT RAISE(ABORT,'fixture');END;").await?;
    let detail = client.movie(101).await?;
    let tx = c.transaction().await?;
    assert_eq!(
        library::refresh::apply(&tx, &target, library::refresh::Details::Movie(detail)).await,
        Err(library::refresh::Error::Storage)
    );
    tx.rollback().await?;
    assert_eq!(
        count(&c, "SELECT runtime FROM movie_metadata WHERE id=1").await,
        120
    );
    assert_eq!(count(&c, "SELECT count(*) FROM movie_metadata WHERE studio=' Studio ' AND genres_json='[]' AND keywords_json='[\"Ý\"]' AND status='announced'").await,1);
    c.execute("DROP TRIGGER reject_alias", ()).await?;
    // Selected add cannot overwrite conflicting facts in a catalog-only record.
    c.execute("INSERT INTO movie_metadata(id,tmdb_id,title,year,imdb_id,runtime) VALUES(7,202,'Other',2020,'tt7654321',130)",()).await?;
    let mut other = movie.clone();
    other["tmdbId"] = json!(202);
    other["title"] = json!("Other");
    other["imdbId"] = json!("tt7654321");
    *value.lock().unwrap() = other;
    assert_eq!(
        http.post(format!("{api}/api/v1/movies/lookup"))
            .header("content-type", "application/json")
            .body(json!({"tmdb_id":202,"path":"/movies/Other"}).to_string())
            .send()
            .await?
            .status(),
        409
    );
    assert_eq!(
        count(&c, "SELECT count(*) FROM movies WHERE metadata_id=7").await,
        0
    );
    assert_eq!(
        count(&c, "SELECT runtime FROM movie_metadata WHERE id=7").await,
        130
    );
    // Invalid bounds and malformed dates are rejected at metadata transport validation.
    for (field, bad) in [
        ("runtime", json!(-1)),
        ("runtime", json!(10081)),
        ("digitalRelease", json!("2020-01-01T00:00:00.1234567891Z")),
        ("originalLanguage", json!("private URL")),
        (
            "alternativeTitles",
            json!(
                (0..65)
                    .map(|i| json!({"title":format!("{i}")}))
                    .collect::<Vec<_>>()
            ),
        ),
    ] {
        let mut bad_movie = movie.clone();
        bad_movie[field] = bad;
        *value.lock().unwrap() = bad_movie;
        assert!(client.movie(101).await.is_err());
    }
    *value.lock().unwrap() =
        json!({"tmdbId":101,"title":"Film","year":2020,"originalLanguage":"zz"});
    let detail = client.movie(101).await?;
    assert!(detail.original_language.is_none());
    assert!(detail.alternative_titles.is_none());
    // Wire absence still derives an announced lifecycle; it is not an unknown enum.
    assert_eq!(detail.status.as_deref(), Some("announced"));
    assert_eq!(
        count(&c, "SELECT count(*) FROM release_delay_policies").await,
        0
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM movie_files WHERE id=1 AND movie_id=1"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM movie_tags WHERE movie_id=1 AND tag_id=1"
        )
        .await,
        1
    );
    app.abort();
    upstream.abort();
    let _ = app.await;
    let _ = upstream.await;
    drop(c);
    drop(db);
    let reopened = Database::open_local(files.0.join("db")).await?;
    let c = reopened.connect().await?;
    assert_eq!(count(&c, "SELECT count(*) FROM movie_metadata WHERE id=1 AND studio=' Studio ' AND genres_json='[]' AND keywords_json='[\"Ý\"]' AND status='announced'").await,1);
    assert_eq!(
        count(&c, "SELECT runtime FROM movie_metadata WHERE id=1").await,
        120
    );
    assert_eq!(
        count(&c, "SELECT count(*) FROM movies WHERE path='/movies/Film'").await,
        1
    );
    Ok(())
}

async fn assert_movie_availability(
    c: &libsql::Connection,
    minimum: &str,
    now: &str,
    allowed: bool,
    context: hrrdarr::search::SearchContext,
) {
    use hrrdarr::{api::MediaDomain, providers::indexer, search};
    c.execute(
        "UPDATE library_settings SET minimum_availability=? WHERE movie_id=1",
        [minimum],
    )
    .await
    .unwrap();
    // Torrent publication is required by the real parser; keep it before all decision clocks.
    let release = indexer::parse_page(r#"<rss xmlns:torznab="http://torznab.com/schemas/2015/feed"><channel><item><title>Film.2020.1080p.WEB-DL</title><guid>fixture</guid><pubDate>2019-01-01T00:00:00Z</pubDate><link>https://example.invalid/release</link><torznab:attr name="category" value="2030"/></item></channel></rss>"#,0,100,true,MediaDomain::Movies).unwrap().items.remove(0);
    let decision = search::evaluate(
        c,
        MediaDomain::Movies,
        uuid::Uuid::nil(),
        &release,
        context,
        chrono::DateTime::parse_from_rfc3339(now)
            .unwrap()
            .timestamp(),
    )
    .await
    .unwrap();
    assert_eq!(
        decision.target,
        Some(search::ReleaseTarget::Movies { movie_id: 1 })
    );
    assert_eq!(
        !decision.reasons.contains(&"movie_unavailable".into()),
        allowed,
        "{minimum} at {now}: {:?}",
        decision.reasons
    );
}

#[tokio::test]
async fn produced_status_preserves_date_driven_availability_and_recovers_deleted()
-> Result<(), Error> {
    use hrrdarr::{
        library::refresh::{self, Details, Target},
        search::SearchContext,
    };
    let files =
        Scratch(std::env::temp_dir().join(format!("hrrdarr-status-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&files.0)?;
    let value = Arc::new(Mutex::new(
        json!({"tmdbId":101,"title":"Film","year":2020,"status":"Released"}),
    ));
    let (origin, upstream) = server(
        Router::new()
            .route("/movie/{id}", get(facts))
            .with_state(value.clone()),
    )
    .await;
    let client = MetadataClient::with_origins(&format!("{origin}/"), &format!("{origin}/"))?;
    let db = Database::open_local(files.0.join("db")).await?;
    let c = db.connect().await?;
    c.execute_batch("INSERT INTO movie_metadata(id,tmdb_id,title,year,status) VALUES(1,101,'Film',2020,'deleted'); INSERT INTO movies(id,metadata_id,path) VALUES(1,1,'/movies/Film'); INSERT INTO library_settings(media_type,movie_id,minimum_availability) VALUES('movies',1,'announced'); INSERT INTO release_delay_policies VALUES('movies',0,0,0);").await?;
    let captured = refresh::capture(&c, Target::Movies { movie_id: 1 }).await?;
    let detail = client.movie(101).await?;
    assert_eq!(detail.status.as_deref(), Some("announced"));
    let tx = c.transaction().await?;
    refresh::apply(&tx, &captured, Details::Movie(detail)).await?;
    tx.commit().await?;
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM movie_metadata WHERE status='announced'"
        )
        .await,
        1
    );
    for (minimum, allowed) in [
        ("tba", true),
        ("announced", true),
        ("in_cinemas", false),
        ("released", false),
    ] {
        assert_movie_availability(
            &c,
            minimum,
            "2020-04-01T00:00:00Z",
            allowed,
            SearchContext::Rss,
        )
        .await;
    }
    // Producer's released status (dates are historical at retrieval) cannot bypass
    // the consumer's injected decision clock, home date, or configured delay.
    value.lock().unwrap()["inCinema"] = json!("2020-01-01T00:00:00Z");
    value.lock().unwrap()["digitalRelease"] = json!("2020-04-01T00:00:00Z");
    let detail = client.movie(101).await?;
    let tx = c.transaction().await?;
    refresh::apply(&tx, &captured, Details::Movie(detail)).await?;
    tx.commit().await?;
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM movie_metadata WHERE status='released'"
        )
        .await,
        1
    );
    assert_movie_availability(
        &c,
        "in_cinemas",
        "2019-12-31T23:59:59Z",
        false,
        SearchContext::Rss,
    )
    .await;
    assert_movie_availability(
        &c,
        "in_cinemas",
        "2020-01-01T00:00:00Z",
        true,
        SearchContext::Rss,
    )
    .await;
    assert_movie_availability(
        &c,
        "released",
        "2020-03-31T23:59:59Z",
        false,
        SearchContext::Rss,
    )
    .await;
    assert_movie_availability(
        &c,
        "released",
        "2020-04-01T00:00:00Z",
        true,
        SearchContext::Rss,
    )
    .await;
    c.execute(
        "UPDATE release_delay_policies SET availability_delay_days=1",
        (),
    )
    .await?;
    assert_movie_availability(
        &c,
        "released",
        "2020-04-01T00:00:00Z",
        false,
        SearchContext::Rss,
    )
    .await;
    assert_movie_availability(
        &c,
        "released",
        "2020-04-02T00:00:00Z",
        true,
        SearchContext::Rss,
    )
    .await;
    assert_movie_availability(
        &c,
        "released",
        "2020-03-01T00:00:00Z",
        true,
        SearchContext::UserSearch,
    )
    .await;
    *value.lock().unwrap() = json!({"tmdbId":101,"title":"Film","year":2020});
    let detail = client.movie(101).await?;
    let tx = c.transaction().await?;
    refresh::apply(&tx, &captured, Details::Movie(detail)).await?;
    tx.commit().await?;
    assert_eq!(count(&c,"SELECT count(*) FROM movie_metadata WHERE status='announced' AND digital_release='2020-04-01 00:00:00'").await,1);
    assert_movie_availability(
        &c,
        "released",
        "2020-04-01T00:00:00Z",
        false,
        SearchContext::Rss,
    )
    .await;
    assert_movie_availability(
        &c,
        "released",
        "2020-04-02T00:00:00Z",
        true,
        SearchContext::Rss,
    )
    .await;
    upstream.abort();
    let _ = upstream.await;
    Ok(())
}
