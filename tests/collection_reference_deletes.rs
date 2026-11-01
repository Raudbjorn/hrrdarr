//! Collection defaults hold RESTRICT references; deleting the referenced root folder,
//! quality profiles and tags stay protected; deleting a root disables its collection policy.
use hrrdarr::db::Database;
use libsql::Connection;
use std::sync::Arc;

struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.0) {
            eprintln!("collection reference fixture cleanup failed: {e}");
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
async fn delete(client: &reqwest::Client, base: &str, path: &str) -> u16 {
    client
        .delete(format!("{base}{path}"))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

#[tokio::test]
async fn deleting_collection_defaults_disables_policy_and_provider_tags_stay_in_use() {
    let scratch = Scratch(std::env::temp_dir().join(format!(
        "hrrdarr-collection-references-{}",
        uuid::Uuid::new_v4()
    )));
    std::fs::create_dir(&scratch.0).unwrap();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let c = db.connect().await.unwrap();
    c.execute_batch(
        "INSERT INTO quality_profiles(id,media_type,name)VALUES(1,'movies','Movie profile');
         INSERT INTO root_folders(id,media_type,path)VALUES(1,'movies','/movies');
         INSERT INTO tags(id,media_type,label)VALUES(1,'movies','collection'),(2,'movies','indexer'),(3,'movies','client');
         INSERT INTO movie_collections(id,tmdb_id,title)VALUES(1,55,'Collection');
         INSERT INTO movie_collection_settings(collection_id,root_folder_id,quality_profile_id,minimum_availability,search_on_add,monitored)VALUES(1,1,1,'released',0,1);
         INSERT INTO movie_collection_tags(collection_id,tag_id)VALUES(1,1); -- trigger: settings_revision 1 -> 2
         INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint)VALUES('00000000-0000-0000-0000-000000000001','torznab','Indexer',1,1,1,1,'http://127.0.0.1:1/');
         INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,remove_year)VALUES('00000000-0000-0000-0000-000000000001','torznab','movies','[2000]','[]',0);
         INSERT INTO provider_scope_tags(provider_id,media_type,tag_id)VALUES('00000000-0000-0000-0000-000000000001','movies',2);
         INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint)VALUES('00000000-0000-0000-0000-000000000002','qbittorrent','Client',1,1,1,1,'http://127.0.0.1:1/');
         INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags)VALUES('00000000-0000-0000-0000-000000000002','qbittorrent','movies','movies',0,0,'started','default',0,0,0);
         INSERT INTO download_client_tags(provider_id,media_type,tag_id)VALUES('00000000-0000-0000-0000-000000000002','movies',3);",
    )
    .await
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = hrrdarr::quality_profiles::router(db.clone())
        .merge(hrrdarr::root_folders::router(db.clone()))
        .merge(hrrdarr::tags::router(db.clone()));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap();

    assert_eq!(
        delete(&client, &base, "/api/v1/movies/quality-profiles/1").await,
        409 // Reasoning: a collection reference protects its quality profile until explicitly cleared.
    );
    assert_eq!(number(&c, "SELECT count(*) FROM movie_collection_settings WHERE quality_profile_id=1 AND monitored=1 AND settings_revision=2").await, 1);
    assert_eq!(
        delete(&client, &base, "/api/v1/movies/root-folders/1").await,
        204
    );
    assert_eq!(number(&c, "SELECT count(*) FROM movie_collection_settings WHERE root_folder_id IS NULL AND monitored=0 AND settings_revision=3").await, 1);
    // Reasoning: retain branch tag-in-use behavior; deletion cannot silently discard collection policy.
    assert_eq!(delete(&client, &base, "/api/v1/movies/tags/1").await, 409);
    assert_eq!(
        number(&c, "SELECT count(*) FROM movie_collection_tags").await,
        1
    );
    assert_eq!(
        number(
            &c,
            "SELECT settings_revision FROM movie_collection_settings"
        )
        .await,
        3 // Reasoning: rejected profile/tag deletions do not consume a settings revision.
    );
    // A provider scope tag is an assignment: conflict, not a storage failure.
    assert_eq!(delete(&client, &base, "/api/v1/movies/tags/2").await, 409);
    assert_eq!(number(&c, "SELECT count(*) FROM tags WHERE id=2").await, 1);
    // Download-client tags retain the same deletion protection as provider authority tags.
    assert_eq!(delete(&client, &base, "/api/v1/movies/tags/3").await, 409);
    assert_eq!(number(&c, "SELECT count(*) FROM tags WHERE id=3").await, 1);
    server.abort();
}
