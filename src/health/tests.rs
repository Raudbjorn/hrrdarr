use super::*;
struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.0) {
            eprintln!("health scratch cleanup: {e}")
        }
    }
}
async fn fixture_all() -> (Scratch, Database, Connection) {
    let path = std::env::temp_dir().join(format!("hrrdarr-health-{}", Uuid::new_v4()));
    std::fs::create_dir(&path).unwrap();
    let db = Database::open_local(path.join("db")).await.unwrap();
    let c = db.connect().await.unwrap();
    (Scratch(path), db, c)
}
// These existing lifecycle cases deliberately use the original two-check registry.
// Extension cases below use fixture_all and exercise the actual eight migration seeds (including both removed-metadata checks).
async fn fixture() -> (Scratch, Database, Connection) {
    let fixture = fixture_all().await;
    fixture
        .2
        .execute(
            "DELETE FROM health_checks WHERE check_key!='completed_download_handling'",
            (),
        )
        .await
        .unwrap();
    fixture
}
async fn init(c: &Connection, at: i64) -> Uuid {
    let epoch = Uuid::new_v4();
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    startup_at(&tx, at, epoch).await.unwrap();
    tx.commit().await.unwrap();
    epoch
}
async fn admit(c: &Connection, scope: HealthScope, at: i64) -> Uuid {
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    let v = manual(
        &tx,
        HealthCommandInput {
            scope,
            priority: CommandPriority::Normal,
        },
        at,
    )
    .await
    .unwrap();
    let id = match v {
        HealthAdmission::Queued { command_id, .. }
        | HealthAdmission::CoalescedQueued { command_id, .. } => command_id,
        HealthAdmission::Pending {
            active_command_id, ..
        } => active_command_id,
    };
    tx.commit().await.unwrap();
    id
}
async fn take(c: &Connection, id: Uuid, at: i64) -> HealthCommand {
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    let cmd = claim(&tx, id, at).await.unwrap();
    tx.commit().await.unwrap();
    cmd
}
async fn tick(c: &Connection, at: i64) {
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    sweep(&tx, at).await.unwrap();
    tx.commit().await.unwrap();
}
async fn dirty(c: &Connection, scope: HealthScope, at: i64) {
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    mark(&tx, &selection(&tx, scope, "1").await.unwrap(), CONFIG, at)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}
async fn count(c: &Connection, sql: &str) -> i64 {
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
fn warning(m: &HealthMember, text: &str) -> Option<HealthIssue> {
    Some(HealthIssue {
        identity: m.identity.clone(),
        severity: HealthSeverity::Warning,
        reason: "test_issue".into(),
        message: text.into(),
        wiki_url: "https://example.invalid/health".into(),
        compatibility_type: "ImportMechanismCheck".into(),
    })
}
#[tokio::test]
async fn health_selected_publication_union_retry_cancel_and_generation_fences() {
    let (_s, _db, c) = fixture().await;
    init(&c, 100).await;
    let id = admit(&c, HealthScope::Tv, 101).await;
    assert_eq!(id, admit(&c, HealthScope::Movies, 102).await);
    let cmd = take(&c, id, 103).await;
    assert_eq!(cmd.members.len(), 2);
    // A request during a running batch leaves membership frozen and advances its token.
    assert_eq!(id, admit(&c, HealthScope::Tv, 104).await);
    publish(&c, &cmd, Ok(vec![None, None]), 105).await.unwrap();
    assert!(matches!(
        read(&c, id).await.unwrap().status,
        CommandStatus::Failed
    ));
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM health_checks WHERE severity IS NOT NULL"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM health_checks WHERE due_at IS NOT NULL"
        )
        .await,
        2
    );
    tick(&c, 106).await;
    let id = active(&c).await.unwrap().unwrap().id;
    let cmd = take(&c, id, 106).await;
    let results = cmd.members.iter().map(|m| warning(m, "first")).collect();
    publish(&c, &cmd, Ok(results), 107).await.unwrap();
    assert_eq!(
        count(&c, "SELECT count(*) FROM health_transitions").await,
        2
    );
    // Only TV is selected. A changed warning payload updates cache without another issue.
    let id = admit(&c, HealthScope::Tv, 108).await;
    let cmd = take(&c, id, 108).await;
    publish(&c, &cmd, Ok(vec![warning(&cmd.members[0], "changed")]), 109)
        .await
        .unwrap();
    assert_eq!(
        count(&c, "SELECT count(*) FROM health_transitions").await,
        2
    );
    let id = admit(&c, HealthScope::Tv, 110).await;
    let cmd = take(&c, id, 110).await;
    publish(&c, &cmd, Ok(vec![None]), 111).await.unwrap();
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM health_transitions WHERE kind='restored' AND message='changed'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM health_checks WHERE scope='movies' AND severity=2"
        )
        .await,
        1
    );
    // Queued cancellation uses admission ownership, retaining a later configuration marker.
    let id = admit(&c, HealthScope::Tv, 112).await;
    dirty(&c, HealthScope::Tv, 113).await;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    cancel(&tx, id, 114).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM health_checks WHERE scope='tv' AND pending_reasons>0"
        )
        .await,
        1
    );
    let id = admit(&c, HealthScope::Tv, 115).await;
    let cmd = take(&c, id, 115).await;
    publish(&c, &cmd, Err("check_timeout"), 116).await.unwrap();
    assert!(matches!(
        read(&c, id).await.unwrap().status,
        CommandStatus::RetryWait
    ));
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    cancel(&tx, id, 117).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM health_checks WHERE scope='tv' AND due_at IS NOT NULL"
        )
        .await,
        0
    );
}
#[tokio::test]
async fn health_grace_recheck_restores_previous_payload_and_announces_once() {
    let (_s, _db, c) = fixture().await;
    init(&c, 100).await;
    tick(&c, 100).await;
    let cmd = take(&c, active(&c).await.unwrap().unwrap().id, 100).await;
    publish(
        &c,
        &cmd,
        Ok(cmd.members.iter().map(|m| warning(m, "startup")).collect()),
        101,
    )
    .await
    .unwrap();
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM health_transitions WHERE in_grace=1"
        )
        .await,
        2
    );
    tick(&c, 1000).await;
    let grace = active(&c).await.unwrap().unwrap();
    assert!(grace.is_grace);
    let generations = count(&c, "SELECT sum(generation) FROM health_checks").await;
    tick(&c, 1001).await;
    assert_eq!(
        generations,
        count(&c, "SELECT sum(generation) FROM health_checks").await
    );
    let cmd = take(&c, grace.id, 1001).await;
    let results = cmd
        .members
        .iter()
        .map(|m| {
            if m.identity.scope == HealthScope::Tv {
                None
            } else {
                warning(m, "continuing")
            }
        })
        .collect();
    publish(&c, &cmd, Ok(results), 1002).await.unwrap();
    assert_eq!(lifecycle(&c).await.unwrap().grace_phase, "expired");
    assert_eq!(count(&c,"SELECT count(*) FROM health_transitions WHERE kind='restored' AND in_grace=1 AND message='startup'").await,1);
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM health_transitions WHERE kind='issue' AND in_grace=0"
        )
        .await,
        1
    );
    publish(&c, &cmd, Ok(vec![None, None]), 1003).await.unwrap();
    assert_eq!(
        count(&c, "SELECT count(*) FROM health_transitions").await,
        4
    );
    let id = admit(&c, HealthScope::Movies, 1004).await;
    let cmd = take(&c, id, 1004).await;
    publish(&c, &cmd, Ok(vec![None]), 1005).await.unwrap();
    assert_eq!(count(&c,"SELECT count(*) FROM health_transitions WHERE kind='restored' AND in_grace=0 AND message='continuing'").await,1);
}
#[tokio::test]
async fn health_restart_retry_budget_and_bounded_grace_failures() {
    let (_s, _db, c) = fixture().await;
    init(&c, 100).await;
    tick(&c, 1000).await;
    let first = take(&c, active(&c).await.unwrap().unwrap().id, 1000).await;
    assert!(first.is_grace);
    publish(&c, &first, Err("check_failed"), 1001)
        .await
        .unwrap();
    assert_eq!(read(&c, first.id).await.unwrap().attempts, 1);
    let epoch = init(&c, 1010).await;
    assert!(matches!(
        read(&c, first.id).await.unwrap().status,
        CommandStatus::Cancelled
    ));
    assert_eq!(lifecycle(&c).await.unwrap().grace_phase, "pending");
    publish(&c, &first, Ok(vec![None, None]), 1011)
        .await
        .unwrap();
    assert_eq!(
        count(&c, "SELECT count(*) FROM health_transitions").await,
        0
    );
    tick(&c, 1910).await;
    let id = active(&c).await.unwrap().unwrap().id;
    for attempt in 1..=3 {
        let cmd = take(&c, id, 1910 + attempt * 10).await;
        assert_eq!(cmd.epoch, epoch);
        assert_eq!(i64::from(cmd.attempts), attempt);
        publish(&c, &cmd, Err("check_failed"), 1911 + attempt * 10)
            .await
            .unwrap();
    }
    assert_eq!(lifecycle(&c).await.unwrap().grace_due_at, 2001);
    assert_eq!(lifecycle(&c).await.unwrap().grace_phase, "pending");
    assert!(matches!(
        read(&c, id).await.unwrap().status,
        CommandStatus::Failed
    ));
    tick(&c, 1942).await;
    assert!(active(&c).await.unwrap().is_some_and(|c| !c.is_grace));
}
#[tokio::test]
async fn health_debounce_rollback_capacity_and_grace_marker_do_not_hotloop() {
    let (_s, _db, c) = fixture().await;
    init(&c, 100).await;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    mark(
        &tx,
        &selection(&tx, HealthScope::Tv, "1").await.unwrap(),
        CONFIG,
        200,
    )
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(
        count(&c, "SELECT generation FROM health_checks WHERE scope='tv'").await,
        1
    );
    // Search has no per-target active uniqueness, so this fills the real shared pool.
    c.execute_batch("INSERT INTO series(id,tvdb_id,title,path) VALUES(1,101,'TV','/tv');INSERT INTO seasons VALUES(1,1,1);INSERT INTO episodes(id,series_id,season,number,title) VALUES(1,1,1,1,'One');").await.unwrap();
    let indexer = Uuid::new_v4().to_string();
    let client = Uuid::new_v4().to_string();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'torznab','Indexer',1,1,1,1,'http://127.0.0.1:1/')",[indexer.clone()]).await.unwrap();
    c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year) VALUES(?,'torznab','tv','[5000]','[]',0,NULL)",[indexer.clone()]).await.unwrap();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'qbittorrent','Client',1,1,1,1,'http://127.0.0.1:1/')",[client.clone()]).await.unwrap();
    c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags) VALUES(?,'qbittorrent','tv','tv',0,1,'started','default',0,0,0)",[client.clone()]).await.unwrap();
    let captured = serde_json::json!({"media_type":"tv","series_id":1,"episode_id":1,"tvdb_id":101,"title":"Boundary","season":1,"number":1,"series_type":"standard","use_scene_numbering":false}).to_string();
    for _ in 0..1024 {
        c.execute("INSERT INTO search_commands(id,mode,media_type,requested_episode_id,captured_target_json,indexer_id,indexer_revision,client_id,client_revision,next_attempt_at,created_at) VALUES(?,'automatic','tv',1,?,?,1,?,1,9007199254740000,100)", params![Uuid::new_v4().to_string(), captured.clone(), indexer.clone(), client.clone()]).await.unwrap();
    }
    tick(&c, 1000).await;
    let generation_sum = count(&c, "SELECT sum(generation) FROM health_checks").await;
    for at in 1001..1005 {
        tick(&c, at).await;
    }
    assert_eq!(
        generation_sum,
        count(&c, "SELECT sum(generation) FROM health_checks").await
    );
    assert_eq!(
        lifecycle(&c).await.unwrap().schedule_error.as_deref(),
        Some("command_history_full")
    );
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    let result = manual(
        &tx,
        HealthCommandInput {
            scope: HealthScope::All,
            priority: CommandPriority::Normal,
        },
        1005,
    )
    .await;
    assert!(matches!(
        result,
        Err(Error(StatusCode::TOO_MANY_REQUESTS, "command_history_full"))
    ));
    tx.rollback().await.unwrap();
    assert_eq!(
        generation_sum,
        count(&c, "SELECT sum(generation) FROM health_checks").await
    );
    c.execute("UPDATE search_commands SET status='cancelled',completed_at=1006 WHERE id=(SELECT id FROM search_commands LIMIT 1)",()).await.unwrap();
    tick(&c, 1006).await;
    let id = active(&c).await.unwrap().unwrap().id;
    assert!(read(&c, id).await.unwrap().is_grace);
    assert_eq!(id, admit(&c, HealthScope::All, 1007).await);
    assert_eq!(crate::commands::command_capacity(&c).await.unwrap(), 1024);
}
#[tokio::test]
async fn health_sequence_exhaustion_rolls_back_entire_publication() {
    let (_s, _db, c) = fixture().await;
    init(&c, 100).await;
    tick(&c, 100).await;
    let cmd = take(&c, active(&c).await.unwrap().unwrap().id, 100).await;
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM sqlite_sequence WHERE name='health_transitions'"
        )
        .await,
        1
    );
    // Migration43 preserves even an empty-ring high-water row; update it rather than
    // inserting duplicate sqlite_sequence metadata that no valid migration produces.
    c.execute(
        "UPDATE sqlite_sequence SET seq=? WHERE name='health_transitions'",
        [MAX_INTEGER - 1],
    )
    .await
    .unwrap();
    let result = publish(
        &c,
        &cmd,
        Ok(cmd.members.iter().map(|m| warning(m, "first")).collect()),
        101,
    )
    .await;
    assert!(matches!(
        result,
        Err(Error(_, "health_generation_exhausted"))
    ));
    assert_eq!(
        count(&c, "SELECT count(*) FROM health_transitions").await,
        0
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM health_checks WHERE severity IS NOT NULL"
        )
        .await,
        0
    );
    assert!(matches!(
        read(&c, cmd.id).await.unwrap().status,
        CommandStatus::Running
    ));
    assert_eq!(
        lifecycle(&c).await.unwrap().schedule_error.as_deref(),
        Some("health_invariant")
    );
    recover(&c, 102, "storage_error").await.unwrap();
    assert!(matches!(
        read(&c, cmd.id).await.unwrap().status,
        CommandStatus::RetryWait
    ));
    assert_eq!(
        lifecycle(&c).await.unwrap().schedule_error.as_deref(),
        Some("health_invariant"),
        "Generic recovery must retain the observable exhaustion diagnostic"
    );
    assert_eq!(
        count(&c, "SELECT count(*) FROM health_transitions").await,
        0
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM health_checks WHERE severity IS NOT NULL"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn health_normal_retry_recaptures_epoch_without_expanding_membership() {
    let (_scratch, _db, c) = fixture().await;
    init(&c, 100).await;
    let id = admit(&c, HealthScope::Tv, 101).await;
    let first = take(&c, id, 101).await;
    publish(&c, &first, Err("check_failed"), 102).await.unwrap();
    assert_eq!(id, admit(&c, HealthScope::Movies, 103).await);
    assert_eq!(read(&c, id).await.unwrap().members.len(), 1);
    let new_epoch = init(&c, 110).await;
    let second = take(&c, id, 111).await;
    assert_eq!(second.attempts, 2);
    assert_eq!(second.epoch, new_epoch);
    assert_eq!(second.members.len(), 1);
    // An old callback cannot settle the newly claimed retry, even with the same ID.
    publish(&c, &first, Ok(vec![None]), 112).await.unwrap();
    assert!(matches!(
        read(&c, id).await.unwrap().status,
        CommandStatus::Running
    ));
    publish(&c, &second, Ok(vec![None]), 113).await.unwrap();
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM health_checks WHERE scope='movies' AND pending_reasons>0"
        )
        .await,
        1
    );
    tick(&c, 114).await;
    let next = active(&c).await.unwrap().unwrap();
    assert_ne!(next.id, id);
    assert_eq!(next.members.len(), 1);
    assert_eq!(next.members[0].identity.scope, HealthScope::Movies);
}

