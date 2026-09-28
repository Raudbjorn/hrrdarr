use super::*;
struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
fn release(tv: bool, n: usize) -> indexer::Release {
    let title = if tv {
        format!("Harbor.S01E01.1080p.WEB-DL.BATCH{n}")
    } else {
        format!("Harbor.2020.1080p.WEB-DL.BATCH{n}")
    };
    let xml = format!(
        r#"<rss xmlns:torznab="http://torznab.com/schemas/2015/feed"><channel><item><title>{title}</title><guid>batch-{n}</guid><pubDate>Mon, 01 Jan 2024 12:00:00 +0000</pubDate><link>magnet:?xt=urn:btih:{:040x}</link><torznab:attr name="category" value="{}"/><torznab:attr name="size" value="1073741824"/></item></channel></rss>"#,
        n + 1,
        if tv { 5030 } else { 2030 }
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
async fn command(c: &Connection, tv: bool, indexer: Uuid, client: Uuid) -> SearchCommand {
    let id = Uuid::new_v4();
    let media = if tv {
        MediaDomain::Tv
    } else {
        MediaDomain::Movies
    };
    let target = if tv {
        MediaTarget::Episode(1)
    } else {
        MediaTarget::Movie(1)
    };
    let captured = identity(c, &target).await.unwrap();
    let time = now().unwrap();
    c.execute("INSERT INTO search_commands(id,mode,decision_context,media_type,requested_episode_id,requested_movie_id,captured_target_json,indexer_id,indexer_revision,client_id,client_revision,next_attempt_at,created_at)VALUES(?,'interactive','user_search',?,?,?,?,?,1,?,1,?,?)",params![id.to_string(),domain(media),tv.then_some(1),(!tv).then_some(1),captured,indexer.to_string(),client.to_string(),time,time]).await.unwrap();
    claim(c, id, time).await.unwrap()
}
#[tokio::test]
async fn batch_evidence_survives_cache_eviction_and_transients_preserve_search_rows() {
    let first = crate::search::operation_evidence().unwrap();
    let second = crate::search::operation_evidence().unwrap();
    assert!(matches!(
        crate::search::operation_evidence(),
        Err(crate::search::SearchError("release_term_busy"))
    ));
    drop(first);
    let replacement = crate::search::operation_evidence().unwrap();
    drop(replacement);
    drop(second);

    let scratch =
        Scratch(std::env::temp_dir().join(format!("hrrdarr-restriction-batch-{}", Uuid::new_v4())));
    std::fs::create_dir_all(&scratch.0).unwrap();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let c = db.connect().await.unwrap();
    c.execute_batch("INSERT INTO series(id,title,path)VALUES(1,'Harbor','/fictional-tv'); INSERT INTO seasons(series_id,number)VALUES(1,1); INSERT INTO episodes(id,series_id,season,number,title,runtime,air_date_utc)VALUES(1,1,1,1,'Pilot',45,'2020-01-01T00:00:00Z'); INSERT INTO movie_metadata(id,title,year,runtime,digital_release)VALUES(1,'Harbor',2020,100,'2020-01-01T00:00:00Z'); INSERT INTO movies(id,metadata_id,path)VALUES(1,1,'/fictional-movie'); INSERT INTO quality_profiles VALUES(1,'tv','HD'),(2,'movies','HD'); INSERT INTO quality_profile_items(profile_id,media_type,quality_id,position,allowed)VALUES(1,'tv',3,0,1),(2,'movies',3,0,1); INSERT INTO quality_profile_policies(profile_id,media_type,upgrade_allowed,cutoff_quality_id,min_format_score,cutoff_format_score,min_upgrade_format_score,language_id)VALUES(1,'tv',1,3,0,0,1,NULL),(2,'movies',1,3,0,0,1,-1); INSERT INTO library_settings(media_type,series_id,quality_profile_id,series_type,use_scene_numbering)VALUES('tv',1,1,'standard',0); INSERT INTO library_settings(media_type,movie_id,quality_profile_id,minimum_availability)VALUES('movies',1,2,'released'); INSERT INTO release_delay_policies VALUES('tv',0,0,0),('movies',0,0,0); UPDATE quality_definitions SET min_size=0,max_size=NULL WHERE quality_id=3;").await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let posts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = posts.clone();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().fallback(move || {
                let observed = observed.clone();
                async move {
                    observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    StatusCode::BAD_REQUEST
                }
            }),
        )
        .await
        .unwrap()
    });
    let key = Arc::new(crate::providers::CredentialKey::from_hex(&"11".repeat(32)).unwrap());
    let (_, client) = crate::providers::router_with_refresh(db.clone(), Some(key));
    for tv in [true, false] {
        let media = if tv {
            MediaDomain::Tv
        } else {
            MediaDomain::Movies
        };
        let d = domain(media);
        let indexer = Uuid::new_v4();
        let downloader = Uuid::new_v4();
        for (id, kind) in [(indexer, "torznab"), (downloader, "qbittorrent")] {
            c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint)VALUES(?,?,?,1,1,1,1,?)",params![id.to_string(),kind,kind,endpoint.clone()]).await.unwrap();
        }
        c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year)VALUES(?,'torznab',?,'[2000,5000]','[]',?,?)",params![indexer.to_string(),d,tv.then_some(0),(!tv).then_some(0)]).await.unwrap();
        c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags)VALUES(?,'qbittorrent',?,?,0,0,'started','default',0,0,0)",params![downloader.to_string(),d,d]).await.unwrap();
        c.execute("INSERT INTO release_profiles(id,media_type,name,enabled,required_json,ignored_json,air_date_restriction,air_date_grace_period_days,allow_season_pack_without_all_episodes_aired)VALUES(?,?,'Batch',1,'[\"WEB\"]','[]',?,?,?)",params![if tv{1}else{2},d,tv.then_some(0),tv.then_some(0),tv.then_some(0)]).await.unwrap();
        let running = command(&c, tv, indexer, downloader).await;
        let batch = (0..129)
            .map(|n| indexer::encode_private(release(tv, n)).unwrap())
            .collect();
        publish(&db, &client, &running, batch).await.unwrap();
        assert!(matches!(
            read(&c, running.id).await.unwrap().status,
            CommandStatus::Succeeded
        ));
        let row=c.query("SELECT count(*),count(private_payload),sum(selected_candidate_id IS NOT NULL) FROM search_results WHERE command_id=?",[running.id.to_string()]).await.unwrap().next().await.unwrap().unwrap();
        assert_eq!(
            (
                row.get::<i64>(0).unwrap(),
                row.get::<i64>(1).unwrap(),
                row.get::<i64>(2).unwrap()
            ),
            (129, 129, 0)
        );
        drop(row);
        // A cold exact title under a writer reaches the real evaluator's StateChanged,
        // then the production run settlement retains durable encrypted offers and retries.
        for code in [
            "release_term_state_changed",
            "release_term_busy",
            "release_term_timeout",
            "release_term_worker_failed",
        ] {
            let mut running = command(&c, tv, indexer, downloader).await;
            let id = Uuid::new_v4();
            let t = now().unwrap();
            let item = release(tv, 999);
            let encrypted = client
                .seal_release(
                    &envelope(id, &running),
                    &indexer::encode_private(item).unwrap(),
                )
                .unwrap();
            c.execute("INSERT INTO search_results(id,command_id,ordinal,fingerprint,title,metadata_json,decision_json,private_payload,created_at,expires_at)VALUES(?,?,0,?,'retained','{}','{}',?,?,?)",params![id.to_string(),running.id.to_string(),rss::digest(id.as_bytes()),encrypted.clone(),t,t+1800]).await.unwrap();
            if code == "release_term_state_changed" {
                let tx = c
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .await
                    .unwrap();
                let error = evaluate(
                    &tx,
                    &running,
                    &release(tv, 10000),
                    &mut crate::release_profile_terms::OperationEvidence::default(),
                )
                .await
                .unwrap_err();
                assert_eq!(error.1, code);
                tx.rollback().await.unwrap();
            }
            for attempt in 1..=3 {
                assert_eq!(running.attempts, attempt);
                let before = now().unwrap();
                settle_failure(&db, &running, code).await.unwrap();
                let saved = read(&c, running.id).await.unwrap();
                assert_eq!(saved.error_code.as_deref(), Some("storage_error"));
                assert!(saved.next_attempt_at >= before + (1i64 << attempt));
                assert!(saved.next_attempt_at <= now().unwrap() + (1i64 << attempt));
                assert_eq!(saved.completed_at.is_some(), attempt == 3);
                assert!(if attempt < 3 {
                    matches!(saved.status, CommandStatus::RetryWait)
                } else {
                    matches!(saved.status, CommandStatus::Failed)
                });
                let row=c.query("SELECT private_payload,selected_candidate_id FROM search_results WHERE id=?",[id.to_string()]).await.unwrap().next().await.unwrap().unwrap();
                assert_eq!(row.get::<Vec<u8>>(0).unwrap(), encrypted);
                assert!(row.get::<Option<String>>(1).unwrap().is_none());
                drop(row);
                if attempt < 3 {
                    running = claim(&c, running.id, now().unwrap()).await.unwrap();
                }
            }
        }
    }
    assert_eq!(posts.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(
        c.query("SELECT count(*) FROM rss_candidates", ())
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
    server.abort();
    let _ = server.await;
}
