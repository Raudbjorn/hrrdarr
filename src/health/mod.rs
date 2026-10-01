//! Persisted, scoped health observations. The diagnostic ring is not a delivery outbox.
use crate::{
    api::{ApiErrorEnvelope, MediaDomain},
    commands::{CommandPriority, CommandStatus},
    db::Database,
};
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use libsql::{Connection, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
mod api;
mod download_roots;
mod engine;
mod removed_metadata;
pub use api::{
    HealthCheckState, HealthCoverage, HealthEvaluation, HealthSnapshot, HealthSummary,
    HealthTransition, HealthTransitions, router,
};
pub(crate) use engine::{claim, recover, run, startup, sweep};
const MAX_INTEGER: i64 = 9_007_199_254_740_991;
const STARTUP: i64 = 1;
const SCHEDULED: i64 = 2;
const MANUAL: i64 = 4;
const CONFIG: i64 = 8;
const GRACE: i64 = 16;
const ACTIVE: &str = "status IN ('queued','running','retry_wait')";
#[derive(Debug)]
pub struct Error(pub(crate) StatusCode, pub(crate) &'static str);
pub type Result<T> = std::result::Result<T, Error>;
impl From<libsql::Error> for Error {
    fn from(e: libsql::Error) -> Self {
        eprintln!(
            "event=health_storage_error kind={:?}",
            std::mem::discriminant(&e)
        );
        Self(StatusCode::SERVICE_UNAVAILABLE, "health_storage_error")
    }
}
impl From<crate::commands::Error> for Error {
    fn from(e: crate::commands::Error) -> Self {
        Self(e.0, e.1)
    }
}
impl From<Error> for crate::commands::Error {
    fn from(e: Error) -> Self {
        Self(e.0, e.1)
    }
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (self.0, Json(ApiErrorEnvelope::new(self.1, self.1))).into_response()
    }
}
fn invariant() -> Error {
    Error(StatusCode::INTERNAL_SERVER_ERROR, "health_invariant")
}
fn bad() -> Error {
    Error(StatusCode::BAD_REQUEST, "invalid_health_request")
}
fn now() -> Result<i64> {
    Ok(crate::commands::now()?)
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, ts_rs::TS, Default)]
#[serde(rename_all = "snake_case")]
pub enum HealthScope {
    Tv,
    Movies,
    System,
    #[default]
    All,
}
impl HealthScope {
    fn text(self) -> &'static str {
        match self {
            Self::Tv => "tv",
            Self::Movies => "movies",
            Self::System => "system",
            Self::All => "all",
        }
    }
    fn parse(s: &str) -> Result<Self> {
        match s {
            "tv" => Ok(Self::Tv),
            "movies" => Ok(Self::Movies),
            "system" => Ok(Self::System),
            "all" => Ok(Self::All),
            _ => Err(invariant()),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
pub struct HealthIdentity {
    pub scope: HealthScope,
    pub check_key: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum HealthSeverity {
    Ok,
    Notice,
    Warning,
    Error,
}
impl HealthSeverity {
    fn number(self) -> i64 {
        match self {
            Self::Ok => 0,
            Self::Notice => 1,
            Self::Warning => 2,
            Self::Error => 3,
        }
    }
    fn parse(v: i64) -> Result<Self> {
        match v {
            0 => Ok(Self::Ok),
            1 => Ok(Self::Notice),
            2 => Ok(Self::Warning),
            3 => Ok(Self::Error),
            _ => Err(invariant()),
        }
    }
}
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
pub struct HealthIssue {
    pub identity: HealthIdentity,
    pub severity: HealthSeverity,
    pub reason: String,
    pub message: String,
    pub wiki_url: String,
    pub compatibility_type: String,
}
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
pub struct HealthSelectionToken {
    pub identity: HealthIdentity,
    pub generation: i64,
}
#[derive(Serialize, ts_rs::TS)]
pub struct HealthMember {
    pub identity: HealthIdentity,
    pub admitted_generation: i64,
    pub captured_generation: Option<i64>,
    pub captured_reasons: Option<i64>,
    pub captured_due_at: Option<i64>,
}
#[derive(Serialize, ts_rs::TS)]
pub struct HealthCommand {
    pub id: Uuid,
    pub scope: HealthScope,
    pub priority: CommandPriority,
    pub status: CommandStatus,
    pub attempts: u8,
    pub next_attempt_at: i64,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub error_code: Option<String>,
    pub epoch: Uuid,
    pub is_grace: bool,
    pub members: Vec<HealthMember>,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct HealthCommandInput {
    pub scope: HealthScope,
    pub priority: CommandPriority,
}
#[derive(Serialize, ts_rs::TS)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum HealthAdmission {
    Queued {
        command_id: Uuid,
        requested_scope: HealthScope,
        selection: Vec<HealthSelectionToken>,
        pending: bool,
    },
    CoalescedQueued {
        command_id: Uuid,
        requested_scope: HealthScope,
        selection: Vec<HealthSelectionToken>,
        pending: bool,
    },
    Pending {
        active_command_id: Uuid,
        requested_scope: HealthScope,
        selection: Vec<HealthSelectionToken>,
        pending: bool,
    },
}
#[derive(Serialize, ts_rs::TS)]
pub struct HealthLifecycle {
    pub epoch: Uuid,
    pub started_at: i64,
    pub grace_due_at: i64,
    pub grace_phase: String,
    pub next_scheduled_at: i64,
    pub schedule_error: Option<String>,
    pub last_batch_completed_at: Option<i64>,
}
async fn lifecycle(c: &Connection) -> Result<HealthLifecycle> {
    let r=c.query("SELECT epoch,started_at,grace_due_at,grace_phase,next_scheduled_at,schedule_error,last_batch_completed_at FROM health_lifecycle WHERE id=1",()).await?.next().await?.ok_or_else(invariant)?;
    Ok(HealthLifecycle {
        epoch: uuid(r.get(0)?)?,
        started_at: r.get(1)?,
        grace_due_at: r.get(2)?,
        grace_phase: r.get(3)?,
        next_scheduled_at: r.get(4)?,
        schedule_error: r.get(5)?,
        last_batch_completed_at: r.get(6)?,
    })
}
fn uuid(v: String) -> Result<Uuid> {
    Uuid::parse_str(&v).map_err(|_| invariant())
}
async fn read(c: &Connection, id: Uuid) -> Result<HealthCommand> {
    let r=c.query("SELECT priority,status,attempts,next_attempt_at,created_at,started_at,completed_at,error_code,epoch,is_grace FROM health_commands WHERE id=?",[id.to_string()]).await?.next().await?.ok_or(Error(StatusCode::NOT_FOUND,"health_command_not_found"))?;
    let mut rows=c.query("SELECT scope,check_key,admitted_generation,captured_generation,captured_reasons,captured_due_at FROM health_command_checks WHERE command_id=? ORDER BY scope,check_key LIMIT 128",[id.to_string()]).await?;
    let mut members = Vec::new();
    while let Some(r) = rows.next().await? {
        members.push(HealthMember {
            identity: HealthIdentity {
                scope: HealthScope::parse(&r.get::<String>(0)?)?,
                check_key: r.get(1)?,
            },
            admitted_generation: r.get(2)?,
            captured_generation: r.get(3)?,
            captured_reasons: r.get(4)?,
            captured_due_at: r.get(5)?,
        })
    }
    Ok(HealthCommand {
        id,
        scope: HealthScope::All,
        priority: match r.get::<i64>(0)? {
            0 => CommandPriority::Normal,
            1 => CommandPriority::High,
            _ => return Err(invariant()),
        },
        status: CommandStatus::parse(&r.get::<String>(1)?).map_err(|_| invariant())?,
        attempts: r.get::<i64>(2)?.try_into().map_err(|_| invariant())?,
        next_attempt_at: r.get(3)?,
        created_at: r.get(4)?,
        started_at: r.get(5)?,
        completed_at: r.get(6)?,
        error_code: r.get(7)?,
        epoch: uuid(r.get(8)?)?,
        is_grace: r.get::<i64>(9)? == 1,
        members,
    })
}
async fn active(c: &Connection) -> Result<Option<HealthCommand>> {
    match c
        .query(
            &format!("SELECT id FROM health_commands WHERE {ACTIVE}"),
            (),
        )
        .await?
        .next()
        .await?
    {
        Some(r) => Ok(Some(read(c, uuid(r.get(0)?)?).await?)),
        None => Ok(None),
    }
}
async fn selection(
    c: &Connection,
    scope: HealthScope,
    predicate: &str,
) -> Result<Vec<HealthSelectionToken>> {
    let mut rows=c.query(&format!("SELECT scope,check_key,generation FROM health_checks WHERE (?1='all' OR scope=?1) AND ({predicate}) ORDER BY scope,check_key LIMIT 128"),[scope.text()]).await?;
    let mut selected = Vec::new();
    while let Some(r) = rows.next().await? {
        selected.push(HealthSelectionToken {
            identity: HealthIdentity {
                scope: HealthScope::parse(&r.get::<String>(0)?)?,
                check_key: r.get(1)?,
            },
            generation: r.get(2)?,
        })
    }
    Ok(selected)
}
async fn mark(
    c: &Connection,
    keys: &[HealthSelectionToken],
    reason: i64,
    timestamp: i64,
) -> Result<()> {
    if c.is_autocommit() {
        return Err(invariant());
    }
    if keys.iter().any(|k| k.generation >= MAX_INTEGER) {
        return Err(Error(StatusCode::CONFLICT, "health_generation_exhausted"));
    }
    let due = timestamp
        .checked_add(if reason == CONFIG { 5 } else { 0 })
        .filter(|v| *v <= MAX_INTEGER)
        .ok_or_else(invariant)?;
    for k in keys {
        if c.execute("UPDATE health_checks SET generation=generation+1,due_at=CASE WHEN ?=8 AND (pending_reasons & 23)!=0 THEN min(due_at,?) ELSE ? END,pending_reasons=pending_reasons|? WHERE scope=? AND check_key=? AND generation=?",params![reason,due,due,reason,k.identity.scope.text(),k.identity.check_key.clone(),k.generation]).await?!=1{return Err(invariant())}
    }
    Ok(())
}
/// Called within the authoritative mutation's Immediate transaction, including equal-value saves.
pub(crate) async fn configuration_changed(c: &Connection, media: MediaDomain) -> Result<()> {
    let scope = match media {
        MediaDomain::Tv => HealthScope::Tv,
        MediaDomain::Movies => HealthScope::Movies,
    };
    let keys = selection(c, scope, "check_key='completed_download_handling'").await?;
    mark(c, &keys, CONFIG, now()?).await
}

/// Provider configuration affects both CDH and communication in the caller's transaction.
pub(crate) async fn provider_configuration_changed(
    c: &Connection,
    media: MediaDomain,
) -> Result<()> {
    let scope = match media {
        MediaDomain::Tv => HealthScope::Tv,
        MediaDomain::Movies => HealthScope::Movies,
    };
    let keys = selection(
        c,
        scope,
        "check_key IN ('completed_download_handling','download_client_communication','download_client_root_folder')",
    )
    .await?;
    mark(c, &keys, CONFIG, now()?).await
}
/// Status is not trailing configuration debounce. Repeated observations cannot postpone work.
/// Callers deduplicate concurrent provider tests; the sole worker serializes refresh outcomes.
pub(crate) async fn communication_status_changed(c: &Connection, media: MediaDomain) -> Result<()> {
    communication_status_at(c, media, now()?).await
}
async fn communication_status_at(c: &Connection, media: MediaDomain, timestamp: i64) -> Result<()> {
    if c.is_autocommit() {
        return Err(invariant());
    }
    let scope = match media {
        MediaDomain::Tv => HealthScope::Tv,
        MediaDomain::Movies => HealthScope::Movies,
    };
    let keys = selection(c, scope, "check_key='download_client_communication'").await?;
    // Preflight before any updates, retaining the authoritative status even at exhaustion.
    if keys.iter().any(|k| k.generation >= MAX_INTEGER) {
        c.execute(
            "UPDATE health_lifecycle SET schedule_error='health_invariant' WHERE id=1",
            (),
        )
        .await?;
        return Ok(());
    }
    let due = timestamp
        .checked_add(5)
        .filter(|v| *v <= MAX_INTEGER)
        .ok_or_else(invariant)?;
    for k in keys {
        if c.execute("UPDATE health_checks SET generation=generation+1,due_at=CASE WHEN due_at IS NULL THEN ? ELSE min(due_at,?) END,pending_reasons=pending_reasons|8 WHERE scope=? AND check_key=? AND generation=?",params![due,due,k.identity.scope.text(),k.identity.check_key,k.generation]).await? != 1 { return Err(invariant()); }
    }
    Ok(())
}

/// Root declarations affect either client's destinations; mappings remain source-domain scoped.
/// Must share the authoritative mutation's transaction so stale probes cannot publish.
pub(crate) async fn download_roots_changed(
    c: &Connection,
    media: Option<MediaDomain>,
) -> Result<()> {
    let scope = match media {
        Some(MediaDomain::Tv) => HealthScope::Tv,
        Some(MediaDomain::Movies) => HealthScope::Movies,
        None => HealthScope::All,
    };
    let keys = selection(c, scope, "check_key='download_client_root_folder'").await?;
    mark(c, &keys, CONFIG, now()?).await
}