#[tokio::test]
async fn health_config_debounce_preserves_immediate_requests_and_grace_cancel_retries() {
    let (_scratch, _db, c) = fixture().await;
    init(&c, 100).await;
    tick(&c, 100).await;
    let cmd = take(&c, active(&c).await.unwrap().unwrap().id, 100).await;
    publish(&c, &cmd, Ok(vec![None, None]), 101).await.unwrap();
    dirty(&c, HealthScope::Tv, 200).await;
    assert_eq!(
        count(&c, "SELECT due_at FROM health_checks WHERE scope='tv'").await,
        205
    );
    dirty(&c, HealthScope::Tv, 203).await;
    assert_eq!(
        count(&c, "SELECT due_at FROM health_checks WHERE scope='tv'").await,
        208
    );
    tick(&c, 207).await;
    assert!(active(&c).await.unwrap().is_none());
    let id = admit(&c, HealthScope::Tv, 207).await;
    dirty(&c, HealthScope::Tv, 208).await;
    assert_eq!(
        count(&c, "SELECT due_at FROM health_checks WHERE scope='tv'").await,
        207
    );
    let cmd = take(&c, id, 208).await;
    publish(&c, &cmd, Ok(vec![None]), 209).await.unwrap();
    tick(&c, 1000).await;
    let grace = active(&c).await.unwrap().unwrap();
    assert!(grace.is_grace);
    dirty(&c, HealthScope::Tv, 1001).await;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    cancel(&tx, grace.id, 1002).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(lifecycle(&c).await.unwrap().grace_due_at, 1062);
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM health_checks WHERE scope='tv' AND pending_reasons>0"
        )
        .await,
        1
    );
    tick(&c, 1003).await;
    let next = active(&c).await.unwrap().unwrap();
    assert!(!next.is_grace);
    let cmd = take(&c, next.id, 1003).await;
    publish(&c, &cmd, Ok(vec![None]), 1004).await.unwrap();
    tick(&c, 1062).await;
    assert!(active(&c).await.unwrap().unwrap().is_grace);
}

