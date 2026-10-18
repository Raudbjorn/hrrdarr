//! Completed owned downloads use one immutable import operation per submission receipt.
use super::*;
use crate::search::ReleaseTarget;
const PATH_MARKER: &str = "hc021_unresolved_attempt";
/// A downloaded-folder scan that captured a receipt owns it until its settled handback (and
/// a handed-back receipt stays scan-owned). Only an exact captured predecessor operation may
/// continue through the singleton path. Mirrors the 0054 `scan_processing_legacy_guard`.
pub(crate) const SCAN_RESERVED: &str = "(download_processing.scan_command_id IS NOT NULL OR EXISTS(SELECT 1 FROM downloaded_scan_commands s WHERE s.candidate_id=download_processing.candidate_id AND NOT EXISTS(SELECT 1 FROM rss_candidate_imports i WHERE i.candidate_id=s.candidate_id AND i.operation_id=s.predecessor_operation_id)))";
fn reserved_by_scan() -> Error {
    Error(StatusCode::CONFLICT, "processing_reserved_by_scan")
}
async fn scan_reserved(c: &Connection, id: Uuid) -> Result<bool> {
    Ok(c.query(
        &format!("SELECT 1 FROM download_processing WHERE candidate_id=? AND {SCAN_RESERVED}"),
        [id.to_string()],
    )
    .await?
    .next()
    .await?
    .is_some())
}

