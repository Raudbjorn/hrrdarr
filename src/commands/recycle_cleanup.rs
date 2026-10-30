//! CleanUpRecycleBin (job.004): removes recycled files older than the domain's cleanup days from
//! the configured bin, then empty subfolders. Manual or scheduled once per day per domain. The bin
//! and retention are captured when the command runs; nothing outside the bin is ever visited.
use super::*;
use crate::recycle_bin::fsops;

/// Bounds for one run; whatever is not visited is kept for the next run.
const MAX_DEPTH: usize = 32;
const MAX_ENTRIES: usize = 100_000;
const MAX_REMOVALS: u64 = 10_000;
// Keep the walk below the worker's 45 s step timeout.
const RUN_SECONDS: u64 = 30;
const BUSY_RETRY_SECONDS: i64 = 30;
/// Returns the retention applied to one bin. A shared bin has no per-entry domain marker, so the
/// longest retention wins; zero disables cleanup for the shared path.
fn effective_cleanup_days(own: i64, other: Option<i64>, shared: bool) -> Option<i64> {
    if !shared {
        return (own > 0).then_some(own);
    }
    let Some(other) = other else {
        return (own > 0).then_some(own);
    };
    if own == 0 || other == 0 {
        return None;
    }
    Some(own.max(other))
}

