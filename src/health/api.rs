use super::*;
use axum::{
    Router,
    body::Bytes,
    extract::{
        DefaultBodyLimit, Path, Query, State,
        rejection::{JsonRejection, QueryRejection},
    },
    routing::{get, post},
};
use std::sync::Arc;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Page {
    #[serde(default)]
    scope: HealthScope,
    #[serde(default = "sixteen")]
    limit: u16,
    #[serde(default)]
    offset: u16,
}
fn sixteen() -> u16 {
    16
}
fn twenty() -> u16 {
    20
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandPage {
    #[serde(default)]
    scope: HealthScope,
    status: Option<CommandStatus>,
    #[serde(default = "twenty")]
    limit: u16,
    #[serde(default)]
    offset: u16,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    #[serde(default)]
    after_sequence: i64,
    #[serde(default = "sixteen")]
    limit: u16,
}
#[derive(Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum HealthEvaluation {
    NeverRun,
    Current,
    Pending,
    Running,
    Stale,
    Failed,
}
#[derive(Serialize, ts_rs::TS)]
pub struct HealthCheckState {
    pub identity: HealthIdentity,
    pub startup: bool,
    pub scheduled: bool,
    pub generation: i64,
    pub observed_generation: Option<i64>,
    pub checked_at: Option<i64>,
    pub observed_epoch: Option<Uuid>,
    pub evaluation: HealthEvaluation,
    pub last_error: Option<String>,
    pub pending_reasons: i64,
    pub due_at: Option<i64>,
}
#[derive(Default, Serialize, ts_rs::TS)]
pub struct HealthSummary {
    pub total: u16,
    pub current: u16,
    pub never_run: u16,
    pub stale: u16,
    pub failed: u16,
    pub non_ok: u16,
}
#[derive(Serialize, ts_rs::TS)]
pub struct HealthCoverage {
    pub registered_only: bool,
    pub identities: Vec<HealthIdentity>,
}
#[derive(Serialize, ts_rs::TS)]
pub struct HealthSnapshot {
    pub checks: Vec<HealthCheckState>,
    pub issues: Vec<HealthIssue>,
    pub total: u16,
    pub limit: u16,
    pub offset: u16,
    pub lifecycle: HealthLifecycle,
    pub active_command: Option<HealthCommand>,
    pub pending_keys: Vec<HealthSelectionToken>,
    pub summary: HealthSummary,
    pub coverage: HealthCoverage,
}
#[derive(Serialize, ts_rs::TS)]
pub struct HealthTransition {
    pub sequence: i64,
    pub event_id: Uuid,
    pub epoch: Uuid,
    pub command_id: Uuid,
    pub kind: String,
    pub in_grace: bool,
    pub created_at: i64,
    pub issue: HealthIssue,
}
#[derive(Serialize, ts_rs::TS)]
pub struct HealthTransitions {
    pub oldest_retained_sequence: Option<i64>,
    pub latest_sequence: Option<i64>,
    pub next_cursor: i64,
    pub gap: bool,
    pub items: Vec<HealthTransition>,
}
pub fn router(db: Arc<Database>) -> Router {
    Router::new()
        .route("/api/v1/health", get(snapshot))
        .route("/api/v1/health/commands", get(commands).post(create))
        .route("/api/v1/health/commands/{id}", get(detail))
        .route("/api/v1/health/commands/{id}/cancel", post(cancel))
        .route("/api/v1/health/transitions", get(transitions))
        .layer(DefaultBodyLimit::max(4096))
        .layer(axum::middleware::from_fn(deadline))
        .with_state(db)
}
async fn deadline(request: axum::extract::Request, next: axum::middleware::Next) -> Response {
    match tokio::time::timeout(std::time::Duration::from_secs(5), next.run(request)).await {
        Ok(v) => v,
        Err(_) => Error(StatusCode::SERVICE_UNAVAILABLE, "health_timeout").into_response(),
    }
}
fn bounded<T: Serialize>(v: T) -> Result<Json<T>> {
    if serde_json::to_vec(&v).map_err(|_| invariant())?.len() > 1024 * 1024 {
        return Err(Error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "health_response_limit",
        ));
    }
    Ok(Json(v))
}
async fn snapshot(
    State(db): State<Arc<Database>>,
    query: std::result::Result<Query<Page>, QueryRejection>,
) -> Result<Json<HealthSnapshot>> {
    let q = query.map_err(|_| bad())?.0;
    if !(1..=16).contains(&q.limit) || q.offset > 127 {
        return Err(bad());
    }
    let c = db.connect().await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::ReadOnly)
        .await?;
    let result = snapshot_at(&tx, q).await;
    finish(tx, result).await
}
async fn snapshot_at(c: &Connection, q: Page) -> Result<Json<HealthSnapshot>> {
    let life = lifecycle(c).await?;
    let command = active(c).await?;
    let mut rows=c.query("SELECT scope,check_key,startup,scheduled,generation,observed_generation,checked_at,observed_epoch,last_error,pending_reasons,due_at,severity,reason,message,wiki_url,compatibility_type FROM health_checks WHERE (?1='all' OR scope=?1) ORDER BY scope,check_key LIMIT 128",[q.scope.text()]).await?;
    let mut checks = Vec::new();
    let mut issues = Vec::new();
    let mut pending = Vec::new();
    let mut identities = Vec::new();
    let mut summary = HealthSummary::default();
    while let Some(r) = rows.next().await? {
        let identity = HealthIdentity {
            scope: HealthScope::parse(&r.get::<String>(0)?)?,
            check_key: r.get(1)?,
        };
        let generation = r.get::<i64>(4)?;
        let observed_generation = r.get::<Option<i64>>(5)?;
        let observed_epoch = r.get::<Option<String>>(7)?.map(uuid).transpose()?;
        let error = r.get::<Option<String>>(8)?;
        let reasons = r.get::<i64>(9)?;
        let severity = r.get::<Option<i64>>(11)?;
        let current = observed_generation == Some(generation)
            && observed_epoch == Some(life.epoch)
            && error.is_none()
            && reasons == 0;
        let running = command.as_ref().is_some_and(|v| {
            matches!(v.status, CommandStatus::Running)
                && v.members.iter().any(|m| m.identity == identity)
        });
        summary.total += 1;
        if current {
            summary.current += 1
        }
        if severity.is_none() {
            summary.never_run += 1
        }
        if !current && severity.is_some() {
            summary.stale += 1
        }
        if error.is_some() {
            summary.failed += 1
        }
        if severity.is_some_and(|v| v > 0) {
            summary.non_ok += 1
        }
        identities.push(identity.clone());
        if reasons != 0 {
            pending.push(HealthSelectionToken {
                identity: identity.clone(),
                generation,
            })
        }
        let index = summary.total - 1;
        if index < q.offset || index >= q.offset + q.limit {
            continue;
        }
        if let Some(severity) = severity.filter(|v| *v > 0) {
            issues.push(HealthIssue {
                identity: identity.clone(),
                severity: HealthSeverity::parse(severity)?,
                reason: r.get(12)?,
                message: r.get(13)?,
                wiki_url: r.get(14)?,
                compatibility_type: r.get(15)?,
            });
        }
        checks.push(HealthCheckState {
            identity,
            startup: r.get::<i64>(2)? == 1,
            scheduled: r.get::<i64>(3)? == 1,
            generation,
            observed_generation,
            checked_at: r.get(6)?,
            observed_epoch,
            evaluation: if running {
                HealthEvaluation::Running
            } else if error.is_some() {
                HealthEvaluation::Failed
            } else if reasons != 0 {
                HealthEvaluation::Pending
            } else if severity.is_none() {
                HealthEvaluation::NeverRun
            } else if current {
                HealthEvaluation::Current
            } else {
                HealthEvaluation::Stale
            },
            last_error: error,
            pending_reasons: reasons,
            due_at: r.get(10)?,
        });
    }
    bounded(HealthSnapshot {
        checks,
        issues,
        total: summary.total,
        limit: q.limit,
        offset: q.offset,
        lifecycle: life,
        active_command: command,
        pending_keys: pending,
        summary,
        coverage: HealthCoverage {
            registered_only: true,
            identities,
        },
    })
}
async fn create(
    State(db): State<Arc<Database>>,
    query: std::result::Result<Query<Empty>, QueryRejection>,
    body: std::result::Result<Json<HealthCommandInput>, JsonRejection>,
) -> Result<(StatusCode, Json<HealthAdmission>)> {
    query.map_err(|_| bad())?;
    let input = body.map_err(|_| bad())?.0;
    let c = db.connect().await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let result = engine::manual(&tx, input, now()?).await.and_then(bounded);
    Ok((StatusCode::ACCEPTED, finish(tx, result).await?))
}
async fn commands(
    State(db): State<Arc<Database>>,
    query: std::result::Result<Query<CommandPage>, QueryRejection>,
) -> Result<Json<crate::api::ApiPage<HealthCommand>>> {
    let q = query.map_err(|_| bad())?.0;
    if !(1..=100).contains(&q.limit) || q.offset > 128 {
        return Err(bad());
    }
    let c = db.connect().await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::ReadOnly)
        .await?;
    let outcome=async{let predicate="(?1='all' OR EXISTS(SELECT 1 FROM health_command_checks m WHERE m.command_id=c.id AND m.scope=?1)) AND (?2 IS NULL OR c.status=?2)";let status=q.status.map(CommandStatus::text);let total=tx.query(&format!("SELECT count(*) FROM health_commands c WHERE {predicate}"),params![q.scope.text(),status]).await?.next().await?.ok_or_else(invariant)?.get(0)?;let mut rows=tx.query(&format!("SELECT id FROM health_commands c WHERE {predicate} ORDER BY created_at DESC,id DESC LIMIT ?3 OFFSET ?4"),params![q.scope.text(),status,i64::from(q.limit),i64::from(q.offset)]).await?;let mut items=Vec::new();while let Some(r)=rows.next().await?{items.push(read(&tx,uuid(r.get(0)?)?).await?)}bounded(crate::api::ApiPage{items,total,limit:q.limit.into(),offset:q.offset.into()})}.await;
    finish(tx, outcome).await
}
async fn detail(
    State(db): State<Arc<Database>>,
    Path(id): Path<String>,
    query: std::result::Result<Query<Empty>, QueryRejection>,
) -> Result<Json<HealthCommand>> {
    query.map_err(|_| bad())?;
    let id = Uuid::parse_str(&id).map_err(|_| bad())?;
    let c = db.connect().await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::ReadOnly)
        .await?;
    let outcome = read(&tx, id).await.and_then(bounded);
    finish(tx, outcome).await
}
async fn cancel(
    State(db): State<Arc<Database>>,
    Path(id): Path<String>,
    query: std::result::Result<Query<Empty>, QueryRejection>,
    body: Bytes,
) -> Result<Json<HealthCommand>> {
    query.map_err(|_| bad())?;
    if !body.is_empty() {
        return Err(bad());
    }
    let id = Uuid::parse_str(&id).map_err(|_| bad())?;
    let c = db.connect().await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let result = engine::cancel(&tx, id, now()?).await.and_then(bounded);
    finish(tx, result).await
}
async fn transitions(
    State(db): State<Arc<Database>>,
    query: std::result::Result<Query<Cursor>, QueryRejection>,
) -> Result<Json<HealthTransitions>> {
    let q = query.map_err(|_| bad())?.0;
    if !(1..=16).contains(&q.limit) || !(0..=MAX_INTEGER).contains(&q.after_sequence) {
        return Err(bad());
    }
    let c = db.connect().await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::ReadOnly)
        .await?;
    let outcome=async{let r=tx.query("SELECT min(sequence),max(sequence) FROM health_transitions",()).await?.next().await?.ok_or_else(invariant)?;let oldest=r.get::<Option<i64>>(0)?;let latest=r.get::<Option<i64>>(1)?;let mut rows=tx.query("SELECT sequence,event_id,epoch,command_id,kind,in_grace,created_at,scope,check_key,severity,reason,message,wiki_url,compatibility_type FROM health_transitions WHERE sequence>? ORDER BY sequence LIMIT ?",params![q.after_sequence,i64::from(q.limit)]).await?;let mut items=Vec::new();while let Some(r)=rows.next().await?{items.push(HealthTransition{sequence:r.get(0)?,event_id:uuid(r.get(1)?)?,epoch:uuid(r.get(2)?)?,command_id:uuid(r.get(3)?)?,kind:r.get(4)?,in_grace:r.get::<i64>(5)?==1,created_at:r.get(6)?,issue:HealthIssue{identity:HealthIdentity{scope:HealthScope::parse(&r.get::<String>(7)?)?,check_key:r.get(8)?},severity:HealthSeverity::parse(r.get(9)?)?,reason:r.get(10)?,message:r.get(11)?,wiki_url:r.get(12)?,compatibility_type:r.get(13)?}})}bounded(HealthTransitions{oldest_retained_sequence:oldest,latest_sequence:latest,next_cursor:items.last().map_or(q.after_sequence,|v|v.sequence),gap:oldest.is_some_and(|v|q.after_sequence<v-1),items})}.await;
    finish(tx, outcome).await
}
