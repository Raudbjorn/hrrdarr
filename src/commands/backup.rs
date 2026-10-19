//! Backup (job.002): manual or scheduled. One active command at a time (unique index), so it is
//! independent of the shared command pool. A run completes within one worker iteration (the
//! engine's budget is below the iteration timeout); an interrupted run is failed, not retried.
use super::*;
use crate::backup::{BackupLimits, BackupType};

const RETAINED_TERMINAL: i64 = 100;
const DAY_SECONDS: i64 = 86_400;

#[derive(Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum BackupCommandName {
    Backup,
}
#[derive(Clone, Copy, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum BackupCommandType {
    Manual,
    Scheduled,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct BackupCommandInput {}
#[derive(Serialize, ts_rs::TS)]
pub struct BackupCommand {
    pub id: Uuid,
    pub name: BackupCommandName,
    pub backup_type: BackupCommandType,
    pub status: CommandStatus,
    pub attempts: i64,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub error_code: Option<String>,
    // The published archive's id (see /api/v1/system/backups); present once succeeded.
    pub backup_id: Option<String>,
    pub backup_name: Option<String>,
    pub size_bytes: Option<i64>,
    // Older backups of the same type removed by retention after this one was published.
    pub pruned: i64,
}
const COLUMNS: &str = "id,backup_type,status,attempts,created_at,started_at,completed_at,error_code,backup_id,backup_name,size_bytes,pruned";
fn row(r: libsql::Row) -> Result<BackupCommand> {
    Ok(BackupCommand {
        id: Uuid::parse_str(&r.get::<String>(0)?).map_err(|_| bad())?,
        name: BackupCommandName::Backup,
        backup_type: match r.get::<String>(1)?.as_str() {
            "manual" => BackupCommandType::Manual,
            "scheduled" => BackupCommandType::Scheduled,
            _ => return Err(bad()),
        },
        status: CommandStatus::parse(&r.get::<String>(2)?)?,
        attempts: r.get(3)?,
        created_at: r.get(4)?,
        started_at: r.get(5)?,
        completed_at: r.get(6)?,
        error_code: r.get(7)?,
        backup_id: r.get(8)?,
        backup_name: r.get(9)?,
        size_bytes: r.get(10)?,
        pruned: r.get(11)?,
    })
}
async fn read(c: &Connection, id: Uuid) -> Result<BackupCommand> {
    row(c
        .query(
            &format!("SELECT {COLUMNS} FROM backup_commands WHERE id=?"),
            [id.to_string()],
        )
        .await?
        .next()
        .await?
        .ok_or(Error(StatusCode::NOT_FOUND, "backup_command_not_found"))?)
}
pub(super) fn router(db: Arc<Database>) -> Router {
    Router::new()
        .route("/api/v1/system/backup-commands", get(list).post(create))
        .route("/api/v1/system/backup-commands/{id}", get(detail))
        .layer(DefaultBodyLimit::max(1024))
        .with_state(db)
}
async fn enqueue(c: &Connection, backup_type: &str, timestamp: i64) -> Result<BackupCommand> {
    if let Some(r) = c
        .query(
            &format!("SELECT {COLUMNS} FROM backup_commands WHERE status IN ('queued','running')"),
            (),
        )
        .await?
        .next()
        .await?
    {
        return row(r);
    }
    let id = Uuid::new_v4();
    c.execute(
        "INSERT INTO backup_commands(id,name,backup_type,status,next_attempt_at,created_at)VALUES(?,'backup',?,'queued',?,?)",
        params![id.to_string(), backup_type, timestamp, timestamp],
    )
    .await?;
    read(c, id).await
}
async fn create(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
    input: std::result::Result<Json<BackupCommandInput>, JsonRejection>,
) -> Result<(StatusCode, Json<BackupCommand>)> {
    q.map_err(|_| bad())?;
    let _ = input.map_err(|_| bad())?;
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = async { bounded(enqueue(&tx, "manual", now()?).await?) }.await;
    Ok((StatusCode::ACCEPTED, finish(tx, outcome).await?))
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "BackupCommandQuery", optional_fields)]
pub struct ListParams {
    #[serde(default = "default_limit")]
    #[ts(as = "Option<u16>", optional)]
    pub limit: u16,
    #[serde(default)]
    #[ts(as = "Option<u32>", optional)]
    pub offset: u32,
}
async fn list(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<ListParams>, QueryRejection>,
) -> Result<Json<ApiPage<BackupCommand>>> {
    let q = q.map_err(|_| bad())?.0;
    if !(1..=100).contains(&q.limit) || q.offset > MAX_COMMANDS as u32 {
        return Err(bad());
    }
    let c = connection(&db).await?;
    let total = c
        .query("SELECT count(*) FROM backup_commands", ())
        .await?
        .next()
        .await?
        .ok_or_else(bad)?
        .get(0)?;
    let mut rows = c
        .query(
            &format!("SELECT {COLUMNS} FROM backup_commands ORDER BY created_at DESC,id DESC LIMIT ? OFFSET ?"),
            params![i64::from(q.limit), i64::from(q.offset)],
        )
        .await?;
    let mut items = vec![];
    while let Some(r) = rows.next().await? {
        items.push(row(r)?);
    }
    bounded(ApiPage {
        items,
        total,
        limit: q.limit,
        offset: q.offset,
    })
}
async fn detail(
    State(db): State<Arc<Database>>,
    Path(id): Path<String>,
) -> Result<Json<BackupCommand>> {
    bounded(
        read(
            &connection(&db).await?,
            Uuid::parse_str(&id).map_err(|_| bad())?,
        )
        .await?,
    )
}

