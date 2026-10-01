use hrrdarr::{
    collections::repository,
    db::Database,
    library::refresh::{self, Details, Target},
    metadata::{
        CollectionAssociation as Association, CollectionSummary, MovieDetails, MovieWithCollection,
    },
};
use libsql::{Connection, TransactionBehavior};
struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.0) {
            eprintln!("collection fixture cleanup failed: {e}");
        }
    }
}
async fn number(c: &Connection, sql: &str) -> i64 {
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
fn association(id: i64) -> Association {
    Association::Present(CollectionSummary {
        tmdb_id: id,
        title: format!("Collection {id}"),
    })
}
fn movie() -> MovieDetails {
    serde_json::from_str(r#"{"tmdb_id":101,"title":"Refreshed","year":2020}"#).unwrap()
}
#[tokio::test]
async fn transactional_adoption_defaults_intent_and_fenced_refresh()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let scratch = Scratch(std::env::temp_dir().join(format!(
        "hrrdarr-collection-adoption-{}",
        uuid::Uuid::new_v4()
    )));
    std::fs::create_dir(&scratch.0)?;
    let db = Database::open_local(scratch.0.join("db")).await?;
    let c = db.connect().await?;
    c.execute_batch("INSERT INTO quality_profiles(id,media_type,name)VALUES(1,'movies','Movie profile'); INSERT INTO root_folders(id,media_type,path)VALUES(1,'movies','/movies'),(2,'movies','/movies/nested'); INSERT INTO movie_metadata(id,tmdb_id,title)VALUES(7,101,'Old'); INSERT INTO movies(id,metadata_id,path)VALUES(1,7,'/movies/nested/Film'); INSERT INTO library_settings(media_type,movie_id,quality_profile_id,minimum_availability)VALUES('movies',1,1,'released'); INSERT INTO tags(id,media_type,label)VALUES(1,'movies','one'); INSERT INTO movie_tags(movie_id,tag_id)VALUES(1,1); INSERT INTO movie_files(id,movie_id,path)VALUES(1,1,'/movies/nested/Film/movie.mkv');").await?;
    assert!(
        repository::adopt(&c, 1, 7, association(55), None)
            .await
            .is_err()
    );
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    assert!(repository::adopt(&tx, 1, 7, association(55), None).await?);
    tx.commit().await?;
    assert_eq!(
        number(&c, "SELECT root_folder_id FROM movie_collection_settings").await,
        2
    );
    assert_eq!(
        number(&c, "SELECT monitored FROM movie_collection_settings").await,
        0
    );
    assert_eq!(
        number(&c, "SELECT count(*) FROM movie_collection_tags").await,
        1
    );
    let id = number(&c, "SELECT id FROM movie_collections").await;
    // Native empty tags remain authoritative on later discovery.
    c.execute(
        "DELETE FROM movie_collection_tags WHERE collection_id=?",
        [id],
    )
    .await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    assert!(!repository::adopt(&tx, 1, 7, association(55), None).await?);
    tx.commit().await?;
    assert_eq!(
        number(&c, "SELECT count(*) FROM movie_collection_tags").await,
        0
    );
    let revision = number(
        &c,
        "SELECT settings_revision FROM movie_collection_settings",
    )
    .await;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    assert!(
        repository::adopt(&tx, 1, 7, association(55), Some(Some(revision + 1)))
            .await
            .is_err()
    );
    tx.rollback().await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    repository::adopt(&tx, 1, 7, association(55), Some(Some(revision))).await?;
    tx.commit().await?;
    assert_eq!(
        number(&c, "SELECT monitored FROM movie_collection_settings").await,
        1
    );
    assert_eq!(
        number(&c, "SELECT local_edit FROM movie_collection_intents").await,
        1
    );
    let captured = refresh::capture(&c, Target::Movies { movie_id: 1 }).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    refresh::apply(
        &tx,
        &captured,
        Details::MovieWithCollection(MovieWithCollection {
            movie: movie(),
            collection: Association::Absent,
        }),
    )
    .await?;
    tx.commit().await?;
    assert_eq!(
        number(&c, "SELECT count(*) FROM movie_collection_members").await,
        1
    );
    // Captured external identity changed while a metadata request was in flight.
    c.execute("UPDATE movie_metadata SET tmdb_id=102 WHERE id=7", ())
        .await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    assert!(matches!(
        refresh::apply(
            &tx,
            &captured,
            Details::MovieWithCollection(MovieWithCollection {
                movie: movie(),
                collection: Association::Clear
            })
        )
        .await,
        Err(refresh::Error::TargetChanged)
    ));
    tx.rollback().await?;
    assert_eq!(
        number(&c, "SELECT count(*) FROM movie_collection_members").await,
        1
    );
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    repository::adopt(&tx, 1, 7, Association::Clear, None).await?;
    tx.commit().await?;
    assert_eq!(
        number(&c, "SELECT count(*) FROM movie_collections").await,
        0
    );
    assert_eq!(
        number(&c, "SELECT removed FROM movie_collection_intents").await,
        1
    );
    assert_eq!(number(&c, "SELECT count(*) FROM movie_files").await, 1);
    assert_eq!(number(&c, "SELECT count(*) FROM movies").await, 1);
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    assert!(repository::adopt(&tx, 1, 7, association(55), Some(None)).await?);
    tx.commit().await?;
    assert_eq!(
        number(
            &c,
            "SELECT removed FROM movie_collection_intents WHERE tmdb_id=55"
        )
        .await,
        0
    );
    assert_eq!(
        number(
            &c,
            "SELECT local_edit FROM movie_collection_intents WHERE tmdb_id=55"
        )
        .await,
        1
    );
    // A later authoritative repoint and rediscovery are not user deletions.
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    repository::adopt(&tx, 1, 7, association(66), None).await?;
    tx.commit().await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    assert!(repository::adopt(&tx, 1, 7, association(55), None).await?);
    tx.commit().await?;
    assert_eq!(
        number(
            &c,
            "SELECT removed FROM movie_collection_intents WHERE tmdb_id=55"
        )
        .await,
        0
    );
    // A real user-removal tombstone cannot be bypassed by fresh discovery.
    c.execute("UPDATE movie_collection_intents SET removed=1,removal_reason='user',revision=revision+1 WHERE tmdb_id=55",()).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    assert!(!repository::adopt(&tx, 1, 7, association(55), None).await?);
    tx.commit().await?;
    // Legacy unidentified memberships are preserved, not guessed from title.
    c.execute(
        "INSERT INTO movie_collections(id,title)VALUES(999,'Legacy')",
        (),
    )
    .await?;
    c.execute(
        "INSERT INTO movie_collection_members(collection_id,metadata_id)VALUES(999,7)",
        (),
    )
    .await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    assert!(
        repository::adopt(&tx, 1, 7, Association::Clear, None)
            .await
            .is_err()
    );
    tx.rollback().await?;
    assert_eq!(
        number(&c, "SELECT count(*) FROM movie_collection_members").await,
        2
    );
    drop(c);
    drop(db);
    let reopened = Database::open_local(scratch.0.join("db")).await?;
    assert_eq!(
        number(
            &reopened.connect().await?,
            "SELECT local_edit FROM movie_collection_intents WHERE tmdb_id=55"
        )
        .await,
        1
    );
    Ok(())
}

#[tokio::test]
async fn selected_movie_http_uses_companion_and_rolls_back_invalid_monitoring()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use axum::{Json, Router, extract::Path, routing::get};
    use serde_json::json;
    use std::{sync::Arc, time::Duration};
    struct Server(tokio::task::JoinHandle<std::io::Result<()>>);
    impl Drop for Server {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    async fn serve(router: Router) -> (String, Server) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        (
            url,
            Server(tokio::spawn(
                async move { axum::serve(listener, router).await },
            )),
        )
    }
    let scratch = Scratch(
        std::env::temp_dir().join(format!("hrrdarr-collection-http-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&scratch.0)?;
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await?);
    let c = db.connect().await?;
    c.execute_batch("INSERT INTO quality_profiles(id,media_type,name)VALUES(1,'movies','Movie'); INSERT INTO root_folders(id,media_type,path)VALUES(1,'movies','/movies'); INSERT INTO series(id,title,path,monitored)VALUES(1,'TV','/tv/unchanged',0);").await?;
    let (origin,mut peer)=serve(Router::new().route("/movie/{id}",get(|Path(id):Path<i64>|async move{Json(json!({"tmdbId":id,"title":format!("Movie {id}"),"collection":{"tmdbId":if id==104 {66}else{55},"name":"Collection"}}))}))).await;
    let metadata = Arc::new(hrrdarr::metadata::MetadataClient::with_origins(
        &format!("{origin}/"),
        &format!("{origin}/"),
    )?);
    let (api, mut server) = serve(hrrdarr::library::metadata_router(db.clone(), metadata)).await;
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    let endpoint = format!("{api}/api/v1/movies/lookup");
    let send = |body: serde_json::Value| {
        http.post(&endpoint)
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
    };
    let response=send(json!({"tmdb_id":101,"path":"/movies/first","settings":{"quality_profile_id":1,"minimum_availability":"released"}})).await?;
    assert_eq!(response.status().as_u16(), 201);
    assert_eq!(
        number(&c, "SELECT monitored FROM movie_collection_settings").await,
        0
    );
    let revision = number(
        &c,
        "SELECT settings_revision FROM movie_collection_settings",
    )
    .await;
    let response=send(json!({"tmdb_id":102,"path":"/movies/second","monitor":"movie_and_collection","collection_expected_revision":revision+1,"settings":{"quality_profile_id":1,"minimum_availability":"released"}})).await?;
    assert_eq!(response.status().as_u16(), 409);
    assert_eq!(number(&c, "SELECT count(*) FROM movies").await, 1);
    let response=send(json!({"tmdb_id":102,"path":"/movies/second","monitor":"movie_and_collection","collection_expected_revision":revision,"settings":{"quality_profile_id":1,"minimum_availability":"released"}})).await?;
    assert_eq!(response.status().as_u16(), 201);
    assert_eq!(
        number(&c, "SELECT monitored FROM movie_collection_settings").await,
        1
    );
    // Missing required profile/root defaults cannot leave a movie or collection behind.
    let response =
        send(json!({"tmdb_id":104,"path":"/elsewhere/new","monitor":"movie_and_collection"}))
            .await?;
    assert_eq!(response.status().as_u16(), 409);
    assert_eq!(number(&c, "SELECT count(*) FROM movies").await, 2);
    assert_eq!(
        number(
            &c,
            "SELECT count(*) FROM movie_collections WHERE tmdb_id=66"
        )
        .await,
        0
    );
    assert_eq!(
        number(&c, "SELECT monitored FROM series WHERE id=1").await,
        0
    );
    assert_eq!(number(&c, "SELECT count(*) FROM search_commands").await, 0);
    server.0.abort();
    peer.0.abort();
    for handle in [&mut server.0, &mut peer.0] {
        assert!(
            tokio::time::timeout(Duration::from_secs(2), handle)
                .await?
                .unwrap_err()
                .is_cancelled()
        );
    }
    Ok(())
}
