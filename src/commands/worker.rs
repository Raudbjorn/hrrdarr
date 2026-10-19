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

fn default_metadata() -> Result<Arc<crate::metadata::MetadataClient>> {
    crate::metadata::MetadataClient::new()
        .map(Arc::new)
        .map_err(|_| Error(StatusCode::SERVICE_UNAVAILABLE, "metadata_unavailable"))
}

pub async fn start(db: Arc<Database>, client: RefreshClient) -> Result<Runtime> {
    start_with_metadata(db, client, default_metadata()?).await
}

pub async fn start_with_metadata(
    db: Arc<Database>,
    client: RefreshClient,
    metadata: Arc<crate::metadata::MetadataClient>,
) -> Result<Runtime> {
    let owner = acquire(&db, &client).await?;
    let task = tokio::spawn(async move {
        let _owner = owner;
        // ponytail: one worker per locally owned DB; add weighted concurrency only with real jobs needing it.
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Terminal-history pruning is a worker-owned recurring job: bounded, idempotent and
        // serialized with this loop's claims. Errors are logged inside and never propagate.
        let mut housekeeping = now().ok().map(housekeeping::Schedule::new);
        let housekeeping_policy = housekeeping::Policy::default();
        loop {
            tick.tick().await;
            if let (Some(schedule), Ok(timestamp)) = (housekeeping.as_mut(), now()) {
                housekeeping::tick(&db, schedule, &housekeeping_policy, timestamp).await;
            }
            // Failures are logged and recovered inside; the next tick retries.
            let _ = iteration(&db, &client, &metadata).await;
        }
    });
    Ok(Runtime { task: Some(task) })
}

/// Test seam: the background worker's ownership, startup recovery and iteration without its
/// one-second cadence or housekeeping, so integration tests decide when a command is claimed.
/// `step` calls are serialized: an overlapping iteration would recover (re-queue) the command
/// the other one is running.
#[doc(hidden)]
pub struct Driver {
    db: Arc<Database>,
    client: RefreshClient,
    metadata: Arc<crate::metadata::MetadataClient>,
    gate: tokio::sync::Mutex<()>,
    _owner: Owner,
}
#[doc(hidden)]
pub async fn start_driven(db: Arc<Database>, client: RefreshClient) -> Result<Driver> {
    let metadata = default_metadata()?;
    let owner = acquire(&db, &client).await?;
    Ok(Driver {
        db,
        client,
        metadata,
        gate: tokio::sync::Mutex::new(()),
        _owner: owner,
    })
}
impl Driver {
    /// Runs one worker iteration; `Ok` reports whether a command was claimed. `Err` means the
    /// iteration failed or timed out and its storage recovery already ran, as in the loop.
    pub async fn step(&self) -> std::result::Result<bool, IterationFailed> {
        let _gate = self.gate.lock().await;
        iteration(&self.db, &self.client, &self.metadata).await
    }
}

/// Validates local ownership, registers the single worker and recovers interrupted work.
async fn acquire(db: &Arc<Database>, client: &RefreshClient) -> Result<Owner> {
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
    tokio::time::timeout(
        Duration::from_secs(10),
        crate::completed_download_handling::startup(&db),
    )
    .await
    .map_err(|_| Error(StatusCode::SERVICE_UNAVAILABLE, "cdh_timeout"))?
    .map_err(|e| Error(e.0, e.1))?;
    tokio::time::timeout(Duration::from_secs(10), crate::health::startup(&db))
        .await
        .map_err(|_| Error(StatusCode::SERVICE_UNAVAILABLE, "health_timeout"))??;
    tokio::time::timeout(Duration::from_secs(10), recover(&db, "interrupted"))
        .await
        .map_err(|_| Error(StatusCode::SERVICE_UNAVAILABLE, "command_storage_timeout"))??;
    // Interrupted explicit file deletions are finished or undone from their journal before any
    // new work runs. Bounded: one recovery pass, each row one transfer at most.
    crate::recycle_bin::deletion::startup(&db)
        .await
        .map_err(|e| Error(e.0, e.1))?;
    // Interrupted rename groups are rolled forward or back from their journal.
    rename::startup(&db).await?;
    tokio::time::timeout(Duration::from_secs(10), recover_rss(&db, "interrupted"))
        .await
        .map_err(|_| Error(StatusCode::SERVICE_UNAVAILABLE, "command_storage_timeout"))??;
    Ok(owner)
}

#[doc(hidden)]
#[derive(Debug)]
pub struct IterationFailed;

