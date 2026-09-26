//! Durable locally owned commands, including guarded RSS submission receipts.
use crate::{
    api::{ApiErrorEnvelope, ApiPage, MediaDomain},
    db::Database,
};
use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, Path, Query, State,
        rejection::{JsonRejection, QueryRejection},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use libsql::{Connection, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

pub mod blocklist;
pub mod manual_import;
pub mod metadata;
pub mod processing;
pub mod rss;
pub mod search;
mod worker;
pub use worker::{Runtime, start, start_with_metadata};

const MAX_COMMANDS: i64 = 1024;
const MAX_SNAPSHOTS: i64 = 64;
const MAX_BYTES: usize = 1024 * 1024;
const MAX_REVISION: i64 = 9007199254740991;

#[derive(Debug)]
pub struct Error(StatusCode, &'static str);
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.1)
    }
}
impl std::error::Error for Error {}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (self.0, Json(ApiErrorEnvelope::new(self.1, self.1))).into_response()
    }
}
impl From<libsql::Error> for Error {
    fn from(error: libsql::Error) -> Self {
        eprintln!(
            "event=command_storage_error kind={:?}",
            std::mem::discriminant(&error)
        );
        Self(StatusCode::INTERNAL_SERVER_ERROR, "command_storage_error")
    }
}
type Result<T> = std::result::Result<T, Error>;
fn bad() -> Error {
    Error(StatusCode::BAD_REQUEST, "invalid_command_request")
}
fn conflict() -> Error {
    Error(StatusCode::CONFLICT, "command_conflict")
}
fn domain(value: MediaDomain) -> &'static str {
    match value {
        MediaDomain::Tv => "tv",
        MediaDomain::Movies => "movies",
    }
}
fn now() -> Result<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|v| i64::try_from(v.as_secs()).ok())
        .filter(|v| *v < MAX_REVISION - 86400)
        .ok_or(Error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "command_clock_error",
        ))
}
fn bounded<T: Serialize>(value: T) -> Result<Json<T>> {
    if serde_json::to_vec(&value).map_err(|_| bad())?.len() > MAX_BYTES {
        return Err(Error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "command_response_limit",
        ));
    }
    Ok(Json(value))
}
#[derive(Clone, Copy, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum CommandName {
    RefreshDownloads,
}
#[derive(Clone, Copy, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum CommandPriority {
    Normal,
    High,
}
impl CommandPriority {
    fn number(self) -> i64 {
        match self {
            Self::Normal => 0,
            Self::High => 1,
        }
    }
}
#[derive(Clone, Copy, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    Queued,
    Running,
    RetryWait,
    Succeeded,
    Failed,
    Cancelled,
}
#[derive(Clone, Copy, Deserialize, Serialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct RefreshTarget {
    pub provider_id: Uuid,
    pub media_type: MediaDomain,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct CommandInput {
    pub name: CommandName,
    pub target: RefreshTarget,
    pub provider_revision: i64,
    pub priority: CommandPriority,
}
#[derive(Serialize, ts_rs::TS)]
pub struct Command {
    pub id: Uuid,
    pub name: CommandName,
    pub target: RefreshTarget,
    pub provider_revision: i64,
    pub priority: CommandPriority,
    pub status: CommandStatus,
    pub attempts: u8,
    pub next_attempt_at: i64,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub error_code: Option<String>,
    pub items_observed: u16,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct CommandQuery {
    #[ts(optional)]
    pub media_type: Option<MediaDomain>,
    #[ts(optional)]
    pub status: Option<CommandStatus>,
    #[serde(default = "default_limit")]
    #[ts(as = "Option<u16>", optional)]
    pub limit: u16,
    #[serde(default)]
    #[ts(as = "Option<u32>", optional)]
    pub offset: u32,
}
fn default_limit() -> u16 {
    50
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct RefreshScheduleInput {
    pub target: RefreshTarget,
    #[serde(deserialize_with = "required_nullable")]
    pub revision: Option<i64>,
    pub provider_revision: i64,
    pub enabled: bool,
    pub interval_seconds: u32,
}
fn required_nullable<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<i64>, D::Error> {
    Option::<i64>::deserialize(d)
}
#[derive(Serialize, ts_rs::TS)]
pub struct RefreshSchedule {
    pub target: RefreshTarget,
    pub revision: i64,
    pub provider_revision: i64,
    pub enabled: bool,
    pub interval_seconds: u32,
    pub next_run_at: i64,
    pub last_run_at: Option<i64>,
    pub error_code: Option<String>,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct QueueQuery {
    pub provider_id: Uuid,
    pub media_type: MediaDomain,
    #[serde(default = "default_limit")]
    #[ts(as = "Option<u16>", optional)]
    pub limit: u16,
    #[serde(default)]
    #[ts(as = "Option<u32>", optional)]
    pub offset: u32,
}
#[derive(Serialize, ts_rs::TS)]
pub struct QueueObservation {
    pub association: Option<crate::db::MediaTarget>,
    pub download: crate::providers::qbittorrent::DownloadItem,
}
#[derive(Serialize, ts_rs::TS)]
pub struct QueueSnapshot {
    pub target: RefreshTarget,
    pub provider_revision: i64,
    pub observed_at: i64,
    pub command_id: Option<Uuid>,
    pub items: Vec<QueueObservation>,
    pub total: u16,
    pub limit: u16,
    pub offset: u32,
}

impl CommandStatus {
    fn text(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::RetryWait => "retry_wait",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
    fn parse(value: &str) -> Result<Self> {
        match value {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "retry_wait" => Ok(Self::RetryWait),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(bad()),
        }
    }
}
const COMMAND_COLUMNS: &str = "id,provider_id,media_type,provider_revision,priority,status,attempts,next_attempt_at,created_at,started_at,completed_at,error_code,items_observed";
fn command_row(row: libsql::Row) -> Result<Command> {
    Ok(Command {
        id: Uuid::parse_str(&row.get::<String>(0)?).map_err(|_| bad())?,
        name: CommandName::RefreshDownloads,
        target: RefreshTarget {
            provider_id: Uuid::parse_str(&row.get::<String>(1)?).map_err(|_| bad())?,
            media_type: MediaDomain::parse(&row.get::<String>(2)?).map_err(|_| bad())?,
        },
        provider_revision: row.get(3)?,
        priority: if row.get::<i64>(4)? == 1 {
            CommandPriority::High
        } else {
            CommandPriority::Normal
        },
        status: CommandStatus::parse(&row.get::<String>(5)?)?,
        attempts: row.get::<i64>(6)? as u8,
        next_attempt_at: row.get(7)?,
        created_at: row.get(8)?,
        started_at: row.get(9)?,
        completed_at: row.get(10)?,
        error_code: row.get(11)?,
        items_observed: row.get::<i64>(12)? as u16,
    })
}
async fn read_command(c: &Connection, id: Uuid) -> Result<Command> {
    command_row(
        c.query(
            &format!("SELECT {COMMAND_COLUMNS} FROM commands WHERE id=?"),
            [id.to_string()],
        )
        .await?
        .next()
        .await?
        .ok_or(Error(StatusCode::NOT_FOUND, "command_not_found"))?,
    )
}
async fn connection(db: &Database) -> Result<Connection> {
    if !db.permits_local_imports() {
        return Err(Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "command_local_ownership_required",
        ));
    }
    Ok(db.connect().await?)
}
async fn valid_provider(c: &Connection, target: RefreshTarget, revision: i64) -> Result<bool> {
    if !(1..=MAX_REVISION).contains(&revision) {
        return Err(bad());
    }
    Ok(c.query("SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=? AND p.revision=? AND p.enabled=1 AND p.implementation='qbittorrent' AND s.media_type=?",params![target.provider_id.to_string(),revision,domain(target.media_type)]).await?.next().await?.is_some())
}
async fn finish<T>(tx: libsql::Transaction, result: Result<T>) -> Result<T> {
    match result {
        Ok(v) => {
            tx.commit().await?;
            Ok(v)
        }
        Err(e) => {
            tx.rollback().await?;
            Err(e)
        }
    }
}
async fn enqueue(c: &Connection, input: CommandInput, timestamp: i64) -> Result<Command> {
    if !valid_provider(c, input.target, input.provider_revision).await? {
        return Err(Error(StatusCode::CONFLICT, "provider_changed"));
    }
    if let Some(row)=c.query(&format!("SELECT {COMMAND_COLUMNS} FROM commands WHERE provider_id=? AND media_type=? AND status IN ('queued','running','retry_wait')"),params![input.target.provider_id.to_string(),domain(input.target.media_type)]).await?.next().await?{
        let current=command_row(row)?;
        if current.provider_revision!=input.provider_revision{return Err(conflict())}
        return Ok(current)
    }
    let count = c
        .query("SELECT (SELECT count(*) FROM commands)+(SELECT count(*) FROM metadata_refresh_commands)+(SELECT count(*) FROM blocklist_clear_commands)+(SELECT count(*) FROM rss_commands)+(SELECT count(*) FROM search_commands)+(SELECT count(*) FROM manual_import_commands)", ())
        .await?
        .next()
        .await?
        .ok_or_else(bad)?
        .get::<i64>(0)?;
    if count >= MAX_COMMANDS {
        return Err(Error(StatusCode::TOO_MANY_REQUESTS, "command_history_full"));
    }
    let id = Uuid::new_v4();
    c.execute("INSERT INTO commands(id,name,provider_id,media_type,provider_revision,priority,status,attempts,next_attempt_at,created_at,items_observed) VALUES(?,'refresh_downloads',?,?,?,?,'queued',0,?,?,0)",params![id.to_string(),input.target.provider_id.to_string(),domain(input.target.media_type),input.provider_revision,input.priority.number(),timestamp,timestamp]).await?;
    read_command(c, id).await
}

pub fn router(db: Arc<Database>) -> Router {
    let metadata = metadata::router(db.clone());
    let blocklist = blocklist::router(db.clone());
    let rss = rss::router(db.clone());
    let processing = processing::router(db.clone());
    let manual_import = manual_import::router(db.clone());
    Router::new()
        .route("/api/v1/commands", get(list).post(create))
        .route("/api/v1/commands/{id}", get(detail).delete(delete))
        .route("/api/v1/commands/{id}/cancel", post(cancel))
        .route(
            "/api/v1/download-refresh/schedules",
            get(schedules).put(schedule).delete(delete_schedule),
        )
        .route("/api/v1/queue", get(queue))
        .layer(DefaultBodyLimit::max(8192))
        .with_state(db)
        .merge(metadata)
        .merge(blocklist)
        .merge(rss)
        .merge(processing)
        .merge(manual_import)
}
async fn create(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
    input: std::result::Result<Json<CommandInput>, JsonRejection>,
) -> Result<(StatusCode, Json<Command>)> {
    q.map_err(|_| bad())?;
    let input = input.map_err(|_| bad())?.0;
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let result = async { bounded(enqueue(&tx, input, now()?).await?) }.await;
    Ok((StatusCode::ACCEPTED, finish(tx, result).await?))
}
async fn list(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<CommandQuery>, QueryRejection>,
) -> Result<Json<ApiPage<Command>>> {
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
    let result=async{
        let predicate="WHERE (? IS NULL OR media_type=?) AND (? IS NULL OR status=?)";
        let total=tx.query(&format!("SELECT count(*) FROM commands {predicate}"),params![media,media,status,status]).await?.next().await?.ok_or_else(bad)?.get(0)?;
        let mut rows=tx.query(&format!("SELECT {COMMAND_COLUMNS} FROM commands {predicate} ORDER BY created_at DESC,id DESC LIMIT ? OFFSET ?"),params![media,media,status,status,i64::from(q.limit),i64::from(q.offset)]).await?;
        let mut items=Vec::new();while let Some(row)=rows.next().await?{items.push(command_row(row)?)}
        bounded(ApiPage{items,total,limit:q.limit,offset:q.offset})
    }.await;
    finish(tx, result).await
}
async fn detail(
    State(db): State<Arc<Database>>,
    Path(id): Path<String>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
) -> Result<Json<Command>> {
    q.map_err(|_| bad())?;
    let id = Uuid::parse_str(&id).map_err(|_| bad())?;
    bounded(read_command(&connection(&db).await?, id).await?)
}
async fn cancel(
    State(db): State<Arc<Database>>,
    Path(id): Path<String>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
) -> Result<Json<Command>> {
    q.map_err(|_| bad())?;
    let id = Uuid::parse_str(&id).map_err(|_| bad())?;
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let result=async{
        tx.execute("UPDATE commands SET status='cancelled',completed_at=?,error_code=NULL WHERE id=? AND status IN ('queued','running','retry_wait')",params![now()?,id.to_string()]).await?;
        bounded(read_command(&tx,id).await?)
    }.await;
    finish(tx, result).await
}
async fn delete(
    State(db): State<Arc<Database>>,
    Path(id): Path<String>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
) -> Result<StatusCode> {
    q.map_err(|_| bad())?;
    let id = Uuid::parse_str(&id).map_err(|_| bad())?;
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let result = async {
        let existing = read_command(&tx, id).await?;
        if matches!(
            existing.status,
            CommandStatus::Queued | CommandStatus::Running | CommandStatus::RetryWait
        ) {
            return Err(conflict());
        }
        tx.execute("DELETE FROM commands WHERE id=?", [id.to_string()])
            .await?;
        Ok(StatusCode::NO_CONTENT)
    }
    .await;
    finish(tx, result).await
}

const SCHEDULE_COLUMNS: &str = "provider_id,media_type,revision,provider_revision,enabled,interval_seconds,next_run_at,last_run_at,error_code";
fn schedule_row(row: libsql::Row) -> Result<RefreshSchedule> {
    Ok(RefreshSchedule {
        target: RefreshTarget {
            provider_id: Uuid::parse_str(&row.get::<String>(0)?).map_err(|_| bad())?,
            media_type: MediaDomain::parse(&row.get::<String>(1)?).map_err(|_| bad())?,
        },
        revision: row.get(2)?,
        provider_revision: row.get(3)?,
        enabled: row.get::<i64>(4)? == 1,
        interval_seconds: row.get::<i64>(5)? as u32,
        next_run_at: row.get(6)?,
        last_run_at: row.get(7)?,
        error_code: row.get(8)?,
    })
}
async fn read_schedule(c: &Connection, target: RefreshTarget) -> Result<Option<RefreshSchedule>> {
    c.query(&format!("SELECT {SCHEDULE_COLUMNS} FROM download_refresh_schedules WHERE provider_id=? AND media_type=?"),params![target.provider_id.to_string(),domain(target.media_type)]).await?.next().await?.map(schedule_row).transpose()
}
async fn schedules(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
) -> Result<Json<Vec<RefreshSchedule>>> {
    q.map_err(|_| bad())?;
    let c = connection(&db).await?;
    let mut rows=c.query(&format!("SELECT {SCHEDULE_COLUMNS} FROM download_refresh_schedules ORDER BY provider_id,media_type LIMIT 65"),()).await?;
    let mut items = Vec::new();
    while let Some(row) = rows.next().await? {
        items.push(schedule_row(row)?)
    }
    if items.len() > 64 {
        return Err(Error(StatusCode::PAYLOAD_TOO_LARGE, "schedule_limit"));
    }
    bounded(items)
}
async fn schedule(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
    input: std::result::Result<Json<RefreshScheduleInput>, JsonRejection>,
) -> Result<Json<RefreshSchedule>> {
    q.map_err(|_| bad())?;
    let input = input.map_err(|_| bad())?.0;
    if !(60..=86400).contains(&input.interval_seconds)
        || !(1..=MAX_REVISION).contains(&input.provider_revision)
        || input
            .revision
            .is_some_and(|r| !(1..MAX_REVISION).contains(&r))
    {
        return Err(bad());
    }
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let result=async{
        let old=read_schedule(&tx,input.target).await?;
        if old.as_ref().map(|v|v.revision)!=input.revision{return Err(conflict())}
        if tx.query("SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=? AND p.revision=? AND p.implementation='qbittorrent' AND s.media_type=? AND (?=0 OR p.enabled=1)",params![input.target.provider_id.to_string(),input.provider_revision,domain(input.target.media_type),i64::from(input.enabled)]).await?.next().await?.is_none(){return Err(Error(StatusCode::CONFLICT,"provider_changed"))}
        if old.is_none()&&tx.query("SELECT count(*) FROM download_refresh_schedules",()).await?.next().await?.ok_or_else(bad)?.get::<i64>(0)?>=64{return Err(Error(StatusCode::TOO_MANY_REQUESTS,"schedule_limit"))}
        tx.execute("INSERT INTO download_refresh_schedules(provider_id,media_type,provider_revision,enabled,interval_seconds,next_run_at,revision,error_code) VALUES(?,?,?,?,?,?,1,NULL) ON CONFLICT(provider_id,media_type) DO UPDATE SET provider_revision=excluded.provider_revision,enabled=excluded.enabled,interval_seconds=excluded.interval_seconds,next_run_at=excluded.next_run_at,revision=download_refresh_schedules.revision+1,error_code=NULL",params![input.target.provider_id.to_string(),domain(input.target.media_type),input.provider_revision,i64::from(input.enabled),i64::from(input.interval_seconds),now()?]).await?;
        bounded(read_schedule(&tx,input.target).await?.ok_or_else(bad)?)
    }.await;
    finish(tx, result).await
}
async fn queue(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<QueueQuery>, QueryRejection>,
) -> Result<Json<QueueSnapshot>> {
    let q = q.map_err(|_| bad())?.0;
    if !(1..=100).contains(&q.limit) || q.offset > 1000 {
        return Err(bad());
    }
    let c = connection(&db).await?;
    let row=c.query("SELECT provider_revision,observed_at,command_id,items_json FROM download_refresh_snapshots WHERE provider_id=? AND media_type=?",params![q.provider_id.to_string(),domain(q.media_type)]).await?.next().await?.ok_or(Error(StatusCode::NOT_FOUND,"queue_snapshot_unavailable"))?;
    let bytes = row.get::<String>(3)?;
    if bytes.len() > MAX_BYTES {
        return Err(Error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "command_response_limit",
        ));
    }
    let downloads: Vec<crate::providers::qbittorrent::DownloadItem> = serde_json::from_str(&bytes)
        .map_err(|_| Error(StatusCode::INTERNAL_SERVER_ERROR, "command_storage_error"))?;
    if downloads.len() > 1000 {
        return Err(Error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "command_response_limit",
        ));
    }
    let total = downloads.len() as u16;
    let provider_revision: i64 = row.get(0)?;
    let observed_at: i64 = row.get(1)?;
    let command_id = row
        .get::<Option<String>>(2)?
        .map(|v| Uuid::parse_str(&v).map_err(|_| bad()))
        .transpose()?;
    let mut items = Vec::new();
    for download in downloads
        .into_iter()
        .skip(q.offset as usize)
        .take(q.limit as usize)
    {
        let association=if let Some(receipt)=c.query("SELECT r.movie_id,e.episode_id FROM rss_candidates r LEFT JOIN rss_candidate_episodes e ON e.candidate_id=r.id WHERE r.client_id=? AND r.observed_hash=? AND r.client_revision=? AND r.media_type=? AND r.status='observed'",params![q.provider_id.to_string(),download.hash.clone(),provider_revision,domain(q.media_type)]).await?.next().await? {
            match (receipt.get::<Option<i64>>(0)?,receipt.get::<Option<i64>>(1)?) {
                (Some(id),None)=>Some(crate::db::MediaTarget::Movie(id)),
                (None,Some(id))=>Some(crate::db::MediaTarget::Episode(id)),
                _=>None,
            }
        }else{None};
        items.push(QueueObservation {
            association,
            download,
        });
    }
    bounded(QueueSnapshot {
        target: RefreshTarget {
            provider_id: q.provider_id,
            media_type: q.media_type,
        },
        provider_revision,
        observed_at,
        command_id,
        items,
        total,
        limit: q.limit,
        offset: q.offset,
    })
}

#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct RefreshScheduleDelete {
    pub target: RefreshTarget,
    pub revision: i64,
}
async fn delete_schedule(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
    input: std::result::Result<Json<RefreshScheduleDelete>, JsonRejection>,
) -> Result<StatusCode> {
    q.map_err(|_| bad())?;
    let input = input.map_err(|_| bad())?.0;
    if !(1..=MAX_REVISION).contains(&input.revision) {
        return Err(bad());
    }
    let c = connection(&db).await?;
    if c.execute("DELETE FROM download_refresh_schedules WHERE provider_id=? AND media_type=? AND revision=?",params![input.target.provider_id.to_string(),domain(input.target.media_type),input.revision]).await?!=1{return Err(conflict())}
    Ok(StatusCode::NO_CONTENT)
}
