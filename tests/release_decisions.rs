use hrrdarr::{
    api::MediaDomain,
    db::Database,
    providers::indexer,
    search::{self, Disposition, ReleaseTarget, SearchContext},
};
use std::sync::Arc;
struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
fn release(title: &str, tv: bool) -> indexer::Release {
    let category = if tv { 5030 } else { 2030 };
    let xml = format!(
        r#"<rss xmlns:torznab="http://torznab.com/schemas/2015/feed"><channel><item><title>{title}</title><guid>release-guid</guid><pubDate>Thu, 24 Sep 2026 12:00:00 +0000</pubDate><link>https://example.invalid/private</link><torznab:attr name="category" value="{category}"/><torznab:attr name="size" value="1073741824"/></item></channel></rss>"#
    );
    indexer::parse_page(
        &xml,
        0,
        100,
        true,
        if tv {
            MediaDomain::Tv
        } else {
            MediaDomain::Movies
        },
    )
    .unwrap()
    .items
    .remove(0)
}
#[tokio::test]
async fn both_domain_decisions_and_real_search_consumer() {
    let path = std::env::temp_dir().join(format!("hrrdarr-release-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&path).unwrap();
    let _scratch = Scratch(path.clone());
    let db = Arc::new(Database::open_local(path.join("test.db")).await.unwrap());
    let c = db.connect().await.unwrap();
    c.execute_batch("INSERT INTO series(id,tvdb_id,title,path) VALUES(1,101,'Harbor','/synthetic/tv'); INSERT INTO seasons(series_id,number) VALUES(1,1); INSERT INTO episodes(id,series_id,season,number,title,runtime) VALUES(1,1,1,1,'Pilot',45),(2,1,1,2,'Second',45); UPDATE episodes SET air_date_utc='2026-09-23 12:00:00'; INSERT INTO movie_metadata(id,tmdb_id,title,year,runtime,digital_release) VALUES(1,201,'Harbor',2026,100,'2026-09-25 12:00:00'); INSERT INTO movies(id,metadata_id,path) VALUES(1,1,'/synthetic/movie'); INSERT INTO movie_alternative_titles VALUES(1,'Safe Harbor'); INSERT INTO quality_profiles VALUES(1,'tv','HD'),(2,'movies','HD'); INSERT INTO quality_profile_items(profile_id,media_type,quality_id,position,allowed) VALUES(1,'tv',5,0,1),(1,'tv',3,1,1),(2,'movies',5,0,1),(2,'movies',3,1,1); INSERT INTO quality_profile_policies(profile_id,media_type,upgrade_allowed,cutoff_quality_id,min_format_score,cutoff_format_score,min_upgrade_format_score,language_id) VALUES(1,'tv',1,3,0,0,1,NULL),(2,'movies',1,3,0,0,1,-2); INSERT INTO library_settings(media_type,series_id,quality_profile_id,series_type,use_scene_numbering) VALUES('tv',1,1,'standard',0); INSERT INTO library_settings(media_type,movie_id,quality_profile_id,minimum_availability) VALUES('movies',1,2,'released'); INSERT INTO release_delay_policies VALUES('tv',60,60,0),('movies',60,60,0);").await.unwrap();
    let now = chrono::DateTime::parse_from_rfc3339("2026-09-24T12:30:00Z")
        .unwrap()
        .timestamp();
    let tv = release("Harbor.S01E01.1080p.WEB-DL", true);
    let movie = release("Safe.Harbor.2026.1080p.WEB-DL", false);
    let result = search::evaluate(&c, MediaDomain::Tv, &tv, SearchContext::Rss, now)
        .await
        .unwrap();
    assert_eq!(result.disposition, Disposition::Delay);
    assert_eq!(result.not_before, Some(now + 1800));
    assert_eq!(
        result.target,
        Some(ReleaseTarget::Tv {
            series_id: 1,
            episode_ids: vec![1]
        })
    );
    let result = search::evaluate(&c, MediaDomain::Movies, &movie, SearchContext::Rss, now)
        .await
        .unwrap();
    assert!(result.reasons.contains(&"movie_unavailable".into()));
    for (domain, item) in [(MediaDomain::Tv, &tv), (MediaDomain::Movies, &movie)] {
        assert_eq!(
            search::evaluate(&c, domain, item, SearchContext::UserSearch, now)
                .await
                .unwrap()
                .disposition,
            Disposition::Accept
        );
        assert_eq!(
            search::evaluate(&c, domain, item, SearchContext::Rss, now + 172800)
                .await
                .unwrap()
                .disposition,
            Disposition::Accept
        );
    }
    c.execute("UPDATE seasons SET monitored=0", ())
        .await
        .unwrap();
    assert!(
        search::evaluate(&c, MediaDomain::Tv, &tv, SearchContext::Rss, now)
            .await
            .unwrap()
            .reasons
            .contains(&"not_monitored".into())
    );
    c.execute("UPDATE seasons SET monitored=1", ())
        .await
        .unwrap();
    let remake = release("Harbor.1982.1080p.WEB-DL", false);
    assert_eq!(
        search::evaluate(
            &c,
            MediaDomain::Movies,
            &remake,
            SearchContext::UserSearch,
            now
        )
        .await
        .unwrap()
        .reasons,
        vec!["no_library_match"]
    );
    let mut wrong = release("Harbor.2026.1080p.WEB-DL", false);
    wrong.metadata.categories = vec![3000];
    assert_eq!(
        search::evaluate(
            &c,
            MediaDomain::Movies,
            &wrong,
            SearchContext::UserSearch,
            now
        )
        .await
        .unwrap()
        .reasons,
        vec!["wrong_media_category"]
    );
    let mut conflicting = release("Harbor.2026.1080p.WEB-DL", false);
    conflicting.facts.identifiers = indexer::ReleaseIdentifiers::Movie {
        tmdb_id: Some(999),
        imdb_id: None,
    };
    assert_eq!(
        search::evaluate(
            &c,
            MediaDomain::Movies,
            &conflicting,
            SearchContext::UserSearch,
            now
        )
        .await
        .unwrap()
        .reasons,
        vec!["no_library_match"]
    );
    c.execute_batch("INSERT INTO episode_files(id,series_id,path) VALUES(1,1,'/synthetic/tv/old'); UPDATE episodes SET episode_file_id=1 WHERE id=1; INSERT INTO file_metadata(media_type,episode_file_id,quality_id) VALUES('tv',1,5)").await.unwrap();
    assert_eq!(
        search::evaluate(&c, MediaDomain::Tv, &tv, SearchContext::UserSearch, now)
            .await
            .unwrap()
            .disposition,
        Disposition::Accept
    );
    c.execute("UPDATE file_metadata SET quality_id=3", ())
        .await
        .unwrap();
    assert!(
        search::evaluate(&c, MediaDomain::Tv, &tv, SearchContext::UserSearch, now)
            .await
            .unwrap()
            .reasons
            .contains(&"cutoff_met".into())
    );
    c.execute("UPDATE episodes SET episode_file_id=NULL", ())
        .await
        .unwrap();

    c.execute(
        "UPDATE episodes SET air_date_utc='2027-01-01 00:00:00' WHERE id=1",
        (),
    )
    .await
    .unwrap();
    assert!(
        search::evaluate(&c, MediaDomain::Tv, &tv, SearchContext::Rss, now)
            .await
            .unwrap()
            .reasons
            .contains(&"episode_not_aired".into())
    );
    c.execute("UPDATE episodes SET air_date_utc='2026-09-23 12:00:00'", ())
        .await
        .unwrap();
    c.execute(
        "UPDATE quality_profile_policies SET cutoff_format_score=10 WHERE profile_id=1",
        (),
    )
    .await
    .unwrap();
    assert!(
        search::evaluate(&c, MediaDomain::Tv, &tv, SearchContext::UserSearch, now)
            .await
            .unwrap()
            .reasons
            .contains(&"custom_format_policy_unsupported".into())
    );
    c.execute(
        "UPDATE quality_profile_policies SET cutoff_format_score=0",
        (),
    )
    .await
    .unwrap();
    c.execute_batch("UPDATE library_settings SET use_scene_numbering=1 WHERE media_type='tv'; UPDATE episodes SET scene_season_number=1,scene_episode_number=1;").await.unwrap();
    let pack = release("Harbor.S01E01E02.1080p.WEB-DL", true);
    assert_eq!(
        search::evaluate(&c, MediaDomain::Tv, &pack, SearchContext::UserSearch, now)
            .await
            .unwrap()
            .reasons,
        vec!["no_library_match"]
    );
    c.execute(
        "UPDATE library_settings SET use_scene_numbering=0 WHERE media_type='tv'",
        (),
    )
    .await
    .unwrap();
    c.execute("UPDATE movie_metadata SET status='deleted'", ())
        .await
        .unwrap();
    assert!(
        search::evaluate(
            &c,
            MediaDomain::Movies,
            &movie,
            SearchContext::UserSearch,
            now
        )
        .await
        .unwrap()
        .reasons
        .contains(&"movie_deleted".into())
    );
    c.execute("UPDATE movie_metadata SET status=NULL", ())
        .await
        .unwrap();
    c.execute_batch("INSERT INTO movie_metadata(id,tmdb_id,title,year,runtime) VALUES(2,202,'Harbor',2026,100); INSERT INTO movies(id,metadata_id,path) VALUES(2,2,'/synthetic/duplicate');").await.unwrap();
    assert_eq!(
        search::evaluate(
            &c,
            MediaDomain::Movies,
            &release("Harbor.2026.1080p.WEB-DL", false),
            SearchContext::UserSearch,
            now
        )
        .await
        .unwrap()
        .reasons,
        vec!["ambiguous_library_match"]
    );
    c.execute("DELETE FROM movies WHERE id=2", ())
        .await
        .unwrap();
    // Equal-quality group membership is one rank; a different catalog ID is not an upgrade.
    c.execute_batch("INSERT INTO quality_profile_groups(id,profile_id,name,position,allowed) VALUES(1,1,'WEB',0,1); UPDATE quality_profile_policies SET cutoff_quality_id=NULL,cutoff_group_id=1 WHERE profile_id=1; UPDATE quality_profile_items SET group_id=1 WHERE profile_id=1; UPDATE episodes SET episode_file_id=1 WHERE id=1; UPDATE file_metadata SET quality_id=5 WHERE episode_file_id=1;").await.unwrap();
    assert!(
        search::evaluate(&c, MediaDomain::Tv, &tv, SearchContext::UserSearch, now)
            .await
            .unwrap()
            .reasons
            .contains(&"cutoff_met".into())
    );
    c.execute_batch("UPDATE quality_profile_items SET group_id=NULL WHERE profile_id=1; UPDATE quality_profile_policies SET cutoff_quality_id=3,cutoff_group_id=NULL WHERE profile_id=1; DELETE FROM quality_profile_groups WHERE id=1; UPDATE episodes SET episode_file_id=NULL;").await.unwrap();
    // Overflow must fail even when the first row already looks like a unique match.
    c.execute_batch("WITH RECURSIVE n(x) AS (SELECT 2 UNION ALL SELECT x+1 FROM n WHERE x<10001) INSERT INTO series(id,title,path) SELECT x,'Other '||x,'/synthetic/extra/'||x FROM n;").await.unwrap();
    assert_eq!(
        search::evaluate(&c, MediaDomain::Tv, &tv, SearchContext::UserSearch, now)
            .await
            .unwrap_err()
            .0,
        "library_match_limit"
    );
    c.execute("DELETE FROM series WHERE id>1", ())
        .await
        .unwrap();
    c.execute_batch("WITH RECURSIVE n(x) AS (SELECT 3 UNION ALL SELECT x+1 FROM n WHERE x<10001) INSERT INTO episodes(id,series_id,season,number,title) SELECT x,1,1,x,'Other' FROM n;").await.unwrap();
    assert_eq!(
        search::evaluate(&c, MediaDomain::Tv, &tv, SearchContext::UserSearch, now)
            .await
            .unwrap_err()
            .0,
        "episode_match_limit"
    );
    c.execute("DELETE FROM episodes WHERE id>2", ())
        .await
        .unwrap();
    // Both read-only HTTP search and RSS evaluator consume the same real Torznab parser.
    async fn feed(axum::extract::OriginalUri(uri): axum::extract::OriginalUri) -> String {
        if uri.query().unwrap_or("").contains("t=caps") {
            return r#"<caps><limits max="100" default="10"/><searching><search available="yes" supportedParams="q"/><tv-search available="yes" supportedParams="q,tvdbid,season,ep"/><movie-search available="yes" supportedParams="q,tmdbid"/></searching><categories><category id="5000"><subcat id="5030"/></category><category id="2000"><subcat id="2030"/></category></categories></caps>"#.into();
        }
        let tv = uri.query().unwrap_or("").contains("tvsearch");
        let (title, category) = if tv {
            ("Harbor.S01E01.1080p.WEB-DL", 5030)
        } else {
            ("Harbor.2026.1080p.WEB-DL", 2030)
        };
        format!(
            r#"<rss xmlns:torznab="http://torznab.com/schemas/2015/feed"><channel><item><title>{title}</title><guid>PRIVATE_GUID</guid><pubDate>Thu, 24 Sep 2026 12:00:00 +0000</pubDate><link>https://example.invalid/PRIVATE_LOCATOR</link><torznab:attr name="category" value="{category}"/><torznab:attr name="size" value="1073741824"/></item></channel></rss>"#
        )
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/api", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            axum::Router::new().route("/api", axum::routing::get(feed)),
        )
        .await
        .unwrap()
    });
    let (providers, transport) = hrrdarr::providers::router_with_refresh(db.clone(), None);
    let app = providers.merge(search::router(db.clone(), transport));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let api = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap();
    let response=client.post(format!("{base}/api/v1/providers")).header("content-type","application/json").body(serde_json::json!({"name":"Test","enabled":true,"priority":1,"settings":{"implementation":"torznab","endpoint":endpoint,"tv":{"categories":[5030],"anime_categories":[]},"movies":{"categories":[2030]}}}).to_string()).send().await.unwrap();
    assert_eq!(response.status(), 201);
    let provider: serde_json::Value =
        serde_json::from_str(&response.text().await.unwrap()).unwrap();
    for media in ["episode", "movie"] {
        let response=client.post(format!("{base}/api/v1/release-search")).header("content-type","application/json").body(serde_json::json!({"provider_id":provider["id"],"provider_revision":1,"target":{"media_type":media,"id":1},"offset":0,"query_index":0,"limit":10}).to_string()).send().await.unwrap();
        let status = response.status();
        let body = response.text().await.unwrap();
        assert_eq!(status, 200, "{body}");
        assert!(!body.contains("PRIVATE_"));
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            value["items"][0]["decision"]["disposition"], "accept",
            "{value}"
        );
    }

    let response = client
        .get(format!("{base}/api/v1/release-policies/tv?unknown=1"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let response = client
        .put(format!("{base}/api/v1/release-policies/tv"))
        .header("content-type", "application/json")
        .body(r#"{"torrent_delay_minutes":0,"usenet_delay_minutes":0,"availability_delay_days":1}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let response = client
        .put(format!("{base}/api/v1/release-policies/movies"))
        .header("content-type", "application/json")
        .body(
            r#"{"torrent_delay_minutes":30,"usenet_delay_minutes":60,"availability_delay_days":2}"#,
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        c.query(
            "SELECT availability_delay_days FROM release_delay_policies WHERE media_type='movies'",
            ()
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<i64>(0)
        .unwrap(),
        2
    );
    api.abort();
    server.abort();
    let _ = api.await;
    let _ = server.await;
    drop(c);
    drop(db);
}