/// One bounded claim/run; storage failures are logged and recovered before returning `Err`.
async fn iteration(
    db: &Arc<Database>,
    client: &RefreshClient,
    metadata: &crate::metadata::MetadataClient,
) -> std::result::Result<bool, IterationFailed> {
    let result = tokio::time::timeout(Duration::from_secs(45), step(db, client, metadata)).await;
    if let Ok(Ok(claimed)) = result {
        return Ok(claimed);
    }
    eprintln!("event=command_worker_error code=storage_error");
    if !matches!(
        tokio::time::timeout(Duration::from_secs(10), recover_rss(db, "storage_error")).await,
        Ok(Ok(()))
    ) {
        eprintln!("event=rss_recovery_error code=storage_error");
    }
    // The failed future is gone. External GETs can repeat; metadata facts and success
    // commit together; local clears likewise commit tombstones and success together.
    // Only uncommitted local effects retry. RSS recovery separately fences
    // dispatched submissions into read-only reconciliation; attempts never reset.
    if !matches!(
        tokio::time::timeout(Duration::from_secs(10), recover(db, "storage_error")).await,
        Ok(Ok(()))
    ) {
        eprintln!("event=command_recovery_error code=storage_error");
    }
    Err(IterationFailed)
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
    rescan::recover(&c, timestamp, code).await?;
    downloaded_scan::recover(&c, timestamp, code).await?;
    recycle_cleanup::recover(&c, timestamp, code).await?;
    collection_refresh::recover(&c, timestamp, code).await?;
    rename::recover(&c, timestamp, code).await?;
    backup::recover(&c, timestamp, code).await?;
    crate::health::recover(&c, timestamp, code).await?;
    Ok(())
}
async fn schedule_due(c: &Connection, timestamp: i64) -> Result<()> {
    let mut rows=c.query(&format!("SELECT {SCHEDULE_COLUMNS} FROM download_refresh_schedules WHERE enabled=1 AND next_run_at<=? AND NOT EXISTS(SELECT 1 FROM download_client_status b JOIN providers p ON p.id=b.provider_id WHERE b.provider_id=download_refresh_schedules.provider_id AND b.media_type=download_refresh_schedules.media_type AND b.config_revision=download_refresh_schedules.provider_revision AND p.revision=b.config_revision AND b.disabled_until>?) ORDER BY next_run_at,provider_id,media_type LIMIT 64"),params![timestamp,timestamp]).await?;
    let mut due = Vec::new();
    while let Some(row) = rows.next().await? {
        due.push(schedule_row(row)?)
    }
    drop(rows);
    for schedule in due {
        let result = enqueue_origin(
            c,
            CommandInput {
                name: CommandName::RefreshDownloads,
                target: schedule.target,
                provider_revision: schedule.provider_revision,
                priority: CommandPriority::Normal,
            },
            timestamp,
            CommandOrigin::Automatic,
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
    Rescan(rescan::RescanCommand),
    Health(crate::health::HealthCommand),
    DownloadedScan(downloaded_scan::ScanClaim),
    RecycleCleanup(recycle_cleanup::RecycleBinCleanupCommand),
    CollectionRefresh(collection_refresh::CollectionRefreshCommand),
    Rename(rename::RenameCommand),
    Backup(backup::BackupCommand),
}
async fn claim(db: &Database) -> Result<Option<Claimed>> {
    let c = connection(db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let result=async{
        let timestamp=now()?;
        tx.execute("UPDATE commands SET status='failed',completed_at=?,error_code='provider_changed' WHERE status IN ('queued','retry_wait') AND NOT EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=commands.provider_id AND p.revision=commands.provider_revision AND p.enabled=1 AND p.implementation IN (SELECT implementation FROM provider_implementations WHERE kind='download_client') AND s.media_type=commands.media_type)",[timestamp]).await?;
        schedule_due(&tx,timestamp).await?;
        metadata::sweep(&tx,timestamp).await?;
        rss::schedule_due(&tx,timestamp).await?;
        crate::health::sweep(&tx,timestamp).await?;
        recycle_cleanup::schedule_due(&tx,timestamp).await?;
        collection_refresh::schedule_due(&tx,timestamp).await?;
        backup::schedule_due(&tx,timestamp).await?;
        if let Some(command)=collection_refresh::claim_due(&tx,timestamp).await? {return Ok(Some(Claimed::CollectionRefresh(command)))}
        if let Some(command)=backup::claim_due(&tx,timestamp).await? {return Ok(Some(Claimed::Backup(command)))}
        let row=tx.query(&format!("SELECT id,kind FROM (SELECT id,0 kind,priority,created_at FROM commands WHERE status IN ('queued','retry_wait') AND next_attempt_at<=? AND (origin!='automatic' OR manual_bypass=1 OR NOT EXISTS(SELECT 1 FROM download_client_status b WHERE b.provider_id=commands.provider_id AND b.media_type=commands.media_type AND b.config_revision=commands.provider_revision AND b.disabled_until>?)) UNION ALL SELECT id,1 kind,priority,created_at FROM metadata_refresh_commands WHERE status IN ('queued','retry_wait') AND next_attempt_at<=? UNION ALL SELECT id,2 kind,priority,created_at FROM blocklist_clear_commands WHERE status IN ('queued','retry_wait') AND next_attempt_at<=? UNION ALL SELECT id,3 kind,priority,created_at FROM rss_commands WHERE status IN ('queued','retry_wait','running') AND next_attempt_at<=? UNION ALL SELECT id,4 kind,0 priority,created_at FROM rss_candidates WHERE status IN ('pending','prepared','reconciling') AND (not_before IS NULL OR not_before<=?) AND (command_id IS NULL OR NOT EXISTS(SELECT 1 FROM rss_commands c WHERE c.id=rss_candidates.command_id AND c.status IN ('queued','running','retry_wait'))) UNION ALL SELECT candidate_id id,5 kind,0 priority,created_at FROM download_processing WHERE ((status='queued' AND NOT {reserved} AND NOT EXISTS(SELECT 1 FROM rescan_commands r JOIN rss_candidates c ON c.id=download_processing.candidate_id WHERE r.status IN ('queued','running','retry_wait') AND ((c.media_type='tv' AND r.media_type='tv' AND r.series_id=c.series_id) OR (c.media_type='movies' AND r.media_type='movies' AND r.movie_id=c.movie_id)))) OR (status='importing' AND (error_code IS NULL OR resume_requested=1))) AND next_attempt_at<=? UNION ALL SELECT id,6 kind,priority,created_at FROM search_commands WHERE status IN ('queued','retry_wait') AND next_attempt_at<=? UNION ALL SELECT id,7 kind,priority,created_at FROM manual_import_commands WHERE status IN ('queued','retry_wait') AND next_attempt_at<=? AND NOT EXISTS(SELECT 1 FROM manual_import_commands r WHERE r.status='running') AND NOT EXISTS(SELECT 1 FROM operations o WHERE o.id=manual_import_commands.operation_id AND EXISTS(SELECT 1 FROM rescan_commands r WHERE r.status IN ('queued','running','retry_wait') AND ((o.media_type='episode' AND r.media_type='tv' AND EXISTS(SELECT 1 FROM episodes e WHERE e.id=o.episode_id AND e.series_id=r.series_id)) OR (o.media_type='movie' AND r.media_type='movies' AND r.movie_id=o.movie_id)))) UNION ALL SELECT id,8 kind,priority,created_at FROM quality_reset_commands WHERE status IN ('queued','retry_wait') AND next_attempt_at<=? UNION ALL SELECT id,9 kind,priority,created_at FROM rescan_commands WHERE status IN ('queued','retry_wait') AND next_attempt_at<=? AND NOT EXISTS(SELECT 1 FROM import_journal j JOIN operations o ON o.id=j.operation_id WHERE j.phase NOT IN ('preview','complete') AND ((rescan_commands.media_type='tv' AND o.media_type='episode' AND EXISTS(SELECT 1 FROM episodes e WHERE e.id=o.episode_id AND e.series_id=rescan_commands.series_id)) OR (rescan_commands.media_type='movies' AND o.media_type='movie' AND o.movie_id=rescan_commands.movie_id))) AND NOT EXISTS(SELECT 1 FROM download_processing dp JOIN rss_candidates r ON r.id=dp.candidate_id WHERE dp.status IN ('checking','importing') AND ((rescan_commands.media_type='tv' AND r.media_type='tv' AND r.series_id=rescan_commands.series_id) OR (rescan_commands.media_type='movies' AND r.media_type='movies' AND r.movie_id=rescan_commands.movie_id))) UNION ALL SELECT id,10 kind,priority,created_at FROM health_commands WHERE status IN ('queued','retry_wait') AND next_attempt_at<=? UNION ALL SELECT id,11 kind,priority,ready_order_at created_at FROM downloaded_scan_commands WHERE (status IN ('queued','retry_wait') OR (status='running' AND (inventory_revision=1 OR candidate_id IS NOT NULL))) AND next_attempt_at<=? UNION ALL SELECT id,12 kind,priority,created_at FROM recycle_bin_cleanup_commands WHERE status IN ('queued','retry_wait') AND next_attempt_at<=? UNION ALL SELECT id,13 kind,priority,created_at FROM rename_commands WHERE status IN ('queued','retry_wait') AND next_attempt_at<=?) ORDER BY priority DESC,created_at,id LIMIT 1",reserved=processing::SCAN_RESERVED),params![timestamp,timestamp,timestamp,timestamp,timestamp,timestamp,timestamp,timestamp,timestamp,timestamp,timestamp,timestamp,timestamp,timestamp,timestamp]).await?.next().await?;
        let Some(row)=row else{return Ok(None)};
        let id=Uuid::parse_str(&row.get::<String>(0)?).map_err(|_|bad())?;
        // Only this arm substitutes ready_order_at for created_at, so scan continuations cannot
        // starve same-priority work in the other queues.
        if row.get::<i64>(1)?==13 {return Ok(Some(Claimed::Rename(rename::claim(&tx,id,timestamp).await?)))}
        if row.get::<i64>(1)?==12 {return Ok(Some(Claimed::RecycleCleanup(recycle_cleanup::claim(&tx,id,timestamp).await?)))}
        if row.get::<i64>(1)?==11 {return Ok(downloaded_scan::claim(&tx,id,timestamp).await?.map(Claimed::DownloadedScan))}
        if row.get::<i64>(1)?==10 {return Ok(Some(Claimed::Health(crate::health::claim(&tx,id,timestamp).await?)))}
        if row.get::<i64>(1)?==9 {return Ok(rescan::claim(&tx,id,timestamp).await?.map(Claimed::Rescan))}
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
) -> Result<bool> {
    // A prior local storage failure can leave an interrupted claim. There is no other worker.
    recover(db, "storage_error").await?;
    let Some(command) = claim(db).await? else {
        return Ok(false);
    };
    run_claimed(db, client, metadata_client, command)
        .await
        .map(|()| true)
}
async fn run_claimed(
    db: &Arc<Database>,
    client: &RefreshClient,
    metadata_client: &crate::metadata::MetadataClient,
    command: Claimed,
) -> Result<()> {
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
        Claimed::Rescan(command) => return rescan::run(db, command).await,
        Claimed::DownloadedScan(claim) => return downloaded_scan::run(db, claim).await,
        Claimed::RecycleCleanup(command) => return recycle_cleanup::run(db, command).await,
        Claimed::CollectionRefresh(command) => {
            return collection_refresh::run(db, metadata_client, command).await;
        }
        Claimed::Rename(command) => return rename::run(db, command).await,
        Claimed::Backup(command) => return backup::run(db, command).await,
        Claimed::Health(command) => {
            return crate::health::run(db, client, command)
                .await
                .map_err(Into::into);
        }
    };
    let id = command.id;
    let result = if !snapshot_capacity(&connection(db).await?, command.target).await? {
        Err(RefreshError {
            code: "refresh_limit",
            retryable: false,
            retry_after_seconds: None,
            client_failure: false,
            definitely_not_submitted: false,
            client_retry_kind: crate::providers::ClientRetryKind::Other,
            client_retry_eligible: false,
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
    publish(db, command, result, client.backoff_grace_elapsed()).await
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
    grace_elapsed: bool,
) -> Result<()> {
    let c = connection(db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome=async{
        let Some(current)=retained_command(&tx,command.id).await? else {return Ok(None)};
        if !matches!(current.status,CommandStatus::Running) || current.attempts != command.attempts {return Ok(None)}
        let provider_current=valid_provider(&tx,command.target,command.provider_revision).await?;
        let result=if provider_current{result}else{Err(RefreshError{code:"provider_changed",retryable:false,retry_after_seconds:None,client_failure:false,definitely_not_submitted:false,client_retry_kind:crate::providers::ClientRetryKind::Other,client_retry_eligible:false})};
        let timestamp=now()?;
        match result {
            Ok(items)=>{
                let json=serde_json::to_string(&items).map_err(|_|bad())?;
                if json.len()>MAX_BYTES||items.len()>1000||!snapshot_capacity(&tx,command.target).await?{
                    fail(&tx,&command,timestamp,RefreshError{code:"refresh_limit",retryable:false,retry_after_seconds:None,client_failure:false,definitely_not_submitted:false,client_retry_kind:crate::providers::ClientRetryKind::Other,client_retry_eligible:false}).await?;
                }else{
                    tx.execute("UPDATE commands SET status='succeeded',completed_at=?,items_observed=? WHERE id=?",params![timestamp,items.len() as i64,command.id.to_string()]).await?;
                    tx.execute("INSERT INTO download_refresh_snapshots(provider_id,media_type,provider_revision,observed_at,command_id,items_json) VALUES(?,?,?,?,?,?) ON CONFLICT(provider_id,media_type) DO UPDATE SET provider_revision=excluded.provider_revision,observed_at=excluded.observed_at,command_id=excluded.command_id,items_json=excluded.items_json",params![command.target.provider_id.to_string(),domain(command.target.media_type),command.provider_revision,timestamp,command.id.to_string(),json]).await?;
                    processing::observe(&tx,command.target,command.provider_revision,&items,timestamp).await?;
                    crate::providers::backoff::apply(&tx,&command.target.provider_id.to_string(),command.provider_revision,command.target.media_type,crate::providers::backoff::Observation::Success,timestamp,grace_elapsed).await.map_err(|_|Error(StatusCode::INTERNAL_SERVER_ERROR,"client_backoff_storage_error"))?;
                }
            },
            Err(error)=>{
                if error.client_failure {
                    crate::providers::backoff::apply(&tx,&command.target.provider_id.to_string(),command.provider_revision,command.target.media_type,crate::providers::backoff::Observation::Failure,timestamp,grace_elapsed).await.map_err(|_|Error(StatusCode::INTERNAL_SERVER_ERROR,"client_backoff_storage_error"))?;
                }
                fail(&tx,&command,timestamp,error).await?;
            },
        }
        // A synthesized stale-provider failure is not a current status observation.
        // This hook shares the snapshot/processing transaction, including rollback.
        if provider_current {
            crate::health::communication_status_changed(&tx,command.target.media_type).await?;
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

    async fn health_refresh_fixture() -> (Scratch, Database, Connection, Uuid) {
        let path = std::env::temp_dir().join(format!("hrrdarr-health-refresh-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        let db = Database::open_local(path.join("db")).await.unwrap();
        let c = db.connect().await.unwrap();
        let provider = Uuid::new_v4();
        c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'qbittorrent','Status fixture',1,1,1,1,'http://127.0.0.1:1/')",[provider.to_string()]).await.unwrap();
        for media in ["tv", "movies"] {
            c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags) VALUES(?,'qbittorrent',?,?,0,1,'started','default',0,0,0)",params![provider.to_string(),media,media]).await.unwrap();
        }
        (Scratch(path), db, c, provider)
    }
    async fn health_running_refresh(c: &Connection, provider: Uuid, media: MediaDomain) -> Command {
        let cmd = enqueue(
            c,
            CommandInput {
                name: CommandName::RefreshDownloads,
                target: RefreshTarget {
                    provider_id: provider,
                    media_type: media,
                },
                provider_revision: 1,
                priority: CommandPriority::Normal,
            },
            1,
        )
        .await
        .unwrap();
        c.execute("UPDATE commands SET status='running',attempts=attempts+1,started_at=1,error_code=NULL WHERE id=?",[cmd.id.to_string()]).await.unwrap();
        read_command(c, cmd.id).await.unwrap()
    }
    async fn scalar(c: &Connection, sql: &str) -> i64 {
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
    #[tokio::test]
    async fn health_refresh_status_publication_is_scoped_atomic_and_revision_fenced() {
        let (_scratch, db, c, provider) = health_refresh_fixture().await;
        let first = health_running_refresh(&c, provider, MediaDomain::Tv).await;
        let id = first.id;
        // A marker write failure must also roll back the accepted snapshot and command finish.
        c.execute("CREATE TRIGGER fail_health_marker BEFORE UPDATE ON health_checks WHEN NEW.generation>OLD.generation BEGIN SELECT RAISE(ABORT,'fixture'); END",()).await.unwrap();
        assert!(publish(&db, first, Ok(vec![]), true).await.is_err());
        assert!(matches!(
            read_command(&c, id).await.unwrap().status,
            CommandStatus::Running
        ));
        assert_eq!(
            scalar(&c, "SELECT count(*) FROM download_refresh_snapshots").await,
            0
        );
        assert_eq!(
            scalar(&c, "SELECT sum(generation) FROM health_checks").await,
            0
        );
        c.execute("DROP TRIGGER fail_health_marker", ())
            .await
            .unwrap();
        publish(&db, read_command(&c, id).await.unwrap(), Ok(vec![]), true)
            .await
            .unwrap();
        assert_eq!(scalar(&c,"SELECT generation FROM health_checks WHERE scope='tv' AND check_key='download_client_communication'").await,1);
        assert_eq!(scalar(&c,"SELECT sum(generation) FROM health_checks WHERE scope='movies' OR check_key='completed_download_handling'").await,0);
        assert_eq!(
            scalar(&c, "SELECT count(*) FROM download_refresh_snapshots").await,
            1
        );
        // A legitimate local/remote failed refresh also requests reevaluation, not an Error issue.
        let failure = health_running_refresh(&c, provider, MediaDomain::Movies).await;
        publish(
            &db,
            failure,
            Err(RefreshError {
                code: "refresh_timeout",
                retryable: false,
                retry_after_seconds: None,
                client_failure: false,
                definitely_not_submitted: false,
                client_retry_kind: crate::providers::ClientRetryKind::Other,
                client_retry_eligible: false,
            }),
            true,
        )
        .await
        .unwrap();
        assert_eq!(scalar(&c,"SELECT generation FROM health_checks WHERE scope='movies' AND check_key='download_client_communication'").await,1);
        assert_eq!(
            scalar(
                &c,
                "SELECT count(*) FROM health_checks WHERE severity IS NOT NULL"
            )
            .await,
            0
        );
        let stale = health_running_refresh(&c, provider, MediaDomain::Tv).await;
        c.execute(
            "UPDATE providers SET revision=2 WHERE id=?",
            [provider.to_string()],
        )
        .await
        .unwrap();
        publish(&db, stale, Ok(vec![]), true).await.unwrap();
        assert_eq!(
            scalar(&c, "SELECT sum(generation) FROM health_checks").await,
            2
        );
        // Existing provider-revision trigger invalidates the old snapshot immediately.
        assert_eq!(
            scalar(&c, "SELECT count(*) FROM download_refresh_snapshots").await,
            0
        );
    }
    #[tokio::test]
    async fn enqueue_persists_origin_and_manual_coalesce_promotes_automatic_once() {
        let (_scratch, _db, c, provider) = health_refresh_fixture().await;
        let input = |media_type| CommandInput {
            name: CommandName::RefreshDownloads,
            target: RefreshTarget {
                provider_id: provider,
                media_type,
            },
            provider_revision: 1,
            priority: CommandPriority::Normal,
        };
        let automatic = enqueue_origin(&c, input(MediaDomain::Tv), 1, CommandOrigin::Automatic)
            .await
            .unwrap();
        let manual = enqueue_origin(&c, input(MediaDomain::Movies), 1, CommandOrigin::Manual)
            .await
            .unwrap();
        assert_eq!(
            scalar(
                &c,
                "SELECT count(*) FROM commands WHERE origin='legacy_unknown'"
            )
            .await,
            0
        );
        assert_eq!(scalar(&c, "SELECT count(*) FROM commands WHERE media_type='tv' AND origin='automatic' AND manual_bypass=0").await, 1);
        assert_eq!(scalar(&c, "SELECT count(*) FROM commands WHERE media_type='movies' AND origin='manual' AND manual_bypass=1").await, 1);
        // Manual work is born bypassing (parity semantics); automatic coalescing never promotes, a manual request onto automatic work does, once.
        assert_eq!(
            enqueue_origin(&c, input(MediaDomain::Tv), 2, CommandOrigin::Automatic)
                .await
                .unwrap()
                .id,
            automatic.id
        );
        assert_eq!(
            scalar(
                &c,
                "SELECT manual_bypass FROM commands WHERE media_type='tv'"
            )
            .await,
            0
        );
        assert_eq!(
            enqueue_origin(&c, input(MediaDomain::Tv), 2, CommandOrigin::Manual)
                .await
                .unwrap()
                .id,
            automatic.id
        );
        assert_eq!(
            enqueue_origin(&c, input(MediaDomain::Tv), 3, CommandOrigin::Manual)
                .await
                .unwrap()
                .id,
            automatic.id
        );
        assert_eq!(scalar(&c, "SELECT count(*) FROM commands WHERE media_type='tv' AND origin='automatic' AND manual_bypass=1").await, 1);
        // Manual work is never relabelled by later admissions.
        assert_eq!(
            enqueue_origin(&c, input(MediaDomain::Movies), 2, CommandOrigin::Automatic)
                .await
                .unwrap()
                .id,
            manual.id
        );
        assert_eq!(scalar(&c, "SELECT count(*) FROM commands WHERE media_type='movies' AND origin='manual' AND manual_bypass=1").await, 1);
    }
    #[tokio::test]
    async fn health_refresh_exhaustion_keeps_snapshot_and_cancelled_callback_does_not_mark() {
        let (_scratch, db, c, provider) = health_refresh_fixture().await;
        c.execute("DELETE FROM health_checks WHERE scope='tv' AND check_key='download_client_communication'",()).await.unwrap();
        c.execute("INSERT INTO health_checks(scope,check_key,startup,scheduled,generation,compatibility_type) VALUES('tv','download_client_communication',1,1,9007199254740991,'DownloadClientCheck')",()).await.unwrap();
        let cmd = health_running_refresh(&c, provider, MediaDomain::Tv).await;
        let id = cmd.id;
        publish(&db, cmd, Ok(vec![]), true).await.unwrap();
        assert!(matches!(
            read_command(&c, id).await.unwrap().status,
            CommandStatus::Succeeded
        ));
        assert_eq!(
            scalar(&c, "SELECT count(*) FROM download_refresh_snapshots").await,
            1
        );
        assert_eq!(
            scalar(
                &c,
                "SELECT count(*) FROM health_lifecycle WHERE schedule_error='health_invariant'"
            )
            .await,
            1
        );
        let cancelled = health_running_refresh(&c, provider, MediaDomain::Movies).await;
        c.execute(
            "UPDATE commands SET status='cancelled',completed_at=2 WHERE id=?",
            [cancelled.id.to_string()],
        )
        .await
        .unwrap();
        publish(&db, cancelled, Ok(vec![]), true).await.unwrap();
        assert_eq!(scalar(&c,"SELECT generation FROM health_checks WHERE scope='movies' AND check_key='download_client_communication'").await,0);
        assert_eq!(
            scalar(&c, "SELECT count(*) FROM download_refresh_snapshots").await,
            1
        );
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
        publish(&db, running, Ok(vec![]), true).await.unwrap();
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

    // A queued manual import for a target with an active rescan must be deferred by claim()'s
    // own SELECT, not claimed and then failed by rescan_blocks_import_update's RAISE.
    #[tokio::test]
    async fn claim_defers_a_queued_manual_import_whose_target_has_an_active_rescan() {
        let path = std::env::temp_dir().join(format!("hrrdarr-kind7-deferral-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        let _scratch = Scratch(path.clone());
        for p in ["tv", "downloads"] {
            std::fs::create_dir(path.join(p)).unwrap();
        }
        let db = Arc::new(Database::open_local(path.join("db")).await.unwrap());
        let tv = path.join("tv").to_str().unwrap().to_owned();
        let source = path.join("downloads/media").to_str().unwrap().to_owned();
        let dest = path.join("tv/media.mkv").to_str().unwrap().to_owned();
        std::fs::write(&source, b"kind7-deferral-content").unwrap();
        let c = connection(&db).await.unwrap();
        c.execute_batch(&format!(
            "INSERT INTO series(id,title,path) VALUES(1,'Show','{tv}');
             INSERT INTO seasons(series_id,number) VALUES(1,1);
             INSERT INTO episodes(id,series_id,season,number,title) VALUES(1,1,1,1,'Pilot');"
        ))
        .await
        .unwrap();
        let operation = crate::import::preview(
            db.clone(),
            crate::import::ImportInput::Typed(crate::import::ManualImportRequest {
                target: crate::db::MediaTarget::Episode(1),
                source,
                mode: crate::import::Mode::Copy,
                destination: dest,
            }),
        )
        .await
        .unwrap()
        .id;
        let command_id = Uuid::new_v4();
        c.execute(
            "INSERT INTO manual_import_commands(id,batch_id,operation_id,priority,status,attempts,next_attempt_at,created_at) VALUES(?,?,?,0,'queued',0,0,0)",
            params![command_id.to_string(), Uuid::new_v4().to_string(), operation.to_string()],
        )
        .await
        .unwrap();
        let rescan_id = Uuid::new_v4();
        // created_at=1 (not 0, tying manual_import_commands' row): if the kind=7 deferral
        // predicate were ever broken, both rows would become simultaneously eligible and this
        // ordering makes manual_import_commands (created_at=0) deterministically win the tie,
        // so a regression here fails this test every time rather than ~50% of the time on
        // random UUID ordering (flagged during the kind=5 test's own dispatch, fixed to match).
        c.execute(
            "INSERT INTO rescan_commands(id,media_type,series_id,priority,status,attempts,next_attempt_at,created_at) VALUES(?,'tv',1,0,'queued',0,0,1)",
            [rescan_id.to_string()],
        )
        .await
        .unwrap();
        // The rescan itself is claimed first (the journal is still at 'preview', so kind=9's own
        // admission predicate doesn't block it) -- proving the manual import was correctly
        // deferred, since it's the only other queued command and claim() only returns one row.
        let Some(Claimed::Rescan(claimed_rescan)) = claim(&db).await.unwrap() else {
            panic!("expected the rescan to be claimed while the manual import is deferred");
        };
        assert_eq!(claimed_rescan.id, rescan_id);
        c.execute(
            "UPDATE rescan_commands SET status='succeeded',files_adopted=0,files_removed=0,completed_at=1 WHERE id=?",
            [rescan_id.to_string()],
        )
        .await
        .unwrap();
        // Picked up once the rescan clears.
        let Some(Claimed::ManualImport(claimed)) = claim(&db).await.unwrap() else {
            panic!("expected the manual import to become claimable once the rescan cleared");
        };
        assert_eq!(claimed.id, command_id);
    }

    // Trimmed, duplicated variant of rescan::tests::admit_pending_replacement's fixture chain (private per-file test modules) that stops at 'observed', skipping its replacement-only wiring.
    async fn observed_candidate(
        c: &Connection,
        series_id: i64,
        episode_id: i64,
        finalize: bool,
        media: MediaDomain,
    ) -> Uuid {
        let tv = matches!(media, MediaDomain::Tv);
        let media_name = domain(media);
        let categories = if tv { "[5000]" } else { "[2000]" };
        let remove_year = if tv { "NULL" } else { "0" };
        let anime_format = if tv { "0" } else { "NULL" };
        let indexer = Uuid::new_v4().to_string();
        let client = Uuid::new_v4().to_string();
        c.execute_batch(&format!(
            "INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES('{indexer}','torznab','Indexer',1,1,1,1,'http://indexer');
             INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year) VALUES('{indexer}','torznab','{media_name}','{categories}','[]',{anime_format},{remove_year});
             INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES('{client}','qbittorrent','Client',1,1,1,1,'http://client');
             INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags) VALUES('{client}','qbittorrent','{media_name}','{media_name}',0,1,'started','default',0,0,0);"
        ))
        .await
        .unwrap();
        let command_id = Uuid::new_v4().to_string();
        c.execute(
            "INSERT INTO rss_commands(id,name,media_type,status,next_attempt_at,created_at,indexer_id,indexer_revision,client_id,client_revision) VALUES(?,'rss_sync',?,'queued',9007199254740991,0,?,1,?,1)",
            params![command_id.clone(),media_name, indexer.clone(), client.clone()],
        )
        .await
        .unwrap();
        let candidate_id = Uuid::new_v4();
        let fingerprint = "a".repeat(64);
        let payload = vec![0u8; 32];
        c.execute(
            "INSERT INTO rss_candidates(id,command_id,media_type,indexer_id,indexer_revision,client_id,client_revision,fingerprint,title,private_payload,series_id,movie_id,status,decision_reasons_json,created_at,updated_at) VALUES(?,?,?, ?,1,?,1,?,'Release',?,?,?,'pending','[]',0,0)",
            params![
                candidate_id.to_string(),
                command_id,
                media_name,
                indexer,
                client.clone(),
                fingerprint,
                payload,
                tv.then_some(series_id),
                (!tv).then_some(episode_id)
            ],
        )
        .await
        .unwrap();
        if tv {
            c.execute(
            "INSERT INTO rss_candidate_episodes(candidate_id,series_id,episode_id) VALUES(?,?,?)",
            params![candidate_id.to_string(), series_id, episode_id],
        )
        .await
        .unwrap();
        }
        let hash = "b".repeat(40);
        let identity = serde_json::json!({
            "version": 1,
            "target": {"media_type": if tv {"episode"} else {"movie"}, "id": episode_id},
            "hashes": [hash.clone()],
            "settings_fingerprint": "c".repeat(64),
            "payload_sha256": "d".repeat(64),
        })
        .to_string();
        c.execute(
            "UPDATE rss_candidates SET status='prepared',submission_identity_json=? WHERE id=?",
            params![identity, candidate_id.to_string()],
        )
        .await
        .unwrap();
        c.execute(
            "INSERT INTO rss_hash_claims(client_id,hash,candidate_id) VALUES(?,?,?)",
            params![client.clone(), hash.clone(), candidate_id.to_string()],
        )
        .await
        .unwrap();
        c.execute(
            "UPDATE rss_candidates SET status='submitting',private_payload=NULL WHERE id=?",
            [candidate_id.to_string()],
        )
        .await
        .unwrap();
        if !finalize {
            return candidate_id;
        }
        c.execute(
            "UPDATE rss_candidates SET status='observed',observed_hash=? WHERE id=?",
            params![hash, candidate_id.to_string()],
        )
        .await
        .unwrap();
        c.execute(
            "INSERT INTO download_processing_policies(provider_id,media_type,provider_revision,revision,enabled,mode) VALUES(?,?,1,1,1,'copy')",
            params![client,media_name],
        )
        .await
        .unwrap();
        c.execute(
            "INSERT INTO download_processing(candidate_id,policy_revision,status,next_attempt_at,created_at,updated_at) VALUES(?,1,'queued',0,0,0)",
            [candidate_id.to_string()],
        )
        .await
        .unwrap();
        candidate_id
    }

    // A queued download_processing row for a target with an active rescan must be deferred by
    // claim()'s own SELECT, not claimed and then aborted by rescan_blocks_processing's RAISE.
    #[tokio::test]
    async fn claim_defers_a_queued_download_processing_whose_target_has_an_active_rescan() {
        let path = std::env::temp_dir().join(format!("hrrdarr-kind5-deferral-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        let _scratch = Scratch(path.clone());
        let db = Arc::new(Database::open_local(path.join("db")).await.unwrap());
        let c = connection(&db).await.unwrap();
        c.execute_batch(
            "INSERT INTO series(id,title,path) VALUES(1,'Show','/tv/show');
             INSERT INTO seasons(series_id,number) VALUES(1,1);
             INSERT INTO episodes(id,series_id,season,number,title) VALUES(1,1,1,1,'Pilot');",
        )
        .await
        .unwrap();
        let candidate_id = observed_candidate(&c, 1, 1, true, MediaDomain::Tv).await;
        let rescan_id = Uuid::new_v4();
        c.execute(
            "INSERT INTO rescan_commands(id,media_type,series_id,priority,status,attempts,next_attempt_at,created_at) VALUES(?,'tv',1,0,'queued',0,0,1)",
            [rescan_id.to_string()],
        )
        .await
        .unwrap();
        // download_processing sorts first on created_at (0 < 1) among eligible kind=5/9 rows, so
        // the rescan being claimed here only happens because the kind=5 predicate excluded it.
        let Some(Claimed::Rescan(claimed_rescan)) = claim(&db).await.unwrap() else {
            panic!("expected the rescan to be claimed while download_processing is deferred");
        };
        assert_eq!(claimed_rescan.id, rescan_id);
        c.execute(
            "UPDATE rescan_commands SET status='succeeded',files_adopted=0,files_removed=0,completed_at=1 WHERE id=?",
            [rescan_id.to_string()],
        )
        .await
        .unwrap();
        // Picked up once the rescan clears.
        let Some(Claimed::Processing(claimed)) = claim(&db).await.unwrap() else {
            panic!("expected download_processing to become claimable once the rescan cleared");
        };
        assert_eq!(claimed.receipt_id, candidate_id);
    }
    // A receipt captured by a downloaded scan stays queued under the scan's fence. Without the
    // claim/recover predicates the singleton path would select it every tick and abort on the
    // 0054 legacy guard, failing the whole worker step instead of skipping one row.
    #[tokio::test]
    async fn a_scan_reserved_receipt_is_never_claimed_or_recovered_by_the_singleton_path() {
        let path = std::env::temp_dir().join(format!("hrrdarr-kind5-scan-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        let _scratch = Scratch(path.clone());
        let db = Arc::new(Database::open_local(path.join("db")).await.unwrap());
        let c = connection(&db).await.unwrap();
        c.execute_batch(
            "INSERT INTO series(id,title,path) VALUES(1,'Show','/tv/show');
             INSERT INTO seasons(series_id,number) VALUES(1,1);
             INSERT INTO episodes(id,series_id,season,number,title) VALUES(1,1,1,1,'Pilot');",
        )
        .await
        .unwrap();
        let candidate = observed_candidate(&c, 1, 1, true, MediaDomain::Tv)
            .await
            .to_string();
        let client: String = c
            .query(
                "SELECT client_id FROM rss_candidates WHERE id=?",
                [candidate.clone()],
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        let scan = Uuid::new_v4().to_string();
        let hash = "b".repeat(40);
        c.execute("INSERT INTO downloaded_scan_commands(id,media_type,source_path,requested_mode,requested_provider_id,requested_remote_id,priority,ready_order_at,next_attempt_at,created_at) VALUES(?,'tv','/downloads/show','auto',?,?,0,1,1,1)", params![scan.clone(), client.clone(), hash.clone()]).await.unwrap();
        c.execute("UPDATE downloaded_scan_commands SET status='running',attempts=1,run_generation=1,started_at=2,next_attempt_at=9007199254740991 WHERE id=?", [scan.clone()]).await.unwrap();
        c.execute("UPDATE downloaded_scan_commands SET candidate_id=?1,coverage_state='pending',tracked_json=json_object('version',1,'provider_id',?2,'provider_revision',1,'remote_id',?3),processing_fence_json=(SELECT json_object('version',1,'policy_revision',policy_revision,'status',status,'resume_requested',resume_requested,'preflight_attempts',preflight_attempts,'total_preflight_attempts',total_preflight_attempts,'next_attempt_at',next_attempt_at,'created_at',created_at,'updated_at',updated_at,'error_code',error_code,'reasons_json',reasons_json,'scan_command_id',scan_command_id) FROM download_processing WHERE candidate_id=?1) WHERE id=?4", params![candidate.clone(), client.clone(), hash, scan]).await.unwrap();
        // The queued receipt is due, but only the scan may hand it back.
        assert!(claim(&db).await.unwrap().is_none());
        // A policy that no longer authorizes the receipt must not make recovery rewrite it.
        c.execute("UPDATE download_processing_policies SET enabled=0,revision=revision+1 WHERE provider_id=?", [client]).await.unwrap();
        recover(&db, "storage_error").await.unwrap();
        let status: String = c
            .query(
                "SELECT status FROM download_processing WHERE candidate_id=?",
                [candidate],
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        assert_eq!(status, "queued");
    }
    async fn blocked_client(c: &Connection, provider: Uuid, media: MediaDomain, timestamp: i64) {
        c.execute("INSERT INTO download_client_status(provider_id,media_type,config_revision,escalation_level,initial_failure_at,last_failure_at,disabled_until,next_health_at) VALUES(?,?,1,3,?,?,?,?)",params![provider.to_string(),domain(media),timestamp-600,timestamp-10,timestamp+900,timestamp+900]).await.unwrap();
    }
    fn refresh_input(provider: Uuid, media: MediaDomain) -> CommandInput {
        CommandInput {
            name: CommandName::RefreshDownloads,
            target: RefreshTarget {
                provider_id: provider,
                media_type: media,
            },
            provider_revision: 1,
            priority: CommandPriority::Normal,
        }
    }
    #[tokio::test]
    async fn client_backoff_preserves_due_attempts_across_reopen_and_manual_coalescing_bypasses() {
        let (scratch, db, c, provider) = health_refresh_fixture().await;
        let timestamp = now().unwrap();
        for media in [MediaDomain::Tv, MediaDomain::Movies] {
            blocked_client(&c, provider, media, timestamp).await;
            let automatic = enqueue_origin(
                &c,
                refresh_input(provider, media),
                timestamp - 100,
                CommandOrigin::Automatic,
            )
            .await
            .unwrap();
            assert_eq!(
                automatic.suppression_reason.as_deref(),
                Some("download_client_backoff")
            );
            assert_eq!(automatic.suppression_until, Some(timestamp + 900));
        }
        assert!(claim(&db).await.unwrap().is_none());
        assert_eq!(scalar(&c, "SELECT sum(attempts) FROM commands").await, 0);
        assert_eq!(
            scalar(&c, "SELECT count(*) FROM download_refresh_snapshots").await,
            0
        );
        drop(c);
        drop(db);
        let db = Database::open_local(scratch.0.join("db")).await.unwrap();
        let c = db.connect().await.unwrap();
        assert!(claim(&db).await.unwrap().is_none());
        for media in [MediaDomain::Tv, MediaDomain::Movies] {
            let manual = enqueue(&c, refresh_input(provider, media), timestamp)
                .await
                .unwrap();
            assert!(matches!(manual.origin, CommandOrigin::Automatic));
            assert!(manual.manual_bypass);
            assert_eq!(manual.next_attempt_at, timestamp - 100);
            assert!(manual.suppression_until.is_none());
            let Some(Claimed::Downloads(accepted)) = claim(&db).await.unwrap() else {
                panic!("manual coalescing must admit blocked automatic command")
            };
            assert_eq!(accepted.id, manual.id);
            assert_eq!(accepted.attempts, 1);
            publish(&db, accepted, Ok(vec![]), true).await.unwrap();
        }
        assert_eq!(
            scalar(&c, "SELECT count(*) FROM download_refresh_snapshots").await,
            2
        );
        assert_eq!(
            scalar(
                &c,
                "SELECT count(*) FROM download_client_status WHERE disabled_until IS NOT NULL"
            )
            .await,
            0
        );
    }
    #[tokio::test]
    async fn client_backoff_filters_schedules_before_limit_and_claim_before_attempt() {
        let (_scratch, db, c, provider) = health_refresh_fixture().await;
        let timestamp = now().unwrap();
        // Isolate the scheduler's 64-row selection bound from the separate 64-config admission cap.
        c.execute("DROP TRIGGER refresh_schedule_admit", ())
            .await
            .unwrap();
        for n in 0..65 {
            let id = if n == 64 { provider } else { Uuid::new_v4() };
            if n != 64 {
                c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'qbittorrent','Blocked',1,1,1,1,'http://127.0.0.1:1/')",[id.to_string()]).await.unwrap();
                c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags) VALUES(?,'qbittorrent','tv','tv',0,1,'started','default',0,0,0)",[id.to_string()]).await.unwrap();
                blocked_client(&c, id, MediaDomain::Tv, timestamp).await;
            }
            c.execute("INSERT INTO download_refresh_schedules(provider_id,media_type,provider_revision,enabled,interval_seconds,next_run_at,intent,requested_enabled) VALUES(?,'tv',1,1,60,?,'explicit',1)",params![id.to_string(),timestamp-100+n]).await.unwrap();
        }
        schedule_due(&c, timestamp).await.unwrap();
        assert_eq!(scalar(&c, "SELECT count(*) FROM commands").await, 1);
        assert_eq!(
            scalar(
                &c,
                "SELECT count(*) FROM download_refresh_schedules WHERE last_run_at IS NULL"
            )
            .await,
            64
        );
        let Some(Claimed::Downloads(eligible)) = claim(&db).await.unwrap() else {
            panic!("eligible schedule starved behind blocked rows")
        };
        assert_eq!(eligible.target.provider_id, provider);
        publish(&db, eligible, Ok(vec![]), true).await.unwrap();
        blocked_client(&c, provider, MediaDomain::Tv, timestamp).await;
        let mut high = refresh_input(provider, MediaDomain::Tv);
        high.priority = CommandPriority::High;
        let blocked = enqueue_origin(&c, high, timestamp - 100, CommandOrigin::Automatic)
            .await
            .unwrap();
        let movies = enqueue_origin(
            &c,
            refresh_input(provider, MediaDomain::Movies),
            timestamp,
            CommandOrigin::Automatic,
        )
        .await
        .unwrap();
        let Some(Claimed::Downloads(eligible)) = claim(&db).await.unwrap() else {
            panic!("eligible movie command starved")
        };
        assert_eq!(eligible.id, movies.id);
        let blocked = read_command(&c, blocked.id).await.unwrap();
        assert_eq!(blocked.attempts, 0);
        assert_eq!(blocked.next_attempt_at, timestamp - 100);
        assert!(matches!(blocked.status, CommandStatus::Queued));
    }
    #[tokio::test]
    async fn client_backoff_publication_is_atomic_and_rejects_stale_cancelled_and_local_failures() {
        let (_scratch, db, c, provider) = health_refresh_fixture().await;
        let timestamp = now().unwrap();
        blocked_client(&c, provider, MediaDomain::Tv, timestamp).await;
        let command = health_running_refresh(&c, provider, MediaDomain::Tv).await;
        let id = command.id;
        c.execute("CREATE TRIGGER fail_snapshot BEFORE INSERT ON download_refresh_snapshots BEGIN SELECT RAISE(ABORT,'fixture'); END",()).await.unwrap();
        assert!(publish(&db, command, Ok(vec![]), true).await.is_err());
        assert_eq!(
            scalar(&c, "SELECT escalation_level FROM download_client_status").await,
            3
        );
        assert!(matches!(
            read_command(&c, id).await.unwrap().status,
            CommandStatus::Running
        ));
        c.execute("DROP TRIGGER fail_snapshot", ()).await.unwrap();
        let cancelled = read_command(&c, id).await.unwrap();
        c.execute(
            "UPDATE commands SET status='cancelled',completed_at=? WHERE id=?",
            params![timestamp, id.to_string()],
        )
        .await
        .unwrap();
        publish(&db, cancelled, Ok(vec![]), true).await.unwrap();
        assert_eq!(
            scalar(&c, "SELECT escalation_level FROM download_client_status").await,
            3
        );
        let local = health_running_refresh(&c, provider, MediaDomain::Tv).await;
        publish(
            &db,
            local,
            Err(RefreshError {
                code: "refresh_limit",
                retryable: false,
                retry_after_seconds: None,
                client_failure: false,
                definitely_not_submitted: false,
                client_retry_kind: crate::providers::ClientRetryKind::Other,
                client_retry_eligible: false,
            }),
            true,
        )
        .await
        .unwrap();
        assert_eq!(
            scalar(&c, "SELECT escalation_level FROM download_client_status").await,
            3
        );
        let stale = health_running_refresh(&c, provider, MediaDomain::Tv).await;
        c.execute(
            "UPDATE providers SET revision=revision+1 WHERE id=?",
            [provider.to_string()],
        )
        .await
        .unwrap();
        publish(
            &db,
            stale,
            Err(RefreshError {
                code: "refresh_failed",
                retryable: true,
                retry_after_seconds: None,
                client_failure: true,
                definitely_not_submitted: false,
                client_retry_kind: crate::providers::ClientRetryKind::Other,
                client_retry_eligible: false,
            }),
            true,
        )
        .await
        .unwrap();
        assert_eq!(
            scalar(&c, "SELECT escalation_level FROM download_client_status").await,
            3
        );
        assert_eq!(
            scalar(&c, "SELECT count(*) FROM download_refresh_snapshots").await,
            0
        );
    }

    #[tokio::test]
    async fn client_backoff_fresh_send_recovery_is_atomic_and_once_per_receipt() {
        for media in [MediaDomain::Tv, MediaDomain::Movies] {
            let (_scratch, _db, c, _provider) = health_refresh_fixture().await;
            c.execute_batch("INSERT INTO series(id,title,path) VALUES(1,'Show','/fixture/show'); INSERT INTO seasons(series_id,number) VALUES(1,1); INSERT INTO episodes(id,series_id,season,number,title) VALUES(1,1,1,1,'Pilot'); INSERT INTO movie_metadata(id,title,year) VALUES(1,'Movie',2020); INSERT INTO movies(id,metadata_id,path) VALUES(1,1,'/fixture/movie');").await.unwrap();
            let id = observed_candidate(&c, 1, 1, false, media).await;
            let row = c
                .query(
                    "SELECT indexer_id,client_id FROM rss_candidates WHERE id=?",
                    [id.to_string()],
                )
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap();
            let source = rss::RssTarget {
                media_type: media,
                indexer_id: Uuid::parse_str(&row.get::<String>(0).unwrap()).unwrap(),
                indexer_revision: 1,
                selection_kind: rss::SelectionKind::LegacyFixed,
                client_id: Some(Uuid::parse_str(&row.get::<String>(1).unwrap()).unwrap()),
                client_revision: Some(1),
            };
            drop(row);
            blocked_client(&c, source.client_id.unwrap(), media, now().unwrap()).await;
            let other = if matches!(media, MediaDomain::Tv) {
                MediaDomain::Movies
            } else {
                MediaDomain::Tv
            };
            c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags) VALUES(?,'qbittorrent',?,?,0,1,'started','default',0,0,0)",params![source.client_id.unwrap().to_string(),domain(other),domain(other)]).await.unwrap();
            blocked_client(&c, source.client_id.unwrap(), other, now().unwrap()).await;
            c.execute("CREATE TRIGGER reject_backoff BEFORE UPDATE ON download_client_status BEGIN SELECT RAISE(ABORT,'fixture'); END",()).await.unwrap();
            assert!(
                rss::accept_observed(&c, id, source, "b".repeat(40), true)
                    .await
                    .is_err()
            );
            assert_eq!(
                scalar(
                    &c,
                    "SELECT count(*) FROM rss_candidates WHERE status='submitting'"
                )
                .await,
                1
            );
            assert_eq!(
                scalar(
                    &c,
                    &format!(
                        "SELECT escalation_level FROM download_client_status WHERE media_type='{}'",
                        domain(media)
                    )
                )
                .await,
                3
            );
            c.execute("DROP TRIGGER reject_backoff", ()).await.unwrap();
            rss::accept_observed(&c, id, source, "b".repeat(40), true)
                .await
                .unwrap();
            assert_eq!(
                scalar(
                    &c,
                    &format!(
                        "SELECT escalation_level FROM download_client_status WHERE media_type='{}'",
                        domain(media)
                    )
                )
                .await,
                2
            );
            assert_eq!(
                scalar(
                    &c,
                    "SELECT count(*) FROM rss_candidates WHERE status='observed'"
                )
                .await,
                1
            );
            rss::accept_observed(&c, id, source, "b".repeat(40), true)
                .await
                .unwrap();
            assert_eq!(
                scalar(
                    &c,
                    &format!(
                        "SELECT escalation_level FROM download_client_status WHERE media_type='{}'",
                        domain(media)
                    )
                )
                .await,
                2
            );
            assert_eq!(scalar(&c, &format!("SELECT escalation_level FROM download_client_status WHERE media_type='{}' AND disabled_until IS NOT NULL",domain(other))).await,3);
        }
    }
    #[tokio::test]
    async fn client_backoff_accepted_failure_and_expiry_keep_history_and_legacy_work_admissible() {
        let (_scratch, db, c, provider) = health_refresh_fixture().await;
        let timestamp = now().unwrap();
        blocked_client(&c, provider, MediaDomain::Tv, timestamp).await;
        let command = health_running_refresh(&c, provider, MediaDomain::Tv).await;
        let old_attempt = read_command(&c, command.id).await.unwrap();
        publish(
            &db,
            command,
            Err(RefreshError {
                code: "refresh_failed",
                retryable: true,
                retry_after_seconds: None,
                client_failure: true,
                definitely_not_submitted: false,
                client_retry_kind: crate::providers::ClientRetryKind::Other,
                client_retry_eligible: false,
            }),
            true,
        )
        .await
        .unwrap();
        assert_eq!(
            scalar(&c, "SELECT escalation_level FROM download_client_status").await,
            4
        );
        c.execute(
            "UPDATE commands SET status='running',attempts=attempts+1,error_code=NULL WHERE id=?",
            [old_attempt.id.to_string()],
        )
        .await
        .unwrap();
        publish(&db, old_attempt, Ok(vec![]), true).await.unwrap();
        assert_eq!(
            scalar(&c, "SELECT escalation_level FROM download_client_status").await,
            4
        );
        c.execute("UPDATE commands SET status='cancelled',completed_at=?,error_code=NULL WHERE status='running'",[timestamp]).await.unwrap();
        // Unknown historical origin must not be silently reclassified as automatic.
        let legacy = enqueue_origin(
            &c,
            refresh_input(provider, MediaDomain::Tv),
            timestamp,
            CommandOrigin::LegacyUnknown,
        )
        .await
        .unwrap();
        assert!(legacy.suppression_until.is_none());
        let coalesced = enqueue(&c, refresh_input(provider, MediaDomain::Tv), timestamp)
            .await
            .unwrap();
        assert_eq!(coalesced.id, legacy.id);
        assert!(matches!(coalesced.origin, CommandOrigin::LegacyUnknown));
        assert!(!coalesced.manual_bypass);
        let Some(Claimed::Downloads(accepted)) = claim(&db).await.unwrap() else {
            panic!("legacy command must remain admissible")
        };
        assert_eq!(accepted.id, legacy.id);
        c.execute(
            "UPDATE commands SET status='cancelled',completed_at=? WHERE id=?",
            params![timestamp, accepted.id.to_string()],
        )
        .await
        .unwrap();
        // Expiry admits the original automatic due work without resetting its failure history.
        c.execute("UPDATE download_client_status SET initial_failure_at=?,last_failure_at=?,disabled_until=?,next_health_at=?",params![timestamp-1000,timestamp-900,timestamp-1,timestamp-1]).await.unwrap();
        let automatic = enqueue_origin(
            &c,
            refresh_input(provider, MediaDomain::Tv),
            timestamp - 100,
            CommandOrigin::Automatic,
        )
        .await
        .unwrap();
        let Some(Claimed::Downloads(accepted)) = claim(&db).await.unwrap() else {
            panic!("expired delay must admit automatic command")
        };
        assert_eq!(accepted.id, automatic.id);
        assert_eq!(accepted.next_attempt_at, timestamp - 100);
        assert_eq!(
            scalar(&c, "SELECT escalation_level FROM download_client_status").await,
            4
        );
    }
    #[tokio::test]
    async fn scan_continuations_order_by_ready_time_so_same_priority_work_interleaves() {
        let path = std::env::temp_dir().join(format!("hrrdarr-scan-fairness-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        let _scratch = Scratch(path.clone());
        let db = Database::open_local(path.join("db")).await.unwrap();
        let c = db.connect().await.unwrap();
        let scan = Uuid::new_v4().to_string();
        let reset = Uuid::new_v4().to_string();
        c.execute("INSERT INTO downloaded_scan_commands(id,media_type,source_path,requested_mode,ready_order_at,next_attempt_at,created_at) VALUES(?,'tv','/scan/source','copy',1000,1000,1000)", [scan.clone()]).await.unwrap();
        c.execute("INSERT INTO quality_reset_commands(id,media_type,reset_titles,priority,status,attempts,next_attempt_at,created_at) VALUES(?,'tv',0,0,'queued',0,1000,2000)", [reset.clone()]).await.unwrap();
        // The older scan runs first; a unpublished continuation re-enters behind later work.
        let Some(Claimed::DownloadedScan(first)) = claim(&db).await.unwrap() else {
            panic!("older scan must be first")
        };
        assert_eq!(first.id.to_string(), scan);
        downloaded_scan::recover(&c, now().unwrap(), "interrupted")
            .await
            .unwrap();
        let Some(Claimed::QualityReset(second)) = claim(&db).await.unwrap() else {
            panic!("same-priority unrelated work must run between scan claims")
        };
        assert_eq!(second.id.to_string(), reset);
        // Cancelled scans are never claimed again; the unrelated pool order is untouched.
        c.execute(
            "UPDATE quality_reset_commands SET status='cancelled',completed_at=1 WHERE id=?",
            [reset],
        )
        .await
        .unwrap();
        let Some(Claimed::DownloadedScan(third)) = claim(&db).await.unwrap() else {
            panic!("scan continues once nothing else is due")
        };
        assert_eq!(third.id.to_string(), scan);
    }
}
