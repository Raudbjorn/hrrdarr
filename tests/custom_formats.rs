use hrrdarr::{
    api::MediaDomain,
    db::Database,
    providers::indexer,
    search::{self, Disposition, SearchContext},
};
use serde_json::{Value, json};
use std::sync::Arc;
struct Scratch(std::path::PathBuf);
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
async fn request(base: &str, method: &str, path: &str, body: Value) -> (u16, Value) {
    let r = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap()
        .request(method.parse().unwrap(), format!("{base}{path}"))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    let code = r.status().as_u16();
    let text = r.text().await.unwrap();
    (
        code,
        if code == 204 {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap()
        },
    )
}
fn definition(name: &str, pattern: &str) -> Value {
    json!({"name":name,"include_when_renaming":false,"specifications":[{"name":"Title","required":false,"negate":false,"condition":{"kind":"release_title","pattern":pattern}}]})
}
fn release(tv: bool, tag: &str) -> indexer::Release {
    let title = if tv {
        format!("Harbor.S01E01.1080p.WEB-DL.{tag}")
    } else {
        format!("Harbor.2020.1080p.WEB-DL.{tag}")
    };
    let cat = if tv { 5030 } else { 2030 };
    indexer::parse_page(&format!(r#"<rss xmlns:torznab="http://torznab.com/schemas/2015/feed"><channel><item><title>{title}</title><guid>x</guid><pubDate>Mon, 01 Jan 2024 12:00:00 +0000</pubDate><link>https://example.invalid/x</link><torznab:attr name="category" value="{cat}"/><torznab:attr name="size" value="1073741824"/></item></channel></rss>"#),0,100,true,if tv{MediaDomain::Tv}else{MediaDomain::Movies}).unwrap().items.remove(0)
}
#[tokio::test]
async fn native_crud_scores_atomicity_and_current_file_recomputation() {
    let scratch = Scratch(
        std::env::temp_dir().join(format!("hrrdarr-formats-http-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&scratch.0).unwrap();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let c = db.connect().await.unwrap();
    c.execute_batch("INSERT INTO series(id,title,path,original_language)VALUES(1,'Harbor','/tv',1);INSERT INTO seasons(series_id,number)VALUES(1,1);INSERT INTO episodes(id,series_id,season,number,title,runtime,air_date_utc)VALUES(1,1,1,1,'Pilot',45,'2020-01-01 00:00:00');INSERT INTO movie_metadata(id,title,year,runtime,original_language,digital_release)VALUES(1,'Harbor',2020,100,1,'2020-01-01 00:00:00');INSERT INTO movies(id,metadata_id,path)VALUES(1,1,'/movies');INSERT INTO release_delay_policies VALUES('tv',0,0,0),('movies',0,0,0);").await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = hrrdarr::custom_formats::router(db.clone())
        .merge(hrrdarr::quality_profiles::router(db.clone()));
    let _server = Server(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap()
    }));
    for (media, tv) in [("tv", true), ("movies", false)] {
        let prefix = format!("/api/v1/{media}/custom-formats");
        let (code, schema) = request(&base, "GET", &format!("{prefix}/schema"), Value::Null).await;
        assert_eq!(code, 200);
        assert_eq!(
            schema["conditions"].as_array().unwrap().len(),
            if tv { 8 } else { 10 }
        );
        let (code, format) =
            request(&base, "POST", &prefix, definition("Preferred", "Preferred")).await;
        assert_eq!(code, 201, "{format}");
        let fid = format["id"].as_i64().unwrap();
        assert_eq!(
            request(&base, "POST", &prefix, definition("Broken", "("))
                .await
                .0,
            400
        );
        assert_eq!(
            request(&base, "POST", &prefix, definition("Preferred", "x"))
                .await
                .0,
            409
        );
        let profile = json!({"name":"HD","items":[{"kind":"quality","quality_id":3,"allowed":true}],"policy":{"upgrade_allowed":true,"cutoff":{"kind":"quality","quality_id":3},"min_format_score":0,"cutoff_format_score":100,"min_upgrade_format_score":10,"language_id":if tv{Value::Null}else{json!(-1)},"format_items":[{"format_id":fid,"score":10}]}});
        let (code, profile) = request(
            &base,
            "POST",
            &format!("/api/v1/{media}/quality-profiles"),
            profile,
        )
        .await;
        assert_eq!(code, 201, "{profile}");
        let pid = profile["id"].as_i64().unwrap();
        if tv {
            c.execute("INSERT INTO library_settings(media_type,series_id,quality_profile_id,series_type,use_scene_numbering)VALUES('tv',1,?,'standard',0)",[pid]).await.unwrap();
            c.execute_batch("INSERT INTO episode_files(id,series_id,path)VALUES(1,1,'/tv/renamed.mkv');UPDATE episodes SET episode_file_id=1 WHERE id=1;INSERT INTO file_metadata(media_type,episode_file_id,quality_id,original_release_title)VALUES('tv',1,3,'Harbor.S01E01.1080p.WEB-DL.Old');").await.unwrap();
        } else {
            c.execute("INSERT INTO library_settings(media_type,movie_id,quality_profile_id,minimum_availability)VALUES('movies',1,?,'released')",[pid]).await.unwrap();
            c.execute_batch("INSERT INTO movie_files(id,movie_id,path)VALUES(1,1,'/movies/renamed.mkv');INSERT INTO file_metadata(media_type,movie_file_id,quality_id,original_release_title)VALUES('movies',1,3,'Harbor.2020.1080p.WEB-DL.Old');").await.unwrap();
        }
        let domain = if tv {
            MediaDomain::Tv
        } else {
            MediaDomain::Movies
        };
        let candidate = release(tv, "Preferred");
        for context in [SearchContext::UserSearch, SearchContext::Rss] {
            let d = search::evaluate(&c, domain, &candidate, context, 1800000000)
                .await
                .unwrap();
            assert_eq!(d.disposition, Disposition::Accept, "{:?}", d.reasons);
            assert_eq!(d.custom_formats.unwrap().score, 10);
        }
        // Equality at the upgrade increment passes; a higher increment rejects without losing old files.
        c.execute(
            "UPDATE quality_profile_policies SET min_upgrade_format_score=11 WHERE profile_id=?",
            [pid],
        )
        .await
        .unwrap();
        let d = search::evaluate(
            &c,
            domain,
            &candidate,
            SearchContext::UserSearch,
            1800000000,
        )
        .await
        .unwrap();
        assert!(
            d.reasons
                .contains(&"custom_format_upgrade_increment".into())
        );
        c.execute("UPDATE quality_profile_policies SET min_upgrade_format_score=10,min_format_score=11 WHERE profile_id=?",[pid]).await.unwrap();
        let d = search::evaluate(
            &c,
            domain,
            &candidate,
            SearchContext::UserSearch,
            1800000000,
        )
        .await
        .unwrap();
        assert!(d.reasons.contains(&"custom_format_minimum_score".into()));
        c.execute(
            "UPDATE quality_profile_policies SET min_format_score=10 WHERE profile_id=?",
            [pid],
        )
        .await
        .unwrap();
        assert_eq!(
            search::evaluate(
                &c,
                domain,
                &candidate,
                SearchContext::UserSearch,
                1800000000
            )
            .await
            .unwrap()
            .disposition,
            Disposition::Accept
        );
        // Cutoff equality prevents a same-quality upgrade even with a better score.
        c.execute(
            "UPDATE quality_profile_policies SET cutoff_format_score=0 WHERE profile_id=?",
            [pid],
        )
        .await
        .unwrap();
        assert!(
            search::evaluate(
                &c,
                domain,
                &candidate,
                SearchContext::UserSearch,
                1800000000
            )
            .await
            .unwrap()
            .reasons
            .contains(&"cutoff_met".into())
        );
        c.execute(
            "UPDATE quality_profile_policies SET cutoff_format_score=100 WHERE profile_id=?",
            [pid],
        )
        .await
        .unwrap();
        // Re-evaluate old ORIGINAL title with current definitions, despite a renamed filename.
        let (code, _) = request(
            &base,
            "PUT",
            &format!("{prefix}/{fid}"),
            definition("Preferred", "Preferred|Old"),
        )
        .await;
        assert_eq!(code, 200);
        let d = search::evaluate(
            &c,
            domain,
            &candidate,
            SearchContext::UserSearch,
            1800000000,
        )
        .await
        .unwrap();
        assert!(d.reasons.contains(&"custom_format_not_upgrade".into()));
        // Negative scores/minima remain signed, and equality at the minimum is allowed.
        c.execute(
            "UPDATE quality_profile_format_scores SET score=-5 WHERE profile_id=?",
            [pid],
        )
        .await
        .unwrap();
        for (minimum, reject) in [(-5, false), (-4, true)] {
            c.execute(
                "UPDATE quality_profile_policies SET min_format_score=? WHERE profile_id=?",
                libsql::params![minimum, pid],
            )
            .await
            .unwrap();
            let decision = search::evaluate(
                &c,
                domain,
                &candidate,
                SearchContext::UserSearch,
                1800000000,
            )
            .await
            .unwrap();
            assert_eq!(decision.custom_formats.unwrap().score, -5);
            assert_eq!(
                decision
                    .reasons
                    .contains(&"custom_format_minimum_score".into()),
                reject
            );
        }
        let before = request(&base, "GET", &format!("{prefix}/{fid}"), Value::Null)
            .await
            .1;
        assert_eq!(
            request(
                &base,
                "PUT",
                &format!("{prefix}/bulk"),
                json!({"ids":[fid,999999],"include_when_renaming":true})
            )
            .await
            .0,
            404
        );
        assert_eq!(
            request(&base, "GET", &format!("{prefix}/{fid}"), Value::Null)
                .await
                .1,
            before
        );
        assert_eq!(
            request(
                &base,
                "PUT",
                &format!("{prefix}/bulk"),
                json!({"ids":[fid],"include_when_renaming":true})
            )
            .await
            .0,
            200
        );
        assert_eq!(
            request(&base, "DELETE", &format!("{prefix}/{fid}"), Value::Null)
                .await
                .0,
            204
        );
        assert_eq!(
            c.query(
                "SELECT count(*) FROM quality_profile_format_scores WHERE profile_id=?",
                [pid]
            )
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
