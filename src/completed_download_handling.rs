//! Application-level intent reconciles existing revision-fenced policies and refresh schedules.
use crate::{api::MediaDomain, db::Database};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Query, State, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use libsql::{Connection, params};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Debug)]
pub struct Error(pub(crate) StatusCode, pub(crate) &'static str);
impl From<libsql::Error> for Error {
    fn from(_: libsql::Error) -> Self {
        Self(StatusCode::SERVICE_UNAVAILABLE, "cdh_storage_error")
    }
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (
            self.0,
            Json(crate::api::ApiErrorEnvelope::new(
                self.1,
                "Completed download handling operation failed",
            )),
        )
            .into_response()
    }
}
pub type Result<T> = std::result::Result<T, Error>;
fn invariant() -> Error {
    Error(StatusCode::INTERNAL_SERVER_ERROR, "cdh_invariant")
}
fn bad() -> Error {
    Error(StatusCode::BAD_REQUEST, "invalid_cdh_request")
}
pub(crate) fn domain(m: MediaDomain) -> &'static str {
    match m {
        MediaDomain::Tv => "tv",
        MediaDomain::Movies => "movies",
    }
}
const MAX_SCHEDULES: i64 = 64;
// The read diagnostic and writer preflight share exactly the same aggregate.
// NULL scope parameters describe the all-domain startup reconciliation batch.
const CAPACITY_SQL: &str = "SELECT (SELECT count(*) FROM download_refresh_schedules WHERE intent!='suppressed')+count(*) FROM cdh_scope_authority v WHERE (?1 IS NULL OR v.provider_id=?1) AND (?2 IS NULL OR v.media_type=?2) AND desired_enabled=1 AND NOT EXISTS(SELECT 1 FROM download_refresh_schedules s WHERE s.provider_id=v.provider_id AND s.media_type=v.media_type)";
#[derive(Debug, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename = "CompletedDownloadHandlingReconciliationReason")]
pub enum ReconciliationReason {
    Pending,
    ScheduleLimit,
}
#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(rename = "CompletedDownloadHandling")]
pub struct Settings {
    pub media_type: MediaDomain,
    pub enabled: bool,
    pub defined: bool,
    pub revision: i64,
    pub locally_edited: bool,
    pub reconciliation_pending: bool,
    pub reconciliation_reason: Option<ReconciliationReason>,
    pub effective_scopes: i64,
    pub observation_disabled_scopes: i64,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "CompletedDownloadHandlingUpdate")]