pub(crate) enum PathReasonChange<'a> {
    Preserve,
    Admission,
    Failure(&'a str),
    Complete,
}
fn measured_marker(code: &str) -> bool {
    if matches!(
        code,
        "hc021_mapping_missing"
            | "hc021_mapping_changed"
            | "hc021_output_missing"
            | "hc021_output_invalid"
    ) {
        return true;
    }
    let parts: Vec<_> = code.split('_').collect();
    parts.len() == 4
        && parts[0] == "hc021"
        && matches!(parts[1], "source" | "root" | "destination")
        && matches!(
            parts[2],
            "missing" | "denied" | "changed" | "invalid" | "io"
        )
        && parts[3] == "prepare"
}
fn validate_reasons(reasons: &[String], reserved: bool) -> Result<Option<&str>> {
    if reasons.len() > 64 || serde_json::to_vec(reasons).map_err(|_| bad())?.len() > 8192 {
        return Err(bad());
    }
    let mut marker = None;
    let mut seen = std::collections::BTreeSet::new();
    for (index, reason) in reasons.iter().enumerate() {
        if !seen.insert(reason)
            || reason.len() > 128
            || !reason
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            return Err(bad());
        }
        if reason.starts_with("hc021_") {
            if !reserved || index != 0 || (reason != PATH_MARKER && !measured_marker(reason)) {
                return Err(bad());
            }
            marker = Some(reason.as_str());
        }
    }
    Ok(marker)
}
pub(crate) fn path_reasons(
    previous: &[String],
    ordinary: &[String],
    change: PathReasonChange<'_>,
) -> Result<Vec<String>> {
    let previous = validate_reasons(previous, true)?;
    validate_reasons(ordinary, false)?;
    let marker = match change {
        PathReasonChange::Preserve => previous,
        PathReasonChange::Admission => Some(previous.unwrap_or(PATH_MARKER)),
        PathReasonChange::Failure(code) if measured_marker(code) => Some(code),
        PathReasonChange::Failure(_) => return Err(bad()),
        PathReasonChange::Complete => None,
    };
    let mut result = Vec::with_capacity(ordinary.len() + usize::from(marker.is_some()));
    if let Some(marker) = marker {
        result.push(marker.to_owned());
    }
    result.extend_from_slice(ordinary);
    validate_reasons(&result, true)?;
    Ok(result)
}
fn encoded_reasons(reasons: &[String]) -> Result<String> {
    serde_json::to_string(reasons).map_err(|_| bad())
}
async fn changed(
    c: &Connection,
    item: &DownloadProcessing,
    change: crate::health::PathDiagnosticChange,
) -> Result<()> {
    crate::health::path_diagnostics_changed(c, media(&item.target), change)
        .await
        .map_err(|e| Error(e.0, e.1))
}
#[derive(Clone, Copy, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ProcessingMode {
    Copy,
    Hardlink,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ProcessingPolicyInput {
    pub provider_revision: i64,
    pub revision: Option<i64>,
    pub enabled: bool,
    pub mode: ProcessingMode,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ProcessingPolicyReset {
    pub provider_revision: i64,
    pub revision: Option<i64>,
    pub mode: ProcessingMode,
}
#[derive(Serialize, ts_rs::TS)]
pub struct ProcessingPolicy {
    pub observation_suppressed: bool,
    pub observation_enabled: bool,
    pub enabled_override: Option<bool>,
    pub desired_enabled: bool,
    pub provider_id: Uuid,
    pub media_type: MediaDomain,
    pub provider_revision: i64,
    pub revision: Option<i64>,
    pub enabled: bool,
    pub mode: ProcessingMode,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ProcessingInput {
    pub provider_id: Uuid,
    pub provider_revision: i64,
    pub media_type: MediaDomain,
    pub receipt_ids: Vec<Uuid>,
}
#[derive(Clone, Copy, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ProcessingStatus {
    Queued,
    Checking,
    Importing,
    Imported,
    Blocked,
    Cancelled,
}
#[derive(Serialize, ts_rs::TS)]
pub struct DownloadProcessing {
    pub receipt_id: Uuid,
    pub target: ReleaseTarget,
    pub provider_id: Uuid,
    pub provider_revision: i64,
    pub remote_id: String,
    pub policy_revision: i64,
    pub status: ProcessingStatus,
    // Attempts in the current explicitly authorized preflight round, at most three.
    pub preflight_attempts: u8,
    // Never resets when an operator explicitly retries blocked/cancelled preflight.
    pub total_preflight_attempts: i64,
    pub next_attempt_at: i64,
    pub operation_id: Option<Uuid>,
    pub import_phase: Option<String>,
    pub resume_requested: bool,
    pub retirement_state: Option<String>,
    // Upgrade recycle of the replaced file: admitted/copied (moving to the recycle bin),
    // complete (in the bin) or retained (recycling failed; kept in the recovery artifact).
    // Null when no recycle bin was configured at retirement.
    pub recycle_state: Option<String>,
    // Original bytes remain in the private recovery artifact inside the library folder.
    pub recovery_bytes_retained: bool,
    pub error_code: Option<String>,
    pub path_failure_code: Option<String>,
    pub recovery_required: bool,
    pub reasons: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
    // Newest downloaded-folder scan that reserved, settled or continues (explicit successor)
    // this receipt, or the latest scan that read its linked predecessor.
    pub scan_command_id: Option<Uuid>,
    // The receipt is owned by scan coverage proof: singleton retry/cancel are refused
    // (`409 processing_reserved_by_scan`); only a coverage-aware scan successor continues it.
    pub scan_reserved: bool,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ProcessingQuery {
    #[ts(optional)]
    pub provider_id: Option<Uuid>,
    #[ts(optional)]
    pub media_type: Option<MediaDomain>,
    #[ts(optional)]
    pub status: Option<ProcessingStatus>,
    #[ts(optional)]
    pub receipt_id: Option<Uuid>,
    #[serde(default = "default_limit")]
    #[ts(as = "Option<u16>", optional)]
    pub limit: u16,
    #[serde(default)]
    #[ts(as = "Option<u32>", optional)]
    pub offset: u32,
}
impl ProcessingMode {
    fn text(self) -> &'static str {
        match self {
            Self::Copy => "copy",
            Self::Hardlink => "hardlink",
        }
    }
    fn parse(value: &str) -> Result<Self> {
        match value {
            "copy" => Ok(Self::Copy),
            "hardlink" => Ok(Self::Hardlink),
            _ => Err(bad()),
        }
    }
}
impl ProcessingStatus {
    fn text(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Checking => "checking",
            Self::Importing => "importing",
            Self::Imported => "imported",
            Self::Blocked => "blocked",
            Self::Cancelled => "cancelled",
        }
    }
    fn parse(value: &str) -> Result<Self> {
        match value {
            "queued" => Ok(Self::Queued),
            "checking" => Ok(Self::Checking),
            "importing" => Ok(Self::Importing),
            "imported" => Ok(Self::Imported),
            "blocked" => Ok(Self::Blocked),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(bad()),
        }
    }
}
const COLUMNS: &str = "d.candidate_id,r.media_type,r.series_id,r.movie_id,e.episode_id,r.client_id,r.client_revision,r.observed_hash,d.policy_revision,d.status,d.preflight_attempts,d.total_preflight_attempts,d.next_attempt_at,i.operation_id,j.phase,d.error_code,CASE WHEN typeof(d.reasons_json)='text' AND length(CAST(d.reasons_json AS BLOB))<=8192 THEN CAST(d.reasons_json AS BLOB) ELSE x'00' END,d.created_at,d.updated_at,d.resume_requested,CASE WHEN i.old_file_json IS NOT NULL THEN i.retirement_state ELSE NULL END,CASE WHEN j.path_failure_code IS NULL THEN NULL WHEN typeof(j.path_failure_code)='text' AND length(CAST(j.path_failure_code AS BLOB))<=64 THEN CAST(j.path_failure_code AS BLOB) ELSE x'00' END,j.error_code IS NOT NULL,coalesce((SELECT s.id FROM downloaded_scan_commands s WHERE s.candidate_id=d.candidate_id ORDER BY s.created_at DESC,s.id DESC LIMIT 1),d.scan_command_id),(d.scan_command_id IS NOT NULL OR EXISTS(SELECT 1 FROM downloaded_scan_commands s WHERE s.candidate_id=d.candidate_id AND NOT EXISTS(SELECT 1 FROM rss_candidate_imports ri WHERE ri.candidate_id=s.candidate_id AND ri.operation_id=s.predecessor_operation_id))),(SELECT u.phase FROM upgrade_recycles u WHERE u.operation_id=i.operation_id ORDER BY CASE u.phase WHEN 'retained' THEN 0 WHEN 'complete' THEN 2 ELSE 1 END,u.created_at,u.id LIMIT 1)";
const FROM: &str = "download_processing d JOIN rss_candidates r ON r.id=d.candidate_id LEFT JOIN rss_candidate_episodes e ON e.candidate_id=r.id LEFT JOIN rss_candidate_imports i ON i.candidate_id=r.id LEFT JOIN import_journal j ON j.operation_id=i.operation_id";
fn positive(value: i64) -> Result<i64> {
    if (1..=MAX_REVISION).contains(&value) {
        Ok(value)
    } else {
        Err(bad())
    }
}
fn row(r: libsql::Row) -> Result<DownloadProcessing> {
    let target = match r.get::<String>(1)?.as_str() {
        "tv" => ReleaseTarget::Tv {
            series_id: positive(r.get(2)?)?,
            episode_ids: vec![positive(r.get(4)?)?],
        },
        "movies" => ReleaseTarget::Movies {
            movie_id: positive(r.get(3)?)?,
        },
        _ => return Err(bad()),
    };
    let reasons: Vec<String> = serde_json::from_slice(&r.get::<Vec<u8>>(16)?).map_err(|_| bad())?;
    validate_reasons(&reasons, true)?;
    let operation_id = r
        .get::<Option<String>>(13)?
        .map(|s| Uuid::parse_str(&s).map_err(|_| bad()))
        .transpose()?;
    let import_phase: Option<String> = r.get(14)?;
    let error_code: Option<String> = r.get(15)?;
    let journal_error = r.get::<i64>(22)? == 1;
    let journal_path = crate::import::checked_path_failure(&r, 21).map_err(|_| bad())?;
    let recovery_required = if let Some(operation_id) = operation_id {
        import_phase.as_deref() != Some("complete")
            && (journal_path.is_some() || journal_error || error_code.is_some())
            && !crate::import::operation_active(&operation_id.to_string())
                .map_err(|e| Error(StatusCode::SERVICE_UNAVAILABLE, e.code()))?
    } else {
        false
    };
    let path_failure_code = journal_path;
    Ok(DownloadProcessing {
        receipt_id: Uuid::parse_str(&r.get::<String>(0)?).map_err(|_| bad())?,
        target,
        provider_id: Uuid::parse_str(&r.get::<String>(5)?).map_err(|_| bad())?,
        provider_revision: r.get(6)?,
        remote_id: r.get(7)?,
        policy_revision: r.get(8)?,
        status: ProcessingStatus::parse(&r.get::<String>(9)?)?,
        preflight_attempts: r.get::<i64>(10)? as u8,
        total_preflight_attempts: r.get(11)?,
        next_attempt_at: r.get(12)?,
        operation_id,
        import_phase,
        error_code,
        path_failure_code,
        recovery_required,
        reasons,
        created_at: r.get(17)?,
        updated_at: r.get(18)?,
        resume_requested: r.get::<i64>(19)? == 1,
        retirement_state: r.get(20)?,
        recycle_state: r.get(25)?,
        recovery_bytes_retained: r
            .get::<Option<String>>(20)?
            .is_some_and(|v| v == "quarantined" || v == "shared_retained")
            && r.get::<Option<String>>(25)?.as_deref() != Some("complete"),
        scan_command_id: r
            .get::<Option<String>>(23)?
            .map(|s| Uuid::parse_str(&s).map_err(|_| bad()))
            .transpose()?,
        scan_reserved: r.get::<i64>(24)? == 1,
    })
}
async fn read(c: &Connection, id: Uuid) -> Result<DownloadProcessing> {
    row(c
        .query(
            &format!("SELECT {COLUMNS} FROM {FROM} WHERE d.candidate_id=?"),
            [id.to_string()],
        )
        .await?
        .next()
        .await?
        .ok_or(Error(StatusCode::NOT_FOUND, "processing_not_found"))?)
}
fn target(value: &ReleaseTarget) -> Result<crate::db::MediaTarget> {
    match value {
        ReleaseTarget::Movies { movie_id } => Ok(crate::db::MediaTarget::Movie(*movie_id)),
        ReleaseTarget::Tv { episode_ids, .. } if episode_ids.len() == 1 => {
            Ok(crate::db::MediaTarget::Episode(episode_ids[0]))
        }
        _ => Err(bad()),
    }
}
fn media(value: &ReleaseTarget) -> MediaDomain {
    match value {
        ReleaseTarget::Tv { .. } => MediaDomain::Tv,
        ReleaseTarget::Movies { .. } => MediaDomain::Movies,
    }
}
async fn policy(c: &Connection, id: Uuid, media: MediaDomain) -> Result<ProcessingPolicy> {
    let provider=c.query("SELECT p.revision FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=? AND p.implementation IN (SELECT implementation FROM provider_implementations WHERE kind='download_client') AND s.media_type=?",params![id.to_string(),domain(media)]).await?.next().await?.ok_or(Error(StatusCode::NOT_FOUND,"provider_not_found"))?;
    let revision: i64 = provider.get(0)?;
    let stored=c.query("SELECT provider_revision,revision,enabled,mode,enabled_override FROM download_processing_policies WHERE provider_id=? AND media_type=?",params![id.to_string(),domain(media)]).await?.next().await?;
    let master = crate::completed_download_handling::read(c, media)
        .await
        .map_err(|e| Error(e.0, e.1))?
        .enabled;
    let observation=c.query("SELECT intent,enabled FROM download_refresh_schedules WHERE provider_id=? AND media_type=?",params![id.to_string(),domain(media)]).await?.next().await?;
    let (observation_suppressed, observation_enabled) = match observation {
        Some(r) => (r.get::<String>(0)? == "suppressed", r.get::<i64>(1)? == 1),
        None => (false, false),
    };
    if let Some(r) = stored {
        let enabled_override = r.get::<Option<i64>>(4)?.map(|v| v == 1);
        Ok(ProcessingPolicy {
            observation_suppressed,
            observation_enabled,
            enabled_override,
            desired_enabled: master && enabled_override.unwrap_or(true),
            provider_id: id,
            media_type: media,
            provider_revision: r.get(0)?,
            revision: Some(r.get(1)?),
            enabled: r.get::<i64>(2)? == 1,
            mode: ProcessingMode::parse(&r.get::<String>(3)?)?,
        })
    } else {
        Ok(ProcessingPolicy {
            observation_suppressed,
            observation_enabled,
            enabled_override: None,
            desired_enabled: master,
            provider_id: id,
            media_type: media,
            provider_revision: revision,
            revision: None,
            enabled: false,
            mode: ProcessingMode::Copy,
        })
    }
}
async fn authorization(c: &Connection, item: &DownloadProcessing) -> Result<ProcessingMode> {
    let Some(r)=c.query("SELECT p.revision,p.enabled,v.revision,v.enabled,p.provider_revision,p.mode FROM download_processing_policies p JOIN providers v ON v.id=p.provider_id JOIN provider_scopes s ON s.provider_id=v.id AND s.media_type=p.media_type WHERE p.provider_id=? AND p.media_type=?",params![item.provider_id.to_string(),domain(media(&item.target))]).await?.next().await? else{return Err(Error(StatusCode::CONFLICT,"provider_changed"))};
    if r.get::<i64>(2)? != item.provider_revision
        || r.get::<i64>(4)? != item.provider_revision
        || r.get::<i64>(3)? != 1
    {
        return Err(Error(StatusCode::CONFLICT, "provider_changed"));
    }
    if r.get::<i64>(0)? != item.policy_revision || r.get::<i64>(1)? != 1 {
        return Err(Error(StatusCode::CONFLICT, "processing_disabled"));
    }
    ProcessingMode::parse(&r.get::<String>(5)?)
}
pub(super) fn router(db: Arc<Database>) -> Router {
    Router::new()
        .route("/api/v1/download-processing", get(list).post(process))
        .route("/api/v1/download-processing/{id}", get(detail))
        .route("/api/v1/download-processing/{id}/cancel", post(cancel))
        .route(
            "/api/v1/download-processing/policies/{provider_id}/{media_type}",
            get(get_policy).put(save_policy),
        )
        .route(
            "/api/v1/download-processing/policies/{provider_id}/{media_type}/inherit",
            axum::routing::put(reset_policy),
        )
        .layer(DefaultBodyLimit::max(8192))
        .layer(axum::middleware::from_fn(deadline))
        .with_state(db)
}
async fn deadline(request: axum::extract::Request, next: axum::middleware::Next) -> Response {
    match tokio::time::timeout(std::time::Duration::from_secs(5), next.run(request)).await {
        Ok(response) => response,
        Err(_) => Error(StatusCode::SERVICE_UNAVAILABLE, "processing_timeout").into_response(),
    }
}
async fn get_policy(
    State(db): State<Arc<Database>>,
    Path((id, m)): Path<(String, String)>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
) -> Result<Json<ProcessingPolicy>> {
    q.map_err(|_| bad())?;
    let c = connection(&db).await?;
    let tx = c.transaction().await?;
    let value = policy(
        &tx,
        Uuid::parse_str(&id).map_err(|_| bad())?,
        MediaDomain::parse(&m).map_err(|_| bad())?,
    )
    .await?;
    tx.commit().await?;
    bounded(value)
}
async fn save_policy(
    State(db): State<Arc<Database>>,
    Path((id, m)): Path<(String, String)>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
    input: std::result::Result<Json<ProcessingPolicyInput>, JsonRejection>,
) -> Result<Json<ProcessingPolicy>> {
    q.map_err(|_| bad())?;
    let input = input.map_err(|_| bad())?.0;
    let id = Uuid::parse_str(&id).map_err(|_| bad())?;
    let m = MediaDomain::parse(&m).map_err(|_| bad())?;
    write_policy(
        &db,
        id,
        m,
        input.provider_revision,
        input.revision,
        Some(input.enabled),
        input.mode,
    )
    .await
}
async fn reset_policy(
    State(db): State<Arc<Database>>,
    Path((id, m)): Path<(String, String)>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
    input: std::result::Result<Json<ProcessingPolicyReset>, JsonRejection>,
) -> Result<Json<ProcessingPolicy>> {
    q.map_err(|_| bad())?;
    let input = input.map_err(|_| bad())?.0;
    write_policy(
        &db,
        Uuid::parse_str(&id).map_err(|_| bad())?,
        MediaDomain::parse(&m).map_err(|_| bad())?,
        input.provider_revision,
        input.revision,
        None,
        input.mode,
    )
    .await
}
async fn write_policy(
    db: &Database,
    id: Uuid,
    m: MediaDomain,
    provider_revision: i64,
    revision: Option<i64>,
    enabled: Option<bool>,
    mode: ProcessingMode,
) -> Result<Json<ProcessingPolicy>> {
    positive(provider_revision)?;
    let c = connection(db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome=async {
        let current=policy(&tx,id,m).await?;
        if current.revision!=revision || revision.is_some_and(|v| !(1..MAX_REVISION).contains(&v)){return Err(conflict())}
        if tx.query("SELECT 1 FROM providers WHERE id=? AND revision=? AND (?=0 OR enabled=1)",params![id.to_string(),provider_revision,i64::from(enabled==Some(true))]).await?.next().await?.is_none(){return Err(Error(StatusCode::CONFLICT,"provider_changed"))}
        // Store intent without temporarily granting authority; the same transaction reconciles it.
        tx.execute("INSERT INTO download_processing_policies(provider_id,media_type,provider_revision,revision,enabled,mode,enabled_override)VALUES(?,?,?,1,0,?,?) ON CONFLICT(provider_id,media_type)DO UPDATE SET provider_revision=excluded.provider_revision,revision=download_processing_policies.revision+1,enabled=0,mode=excluded.mode,enabled_override=excluded.enabled_override",params![id.to_string(),domain(m),provider_revision,mode.text(),enabled.map(i64::from)]).await?;
        crate::completed_download_handling::reconcile(&tx,Some(&id.to_string()),Some(m)).await.map_err(|e|Error(e.0,e.1))?;
        bounded(policy(&tx,id,m).await?)
    }.await;
    finish(tx, outcome).await
}

async fn process(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
    input: std::result::Result<Json<ProcessingInput>, JsonRejection>,
) -> Result<(StatusCode, Json<Vec<DownloadProcessing>>)> {
    q.map_err(|_| bad())?;
    let input = input.map_err(|_| bad())?.0;
    let ids = input
        .receipt_ids
        .iter()
        .collect::<std::collections::BTreeSet<_>>();
    if ids.is_empty() || ids.len() > 100 || ids.len() != input.receipt_ids.len() {
        return Err(bad());
    }
    let c = connection(&db).await?;
    // The reservation outlives transaction cleanup, including cancellation of this future.
    let mut resume_guard = None;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        let timestamp = now()?;
        let mut rows = Vec::new();
        for id in input.receipt_ids {
            let valid=tx.query("SELECT 1 FROM rss_candidates WHERE id=? AND status='observed' AND client_id=? AND client_revision=? AND media_type=?",params![id.to_string(),input.provider_id.to_string(),input.provider_revision,domain(input.media_type)]).await?.next().await?.is_some();
            if !valid { return Err(Error(StatusCode::CONFLICT,"invalid_processing_receipt")); }
            // A scan-reserved receipt is retried only through coverage-aware scan authority.
            if scan_reserved(&tx,id).await? || tx.query("SELECT 1 FROM downloaded_scan_commands WHERE candidate_id=? AND NOT EXISTS(SELECT 1 FROM download_processing d WHERE d.candidate_id=downloaded_scan_commands.candidate_id)",[id.to_string()]).await?.next().await?.is_some() { return Err(reserved_by_scan()); }
            let existing = match read(&tx,id).await {
                Ok(row) => Some(row),
                Err(Error(StatusCode::NOT_FOUND,"processing_not_found")) => None,
                Err(e) => return Err(e),
            };
            if let Some(item) = existing.as_ref().filter(|r| r.operation_id.is_some()) {
                if item.import_phase.as_deref() != Some("complete") {
                    if !matches!(item.status,ProcessingStatus::Importing) || !item.recovery_required { return Err(conflict()); }
                    if resume_guard.is_none() {
                        resume_guard = Some(crate::import::acquire_resume_guard(&db).map_err(|e| Error(StatusCode::CONFLICT,e.code()))?);
                    }
                    // A linked journal retains recovery authority when new-import policy is disabled.
                    if tx.execute("UPDATE download_processing SET resume_requested=1,next_attempt_at=?,updated_at=? WHERE candidate_id=? AND status='importing' AND total_preflight_attempts=?",params![timestamp,timestamp,id.to_string(),item.total_preflight_attempts]).await? != 1 { return Err(conflict()); }
                    changed(&tx,item,crate::health::PathDiagnosticChange::Progress).await?;
                }
            } else {
                let policy = policy(&tx,input.provider_id,input.media_type).await?;
                if !policy.enabled || policy.provider_revision!=input.provider_revision { return Err(Error(StatusCode::CONFLICT,"processing_disabled")); }
                let revision = policy.revision.ok_or_else(bad)?;
                if let Some(item) = existing {
                    if matches!(item.status,ProcessingStatus::Blocked|ProcessingStatus::Cancelled) {
                        let reasons = path_reasons(&item.reasons,&[],PathReasonChange::Preserve)?;
                        tx.execute("UPDATE download_processing SET status='queued',policy_revision=?,preflight_attempts=0,error_code=NULL,reasons_json=?,next_attempt_at=?,updated_at=? WHERE candidate_id=?",params![revision,encoded_reasons(&reasons)?,timestamp,timestamp,id.to_string()]).await?;
                        changed(&tx,&item,crate::health::PathDiagnosticChange::Progress).await?;
                    }
                } else {
                    tx.execute("INSERT INTO download_processing(candidate_id,policy_revision,status,next_attempt_at,created_at,updated_at)VALUES(?,?,'queued',?,?,?)",params![id.to_string(),revision,timestamp,timestamp,timestamp]).await?;
                }
            }
            rows.push(read(&tx,id).await?);
        }
        bounded(rows)
    }.await;
    let result = finish(tx, outcome).await;
    drop(resume_guard);
    Ok((StatusCode::ACCEPTED, result?))
}