#[tokio::test]
async fn health_owned_probe_cancellation_timeout_and_worker_recovery() {
    let (_scratch, db, c) = fixture().await;
    let db = std::sync::Arc::new(db);
    init(&c, 100).await;
    let entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let signal = entered.clone();
    let remote = axum::Router::new().fallback(move |uri: axum::http::Uri| {
        let signal = signal.clone();
        async move {
            if uri.path().ends_with("webapiVersion") {
                return "2.8.3";
            }
            signal.notify_one();
            std::future::pending::<&'static str>().await
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, remote).await.unwrap() });
    let provider = Uuid::new_v4().to_string();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'qbittorrent','Health cancellation',1,1,1,1,?)",params![provider.clone(),endpoint]).await.unwrap();
    c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags) VALUES(?,'qbittorrent','tv','tv',0,1,'started','default',0,0,0)",[provider]).await.unwrap();
    let (_, client) = crate::providers::router_with_refresh(db.clone(), None);
    let id = admit(&c, HealthScope::Tv, 101).await;
    let cmd = take(&c, id, 101).await;
    let mut work = Box::pin(run(&db, &client, cmd));
    tokio::time::timeout(Duration::from_secs(5),async{tokio::select!{v=&mut work=>panic!("probe completed: {}",v.is_ok()),_=entered.notified()=>{}}}).await.unwrap();
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    cancel(&tx, id, 102).await.unwrap();
    tx.commit().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), &mut work)
        .await
        .unwrap()
        .unwrap();
    drop(work);
    assert!(matches!(
        read(&c, id).await.unwrap().status,
        CommandStatus::Cancelled
    ));
    let id = admit(&c, HealthScope::Tv, 103).await;
    let cmd = take(&c, id, 103).await;
    run_with_deadline(&db, &client, cmd, Duration::from_millis(50))
        .await
        .unwrap();
    assert_eq!(
        read(&c, id).await.unwrap().error_code.as_deref(),
        Some("check_timeout")
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM health_checks WHERE severity IS NOT NULL"
        )
        .await,
        0
    );
    let cmd = take(&c, id, 110).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(50), run(&db, &client, cmd))
            .await
            .is_err()
    );
    recover(&c, 111, "storage_error").await.unwrap();
    let recovered = read(&c, id).await.unwrap();
    assert!(matches!(recovered.status, CommandStatus::RetryWait));
    assert_eq!(recovered.attempts, 2);
    assert_eq!(
        count(&c, "SELECT count(*) FROM health_transitions").await,
        0
    );
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn health_unselected_cached_issue_announced_at_grace_without_freshness_claim() {
    let (_scratch, _db, c) = fixture().await;
    init(&c, 100).await;
    let old_epoch = Uuid::new_v4().to_string();
    c.execute("INSERT INTO health_checks(scope,check_key,startup,scheduled,generation,observed_generation,observed_epoch,checked_at,severity,reason,message,wiki_url,compatibility_type) VALUES('system','unselected_fixture',0,0,0,0,?,1,2,'old_issue','old issue payload','https://example.invalid','Fixture')",[old_epoch.clone()]).await.unwrap();
    tick(&c, 1000).await;
    let cmd = take(&c, active(&c).await.unwrap().unwrap().id, 1000).await;
    assert_eq!(cmd.members.len(), 2);
    publish(&c, &cmd, Ok(vec![None, None]), 1001).await.unwrap();
    assert_eq!(count(&c,"SELECT count(*) FROM health_transitions WHERE scope='system' AND in_grace=0 AND kind='issue'").await,1);
    assert_eq!(
        c.query(
            "SELECT observed_epoch FROM health_checks WHERE scope='system'",
            ()
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<String>(0)
        .unwrap(),
        old_epoch
    );
}

#[tokio::test]
async fn health_counter_exhaustion_reports_without_poisoning_shared_worker() {
    let (_scratch, db, c) = fixture().await;
    init(&c, 100).await;
    c.execute("INSERT INTO health_checks(scope,check_key,startup,scheduled,generation,compatibility_type) VALUES('system','exhausted_fixture',1,1,?,'Fixture')",[MAX_INTEGER]).await.unwrap();
    init(&c, 200).await;
    assert_eq!(
        lifecycle(&c).await.unwrap().schedule_error.as_deref(),
        Some("health_invariant")
    );
    let before = count(
        &c,
        "SELECT sum(generation) FROM health_checks WHERE scope!='system'",
    )
    .await;
    tick(&c, 22000).await;
    assert_eq!(
        before,
        count(
            &c,
            "SELECT sum(generation) FROM health_checks WHERE scope!='system'"
        )
        .await
    );
    assert_eq!(lifecycle(&c).await.unwrap().next_scheduled_at, 21700);
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM health_checks WHERE severity IS NOT NULL"
        )
        .await,
        0
    );
    let id = Uuid::new_v4().to_string();
    c.execute("INSERT INTO quality_reset_commands(id,media_type,reset_titles,status,next_attempt_at,created_at) VALUES(?,'tv',0,'queued',0,0)",[id.clone()]).await.unwrap();
    let db = std::sync::Arc::new(db);
    let (_, client) = crate::providers::router_with_refresh(db.clone(), None);
    let runtime = crate::commands::start(db.clone(), client).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = c
                .query(
                    "SELECT status FROM quality_reset_commands WHERE id=?",
                    [id.clone()],
                )
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap()
                .get::<String>(0)
                .unwrap();
            if status == "succeeded" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    runtime.shutdown().await;
    assert_eq!(
        lifecycle(&c).await.unwrap().schedule_error.as_deref(),
        Some("health_invariant")
    );
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    assert!(matches!(
        manual(
            &tx,
            HealthCommandInput {
                scope: HealthScope::All,
                priority: CommandPriority::Normal
            },
            22001
        )
        .await,
        Err(Error(_, "health_generation_exhausted"))
    ));
    tx.rollback().await.unwrap();
}