pub struct Update {
    pub enabled: bool,
    pub revision: i64,
}
pub async fn read(c: &Connection, m: MediaDomain) -> Result<Settings> {
    let r=c.query(&format!("SELECT enabled,defined,revision,locally_edited,reconciliation_pending,(SELECT count(*) FROM download_processing_policies p WHERE p.media_type=d.media_type AND p.enabled=1),(SELECT count(*) FROM download_processing_policies p WHERE p.media_type=d.media_type AND p.enabled=1 AND NOT EXISTS(SELECT 1 FROM download_refresh_schedules s WHERE s.provider_id=p.provider_id AND s.media_type=p.media_type AND s.enabled=1)),({CAPACITY_SQL}) FROM completed_download_handling_settings d WHERE media_type=?3"),params![Option::<&str>::None,Option::<&str>::None,domain(m)]).await?.next().await?.ok_or_else(invariant)?;
    let pending = r.get::<i64>(4)? == 1;
    let reconciliation_reason = if !pending {
        None
    } else if r.get::<i64>(7)? > MAX_SCHEDULES {
        Some(ReconciliationReason::ScheduleLimit)
    } else {
        Some(ReconciliationReason::Pending)
    };
    Ok(Settings {
        media_type: m,
        enabled: r.get::<i64>(0)? == 1,
        defined: r.get::<i64>(1)? == 1,
        revision: r.get(2)?,
        locally_edited: r.get::<i64>(3)? == 1,
        reconciliation_pending: pending,
        reconciliation_reason,
        effective_scopes: r.get(5)?,
        observation_disabled_scopes: r.get(6)?,
    })
}
/// Caller owns Immediate transaction. SQL set operations cover every affected scope;
/// the capacity aggregate is not a truncated list of providers. No network or media I/O.
pub(crate) async fn reconcile(
    c: &Connection,
    provider: Option<&str>,
    media: Option<MediaDomain>,
) -> Result<()> {
    if c.is_autocommit() {
        return Err(invariant());
    }
    let media = media.map(domain);
    let filter = "(?1 IS NULL OR v.provider_id=?1) AND (?2 IS NULL OR v.media_type=?2)";
    let needed = c
        .query(CAPACITY_SQL, params![provider, media])
        .await?
        .next()
        .await?
        .ok_or_else(invariant)?
        .get::<i64>(0)?;
    if needed > MAX_SCHEDULES {
        return Err(Error(StatusCode::TOO_MANY_REQUESTS, "schedule_limit"));
    }
    c.execute(&format!("INSERT INTO download_processing_policies(provider_id,media_type,provider_revision,revision,enabled,mode,enabled_override) SELECT v.provider_id,v.media_type,v.provider_revision,1,v.desired_enabled,'copy',NULL FROM cdh_scope_authority v WHERE {filter} AND NOT EXISTS(SELECT 1 FROM download_processing_policies a WHERE a.provider_id=v.provider_id AND a.media_type=v.media_type)"),params![provider,media]).await?;
    c.execute(&format!("UPDATE download_processing_policies AS a SET provider_revision=v.provider_revision,enabled=v.desired_enabled,revision=a.revision+1 FROM cdh_scope_authority v WHERE a.provider_id=v.provider_id AND a.media_type=v.media_type AND {filter} AND (a.provider_revision!=v.provider_revision OR a.enabled!=v.desired_enabled)"),params![provider,media]).await?;
    c.execute(&format!("INSERT INTO download_refresh_schedules(provider_id,media_type,provider_revision,enabled,interval_seconds,next_run_at,revision,intent,requested_enabled) SELECT v.provider_id,v.media_type,v.provider_revision,1,60,0,1,'inherited',NULL FROM cdh_scope_authority v WHERE {filter} AND v.desired_enabled=1 AND NOT EXISTS(SELECT 1 FROM download_refresh_schedules s WHERE s.provider_id=v.provider_id AND s.media_type=v.media_type)"),params![provider,media]).await?;
    // Explicit schedules observe independently of the CDH master; suppression never re-enables.
    let desired = "CASE WHEN p.enabled=0 OR s.intent='suppressed' THEN 0 WHEN s.intent='explicit' THEN s.requested_enabled ELSE v.desired_enabled END";
    c.execute(&format!("UPDATE download_refresh_schedules AS s SET provider_revision=v.provider_revision,enabled={desired},next_run_at=CASE WHEN ({desired})=1 AND (s.enabled=0 OR s.provider_revision!=v.provider_revision) THEN 0 ELSE s.next_run_at END,error_code=CASE WHEN s.error_code='provider_changed' THEN NULL ELSE s.error_code END,revision=s.revision+1 FROM cdh_scope_authority v JOIN providers p ON p.id=v.provider_id WHERE s.provider_id=v.provider_id AND s.media_type=v.media_type AND {filter} AND (s.provider_revision!=v.provider_revision OR s.enabled!=({desired}) OR s.error_code='provider_changed')"),params![provider,media]).await?;
    if provider.is_none() {
        c.execute("UPDATE completed_download_handling_settings SET reconciliation_pending=0 WHERE ?1 IS NULL OR media_type=?1",[media]).await?;
    }
    Ok(())
}
/// A capacity-releasing mutation still commits when the remaining batch does not fit.
/// reconcile performs its aggregate capacity check before any write, so this cannot
/// commit a partially activated prefix. All other errors abort the caller transaction.
pub(crate) async fn reconcile_pending(c: &Connection) -> Result<()> {
    if c.is_autocommit() {
        return Err(invariant());
    }
    if c.query(
        "SELECT 1 FROM completed_download_handling_settings WHERE reconciliation_pending=1 LIMIT 1",
        (),
    )
    .await?
    .next()
    .await?
    .is_none()
    {
        return Ok(());
    }
    match reconcile(c, None, None).await {
        Err(e) if e.1 == "schedule_limit" => Ok(()),
        result => result,
    }
}
/// Upgrade capacity failure leaves all proposed inherited work rolled back. Existing explicit
/// authority remains governed by its old provider/revision fences, not the readiness flag.
pub(crate) async fn startup(db: &Database) -> Result<()> {
    let c = db.connect().await?;
    let pending=c.query("SELECT 1 FROM completed_download_handling_settings WHERE reconciliation_pending=1 LIMIT 1",()).await?.next().await?.is_some();
    if !pending {
        return Ok(());
    }
    let tx = c
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    match reconcile(&tx, None, None).await {
        Ok(()) => {
            tx.commit().await?;
            Ok(())
        }
        Err(e) => {
            tx.rollback().await?;
            if e.1 == "schedule_limit" {
                Ok(())
            } else {
                Err(e)
            }
        }
    }
}
pub(crate) async fn set(
    c: &Connection,
    m: MediaDomain,
    enabled: bool,
    defined: bool,
    revision: i64,
    local: bool,
) -> Result<Settings> {
    if c.is_autocommit() {
        return Err(invariant());
    }
    if !(1..9_007_199_254_740_991).contains(&revision) {
        return Err(bad());
    }
    if c.execute("UPDATE completed_download_handling_settings SET enabled=?,defined=?,revision=revision+1,locally_edited=CASE WHEN ? THEN 1 ELSE locally_edited END WHERE media_type=? AND revision=?",params![enabled,defined,local,domain(m),revision]).await?!=1{return Err(Error(StatusCode::CONFLICT,"cdh_conflict"))}
    // Disabling cannot require a schedule slot. The projection gates explicit-on policies too.
    reconcile(c, None, Some(m)).await?;
    read(c, m).await
}
pub fn router(db: Arc<Database>) -> Router {
    let mut r = Router::new();
    for m in [MediaDomain::Tv, MediaDomain::Movies] {
        r = r.merge(
            Router::new()
                .route(
                    &format!("/api/v1/{}/completed-download-handling", domain(m)),
                    get(fetch).put(update),
                )
                .layer(Extension(m))
                .layer(DefaultBodyLimit::max(4096))
                .layer(axum::middleware::from_fn(deadline))
                .with_state(db.clone()),
        );
    }
    r
}
async fn deadline(request: axum::extract::Request, next: axum::middleware::Next) -> Response {
    match tokio::time::timeout(std::time::Duration::from_secs(5), next.run(request)).await {
        Ok(r) => r,
        Err(_) => Error(StatusCode::SERVICE_UNAVAILABLE, "cdh_timeout").into_response(),
    }
}
async fn fetch(
    State(db): State<Arc<Database>>,
    Extension(m): Extension<MediaDomain>,
    Query(q): Query<BTreeMap<String, String>>,
) -> Result<Json<Settings>> {
    if !q.is_empty() {
        return Err(bad());
    }
    Ok(Json(read(&db.connect().await?, m).await?))
}
async fn update(
    State(db): State<Arc<Database>>,
    Extension(m): Extension<MediaDomain>,
    Query(q): Query<BTreeMap<String, String>>,
    body: std::result::Result<Json<Update>, JsonRejection>,
) -> Result<Json<Settings>> {
    if !q.is_empty() {
        return Err(bad());
    }
    let body = body.map_err(|_| bad())?.0;
    let c = db.connect().await?;
    let tx = c
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    match set(&tx, m, body.enabled, true, body.revision, true).await {
        Ok(v) => {
            tx.commit().await?;
            Ok(Json(v))
        }
        Err(e) => {
            tx.rollback().await?;
            Err(e)
        }
    }
}