#[derive(Clone, Copy, Deserialize, Serialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct RecycleBinCleanupTarget {
    pub media_type: MediaDomain,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct RecycleBinCleanupInput {
    pub target: RecycleBinCleanupTarget,
    pub priority: CommandPriority,
}
#[derive(Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum RecycleBinCleanupName {
    CleanUpRecycleBin,
}
#[derive(Clone, Copy, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum RecycleBinCleanupOrigin {
    Manual,
    Automatic,
}
#[derive(Clone, Copy, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum RecycleBinCleanupOutcome {
    Cleaned,
    NotConfigured,
    Disabled,
}
#[derive(Serialize, ts_rs::TS)]
pub struct RecycleBinCleanupCommand {
    pub id: Uuid,
    pub name: RecycleBinCleanupName,
    pub target: RecycleBinCleanupTarget,
    pub origin: RecycleBinCleanupOrigin,
    pub priority: CommandPriority,
    pub status: CommandStatus,
    pub attempts: u8,
    pub next_attempt_at: i64,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub error_code: Option<String>,
    pub outcome: Option<RecycleBinCleanupOutcome>,
    pub bin_path: Option<String>,
    pub cleanup_days: Option<i64>,
    pub files_removed: i64,
    pub folders_removed: i64,
    pub entries_skipped: i64,
    pub entries_failed: i64,
    // A run bound was reached; remaining expired entries are kept for the next run.
    pub bounded: bool,
}
const COLUMNS: &str = "id,media_type,origin,priority,status,attempts,next_attempt_at,created_at,started_at,completed_at,error_code,outcome,bin_path,cleanup_days,files_removed,folders_removed,entries_skipped,entries_failed,bounded";
fn row(r: libsql::Row) -> Result<RecycleBinCleanupCommand> {
    Ok(RecycleBinCleanupCommand {
        id: Uuid::parse_str(&r.get::<String>(0)?).map_err(|_| bad())?,
        name: RecycleBinCleanupName::CleanUpRecycleBin,
        target: RecycleBinCleanupTarget {
            media_type: MediaDomain::parse(&r.get::<String>(1)?).map_err(|_| bad())?,
        },
        origin: match r.get::<String>(2)?.as_str() {
            "manual" => RecycleBinCleanupOrigin::Manual,
            "automatic" => RecycleBinCleanupOrigin::Automatic,
            _ => return Err(bad()),
        },
        priority: if r.get::<i64>(3)? == 1 {
            CommandPriority::High
        } else {
            CommandPriority::Normal
        },
        status: CommandStatus::parse(&r.get::<String>(4)?)?,
        attempts: r.get::<i64>(5)? as u8,
        next_attempt_at: r.get(6)?,
        created_at: r.get(7)?,
        started_at: r.get(8)?,
        completed_at: r.get(9)?,
        error_code: r.get(10)?,
        outcome: match r.get::<Option<String>>(11)?.as_deref() {
            None => None,
            Some("cleaned") => Some(RecycleBinCleanupOutcome::Cleaned),
            Some("not_configured") => Some(RecycleBinCleanupOutcome::NotConfigured),
            Some("disabled") => Some(RecycleBinCleanupOutcome::Disabled),
            Some(_) => return Err(bad()),
        },
        bin_path: r.get(12)?,
        cleanup_days: r.get(13)?,
        files_removed: r.get(14)?,
        folders_removed: r.get(15)?,
        entries_skipped: r.get(16)?,
        entries_failed: r.get(17)?,
        bounded: r.get::<i64>(18)? == 1,
    })
}
async fn read(c: &Connection, id: Uuid) -> Result<RecycleBinCleanupCommand> {
    row(c
        .query(
            &format!("SELECT {COLUMNS} FROM recycle_bin_cleanup_commands WHERE id=?"),
            [id.to_string()],
        )
        .await?
        .next()
        .await?
        .ok_or(Error(
            StatusCode::NOT_FOUND,
            "recycle_bin_cleanup_command_not_found",
        ))?)
}
async fn retained(c: &Connection, id: Uuid) -> Result<Option<RecycleBinCleanupCommand>> {
    match read(c, id).await {
        Ok(v) => Ok(Some(v)),
        Err(Error(StatusCode::NOT_FOUND, "recycle_bin_cleanup_command_not_found")) => Ok(None),
        Err(e) => Err(e),
    }
}
pub(super) fn router(db: Arc<Database>) -> Router {
    Router::new()
        .route(
            "/api/v1/recycle-bin/cleanup-commands",
            get(list).post(create),
        )
        .route("/api/v1/recycle-bin/cleanup-commands/{id}", get(detail))
        .route(
            "/api/v1/recycle-bin/cleanup-commands/{id}/cancel",
            post(cancel),
        )
        .layer(DefaultBodyLimit::max(8192))
        .layer(axum::middleware::from_fn(request_deadline))
        .with_state(db)
}
async fn request_deadline(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    match tokio::time::timeout(std::time::Duration::from_secs(5), next.run(request)).await {
        Ok(response) => response,
        Err(_) => Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "recycle_bin_cleanup_timeout",
        )
        .into_response(),
    }
}
/// Inserts (or returns the existing active) command for a domain. Caller owns the transaction.
async fn enqueue(
    tx: &Connection,
    media: MediaDomain,
    priority: CommandPriority,
    origin: &str,
    timestamp: i64,
) -> Result<RecycleBinCleanupCommand> {
    if let Some(r) = tx
        .query(
            &format!("SELECT {COLUMNS} FROM recycle_bin_cleanup_commands WHERE media_type=? AND status IN ('queued','running','retry_wait')"),
            [domain(media)],
        )
        .await?
        .next()
        .await?
    {
        return row(r);
    }
    if command_capacity(tx).await? >= MAX_COMMANDS {
        return Err(Error(StatusCode::TOO_MANY_REQUESTS, "command_history_full"));
    }
    let id = Uuid::new_v4();
    tx.execute(
        "INSERT INTO recycle_bin_cleanup_commands(id,name,media_type,origin,priority,status,attempts,next_attempt_at,created_at)VALUES(?,'clean_up_recycle_bin',?,?,?,'queued',0,?,?)",
        params![id.to_string(), domain(media), origin, priority.number(), timestamp, timestamp],
    )
    .await?;
    read(tx, id).await
}
async fn create(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
    input: std::result::Result<Json<RecycleBinCleanupInput>, JsonRejection>,
) -> Result<(StatusCode, Json<RecycleBinCleanupCommand>)> {
    q.map_err(|_| bad())?;
    let input = input.map_err(|_| bad())?.0;
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        bounded(
            enqueue(
                &tx,
                input.target.media_type,
                input.priority,
                "manual",
                now()?,
            )
            .await?,
        )
    }
    .await;
    Ok((StatusCode::ACCEPTED, finish(tx, outcome).await?))
}
async fn list(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<CommandQuery>, QueryRejection>,
) -> Result<Json<ApiPage<RecycleBinCleanupCommand>>> {
    let q = q.map_err(|_| bad())?.0;
    if !(1..=100).contains(&q.limit) || q.offset > MAX_COMMANDS as u32 {
        return Err(bad());
    }
    let media = q.media_type.map(domain);
    let status = q.status.map(CommandStatus::text);
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::ReadOnly)
        .await?;
    let outcome = async {
        let predicate = "WHERE (? IS NULL OR media_type=?) AND (? IS NULL OR status=?)";
        let total = tx
            .query(
                &format!("SELECT count(*) FROM recycle_bin_cleanup_commands {predicate}"),
                params![media, media, status, status],
            )
            .await?
            .next()
            .await?
            .ok_or_else(bad)?
            .get(0)?;
        let mut rows = tx
            .query(
                &format!("SELECT {COLUMNS} FROM recycle_bin_cleanup_commands {predicate} ORDER BY created_at DESC,id DESC LIMIT ? OFFSET ?"),
                params![media, media, status, status, i64::from(q.limit), i64::from(q.offset)],
            )
            .await?;
        let mut items = vec![];
        while let Some(r) = rows.next().await? {
            items.push(row(r)?)
        }
        bounded(ApiPage {
            items,
            total,
            limit: q.limit,
            offset: q.offset,
        })
    }
    .await;
    finish(tx, outcome).await
}
async fn detail(
    State(db): State<Arc<Database>>,
    Path(id): Path<String>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
) -> Result<Json<RecycleBinCleanupCommand>> {
    q.map_err(|_| bad())?;
    bounded(
        read(
            &connection(&db).await?,
            Uuid::parse_str(&id).map_err(|_| bad())?,
        )
        .await?,
    )
}
async fn cancel(
    State(db): State<Arc<Database>>,
    Path(id): Path<String>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
) -> Result<Json<RecycleBinCleanupCommand>> {
    q.map_err(|_| bad())?;
    let id = Uuid::parse_str(&id).map_err(|_| bad())?;
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    // Running cleanup cannot be interrupted mid-walk; only queued or waiting runs cancel.
    let outcome = async {
        tx.execute(
            "UPDATE recycle_bin_cleanup_commands SET status='cancelled',completed_at=?,error_code=NULL WHERE id=? AND status IN ('queued','retry_wait')",
            params![now()?, id.to_string()],
        )
        .await?;
        bounded(read(&tx, id).await?)
    }
    .await;
    finish(tx, outcome).await
}
/// Enqueues automatic cleanup once per interval for domains with a bin and retention enabled.
/// Runs inside the worker's claim transaction.
pub(super) async fn schedule_due(c: &Connection, timestamp: i64) -> Result<()> {
    let interval = crate::recycle_bin::CLEANUP_INTERVAL_SECONDS;
    // A configured bin without a schedule (for example restored settings) waits one interval.
    c.execute(
        "UPDATE recycle_bin_settings SET next_cleanup_at=? WHERE path IS NOT NULL AND next_cleanup_at IS NULL",
        [timestamp + interval],
    )
    .await?;
    let mut rows = c
        .query(
            "SELECT media_type FROM recycle_bin_settings WHERE path IS NOT NULL AND cleanup_days>0 AND next_cleanup_at<=? ORDER BY media_type",
            [timestamp],
        )
        .await?;
    let mut due = Vec::new();
    while let Some(r) = rows.next().await? {
        due.push(MediaDomain::parse(&r.get::<String>(0)?).map_err(|_| bad())?);
    }
    drop(rows);
    for media in due {
        match enqueue(c, media, CommandPriority::Normal, "automatic", timestamp).await {
            Ok(_) | Err(Error(_, "command_history_full")) => {}
            Err(e) => return Err(e),
        }
        c.execute(
            "UPDATE recycle_bin_settings SET next_cleanup_at=? WHERE media_type=?",
            params![timestamp + interval, domain(media)],
        )
        .await?;
    }
    Ok(())
}
pub(super) async fn recover(c: &Connection, timestamp: i64, code: &str) -> Result<()> {
    c.execute("UPDATE recycle_bin_cleanup_commands SET status=CASE WHEN attempts<3 THEN 'retry_wait' ELSE 'failed' END,next_attempt_at=?,completed_at=CASE WHEN attempts<3 THEN NULL ELSE ? END,error_code=? WHERE status='running'",params![timestamp,timestamp,code]).await?;
    Ok(())
}
pub(super) async fn claim(
    c: &Connection,
    id: Uuid,
    timestamp: i64,
) -> Result<RecycleBinCleanupCommand> {
    c.execute("UPDATE recycle_bin_cleanup_commands SET status='running',attempts=attempts+1,started_at=?,error_code=NULL WHERE id=?",params![timestamp,id.to_string()]).await?;
    read(c, id).await
}
async fn settle(
    db: &Database,
    id: Uuid,
    sql: &str,
    values: impl libsql::params::IntoParams,
) -> Result<()> {
    connection(db).await?.execute(sql, values).await?;
    if let Some(done) = retained(&connection(db).await?, id).await? {
        eprintln!(
            "event=recycle_bin_cleanup_settled command_id={} media_type={} status={} files_removed={} folders_removed={} skipped={} failed={} bounded={}",
            done.id,
            domain(done.target.media_type),
            done.status.text(),
            done.files_removed,
            done.folders_removed,
            done.entries_skipped,
            done.entries_failed,
            done.bounded
        );
    }
    Ok(())
}
pub(super) async fn run(db: &Arc<Database>, command: RecycleBinCleanupCommand) -> Result<()> {
    let id = command.id;
    let media = command.target.media_type;
    let c = connection(db).await?;
    let Some(current) = retained(&c, id).await? else {
        return Ok(());
    };
    if !matches!(current.status, CommandStatus::Running) {
        return Ok(());
    }
    let settings = crate::recycle_bin::read(&c, media)
        .await
        .map_err(|e| Error(StatusCode::SERVICE_UNAVAILABLE, e.code()))?;
    let running = "WHERE id=? AND status='running'";
    let Some(bin) = settings.path.clone() else {
        // Upstream logs and returns when no bin is configured.
        return settle(db, id, &format!("UPDATE recycle_bin_cleanup_commands SET status='succeeded',completed_at=?,outcome='not_configured' {running}"), params![now()?, id.to_string()]).await;
    };
    let other_media = match media {
        MediaDomain::Tv => MediaDomain::Movies,
        MediaDomain::Movies => MediaDomain::Tv,
    };
    let other_settings = crate::recycle_bin::read(&c, other_media)
        .await
        .map_err(|e| Error(StatusCode::SERVICE_UNAVAILABLE, e.code()))?;
    drop(c);
    let shared = other_settings.path.as_deref() == Some(bin.as_str());
    let Some(days) = effective_cleanup_days(
        settings.cleanup_days,
        Some(other_settings.cleanup_days),
        shared,
    ) else {
        return settle(db, id, &format!("UPDATE recycle_bin_cleanup_commands SET status='succeeded',completed_at=?,outcome='disabled',bin_path=?,cleanup_days=0 {running}"), params![now()?, bin, id.to_string()]).await;
    };
    // Serialize with imports and explicit deletions; never walk a bin a transfer is writing.
    let guard = match crate::import::acquire_deletion_guard(db, &id.to_string()) {
        Ok(guard) => guard,
        Err(_) => {
            return settle(db, id, &format!("UPDATE recycle_bin_cleanup_commands SET status=CASE WHEN attempts<3 THEN 'retry_wait' ELSE 'failed' END,next_attempt_at=?,completed_at=CASE WHEN attempts<3 THEN NULL ELSE ? END,error_code='import_busy' {running}"), params![now()? + BUSY_RETRY_SECONDS, now()?, id.to_string()]).await;
        }
    };
    let walk_bin = bin.clone();
    let report = tokio::task::spawn_blocking(move || {
        let _guard = guard;
        let limits = fsops::CleanupLimits {
            max_depth: MAX_DEPTH,
            max_entries: MAX_ENTRIES,
            max_removals: MAX_REMOVALS,
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(RUN_SECONDS),
        };
        fsops::cleanup(&walk_bin, days, std::time::SystemTime::now(), &limits)
    })
    .await
    .map_err(|_| Error(StatusCode::INTERNAL_SERVER_ERROR, "command_storage_error"))?;
    match report {
        Ok(report) => {
            let count = |v: u64| i64::try_from(v).unwrap_or(MAX_REVISION).min(MAX_REVISION);
            settle(db, id, &format!("UPDATE recycle_bin_cleanup_commands SET status='succeeded',completed_at=?,outcome='cleaned',bin_path=?,cleanup_days=?,files_removed=?,folders_removed=?,entries_skipped=?,entries_failed=?,bounded=? {running}"), params![now()?, bin, days, count(report.files_removed), count(report.folders_removed), count(report.skipped), count(report.failed), i64::from(report.bounded), id.to_string()]).await
        }
        Err(error) => {
            let code = match error {
                fsops::FsError::Missing | fsops::FsError::Unsafe | fsops::FsError::Denied => {
                    "recycle_bin_unavailable"
                }
                _ => "filesystem_error",
            };
            // A missing or unsafe bin does not heal by retrying; Health reports it (hc.036).
            settle(db, id, &format!("UPDATE recycle_bin_cleanup_commands SET status='failed',completed_at=?,error_code=?,bin_path=?,cleanup_days=? {running}"), params![now()?, code, bin, days, id.to_string()]).await
        }
    }
}