fn mixed(cmd: &HealthCommand, issue_present: bool) -> Vec<CheckOutcome> {
    cmd.members
        .iter()
        .map(|m| {
            if m.identity.check_key == "download_client_communication" {
                Ok(if issue_present {
                    warning(m, "communication fixture").map(|mut v| {
                        v.severity = HealthSeverity::Error;
                        v.compatibility_type = "DownloadClientCheck".into();
                        v
                    })
                } else {
                    None
                })
            } else if matches!(
                m.identity.check_key.as_str(),
                "download_client_root_folder" | "removed_metadata"
            ) {
                Ok(None)
            } else {
                Err("check_failed")
            }
        })
        .collect()
}

#[tokio::test]
async fn communication_partial_attempts_preserve_failed_payload_and_retry_membership() {
    let (_scratch, _db, c) = fixture_all().await;
    init(&c, 100).await;
    let baseline = admit(&c, HealthScope::All, 100).await;
    let cmd = take(&c, baseline, 100).await;
    publish(
        &c,
        &cmd,
        Ok(cmd
            .members
            .iter()
            .map(|m| {
                if m.identity.check_key == "completed_download_handling" {
                    warning(m, "previous CDH")
                } else {
                    None
                }
            })
            .collect()),
        101,
    )
    .await
    .unwrap();
    let id = admit(&c, HealthScope::All, 102).await;
    for attempt in 1..=3 {
        let cmd = take(&c, id, 110 + attempt * 10).await;
        // Registry now contains four checks per domain; retry must retain all eight.
        assert_eq!(cmd.members.len(), 8);
        assert_eq!(i64::from(cmd.attempts), attempt);
        publish_outcomes(&c, &cmd, mixed(&cmd, attempt != 2), 111 + attempt * 10)
            .await
            .unwrap();
        // Each retry captures every member with nonempty reasons, including successful ones.
        assert_eq!(count(&c,"SELECT count(*) FROM health_checks WHERE check_key='completed_download_handling' AND message='previous CDH' AND last_error='check_failed'").await,2);
        assert_eq!(count(&c,"SELECT count(*) FROM health_checks WHERE check_key='download_client_communication' AND last_error IS NULL").await,2);
        assert_eq!(
            count(
                &c,
                "SELECT count(*) FROM health_checks WHERE pending_reasons>0"
            )
            .await,
            if attempt < 3 { 8 } else { 0 }
        );
        assert_eq!(
            lifecycle(&c).await.unwrap().last_batch_completed_at,
            Some(101)
        );
        // Replayed callbacks cannot duplicate an already settled attempt.
        publish_outcomes(&c, &cmd, mixed(&cmd, true), 112 + attempt * 10)
            .await
            .unwrap();
    }
    assert!(matches!(
        read(&c, id).await.unwrap().status,
        CommandStatus::Failed
    ));
    let mut rows=c.query("SELECT command_attempt,kind FROM health_transitions WHERE command_id=? AND scope='tv' ORDER BY sequence",[id.to_string()]).await.unwrap();
    for (attempt, kind) in [(1, "issue"), (2, "restored"), (3, "issue")] {
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get::<i64>(0).unwrap(), attempt);
        assert_eq!(row.get::<String>(1).unwrap(), kind);
    }
    assert!(rows.next().await.unwrap().is_none());
}