async fn list(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<ProcessingQuery>, QueryRejection>,
) -> Result<Json<ApiPage<DownloadProcessing>>> {
    let q = q.map_err(|_| bad())?.0;
    if q.limit == 0 || q.limit > 100 || q.offset > 10000 {
        return Err(bad());
    }
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::ReadOnly)
        .await?;
    let outcome=async{
        let provider=q.provider_id.map(|v|v.to_string());let media=q.media_type.map(domain);let status=q.status.map(ProcessingStatus::text);let receipt=q.receipt_id.map(|v|v.to_string());
        let filter="(? IS NULL OR r.client_id=?) AND (? IS NULL OR r.media_type=?) AND (? IS NULL OR d.status=?) AND (? IS NULL OR d.candidate_id=?)";
        let total=tx.query(&format!("SELECT count(*) FROM {FROM} WHERE {filter}"),params![provider.clone(),provider.clone(),media,media,status,status,receipt.clone(),receipt.clone()]).await?.next().await?.ok_or_else(bad)?.get::<i64>(0)?;
        let mut result=tx.query(&format!("SELECT {COLUMNS} FROM {FROM} WHERE {filter} ORDER BY d.created_at DESC,d.candidate_id DESC LIMIT ? OFFSET ?"),params![provider.clone(),provider,media,media,status,status,receipt.clone(),receipt,i64::from(q.limit),i64::from(q.offset)]).await?;
        let mut items=Vec::new();while let Some(r)=result.next().await?{items.push(row(r)?)}bounded(ApiPage{items,total,limit:q.limit,offset:q.offset})
    }.await;
    finish(tx, outcome).await
}
async fn detail(
    State(db): State<Arc<Database>>,
    Path(id): Path<String>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
) -> Result<Json<DownloadProcessing>> {
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
) -> Result<Json<DownloadProcessing>> {
    q.map_err(|_| bad())?;
    let id = Uuid::parse_str(&id).map_err(|_| bad())?;
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        let item = read(&tx,id).await?;
        if item.operation_id.is_some() { return Err(conflict()); }
        if scan_reserved(&tx,id).await? { return Err(reserved_by_scan()); }
        let reasons = path_reasons(&item.reasons,&[],PathReasonChange::Preserve)?;
        tx.execute("UPDATE download_processing SET status='cancelled',error_code=NULL,reasons_json=?,updated_at=? WHERE candidate_id=?",params![encoded_reasons(&reasons)?,now()?,id.to_string()]).await?;
        changed(&tx,&item,crate::health::PathDiagnosticChange::Progress).await?;
        bounded(read(&tx,id).await?)
    }.await;
    finish(tx, outcome).await
}

