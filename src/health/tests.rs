use super::*;
struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.0) {
            eprintln!("health scratch cleanup: {e}")
        }
    }
}
async fn fixture() -> (Scratch, Database, Connection) {
    let path = std::env::temp_dir().join(format!("hrrdarr-health-{}", Uuid::new_v4()));
    std::fs::create_dir(&path).unwrap();
    let db = Database::open_local(path.join("db")).await.unwrap();
    let c = db.connect().await.unwrap();
    (Scratch(path), db, c)
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
    c.execute(
        "INSERT INTO sqlite_sequence(name,seq) VALUES('health_transitions',?)",
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