#[tokio::test]
async fn communication_partial_grace_exhaustion_preserves_other_reasons_and_later_completion() {
    let (_scratch, _db, c) = fixture_all().await;
    init(&c, 100).await;
    tick(&c, 1000).await;
    let id = active(&c).await.unwrap().unwrap().id;
    for attempt in 1..=3 {
        let cmd = take(&c, id, 1000 + attempt * 10).await;
        assert!(cmd.is_grace);
        publish_outcomes(&c, &cmd, mixed(&cmd, true), 1001 + attempt * 10)
            .await
            .unwrap();
        assert_ne!(lifecycle(&c).await.unwrap().grace_phase, "expired");
        assert_eq!(
            count(
                &c,
                "SELECT count(*) FROM health_transitions WHERE in_grace=0"
            )
            .await,
            0
        );
    }
    assert_eq!(lifecycle(&c).await.unwrap().grace_due_at, 1091);
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM health_checks WHERE pending_reasons=1"
        )
        .await,
        8 // Both removed-metadata seeds also retain pending reasons; all registered checks retain their pending reasons.
    );
    tick(&c, 1032).await;
    let ordinary = take(&c, active(&c).await.unwrap().unwrap().id, 1032).await;
    assert!(!ordinary.is_grace);
    publish(
        &c,
        &ordinary,
        Ok(ordinary
            .members
            .iter()
            .map(|m| {
                if m.identity.check_key == "download_client_communication" {
                    warning(m, "continuing")
                } else {
                    None
                }
            })
            .collect()),
        1033,
    )
    .await
    .unwrap();
    tick(&c, 1091).await;
    let grace = take(&c, active(&c).await.unwrap().unwrap().id, 1091).await;
    publish(
        &c,
        &grace,
        Ok(grace
            .members
            .iter()
            .map(|m| {
                if m.identity.check_key == "download_client_communication" {
                    warning(m, "continuing")
                } else {
                    None
                }
            })
            .collect()),
        1092,
    )
    .await
    .unwrap();
    assert_eq!(lifecycle(&c).await.unwrap().grace_phase, "expired");
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM health_transitions WHERE in_grace=0 AND kind='issue'"
        )
        .await,
        2
    );
}

#[tokio::test]
// Stale-input settlement deliberately preserves every pending selection for a fresh batch,
// including unchanged members: no part of that invalidated attempt was published.
async fn communication_status_deadline_rollback_and_whole_attempt_invalidation() {
    let (_scratch, _db, c) = fixture_all().await;
    init(&c, 100).await;
    let id = admit(&c, HealthScope::All, 100).await;
    let cmd = take(&c, id, 100).await;
    publish(&c, &cmd, Ok(vec![None; 8]), 101).await.unwrap();
    for at in 200..212 {
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .unwrap();
        communication_status_at(&tx, MediaDomain::Tv, at)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(count(&c,"SELECT due_at FROM health_checks WHERE scope='tv' AND check_key='download_client_communication'").await,205);
        tick(&c, at).await;
    }
    let queued = active(&c).await.unwrap().unwrap();
    assert_eq!(queued.created_at, 205);
    assert_eq!(queued.members.len(), 1);
    let generation=count(&c,"SELECT generation FROM health_checks WHERE scope='tv' AND check_key='download_client_communication'").await;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    communication_status_at(&tx, MediaDomain::Tv, 212)
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(count(&c,"SELECT generation FROM health_checks WHERE scope='tv' AND check_key='download_client_communication'").await,generation);
    // Coalesce all eight while queued, then change one input while the batch is running.
    assert_eq!(admit(&c, HealthScope::All, 212).await, queued.id);
    let cmd = take(&c, queued.id, 212).await;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    communication_status_at(&tx, MediaDomain::Tv, 213)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    publish_outcomes(&c, &cmd, mixed(&cmd, true), 214)
        .await
        .unwrap();
    assert_eq!(
        count(&c, "SELECT count(*) FROM health_transitions").await,
        0
    );
    assert_eq!(
        read(&c, cmd.id).await.unwrap().error_code.as_deref(),
        Some("stale_inputs")
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM health_checks WHERE pending_reasons>0"
        )
        .await,
        8 // Both removed-metadata seeds also retain pending reasons; all registered checks retain their pending reasons.
    );
}

#[tokio::test]
async fn communication_status_exhaustion_preserves_authoritative_fact_and_reports() {
    let (_scratch, _db, c) = fixture_all().await;
    // Fresh fixture, before membership exists; seed the actual selected key at exhaustion.
    c.execute(
        "DELETE FROM health_checks WHERE scope='tv' AND check_key='download_client_communication'",
        (),
    )
    .await
    .unwrap();
    c.execute("INSERT INTO health_checks(scope,check_key,startup,scheduled,generation,compatibility_type) VALUES('tv','download_client_communication',1,1,?,'DownloadClientCheck')",[MAX_INTEGER]).await.unwrap();
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    tx.execute("CREATE TABLE status_fact(value INTEGER NOT NULL)", ())
        .await
        .unwrap();
    tx.execute("INSERT INTO status_fact VALUES(1)", ())
        .await
        .unwrap();
    communication_status_at(&tx, MediaDomain::Tv, 200)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(count(&c, "SELECT value FROM status_fact").await, 1);
    assert_eq!(
        lifecycle(&c).await.unwrap().schedule_error.as_deref(),
        Some("health_invariant")
    );
    assert_eq!(count(&c,"SELECT count(*) FROM health_checks WHERE observed_generation IS NOT NULL OR pending_reasons!=0").await,0);
}

