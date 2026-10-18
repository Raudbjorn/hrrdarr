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
    let fixture_indexer_id = fixture_indexer(&c).await;
    c.execute_batch("INSERT INTO series(id,title,path,original_language)VALUES(1,'Harbor','/tv',1);INSERT INTO seasons(series_id,number)VALUES(1,1);INSERT INTO episodes(id,series_id,season,number,title,runtime,air_date_utc)VALUES(1,1,1,1,'Pilot',45,'2020-01-01 00:00:00');INSERT INTO movie_metadata(id,title,year,runtime,original_language,digital_release)VALUES(1,'Harbor',2020,100,1,'2020-01-01 00:00:00');INSERT INTO movies(id,metadata_id,path)VALUES(1,1,'/movies');INSERT INTO release_delay_policies VALUES('tv',0,0,0),('movies',0,0,0);").await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = hrrdarr::custom_formats::router(db.clone())
        .merge(hrrdarr::quality_profiles::router(db.clone()))
        .merge(hrrdarr::revision_policy::router(db.clone()));
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
        // These historical file rows genuinely lack revision facts. Default
        // revision preference must not invent baseline facts from their old titles.
        // Explicit DoNotPrefer then isolates the original CF-only upgrade checks.
        for context in [SearchContext::UserSearch, SearchContext::Rss] {
            let decision = search::evaluate(
                &c,
                domain,
                fixture_indexer_id,
                &candidate,
                context,
                1800000000,
            )
            .await
            .unwrap();
            assert_eq!(decision.disposition, Disposition::Reject);
            assert_eq!(decision.reasons, vec!["revision_unknown"]);
        }
        let policy_path = format!("/api/v1/{media}/revision-policy");
        let (code, policy) = request(&base, "GET", &policy_path, Value::Null).await;
        assert_eq!(code, 200);
        let (code, result) = request(
            &base,
            "PUT",
            &policy_path,
            json!({"mode":"do_not_prefer","revision":policy["revision"]}),
        )
        .await;
        assert_eq!(code, 200, "{result}");
        let unchanged: i64 = c
            .query(
                "SELECT count(*) FROM file_metadata WHERE media_type=? AND revision_json IS NULL",
                [media],
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        assert_eq!(unchanged, 1);
        for context in [SearchContext::UserSearch, SearchContext::Rss] {
            let d = search::evaluate(
                &c,
                domain,
                fixture_indexer_id,
                &candidate,
                context,
                1800000000,
            )
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
            fixture_indexer_id,
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
            fixture_indexer_id,
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
                fixture_indexer_id,
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
                fixture_indexer_id,
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
            fixture_indexer_id,
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
                fixture_indexer_id,
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

#[tokio::test]
async fn schema_presets_are_valid_scoped_copies_with_independent_patterns() {
    let scratch = Scratch(
        std::env::temp_dir().join(format!("hrrdarr-format-presets-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&scratch.0).unwrap();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let _server = Server(tokio::spawn(async move {
        axum::serve(listener, hrrdarr::custom_formats::router(db))
            .await
            .unwrap()
    }));
    // Independently chosen release tokens and counterexamples, not upstream expression fixtures.
    let cases = [
        ("x264", "Film.H.264-GRP", "Film.x263-GRP"),
        ("x265", "Film.HEVC-GRP", "Film.AVC-GRP"),
        ("Simple Hardcoded Subs", "Film.Subs.1080p", "Film.HDR.1080p"),
        ("Hardcoded Subs", "Film.ENGsub.1080p", "Film.Subs.1080p"),
        ("Surround Sound", "Film.DDP5.1-GRP", "Film.AAC2.0-GRP"),
        (
            "Preferred Words",
            "Film.Framestor-GRP",
            "Film.Unpreferred-GRP",
        ),
    ];
    for media in ["tv", "movies"] {
        let prefix = format!("/api/v1/{media}/custom-formats");
        let (code, schema) = request(&base, "GET", &format!("{prefix}/schema"), Value::Null).await;
        assert_eq!(code, 200);
        let presets = schema["presets"]["release_title"].as_array().unwrap();
        assert_eq!(presets.len(), 6);
        for (preset, (name, yes, no)) in presets.iter().zip(cases) {
            assert_eq!(preset["label"], name);
            let pattern = preset["specification"]["condition"]["pattern"]
                .as_str()
                .unwrap();
            let re = fancy_regex::Regex::new(&format!("(?i){pattern}")).unwrap();
            assert!(re.is_match(yes).unwrap(), "{name}: {yes}");
            assert!(!re.is_match(no).unwrap(), "{name}: {no}");
            let (positive, negative): (&[&str], &[&str]) = match name {
                "x264" => (
                    &["ax264z", "h264", "X.264", "x2640"],
                    &["x 264", "x-264", "x263"],
                ),
                "x265" => (
                    &["h.265", "x265suffix", "WHEVC", "H265"],
                    &["h 265", "h-265", "h264"],
                ),
                "Simple Hardcoded Subs" => {
                    (&["SUB", "subs", "submarine", "ENGsubs"], &["HC", "caption"])
                }
                "Hardcoded Subs" => (
                    &["ENGsub", "FRENCHSUBS", "HC", "SUBBED", "prefixHC"],
                    &["SUB", "SUBS", "HCA", "SUBBEDextra"],
                ),
                "Surround Sound" => (
                    &[
                        "ATMOS", "TRUEHD", "DTS-HD", "DTSES", "DTS-X7", "DTSX", "DD+5.1",
                        "DDP 7.1", "EAC3.9",
                    ],
                    &["DTS", "DTS-Xfoo", "AAC5.1", "AC3.5.1", "DD5.1", "DDP2.0"],
                ),
                "Preferred Words" => (
                    &["SPARKS", "Film-Framestor", "[sparks]"],
                    &["Framestory", "XSparks", "preferred", "favorite"],
                ),
                _ => unreachable!(),
            };
            for text in positive {
                assert!(re.is_match(text).unwrap(), "{name}: {text}");
            }
            for text in negative {
                assert!(!re.is_match(text).unwrap(), "{name}: {text}");
            }
            let input = json!({"name":name,"include_when_renaming":false,"specifications":[preset["specification"].clone()]});
            assert_eq!(request(&base, "POST", &prefix, input).await.0, 201);
        }
        for (pattern, status) in [
            (r"(?<=Harbor\.)WEB", 201),
            (r"\b([a-z]+)-\1\b", 201),
            ("(", 400),
            ("(?0)", 400),
            ("((?=a)){1000000000}", 400),
        ] {
            assert_eq!(
                request(&base, "POST", &prefix, definition(pattern, pattern))
                    .await
                    .0,
                status,
                "{media}: {pattern}"
            );
        }
        let name = "界".repeat(100);
        let mut input = definition(&name, "original");
        input["specifications"][0]["name"] = json!(name);
        input["specifications"][0]["required"] = json!(true);
        input["specifications"][0]["negate"] = json!(true);
        input["specifications"].as_array_mut().unwrap().push(json!({"name":"Group","negate":false,"required":true,"condition":{"kind":"release_group","pattern":"group"}}));
        let (code, original) = request(&base, "POST", &prefix, input.clone()).await;
        assert_eq!(code, 201);
        let (_, schema) = request(&base, "GET", &format!("{prefix}/schema"), Value::Null).await;
        let saved = schema["presets"]["release_title"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["label"].as_str().unwrap().chars().count() == 202)
            .unwrap();
        assert_eq!(saved["specification"], input["specifications"][0]);
        assert!(
            schema["presets"]["release_group"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p["label"] == format!("{name}: Group"))
        );
        let clone = json!({"name":format!("Clone {media}"),"include_when_renaming":false,"specifications":[saved["specification"].clone()]});
        assert_eq!(request(&base, "POST", &prefix, clone).await.0, 201);
        input["specifications"][0]["condition"]["pattern"] = json!("changed");
        let id = original["id"].as_i64().unwrap();
        assert_eq!(
            request(&base, "PUT", &format!("{prefix}/{id}"), input)
                .await
                .0,
            200
        );
        let (_, schema) = request(&base, "GET", &format!("{prefix}/schema"), Value::Null).await;
        let copied = schema["presets"]["release_title"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["label"] == format!("Clone {media}: {name}"))
            .unwrap();
        assert_eq!(copied["specification"]["condition"]["pattern"], "original");
        let other = if media == "tv" { "movies" } else { "tv" };
        assert!(!schema.to_string().contains(&format!("Clone {other}:")));
        assert_eq!(
            request(&base, "DELETE", &format!("{prefix}/{id}"), Value::Null)
                .await
                .0,
            204
        );
        let (_, schema) = request(&base, "GET", &format!("{prefix}/schema"), Value::Null).await;
        assert!(
            !schema["presets"]["release_title"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p["label"] == format!("{name}: {name}"))
        );
    }
}

#[tokio::test]
async fn final_format_delete_resets_only_its_domain_atomically_and_survives_reopen() {
    for media in ["tv", "movies"] {
        for bulk in [false, true] {
            let scratch = Scratch(
                std::env::temp_dir()
                    .join(format!("hrrdarr-format-delete-{}", uuid::Uuid::new_v4())),
            );
            std::fs::create_dir(&scratch.0).unwrap();
            let path = scratch.0.join("db");
            let db = Arc::new(Database::open_local(&path).await.unwrap());
            let c = db.connect().await.unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let app = hrrdarr::custom_formats::router(db.clone())
                .merge(hrrdarr::quality_profiles::router(db.clone()));
            let mut server = Server(tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap()
            }));
            let mut profiles = Vec::new();
            let mut ids = Vec::new();
            for domain in [media, if media == "tv" { "movies" } else { "tv" }] {
                let prefix = format!("/api/v1/{domain}");
                let (status, first) = request(
                    &base,
                    "POST",
                    &format!("{prefix}/custom-formats"),
                    definition("Scored", "Scored"),
                )
                .await;
                assert_eq!(status, 201);
                let fid = first["id"].as_i64().unwrap();
                let (status, zero) = request(
                    &base,
                    "POST",
                    &format!("{prefix}/custom-formats"),
                    definition("Zero", "Zero"),
                )
                .await;
                assert_eq!(status, 201);
                let zid = zero["id"].as_i64().unwrap();
                let (status, profile) = request(&base, "POST", &format!("{prefix}/quality-profiles"), json!({"name":"Policy","items":[{"kind":"quality","quality_id":3,"allowed":true}],"policy":{"upgrade_allowed":true,"cutoff":{"kind":"quality","quality_id":3},"min_format_score":-5,"cutoff_format_score":100,"min_upgrade_format_score":10,"language_id":if domain=="tv"{Value::Null}else{json!(-2)},"format_items":[{"format_id":fid,"score":10},{"format_id":zid,"score":0}]}})).await;
                assert_eq!(status, 201, "{profile}");
                c.execute(
                    "INSERT INTO quality_profiles(media_type,name)VALUES(?,'No policy')",
                    [domain],
                )
                .await
                .unwrap();
                profiles.push(profile);
                ids.push((fid, zid));
            }
            let prefix = format!("/api/v1/{media}/custom-formats");
            let profile_path = format!("/api/v1/{media}/quality-profiles/{}", profiles[0]["id"]);
            let (fid, zid) = ids[0];
            // Invalid mixed batches must preserve the definitions and the original policy.
            for (bad, status) in [
                (json!([fid, 0]), 400),
                (json!([fid, 999999]), 404),
                (json!([fid, ids[1].0]), 404),
            ] {
                assert_eq!(
                    request(
                        &base,
                        "DELETE",
                        &format!("{prefix}/bulk"),
                        json!({"ids":bad})
                    )
                    .await
                    .0,
                    status
                );
                assert_eq!(
                    request(&base, "GET", &profile_path, Value::Null).await.1,
                    profiles[0]
                );
                assert_eq!(
                    request(&base, "GET", &format!("{prefix}/{fid}"), Value::Null)
                        .await
                        .0,
                    200
                );
            }
            if !bulk {
                assert_eq!(
                    request(&base, "DELETE", &format!("{prefix}/{fid}"), Value::Null)
                        .await
                        .0,
                    204
                );
                // An implicit zero item remains even though the sparse score table is now empty.
                profiles[0]["policy"]["format_items"] = json!([{"format_id":zid,"score":0}]);
                assert_eq!(
                    request(&base, "GET", &profile_path, Value::Null).await.1,
                    profiles[0]
                );
            }
            let delete_path = if bulk {
                format!("{prefix}/bulk")
            } else {
                format!("{prefix}/{zid}")
            };
            let body = if bulk {
                json!({"ids":[fid,zid]})
            } else {
                Value::Null
            };
            c.execute_batch("CREATE TRIGGER reject_format_reset BEFORE UPDATE OF min_format_score ON quality_profile_policies BEGIN SELECT RAISE(ABORT,'injected reset failure'); END;").await.unwrap();
            assert_eq!(
                request(&base, "DELETE", &delete_path, body.clone()).await.0,
                500
            );
            assert_eq!(
                request(&base, "GET", &profile_path, Value::Null).await.1,
                profiles[0]
            );
            assert_eq!(
                request(&base, "GET", &format!("{prefix}/{zid}"), Value::Null)
                    .await
                    .0,
                200
            );
            c.execute_batch("DROP TRIGGER reject_format_reset")
                .await
                .unwrap();
            assert_eq!(request(&base, "DELETE", &delete_path, body).await.0, 204);
            let expected = &mut profiles[0];
            expected["policy"]["format_items"] = json!([]);
            expected["policy"]["min_format_score"] = json!(0);
            expected["policy"]["cutoff_format_score"] = json!(0);
            expected["policy"]["min_upgrade_format_score"] = json!(1);
            assert_eq!(
                request(&base, "GET", &profile_path, Value::Null).await.1,
                *expected
            );
            server.0.abort();
            // Await cancellation before reopening: the router owns the database lock.
            let stopped = tokio::time::timeout(std::time::Duration::from_secs(10), &mut server.0)
                .await
                .unwrap();
            assert!(stopped.unwrap_err().is_cancelled());
            drop(server);
            drop(c);
            drop(db);
            let reopened = Arc::new(Database::open_local(&path).await.unwrap());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let app = hrrdarr::quality_profiles::router(reopened.clone());
            let _server = Server(tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap()
            }));
            for (i, domain) in [media, if media == "tv" { "movies" } else { "tv" }]
                .into_iter()
                .enumerate()
            {
                let (status, all) = request(
                    &base,
                    "GET",
                    &format!("/api/v1/{domain}/quality-profiles"),
                    Value::Null,
                )
                .await;
                assert_eq!(status, 200);
                let all = all["items"].as_array().unwrap();
                for name in ["Policy", "No policy"] {
                    let id = &all.iter().find(|p| p["name"] == name).unwrap()["id"];
                    let (status, detail) = request(
                        &base,
                        "GET",
                        &format!("/api/v1/{domain}/quality-profiles/{id}"),
                        Value::Null,
                    )
                    .await;
                    assert_eq!(status, 200);
                    if name == "Policy" {
                        assert_eq!(detail, profiles[i]);
                    } else {
                        assert!(detail["policy"].is_null());
                    }
                }
            }
        }
    }
}

async fn fixture_indexer(c: &libsql::Connection) -> uuid::Uuid {
    let id = uuid::Uuid::new_v4();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint)VALUES(?,'torznab','Decision fixture',1,1,1,1,'http://fixture.invalid')", [id.to_string()]).await.unwrap();
    for media in ["tv", "movies"] {
        c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year)VALUES(?,'torznab',?,'[2000,5000]','[]',?,?)", libsql::params![id.to_string(),media,(media=="tv").then_some(0),(media=="movies").then_some(0)]).await.unwrap();
    }
    id
}
