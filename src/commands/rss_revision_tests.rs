use super::*;
use crate::revision_policy::Mode;
use serde_json::json;
struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn release(media: MediaDomain, timestamp: i64) -> indexer::Release {
    let tv = matches!(media, MediaDomain::Tv);
    let title = if tv {
        "Harbor.S01E01.1080p.WEB-DL.BONUS"
    } else {
        "Harbor.2020.1080p.WEB-DL.BONUS"
    };
    let date = chrono::DateTime::from_timestamp(timestamp, 0)
        .unwrap()
        .to_rfc2822();
    let xml = format!(
        r#"<rss xmlns:torznab="http://torznab.com/schemas/2015/feed"><channel><item><title>{title}</title><guid>revision-fixture</guid><pubDate>{date}</pubDate><link>https://fixture.invalid/private</link><torznab:attr name="category" value="{}"/><torznab:attr name="size" value="1073741824"/></item></channel></rss>"#,
        if tv { 5030 } else { 2030 }
    );
    indexer::parse_page(&xml, 0, 100, true, media)
        .unwrap()
        .items
        .remove(0)
}
fn captured(media: MediaDomain, timestamp: i64) -> CapturedRelease {
    let release = release(media, timestamp);
    CapturedRelease {
        id: Uuid::new_v4(),
        // Candidate fingerprints use the production SHA256 contract, not UUID text.
        fingerprint: digest(Uuid::new_v4().as_bytes()),
        title: release.metadata.title.clone().unwrap(),
        release,
    }
}
async fn mode(base: &str, media: MediaDomain, value: Mode) {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
    let url = format!("{base}/api/v1/{}/revision-policy", domain(media));
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
    let response = client
        .put(url)
        .header("content-type", "application/json")
        .body(json!({"mode":value,"revision":old["revision"]}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
async fn put_capture(
    c: &Connection,
    client: &RefreshClient,
    command: &RssCommand,
    timestamp: i64,
) -> Uuid {
    let item = captured(command.target.media_type, timestamp);
    let id = item.id;
    crate::search::evaluate(
        c,
        command.target.media_type,
        &item.release,
        SearchContext::Rss,
        timestamp,
    )
    .await
    .unwrap();
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    insert_captured(&tx, client, command, item, timestamp)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    id
}
async fn settle(
    c: &Connection,
    expected: &RssCandidate,
    timestamp: i64,
) -> Result<Option<crate::search::ReleaseDecision>> {
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = settle_local(
        &tx,
        expected,
        &release(expected.source.media_type, timestamp),
        timestamp,
    )
    .await;
    finish(tx, outcome).await
}
async fn bytes(c: &Connection, id: Uuid) -> String {
    c.query("SELECT json_object('status',status,'payload',hex(private_payload),'deadline',not_before,'reasons',decision_reasons_json,'identity',submission_identity_json,'facts',comparison_facts_json) FROM rss_candidates WHERE id=?",[id.to_string()]).await.unwrap().next().await.unwrap().unwrap().get(0).unwrap()
}
#[tokio::test]
async fn revision_policy_orders_pending_settlement_and_capture() {
    let scratch =
        Scratch(std::env::temp_dir().join(format!("hrrdarr-revision-rss-{}", Uuid::new_v4())));
    std::fs::create_dir_all(&scratch.0).unwrap();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let c = db.connect().await.unwrap();
    // Same-quality CF upgrade with genuinely unknown current revision: DoNotPrefer
    // permits it, whereas preference mode must reject unknown replacement evidence.
    c.execute_batch("INSERT INTO series(id,title,path)VALUES(1,'Harbor','/fictional-tv'); INSERT INTO seasons(series_id,number)VALUES(1,1); INSERT INTO episode_files(id,series_id,path)VALUES(1,1,'/fictional-tv/old.mkv'); INSERT INTO episodes(id,series_id,season,number,title,runtime,air_date_utc,episode_file_id)VALUES(1,1,1,1,'Pilot',45,'2020-01-01 00:00:00',1); INSERT INTO movie_metadata(id,title,year,runtime,digital_release)VALUES(1,'Harbor',2020,100,'2020-01-01 00:00:00'); INSERT INTO movies(id,metadata_id,path)VALUES(1,1,'/fictional-movie'); INSERT INTO movie_files(id,movie_id,path)VALUES(1,1,'/fictional-movie/old.mkv'); INSERT INTO file_metadata(media_type,episode_file_id,quality_id)VALUES('tv',1,3); INSERT INTO file_metadata(media_type,movie_file_id,quality_id)VALUES('movies',1,3); INSERT INTO quality_profiles VALUES(1,'tv','HD'),(2,'movies','HD'); INSERT INTO quality_profile_items(profile_id,media_type,quality_id,position,allowed)VALUES(1,'tv',3,0,1),(2,'movies',3,0,1); INSERT INTO quality_profile_policies(profile_id,media_type,upgrade_allowed,cutoff_quality_id,min_format_score,cutoff_format_score,min_upgrade_format_score,language_id)VALUES(1,'tv',1,3,0,100,1,NULL),(2,'movies',1,3,0,100,1,-1); INSERT INTO library_settings(media_type,series_id,quality_profile_id,series_type,use_scene_numbering)VALUES('tv',1,1,'standard',0); INSERT INTO library_settings(media_type,movie_id,quality_profile_id,minimum_availability)VALUES('movies',1,2,'released'); INSERT INTO release_delay_policies VALUES('tv',60,60,0),('movies',60,60,0); UPDATE quality_definitions SET min_size=0,max_size=NULL WHERE quality_id=3;").await.unwrap();
    for (id, media) in [(1, "tv"), (2, "movies")] {
        let specs = json!([{"name":"Bonus","required":false,"negate":false,"condition":{"kind":"release_title","pattern":"BONUS"}}]);
        c.execute("INSERT INTO custom_formats(id,media_type,name,include_when_renaming,specifications_json)VALUES(?,?,'Bonus',0,?)",params![id,media,specs.to_string()]).await.unwrap();
        c.execute(
            "INSERT INTO quality_profile_format_scores VALUES(?,?,?,10)",
            params![id, id, media],
        )
        .await
        .unwrap();
    }
    let key = Arc::new(crate::providers::CredentialKey::from_hex(&"11".repeat(32)).unwrap());
    let (_, client) = crate::providers::router_with_refresh(db.clone(), Some(key));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = crate::revision_policy::router(db.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let timestamp = now().unwrap();
    for media in [MediaDomain::Tv, MediaDomain::Movies] {
        let indexer = Uuid::new_v4();
        let downloader = Uuid::new_v4();
        for (id, implementation) in [(indexer, "torznab"), (downloader, "qbittorrent")] {
            c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint)VALUES(?,?,?,1,1,1,1,'http://fixture.invalid')",params![id.to_string(),implementation,implementation]).await.unwrap();
        }
        c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year)VALUES(?,'torznab',?,'[5000]','[]',?,?)",params![indexer.to_string(),domain(media),matches!(media,MediaDomain::Tv).then_some(0),matches!(media,MediaDomain::Movies).then_some(0)]).await.unwrap();
        c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags)VALUES(?,'qbittorrent',?,?,0,0,'started','default',0,0,0)",params![downloader.to_string(),domain(media),domain(media)]).await.unwrap();
        let command_id = Uuid::new_v4();
        c.execute("INSERT INTO rss_commands(id,name,media_type,indexer_id,indexer_revision,client_id,client_revision,next_attempt_at,created_at)VALUES(?,'rss_sync',?,?,1,?,1,100,100)",params![command_id.to_string(),domain(media),indexer.to_string(),downloader.to_string()]).await.unwrap();
        let command = read(&c, command_id).await.unwrap();
        mode(&base, media, Mode::DoNotPrefer).await;
        let id = put_capture(&c, &client, &command, timestamp).await;
        let expected = candidate(&c, id).await.unwrap().public;
        assert_eq!(expected.not_before, Some(timestamp + 3600));
        let stale = crate::search::evaluate(
            &c,
            media,
            &release(media, timestamp),
            SearchContext::Rss,
            timestamp,
        )
        .await
        .unwrap();
        assert_eq!(stale.disposition, Disposition::Delay);
        // PUT commits first; old Delay must not overwrite wake or fresh rejection.
        mode(&base, media, Mode::PreferAndUpgrade).await;
        assert_eq!(candidate(&c, id).await.unwrap().public.not_before, Some(0));
        assert!(settle(&c, &expected, timestamp).await.unwrap().is_none());
        let fresh = candidate(&c, id).await.unwrap().public;
        assert_eq!(fresh.status, "rejected");
        assert!(fresh.not_before.is_none());
        assert!(fresh.reasons.iter().any(|s| s == "revision_unknown"));
        // A row absent during PUT cannot be woken: capture must evaluate anew.
        let stale = crate::search::evaluate(
            &c,
            media,
            &release(media, timestamp),
            SearchContext::Rss,
            timestamp,
        )
        .await
        .unwrap();
        assert_eq!(stale.disposition, Disposition::Reject);
        let item = captured(media, timestamp);
        let captured_id = item.id;
        mode(&base, media, Mode::DoNotPrefer).await;
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .unwrap();
        insert_captured(&tx, &client, &command, item, timestamp)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let expected = candidate(&c, captured_id).await.unwrap().public;
        assert_eq!(expected.status, "pending");
        assert_eq!(expected.not_before, Some(timestamp + 3600));
        // Consumer commits first; later PUT must win the serialized ordering.
        assert!(settle(&c, &expected, timestamp).await.unwrap().is_none());
        mode(&base, media, Mode::PreferAndUpgrade).await;
        assert_eq!(
            candidate(&c, captured_id).await.unwrap().public.not_before,
            Some(0)
        );
        assert!(settle(&c, &expected, timestamp).await.unwrap().is_none());
        assert_eq!(
            candidate(&c, captured_id).await.unwrap().public.status,
            "rejected"
        );
        mode(&base, media, Mode::DoNotPrefer).await;
        for status in ["prepared", "cancelled", "submitting"] {
            let id = put_capture(&c, &client, &command, timestamp).await;
            let expected = candidate(&c, id).await.unwrap().public;
            if status == "cancelled" {
                candidate_state(&c, id, "cancelled", None, None)
                    .await
                    .unwrap();
            } else {
                let hash = digest(id.as_bytes())[..40].to_owned();
                let identity = json!({"version":1,"target":{"media_type":if matches!(media,MediaDomain::Tv){"episode"}else{"movie"},"id":1},"hashes":[hash],"settings_fingerprint":"b".repeat(64),"payload_sha256":"c".repeat(64)});
                c.execute("UPDATE rss_candidates SET status='prepared',submission_identity_json=? WHERE id=?",params![identity.to_string(),id.to_string()]).await.unwrap();
                if status != "prepared" {
                    // The real dispatch transition requires complete per-candidate hash claims.
                    c.execute(
                        "INSERT INTO rss_hash_claims VALUES(?,?,?)",
                        params![downloader.to_string(), hash, id.to_string()],
                    )
                    .await
                    .unwrap();
                    c.execute("UPDATE rss_candidates SET status='submitting',private_payload=NULL WHERE id=?",[id.to_string()]).await.unwrap();
                }
            }
            let before = bytes(&c, id).await;
            assert!(settle(&c, &expected, timestamp).await.unwrap().is_none());
            assert_eq!(bytes(&c, id).await, before, "{status}");
            if status == "submitting" {
                // Reuse the same owner for reconciliation: a second prepared row
                // for this episode/movie is correctly forbidden by ownership guards.
                c.execute(
                    "UPDATE rss_candidates SET status='reconciling',attempts=1 WHERE id=?",
                    [id.to_string()],
                )
                .await
                .unwrap();
                let before = bytes(&c, id).await;
                assert!(settle(&c, &expected, timestamp).await.unwrap().is_none());
                assert_eq!(bytes(&c, id).await, before);
            }

            if status == "prepared" {
                // Already-prepared expected work retains the previous rule: a newly
                // delayed decision invalidates preparation, unlike a policy wake.
                let prepared = candidate(&c, id).await.unwrap().public;
                assert!(settle(&c, &prepared, timestamp).await.unwrap().is_none());
                assert_eq!(
                    candidate(&c, id)
                        .await
                        .unwrap()
                        .public
                        .error_code
                        .as_deref(),
                    Some("target_changed")
                );
            }
        }
        let id = put_capture(&c, &client, &command, timestamp).await;
        let expected = candidate(&c, id).await.unwrap().public;
        let before = bytes(&c, id).await;
        c.execute_batch("CREATE TRIGGER reject_revision_deadline BEFORE UPDATE OF not_before ON rss_candidates BEGIN SELECT RAISE(ABORT,'fixture deadline rollback'); END").await.unwrap();
        assert!(settle(&c, &expected, timestamp).await.is_err());
        assert_eq!(bytes(&c, id).await, before);
        c.execute_batch("DROP TRIGGER reject_revision_deadline; CREATE TRIGGER reject_revision_capture BEFORE INSERT ON rss_candidate_episodes BEGIN SELECT RAISE(ABORT,'fixture capture rollback'); END").await.unwrap();
        if matches!(media, MediaDomain::Tv) {
            let item = captured(media, timestamp);
            let id = item.id;
            let tx = c
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .await
                .unwrap();
            assert!(
                insert_captured(&tx, &client, &command, item, timestamp)
                    .await
                    .is_err()
            );
            tx.rollback().await.unwrap();
            assert!(
                c.query("SELECT 1 FROM rss_candidates WHERE id=?", [id.to_string()])
                    .await
                    .unwrap()
                    .next()
                    .await
                    .unwrap()
                    .is_none()
            );
        }
        c.execute_batch("DROP TRIGGER reject_revision_capture")
            .await
            .unwrap();
        let id = put_capture(&c, &client, &command, timestamp).await;
        c.execute(
            "UPDATE rss_candidates SET not_before=0 WHERE id=?",
            [id.to_string()],
        )
        .await
        .unwrap();
        let before = bytes(&c, id).await;
        for code in [
            "custom_format_state_changed",
            "custom_format_busy",
            "custom_format_timeout",
            "custom_format_worker_failed",
        ] {
            let error = settle_due_error(&c, id, Error(StatusCode::SERVICE_UNAVAILABLE, code))
                .await
                .unwrap_err();
            assert_eq!(error.1, code);
            assert_eq!(bytes(&c, id).await, before);
            assert!(
                candidate(&c, id)
                    .await
                    .unwrap()
                    .public
                    .not_before
                    .is_some_and(|n| n <= timestamp)
            );
        }
        settle_due_error(
            &c,
            id,
            Error(StatusCode::CONFLICT, "custom_format_facts_invalid"),
        )
        .await
        .unwrap();
        assert_eq!(candidate(&c, id).await.unwrap().public.status, "rejected");
        assert!(candidate(&c, id).await.unwrap().payload.is_none());
    }
    server.abort();
    let _ = server.await;
}