async fn communication_client(c: &Connection, endpoint: &str) {
    let provider = Uuid::new_v4().to_string();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'qbittorrent','Communication fixture',1,1,1,1,?)",params![provider.clone(),endpoint]).await.unwrap();
    for domain in ["tv", "movies"] {
        c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags) VALUES(?,'qbittorrent',?,?,0,1,'started','default',0,0,0)",params![provider.clone(),domain,domain]).await.unwrap();
    }
}

#[tokio::test]
async fn communication_real_tv_and_all_failure_is_visible_despite_cdh_failure() {
    for scope in [HealthScope::Tv, HealthScope::All] {
        let (_scratch, db, c) = fixture_all().await;
        init(&c, 100).await;
        let remote = axum::Router::new().fallback(|| async { StatusCode::INTERNAL_SERVER_ERROR });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        communication_client(&c, &format!("http://{}", listener.local_addr().unwrap())).await;
        let server = tokio::spawn(async move { axum::serve(listener, remote).await.unwrap() });
        let db = std::sync::Arc::new(db);
        let (_, client) = crate::providers::router_with_refresh(db.clone(), None);
        let id = admit(&c, scope, 100).await;
        let cmd = take(&c, id, 100).await;
        publish(
            &c,
            &cmd,
            Ok(cmd
                .members
                .iter()
                .map(|m| {
                    if m.identity.check_key == "completed_download_handling" {
                        warning(m, "previous")
                    } else {
                        None
                    }
                })
                .collect()),
            101,
        )
        .await
        .unwrap();
        let id = admit(&c, scope, 102).await;
        let cmd = take(&c, id, 102).await;
        run(&db, &client, cmd).await.unwrap();
        assert!(matches!(
            read(&c, id).await.unwrap().status,
            CommandStatus::RetryWait
        ));
        assert_eq!(count(&c,"SELECT count(*) FROM health_checks WHERE check_key='download_client_communication' AND severity=3 AND last_error IS NULL").await,if scope==HealthScope::All {2}else{1});
        assert_eq!(count(&c,"SELECT count(*) FROM health_checks WHERE scope='tv' AND check_key='completed_download_handling' AND message='previous' AND last_error='check_failed'").await,1);
        // Direct probes neither record provider tests nor dirty their own generations.
        assert_eq!(count(&c, "SELECT count(*) FROM provider_tests").await, 0);
        assert_eq!(count(&c,"SELECT count(*) FROM health_checks WHERE generation!=observed_generation AND check_key='download_client_communication' AND observed_generation IS NOT NULL").await,0);
        server.abort();
        let _ = server.await;
    }
}

