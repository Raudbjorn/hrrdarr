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
    client: &RefreshClient,
    expected: &RssCandidate,
    timestamp: i64,
) -> Result<Option<crate::search::ReleaseDecision>> {
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = settle_local(
        &tx,
        client,
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
        assert!(
            settle(&c, &client, &expected, timestamp)
                .await
                .unwrap()
                .is_none()
        );
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
        assert!(
            settle(&c, &client, &expected, timestamp)
                .await
                .unwrap()
                .is_none()
        );
        mode(&base, media, Mode::PreferAndUpgrade).await;
        assert_eq!(
            candidate(&c, captured_id).await.unwrap().public.not_before,
            Some(0)
        );
        assert!(
            settle(&c, &client, &expected, timestamp)
                .await
                .unwrap()
                .is_none()
        );
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
            assert!(
                settle(&c, &client, &expected, timestamp)
                    .await
                    .unwrap()
                    .is_none()
            );
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
                assert!(
                    settle(&c, &client, &expected, timestamp)
                        .await
                        .unwrap()
                        .is_none()
                );
                assert_eq!(bytes(&c, id).await, before);
            }

            if status == "prepared" {
                // Already-prepared expected work retains the previous rule: a newly
                // delayed decision invalidates preparation, unlike a policy wake.
                let prepared = candidate(&c, id).await.unwrap().public;
                assert!(
                    settle(&c, &client, &prepared, timestamp)
                        .await
                        .unwrap()
                        .is_none()
                );
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
        assert!(settle(&c, &client, &expected, timestamp).await.is_err());
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

async fn cohort_command(c: &Connection, media: MediaDomain, endpoint: &str) -> RssCommand {
    let indexer = Uuid::new_v4();
    let downloader = Uuid::new_v4();
    for (id, implementation) in [(indexer, "torznab"), (downloader, "qbittorrent")] {
        c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint)VALUES(?,?,?,1,1,1,1,?)",params![id.to_string(),implementation,implementation,endpoint]).await.unwrap();
    }
    c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year)VALUES(?,'torznab',?,'[5000]','[]',?,?)",params![indexer.to_string(),domain(media),matches!(media,MediaDomain::Tv).then_some(0),matches!(media,MediaDomain::Movies).then_some(0)]).await.unwrap();
    c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags)VALUES(?,'qbittorrent',?,?,0,0,'started','default',0,0,0)",params![downloader.to_string(),domain(media),domain(media)]).await.unwrap();
    let id = Uuid::new_v4();
    c.execute("INSERT INTO rss_commands(id,name,media_type,indexer_id,indexer_revision,client_id,client_revision,next_attempt_at,created_at)VALUES(?,'rss_sync',?,?,1,?,1,100,100)",params![id.to_string(),domain(media),indexer.to_string(),downloader.to_string()]).await.unwrap();
    read(c, id).await.unwrap()
}
async fn full_delay(base: &str, media: MediaDomain, minutes: u32) {
    full_delay_preferred(base, media, minutes, "torrent").await;
}
async fn full_delay_preferred(base: &str, media: MediaDomain, minutes: u32, preferred: &str) {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
    let url = format!("{base}/api/v1/{}/delay-profiles", domain(media));
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
        .header("content-type", "application/json").body(json!({"revision":old["revision"],"profile":{"tag_ids":[],"settings":{"torrent_delay_minutes":minutes,"usenet_delay_minutes":minutes,"enable_torrent":true,"enable_usenet":true,"preferred_protocol":preferred,"bypass_if_highest_quality":false,"bypass_if_above_custom_format_score":false,"minimum_custom_format_score":0}}}).to_string())
        .send().await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        response.text().await.unwrap()
    );
}
async fn replace_scratch_payload(c: &Connection, id: Uuid, payload: Vec<u8>) {
    // Simulate corrupt historical evidence only in this isolated DB. Restore the
    // exact installed transition guard in the same transaction before exercising readers.
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    let sql: String = tx.query("SELECT sql FROM sqlite_master WHERE type='trigger' AND name='rss_candidate_transition'",()).await.unwrap().next().await.unwrap().unwrap().get(0).unwrap();
    tx.execute_batch("DROP TRIGGER rss_candidate_transition")
        .await
        .unwrap();
    tx.execute(
        "UPDATE rss_candidates SET private_payload=? WHERE id=?",
        params![payload, id.to_string()],
    )
    .await
    .unwrap();
    tx.execute_batch(&sql).await.unwrap();
    tx.commit().await.unwrap();
}
#[tokio::test]
async fn pending_cohort_crosses_commands_and_attributes_sibling_errors() {
    let scratch =
        Scratch(std::env::temp_dir().join(format!("hrrdarr-delay-cohort-{}", Uuid::new_v4())));
    std::fs::create_dir_all(&scratch.0).unwrap();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let c = db.connect().await.unwrap();
    c.execute_batch("INSERT INTO series(id,title,path)VALUES(1,'Harbor','/fictional-tv'); INSERT INTO seasons(series_id,number)VALUES(1,1); INSERT INTO episodes(id,series_id,season,number,title,runtime,air_date_utc)VALUES(1,1,1,1,'Pilot',45,'2020-01-01 00:00:00'); INSERT INTO movie_metadata(id,title,year,runtime,digital_release)VALUES(1,'Harbor',2020,100,'2020-01-01 00:00:00'); INSERT INTO movies(id,metadata_id,path)VALUES(1,1,'/fictional-movie'); INSERT INTO quality_profiles VALUES(1,'tv','HD'),(2,'movies','HD'); INSERT INTO quality_profile_items(profile_id,media_type,quality_id,position,allowed)VALUES(1,'tv',3,0,1),(2,'movies',3,0,1); INSERT INTO quality_profile_policies(profile_id,media_type,upgrade_allowed,cutoff_quality_id,min_format_score,cutoff_format_score,min_upgrade_format_score,language_id)VALUES(1,'tv',1,3,0,0,1,NULL),(2,'movies',1,3,0,0,1,-1); INSERT INTO library_settings(media_type,series_id,quality_profile_id,series_type,use_scene_numbering)VALUES('tv',1,1,'standard',0); INSERT INTO library_settings(media_type,movie_id,quality_profile_id,minimum_availability)VALUES('movies',1,2,'released'); INSERT INTO release_delay_policies VALUES('tv',60,60,0),('movies',60,60,0); UPDATE quality_definitions SET min_size=0,max_size=NULL WHERE quality_id=3;").await.unwrap();
    let key = Arc::new(crate::providers::CredentialKey::from_hex(&"11".repeat(32)).unwrap());
    let (_, client) = crate::providers::router_with_refresh(db.clone(), Some(key));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = crate::delay_profiles::router(db.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    for media in [MediaDomain::Tv, MediaDomain::Movies] {
        let timestamp = now().unwrap();
        let old_command = cohort_command(&c, media, &base).await;
        let new_command = cohort_command(&c, media, &base).await;
        let cancelled = put_capture(&c, &client, &old_command, timestamp - 9999).await;
        candidate_state(&c, cancelled, "cancelled", None, None)
            .await
            .unwrap();
        let cancelled_before = bytes(&c, cancelled).await;
        if media == MediaDomain::Tv {
            c.execute("INSERT INTO episodes(id,series_id,season,number,title,runtime,air_date_utc)VALUES(2,1,1,2,'Second',45,'2020-01-01 00:00:00')",()).await.unwrap();
            let mut other = captured(media, timestamp - 9999);
            other.title = "Harbor.S01E02.1080p.WEB-DL.BONUS".into();
            other.release.metadata.title = Some(other.title.clone());
            crate::search::evaluate(&c, media, &other.release, SearchContext::Rss, timestamp)
                .await
                .unwrap();
            let tx = c
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .await
                .unwrap();
            insert_captured(&tx, &client, &old_command, other, timestamp)
                .await
                .unwrap();
            tx.commit().await.unwrap();
        }
        let old = put_capture(&c, &client, &old_command, timestamp - 601).await;
        let young = put_capture(&c, &client, &new_command, timestamp).await;
        assert_ne!(old_command.id, new_command.id);
        c.execute(
            "UPDATE providers SET enabled=0,revision=revision+1 WHERE id=?",
            [old_command.target.indexer_id.to_string()],
        )
        .await
        .unwrap();
        full_delay(&base, media, 10).await;
        let target = candidate(&c, young).await.unwrap().public.target.unwrap();
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .unwrap();
        let items = pending::cohort(&tx, &client, &target).await.unwrap();
        assert_eq!(
            items.len(),
            2,
            "Cancelled, other-media and disjoint-TV-episode rows are outside this cohort"
        );
        assert_eq!(pending::oldest(&items), Some(timestamp - 601));
        // Oldest publication is independent of the old candidate's disabled provider.
        // Its age==delay does not bypass; age>delay does, even across command IDs.
        assert_eq!(
            pending::best(&tx, &items, timestamp - 1).await.unwrap(),
            None
        );
        assert_eq!(
            pending::best(&tx, &items, timestamp).await.unwrap(),
            Some(young)
        );
        tx.commit().await.unwrap();
        let before = bytes(&c, young).await;
        let selected = pending::select_work(&db, &client, candidate(&c, old).await.unwrap())
            .await
            .unwrap();
        assert_eq!(selected.public.id, young);
        assert_eq!(bytes(&c, young).await, before);
        assert_eq!(bytes(&c, cancelled).await, cancelled_before);
        assert_eq!(candidate(&c, old).await.unwrap().public.status, "pending");
        for decoded_but_invalid in [false, true] {
            let broken_command = cohort_command(&c, media, &base).await;
            let broken = put_capture(&c, &client, &broken_command, timestamp).await;
            let payload = if decoded_but_invalid {
                let work = candidate(&c, broken).await.unwrap();
                let mut release = pending::decode(&client, &work).unwrap();
                release.metadata.languages = vec!["English".into(); 65];
                let mut encoded = indexer::encode_private(release).unwrap();
                let payload = client
                    .seal_release(&payload_context(broken, work.public.source), &encoded)
                    .unwrap();
                encoded.fill(0);
                payload
            } else {
                vec![0; 29]
            };
            replace_scratch_payload(&c, broken, payload).await;
            // Exercise the actual selector with the corrupt sibling as original trigger.
            // A permanent sibling failure must never reject the healthy winner.
            let selected = pending::select_work(&db, &client, candidate(&c, broken).await.unwrap())
                .await
                .unwrap();
            assert_eq!(selected.public.id, young);
            let rejected = candidate(&c, broken).await.unwrap();
            assert_eq!(rejected.public.status, "rejected");
            assert!(rejected.payload.is_none());
            assert_eq!(bytes(&c, young).await, before);
        }
        for (table, code) in [
            ("delay_profile_domains", "delay_profile_storage_error"),
            ("revision_policies", "revision_policy_storage_error"),
            ("episodes", "release_storage_error"),
            ("quality_profile_items", "release_storage_error"),
            ("custom_formats", "release_storage_error"),
            ("series_tags", "delay_profile_storage_error"),
            ("movie_tags", "delay_profile_storage_error"),
        ] {
            if ((table == "episodes" || table == "series_tags") && media == MediaDomain::Movies)
                || (table == "movie_tags" && media == MediaDomain::Tv)
            {
                continue;
            }
            let before_old = bytes(&c, old).await;
            // Actual readers, not injected Error values: temporarily unavailable
            // tables abort run_due and leave the awakened due receipt retryable.
            c.execute_batch(&format!(
                "ALTER TABLE {table} RENAME TO scratch_unavailable"
            ))
            .await
            .unwrap();
            let outcome = run_due(&db, &client, young).await;
            c.execute_batch(&format!(
                "ALTER TABLE scratch_unavailable RENAME TO {table}"
            ))
            .await
            .unwrap();
            assert_eq!(outcome.unwrap_err().1, code);
            assert_eq!(bytes(&c, young).await, before);
            assert_eq!(bytes(&c, old).await, before_old);
            assert_eq!(
                candidate(&c, young).await.unwrap().public.not_before,
                Some(0)
            );
        }
        // The shared rank boundary reads the real selected profile. This proves
        // preference ordering, not an unavailable Usenet submission transport.
        let torrent = release(media, timestamp);
        let mut usenet = release(media, timestamp);
        usenet.facts.torrent = None;
        let torrent_decision =
            crate::search::evaluate(&c, media, &torrent, SearchContext::UserSearch, timestamp)
                .await
                .unwrap();
        let usenet_decision =
            crate::search::evaluate(&c, media, &usenet, SearchContext::UserSearch, timestamp)
                .await
                .unwrap();
        let torrent_rank =
            crate::search::revision::release_preference(&c, media, &torrent, &torrent_decision)
                .await
                .unwrap();
        let usenet_rank =
            crate::search::revision::release_preference(&c, media, &usenet, &usenet_decision)
                .await
                .unwrap();
        assert!(torrent_rank > usenet_rank);
        full_delay_preferred(&base, media, 10, "usenet").await;
        let torrent_rank =
            crate::search::revision::release_preference(&c, media, &torrent, &torrent_decision)
                .await
                .unwrap();
        let usenet_rank =
            crate::search::revision::release_preference(&c, media, &usenet, &usenet_decision)
                .await
                .unwrap();
        assert!(usenet_rank > torrent_rank);
        // Excess stored assignments are semantic corruption, not temporary SQL
        // unavailability. They must retain the permanent failure classification.
        let (table, owner, start) = if media == MediaDomain::Tv {
            ("series_tags", "series_id", 1000)
        } else {
            ("movie_tags", "movie_id", 2000)
        };
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .unwrap();
        for offset in 0..201 {
            let id = start + offset;
            tx.execute(
                "INSERT INTO tags(id,media_type,label)VALUES(?,?,?)",
                params![id, domain(media), format!("stored-{id}")],
            )
            .await
            .unwrap();
            tx.execute(
                &format!("INSERT INTO {table}({owner},tag_id)VALUES(1,?)"),
                [id],
            )
            .await
            .unwrap();
        }
        tx.commit().await.unwrap();
        let invalid = crate::delay_profiles::select(&c, media, 1)
            .await
            .unwrap_err();
        assert_eq!(invalid.code(), "delay_profile_invariant");
        let old_before = bytes(&c, old).await;
        run_due(&db, &client, young).await.unwrap();
        let rejected = candidate(&c, young).await.unwrap();
        assert_eq!(rejected.public.status, "rejected");
        assert_eq!(rejected.public.error_code.as_deref(), Some("storage_error"));
        assert!(rejected.payload.is_none());
        assert_eq!(bytes(&c, old).await, old_before);
    }
    server.abort();
    let _ = server.await;
}