pub(super) async fn observe(
    c: &Connection,
    scope: RefreshTarget,
    revision: i64,
    items: &[crate::providers::qbittorrent::DownloadItem],
    timestamp: i64,
) -> Result<()> {
    for item in items.iter().filter(|item| item.completed) {
        c.execute("INSERT INTO download_processing(candidate_id,policy_revision,status,next_attempt_at,created_at,updated_at) SELECT r.id,p.revision,'queued',?,?,? FROM rss_candidates r JOIN download_processing_policies p ON p.provider_id=r.client_id AND p.media_type=r.media_type WHERE r.status='observed' AND r.client_id=? AND r.client_revision=? AND r.media_type=? AND r.observed_hash=? AND p.enabled=1 AND p.provider_revision=r.client_revision AND NOT EXISTS(SELECT 1 FROM download_processing d WHERE d.candidate_id=r.id) AND NOT EXISTS(SELECT 1 FROM downloaded_scan_commands s WHERE s.candidate_id=r.id)",params![timestamp,timestamp,timestamp,scope.provider_id.to_string(),revision,domain(scope.media_type),item.hash.clone()]).await?;
    }
    Ok(())
}
pub(super) async fn recover(c: &Connection, timestamp: i64, code: &str) -> Result<()> {
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        let mut rows = tx.query(&format!("SELECT candidate_id FROM download_processing WHERE status IN ('checking','queued') AND NOT {SCAN_RESERVED} ORDER BY candidate_id LIMIT 1025"),()).await?;
        let mut ids = Vec::new();
        while let Some(row) = rows.next().await? {
            if ids.len() == 1024 { return Err(bad()); }
            ids.push(Uuid::parse_str(&row.get::<String>(0)?).map_err(|_| bad())?);
        }
        drop(rows);
        for id in ids {
            let item = read(&tx,id).await?;
            if matches!(item.status,ProcessingStatus::Checking) {
                failure_in(&tx,&item,code,vec![code.into()],None,true,None).await?;
            } else if let Err(error) = authorization(&tx,&item).await {
                if !matches!(error.1,"provider_changed"|"processing_disabled") { return Err(error); }
                let reasons = path_reasons(&item.reasons,&[],PathReasonChange::Preserve)?;
                tx.execute("UPDATE download_processing SET status='blocked',error_code='processing_disabled',reasons_json=?,updated_at=? WHERE candidate_id=? AND status='queued' AND total_preflight_attempts=?",params![encoded_reasons(&reasons)?,timestamp,id.to_string(),item.total_preflight_attempts]).await?;
                changed(&tx,&item,crate::health::PathDiagnosticChange::Progress).await?;
            }
        }
        Ok(())
    }.await;
    finish(tx, outcome).await
}
pub(super) async fn claim(c: &Connection, id: Uuid, timestamp: i64) -> Result<DownloadProcessing> {
    if c.is_autocommit() {
        return Err(bad());
    }
    let item = read(c, id).await?;
    if matches!(item.status, ProcessingStatus::Importing) {
        return Ok(item);
    }
    if !matches!(item.status, ProcessingStatus::Queued)
        || item.total_preflight_attempts >= MAX_REVISION
    {
        return Err(conflict());
    }
    if scan_reserved(c, id).await? {
        return Err(reserved_by_scan());
    }
    authorization(c, &item).await?;
    let reasons = path_reasons(&item.reasons, &[], PathReasonChange::Admission)?;
    if c.execute("UPDATE download_processing SET status='checking',preflight_attempts=preflight_attempts+1,total_preflight_attempts=total_preflight_attempts+1,error_code=NULL,reasons_json=?,updated_at=? WHERE candidate_id=? AND status='queued' AND total_preflight_attempts=?",params![encoded_reasons(&reasons)?,timestamp,id.to_string(),item.total_preflight_attempts]).await? != 1 { return Err(conflict()); }
    changed(c, &item, crate::health::PathDiagnosticChange::Admission).await?;
    read(c, id).await
}
fn same_attempt(current: &DownloadProcessing, captured: &DownloadProcessing) -> bool {
    current.receipt_id == captured.receipt_id
        && current.target == captured.target
        && current.provider_id == captured.provider_id
        && current.provider_revision == captured.provider_revision
        && current.policy_revision == captured.policy_revision
        && current.remote_id == captured.remote_id
        && current.total_preflight_attempts == captured.total_preflight_attempts
        && current.operation_id == captured.operation_id
}
struct MappingFence {
    id: i64,
    revision: i64,
    host: String,
}
async fn blocked(
    c: &Connection,
    item: &DownloadProcessing,
    code: &str,
    reasons: Vec<String>,
    marker: Option<&str>,
) -> Result<()> {
    persist_failure(c, item, code, reasons, marker, false, None).await
}
async fn persist_failure(
    c: &Connection,
    item: &DownloadProcessing,
    code: &str,
    reasons: Vec<String>,
    marker: Option<&str>,
    allow_retry: bool,
    mapping: Option<&MappingFence>,
) -> Result<()> {
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = failure_in(&tx, item, code, reasons, marker, allow_retry, mapping).await;
    finish(tx, outcome).await
}
async fn failure_in(
    c: &Connection,
    item: &DownloadProcessing,
    code: &str,
    mut reasons: Vec<String>,
    mut marker: Option<&str>,
    allow_retry: bool,
    mapping: Option<&MappingFence>,
) -> Result<()> {
    if c.is_autocommit() {
        return Err(bad());
    }
    let current = read(c, item.receipt_id).await?;
    if !same_attempt(&current, item)
        || current.operation_id.is_some()
        || !matches!(current.status, ProcessingStatus::Checking)
    {
        return Ok(());
    }
    let mut code = match code {
        "command_storage_error" | "invalid_command_request" => "storage_error",
        other => other,
    };
    // A changed authority is a current generic fact, not evidence about the old mapped path.
    let authorized = match authorization(c, &current).await {
        Ok(_) => true,
        Err(error) if matches!(error.1, "provider_changed" | "processing_disabled") => {
            code = error.1;
            reasons = vec![code.into()];
            marker = None;
            false
        }
        Err(error) => return Err(error),
    };
    if authorized {
        if let Some(mapping) = mapping {
            let matches = c.query("SELECT 1 FROM remote_path_mappings WHERE id=? AND revision=? AND host=? AND media_type=?",params![mapping.id,mapping.revision,mapping.host.clone(),domain(media(&item.target))]).await?.next().await?.is_some();
            if !matches {
                code = "path_mapping_changed";
                reasons = vec![code.into()];
                marker = Some("hc021_mapping_changed");
            }
        }
    }
    let reasons = path_reasons(
        &current.reasons,
        &reasons,
        match marker {
            Some(code) => PathReasonChange::Failure(code),
            None => PathReasonChange::Preserve,
        },
    )?;
    let retry = authorized
        && allow_retry
        && matches!(
            code,
            "interrupted" | "storage_error" | "download_timeout" | "download_unavailable"
        )
        && current.preflight_attempts < 3;
    let timestamp = now()?;
    let status = if retry { "queued" } else { "blocked" };
    let next = if retry {
        timestamp + (1i64 << current.preflight_attempts)
    } else {
        current.next_attempt_at
    };
    if c.execute("UPDATE download_processing SET status=?,error_code=?,reasons_json=?,next_attempt_at=?,updated_at=? WHERE candidate_id=? AND status='checking' AND total_preflight_attempts=?",params![status,code,encoded_reasons(&reasons)?,next,timestamp,item.receipt_id.to_string(),item.total_preflight_attempts]).await? != 1 { return Err(conflict()); }
    changed(c, &current, crate::health::PathDiagnosticChange::Failure).await
}
fn processing_error(code: &str) -> &'static str {
    match code {
        "provider_changed" => "provider_changed",
        "processing_disabled" => "processing_disabled",
        "refresh_timeout" => "download_timeout",
        "target_changed" => "target_changed",
        "import_busy" => "import_busy",
        "source_unavailable" => "source_unavailable",
        _ => "import_conflict",
    }
}
pub(super) async fn run(
    db: &Arc<Database>,
    client: &crate::providers::RefreshClient,
    item: DownloadProcessing,
) -> Result<()> {
    let c = connection(db).await?;
    if item.operation_id.is_some() {
        return advance_import(db, &c, item).await;
    }
    if let Err(Error(_, code)) = preflight(db, client, &item).await {
        persist_failure(&c, &item, code, vec![code.into()], None, true, None).await?;
    }
    Ok(())
}
async fn preflight(
    db: &Arc<Database>,
    client: &crate::providers::RefreshClient,
    item: &DownloadProcessing,
) -> Result<()> {
    let c = connection(db).await?;
    let mode = authorization(&c, item).await?;
    let target = target(&item.target)?;
    let observed = match client
        .inspect_download(
            &item.provider_id.to_string(),
            item.provider_revision,
            &target,
            &item.remote_id,
        )
        .await
    {
        Ok(observed) => observed,
        // Usenet output folders are listed locally, so an unmapped output blocks like an unmapped file.
        Err(e) if e.code == "path_mapping_missing" => {
            blocked(
                &c,
                item,
                "path_mapping_missing",
                vec!["path_mapping_missing".into()],
                Some("hc021_mapping_missing"),
            )
            .await?;
            return Ok(());
        }
        Err(e) => {
            return Err(Error(
                StatusCode::CONFLICT,
                match e.code {
                    "provider_changed" => "provider_changed",
                    "refresh_timeout" => "download_timeout",
                    _ => "download_unavailable",
                },
            ));
        }
    };
    let details = observed.details;
    if matches!(
        details.item.status,
        crate::providers::qbittorrent::DownloadStatus::Failed
    ) {
        return Err(Error(StatusCode::CONFLICT, "download_failed"));
    }
    if !details.item.completed {
        return Err(Error(StatusCode::CONFLICT, "download_not_complete"));
    }
    let mut accepted = None;
    let mut reasons = Vec::new();
    for file in &details.files {
        if file.priority == 0 {
            continue;
        }
        if file.progress < 1.0 {
            return Err(Error(StatusCode::CONFLICT, "download_not_complete"));
        }
        let parts: Vec<_> = file.name.split(['/', '\\']).collect();
        if parts.is_empty()
            || parts
                .iter()
                .any(|p| p.is_empty() || *p == "." || *p == ".." || p.contains(':'))
        {
            return Err(Error(StatusCode::CONFLICT, "invalid_download_files"));
        }
        let result = crate::search::downloaded::evaluate_receipt(
            &c,
            &target,
            &file.name,
            file.size_bytes,
            &item.receipt_id.to_string(),
        )
        .await
        .map_err(|_| Error(StatusCode::CONFLICT, "quality_rejected"))?;
        if let Some(facts) = result.accepted {
            if accepted.is_some() {
                return Err(Error(StatusCode::CONFLICT, "ambiguous_files"));
            }
            let separator = if details.save_path.contains('\\') {
                '\\'
            } else {
                '/'
            };
            let path = format!(
                "{}{}{}",
                details.save_path.trim_end_matches(['/', '\\']),
                separator,
                parts.join(&separator.to_string())
            );
            let mapped = crate::remote_paths::resolve(
                &c,
                media(&item.target),
                crate::remote_paths::ResolveInput {
                    host: observed.host.clone(),
                    path,
                    direction: crate::remote_paths::Direction::RemoteToLocal,
                },
            )
            .await
            .map_err(|_| Error(StatusCode::CONFLICT, "path_mapping_missing"))?;
            let (Some(mapping_id), Some(mapping_revision)) =
                (mapped.mapping_id, mapped.mapping_revision)
            else {
                blocked(
                    &c,
                    item,
                    "path_mapping_missing",
                    vec!["path_mapping_missing".into()],
                    Some("hc021_mapping_missing"),
                )
                .await?;
                return Ok(());
            };
            let destination = match crate::naming::destination::resolve_owned_destination(
                &c,
                &target,
                &facts.root,
                &facts.basename,
                facts.quality_id,
                &facts.revision_binding.effective,
                facts.edition.as_deref(),
                &facts.evidence,
            )
            .await
            {
                Ok(destination) => destination,
                Err(crate::naming::destination::DestinationError::Render(detail)) => {
                    // Stored templates may be corrupt: never log their text or parser errors.
                    // Preserve the existing blocked reason and allowlist diagnostic identifiers.
                    eprintln!(
                        "{}",
                        serde_json::json!({"level":"ERROR","event":"naming_render_failed","candidate_id":item.receipt_id.to_string(),"field":detail.field,"error_class":detail.class})
                    );
                    blocked(
                        &c,
                        item,
                        "unsupported_download",
                        vec!["naming_render_failed".to_string()],
                        None,
                    )
                    .await?;
                    return Ok(());
                }
                Err(crate::naming::destination::DestinationError::Db(e)) => {
                    return Err(Error::from(e));
                }
                Err(crate::naming::destination::DestinationError::Internal(reason)) => {
                    eprintln!("event=naming_destination_unavailable condition={reason}");
                    return Err(Error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "command_storage_error",
                    ));
                }
            };
            accepted = Some(crate::import::OwnedImport {
                candidate_id: item.receipt_id.to_string(),
                target: match target {
                    crate::db::MediaTarget::Episode(id) => crate::db::MediaTarget::Episode(id),
                    crate::db::MediaTarget::Movie(id) => crate::db::MediaTarget::Movie(id),
                },
                source: mapped.output,
                destination,
                mode: match mode {
                    ProcessingMode::Copy => crate::import::Mode::Copy,
                    ProcessingMode::Hardlink => crate::import::Mode::Hardlink,
                },
                expected_size: file.size_bytes,
                quality_id: facts.quality_id,
                revision_json: facts.revision_json,
                edition: facts.edition,
                policy_revision: item.policy_revision,
                total_preflight_attempts: item.total_preflight_attempts,
                mapping_id,
                mapping_revision,
                host: observed.host.clone(),
            });
        } else {
            for reason in result.reasons {
                if !reasons.contains(&reason) {
                    reasons.push(reason);
                }
                if reasons.len() > 64 {
                    return Err(bad());
                }
            }
        }
    }
    let Some(input) = accepted else {
        blocked(&c, item, "quality_rejected", reasons, None).await?;
        return Ok(());
    };
    // Recheck captured mapping facts inside failure persistence; old mapped failures cannot replace newer facts.
    let mapping = MappingFence {
        id: input.mapping_id,
        revision: input.mapping_revision,
        host: input.host.clone(),
    };
    if let Err(error) = crate::import::prepare_owned(db.clone(), input).await {
        let code = processing_error(error.code());
        persist_failure(
            &c,
            item,
            code,
            vec![code.into()],
            error.prepare_failure_code(),
            false,
            Some(&mapping),
        )
        .await?;
    }
    Ok(())
}
async fn advance_import(
    db: &Arc<Database>,
    c: &Connection,
    item: DownloadProcessing,
) -> Result<()> {
    let operation = item.operation_id.ok_or_else(bad)?.to_string();
    let mut reservation = None;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        let current = read(&tx,item.receipt_id).await?;
        if !same_attempt(&current,&item) || !matches!(current.status,ProcessingStatus::Importing) { return Ok(None); }
        if current.import_phase.as_deref() == Some("complete") {
            let reasons = path_reasons(&current.reasons,&[],PathReasonChange::Complete)?;
            if tx.execute("UPDATE download_processing SET status='imported',resume_requested=0,error_code=NULL,reasons_json=?,updated_at=? WHERE candidate_id=? AND status='importing' AND total_preflight_attempts=?",params![encoded_reasons(&reasons)?,now()?,item.receipt_id.to_string(),item.total_preflight_attempts]).await? != 1 { return Err(conflict()); }
            changed(&tx,&current,crate::health::PathDiagnosticChange::Completion).await?;
            return Ok(None);
        }
        if crate::import::operation_active(&operation).map_err(|e| Error(StatusCode::SERVICE_UNAVAILABLE,e.code()))? { return Ok(None); }
        if current.recovery_required && !current.resume_requested {
            if current.error_code.is_none() {
                reservation = match crate::import::acquire_resume_guard(db) {
                    Ok(guard)=>Some(guard),
                    Err(error) if error.code()=="import_busy"=>return Ok(None),
                    Err(error)=>return Err(Error(StatusCode::SERVICE_UNAVAILABLE,error.code())),
                };
                import_failed(&tx,&current).await?;
            }
            return Ok(None);
        }
        let generation = tx.query("SELECT generation FROM health_checks WHERE scope=? AND check_key='remote_path_mapping'",[domain(media(&current.target))]).await?.next().await?.ok_or_else(bad)?.get::<i64>(0)?;
        Ok(Some(generation))
    }.await;
    let generation = finish(tx, outcome).await;
    drop(reservation);
    let Some(generation) = generation? else {
        return Ok(());
    };
    match crate::import::start_owned(db.clone(), &operation).await {
        Ok(_) => {
            // Scheduling does not change evidence or authorization; fence it to this linked lifetime.
            c.execute("UPDATE download_processing SET next_attempt_at=?,updated_at=? WHERE candidate_id=? AND status='importing' AND total_preflight_attempts=? AND EXISTS(SELECT 1 FROM rss_candidate_imports WHERE candidate_id=download_processing.candidate_id AND operation_id=?)",params![now()?+2,now()?,item.receipt_id.to_string(),item.total_preflight_attempts,operation]).await?;
        }
        Err(error) => {
            eprintln!(
                "event=owned_import_start_error receipt_id={} code={}",
                item.receipt_id,
                error.code()
            );
            let guard = match crate::import::acquire_resume_guard(db) {
                Ok(guard) => guard,
                Err(error) if error.code() == "import_busy" => return Ok(()),
                Err(error) => return Err(Error(StatusCode::SERVICE_UNAVAILABLE, error.code())),
            };
            let tx = c
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .await?;
            let outcome = async {
                let current = read(&tx,item.receipt_id).await?;
                let unchanged=tx.query("SELECT 1 FROM health_checks WHERE scope=? AND check_key='remote_path_mapping' AND generation=?",params![domain(media(&current.target)),generation]).await?.next().await?.is_some();
                // A later admission/resume wins; do not consume its permission with this old failure.
                if unchanged && same_attempt(&current,&item) && matches!(current.status,ProcessingStatus::Importing) && current.import_phase.as_deref()!=Some("complete") {
                    import_failed(&tx,&current).await?;
                }
                Ok(())
            }.await;
            let result = finish(tx, outcome).await;
            drop(guard);
            result?;
        }
    }
    Ok(())
}
async fn import_failed(c: &Connection, item: &DownloadProcessing) -> Result<()> {
    if c.is_autocommit() {
        return Err(bad());
    }
    let reasons = path_reasons(
        &item.reasons,
        &["import_failed".into()],
        PathReasonChange::Preserve,
    )?;
    if c.execute("UPDATE download_processing SET resume_requested=0,error_code='import_failed',reasons_json=?,updated_at=? WHERE candidate_id=? AND status='importing' AND total_preflight_attempts=?",params![encoded_reasons(&reasons)?,now()?,item.receipt_id.to_string(),item.total_preflight_attempts]).await? != 1 { return Err(conflict()); }
    changed(c, item, crate::health::PathDiagnosticChange::Failure).await
}