#[tokio::test]
async fn communication_aggregate_deadline_retains_completed_results_and_releases_probe() {
    let (_scratch, db, c) = fixture_all().await;
    init(&c, 100).await;
    // A later registered fixture must remain explicitly unevaluated at aggregate expiry.
    c.execute("INSERT INTO health_checks(scope,check_key,startup,scheduled,compatibility_type) VALUES('tv','zzz_unvisited_fixture',1,1,'Fixture')",()).await.unwrap();

    let entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let signal = entered.clone();
    let remote = axum::Router::new().fallback(move |uri: axum::http::Uri| {
        let signal = signal.clone();
        async move {
            if uri.path().ends_with("webapiVersion") {
                return "2.8.3";
            }
            if uri.path().ends_with("torrents/info") {
                return "[]";
            }
            signal.notify_one();
            std::future::pending::<&'static str>().await
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    communication_client(&c, &format!("http://{}", listener.local_addr().unwrap())).await;
    let server = tokio::spawn(async move { axum::serve(listener, remote).await.unwrap() });
    let db = std::sync::Arc::new(db);
    let (_, client) = crate::providers::router_with_refresh(db.clone(), None);
    let id = admit(&c, HealthScope::Tv, 101).await;
    let cmd = take(&c, id, 101).await;
    let work = run_with_deadline(&db, &client, cmd, Duration::from_secs(2));
    tokio::pin!(work);
    tokio::select! {r=&mut work=>panic!("did not reach blocked CDH: {}",r.is_ok()),_=entered.notified()=>{}}
    work.await.unwrap();
    assert_eq!(
        read(&c, id).await.unwrap().error_code.as_deref(),
        Some("check_timeout")
    );
    assert_eq!(count(&c,"SELECT count(*) FROM health_checks WHERE scope='tv' AND check_key='download_client_communication' AND severity=0 AND last_error IS NULL").await,1);
    assert_eq!(count(&c,"SELECT count(*) FROM health_checks WHERE scope='tv' AND check_key='completed_download_handling' AND observed_generation IS NULL AND last_error='check_timeout'").await,1);
    assert_eq!(count(&c,"SELECT count(*) FROM health_checks WHERE check_key='zzz_unvisited_fixture' AND observed_generation IS NULL AND last_error='check_timeout'").await,1);
    // The deadline-dropped CDH future releases the same-provider permit.
    assert!(matches!(
        crate::health_detectors::evaluate_communication(&db, &client, MediaDomain::Tv).await,
        Ok(None)
    ));
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn communication_partial_settlement_rollback_and_new_generation_cancel() {
    let (_scratch, _db, c) = fixture_all().await;
    init(&c, 100).await;
    let id = admit(&c, HealthScope::All, 100).await;
    let first = take(&c, id, 100).await;
    let mut invalid = mixed(&first, true);
    // Late storage failure must roll back an earlier completed member and its transition.
    // Select the last communication payload explicitly: the newer root member is OK.
    let last_communication = first
        .members
        .iter()
        .rposition(|m| m.identity.check_key == "download_client_communication")
        .unwrap();
    invalid
        .get_mut(last_communication)
        .unwrap()
        .as_mut()
        .unwrap()
        .as_mut()
        .unwrap()
        .message = "x".repeat(4097);
    assert!(publish_outcomes(&c, &first, invalid, 101).await.is_err());
    assert_eq!(
        count(&c, "SELECT count(*) FROM health_transitions").await,
        0
    );
    assert_eq!(count(&c,"SELECT count(*) FROM health_checks WHERE observed_generation IS NOT NULL OR last_error IS NOT NULL").await,0);
    assert!(matches!(
        read(&c, id).await.unwrap().status,
        CommandStatus::Running
    ));
    publish_outcomes(&c, &first, mixed(&first, true), 102)
        .await
        .unwrap();
    // A new TV request belongs to a later generation and cannot be cancelled with old admission.
    assert_eq!(admit(&c, HealthScope::Tv, 103).await, id);
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    cancel(&tx, id, 104).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM health_checks WHERE scope='tv' AND pending_reasons>0"
        )
        .await,
        4 // Removed metadata adds a fourth TV check to the scoped request.
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM health_checks WHERE scope='movies' AND pending_reasons>0"
        )
        .await,
        0
    );
    publish_outcomes(&c, &first, mixed(&first, false), 105)
        .await
        .unwrap();
    assert_eq!(count(&c,"SELECT count(*) FROM health_checks WHERE check_key='download_client_communication' AND severity=3").await,2);
    assert_eq!(
        count(&c, "SELECT count(*) FROM health_transitions").await,
        2
    );
    tick(&c, 106).await;
    let next = active(&c).await.unwrap().unwrap();
    assert_ne!(next.id, id);
    assert_eq!(next.members.len(), 4); // All four newly requested TV checks survive.
    assert!(
        next.members
            .iter()
            .all(|m| m.identity.scope == HealthScope::Tv)
    );
}

fn removed_identity(scope: HealthScope) -> HealthIdentity {
    HealthIdentity {
        scope,
        check_key: "removed_metadata".into(),
    }
}

#[tokio::test]
async fn removed_metadata_counts_library_owners_and_bounds_snapshot_messages() {
    let (_scratch, db, c) = fixture_all().await;
    c.execute_batch("INSERT INTO series(id,title,path,monitored,status) VALUES(1,'Missing source','/tv1',0,'deleted'),(2,'Unknown','/tv2',0,NULL); INSERT INTO movie_metadata(id,title,status) VALUES(1,'Orphan','deleted'),(2,'Library movie','deleted'),(3,'Unknown',NULL); INSERT INTO movies(id,metadata_id,path,monitored) VALUES(1,2,'/movie1',0),(2,3,'/movie2',0);").await.unwrap();
    for (scope, title, source, compatibility) in [
        (
            HealthScope::Tv,
            "Missing source",
            "TVDB",
            "RemovedSeriesCheck",
        ),
        (
            HealthScope::Movies,
            "Library movie",
            "TMDb",
            "RemovedMovieCheck",
        ),
    ] {
        let issue = removed_metadata::evaluate(&db, &removed_identity(scope))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(issue.severity, HealthSeverity::Error);
        assert!(issue.reason.ends_with("_single"));
        assert_eq!(issue.compatibility_type, compatibility);
        assert!(issue.message.starts_with("1 library"));
        assert!(issue.message.contains(title));
        assert!(issue.message.contains(&format!("{source} ID unavailable")));
        assert!(!issue.message.contains("Orphan"));
        assert!(!issue.message.contains("Unknown"));
    }
    c.execute_batch("INSERT INTO movie_metadata(id,tmdb_id,title,status) VALUES(4,404,'Second movie','deleted'); INSERT INTO movies(id,metadata_id,path,monitored) VALUES(4,4,'/movie4',0);").await.unwrap();
    let multiple = removed_metadata::evaluate(&db, &removed_identity(HealthScope::Movies))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(multiple.reason, "removed_movie_multiple");
    assert!(multiple.message.starts_with("2 library movies"));
    assert!(multiple.message.contains("TMDb ID 404"));
    assert!(
        multiple.message.find("Library movie").unwrap()
            < multiple.message.find("Second movie").unwrap()
    );
    c.execute("DELETE FROM movies WHERE id=4", ())
        .await
        .unwrap();
    c.execute(
        "UPDATE series SET title=? WHERE id=1",
        ["\0hidden title suffix"],
    )
    .await
    .unwrap();
    let nul = removed_metadata::evaluate(&db, &removed_identity(HealthScope::Tv))
        .await
        .unwrap()
        .unwrap();
    assert!(nul.message.contains("(untitled) [title truncated]"));
    assert!(!nul.message.contains('\0'));
    // Huge direct/snapshot data is projected to <=161 characters by SQL, then clipped by bytes.
    let huge = format!("\n{}\tTAIL", "🦀".repeat(100_000));
    for id in 3..=20 {
        c.execute("INSERT INTO series(id,tvdb_id,title,path,status,monitored) VALUES(?,?,?,?, 'deleted',0)",params![id, 1000+id, huge.clone(),format!("/tv{id}")]).await.unwrap();
    }
    let issue = removed_metadata::evaluate(&db, &removed_identity(HealthScope::Tv))
        .await
        .unwrap()
        .unwrap();
    assert!(issue.message.starts_with("19 library series"));
    assert_eq!(issue.reason, "removed_series_multiple");
    assert!(issue.message.contains("3 additional titles omitted"));
    assert!(issue.message.contains("[title truncated]"));
    assert!(issue.message.contains("TVDB ID 1017"));
    assert!(!issue.message.contains("TVDB ID 1018"));
    assert!(!issue.message.chars().any(char::is_control));
    assert!(issue.message.len() <= 4096);
    c.execute("DELETE FROM movies WHERE metadata_id=2", ())
        .await
        .unwrap();
    assert!(
        removed_metadata::evaluate(&db, &removed_identity(HealthScope::Movies))
            .await
            .unwrap()
            .is_none()
    );
    // A failed database read must not become an empty, healthy observation.
    c.execute("ALTER TABLE series RENAME TO series_unavailable", ())
        .await
        .unwrap();
    assert_eq!(
        removed_metadata::evaluate(&db, &removed_identity(HealthScope::Tv))
            .await
            .unwrap_err(),
        "storage_error"
    );
}

async fn removed_outcomes(db: &Database, cmd: &HealthCommand) -> Vec<CheckOutcome> {
    let mut outcomes = Vec::new();
    for member in &cmd.members {
        outcomes.push(if member.identity.check_key == "removed_metadata" {
            removed_metadata::evaluate(db, &member.identity).await
        } else {
            Ok(None)
        });
    }
    outcomes
}

#[tokio::test]
async fn removed_metadata_rejects_stale_healthy_and_error_snapshots_and_rolls_back() {
    let (_scratch, db, c) = fixture_all().await;
    c.execute_batch("INSERT INTO series(id,tvdb_id,title,path,status) VALUES(1,101,'TV original','/tv','continuing'); INSERT INTO movie_metadata(id,tmdb_id,title,status) VALUES(1,202,'Movie original','released'); INSERT INTO movies(id,metadata_id,path) VALUES(1,1,'/movie');").await.unwrap();
    init(&c, 100).await;
    let first = take(&c, admit(&c, HealthScope::All, 100).await, 100).await;
    let healthy = removed_outcomes(&db, &first).await;
    c.execute_batch(
        "UPDATE series SET status='deleted'; UPDATE movie_metadata SET status='deleted';",
    )
    .await
    .unwrap();
    publish_outcomes(&c, &first, healthy, 101).await.unwrap();
    assert_eq!(
        read(&c, first.id).await.unwrap().error_code.as_deref(),
        Some("stale_inputs")
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM health_checks WHERE observed_generation IS NOT NULL"
        )
        .await,
        0
    );
    let current = take(&c, admit(&c, HealthScope::All, 102).await, 102).await;
    let errors = removed_outcomes(&db, &current).await;
    publish_outcomes(&c, &current, errors, 103).await.unwrap();
    assert_eq!(count(&c,"SELECT count(*) FROM health_checks WHERE check_key='removed_metadata' AND severity=3 AND observed_generation=generation").await,2);
    let stale = take(&c, admit(&c, HealthScope::All, 104).await, 104).await;
    let old_errors = removed_outcomes(&db, &stale).await;
    c.execute_batch("UPDATE series SET title='TV restored',status='continuing'; UPDATE movie_metadata SET title='Movie restored',status='released';").await.unwrap();
    publish_outcomes(&c, &stale, old_errors, 105).await.unwrap();
    assert_eq!(
        read(&c, stale.id).await.unwrap().error_code.as_deref(),
        Some("stale_inputs")
    );
    assert_eq!(count(&c,"SELECT count(*) FROM health_checks WHERE check_key='removed_metadata' AND severity=3 AND observed_generation<generation").await,2);
    let restored = take(&c, admit(&c, HealthScope::All, 106).await, 106).await;
    // Fail the final command settlement, after health rows/transitions were written.
    c.execute_batch("CREATE TRIGGER reject_health_settlement BEFORE UPDATE OF status ON health_commands WHEN NEW.status='succeeded' BEGIN SELECT RAISE(ABORT,'fixture late settlement'); END;").await.unwrap();
    assert!(
        publish_outcomes(&c, &restored, removed_outcomes(&db, &restored).await, 107)
            .await
            .is_err()
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM health_transitions WHERE kind='restored'"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM health_checks WHERE check_key='removed_metadata' AND severity=3"
        )
        .await,
        2
    );
    c.execute("DROP TRIGGER reject_health_settlement", ())
        .await
        .unwrap();
    publish_outcomes(&c, &restored, removed_outcomes(&db, &restored).await, 108)
        .await
        .unwrap();
    assert_eq!(count(&c,"SELECT count(*) FROM health_checks WHERE check_key='removed_metadata' AND severity=0 AND observed_generation=generation").await,2);
    assert_eq!(count(&c,"SELECT count(*) FROM health_transitions WHERE kind='restored' AND (message LIKE '%TV original%' OR message LIKE '%Movie original%')").await,2);
    // Source mutation rollback restores the exact generation along with authoritative status.
    let generation = count(
        &c,
        "SELECT generation FROM health_checks WHERE scope='tv' AND check_key='removed_metadata'",
    )
    .await;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    tx.execute("UPDATE series SET status='deleted'", ())
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(
        count(
            &c,
            "SELECT generation FROM health_checks WHERE scope='tv' AND check_key='removed_metadata'"
        )
        .await,
        generation
    );
    assert!(
        removed_metadata::evaluate(&db, &removed_identity(HealthScope::Tv))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn removed_metadata_scheduled_dispatch_and_invalid_identity_preserve_prior_issue() {
    let (_scratch, db, c) = fixture_all().await;
    let db = std::sync::Arc::new(db);
    let (_, client) = crate::providers::router_with_refresh(db.clone(), None);
    c.execute_batch("INSERT INTO series(id,tvdb_id,title,path,status,monitored) VALUES(1,101,'Original TV','/tv','deleted',0); INSERT INTO movie_metadata(id,tmdb_id,title,status) VALUES(1,202,'Original movie','deleted'); INSERT INTO movies(id,metadata_id,path,monitored) VALUES(1,1,'/movie',0);").await.unwrap();
    init(&c, 100).await;
    // Exercise the actual dispatcher on startup and the existing six-hour sweep.
    for timestamp in [100, 21700] {
        tick(&c, timestamp).await;
        let queued = active(&c).await.unwrap().unwrap();
        let cmd = take(&c, queued.id, timestamp).await;
        let mut results = Vec::new();
        for member in &cmd.members {
            if timestamp == 21700 {
                assert_ne!(member.captured_reasons.unwrap() & SCHEDULED, 0);
            }
            results.push(evaluate(&db, &client, member).await);
        }
        publish_outcomes(&c, &cmd, results, timestamp + 1)
            .await
            .unwrap();
        assert_eq!(count(&c,"SELECT count(*) FROM health_checks WHERE check_key='removed_metadata' AND severity=3 AND last_error IS NULL AND observed_generation=generation").await,2);
    }
    // SQLite's affinity/check semantics can admit nonnumeric text. Never decode it as
    // a native-ID fallback or fabricate a healthy observation; retain the last snapshot.
    c.execute_batch(
        "UPDATE series SET tvdb_id='invalid'; UPDATE movie_metadata SET tmdb_id='invalid';",
    )
    .await
    .unwrap();
    let cmd = take(&c, admit(&c, HealthScope::All, 21702).await, 21702).await;
    let results = removed_outcomes(&db, &cmd).await;
    for (member, result) in cmd.members.iter().zip(&results) {
        if member.identity.check_key == "removed_metadata" {
            assert_eq!(result.as_ref().unwrap_err(), &"check_failed");
        }
    }
    publish_outcomes(&c, &cmd, results, 21703).await.unwrap();
    assert_eq!(count(&c,"SELECT count(*) FROM health_checks WHERE check_key='removed_metadata' AND severity=3 AND last_error='check_failed' AND observed_generation<generation AND (message LIKE '%TVDB ID 101%' OR message LIKE '%TMDb ID 202%')").await,2);
    // Corrupt legacy nonpositive IDs also fail; constraints are bypassed only on this
    // fixture connection to exercise defensive reading of an old/imported database.
    c.execute_batch("PRAGMA ignore_check_constraints=ON; UPDATE series SET tvdb_id=0; UPDATE movie_metadata SET tmdb_id=-1; PRAGMA ignore_check_constraints=OFF;").await.unwrap();
    for scope in [HealthScope::Tv, HealthScope::Movies] {
        assert_eq!(
            removed_metadata::evaluate(&db, &removed_identity(scope))
                .await
                .unwrap_err(),
            "check_failed"
        );
    }
}
