use super::*;
use crate::providers::{RefreshClient, RefreshError};
use std::{
    collections::HashSet,
    sync::{LazyLock, Mutex},
    time::Duration,
};
use tokio::task::JoinHandle;

static OWNERS: LazyLock<Mutex<HashSet<usize>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
struct Owner(Option<Arc<Database>>);
impl Drop for Owner {
    fn drop(&mut self) {
        if let Some(db) = self.0.take() {
            let key = Arc::as_ptr(&db) as usize;
            drop(db);
            match OWNERS.lock() {
                Ok(mut owners) => {
                    owners.remove(&key);
                }
                Err(_) => eprintln!("event=command_owner_error code=poisoned"),
            }
        }
    }
}
/// Dropping aborts the task; shutdown also waits until its DB ownership and HTTP operation drop.
/// A cancelled in-flight read remains durable and is recovered at the next start.
pub struct Runtime {
    task: Option<JoinHandle<()>>,
}
impl Runtime {
    pub async fn shutdown(mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

pub async fn start(db: Arc<Database>, client: RefreshClient) -> Result<Runtime> {
    let metadata = Arc::new(
        crate::metadata::MetadataClient::new()
            .map_err(|_| Error(StatusCode::SERVICE_UNAVAILABLE, "metadata_unavailable"))?,
    );
    start_with_metadata(db, client, metadata).await
}

pub async fn start_with_metadata(
    db: Arc<Database>,
    client: RefreshClient,
    metadata: Arc<crate::metadata::MetadataClient>,
) -> Result<Runtime> {
    if !db.permits_local_imports() {
        return Err(Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "command_local_ownership_required",
        ));
    }
    if !client.matches_database(&db) {
        return Err(Error(StatusCode::CONFLICT, "command_database_mismatch"));
    }
    {
        let mut owners = OWNERS
            .lock()
            .map_err(|_| Error(StatusCode::SERVICE_UNAVAILABLE, "command_owner_unavailable"))?;
        if !owners.insert(Arc::as_ptr(&db) as usize) {
            return Err(Error(StatusCode::CONFLICT, "command_worker_active"));
        }
    }
    let owner = Owner(Some(db.clone()));
    tokio::time::timeout(Duration::from_secs(10), recover(&db, "interrupted"))
        .await
        .map_err(|_| Error(StatusCode::SERVICE_UNAVAILABLE, "command_storage_timeout"))??;
    tokio::time::timeout(Duration::from_secs(10), recover_rss(&db, "interrupted"))
        .await
        .map_err(|_| Error(StatusCode::SERVICE_UNAVAILABLE, "command_storage_timeout"))??;
    let task = tokio::spawn(async move {
        let _owner = owner;
        // ponytail: one worker per locally owned DB; add weighted concurrency only with real jobs needing it.
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            let result =
                tokio::time::timeout(Duration::from_secs(45), step(&db, &client, &metadata)).await;
            if !matches!(result, Ok(Ok(()))) {
                eprintln!("event=command_worker_error code=storage_error");
                if !matches!(
                    tokio::time::timeout(
                        Duration::from_secs(10),
                        recover_rss(&db, "storage_error")
                    )
                    .await,
                    Ok(Ok(()))
                ) {
                    eprintln!("event=rss_recovery_error code=storage_error");
                }
                // The failed future is gone. External GETs can repeat; metadata facts and success
                // commit together; local clears likewise commit tombstones and success together.
                // Only uncommitted local effects retry. RSS recovery separately fences
                // dispatched submissions into read-only reconciliation; attempts never reset.
                if !matches!(
                    tokio::time::timeout(Duration::from_secs(10), recover(&db, "storage_error"))
                        .await,
                    Ok(Ok(()))
                ) {
                    eprintln!("event=command_recovery_error code=storage_error");
                }
            }
        }
    });
    Ok(Runtime { task: Some(task) })
}
async fn recover_rss(db: &Database, code: &str) -> Result<()> {
    rss::recover(&connection(db).await?, now()?, code).await
}
async fn recover(db: &Database, code: &str) -> Result<()> {
    let c = connection(db).await?;
    let timestamp = now()?;
    c.execute("UPDATE commands SET status=CASE WHEN attempts<3 THEN 'retry_wait' ELSE 'failed' END,next_attempt_at=?,completed_at=CASE WHEN attempts<3 THEN NULL ELSE ? END,error_code=? WHERE status='running'",params![timestamp,timestamp,code]).await?;
    metadata::recover(&c, timestamp, code).await?;
    blocklist::recover(&c, timestamp, code).await?;
    processing::recover(&c, timestamp, code).await?;
    search::recover(&c, timestamp, code).await?;
    manual_import::recover(&c, timestamp, code).await?;
    quality_reset::recover(&c, timestamp, code).await?;
    Ok(())
}
async fn schedule_due(c: &Connection, timestamp: i64) -> Result<()> {
    let mut rows=c.query(&format!("SELECT {SCHEDULE_COLUMNS} FROM download_refresh_schedules WHERE enabled=1 AND next_run_at<=? ORDER BY next_run_at,provider_id,media_type LIMIT 64"),[timestamp]).await?;
    let mut due = Vec::new();
    while let Some(row) = rows.next().await? {
        due.push(schedule_row(row)?)
    }
    drop(rows);
    for schedule in due {
        let result = enqueue(
            c,
            CommandInput {
                name: CommandName::RefreshDownloads,
                target: schedule.target,
                provider_revision: schedule.provider_revision,
                priority: CommandPriority::Normal,
            },
            timestamp,
        )
        .await;
        let error = match result {
            Ok(_) => None,
            Err(Error(_, "command_history_full")) => Some("command_history_full"),
            Err(Error(_, "command_conflict")) => Some("command_conflict"),
            Err(Error(_, "provider_changed")) => Some("provider_changed"),
            Err(e) => return Err(e),
        };
        c.execute("UPDATE download_refresh_schedules SET next_run_at=?,last_run_at=CASE WHEN ? IS NULL THEN ? ELSE last_run_at END,error_code=?,enabled=CASE WHEN ?='provider_changed' THEN 0 ELSE enabled END WHERE provider_id=? AND media_type=?",params![timestamp+i64::from(schedule.interval_seconds),error,timestamp,error,error,schedule.target.provider_id.to_string(),domain(schedule.target.media_type)]).await?;
    }
    Ok(())
}
enum Claimed {
    Downloads(Command),
    Metadata(metadata::MetadataCommand),
    Blocklist(blocklist::BlocklistClearCommand),
    Rss(rss::RssCommand),
    RssCandidate(Uuid),
    Processing(processing::DownloadProcessing),
    Search(search::SearchCommand),
    ManualImport(manual_import::ManualImportCommand),
    QualityReset(quality_reset::QualityResetCommand),
}
async fn claim(db: &Database) -> Result<Option<Claimed>> {
    let c = connection(db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let result=async{
        let timestamp=now()?;
        tx.execute("UPDATE commands SET status='failed',completed_at=?,error_code='provider_changed' WHERE status IN ('queued','retry_wait') AND NOT EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=commands.provider_id AND p.revision=commands.provider_revision AND p.enabled=1 AND p.implementation='qbittorrent' AND s.media_type=commands.media_type)",[timestamp]).await?;
        schedule_due(&tx,timestamp).await?;
        metadata::sweep(&tx,timestamp).await?;
        rss::schedule_due(&tx,timestamp).await?;
        let row=tx.query("SELECT id,kind FROM (SELECT id,0 kind,priority,created_at FROM commands WHERE status IN ('queued','retry_wait') AND next_attempt_at<=? UNION ALL SELECT id,1 kind,priority,created_at FROM metadata_refresh_commands WHERE status IN ('queued','retry_wait') AND next_attempt_at<=? UNION ALL SELECT id,2 kind,priority,created_at FROM blocklist_clear_commands WHERE status IN ('queued','retry_wait') AND next_attempt_at<=? UNION ALL SELECT id,3 kind,priority,created_at FROM rss_commands WHERE status IN ('queued','retry_wait','running') AND next_attempt_at<=? UNION ALL SELECT id,4 kind,0 priority,created_at FROM rss_candidates WHERE status IN ('pending','prepared','reconciling') AND (not_before IS NULL OR not_before<=?) AND (command_id IS NULL OR NOT EXISTS(SELECT 1 FROM rss_commands c WHERE c.id=rss_candidates.command_id AND c.status IN ('queued','running','retry_wait'))) UNION ALL SELECT candidate_id id,5 kind,0 priority,created_at FROM download_processing WHERE (status='queued' OR (status='importing' AND (error_code IS NULL OR resume_requested=1))) AND next_attempt_at<=? UNION ALL SELECT id,6 kind,priority,created_at FROM search_commands WHERE status IN ('queued','retry_wait') AND next_attempt_at<=? UNION ALL SELECT id,7 kind,priority,created_at FROM manual_import_commands WHERE status IN ('queued','retry_wait') AND next_attempt_at<=? AND NOT EXISTS(SELECT 1 FROM manual_import_commands r WHERE r.status='running') UNION ALL SELECT id,8 kind,priority,created_at FROM quality_reset_commands WHERE status IN ('queued','retry_wait') AND next_attempt_at<=?) ORDER BY priority DESC,created_at,id LIMIT 1",params![timestamp,timestamp,timestamp,timestamp,timestamp,timestamp,timestamp,timestamp,timestamp]).await?.next().await?;
        let Some(row)=row else{return Ok(None)};
        let id=Uuid::parse_str(&row.get::<String>(0)?).map_err(|_|bad())?;
        if row.get::<i64>(1)?==8 {return Ok(Some(Claimed::QualityReset(quality_reset::claim(&tx,id,timestamp).await?)))}
        if row.get::<i64>(1)?==7 {return Ok(Some(Claimed::ManualImport(manual_import::claim(&tx,id,timestamp).await?)))}
        if row.get::<i64>(1)?==6 {return Ok(Some(Claimed::Search(search::claim(&tx,id,timestamp).await?)))}
        if row.get::<i64>(1)?==5 {return Ok(Some(Claimed::Processing(processing::claim(&tx,id,timestamp).await?)))}
        if row.get::<i64>(1)?==4 {return Ok(Some(Claimed::RssCandidate(id)))}
        if row.get::<i64>(1)?==3 {return Ok(Some(Claimed::Rss(rss::claim(&tx,id,timestamp).await?)))}
        if row.get::<i64>(1)?==2 {return Ok(Some(Claimed::Blocklist(blocklist::claim(&tx,id,timestamp).await?)))}
        if row.get::<i64>(1)?==1 {return Ok(Some(Claimed::Metadata(metadata::claim(&tx,id,timestamp).await?)))}
        let command=read_command(&tx,id).await?;
        if !valid_provider(&tx,command.target,command.provider_revision).await?{
            tx.execute("UPDATE commands SET status='failed',completed_at=?,error_code='provider_changed' WHERE id=?",params![timestamp,command.id.to_string()]).await?;return Ok(None)
        }
        tx.execute("UPDATE commands SET status='running',attempts=attempts+1,started_at=?,error_code=NULL WHERE id=?",params![timestamp,command.id.to_string()]).await?;
        Ok(Some(Claimed::Downloads(read_command(&tx,command.id).await?)))
    }.await;
    finish(tx, result).await
}
async fn snapshot_capacity(c: &Connection, target: RefreshTarget) -> Result<bool> {
    Ok(c.query("SELECT (SELECT count(*) FROM download_refresh_snapshots)<? OR EXISTS(SELECT 1 FROM download_refresh_snapshots WHERE provider_id=? AND media_type=?)",params![MAX_SNAPSHOTS,target.provider_id.to_string(),domain(target.media_type)]).await?.next().await?.ok_or_else(bad)?.get::<i64>(0)?==1)
}
async fn step(
    db: &Arc<Database>,
    client: &RefreshClient,
    metadata_client: &crate::metadata::MetadataClient,
) -> Result<()> {
    // A prior local storage failure can leave an interrupted claim. There is no other worker.
    recover(db, "storage_error").await?;
    let Some(command) = claim(db).await? else {
        return Ok(());
    };
    let command = match command {
        Claimed::Downloads(command) => command,
        Claimed::Metadata(command) => return metadata::run(db, metadata_client, command).await,
        Claimed::Blocklist(command) => return blocklist::run(db, command).await,
        Claimed::Rss(command) => return rss::run(db, client, command).await,
        Claimed::RssCandidate(id) => return rss::run_due(db, client, id).await,
        Claimed::Processing(item) => return processing::run(db, client, item).await,
        Claimed::Search(item) => return search::run(db, client, item).await,
        Claimed::ManualImport(command) => return manual_import::run(db, command).await,
        Claimed::QualityReset(command) => return quality_reset::run(db, command).await,
    };
    let id = command.id;
    let result = if !snapshot_capacity(&connection(db).await?, command.target).await? {
        Err(RefreshError {
            code: "refresh_limit",
            retryable: false,
            retry_after_seconds: None,
        })
    } else {
        let probe = client.refresh(
            command.target.provider_id,
            command.provider_revision,
            command.target.media_type,
        );
        tokio::pin!(probe);
        let mut cancellation = tokio::time::interval(Duration::from_millis(250));
        cancellation.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                value=&mut probe=>break value,
                _=cancellation.tick()=>{
                    let Some(current)=retained_command(&connection(db).await?,id).await? else {return Ok(())};
                    if !matches!(current.status,CommandStatus::Running){return Ok(())}
                }
            }
        }
    };
    publish(db, command, result).await
}
// Active rows cannot be deleted through the API. A missing claimed row therefore
// means its terminal history was explicitly removed after cancellation.
async fn retained_command(c: &Connection, id: Uuid) -> Result<Option<Command>> {
    match read_command(c, id).await {
        Ok(command) => Ok(Some(command)),
        Err(Error(StatusCode::NOT_FOUND, "command_not_found")) => Ok(None),
        Err(error) => Err(error),
    }
}
async fn publish(
    db: &Database,
    command: Command,
    result: std::result::Result<Vec<crate::providers::qbittorrent::DownloadItem>, RefreshError>,
) -> Result<()> {
    let c = connection(db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome=async{
        let Some(current)=retained_command(&tx,command.id).await? else {return Ok(None)};
        if !matches!(current.status,CommandStatus::Running){return Ok(None)}
        let result=if valid_provider(&tx,command.target,command.provider_revision).await?{result}else{Err(RefreshError{code:"provider_changed",retryable:false,retry_after_seconds:None})};
        let timestamp=now()?;
        match result {
            Ok(items)=>{
                let json=serde_json::to_string(&items).map_err(|_|bad())?;
                if json.len()>MAX_BYTES||items.len()>1000||!snapshot_capacity(&tx,command.target).await?{
                    fail(&tx,&command,timestamp,RefreshError{code:"refresh_limit",retryable:false,retry_after_seconds:None}).await?;
                }else{
                    tx.execute("UPDATE commands SET status='succeeded',completed_at=?,items_observed=? WHERE id=?",params![timestamp,items.len() as i64,command.id.to_string()]).await?;
                    tx.execute("INSERT INTO download_refresh_snapshots(provider_id,media_type,provider_revision,observed_at,command_id,items_json) VALUES(?,?,?,?,?,?) ON CONFLICT(provider_id,media_type) DO UPDATE SET provider_revision=excluded.provider_revision,observed_at=excluded.observed_at,command_id=excluded.command_id,items_json=excluded.items_json",params![command.target.provider_id.to_string(),domain(command.target.media_type),command.provider_revision,timestamp,command.id.to_string(),json]).await?;
                    processing::observe(&tx,command.target,command.provider_revision,&items,timestamp).await?;
                }
            },
            Err(error)=>fail(&tx,&command,timestamp,error).await?,
        }
        Ok(Some(read_command(&tx, command.id).await?))
    }.await;
    let settled = finish(tx, outcome).await?;
    if let Some(settled) = settled {
        eprintln!(
            "event=command_settled command_id={} media_type={} status={} error_code={}",
            settled.id,
            domain(settled.target.media_type),
            settled.status.text(),
            settled.error_code.as_deref().unwrap_or("none")
        );
    }
    Ok(())
}
async fn fail(
    c: &Connection,
    command: &Command,
    timestamp: i64,
    error: RefreshError,
) -> Result<()> {
    let retry = error.retryable && command.attempts < 3;
    let jitter = i64::from(Uuid::new_v4().as_bytes()[0] % 3);
    let delay = (1i64 << command.attempts) + jitter;
    let delay = delay.max(i64::from(error.retry_after_seconds.unwrap_or(0).min(86400)));
    c.execute(
        "UPDATE commands SET status=?,next_attempt_at=?,completed_at=?,error_code=? WHERE id=?",
        params![
            if retry { "retry_wait" } else { "failed" },
            timestamp + delay,
            if retry { None } else { Some(timestamp) },
            error.code,
            command.id.to_string()
        ],
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            if let Err(error) = std::fs::remove_dir_all(&self.0) {
                eprintln!("scratch cleanup failed: {error}");
            }
        }
    }

    #[tokio::test]
    async fn cancelled_deleted_history_is_terminal_during_probe_and_publication() {
        let path = std::env::temp_dir().join(format!("hrrdarr-cancel-delete-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        let _scratch = Scratch(path.clone());
        let db = Arc::new(Database::open_local(path.join("db")).await.unwrap());
        let started = Arc::new(tokio::sync::Notify::new());
        let signal = started.clone();
        let remote = axum::Router::new().fallback(move |uri: axum::http::Uri| {
            let signal = signal.clone();
            async move {
                if uri.path().ends_with("webapiVersion") {
                    return "2.8.3";
                }
                assert!(uri.path().ends_with("torrents/info"));
                signal.notify_one();
                std::future::pending::<&'static str>().await
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let remote_task = tokio::spawn(async move { axum::serve(listener, remote).await.unwrap() });
        let (app, client) = crate::providers::router_with_refresh(db.clone(), None);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let scope = json!({"category":"tv","imported_category":null,"recent_priority":0,"older_priority":1});
        let response = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().unwrap()
            .post(format!("{base}/api/v1/providers")).header("content-type", "application/json").body(json!({"name":"cancel-delete", "enabled":true,"priority":1,"settings":{"implementation":"qbittorrent","endpoint":endpoint,"tv":scope,"movies":null},"credentials":null}).to_string()).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let provider: serde_json::Value =
            serde_json::from_str(&response.text().await.unwrap()).unwrap();
        let c = connection(&db).await.unwrap();
        let command = enqueue(
            &c,
            CommandInput {
                name: CommandName::RefreshDownloads,
                target: RefreshTarget {
                    provider_id: Uuid::parse_str(provider["id"].as_str().unwrap()).unwrap(),
                    media_type: MediaDomain::Tv,
                },
                provider_revision: provider["revision"].as_i64().unwrap(),
                priority: CommandPriority::Normal,
            },
            now().unwrap(),
        )
        .await
        .unwrap();
        let metadata = crate::metadata::MetadataClient::new().unwrap();
        let mut work = Box::pin(step(&db, &client, &metadata));
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                result = &mut work => panic!("probe unexpectedly settled: {}", result.is_ok()),
                _ = started.notified() => (),
            }
        })
        .await
        .unwrap();
        let running = read_command(&c, command.id).await.unwrap();
        assert!(matches!(running.status, CommandStatus::Running));
        // Polling is paused here so the real handlers delete before the next cancellation tick.
        let _ = cancel(
            State(db.clone()),
            Path(command.id.to_string()),
            Ok(Query(Empty {})),
        )
        .await
        .unwrap();
        delete(
            State(db.clone()),
            Path(command.id.to_string()),
            Ok(Query(Empty {})),
        )
        .await
        .unwrap();
        assert!(matches!(
            read_command(&c, command.id).await,
            Err(Error(StatusCode::NOT_FOUND, "command_not_found"))
        ));
        // Missing history is a supported cancellation outcome, not a storage error/retry.
        tokio::time::timeout(Duration::from_secs(2), &mut work)
            .await
            .unwrap()
            .unwrap();
        // The other race: HTTP completes before the cancellation tick and reaches publication.
        publish(&db, running, Ok(vec![])).await.unwrap();
        assert_eq!(
            c.query("SELECT count(*) FROM download_refresh_snapshots", ())
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
        remote_task.abort();
        let _ = remote_task.await;
        drop(c);
        drop(work);
        drop(client);
        drop(db);
    }
}
