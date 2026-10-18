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
    let fixture_indexer_id = fixture_indexer(&c).await;
    // Any (-1) keeps this fixture language-unrestricted; Original (-2) requires audio matching.
    c.execute_batch("INSERT INTO series(id,tvdb_id,title,path) VALUES(1,101,'Harbor','/synthetic/tv'); INSERT INTO seasons(series_id,number) VALUES(1,1); INSERT INTO episodes(id,series_id,season,number,title,runtime) VALUES(1,1,1,1,'Pilot',45),(2,1,1,2,'Second',45); UPDATE episodes SET air_date_utc='2026-09-23 12:00:00'; INSERT INTO movie_metadata(id,tmdb_id,title,year,runtime,digital_release) VALUES(1,201,'Harbor',2026,100,'2026-09-25 12:00:00'); INSERT INTO movies(id,metadata_id,path) VALUES(1,1,'/synthetic/movie'); INSERT INTO movie_alternative_titles VALUES(1,'Safe Harbor'); INSERT INTO quality_profiles VALUES(1,'tv','HD'),(2,'movies','HD'); INSERT INTO quality_profile_items(profile_id,media_type,quality_id,position,allowed) VALUES(1,'tv',5,0,1),(1,'tv',3,1,1),(2,'movies',5,0,1),(2,'movies',3,1,1); INSERT INTO quality_profile_policies(profile_id,media_type,upgrade_allowed,cutoff_quality_id,min_format_score,cutoff_format_score,min_upgrade_format_score,language_id) VALUES(1,'tv',1,3,0,0,1,NULL),(2,'movies',1,3,0,0,1,-1); INSERT INTO library_settings(media_type,series_id,quality_profile_id,series_type,use_scene_numbering) VALUES('tv',1,1,'standard',0); INSERT INTO library_settings(media_type,movie_id,quality_profile_id,minimum_availability) VALUES('movies',1,2,'released'); INSERT INTO release_delay_policies VALUES('tv',60,60,0),('movies',60,60,0);").await.unwrap();
    let now = chrono::DateTime::parse_from_rfc3339("2026-09-24T12:30:00Z")
        .unwrap()
        .timestamp();
    let tv = release("Harbor.S01E01.1080p.WEB-DL", true);
    let movie = release("Safe.Harbor.2026.1080p.WEB-DL", false);
    let result = search::evaluate(
        &c,
        MediaDomain::Tv,
        fixture_indexer_id,
        &tv,
        SearchContext::Rss,
        now,
    )
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
    let result = search::evaluate(
        &c,
        MediaDomain::Movies,
        fixture_indexer_id,
        &movie,
        SearchContext::Rss,
        now,
    )
    .await
    .unwrap();
    assert!(result.reasons.contains(&"movie_unavailable".into()));
    for (domain, item) in [(MediaDomain::Tv, &tv), (MediaDomain::Movies, &movie)] {
        assert_eq!(
            search::evaluate(
                &c,
                domain,
                fixture_indexer_id,
                item,
                SearchContext::UserSearch,
                now
            )
            .await
            .unwrap()
            .disposition,
            Disposition::Accept
        );
        assert_eq!(
            search::evaluate(
                &c,
                domain,
                fixture_indexer_id,
                item,
                SearchContext::Rss,
                now + 172800
            )
            .await
            .unwrap()
            .disposition,
            Disposition::Accept
        );
    }
    // Explicit indexer language is now trusted parsed evidence (not media-stream verification).
    // Original/concrete English match; Unknown and other languages remain rejected.
    c.execute("UPDATE movie_metadata SET original_language=1", ())
        .await
        .unwrap();
    let mut language_movie = release("Safe.Harbor.2026.1080p.WEB-DL", false);
    language_movie.metadata.languages = vec!["English".into()];
    for language in [-2, 0, 1, 57, -1] {
        c.execute(
            "UPDATE quality_profile_policies SET language_id=? WHERE media_type='movies'",
            [language],
        )
        .await
        .unwrap();
        for context in [SearchContext::UserSearch, SearchContext::Rss] {
            let result = search::evaluate(
                &c,
                MediaDomain::Movies,
                fixture_indexer_id,
                &language_movie,
                context,
                now + 172800,
            )
            .await
            .unwrap();
            assert_eq!(
                result.disposition,
                if matches!(language, -2 | 1 | -1) {
                    Disposition::Accept
                } else {
                    Disposition::Reject
                }
            );
            assert_eq!(
                result.reasons.contains(&"language_not_wanted".into()),
                !matches!(language, -2 | 1 | -1)
            );
            assert_eq!(
                search::evaluate(
                    &c,
                    MediaDomain::Tv,
                    fixture_indexer_id,
                    &tv,
                    context,
                    now + 172800
                )
                .await
                .unwrap()
                .disposition,
                Disposition::Accept
            );
        }
    }
    c.execute("UPDATE seasons SET monitored=0", ())
        .await
        .unwrap();
    assert!(
        search::evaluate(
            &c,
            MediaDomain::Tv,
            fixture_indexer_id,
            &tv,
            SearchContext::Rss,
            now
        )
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
            fixture_indexer_id,
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
            fixture_indexer_id,
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
            fixture_indexer_id,
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
        search::evaluate(
            &c,
            MediaDomain::Tv,
            fixture_indexer_id,
            &tv,
            SearchContext::UserSearch,
            now
        )
        .await
        .unwrap()
        .disposition,
        Disposition::Accept
    );
    c.execute("UPDATE file_metadata SET quality_id=3", ())
        .await
        .unwrap();
    assert!(
        search::evaluate(
            &c,
            MediaDomain::Tv,
            fixture_indexer_id,
            &tv,
            SearchContext::UserSearch,
            now
        )
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
    // With no active air-date restriction, future episodes have no blanket RSS veto.
    // Precise publication/grace and pack rules are exercised separately below.
    assert!(
        !search::evaluate(
            &c,
            MediaDomain::Tv,
            fixture_indexer_id,
            &tv,
            SearchContext::Rss,
            now
        )
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
    // A nonzero format cutoff is supported; without matching formats it adds no rejection.
    assert!(
        !search::evaluate(
            &c,
            MediaDomain::Tv,
            fixture_indexer_id,
            &tv,
            SearchContext::UserSearch,
            now
        )
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
        search::evaluate(
            &c,
            MediaDomain::Tv,
            fixture_indexer_id,
            &pack,
            SearchContext::UserSearch,
            now
        )
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
            fixture_indexer_id,
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
            fixture_indexer_id,
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
        search::evaluate(
            &c,
            MediaDomain::Tv,
            fixture_indexer_id,
            &tv,
            SearchContext::UserSearch,
            now
        )
        .await
        .unwrap()
        .reasons
        .contains(&"cutoff_met".into())
    );
    c.execute_batch("UPDATE quality_profile_items SET group_id=NULL WHERE profile_id=1; UPDATE quality_profile_policies SET cutoff_quality_id=3,cutoff_group_id=NULL WHERE profile_id=1; DELETE FROM quality_profile_groups WHERE id=1; UPDATE episodes SET episode_file_id=NULL;").await.unwrap();
    // Overflow must fail even when the first row already looks like a unique match.
    c.execute_batch("WITH RECURSIVE n(x) AS (SELECT 2 UNION ALL SELECT x+1 FROM n WHERE x<10001) INSERT INTO series(id,title,path) SELECT x,'Other '||x,'/synthetic/extra/'||x FROM n;").await.unwrap();
    assert_eq!(
        search::evaluate(
            &c,
            MediaDomain::Tv,
            fixture_indexer_id,
            &tv,
            SearchContext::UserSearch,
            now
        )
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
        search::evaluate(
            &c,
            MediaDomain::Tv,
            fixture_indexer_id,
            &tv,
            SearchContext::UserSearch,
            now
        )
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

    // Missing audio evidence now rejects with the actual language mismatch reason.
    for language in [-2, 1, -1] {
        c.execute(
            "UPDATE quality_profile_policies SET language_id=? WHERE media_type='movies'",
            [language],
        )
        .await
        .unwrap();
        let response = client.post(format!("{base}/api/v1/release-search"))
            .header("content-type", "application/json").body(serde_json::json!({"provider_id":provider["id"],"provider_revision":1,"target":{"media_type":"movie","id":1},"offset":0,"query_index":0,"limit":10}).to_string())
            .send().await.unwrap().error_for_status().unwrap().text().await.unwrap();
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(
            value["items"][0]["decision"]["disposition"],
            if language == -1 { "accept" } else { "reject" }
        );
        assert_eq!(
            value["items"][0]["decision"]["reasons"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("language_not_wanted")),
            language != -1
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

// scn.002 follow-up: decision::evaluate's own Absolute (anime) numbering arm had the same
// scene-absolute-number-vs-plain-column gap search::downloaded::evaluate's first draft had --
// no fallback, so an empty or ambiguous scene_absolute_episode_number match silently rejected a
// real anime release at search/decision time instead of falling back to absolute_episode_number
// (mirrors downloaded::match_absolute and upstream ParsingService.GetAnimeEpisodes). This test
// exercises the three cases that distinguish "no fallback" from "fallback, discard-not-reject":
// an empty scene-column result, an ambiguous (multi-hit) scene-column result, and a genuine
// plain-column duplicate that must still fail once the fallback runs.
#[tokio::test]
async fn anime_scene_absolute_falls_back_to_plain_column_at_decision_time() {
    let path = std::env::temp_dir().join(format!("hrrdarr-release-anime-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&path).unwrap();
    let _scratch = Scratch(path.clone());
    let db = Database::open_local(path.join("test.db")).await.unwrap();
    let c = db.connect().await.unwrap();
    let fixture_indexer_id = fixture_indexer(&c).await;
    c.execute_batch(
        "INSERT INTO quality_profiles VALUES(1,'tv','HD');\
         INSERT INTO quality_profile_items(profile_id,media_type,quality_id,position,allowed)VALUES(1,'tv',3,0,1);\
         INSERT INTO quality_profile_policies(profile_id,media_type,upgrade_allowed,cutoff_quality_id,min_format_score,cutoff_format_score,min_upgrade_format_score,language_id)VALUES(1,'tv',1,3,0,0,1,NULL);\
         INSERT INTO release_delay_policies VALUES('tv',0,0,0);\
         INSERT INTO series(id,title,path)VALUES(1,'Aurora','/synthetic/tv-anime');\
         INSERT INTO seasons(series_id,number)VALUES(1,1);\
         INSERT INTO library_settings(media_type,series_id,quality_profile_id,series_type,use_scene_numbering)VALUES('tv',1,1,'anime',1);\
         INSERT INTO episodes(id,series_id,season,number,title,runtime,absolute_episode_number,scene_absolute_episode_number)VALUES\
            (1,1,1,1,'No scene mapping, falls back to the plain column',45,51,NULL),\
            (2,1,1,2,'Shares a scene number but unique on the plain column',45,60,60),\
            (3,1,1,3,'Collides on the scene number only',45,70,60),\
            (4,1,1,4,'Duplicate on the plain column too',45,80,NULL),\
            (5,1,1,5,'Second duplicate on the plain column',45,80,NULL);",
    )
    .await
    .unwrap();
    let now = chrono::DateTime::parse_from_rfc3339("2026-09-26T12:00:00Z")
        .unwrap()
        .timestamp();

    // Empty scene-column result (no episode has scene_absolute_episode_number=51): discarded,
    // not rejected -- falls back to absolute_episode_number, which uniquely resolves to
    // episode 1.
    let empty_scene = release("Aurora - 051 1080p WEB-DL", true);
    let result = search::evaluate(
        &c,
        MediaDomain::Tv,
        fixture_indexer_id,
        &empty_scene,
        SearchContext::UserSearch,
        now,
    )
    .await
    .unwrap();
    assert_eq!(
        result.disposition,
        Disposition::Accept,
        "{:?}",
        result.reasons
    );
    assert_eq!(
        result.target,
        Some(ReleaseTarget::Tv {
            series_id: 1,
            episode_ids: vec![1]
        })
    );

    // Ambiguous scene-column result (episodes 2 and 3 both have scene_absolute_episode_number=60):
    // discarded, not rejected -- falls back to absolute_episode_number=60, which only episode 2
    // has (episode 3's plain column is 70), so the fallback still resolves uniquely.
    let ambiguous_scene = release("Aurora - 060 1080p WEB-DL", true);
    let result = search::evaluate(
        &c,
        MediaDomain::Tv,
        fixture_indexer_id,
        &ambiguous_scene,
        SearchContext::UserSearch,
        now,
    )
    .await
    .unwrap();
    assert_eq!(
        result.disposition,
        Disposition::Accept,
        "{:?}",
        result.reasons
    );
    assert_eq!(
        result.target,
        Some(ReleaseTarget::Tv {
            series_id: 1,
            episode_ids: vec![2]
        })
    );

    // Once the scene column is empty/ambiguous and matching falls back to the plain column,
    // a genuine duplicate there (episodes 4 and 5 both have absolute_episode_number=80) is a
    // real, unresolved ambiguity -- the series never becomes a complete/unique candidate.
    let plain_duplicate = release("Aurora - 080 1080p WEB-DL", true);
    let result = search::evaluate(
        &c,
        MediaDomain::Tv,
        fixture_indexer_id,
        &plain_duplicate,
        SearchContext::UserSearch,
        now,
    )
    .await
    .unwrap();
    assert_eq!(result.reasons, vec!["no_library_match"]);
}

#[tokio::test]
async fn cross_type_numbering_resolves_identity_before_policy_facts() {
    let path =
        std::env::temp_dir().join(format!("hrrdarr-cross-numbering-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&path).unwrap();
    let _scratch = Scratch(path.clone());
    let db = Database::open_local(path.join("db")).await.unwrap();
    let c = db.connect().await.unwrap();
    let fixture_indexer_id = fixture_indexer(&c).await;
    c.execute_batch("INSERT INTO series(id,title,path)VALUES(1,'Harbor','/synthetic/harbor');INSERT INTO seasons(series_id,number,monitored)VALUES(1,0,0),(1,1,1);INSERT INTO episodes(id,series_id,season,number,title,runtime,air_date,air_date_utc,absolute_episode_number,monitored)VALUES(1,1,1,1,'Regular',45,'2020-01-01','2020-01-01T00:00:00Z',23,1),(2,1,0,1,'Unmonitored special',NULL,'2020-01-01',NULL,NULL,0);INSERT INTO quality_profiles VALUES(1,'tv','HD');INSERT INTO quality_profile_items(profile_id,media_type,quality_id,position,allowed)VALUES(1,'tv',3,0,1);INSERT INTO quality_profile_policies(profile_id,media_type,upgrade_allowed,cutoff_quality_id,min_format_score,cutoff_format_score,min_upgrade_format_score,language_id)VALUES(1,'tv',1,3,0,0,1,NULL);INSERT INTO library_settings(media_type,series_id,quality_profile_id,series_type,use_scene_numbering)VALUES('tv',1,1,'daily',0);INSERT INTO release_delay_policies VALUES('tv',0,0,0);").await.unwrap();
    let daily = release("Harbor.2020.01.01.1080p.WEB-DL", true);
    let absolute = release("Harbor - 023 1080p WEB-DL", true);
    for kind in ["daily", "anime", "standard"] {
        c.execute(
            "UPDATE library_settings SET series_type=? WHERE series_id=1",
            [kind],
        )
        .await
        .unwrap();
        for context in [SearchContext::Rss, SearchContext::UserSearch] {
            let result = search::evaluate(
                &c,
                MediaDomain::Tv,
                fixture_indexer_id,
                &daily,
                context,
                1800000000,
            )
            .await
            .unwrap();
            // The excluded special's monitoring/date/runtime must not reject the regular airing.
            assert_eq!(
                result.disposition == Disposition::Accept,
                kind != "standard",
                "{kind}: {:?}",
                result.reasons
            );
            if kind != "standard" {
                assert_eq!(
                    result.target,
                    Some(ReleaseTarget::Tv {
                        series_id: 1,
                        episode_ids: vec![1]
                    })
                );
            }
            let result = search::evaluate(
                &c,
                MediaDomain::Tv,
                fixture_indexer_id,
                &absolute,
                context,
                1800000000,
            )
            .await
            .unwrap();
            assert_eq!(
                result.disposition,
                Disposition::Accept,
                "{kind}: {:?}",
                result.reasons
            );
            assert_eq!(
                result.target,
                Some(ReleaseTarget::Tv {
                    series_id: 1,
                    episode_ids: vec![1]
                })
            );
        }
    }
    c.execute_batch("UPDATE library_settings SET series_type='anime' WHERE series_id=1;INSERT INTO episodes(id,series_id,season,number,title,runtime,air_date,air_date_utc,absolute_episode_number)VALUES(3,1,1,2,'Duplicate',45,'2020-01-01','2020-01-01T00:00:00Z',23);").await.unwrap();
    for item in [&daily, &absolute] {
        assert_ne!(
            search::evaluate(
                &c,
                MediaDomain::Tv,
                fixture_indexer_id,
                item,
                SearchContext::UserSearch,
                1800000000
            )
            .await
            .unwrap()
            .disposition,
            Disposition::Accept
        );
    }
    c.execute_batch(
        "UPDATE episodes SET air_date=NULL,absolute_episode_number=NULL WHERE id IN(1,3);",
    )
    .await
    .unwrap();
    assert_ne!(
        search::evaluate(
            &c,
            MediaDomain::Tv,
            fixture_indexer_id,
            &absolute,
            SearchContext::UserSearch,
            1800000000
        )
        .await
        .unwrap()
        .disposition,
        Disposition::Accept
    );
    // A sole special is an exact date match; unlike the former regular/special pair,
    // its own monitoring/runtime/airing state must now satisfy the ordinary policy checks.
    c.execute_batch("UPDATE seasons SET monitored=1 WHERE number=0;UPDATE episodes SET monitored=1,runtime=45,air_date_utc='2020-01-01T00:00:00Z' WHERE id=2;").await.unwrap();
    let special = search::evaluate(
        &c,
        MediaDomain::Tv,
        fixture_indexer_id,
        &daily,
        SearchContext::Rss,
        1800000000,
    )
    .await
    .unwrap();
    assert_eq!(
        special.disposition,
        Disposition::Accept,
        "{:?}",
        special.reasons
    );
    assert_eq!(
        special.target,
        Some(ReleaseTarget::Tv {
            series_id: 1,
            episode_ids: vec![2]
        })
    );
    c.execute_batch("INSERT INTO episodes(series_id,season,number,title,runtime,air_date)VALUES(1,0,2,'Another special',45,'2020-01-01');").await.unwrap();
    assert_ne!(
        search::evaluate(
            &c,
            MediaDomain::Tv,
            fixture_indexer_id,
            &daily,
            SearchContext::UserSearch,
            1800000000
        )
        .await
        .unwrap()
        .disposition,
        Disposition::Accept
    );
}

#[tokio::test]
async fn factual_revisions_apply_domain_policy_to_every_current_file() {
    let scratch = Scratch(std::env::temp_dir().join(format!(
        "hrrdarr-revision-decisions-{}",
        uuid::Uuid::new_v4()
    )));
    std::fs::create_dir_all(&scratch.0).unwrap();
    let db = Database::open_local(scratch.0.join("db")).await.unwrap();
    let c = db.connect().await.unwrap();
    let fixture_indexer_id = fixture_indexer(&c).await;
    c.execute_batch("INSERT INTO series(id,title,path)VALUES(1,'Harbor','/fictional-tv'); INSERT INTO seasons(series_id,number)VALUES(1,1); INSERT INTO episode_files(id,series_id,path)VALUES(1,1,'/fictional-tv/old.mkv'); INSERT INTO episodes(id,series_id,season,number,title,runtime,air_date_utc,episode_file_id)VALUES(1,1,1,1,'Pilot',45,'2020-01-01 00:00:00',1); INSERT INTO movie_metadata(id,title,year,runtime,digital_release)VALUES(1,'Harbor',2020,100,'2020-01-01 00:00:00'); INSERT INTO movies(id,metadata_id,path)VALUES(1,1,'/fictional-movie'); INSERT INTO movie_files(id,movie_id,path)VALUES(1,1,'/fictional-movie/old.mkv'); INSERT INTO file_metadata(media_type,episode_file_id,quality_id,revision_json,release_group,date_added)VALUES('tv',1,3,'{\"version\":1,\"real\":0,\"is_repack\":false}','TEAM','2026-09-24T00:00:00Z'); INSERT INTO file_metadata(media_type,movie_file_id,quality_id,revision_json,release_group,date_added)VALUES('movies',1,3,'{\"version\":1,\"real\":0,\"is_repack\":false}','TEAM','2026-09-24T00:00:00Z'); INSERT INTO quality_profiles VALUES(1,'tv','HD'),(2,'movies','HD'); INSERT INTO quality_profile_items(profile_id,media_type,quality_id,position,allowed)VALUES(1,'tv',3,0,1),(2,'movies',3,0,1); INSERT INTO quality_profile_policies(profile_id,media_type,upgrade_allowed,cutoff_quality_id,min_format_score,cutoff_format_score,min_upgrade_format_score,language_id)VALUES(1,'tv',1,3,0,100,1,NULL),(2,'movies',1,3,0,100,1,-1); INSERT INTO library_settings(media_type,series_id,quality_profile_id,series_type,use_scene_numbering)VALUES('tv',1,1,'standard',0); INSERT INTO library_settings(media_type,movie_id,quality_profile_id,minimum_availability)VALUES('movies',1,2,'released'); INSERT INTO release_delay_policies VALUES('tv',0,0,0),('movies',0,0,0); UPDATE quality_definitions SET min_size=0,max_size=NULL WHERE quality_id=3;").await.unwrap();
    let now = chrono::DateTime::parse_from_rfc3339("2026-09-24T12:00:00Z")
        .unwrap()
        .timestamp();
    for (media, tv) in [(MediaDomain::Tv, true), (MediaDomain::Movies, false)] {
        let domain = if tv { "tv" } else { "movies" };
        let stem = if tv {
            "Harbor.S01E01.1080p.WEB-DL"
        } else {
            "Harbor.2020.1080p.WEB-DL"
        };
        for mode in ["prefer_and_upgrade", "do_not_upgrade", "do_not_prefer"] {
            c.execute("UPDATE revision_policies SET mode=?,revision=revision+1,locally_edited=1 WHERE media_type=?",libsql::params![mode,domain]).await.unwrap();
            for context in [SearchContext::UserSearch, SearchContext::Rss] {
                let decision = search::evaluate(
                    &c,
                    media,
                    fixture_indexer_id,
                    &release(&format!("{stem}.PROPER-team"), tv),
                    context,
                    now,
                )
                .await
                .unwrap();
                let accepted = mode != "do_not_prefer"
                    && (mode != "do_not_upgrade" || context == SearchContext::UserSearch);
                assert_eq!(
                    decision.disposition == Disposition::Accept,
                    accepted,
                    "{domain}/{mode}/{context:?}: {:?}",
                    decision.reasons
                );
                if context == SearchContext::Rss && mode == "do_not_upgrade" {
                    assert!(
                        decision
                            .reasons
                            .contains(&"revision_upgrade_disabled".into())
                    );
                }
            }
            let repack = search::evaluate(
                &c,
                media,
                fixture_indexer_id,
                &release(&format!("{stem}.RERIP2-team"), tv),
                SearchContext::UserSearch,
                now,
            )
            .await
            .unwrap();
            if mode == "prefer_and_upgrade" {
                assert_eq!(
                    repack.disposition,
                    Disposition::Accept,
                    "{:?}",
                    repack.reasons
                );
            }
            if mode == "do_not_upgrade" {
                assert!(repack.reasons.contains(&"repack_upgrade_disabled".into()));
            }
        }
        c.execute("UPDATE revision_policies SET mode='prefer_and_upgrade',revision=revision+1 WHERE media_type=?",[domain]).await.unwrap();
        c.execute(
            "UPDATE file_metadata SET date_added='2020-01-01T00:00:00Z' WHERE media_type=?",
            [domain],
        )
        .await
        .unwrap();
        let old = search::evaluate(
            &c,
            media,
            fixture_indexer_id,
            &release(&format!("{stem}.PROPER-team"), tv),
            SearchContext::Rss,
            now,
        )
        .await
        .unwrap();
        assert!(old.reasons.contains(&"revision_too_old".into()));
        let searched = search::evaluate(
            &c,
            media,
            fixture_indexer_id,
            &release(&format!("{stem}.PROPER-team"), tv),
            SearchContext::UserSearch,
            now,
        )
        .await
        .unwrap();
        assert_eq!(searched.disposition, Disposition::Accept);
        c.execute(
            "UPDATE file_metadata SET release_group=NULL WHERE media_type=?",
            [domain],
        )
        .await
        .unwrap();
        let unknown_group = search::evaluate(
            &c,
            media,
            fixture_indexer_id,
            &release(&format!("{stem}.REPACK-team"), tv),
            SearchContext::UserSearch,
            now,
        )
        .await
        .unwrap();
        assert!(
            unknown_group
                .reasons
                .contains(&"repack_group_mismatch".into())
        );
        c.execute(
            "UPDATE file_metadata SET release_group='TEAM',revision_json=NULL WHERE media_type=?",
            [domain],
        )
        .await
        .unwrap();
        let unknown = search::evaluate(
            &c,
            media,
            fixture_indexer_id,
            &release(&format!("{stem}.PROPER-team"), tv),
            SearchContext::UserSearch,
            now,
        )
        .await
        .unwrap();
        assert!(unknown.reasons.contains(&"revision_unknown".into()));
        c.execute("UPDATE file_metadata SET revision_json='{\"version\":1,\"real\":0,\"is_repack\":false}' WHERE media_type=?",[domain]).await.unwrap();
    }
    c.execute(
        "UPDATE library_settings SET series_type='anime' WHERE series_id=1",
        (),
    )
    .await
    .unwrap();
    let anime = search::evaluate(
        &c,
        MediaDomain::Tv,
        fixture_indexer_id,
        &release("Harbor.S01E01.1080p.WEB-DL.PROPER-team", true),
        SearchContext::UserSearch,
        now,
    )
    .await
    .unwrap();
    assert!(
        anime
            .reasons
            .contains(&"anime_revision_group_mismatch".into())
    );
    // Keep WEB-DL a standalone source token: the existing bounded quality grammar
    // does not split WEB-DL-TEAM. This case proves attached v3 and exact-case group policy.
    let anime = search::evaluate(
        &c,
        MediaDomain::Tv,
        fixture_indexer_id,
        &release("Harbor.S01E01v3.1080p.WEB-DL.x264-TEAM", true),
        SearchContext::UserSearch,
        now,
    )
    .await
    .unwrap();
    assert_eq!(
        anime.disposition,
        Disposition::Accept,
        "{:?}",
        anime.reasons
    );
    c.execute_batch("UPDATE library_settings SET series_type='standard' WHERE series_id=1; INSERT INTO episode_files(id,series_id,path)VALUES(2,1,'/fictional-tv/second.mkv'); INSERT INTO episodes(id,series_id,season,number,title,runtime,air_date_utc,episode_file_id)VALUES(2,1,1,2,'Second',45,'2020-01-01 00:00:00',2); INSERT INTO file_metadata(media_type,episode_file_id,quality_id,revision_json,release_group,date_added)VALUES('tv',2,3,'{\"version\":3,\"real\":0,\"is_repack\":false}','TEAM','2026-09-24T00:00:00Z');").await.unwrap();
    let multi = search::evaluate(
        &c,
        MediaDomain::Tv,
        fixture_indexer_id,
        &release("Harbor.S01E01E02.1080p.WEB-DL.PROPER-TEAM", true),
        SearchContext::UserSearch,
        now,
    )
    .await
    .unwrap();
    assert!(
        multi.reasons.contains(&"not_revision_upgrade".into()),
        "{:?}",
        multi.reasons
    );
    assert_eq!(
        multi.target,
        Some(ReleaseTarget::Tv {
            series_id: 1,
            episode_ids: vec![1, 2]
        })
    );
}

async fn selected_delay(base: &str, domain: &str, settings: serde_json::Value) {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
    let url = format!("{base}/api/v1/{domain}/delay-profiles");
    let old: serde_json::Value = serde_json::from_slice(
        &client
            .get(&url)
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap(),
    )
    .unwrap();
    let global = old["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["is_global"] == true)
        .unwrap();
    let response = client.put(format!("{url}/{}",global["id"]))
        .header("content-type","application/json")
        .body(serde_json::json!({"revision":old["revision"],"profile":{"settings":settings,"tag_ids":[]}}).to_string())
        .send().await.unwrap();
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
}
#[tokio::test]
async fn selected_full_delay_uses_domain_revision_and_highest_allowed_quality() {
    use serde_json::json;
    let scratch = Scratch(std::env::temp_dir().join(format!(
        "hrrdarr-full-delay-decisions-{}",
        uuid::Uuid::new_v4()
    )));
    std::fs::create_dir_all(&scratch.0).unwrap();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let c = db.connect().await.unwrap();
    let fixture_indexer_id = fixture_indexer(&c).await;
    c.execute_batch("INSERT INTO series(id,title,path)VALUES(1,'Harbor','/fictional-tv'); INSERT INTO seasons(series_id,number)VALUES(1,1); INSERT INTO episode_files(id,series_id,path)VALUES(1,1,'/fictional-tv/old.mkv'); INSERT INTO episodes(id,series_id,season,number,title,runtime,air_date_utc,episode_file_id)VALUES(1,1,1,1,'Pilot',45,'2020-01-01 00:00:00',1); INSERT INTO movie_metadata(id,title,year,runtime,digital_release)VALUES(1,'Harbor',2020,100,'2020-01-01 00:00:00'); INSERT INTO movies(id,metadata_id,path)VALUES(1,1,'/fictional-movie'); INSERT INTO movie_files(id,movie_id,path)VALUES(1,1,'/fictional-movie/old.mkv'); INSERT INTO file_metadata(media_type,episode_file_id,quality_id,revision_json,release_group,date_added)VALUES('tv',1,3,'{\"version\":1,\"real\":0,\"is_repack\":false}','TEAM','2026-09-24T00:00:00Z'); INSERT INTO file_metadata(media_type,movie_file_id,quality_id,revision_json,release_group,date_added)VALUES('movies',1,3,'{\"version\":1,\"real\":0,\"is_repack\":false}','TEAM','2026-09-24T00:00:00Z'); INSERT INTO quality_profiles VALUES(1,'tv','HD'),(2,'movies','HD'); INSERT INTO quality_profile_items(profile_id,media_type,quality_id,position,allowed)VALUES(1,'tv',3,0,1),(2,'movies',3,0,1); INSERT INTO quality_profile_policies(profile_id,media_type,upgrade_allowed,cutoff_quality_id,min_format_score,cutoff_format_score,min_upgrade_format_score,language_id)VALUES(1,'tv',1,3,0,100,1,NULL),(2,'movies',1,3,0,100,1,-1); INSERT INTO library_settings(media_type,series_id,quality_profile_id,series_type,use_scene_numbering)VALUES('tv',1,1,'standard',0); INSERT INTO library_settings(media_type,movie_id,quality_profile_id,minimum_availability)VALUES('movies',1,2,'released'); INSERT INTO release_delay_policies VALUES('tv',0,0,0),('movies',0,0,0); UPDATE quality_definitions SET min_size=0,max_size=NULL WHERE quality_id=3;").await.unwrap();
    for (id, domain) in [(1, "tv"), (2, "movies")] {
        let specs = json!([{"name":"Bonus","required":false,"negate":false,"condition":{"kind":"release_title","pattern":"BONUS"}}]);
        c.execute("INSERT INTO custom_formats(id,media_type,name,include_when_renaming,specifications_json)VALUES(?,?,'Bonus',0,?)",libsql::params![id,domain,specs.to_string()]).await.unwrap();
        c.execute(
            "INSERT INTO quality_profile_format_scores VALUES(?,?,?,10)",
            libsql::params![id, id, domain],
        )
        .await
        .unwrap();
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = hrrdarr::delay_profiles::router(db.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let now = chrono::DateTime::parse_from_rfc3339("2026-09-24T12:00:00Z")
        .unwrap()
        .timestamp();
    let settings = json!({"torrent_delay_minutes":60,"usenet_delay_minutes":60,"enable_torrent":true,"enable_usenet":true,"preferred_protocol":"torrent","bypass_if_highest_quality":false,"bypass_if_above_custom_format_score":false,"minimum_custom_format_score":10});
    for (media, tv, domain) in [
        (MediaDomain::Tv, true, "tv"),
        (MediaDomain::Movies, false, "movies"),
    ] {
        let stem = if tv { "Harbor.S01E01" } else { "Harbor.2020" };
        let proper = release(&format!("{stem}.1080p.WEB-DL.PROPER.BONUS-team"), tv);
        selected_delay(&base, domain, settings.clone()).await;
        let result = search::evaluate(
            &c,
            media,
            fixture_indexer_id,
            &proper,
            SearchContext::Rss,
            now,
        )
        .await
        .unwrap();
        assert_eq!(
            result.disposition,
            Disposition::Accept,
            "{domain}: {:?}",
            result.reasons
        );
        let mut nonpreferred = settings.clone();
        nonpreferred["preferred_protocol"] = json!("usenet");
        selected_delay(&base, domain, nonpreferred).await;
        assert_eq!(
            search::evaluate(
                &c,
                media,
                fixture_indexer_id,
                &proper,
                SearchContext::Rss,
                now
            )
            .await
            .unwrap()
            .disposition,
            Disposition::Delay
        );
        selected_delay(&base, domain, settings.clone()).await;
        c.execute("UPDATE revision_policies SET mode='do_not_prefer',revision=revision+1,locally_edited=1 WHERE media_type=?",[domain]).await.unwrap();
        // Both offers are legitimate CF upgrades. Only movie delay bypass lacks
        // TV's PreferAndUpgrade mode condition on the exact revision predicate.
        let result = search::evaluate(
            &c,
            media,
            fixture_indexer_id,
            &proper,
            SearchContext::Rss,
            now,
        )
        .await
        .unwrap();
        assert_eq!(
            result.disposition,
            if tv {
                Disposition::Delay
            } else {
                Disposition::Accept
            },
            "{domain}: {:?}",
            result.reasons
        );
        let mut disabled = settings.clone();
        disabled["enable_torrent"] = json!(false);
        disabled["preferred_protocol"] = json!("usenet");
        selected_delay(&base, domain, disabled).await;
        for context in [SearchContext::Rss, SearchContext::UserSearch] {
            let result = search::evaluate(&c, media, fixture_indexer_id, &proper, context, now)
                .await
                .unwrap();
            assert_eq!(result.disposition, Disposition::Reject);
            assert!(
                result
                    .reasons
                    .iter()
                    .any(|reason| reason == "protocol_disabled")
            );
        }
    }
    // File metadata deliberately restricts file deletion; remove its scratch facts
    // before resetting the fixture to missing files for highest-quality admission.
    c.execute_batch("UPDATE episodes SET episode_file_id=NULL;DELETE FROM file_metadata;DELETE FROM episode_files;DELETE FROM movie_files;UPDATE quality_profile_items SET position=1;INSERT INTO quality_profile_items(profile_id,media_type,quality_id,position,allowed)VALUES(1,'tv',5,0,1),(2,'movies',5,0,1);UPDATE quality_profile_policies SET cutoff_quality_id=5;UPDATE quality_definitions SET min_size=0,max_size=NULL WHERE quality_id=5;").await.unwrap();
    for (media, tv, domain) in [
        (MediaDomain::Tv, true, "tv"),
        (MediaDomain::Movies, false, "movies"),
    ] {
        let stem = if tv { "Harbor.S01E01" } else { "Harbor.2020" };
        let high = release(&format!("{stem}.1080p.WEB-DL"), tv);
        let low = release(&format!("{stem}.720p.WEB-DL"), tv);
        let mut highest = settings.clone();
        highest["bypass_if_highest_quality"] = json!(true);
        selected_delay(&base, domain, highest).await;
        assert_eq!(
            search::evaluate(
                &c,
                media,
                fixture_indexer_id,
                &high,
                SearchContext::Rss,
                now
            )
            .await
            .unwrap()
            .disposition,
            Disposition::Accept
        );
        assert_eq!(
            search::evaluate(&c, media, fixture_indexer_id, &low, SearchContext::Rss, now)
                .await
                .unwrap()
                .disposition,
            Disposition::Delay,
            "The configured cutoff is not the highest allowed quality"
        );
        c.execute(
            "UPDATE quality_profile_items SET allowed=0 WHERE media_type=? AND quality_id=3",
            [domain],
        )
        .await
        .unwrap();
        assert_eq!(
            search::evaluate(&c, media, fixture_indexer_id, &low, SearchContext::Rss, now)
                .await
                .unwrap()
                .disposition,
            Disposition::Accept,
            "Disabled higher leaves do not prevent highest-allowed bypass"
        );
        c.execute(
            "UPDATE quality_profile_items SET allowed=1 WHERE media_type=? AND quality_id=3",
            [domain],
        )
        .await
        .unwrap();
        let mut score = settings.clone();
        score["bypass_if_above_custom_format_score"] = json!(true);
        selected_delay(&base, domain, score).await;
        let bonus = release(&format!("{stem}.1080p.WEB-DL.BONUS"), tv);
        assert_eq!(
            search::evaluate(
                &c,
                media,
                fixture_indexer_id,
                &bonus,
                SearchContext::Rss,
                now
            )
            .await
            .unwrap()
            .disposition,
            Disposition::Accept,
            "Exact configured CF threshold bypasses"
        );
        assert_eq!(
            search::evaluate(
                &c,
                media,
                fixture_indexer_id,
                &high,
                SearchContext::Rss,
                now
            )
            .await
            .unwrap()
            .disposition,
            Disposition::Delay
        );
    }
    server.abort();
    let _ = server.await;
}

async fn fixture_indexer(c: &libsql::Connection) -> uuid::Uuid {
    let id = uuid::Uuid::new_v4();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint)VALUES(?,'torznab','Decision fixture',1,1,1,1,'http://fixture.invalid')", [id.to_string()]).await.unwrap();
    for media in ["tv", "movies"] {
        c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year)VALUES(?,'torznab',?,'[2000,5000]','[]',?,?)", libsql::params![id.to_string(),media,(media=="tv").then_some(0),(media=="movies").then_some(0)]).await.unwrap();
    }
    id
}

async fn restriction_save(
    base: &str,
    media: &str,
    id: Option<i64>,
    profile: serde_json::Value,
) -> i64 {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap();
    let url = format!("{base}/api/v1/{media}/release-profiles");
    let old: serde_json::Value = serde_json::from_slice(
        &client
            .get(&url)
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap(),
    )
    .unwrap();
    let request = if let Some(id) = id {
        client.put(format!("{url}/{id}"))
    } else {
        client.post(url)
    };
    let response = request
        .header("content-type", "application/json")
        .body(serde_json::json!({"revision":old["revision"],"profile":profile}).to_string())
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body: serde_json::Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
    assert_eq!(
        status.as_u16(),
        if id.is_some() { 200 } else { 201 },
        "{body}"
    );
    id.unwrap_or_else(|| {
        body["profiles"].as_array().unwrap().last().unwrap()["id"]
            .as_i64()
            .unwrap()
    })
}
fn restriction_profile(tv: bool) -> serde_json::Value {
    let mut p = serde_json::json!({"name":"Admission","enabled":true,"required":["WEB","absent"],"ignored":[],"tag_ids":[],"indexers":[]});
    if tv {
        p["excluded_tag_ids"] = serde_json::json!([]);
        p["air_date_restriction"] = serde_json::json!(false);
        p["air_date_grace_period_days"] = serde_json::json!(0);
        p["allow_season_pack_without_all_episodes_aired"] = serde_json::json!(false);
    }
    p
}
#[tokio::test]
async fn applicable_release_restrictions_and_precise_tv_time_rules() {
    use serde_json::json;
    let scratch = Scratch(
        std::env::temp_dir().join(format!("hrrdarr-restrictions-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir_all(&scratch.0).unwrap();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let c = db.connect().await.unwrap();
    let indexer = fixture_indexer(&c).await;
    let other_indexer = fixture_indexer(&c).await;
    c.execute_batch("INSERT INTO series(id,title,path)VALUES(1,'Harbor','/fictional-tv'); INSERT INTO seasons(series_id,number)VALUES(1,1); INSERT INTO episodes(id,series_id,season,number,title,runtime,air_date_utc)VALUES(1,1,1,1,'Pilot',45,'2026-09-24T12:00:00Z'); INSERT INTO movie_metadata(id,title,year,runtime,digital_release)VALUES(1,'Harbor',2026,100,'2020-01-01T00:00:00Z'); INSERT INTO movies(id,metadata_id,path)VALUES(1,1,'/fictional-movie'); INSERT INTO quality_profiles VALUES(1,'tv','HD'),(2,'movies','HD'); INSERT INTO quality_profile_items(profile_id,media_type,quality_id,position,allowed)VALUES(1,'tv',3,0,1),(2,'movies',3,0,1); INSERT INTO quality_profile_policies(profile_id,media_type,upgrade_allowed,cutoff_quality_id,min_format_score,cutoff_format_score,min_upgrade_format_score,language_id)VALUES(1,'tv',1,3,0,0,1,NULL),(2,'movies',1,3,0,0,1,-1); INSERT INTO library_settings(media_type,series_id,quality_profile_id,series_type,use_scene_numbering)VALUES('tv',1,1,'standard',0); INSERT INTO library_settings(media_type,movie_id,quality_profile_id,minimum_availability)VALUES('movies',1,2,'released'); INSERT INTO release_delay_policies VALUES('tv',0,0,0),('movies',0,0,0); UPDATE quality_definitions SET min_size=0,max_size=NULL WHERE quality_id=3; INSERT INTO tags(id,media_type,label)VALUES(1,'tv','included'),(2,'movies','included'),(3,'tv','excluded');").await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = hrrdarr::release_profiles::router(db.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let now = chrono::DateTime::parse_from_rfc3339("2026-09-24T12:00:00Z")
        .unwrap()
        .timestamp();

    // Empty catalogs still require the default full-season window; explicit
    // multi-episode numbering does not acquire the full-season-only restriction.
    c.execute("INSERT INTO episodes(id,series_id,season,number,title,runtime,air_date_utc)VALUES(2,1,1,2,'Second',45,'2026-09-25T12:00:00.001Z')",()).await.unwrap();
    let empty_pack = release("Harbor.S01.1080p.WEB-DL", true);
    assert_eq!(
        search::evaluate(
            &c,
            MediaDomain::Tv,
            indexer,
            &empty_pack,
            SearchContext::UserSearch,
            now
        )
        .await
        .unwrap()
        .reasons,
        vec!["season_pack_not_aired"]
    );
    let explicit = release("Harbor.S01E01E02.1080p.WEB-DL", true);
    assert_eq!(
        search::evaluate(
            &c,
            MediaDomain::Tv,
            indexer,
            &explicit,
            SearchContext::UserSearch,
            now
        )
        .await
        .unwrap()
        .disposition,
        Disposition::Accept
    );
    c.execute(
        "UPDATE episodes SET air_date_utc='2026-09-24T12:00:00Z'",
        (),
    )
    .await
    .unwrap();
    for (media, d, tv, tag) in [
        (MediaDomain::Tv, "tv", true, 1),
        (MediaDomain::Movies, "movies", false, 2),
    ] {
        let item = release(
            if tv {
                "Harbor.S01E01.1080p.WEB-DL"
            } else {
                "Harbor.2026.1080p.WEB-DL"
            },
            tv,
        );
        let first = restriction_profile(tv);
        restriction_save(&base, d, None, first).await;
        let mut second = restriction_profile(tv);
        second["required"] = json!(["NEVER"]);
        second["indexers"] = json!([{"kind":"provider","id":indexer}]);
        let id = restriction_save(&base, d, None, second.clone()).await;
        for context in [SearchContext::Rss, SearchContext::UserSearch] {
            let rejected = search::evaluate(&c, media, indexer, &item, context, now)
                .await
                .unwrap();
            assert_eq!(rejected.reasons, vec!["release_required_term_missing"]);
            assert_eq!(
                search::evaluate(&c, media, other_indexer, &item, context, now)
                    .await
                    .unwrap()
                    .disposition,
                Disposition::Accept
            );
        }
        second["required"] = json!(["Harbor"]);
        second["ignored"] = json!(["/WEB/"]);
        restriction_save(&base, d, Some(id), second.clone()).await;
        assert_eq!(
            search::evaluate(&c, media, indexer, &item, SearchContext::UserSearch, now)
                .await
                .unwrap()
                .reasons,
            vec!["release_ignored_term"]
        );
        second["tag_ids"] = json!([tag]);
        restriction_save(&base, d, Some(id), second.clone()).await;
        assert_eq!(
            search::evaluate(&c, media, indexer, &item, SearchContext::UserSearch, now)
                .await
                .unwrap()
                .disposition,
            Disposition::Accept
        );
        let (table, owner) = if tv {
            ("series_tags", "series_id")
        } else {
            ("movie_tags", "movie_id")
        };
        c.execute(
            &format!("INSERT INTO {table}({owner},tag_id)VALUES(1,?)"),
            [tag],
        )
        .await
        .unwrap();
        assert_eq!(
            search::evaluate(&c, media, indexer, &item, SearchContext::UserSearch, now)
                .await
                .unwrap()
                .reasons,
            vec!["release_ignored_term"]
        );
        if tv {
            second["excluded_tag_ids"] = json!([3]);
            restriction_save(&base, d, Some(id), second.clone()).await;
            c.execute("INSERT INTO series_tags(series_id,tag_id)VALUES(1,3)", ())
                .await
                .unwrap();
            assert_eq!(
                search::evaluate(&c, media, indexer, &item, SearchContext::UserSearch, now)
                    .await
                    .unwrap()
                    .disposition,
                Disposition::Accept
            );
        }
        second["enabled"] = json!(false);
        restriction_save(&base, d, Some(id), second).await;
        assert_eq!(
            search::evaluate(&c, media, indexer, &item, SearchContext::UserSearch, now)
                .await
                .unwrap()
                .disposition,
            Disposition::Accept
        );
    }
    // Publication compares exact UTC instants, not local midnight or truncated seconds.
    let mut policy = restriction_profile(true);
    policy["air_date_restriction"] = json!(true);
    let id = restriction_save(&base, "tv", None, policy.clone()).await;
    let mut item = release("Harbor.S01E01.1080p.WEB-DL", true);
    item.metadata.published_at = "2026-09-24T12:00:00Z".into();
    for (date, denied) in [
        ("2026-09-24T11:59:59Z", false),
        ("2026-09-24T12:00:00Z", false),
        ("2026-09-24T12:00:00.500Z", true),
        ("2026-09-24", true),
        ("2026-09-24 12:00:00", true),
    ] {
        c.execute("UPDATE episodes SET air_date_utc=?", [date])
            .await
            .unwrap();
        for context in [SearchContext::Rss, SearchContext::UserSearch] {
            assert_eq!(
                search::evaluate(&c, MediaDomain::Tv, indexer, &item, context, now)
                    .await
                    .unwrap()
                    .reasons,
                if denied {
                    vec!["release_before_air_date"]
                } else {
                    vec![]
                },
                "{date}"
            );
        }
    }
    c.execute(
        "UPDATE episodes SET air_date_utc='2026-09-24T12:00:00.500Z'",
        (),
    )
    .await
    .unwrap();
    item.metadata.published_at = "2026-09-24T12:00:00.500Z".into();
    assert_eq!(
        search::evaluate(&c, MediaDomain::Tv, indexer, &item, SearchContext::Rss, now)
            .await
            .unwrap()
            .disposition,
        Disposition::Accept
    );
    policy["air_date_grace_period_days"] = json!(-1);
    restriction_save(&base, "tv", Some(id), policy.clone()).await;
    c.execute(
        "UPDATE episodes SET air_date_utc='2026-09-25T12:00:00.500Z'",
        (),
    )
    .await
    .unwrap();
    assert_eq!(
        search::evaluate(&c, MediaDomain::Tv, indexer, &item, SearchContext::Rss, now)
            .await
            .unwrap()
            .disposition,
        Disposition::Accept
    );

    for grace in [i32::MIN, i32::MAX] {
        policy["air_date_grace_period_days"] = json!(grace);
        restriction_save(&base, "tv", Some(id), policy.clone()).await;
        assert_eq!(
            search::evaluate(&c, MediaDomain::Tv, indexer, &item, SearchContext::Rss, now)
                .await
                .unwrap_err()
                .0,
            "release_profile_time_overflow"
        );
    }
    policy["air_date_grace_period_days"] = json!(-1);
    restriction_save(&base, "tv", Some(id), policy.clone()).await;
    let mut stricter = policy.clone();
    stricter["air_date_grace_period_days"] = json!(0);
    let stricter_id = restriction_save(&base, "tv", None, stricter.clone()).await;
    assert_eq!(
        search::evaluate(&c, MediaDomain::Tv, indexer, &item, SearchContext::Rss, now)
            .await
            .unwrap()
            .reasons,
        vec!["release_before_air_date"]
    );
    stricter["air_date_restriction"] = json!(false);
    stricter["air_date_grace_period_days"] = json!(i32::MAX);
    restriction_save(&base, "tv", Some(stricter_id), stricter.clone()).await;
    assert_eq!(
        search::evaluate(&c, MediaDomain::Tv, indexer, &item, SearchContext::Rss, now)
            .await
            .unwrap()
            .disposition,
        Disposition::Accept
    );
    policy["air_date_restriction"] = json!(false);
    restriction_save(&base, "tv", Some(id), policy.clone()).await;
    let pack = release("Harbor.S01.1080p.WEB-DL", true);
    for (date, denied) in [
        ("2026-09-25T11:59:59Z", false),
        ("2026-09-25T12:00:00Z", false),
        ("2026-09-25T12:00:00.001Z", true),
        ("2026-09-25", true),
    ] {
        c.execute("UPDATE episodes SET air_date_utc=?", [date])
            .await
            .unwrap();
        assert_eq!(
            search::evaluate(
                &c,
                MediaDomain::Tv,
                indexer,
                &pack,
                SearchContext::UserSearch,
                now
            )
            .await
            .unwrap()
            .reasons,
            if denied {
                vec!["season_pack_not_aired"]
            } else {
                vec![]
            },
            "{date}"
        );
    }
    // Every applicable profile must allow the pack: even an unrelated term-only
    // global profile supplies the default false flag. Update all active profiles.
    let rows = c
        .query(
            "SELECT id FROM release_profiles WHERE media_type='tv' AND enabled=1",
            (),
        )
        .await
        .unwrap();
    let mut rows = rows;
    let mut ids = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        ids.push(row.get::<i64>(0).unwrap());
    }
    drop(rows);
    policy["allow_season_pack_without_all_episodes_aired"] = json!(true);
    for active in ids {
        restriction_save(&base, "tv", Some(active), policy.clone()).await;
    }
    assert_eq!(
        search::evaluate(
            &c,
            MediaDomain::Tv,
            indexer,
            &pack,
            SearchContext::UserSearch,
            now
        )
        .await
        .unwrap()
        .disposition,
        Disposition::Accept
    );
    policy["air_date_restriction"] = json!(true);
    policy["air_date_grace_period_days"] = json!(0);
    restriction_save(&base, "tv", Some(id), policy).await;
    assert_eq!(
        search::evaluate(
            &c,
            MediaDomain::Tv,
            indexer,
            &pack,
            SearchContext::UserSearch,
            now
        )
        .await
        .unwrap()
        .reasons,
        vec!["release_before_air_date"]
    );
    server.abort();
    let _ = server.await;
}
