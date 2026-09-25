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
    let task = tokio::spawn(async move {
        let _owner = owner;
        // ponytail: one worker per locally owned DB; add weighted concurrency only with real jobs needing it.
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            let result = tokio::time::timeout(Duration::from_secs(45), step(&db, &client)).await;
            if !matches!(result, Ok(Ok(()))) {
                eprintln!("event=command_worker_error code=storage_error");
                // The failed future is gone, so repeating this read-only command is safe. Recovery
                // never resets attempts and never applies to future mutating commands implicitly.
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
async fn recover(db: &Database, code: &str) -> Result<()> {
    let c = connection(db).await?;
    let timestamp = now()?;
    c.execute("UPDATE commands SET status=CASE WHEN attempts<3 THEN 'retry_wait' ELSE 'failed' END,next_attempt_at=?,completed_at=CASE WHEN attempts<3 THEN NULL ELSE ? END,error_code=? WHERE status='running'",params![timestamp,timestamp,code]).await?;
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
async fn claim(db: &Database) -> Result<Option<Command>> {
    let c = connection(db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let result=async{
        let timestamp=now()?;
        tx.execute("UPDATE commands SET status='failed',completed_at=?,error_code='provider_changed' WHERE status IN ('queued','retry_wait') AND NOT EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=commands.provider_id AND p.revision=commands.provider_revision AND p.enabled=1 AND p.implementation='qbittorrent' AND s.media_type=commands.media_type)",[timestamp]).await?;
        schedule_due(&tx,timestamp).await?;
        let row=tx.query(&format!("SELECT {COMMAND_COLUMNS} FROM commands WHERE status IN ('queued','retry_wait') AND next_attempt_at<=? ORDER BY priority DESC,created_at,id LIMIT 1"),[timestamp]).await?.next().await?;
        let Some(row)=row else{return Ok(None)};
        let command=command_row(row)?;
        if !valid_provider(&tx,command.target,command.provider_revision).await?{
            tx.execute("UPDATE commands SET status='failed',completed_at=?,error_code='provider_changed' WHERE id=?",params![timestamp,command.id.to_string()]).await?;return Ok(None)
        }
        tx.execute("UPDATE commands SET status='running',attempts=attempts+1,started_at=?,error_code=NULL WHERE id=?",params![timestamp,command.id.to_string()]).await?;
        Ok(Some(read_command(&tx,command.id).await?))
    }.await;
    finish(tx, result).await
}
async fn snapshot_capacity(c: &Connection, target: RefreshTarget) -> Result<bool> {
    Ok(c.query("SELECT (SELECT count(*) FROM download_refresh_snapshots)<? OR EXISTS(SELECT 1 FROM download_refresh_snapshots WHERE provider_id=? AND media_type=?)",params![MAX_SNAPSHOTS,target.provider_id.to_string(),domain(target.media_type)]).await?.next().await?.ok_or_else(bad)?.get::<i64>(0)?==1)
}
async fn step(db: &Arc<Database>, client: &RefreshClient) -> Result<()> {
    // A prior local storage failure can leave an interrupted read claim. There is no other worker.
    recover(db, "storage_error").await?;
    let Some(command) = claim(db).await? else {
        return Ok(());
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
                    let current=read_command(&connection(db).await?,id).await?;
                    if !matches!(current.status,CommandStatus::Running){return Ok(())}
                }
            }
        }
    };
    publish(db, command, result).await
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
        let current=read_command(&tx,command.id).await?;
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
