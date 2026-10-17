//! Durable, worker-driven execution of already-previewed manual imports.
//!
//! `crate::import::execute` acquires a single process-wide execution permit and blocks until the
//! whole phase pipeline reaches `complete` or errors (verified by reading `run_inner`: each
//! `if rec.phase==X {...}` block falls through into the next in the same call, so one call always
//! drains to a terminal outcome unless the permit itself could not be acquired, which returns
//! `import_busy` before touching anything). A command row's own `run()` therefore normally settles
//! in the same tick it was claimed. The only way a row is left `running` is the worker step's
//! 45-second timeout dropping the awaiting future while `execute`'s detached `tokio::spawn` task
//! (which owns the permit) keeps transferring bytes in the background. This module never treats
//! that as a failure: `import_journal` is the durable authority, and settlement always comes from
//! reading it, never from guessing based on elapsed time or a dropped future.
use super::*;
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ManualImportBatchInput {
    pub operation_ids: Vec<Uuid>,
    pub priority: CommandPriority,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ManualImportReconcileInput {
    pub operation_ids: Vec<Uuid>,
}
#[derive(Serialize, ts_rs::TS)]
pub struct ManualImportReconciliation {
    pub operation_id: Uuid,
    pub operation: Option<crate::api::Operation>,
    pub active_command: Option<ManualImportCommand>,
}
#[derive(Serialize, ts_rs::TS)]
pub struct ManualImportCommand {
    pub id: Uuid,
    pub batch_id: Uuid,
    pub operation_id: Uuid,
    pub priority: CommandPriority,
    pub status: CommandStatus,
    pub attempts: u8,
    pub next_attempt_at: i64,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub error_code: Option<String>,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ManualImportCommandQuery {
    #[ts(optional)]
    pub batch_id: Option<Uuid>,
    #[ts(optional)]
    pub status: Option<CommandStatus>,
    #[serde(default = "default_limit")]
    #[ts(as = "Option<u16>", optional)]
    pub limit: u16,
    #[serde(default)]
    #[ts(as = "Option<u32>", optional)]
    pub offset: u32,
}
const COLUMNS: &str = "id,batch_id,operation_id,priority,status,attempts,next_attempt_at,created_at,started_at,completed_at,error_code";
const BATCH_CAP: usize = 100;
fn row(r: libsql::Row) -> Result<ManualImportCommand> {
    Ok(ManualImportCommand {
        id: Uuid::parse_str(&r.get::<String>(0)?).map_err(|_| bad())?,
        batch_id: Uuid::parse_str(&r.get::<String>(1)?).map_err(|_| bad())?,
        operation_id: Uuid::parse_str(&r.get::<String>(2)?).map_err(|_| bad())?,
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
    })
}
async fn read(c: &Connection, id: Uuid) -> Result<ManualImportCommand> {
    row(c
        .query(
            &format!("SELECT {COLUMNS} FROM manual_import_commands WHERE id=?"),
            [id.to_string()],
        )
        .await?
        .next()
        .await?
        .ok_or(Error(
            StatusCode::NOT_FOUND,
            "manual_import_command_not_found",
        ))?)
}
async fn retained(c: &Connection, id: Uuid) -> Result<Option<ManualImportCommand>> {
    match read(c, id).await {
        Ok(v) => Ok(Some(v)),
        Err(Error(StatusCode::NOT_FOUND, "manual_import_command_not_found")) => Ok(None),
        Err(e) => Err(e),
    }
}
struct Journal {
    phase: String,
    error_code: Option<String>,
    path_failure_code: Option<String>,
}
async fn journal(c: &Connection, operation_id: Uuid) -> Result<Option<Journal>> {
    let Some(r) = c
        .query(
            "SELECT phase,error_code,CASE WHEN path_failure_code IS NULL THEN NULL WHEN typeof(path_failure_code)='text' THEN substr(CAST(path_failure_code AS BLOB),1,65) ELSE x'00' END FROM import_journal WHERE operation_id=?",
            [operation_id.to_string()],
        )
        .await?
        .next()
        .await?
    else {
        return Ok(None);
    };
    Ok(Some(Journal {
        phase: r.get(0)?,
        error_code: r.get(1)?,
        path_failure_code: crate::import::checked_path_failure(&r, 2).map_err(|_| {
            Error(
                StatusCode::SERVICE_UNAVAILABLE,
                "manual_import_journal_unavailable",
            )
        })?,
    }))
}
// The DB CHECK on error_code is structural ([a-z0-9_]{1,64}), not a closed enum, so any
// crate::import::Error code or import_journal.error_code value already satisfies it -- this only
// guards against a future value that doesn't, so a settle attempt can never fail the CHECK and
// strand the row in 'running' with no further claimable path.
fn normalize(code: &str) -> &str {
    if !code.is_empty()
        && code.len() <= 64
        && code
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        code
    } else {
        "import_failed"
    }
}
pub(super) fn router(db: Arc<Database>) -> Router {
    Router::new()
        .route("/api/v1/manual-import/commands", get(list).post(create))
        .route("/api/v1/manual-import/commands/reconcile", post(reconcile))
        .route("/api/v1/manual-import/commands/{id}", get(detail))
        .route("/api/v1/manual-import/commands/{id}/cancel", post(cancel))
        .layer(DefaultBodyLimit::max(8192))
        .layer(axum::middleware::from_fn(deadline))
        .with_state(db)
}
async fn reconcile(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
    input: std::result::Result<Json<ManualImportReconcileInput>, JsonRejection>,
) -> Result<Json<Vec<ManualImportReconciliation>>> {
    q.map_err(|_| bad())?;
    let ids = input.map_err(|_| bad())?.0.operation_ids;
    if ids.is_empty()
        || ids.len() > BATCH_CAP
        || ids.iter().collect::<std::collections::BTreeSet<_>>().len() != ids.len()
    {
        return Err(bad());
    }
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::ReadOnly)
        .await?;
    let outcome = async {
        let mut result = Vec::with_capacity(ids.len());
        for operation_id in ids {
            let key = operation_id.to_string();
            // Missing operations are explicit; other canonical projection failures fail the
            // whole snapshot rather than silently presenting partial reconciliation as complete.
            let operation = match crate::import::status_in(&tx, &key).await {
                Ok(operation) => Some(operation),
                Err(error) if error.code() == "not_found" => None,
                Err(_) => return Err(Error(StatusCode::SERVICE_UNAVAILABLE, "manual_import_journal_unavailable")),
            };
            let active_command = if operation.is_some() {
                // The partial unique index bounds this lookup independently of retained history.
                // Read a second row defensively; corrupted uniqueness must not select an arbitrary owner.
                let mut rows = tx.query(&format!("SELECT {COLUMNS} FROM manual_import_commands WHERE operation_id=? AND status IN ('queued','retry_wait','running') LIMIT 2"), [key]).await?;
                let command = rows.next().await?.map(row).transpose()?;
                if rows.next().await?.is_some() { return Err(Error(StatusCode::SERVICE_UNAVAILABLE, "manual_import_journal_unavailable")); }
                command
            } else { None };
            result.push(ManualImportReconciliation { operation_id, operation, active_command });
        }
        bounded(result)
    }.await;
    // This snapshot neither settles an earlier HTTP request nor reserves future admission.
    finish(tx, outcome).await
}
async fn deadline(request: axum::extract::Request, next: axum::middleware::Next) -> Response {
    match tokio::time::timeout(std::time::Duration::from_secs(5), next.run(request)).await {
        Ok(response) => response,
        Err(_) => Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "manual_import_command_timeout",
        )
        .into_response(),
    }
}
// Admission requires the operation to exist, have gone through preview, not already be claimed by
// this or the automated-download path, and carry no journal error -- resuming an errored journal
// re-runs `transfer` before any checkpoint clears it (see run_inner), which this module's own
// attempt/backoff bookkeeping must never race against. An operator retries a failed operation
// through the existing direct execute endpoint, then submits a fresh command for the next attempt.
async fn validate_admission(tx: &Connection, operation_id: Uuid) -> Result<()> {
    let opstr = operation_id.to_string();
    let exists = tx
        .query("SELECT 1 FROM operations WHERE id=?", [opstr.clone()])
        .await?
        .next()
        .await?
        .is_some();
    if !exists {
        return Err(Error(
            StatusCode::NOT_FOUND,
            "manual_import_operation_not_found",
        ));
    }
    // Automated ownership is a permanent admission boundary, independent of journal progress.
    // A worker may finish between a client's observation and its submission.
    if tx
        .query(
            "SELECT 1 FROM rss_candidate_imports WHERE operation_id=?",
            [opstr.clone()],
        )
        .await?
        .next()
        .await?
        .is_some()
    {
        return Err(Error(
            StatusCode::CONFLICT,
            "manual_import_owned_by_download",
        ));
    }
    let Some(j) = journal(tx, operation_id).await? else {
        return Err(Error(StatusCode::CONFLICT, "manual_import_not_previewed"));
    };
    if j.phase != "preview" || j.error_code.is_some() || j.path_failure_code.is_some() {
        return Err(Error(StatusCode::CONFLICT, "manual_import_not_ready"));
    }
    if tx.query("SELECT 1 FROM manual_import_commands WHERE operation_id=? AND status IN ('queued','retry_wait','running')",[opstr.clone()]).await?.next().await?.is_some(){
        return Err(Error(StatusCode::CONFLICT, "manual_import_already_queued"));
    }
    Ok(())
}
async fn create(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
    input: std::result::Result<Json<ManualImportBatchInput>, JsonRejection>,
) -> Result<(StatusCode, Json<Vec<ManualImportCommand>>)> {
    q.map_err(|_| bad())?;
    let input = input.map_err(|_| bad())?.0;
    let ids = input
        .operation_ids
        .iter()
        .collect::<std::collections::BTreeSet<_>>();
    if ids.is_empty() || ids.len() > BATCH_CAP || ids.len() != input.operation_ids.len() {
        return Err(bad());
    }
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        let timestamp = now()?;
        // Seven command tables share the 1024-row capacity pool (migration 0031's *_admit triggers);
        // this pre-check must count all seven or the batch fails mid-insert against the trigger
        // instead of cleanly up front.
        let count = command_capacity(&tx).await?;
        if count + input.operation_ids.len() as i64 > MAX_COMMANDS {
            return Err(Error(StatusCode::TOO_MANY_REQUESTS, "command_history_full"));
        }
        for operation_id in &input.operation_ids {
            validate_admission(&tx, *operation_id).await?;
        }
        let batch_id = Uuid::new_v4();
        let mut rows = Vec::with_capacity(input.operation_ids.len());
        for operation_id in &input.operation_ids {
            let id = Uuid::new_v4();
            tx.execute("INSERT INTO manual_import_commands(id,batch_id,operation_id,priority,status,attempts,next_attempt_at,created_at) VALUES(?,?,?,?,'queued',0,?,?)",params![id.to_string(),batch_id.to_string(),operation_id.to_string(),input.priority.number(),timestamp,timestamp]).await?;
            rows.push(read(&tx, id).await?);
        }
        bounded(rows)
    }
    .await;
    Ok((StatusCode::ACCEPTED, finish(tx, outcome).await?))
}
async fn list(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<ManualImportCommandQuery>, QueryRejection>,
) -> Result<Json<ApiPage<ManualImportCommand>>> {
    let q = q.map_err(|_| bad())?.0;
    if !(1..=100).contains(&q.limit) || q.offset > MAX_COMMANDS as u32 {
        return Err(bad());
    }
    let batch = q.batch_id.map(|v| v.to_string());
    let status = q.status.map(CommandStatus::text);
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::ReadOnly)
        .await?;
    let outcome=async{
        let predicate="WHERE (? IS NULL OR batch_id=?) AND (? IS NULL OR status=?)";
        let total=tx.query(&format!("SELECT count(*) FROM manual_import_commands {predicate}"),params![batch.clone(),batch.clone(),status,status]).await?.next().await?.ok_or_else(bad)?.get::<i64>(0)?;
        let mut rows=tx.query(&format!("SELECT {COLUMNS} FROM manual_import_commands {predicate} ORDER BY created_at DESC,id DESC LIMIT ? OFFSET ?"),params![batch.clone(),batch,status,status,i64::from(q.limit),i64::from(q.offset)]).await?;
        let mut items=vec![];while let Some(r)=rows.next().await?{items.push(row(r)?)}
        bounded(ApiPage{items,total,limit:q.limit,offset:q.offset})
    }.await;
    finish(tx, outcome).await
}
async fn detail(
    State(db): State<Arc<Database>>,
    Path(id): Path<String>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
) -> Result<Json<ManualImportCommand>> {
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
) -> Result<Json<ManualImportCommand>> {
    q.map_err(|_| bad())?;
    let id = Uuid::parse_str(&id).map_err(|_| bad())?;
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        let current = read(&tx, id).await?;
        if !matches!(
            current.status,
            CommandStatus::Queued | CommandStatus::RetryWait
        ) {
            return Err(conflict());
        }
        // A copy/move/hardlink in progress or completed cannot be undone by cancelling the
        // wrapper: once the journal has left 'preview' (whether through this command or the
        // direct execute endpoint racing ahead of it), execution has begun.
        let Some(j) = journal(&tx, current.operation_id).await? else {
            return Err(bad());
        };
        if j.phase != "preview" || j.path_failure_code.is_some() || crate::import::operation_active(&current.operation_id.to_string()).map_err(|_| Error(StatusCode::SERVICE_UNAVAILABLE, "manual_import_journal_unavailable"))? {
            return Err(conflict());
        }
        tx.execute("UPDATE manual_import_commands SET status='cancelled',completed_at=?,error_code=NULL WHERE id=? AND status IN ('queued','retry_wait')",params![now()?,id.to_string()]).await?;
        bounded(read(&tx, id).await?)
    }
    .await;
    finish(tx, outcome).await
}
pub(super) async fn claim(c: &Connection, id: Uuid, timestamp: i64) -> Result<ManualImportCommand> {
    c.execute("UPDATE manual_import_commands SET status='running',attempts=attempts+1,started_at=?,error_code=NULL WHERE id=?",params![timestamp,id.to_string()]).await?;
    read(c, id).await
}
async fn settle_succeeded(db: &Arc<Database>, id: Uuid) -> Result<()> {
    let c = connection(db).await?;
    c.execute("UPDATE manual_import_commands SET status='succeeded',completed_at=? WHERE id=? AND status='running'",params![now()?,id.to_string()]).await?;
    Ok(())
}
async fn settle_failed(db: &Arc<Database>, id: Uuid, code: &str) -> Result<()> {
    let c = connection(db).await?;
    c.execute("UPDATE manual_import_commands SET status='failed',completed_at=?,error_code=? WHERE id=? AND status='running'",params![now()?,normalize(code),id.to_string()]).await?;
    Ok(())
}
// Transient contention over crate::import's single global execution permit (or, defensively, any
// other pre-journal bail). Retries within this command's own 3-attempt budget; the underlying
// operation was never touched, so this never desyncs from import_journal.
async fn settle_retry(db: &Arc<Database>, id: Uuid, attempts: u8, code: &str) -> Result<()> {
    let c = connection(db).await?;
    let timestamp = now()?;
    let code = normalize(code);
    if attempts < 3 {
        let jitter = i64::from(Uuid::new_v4().as_bytes()[0] % 3);
        let delay = (1i64 << attempts) + jitter;
        c.execute("UPDATE manual_import_commands SET status='retry_wait',next_attempt_at=?,error_code=? WHERE id=? AND status='running'",params![timestamp+delay,code,id.to_string()]).await?;
    } else {
        c.execute("UPDATE manual_import_commands SET status='failed',completed_at=?,error_code=? WHERE id=? AND status='running'",params![timestamp,code,id.to_string()]).await?;
    }
    Ok(())
}
pub(super) async fn run(db: &Arc<Database>, command: ManualImportCommand) -> Result<()> {
    let c = connection(db).await?;
    if retained(&c, command.id).await?.is_none() {
        return Ok(());
    }
    let Some(j) = journal(&c, command.operation_id).await? else {
        return settle_failed(db, command.id, "manual_import_journal_missing").await;
    };
    run_after_journal(db, command, j).await
}
// Keep the continuation explicit so tests can deterministically exercise a stale journal read.
async fn run_after_journal(
    db: &Arc<Database>,
    command: ManualImportCommand,
    j: Journal,
) -> Result<()> {
    if j.phase == "complete" {
        return settle_succeeded(db, command.id).await;
    }
    if crate::import::operation_active(&command.operation_id.to_string()).map_err(|_| {
        Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "manual_import_journal_unavailable",
        )
    })? {
        return Ok(());
    }
    if j.path_failure_code.is_some() && j.error_code.is_none() {
        return settle_failed(db, command.id, "resume_required").await;
    }
    if let Some(code) = j.error_code {
        // Already a durable terminal failure (for example the direct execute endpoint raced
        // ahead and failed this same operation between admission and claim); never call execute
        // again to "retry" it here, since that would blindly resume the transfer under this
        // command's own attempt bookkeeping instead of the operation's own explicit-retry
        // contract.
        return settle_failed(db, command.id, &code).await;
    }
    let result =
        crate::import::execute_command(Arc::clone(db), &command.operation_id.to_string()).await;
    let c = connection(db).await?;
    let Some(j) = journal(&c, command.operation_id).await? else {
        return settle_failed(db, command.id, "manual_import_journal_missing").await;
    };
    if j.phase == "complete" {
        return settle_succeeded(db, command.id).await;
    }
    if crate::import::operation_active(&command.operation_id.to_string()).map_err(|_| {
        Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "manual_import_journal_unavailable",
        )
    })? {
        return Ok(());
    }
    if j.path_failure_code.is_some() && j.error_code.is_none() {
        return settle_failed(db, command.id, "resume_required").await;
    }
    if let Some(code) = j.error_code {
        return settle_failed(db, command.id, &code).await;
    }
    // Only confirmed pre-admission contention may use the command's automatic retry budget.
    match result {
        Err(e) if !e.started_attempt() && matches!(e.code(), "import_busy" | "rescan_active") => {
            settle_retry(db, command.id, command.attempts, e.code()).await
        }
        Err(e) => settle_failed(db, command.id, e.code()).await,
        Ok(_) => {
            // Per this module's own reading of run_inner, a single execute() call always drains to
            // a terminal journal phase or a persisted error; this should be unreachable. Treat it
            // as a transient anomaly rather than silently leaving the row 'running' forever.
            eprintln!(
                "{}",
                serde_json::json!({"level":"ERROR","component":"manual_import_command","command_id":command.id.to_string(),"operation_id":command.operation_id.to_string(),"condition":"execute_ok_without_terminal_journal"})
            );
            settle_failed(db, command.id, "import_incomplete").await
        }
    }
}
// A detached transfer outlives its awaiting command. Settle completed work, retain active
// leases, and require explicit recovery for abandoned admitted work even if error writes failed.
// Only legacy unmarked journals use the old startup retry budget.
pub(super) async fn recover(c: &Connection, timestamp: i64, code: &str) -> Result<()> {
    let mut rows = c
        .query(
            "SELECT m.id,m.attempts,j.phase,j.error_code,m.operation_id,CASE WHEN j.path_failure_code IS NULL THEN NULL WHEN typeof(j.path_failure_code)='text' THEN substr(CAST(j.path_failure_code AS BLOB),1,65) ELSE x'00' END FROM manual_import_commands m JOIN import_journal j ON j.operation_id=m.operation_id WHERE m.status='running'",
            (),
        )
        .await?;
    let mut pending = Vec::new();
    while let Some(r) = rows.next().await? {
        pending.push((
            Uuid::parse_str(&r.get::<String>(0)?).map_err(|_| bad())?,
            r.get::<i64>(1)? as u8,
            r.get::<String>(2)?,
            r.get::<Option<String>>(3)?,
            r.get::<String>(4)?,
            crate::import::checked_path_failure(&r, 5).map_err(|_| {
                Error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "manual_import_journal_unavailable",
                )
            })?,
        ));
    }
    drop(rows);
    for (id, attempts, phase, error, operation, marker) in pending {
        if phase == "complete" {
            c.execute("UPDATE manual_import_commands SET status='succeeded',completed_at=? WHERE id=? AND status='running'",params![timestamp,id.to_string()]).await?;
        } else if crate::import::operation_active(&operation).map_err(|_| {
            Error(
                StatusCode::SERVICE_UNAVAILABLE,
                "manual_import_journal_unavailable",
            )
        })? {
            continue;
        } else if marker.is_some() && error.is_none() {
            c.execute("UPDATE manual_import_commands SET status='failed',completed_at=?,error_code='resume_required' WHERE id=? AND status='running'", params![timestamp,id.to_string()]).await?;
        } else if let Some(journal_code) = error {
            c.execute("UPDATE manual_import_commands SET status='failed',completed_at=?,error_code=? WHERE id=? AND status='running'",params![timestamp,normalize(&journal_code),id.to_string()]).await?;
        } else if code == "interrupted" {
            if attempts < 3 {
                c.execute("UPDATE manual_import_commands SET status='retry_wait',next_attempt_at=?,error_code='interrupted' WHERE id=? AND status='running'",params![timestamp,id.to_string()]).await?;
            } else {
                c.execute("UPDATE manual_import_commands SET status='failed',completed_at=?,error_code='interrupted' WHERE id=? AND status='running'",params![timestamp,id.to_string()]).await?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Scratch(std::path::PathBuf);
    impl Scratch {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("hrrdarr-manual-import-cmd-{}", Uuid::new_v4()));
            std::fs::create_dir(&path).unwrap();
            for p in ["tv", "movies", "downloads"] {
                std::fs::create_dir(path.join(p)).unwrap();
            }
            Self(path)
        }
        fn path(&self, p: &str) -> String {
            self.0.join(p).to_str().unwrap().to_owned()
        }
        async fn database(&self) -> Arc<Database> {
            Arc::new(Database::open_local(self.0.join("db")).await.unwrap())
        }
        async fn setup(&self, db: &Database) {
            db.connect().await.unwrap().execute_batch(&format!("INSERT INTO series(id,title,path)VALUES(1,'TV','{}');INSERT INTO seasons(series_id,number)VALUES(1,1);INSERT INTO episodes(id,series_id,season,number,title)VALUES(1,1,1,1,'Episode');INSERT INTO movie_metadata(id,title)VALUES(1,'Movie');INSERT INTO movies(id,metadata_id,path)VALUES(1,1,'{}');",self.path("tv"),self.path("movies"))).await.unwrap();
        }
        async fn preview(&self, db: Arc<Database>) -> Uuid {
            std::fs::write(
                self.path("downloads/media"),
                b"manual-import-command-content",
            )
            .unwrap();
            let operation = crate::import::preview(
                db,
                crate::import::ImportInput::Typed(crate::import::ManualImportRequest {
                    target: crate::db::MediaTarget::Episode(1),
                    source: self.path("downloads/media"),
                    mode: crate::import::Mode::Copy,
                    destination: self.path("tv/media.mkv"),
                }),
            )
            .await
            .unwrap();
            operation.id
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    fn input(operation_ids: Vec<Uuid>) -> ManualImportBatchInput {
        ManualImportBatchInput {
            operation_ids,
            priority: CommandPriority::Normal,
        }
    }
    #[test]
    fn normalize_falls_back_for_codes_violating_the_storage_check() {
        assert_eq!(normalize("target_changed"), "target_changed");
        assert_eq!(normalize(""), "import_failed");
        assert_eq!(normalize("Has-Dash"), "import_failed");
        assert_eq!(normalize(&"x".repeat(65)), "import_failed");
    }
    #[tokio::test]
    async fn batch_admission_enforces_cap_dedup_readiness_and_existing_claims() {
        let scratch = Scratch::new();
        let db = scratch.database().await;
        scratch.setup(&db).await;
        let operation = scratch.preview(db.clone()).await;
        // Cap: 101 unique IDs rejected before touching storage.
        let oversized: Vec<Uuid> = (0..=BATCH_CAP).map(|_| Uuid::new_v4()).collect();
        assert!(matches!(
            create(
                State(db.clone()),
                Ok(Query(Empty {})),
                Ok(Json(input(oversized)))
            )
            .await,
            Err(Error(StatusCode::BAD_REQUEST, "invalid_command_request"))
        ));
        // Duplicate IDs within one batch rejected.
        assert!(matches!(
            create(
                State(db.clone()),
                Ok(Query(Empty {})),
                Ok(Json(input(vec![operation, operation])))
            )
            .await,
            Err(Error(StatusCode::BAD_REQUEST, "invalid_command_request"))
        ));
        // Unknown operation ID rejected with a clean 404, nothing inserted.
        assert!(matches!(
            create(
                State(db.clone()),
                Ok(Query(Empty {})),
                Ok(Json(input(vec![Uuid::new_v4()])))
            )
            .await,
            Err(Error(
                StatusCode::NOT_FOUND,
                "manual_import_operation_not_found"
            ))
        ));
        assert_eq!(
            connection(&db)
                .await
                .unwrap()
                .query("SELECT count(*) FROM manual_import_commands", ())
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
        // A valid preview admits cleanly.
        let created = create(
            State(db.clone()),
            Ok(Query(Empty {})),
            Ok(Json(input(vec![operation]))),
        )
        .await
        .unwrap()
        .1
        .0;
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].operation_id, operation);
        assert!(matches!(created[0].status, CommandStatus::Queued));
        let batch_id = created[0].batch_id;
        // Re-submitting the same operation while it is already queued is rejected.
        assert!(matches!(
            create(
                State(db.clone()),
                Ok(Query(Empty {})),
                Ok(Json(input(vec![operation])))
            )
            .await,
            Err(Error(StatusCode::CONFLICT, "manual_import_already_queued"))
        ));
        // Listing filters by batch_id.
        let page = list(
            State(db.clone()),
            Ok(Query(ManualImportCommandQuery {
                batch_id: Some(batch_id),
                status: None,
                limit: 50,
                offset: 0,
            })),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].id, created[0].id);
    }
    #[tokio::test]
    async fn cancel_permitted_before_execution_conflicts_once_journal_advances() {
        let scratch = Scratch::new();
        let db = scratch.database().await;
        scratch.setup(&db).await;
        let operation = scratch.preview(db.clone()).await;
        let created = create(
            State(db.clone()),
            Ok(Query(Empty {})),
            Ok(Json(input(vec![operation]))),
        )
        .await
        .unwrap()
        .1
        .0;
        let id = created[0].id;
        let cancelled = cancel(State(db.clone()), Path(id.to_string()), Ok(Query(Empty {})))
            .await
            .unwrap()
            .0;
        assert!(matches!(cancelled.status, CommandStatus::Cancelled));
        // A second, fresh preview and command, still 'queued' (never claimed by this worker). The
        // journal advancing past 'preview' -- as a direct `execute` call racing ahead of this
        // worker would do -- must be what cancel rejects on, not the command's own (still 'queued')
        // status column. Advance the journal directly rather than through a real `execute()` call:
        // `execute()` shares a single process-wide execution permit with every other test in this
        // binary (see the note above the two removed real-execute tests), and this assertion only
        // needs the journal's phase, not a real transfer.
        let operation = scratch.preview(db.clone()).await;
        let created = create(
            State(db.clone()),
            Ok(Query(Empty {})),
            Ok(Json(input(vec![operation]))),
        )
        .await
        .unwrap()
        .1
        .0;
        let id = created[0].id;
        assert!(matches!(
            read(&connection(&db).await.unwrap(), id)
                .await
                .unwrap()
                .status,
            CommandStatus::Queued
        ));
        connection(&db)
            .await
            .unwrap()
            .execute(
                "UPDATE import_journal SET phase='staging' WHERE operation_id=?",
                [operation.to_string()],
            )
            .await
            .unwrap();
        assert!(matches!(
            cancel(State(db.clone()), Path(id.to_string()), Ok(Query(Empty {}))).await,
            Err(Error(StatusCode::CONFLICT, "command_conflict"))
        ));
    }
    // Deliberately not covered here: run()'s own call into crate::import::execute() (the
    // post-execute succeeded/failed-from-a-freshly-produced-journal-error branches). A prototype of
    // both (one driving a real copy to completion, one forcing a fast real "target_changed" failure)
    // was written and removed: crate::import::execute() holds a single process-wide execution
    // permit (EXECUTING/ACTIVE_OPERATION in src/import/mod.rs) that src/import/tests.rs and
    // src/import/owned_tests.rs's own tests never guard against either, and cargo test's default
    // parallelism on this machine made either combination collide on a spurious `import_busy`
    // reliably (reproduced 5/5 runs), not rarely -- failing tests outside this module's ownership.
    // Closing this needs a lock shared across every real-execute()-calling test suite, which
    // belongs in src/import/ (outside this module's file ownership), not a local one here. Real
    // coverage of these branches is left to the separate testing-verification pass, ideally via an
    // out-of-process integration test in the style of tests/download_processing.rs.
    #[tokio::test]
    async fn run_settles_failed_immediately_for_a_pre_existing_journal_error_without_reexecuting() {
        let scratch = Scratch::new();
        let db = scratch.database().await;
        scratch.setup(&db).await;
        let operation = scratch.preview(db.clone()).await;
        let created = create(
            State(db.clone()),
            Ok(Query(Empty {})),
            Ok(Json(input(vec![operation]))),
        )
        .await
        .unwrap()
        .1
        .0;
        let id = created[0].id;
        let c = connection(&db).await.unwrap();
        c.execute(
            "UPDATE import_journal SET error_code='storage_error' WHERE operation_id=?",
            [operation.to_string()],
        )
        .await
        .unwrap();
        let claimed = claim(&c, id, now().unwrap()).await.unwrap();
        run(&db, claimed).await.unwrap();
        let settled = read(&c, id).await.unwrap();
        assert!(matches!(settled.status, CommandStatus::Failed));
        assert_eq!(settled.error_code.as_deref(), Some("storage_error"));
        // The journal itself was never touched by a fresh execute() call: still 'preview'.
        let phase: String = c
            .query(
                "SELECT phase FROM import_journal WHERE operation_id=?",
                [operation.to_string()],
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        assert_eq!(phase, "preview");
    }
    #[tokio::test]
    async fn recover_settles_only_terminal_journals_and_only_orphans_running_rows_when_interrupted()
    {
        let scratch = Scratch::new();
        let db = scratch.database().await;
        scratch.setup(&db).await;
        let operation = scratch.preview(db.clone()).await;
        let created = create(
            State(db.clone()),
            Ok(Query(Empty {})),
            Ok(Json(input(vec![operation]))),
        )
        .await
        .unwrap()
        .1
        .0;
        let id = created[0].id;
        let c = connection(&db).await.unwrap();
        claim(&c, id, now().unwrap()).await.unwrap();
        // Journal is still 'preview' (execute() was never actually called): a per-tick sweep must
        // never touch this, since a genuinely in-flight transfer looks identical from the DB alone.
        recover(&c, now().unwrap(), "storage_error").await.unwrap();
        assert!(matches!(
            read(&c, id).await.unwrap().status,
            CommandStatus::Running
        ));
        // Only a fresh-process "interrupted" sweep may treat a non-terminal, error-free running row
        // as orphaned.
        recover(&c, now().unwrap(), "interrupted").await.unwrap();
        let recovered = read(&c, id).await.unwrap();
        assert!(matches!(recovered.status, CommandStatus::RetryWait));
        assert_eq!(recovered.error_code.as_deref(), Some("interrupted"));
        // A terminal journal is always settled, regardless of the sweep's reason code -- here via a
        // persisted error (the detached execute() task's own launch() catch handler writes exactly
        // this fact). Set it directly rather than through a real `execute()` call: `execute()` shares
        // a single process-wide execution permit with every other test in this binary, and no
        // trigger restricts `error_code` on its own, so this exercises recover()'s terminal-error
        // branch without that shared-permit risk. The terminal-`complete` branch is exercised
        // end-to-end by `claim_run_settles_succeeded_through_real_execute` instead, since reaching
        // `phase='complete'` requires a real committed `import_history` row
        // (`import_commit_requires_history`) that cannot be fabricated here.
        let claimed = claim(&c, id, now().unwrap()).await.unwrap();
        assert_eq!(claimed.attempts, 2);
        c.execute(
            "UPDATE import_journal SET error_code='storage_error' WHERE operation_id=?",
            [operation.to_string()],
        )
        .await
        .unwrap();
        recover(&c, now().unwrap(), "storage_error").await.unwrap();
        let settled = read(&c, id).await.unwrap();
        assert!(matches!(settled.status, CommandStatus::Failed));
        assert_eq!(settled.error_code.as_deref(), Some("storage_error"));
    }
    // SQLite waits synchronously for the held writer; a second worker lets the observer
    // see the real import lease and release that barrier before the busy timeout.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn admitted_uncertainty_requires_recovery_but_active_transfer_survives_worker_restart() {
        stale_worker_snapshot_cannot_resume_a_direct_failure().await;
        let scratch = Scratch::new();
        let db = scratch.database().await;
        scratch.setup(&db).await;
        let operation = scratch.preview(db.clone()).await;
        let created = create(
            State(db.clone()),
            Ok(Query(Empty {})),
            Ok(Json(input(vec![operation]))),
        )
        .await
        .unwrap()
        .1
        .0;
        let c = connection(&db).await.unwrap();
        let id = created[0].id;
        claim(&c, id, now().unwrap()).await.unwrap();
        c.execute(
            "UPDATE import_journal SET path_failure_code='unresolved_attempt' WHERE operation_id=?",
            [operation.to_string()],
        )
        .await
        .unwrap();
        // Hold the writer before execute acquires its real lease, so no sleep or transfer-size
        // guess is needed to observe a still-owned detached invocation.
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .unwrap();
        let executing_db = db.clone();
        let execution = tokio::spawn(async move {
            crate::import::execute(executing_db, &operation.to_string()).await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !crate::import::operation_active(&operation.to_string()).unwrap() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        recover(&tx, now().unwrap(), "interrupted").await.unwrap();
        assert!(matches!(
            read(&tx, id).await.unwrap().status,
            CommandStatus::Running
        ));
        tx.rollback().await.unwrap();
        assert_eq!(execution.await.unwrap().unwrap().status, "complete");
        recover(&c, now().unwrap(), "storage_error").await.unwrap();
        assert!(matches!(
            read(&c, id).await.unwrap().status,
            CommandStatus::Succeeded
        ));

        let scratch = Scratch::new();
        let db = scratch.database().await;
        scratch.setup(&db).await;
        let c = connection(&db).await.unwrap();
        let operation = scratch.preview(db.clone()).await;
        let created = create(
            State(db.clone()),
            Ok(Query(Empty {})),
            Ok(Json(input(vec![operation]))),
        )
        .await
        .unwrap()
        .1
        .0;
        let id = created[0].id;
        let command = claim(&c, id, now().unwrap()).await.unwrap();
        c.execute(
            "UPDATE import_journal SET path_failure_code='unresolved_attempt' WHERE operation_id=?",
            [operation.to_string()],
        )
        .await
        .unwrap();
        run(&db, command).await.unwrap();
        let failed = read(&c, id).await.unwrap();
        assert!(matches!(failed.status, CommandStatus::Failed));
        assert_eq!(failed.error_code.as_deref(), Some("resume_required"));
        assert_eq!(
            journal(&c, operation).await.unwrap().unwrap().phase,
            "preview"
        );
    }
    async fn stale_worker_snapshot_cannot_resume_a_direct_failure() {
        use std::os::unix::fs::MetadataExt;
        for movie in [false, true] {
            for lose_error_write in [false, true] {
                let scratch = Scratch::new();
                let db = scratch.database().await;
                scratch.setup(&db).await;
                let source = scratch.path("downloads/media");
                let destination = scratch.path(if movie {
                    "movies/media.mkv"
                } else {
                    "tv/media.mkv"
                });
                std::fs::write(&source, b"stale-command-source").unwrap();
                let original = std::fs::metadata(&source).unwrap();
                let operation = crate::import::preview(
                    db.clone(),
                    crate::import::ImportInput::Typed(crate::import::ManualImportRequest {
                        target: if movie {
                            crate::db::MediaTarget::Movie(1)
                        } else {
                            crate::db::MediaTarget::Episode(1)
                        },
                        source: source.clone(),
                        destination: destination.clone(),
                        mode: crate::import::Mode::Copy,
                    }),
                )
                .await
                .unwrap()
                .id;
                let created = create(
                    State(db.clone()),
                    Ok(Query(Empty {})),
                    Ok(Json(input(vec![operation]))),
                )
                .await
                .unwrap()
                .1
                .0;
                let c = connection(&db).await.unwrap();
                let command = claim(&c, created[0].id, now().unwrap()).await.unwrap();
                let captured = journal(&c, operation).await.unwrap().unwrap();
                assert_eq!(captured.phase, "preview");
                assert!(captured.error_code.is_none() && captured.path_failure_code.is_none());
                if lose_error_write {
                    c.execute_batch("CREATE TRIGGER lose_direct_error BEFORE UPDATE OF error_code ON import_journal WHEN NEW.error_code IS NOT NULL BEGIN SELECT RAISE(ABORT,'test error persistence failure'); END;").await.unwrap();
                }
                // Move the parent, not the file: restoring it preserves even the source ctime,
                // so an incorrect implicit retry would really succeed instead of failing identity checks.
                std::fs::rename(scratch.path("downloads"), scratch.path("held-downloads")).unwrap();
                let failure = crate::import::execute(db.clone(), &operation.to_string())
                    .await
                    .unwrap_err();
                assert_eq!(failure.code(), "filesystem_error");
                assert!(!crate::import::operation_active(&operation.to_string()).unwrap());
                std::fs::rename(scratch.path("held-downloads"), scratch.path("downloads")).unwrap();
                let restored = std::fs::metadata(&source).unwrap();
                assert_eq!(
                    (
                        restored.dev(),
                        restored.ino(),
                        restored.mtime(),
                        restored.mtime_nsec(),
                        restored.ctime(),
                        restored.ctime_nsec()
                    ),
                    (
                        original.dev(),
                        original.ino(),
                        original.mtime(),
                        original.mtime_nsec(),
                        original.ctime(),
                        original.ctime_nsec()
                    )
                );
                if lose_error_write {
                    c.execute_batch("DROP TRIGGER lose_direct_error;")
                        .await
                        .unwrap();
                }
                let failed_journal = journal(&c, operation).await.unwrap().unwrap();
                assert_eq!(failed_journal.phase, "staging");
                assert_eq!(failed_journal.error_code.is_none(), lose_error_write);
                assert_eq!(
                    failed_journal.path_failure_code.as_deref(),
                    Some(if lose_error_write {
                        "unresolved_attempt"
                    } else {
                        "source_missing_precommit"
                    })
                );
                // This is the actual worker continuation with its captured clean snapshot, not a
                // fresh run() call that would trivially notice the newer failure before starting.
                let command_id = command.id;
                run_after_journal(&db, command, captured).await.unwrap();
                let settled = read(&c, command_id).await.unwrap();
                assert!(matches!(settled.status, CommandStatus::Failed));
                assert_eq!(
                    settled.error_code.as_deref(),
                    Some(if lose_error_write {
                        "resume_required"
                    } else {
                        "filesystem_error"
                    })
                );
                let retained = journal(&c, operation).await.unwrap().unwrap();
                assert_eq!(retained.phase, failed_journal.phase);
                assert_eq!(retained.error_code, failed_journal.error_code);
                assert_eq!(retained.path_failure_code, failed_journal.path_failure_code);
                assert!(!std::path::Path::new(&destination).exists());
                assert_eq!(
                    c.query(
                        "SELECT count(*) FROM import_history WHERE operation_id=?",
                        [operation.to_string()]
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
                let complete = crate::import::execute(db.clone(), &operation.to_string())
                    .await
                    .unwrap();
                assert_eq!(complete.status, "complete");
                assert_eq!(
                    std::fs::read(&destination).unwrap(),
                    b"stale-command-source"
                );
                assert_eq!(std::fs::read(&source).unwrap(), b"stale-command-source");
                assert!(complete.path_failure_code.is_none());
                // Command-only entry still treats a completed operation as an idempotent no-op.
                assert_eq!(
                    crate::import::execute_command(db.clone(), &operation.to_string())
                        .await
                        .unwrap()
                        .status,
                    "complete"
                );
                assert_eq!(
                    c.query(
                        "SELECT count(*) FROM import_history WHERE operation_id=?",
                        [operation.to_string()]
                    )
                    .await
                    .unwrap()
                    .next()
                    .await
                    .unwrap()
                    .unwrap()
                    .get::<i64>(0)
                    .unwrap(),
                    1
                );
            }
        }
    }
}