/// Inside the worker's claim transaction: one scheduled backup per interval. The first interval
/// starts when the policy is first observed; a due slot with an active backup is skipped.
pub(super) async fn schedule_due(c: &Connection, timestamp: i64) -> Result<()> {
    c.execute(
        "UPDATE backup_settings SET next_backup_at=?+interval_days*? WHERE id=1 AND next_backup_at IS NULL",
        params![timestamp, DAY_SECONDS],
    )
    .await?;
    let due = c
        .query(
            "SELECT next_backup_at<=? FROM backup_settings WHERE id=1",
            [timestamp],
        )
        .await?
        .next()
        .await?
        .map(|r| r.get::<i64>(0))
        .transpose()?
        .unwrap_or(0);
    if due == 1 {
        enqueue(c, "scheduled", timestamp).await?;
        c.execute(
            "UPDATE backup_settings SET next_backup_at=?+interval_days*? WHERE id=1",
            params![timestamp, DAY_SECONDS],
        )
        .await?;
    }
    Ok(())
}
pub(super) async fn claim_due(c: &Connection, timestamp: i64) -> Result<Option<BackupCommand>> {
    let Some(r) = c
        .query(
            "SELECT id FROM backup_commands WHERE status='queued' AND next_attempt_at<=? ORDER BY created_at,id LIMIT 1",
            [timestamp],
        )
        .await?
        .next()
        .await?
    else {
        return Ok(None);
    };
    let id = Uuid::parse_str(&r.get::<String>(0)?).map_err(|_| bad())?;
    c.execute(
        "UPDATE backup_commands SET status='running',attempts=1,started_at=? WHERE id=?",
        params![timestamp, id.to_string()],
    )
    .await?;
    Ok(Some(read(c, id).await?))
}
/// A running backup never survives an iteration; its staging is removed by the engine or the
/// next run. It is failed rather than retried (including the row captured by a restored snapshot).
pub(super) async fn recover(c: &Connection, timestamp: i64, code: &str) -> Result<()> {
    let code = if code == "storage_error" {
        "storage_error"
    } else {
        "interrupted"
    };
    c.execute(
        "UPDATE backup_commands SET status='failed',completed_at=?,error_code=? WHERE status='running'",
        params![timestamp, code],
    )
    .await?;
    Ok(())
}
pub(super) async fn run(db: &Arc<Database>, command: BackupCommand) -> Result<()> {
    let backup_type = match command.backup_type {
        BackupCommandType::Manual => BackupType::Manual,
        BackupCommandType::Scheduled => BackupType::Scheduled,
    };
    let result = crate::backup::create_backup(db, backup_type, BackupLimits::command()).await;
    let c = connection(db).await?;
    match result {
        Ok(published) => {
            let backup = published.backup;
            c.execute(
                "UPDATE backup_commands SET status='succeeded',completed_at=?,backup_id=?,backup_name=?,size_bytes=?,pruned=? WHERE id=? AND status='running'",
                params![
                    now()?,
                    backup.id,
                    backup.name,
                    i64::try_from(backup.size_bytes).unwrap_or(MAX_REVISION).min(MAX_REVISION),
                    i64::try_from(published.pruned).unwrap_or(100_000).min(100_000),
                    command.id.to_string()
                ],
            )
            .await?;
        }
        Err(failure) => {
            c.execute(
                "UPDATE backup_commands SET status='failed',completed_at=?,error_code=? WHERE id=? AND status='running'",
                params![now()?, failure.0, command.id.to_string()],
            )
            .await?;
        }
    }
    c.execute(
        "DELETE FROM backup_commands WHERE status IN ('succeeded','failed','cancelled') AND id NOT IN (SELECT id FROM backup_commands WHERE status IN ('succeeded','failed','cancelled') ORDER BY created_at DESC,id DESC LIMIT ?)",
        [RETAINED_TERMINAL],
    )
    .await?;
    Ok(())
}