#[cfg(test)]
mod path_diagnostics_tests {
    use super::*;
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    async fn fixture() -> std::result::Result<(Scratch, Arc<Database>, Connection), crate::db::Error>
    {
        let dir = std::env::temp_dir().join(format!("hrrdarr-path-processing-{}", Uuid::new_v4()));
        std::fs::create_dir(&dir)?;
        let scratch = Scratch(dir);
        let db = Arc::new(Database::open_local(scratch.0.join("db")).await?);
        let c = db.connect().await?;
        seed(&c).await?;
        Ok((scratch, db, c))
    }
    async fn seed(c: &Connection) -> std::result::Result<(), crate::db::Error> {
        c.execute_batch("PRAGMA foreign_keys=ON;
      INSERT INTO series(id,tvdb_id,title,path) VALUES(1,101,'Series','/tv/one');
      INSERT INTO seasons(series_id,number) VALUES(1,1);
      INSERT INTO episodes(id,series_id,season,number,title) VALUES(1,1,1,1,'Episode');
      INSERT INTO movie_metadata(id,tmdb_id,title) VALUES(1,201,'Film');
      INSERT INTO movies(id,metadata_id,path) VALUES(1,1,'/movies/one');
      INSERT INTO operations VALUES('tv','episode',1,NULL,'/source/tv','copy','/tv/one/a.mkv','preview','kept');
      INSERT INTO operations VALUES('movie','movie',NULL,1,'/source/movie','copy','/movies/one/a.mkv','preview','kept');
      INSERT INTO import_journal(operation_id,plan_json,phase,error_code) VALUES('tv','{\"private\":\"tv\"}','preview','import_error'),('movie','{\"private\":\"movie\"}','staging',NULL);
      INSERT INTO movie_metadata(id,tmdb_id,title) VALUES(2,202,'Completed Film');
      INSERT INTO movies(id,metadata_id,path) VALUES(2,2,'/movies/completed');
      INSERT INTO movie_files(id,movie_id,path) VALUES(2,2,'/movies/completed/a.mkv');
      INSERT INTO operations VALUES('done','movie',NULL,2,'/source/done','copy','/movies/completed/a.mkv','complete','kept history');
      INSERT INTO import_journal(operation_id,plan_json,stage_json,phase) VALUES('done','{}','{}','staging');
      INSERT INTO import_history(operation_id,media_type,movie_id,movie_file_id,source,destination,size,sha256) VALUES('done','movie',2,2,'/source/done','/movies/completed/a.mkv',1,'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa');
      UPDATE import_journal SET phase='complete' WHERE operation_id='done';
      INSERT INTO remote_path_mappings(media_type,host,remote_path,remote_kind,remote_key,local_path) VALUES('tv','fixture.invalid','/remote','posix','/remote','/source/tv'),('movies','fixture.invalid','/remote','posix','/remote','/source/movie');
      UPDATE health_checks SET generation=1,observed_generation=1,observed_epoch='00000000-0000-0000-0000-000000000001',checked_at=100,severity=0 WHERE check_key='completed_download_handling';").await?;
        let indexer = uuid::Uuid::new_v4().to_string();
        let client = uuid::Uuid::new_v4().to_string();
        for (id, implementation) in [(&indexer, "torznab"), (&client, "qbittorrent")] {
            c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,?,?,1,1,1,1,'http://fixture.invalid')", params![id.clone(),implementation,implementation]).await?;
        }
        // Add both scopes before policy rows: scope changes intentionally invalidate existing policy.
        for media in ["tv", "movies"] {
            let (anime, year, categories) = if media == "tv" {
                (Some(0), None, "[5000]")
            } else {
                (None, Some(0), "[2000]")
            };
            c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year) VALUES(?,'torznab',?,?,'[]',?,?)", params![indexer.clone(),media,categories,anime,year]).await?;
            c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags) VALUES(?,'qbittorrent',?,?,0,0,'started','default',0,0,0)", params![client.clone(),media,media]).await?;
        }
        for (media, digit) in [("tv", "a"), ("movies", "b")] {
            c.execute("INSERT INTO download_processing_policies(provider_id,media_type,provider_revision,revision,enabled,mode) VALUES(?,?,1,1,1,'copy')", params![client.clone(),media]).await?;
            let command = uuid::Uuid::new_v4().to_string();
            let candidate = uuid::Uuid::new_v4().to_string();
            c.execute("INSERT INTO rss_commands(id,name,media_type,indexer_id,indexer_revision,client_id,client_revision,next_attempt_at,created_at) VALUES(?,'rss_sync',?,?,1,?,1,100,100)", params![command.clone(),media,indexer.clone(),client.clone()]).await?;
            c.execute("INSERT INTO rss_candidates(id,command_id,media_type,indexer_id,indexer_revision,client_id,client_revision,fingerprint,title,private_payload,series_id,status,decision_reasons_json,created_at,updated_at) SELECT ?,id,media_type,indexer_id,indexer_revision,client_id,client_revision,?,'Release',?,?,'pending','[]',100,100 FROM rss_commands WHERE id=?", params![candidate.clone(),digit.repeat(64),vec![1u8;29],if media=="tv" {Some(1)} else {None},command]).await?;
            if media == "tv" {
                c.execute(
                    "INSERT INTO rss_candidate_episodes VALUES(?,1,1)",
                    [candidate.clone()],
                )
                .await?;
            }
            let hash = digit.repeat(40);
            let identity = serde_json::json!({"version":1,"target":{"media_type":if media=="tv" {"episode"} else {"movie"},"id":1},"hashes":[hash],"settings_fingerprint":"c".repeat(64),"payload_sha256":"d".repeat(64)}).to_string();
            c.execute("UPDATE rss_candidates SET status='prepared',submission_identity_json=?,movie_id=? WHERE id=?", params![identity,if media=="movies" {Some(1)} else {None},candidate.clone()]).await?;
            c.execute(
                "INSERT INTO rss_hash_claims VALUES(?,?,?)",
                params![client.clone(), hash.clone(), candidate.clone()],
            )
            .await?;
            c.execute(
                "UPDATE rss_candidates SET status='submitting',private_payload=NULL WHERE id=?",
                [candidate.clone()],
            )
            .await?;
            c.execute(
                "UPDATE rss_candidates SET status='observed',observed_hash=? WHERE id=?",
                params![hash, candidate.clone()],
            )
            .await?;
            c.execute("INSERT INTO download_processing(candidate_id,policy_revision,status,next_attempt_at,created_at,updated_at) VALUES(?,1,'queued',100,100,100)", [candidate.clone()]).await?;
            c.execute("UPDATE download_processing SET status='checking',preflight_attempts=1,total_preflight_attempts=1 WHERE candidate_id=?", [candidate.clone()]).await?;
            c.execute("UPDATE download_processing SET status='blocked',error_code='source_unavailable',reasons_json='[\"source_unavailable\"]' WHERE candidate_id=?", [candidate]).await?;
        }
        Ok(())
    }

    async fn receipt(c: &Connection, media: MediaDomain) -> Result<DownloadProcessing> {
        let row = c
            .query(
                "SELECT id FROM rss_candidates WHERE media_type=?",
                [domain(media)],
            )
            .await?
            .next()
            .await?
            .ok_or_else(bad)?;
        read(
            c,
            Uuid::parse_str(&row.get::<String>(0)?).map_err(|_| bad())?,
        )
        .await
    }
    async fn retry(db: &Arc<Database>, item: &DownloadProcessing) -> Result<()> {
        let _ = process(
            State(db.clone()),
            Ok(Query(Empty {})),
            Ok(Json(ProcessingInput {
                provider_id: item.provider_id,
                provider_revision: item.provider_revision,
                media_type: media(&item.target),
                receipt_ids: vec![item.receipt_id],
            })),
        )
        .await?;
        Ok(())
    }
    async fn admitted(c: &Connection, id: Uuid) -> Result<DownloadProcessing> {
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        let result = claim(&tx, id, now()?).await;
        finish(tx, result).await
    }
    async fn generation(c: &Connection, media: MediaDomain) -> Result<i64> {
        Ok(c.query("SELECT generation FROM health_checks WHERE scope=? AND check_key='remote_path_mapping'",[domain(media)]).await?.next().await?.ok_or_else(bad)?.get(0)?)
    }
    #[test]
    fn reason_lifecycle_rejects_duplicates_malformed_markers_and_overflow() {
        let prior = vec![
            "hc021_source_missing_prepare".into(),
            "source_unavailable".into(),
        ];
        assert_eq!(
            path_reasons(&prior, &[], PathReasonChange::Preserve).unwrap(),
            vec!["hc021_source_missing_prepare"]
        );
        assert_eq!(
            path_reasons(&prior, &[], PathReasonChange::Admission).unwrap(),
            vec!["hc021_source_missing_prepare"]
        );
        assert_eq!(
            path_reasons(&[], &[], PathReasonChange::Admission).unwrap(),
            vec![PATH_MARKER]
        );
        assert_eq!(
            path_reasons(
                &prior,
                &[],
                PathReasonChange::Failure("hc021_root_denied_prepare")
            )
            .unwrap(),
            vec!["hc021_root_denied_prepare"]
        );
        assert!(
            path_reasons(&prior, &[], PathReasonChange::Complete)
                .unwrap()
                .is_empty()
        );
        for malformed in [
            vec!["hc021_unknown".into()],
            vec!["ordinary".into(), PATH_MARKER.into()],
            vec!["ordinary".into(), "ordinary".into()],
            vec![PATH_MARKER.into(), PATH_MARKER.into()],
        ] {
            assert!(path_reasons(&malformed, &[], PathReasonChange::Preserve).is_err());
            assert!(path_reasons(&malformed, &[], PathReasonChange::Complete).is_err());
        }
        assert!(
            path_reasons(
                &[],
                &["ordinary".into(), "ordinary".into()],
                PathReasonChange::Complete
            )
            .is_err()
        );
        assert!(path_reasons(&[], &[PATH_MARKER.into()], PathReasonChange::Preserve).is_err());
        assert!(path_reasons(&prior, &[], PathReasonChange::Failure(PATH_MARKER)).is_err());
        assert!(
            path_reasons(
                &prior,
                &[],
                PathReasonChange::Failure("hc021_stage_missing_prepare")
            )
            .is_err()
        );
        let ordinary: Vec<_> = (0..64).map(|i| format!("reason_{i}")).collect();
        assert!(path_reasons(&[], &ordinary, PathReasonChange::Admission).is_err());
        assert!(path_reasons(&[], &["x".repeat(129)], PathReasonChange::Preserve).is_err());
    }
    #[tokio::test]
    async fn both_domains_retain_markers_and_reject_old_lifetime_failures()
    -> std::result::Result<(), crate::db::Error> {
        let (_scratch, db, c) = fixture().await?;
        for media in [MediaDomain::Tv, MediaDomain::Movies] {
            let initial = receipt(&c, media).await?;
            retry(&db, &initial).await?;
            let first = admitted(&c, initial.receipt_id).await?;
            assert_eq!(first.reasons, vec![PATH_MARKER]);
            assert_eq!(first.total_preflight_attempts, 2);
            assert!(first.path_failure_code.is_none());
            persist_failure(
                &c,
                &first,
                "source_unavailable",
                vec!["source_unavailable".into()],
                Some("hc021_source_missing_prepare"),
                false,
                None,
            )
            .await?;
            let failed = read(&c, first.receipt_id).await?;
            assert_eq!(failed.reasons[0], "hc021_source_missing_prepare");
            retry(&db, &failed).await?;
            let second = admitted(&c, failed.receipt_id).await?;
            assert_eq!(second.total_preflight_attempts, 3);
            assert_eq!(second.reasons, vec!["hc021_source_missing_prepare"]);
            let before = generation(&c, media).await?;
            persist_failure(
                &c,
                &first,
                "source_unavailable",
                vec![],
                Some("hc021_root_denied_prepare"),
                false,
                None,
            )
            .await?;
            assert_eq!(read(&c, second.receipt_id).await?.reasons, second.reasons);
            assert_eq!(generation(&c, media).await?, before);
            let _ = cancel(
                State(db.clone()),
                Path(second.receipt_id.to_string()),
                Ok(Query(Empty {})),
            )
            .await?;
            let cancelled = read(&c, second.receipt_id).await?;
            assert!(matches!(cancelled.status, ProcessingStatus::Cancelled));
            assert_eq!(cancelled.reasons, second.reasons);
            let before = generation(&c, media).await?;
            persist_failure(
                &c,
                &second,
                "source_unavailable",
                vec![],
                Some("hc021_destination_denied_prepare"),
                false,
                None,
            )
            .await?;
            assert_eq!(generation(&c, media).await?, before);
            retry(&db, &cancelled).await?;
            let resumed = admitted(&c, cancelled.receipt_id).await?;
            assert_eq!(resumed.total_preflight_attempts, 4);
            assert_eq!(resumed.reasons, second.reasons);
        }
        Ok(())
    }
    #[tokio::test]
    async fn storage_failure_rolls_back_admission_and_failure_keeps_durable_marker()
    -> std::result::Result<(), crate::db::Error> {
        let (_scratch, db, c) = fixture().await?;
        let item = receipt(&c, MediaDomain::Tv).await?;
        retry(&db, &item).await?;
        c.execute_batch("CREATE TRIGGER reject_path_health BEFORE UPDATE ON health_checks WHEN OLD.scope='tv' AND OLD.check_key='remote_path_mapping' BEGIN SELECT RAISE(ABORT,'fixture generation write failure'); END;").await?;
        assert!(admitted(&c, item.receipt_id).await.is_err());
        let untouched = read(&c, item.receipt_id).await?;
        assert!(matches!(untouched.status, ProcessingStatus::Queued));
        assert_eq!(untouched.total_preflight_attempts, 1);
        assert!(untouched.reasons.is_empty());
        c.execute("DROP TRIGGER reject_path_health", ()).await?;
        let admitted = admitted(&c, item.receipt_id).await?;
        c.execute_batch("CREATE TRIGGER reject_path_health BEFORE UPDATE ON health_checks WHEN OLD.scope='tv' AND OLD.check_key='remote_path_mapping' BEGIN SELECT RAISE(ABORT,'fixture generation write failure'); END;").await?;
        assert!(
            persist_failure(
                &c,
                &admitted,
                "source_unavailable",
                vec![],
                Some("hc021_source_denied_prepare"),
                false,
                None
            )
            .await
            .is_err()
        );
        let retained = read(&c, item.receipt_id).await?;
        assert!(matches!(retained.status, ProcessingStatus::Checking));
        assert_eq!(retained.reasons, vec![PATH_MARKER]);
        assert!(retained.error_code.is_none());
        c.execute("DROP TRIGGER reject_path_health", ()).await?;
        recover(&c, now()?, "interrupted").await?;
        assert_eq!(read(&c, item.receipt_id).await?.reasons[0], PATH_MARKER);
        Ok(())
    }
    #[tokio::test]
    async fn captured_mapping_change_wins_over_obsolete_source_failure()
    -> std::result::Result<(), crate::db::Error> {
        let (_scratch, db, c) = fixture().await?;
        let item = receipt(&c, MediaDomain::Tv).await?;
        retry(&db, &item).await?;
        let item = admitted(&c, item.receipt_id).await?;
        let mapping = MappingFence {
            id: 1,
            revision: 1,
            host: "fixture.invalid".into(),
        };
        c.execute(
            "UPDATE remote_path_mappings SET revision=2,local_path='/source/repaired' WHERE id=1",
            (),
        )
        .await?;
        persist_failure(
            &c,
            &item,
            "source_unavailable",
            vec!["source_unavailable".into()],
            Some("hc021_source_missing_prepare"),
            false,
            Some(&mapping),
        )
        .await?;
        let current = read(&c, item.receipt_id).await?;
        assert_eq!(current.error_code.as_deref(), Some("path_mapping_changed"));
        assert_eq!(current.reasons[0], "hc021_mapping_changed");
        Ok(())
    }
    async fn linked(c: &Connection, item: &DownloadProcessing) -> Result<String> {
        let operation = Uuid::new_v4().to_string();
        c.execute("INSERT INTO operations(id,media_type,movie_id,source,mode,destination,status,message)VALUES(?,'movie',1,'/source/new','copy','/movies/one/new.mkv','preview','fixture')",[operation.clone()]).await?;
        c.execute("INSERT INTO import_journal(operation_id,plan_json,phase,path_failure_code)VALUES(?,'{}','preview','unresolved_attempt')",[operation.clone()]).await?;
        c.execute("INSERT INTO rss_candidate_imports(candidate_id,operation_id,quality_id,revision_json,provenance_json)VALUES(?,?,1,'{\"version\":1,\"real\":0,\"is_repack\":false}','{}')",params![item.receipt_id.to_string(),operation.clone()]).await?;
        c.execute(
            "UPDATE download_processing SET status='importing' WHERE candidate_id=?",
            [item.receipt_id.to_string()],
        )
        .await?;
        Ok(operation)
    }
    #[tokio::test]
    async fn linked_resume_reservation_rolls_back_mixed_batch_and_complete_clears_only_its_receipt()
    -> std::result::Result<(), crate::db::Error> {
        let (_scratch, db, c) = fixture().await?;
        let movie = receipt(&c, MediaDomain::Movies).await?;
        retry(&db, &movie).await?;
        let movie = admitted(&c, movie.receipt_id).await?;
        let operation = linked(&c, &movie).await?;
        let linked = read(&c, movie.receipt_id).await?;
        assert_eq!(
            linked.path_failure_code.as_deref(),
            Some("unresolved_attempt")
        );
        assert!(linked.recovery_required);
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        let settings = crate::completed_download_handling::read(&tx, MediaDomain::Movies)
            .await
            .map_err(|e| e.1.to_owned())?;
        crate::completed_download_handling::set(
            &tx,
            MediaDomain::Movies,
            false,
            true,
            settings.revision,
            true,
        )
        .await
        .map_err(|e| e.1.to_owned())?;
        tx.commit().await?;
        assert!(
            !policy(&c, linked.provider_id, MediaDomain::Movies)
                .await?
                .enabled
        );
        // Disabling new admission cannot revoke the existing operation's explicit recovery.
        let guard = crate::import::acquire_resume_guard(&db).map_err(|e| e.code().to_owned())?;
        assert!(retry(&db, &linked).await.is_err());
        assert!(!read(&c, movie.receipt_id).await?.resume_requested);
        drop(guard);
        let result = process(
            State(db.clone()),
            Ok(Query(Empty {})),
            Ok(Json(ProcessingInput {
                provider_id: linked.provider_id,
                provider_revision: linked.provider_revision,
                media_type: MediaDomain::Movies,
                receipt_ids: vec![linked.receipt_id, Uuid::new_v4()],
            })),
        )
        .await;
        assert!(result.is_err());
        assert!(!read(&c, movie.receipt_id).await?.resume_requested);
        // The failed batch released its reservation, allowing exactly one explicit request.
        retry(&db, &linked).await?;
        assert!(read(&c, movie.receipt_id).await?.resume_requested);
        let tv = receipt(&c, MediaDomain::Tv).await?;
        retry(&db, &tv).await?;
        let tv = admitted(&c, tv.receipt_id).await?;
        c.execute(
            "INSERT INTO movie_files(id,movie_id,path)VALUES(1,1,'/movies/one/new.mkv')",
            (),
        )
        .await?;
        c.execute(
            "UPDATE import_journal SET phase='staging',stage_json='{}' WHERE operation_id=?",
            [operation.clone()],
        )
        .await?;
        c.execute("INSERT INTO import_history(operation_id,media_type,movie_id,movie_file_id,source,destination,size,sha256)VALUES(?,'movie',1,1,'/source/new','/movies/one/new.mkv',1,?)",params![operation.clone(),"a".repeat(64)]).await?;
        c.execute("UPDATE import_journal SET phase='complete',path_failure_code=NULL WHERE operation_id=?",[operation]).await?;
        advance_import(&db, &c, linked).await?;
        let completed = read(&c, movie.receipt_id).await?;
        assert!(matches!(completed.status, ProcessingStatus::Imported));
        assert!(completed.reasons.is_empty());
        assert!(!completed.recovery_required);
        assert_eq!(read(&c, tv.receipt_id).await?.reasons, vec![PATH_MARKER]);
        Ok(())
    }
}
