//! Completed owned downloads use one immutable import operation per submission receipt.
use super::*;
use crate::search::ReleaseTarget;
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
#[derive(Serialize, ts_rs::TS)]
pub struct ProcessingPolicy {
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
    pub recovery_bytes_retained: bool,
    pub error_code: Option<String>,
    pub reasons: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
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
const COLUMNS: &str = "d.candidate_id,r.media_type,r.series_id,r.movie_id,e.episode_id,r.client_id,r.client_revision,r.observed_hash,d.policy_revision,d.status,d.preflight_attempts,d.total_preflight_attempts,d.next_attempt_at,i.operation_id,j.phase,d.error_code,d.reasons_json,d.created_at,d.updated_at,d.resume_requested,CASE WHEN i.old_file_json IS NOT NULL THEN i.retirement_state ELSE NULL END";
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
        operation_id: r
            .get::<Option<String>>(13)?
            .map(|s| Uuid::parse_str(&s).map_err(|_| bad()))
            .transpose()?,
        import_phase: r.get(14)?,
        error_code: r.get(15)?,
        reasons: serde_json::from_str(&r.get::<String>(16)?).map_err(|_| bad())?,
        created_at: r.get(17)?,
        updated_at: r.get(18)?,
        resume_requested: r.get::<i64>(19)? == 1,
        retirement_state: r.get(20)?,
        recovery_bytes_retained: r
            .get::<Option<String>>(20)?
            .is_some_and(|v| v == "quarantined" || v == "shared_retained"),
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
    let provider=c.query("SELECT p.revision FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=? AND p.implementation='qbittorrent' AND s.media_type=?",params![id.to_string(),domain(media)]).await?.next().await?.ok_or(Error(StatusCode::NOT_FOUND,"provider_not_found"))?;
    let revision: i64 = provider.get(0)?;
    let stored=c.query("SELECT provider_revision,revision,enabled,mode FROM download_processing_policies WHERE provider_id=? AND media_type=?",params![id.to_string(),domain(media)]).await?.next().await?;
    if let Some(r) = stored {
        Ok(ProcessingPolicy {
            provider_id: id,
            media_type: media,
            provider_revision: r.get(0)?,
            revision: Some(r.get(1)?),
            enabled: r.get::<i64>(2)? == 1,
            mode: ProcessingMode::parse(&r.get::<String>(3)?)?,
        })
    } else {
        Ok(ProcessingPolicy {
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
    bounded(
        policy(
            &connection(&db).await?,
            Uuid::parse_str(&id).map_err(|_| bad())?,
            MediaDomain::parse(&m).map_err(|_| bad())?,
        )
        .await?,
    )
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
    positive(input.provider_revision)?;
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome=async{
        let current=policy(&tx,id,m).await?;
        if current.revision!=input.revision||input.revision.is_some_and(|v|v>=MAX_REVISION){return Err(conflict())}
        if tx.query("SELECT 1 FROM providers WHERE id=? AND revision=? AND (?=0 OR enabled=1)",params![id.to_string(),input.provider_revision,i64::from(input.enabled)]).await?.next().await?.is_none(){return Err(Error(StatusCode::CONFLICT,"provider_changed"))}
        tx.execute("INSERT INTO download_processing_policies(provider_id,media_type,provider_revision,revision,enabled,mode)VALUES(?,?,?,1,?,?) ON CONFLICT(provider_id,media_type)DO UPDATE SET provider_revision=excluded.provider_revision,revision=download_processing_policies.revision+1,enabled=excluded.enabled,mode=excluded.mode",params![id.to_string(),domain(m),input.provider_revision,i64::from(input.enabled),input.mode.text()]).await?;
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
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome=async{
        let timestamp=now()?;let mut rows=Vec::new();
        for id in input.receipt_ids {
            let valid=tx.query("SELECT 1 FROM rss_candidates WHERE id=? AND status='observed' AND client_id=? AND client_revision=? AND media_type=?",params![id.to_string(),input.provider_id.to_string(),input.provider_revision,domain(input.media_type)]).await?.next().await?.is_some();
            if !valid{return Err(Error(StatusCode::CONFLICT,"invalid_processing_receipt"))}
            let existing=match read(&tx,id).await{Ok(row)=>Some(row),Err(Error(StatusCode::NOT_FOUND,"processing_not_found"))=>None,Err(e)=>return Err(e)};
            if existing.as_ref().is_some_and(|r|r.operation_id.is_some()){
                if existing.as_ref().is_some_and(|r|matches!(r.status,ProcessingStatus::Importing)) && tx.query("SELECT 1 FROM download_processing d JOIN rss_candidate_imports i ON i.candidate_id=d.candidate_id JOIN import_journal j ON j.operation_id=i.operation_id WHERE d.candidate_id=? AND (d.error_code IS NOT NULL OR j.error_code IS NOT NULL)",[id.to_string()]).await?.next().await?.is_some(){tx.execute("UPDATE download_processing SET resume_requested=1,next_attempt_at=?,updated_at=? WHERE candidate_id=?",params![timestamp,timestamp,id.to_string()]).await?;}
            }else{
                let p=policy(&tx,input.provider_id,input.media_type).await?;
                if !p.enabled||p.provider_revision!=input.provider_revision{return Err(Error(StatusCode::CONFLICT,"processing_disabled"))}
                let revision=p.revision.ok_or_else(bad)?;
                if let Some(existing)=existing {
                    if matches!(existing.status,ProcessingStatus::Blocked|ProcessingStatus::Cancelled){tx.execute("UPDATE download_processing SET status='queued',policy_revision=?,preflight_attempts=0,error_code=NULL,reasons_json='[]',next_attempt_at=?,updated_at=? WHERE candidate_id=?",params![revision,timestamp,timestamp,id.to_string()]).await?;}
                }else{tx.execute("INSERT INTO download_processing(candidate_id,policy_revision,status,next_attempt_at,created_at,updated_at)VALUES(?,?,'queued',?,?,?)",params![id.to_string(),revision,timestamp,timestamp,timestamp]).await?;}
            }
            rows.push(read(&tx,id).await?);
        }
        bounded(rows)
    }.await;
    Ok((StatusCode::ACCEPTED, finish(tx, outcome).await?))
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
    let outcome=async{if read(&tx,id).await?.operation_id.is_some(){return Err(conflict())}tx.execute("UPDATE download_processing SET status='cancelled',error_code=NULL,reasons_json='[]',updated_at=? WHERE candidate_id=?",params![now()?,id.to_string()]).await?;bounded(read(&tx,id).await?)}.await;
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
        c.execute("INSERT INTO download_processing(candidate_id,policy_revision,status,next_attempt_at,created_at,updated_at) SELECT r.id,p.revision,'queued',?,?,? FROM rss_candidates r JOIN download_processing_policies p ON p.provider_id=r.client_id AND p.media_type=r.media_type WHERE r.status='observed' AND r.client_id=? AND r.client_revision=? AND r.media_type=? AND r.observed_hash=? AND p.enabled=1 AND p.provider_revision=r.client_revision AND NOT EXISTS(SELECT 1 FROM download_processing d WHERE d.candidate_id=r.id)",params![timestamp,timestamp,timestamp,scope.provider_id.to_string(),revision,domain(scope.media_type),item.hash.clone()]).await?;
    }
    Ok(())
}
pub(super) async fn recover(c: &Connection, timestamp: i64, code: &str) -> Result<()> {
    // Only preflight reads repeat automatically. A linked transfer has its own durable authority.
    let mut rows = c
        .query(
            "SELECT candidate_id FROM download_processing WHERE status='checking'",
            (),
        )
        .await?;
    let mut ids = Vec::new();
    while let Some(r) = rows.next().await? {
        ids.push(Uuid::parse_str(&r.get::<String>(0)?).map_err(|_| bad())?)
    }
    drop(rows);
    for id in ids {
        let item = read(c, id).await?;
        preflight_failure(c, &item, code, vec![code.into()]).await?;
    }
    c.execute("UPDATE download_processing SET status='blocked',error_code='processing_disabled',reasons_json='[]',updated_at=? WHERE status='queued' AND NOT EXISTS(SELECT 1 FROM rss_candidates r JOIN download_processing_policies p ON p.provider_id=r.client_id AND p.media_type=r.media_type JOIN providers v ON v.id=p.provider_id JOIN provider_scopes s ON s.provider_id=v.id AND s.media_type=p.media_type WHERE r.id=download_processing.candidate_id AND p.enabled=1 AND p.revision=download_processing.policy_revision AND p.provider_revision=r.client_revision AND v.revision=r.client_revision AND v.enabled=1)",[timestamp]).await?;
    Ok(())
}
pub(super) async fn claim(c: &Connection, id: Uuid, timestamp: i64) -> Result<DownloadProcessing> {
    c.execute("UPDATE download_processing SET status='checking',preflight_attempts=preflight_attempts+1,total_preflight_attempts=total_preflight_attempts+1,error_code=NULL,reasons_json='[]',updated_at=? WHERE candidate_id=? AND status='queued'",params![timestamp,id.to_string()]).await?;
    read(c, id).await
}
async fn blocked(c: &Connection, id: Uuid, code: &str, reasons: Vec<String>) -> Result<()> {
    let reasons: Vec<_> = reasons.into_iter().take(64).collect();
    c.execute("UPDATE download_processing SET status='blocked',error_code=?,reasons_json=?,updated_at=? WHERE candidate_id=? AND status='checking'",params![code,serde_json::to_string(&reasons).map_err(|_|bad())?,now()?,id.to_string()]).await?;
    Ok(())
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
async fn preflight_failure(
    c: &Connection,
    item: &DownloadProcessing,
    code: &str,
    reasons: Vec<String>,
) -> Result<()> {
    let code = match code {
        "command_storage_error" | "invalid_command_request" => "storage_error",
        other => other,
    };
    let retry = matches!(
        code,
        "interrupted" | "storage_error" | "download_timeout" | "download_unavailable"
    ) && item.preflight_attempts < 3;
    if retry && authorization(c, item).await.is_ok() {
        c.execute("UPDATE download_processing SET status='queued',error_code=?,reasons_json=?,next_attempt_at=?,updated_at=? WHERE candidate_id=? AND status='checking'",params![code,serde_json::to_string(&reasons).map_err(|_|bad())?,now()?+(1i64<<item.preflight_attempts),now()?,item.receipt_id.to_string()]).await?;
    } else {
        blocked(c, item.receipt_id, code, reasons).await?;
    }
    Ok(())
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
    let outcome = preflight(db, client, &item).await;
    if let Err(Error(_, code)) = outcome {
        let current = read(&c, item.receipt_id).await?;
        if current.operation_id.is_some() {
            return Ok(());
        }
        preflight_failure(&c, &current, code, vec![code.into()]).await?;
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
    let observed = client
        .inspect_download(
            &item.provider_id.to_string(),
            item.provider_revision,
            &target,
            &item.remote_id,
        )
        .await
        .map_err(|e| {
            Error(
                StatusCode::CONFLICT,
                match e.code {
                    "provider_changed" => "provider_changed",
                    "refresh_timeout" => "download_timeout",
                    _ => "download_unavailable",
                },
            )
        })?;
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
        let result = crate::search::downloaded::evaluate(&c, &target, &file.name, file.size_bytes)
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
            let mapping_id = mapped
                .mapping_id
                .ok_or(Error(StatusCode::CONFLICT, "path_mapping_missing"))?;
            let mapping_revision = mapped
                .mapping_revision
                .ok_or(Error(StatusCode::CONFLICT, "path_mapping_missing"))?;
            let destination = std::path::Path::new(&facts.root)
                .join(&facts.basename)
                .to_str()
                .ok_or_else(bad)?
                .to_string();
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
                mapping_id,
                mapping_revision,
                host: observed.host.clone(),
            });
        } else {
            reasons.extend(
                result
                    .reasons
                    .into_iter()
                    .take(64usize.saturating_sub(reasons.len())),
            );
        }
    }
    let Some(input) = accepted else {
        blocked(&c, item.receipt_id, "quality_rejected", reasons).await?;
        return Ok(());
    };
    // prepare_owned atomically rechecks policy, mapping, target, filename/profile and cancellation.
    crate::import::prepare_owned(db.clone(), input)
        .await
        .map_err(|e| Error(StatusCode::CONFLICT, processing_error(e.code())))?;
    Ok(())
}
async fn advance_import(
    db: &Arc<Database>,
    c: &Connection,
    item: DownloadProcessing,
) -> Result<()> {
    let operation = item.operation_id.ok_or_else(bad)?.to_string();
    let journal = c
        .query(
            "SELECT phase,error_code FROM import_journal WHERE operation_id=?",
            [operation.clone()],
        )
        .await?
        .next()
        .await?
        .ok_or_else(bad)?;
    let phase: String = journal.get(0)?;
    let error: Option<String> = journal.get(1)?;
    drop(journal);
    if phase == "complete" {
        c.execute("UPDATE download_processing SET status='imported',resume_requested=0,error_code=NULL,reasons_json='[]',updated_at=? WHERE candidate_id=?",params![now()?,item.receipt_id.to_string()]).await?;
        return Ok(());
    }
    let resume = c
        .query(
            "SELECT resume_requested FROM download_processing WHERE candidate_id=?",
            [item.receipt_id.to_string()],
        )
        .await?
        .next()
        .await?
        .ok_or_else(bad)?
        .get::<i64>(0)?
        == 1;
    if item.error_code.is_some() && !resume {
        return Ok(());
    }
    if error.is_some() && !resume {
        c.execute("UPDATE download_processing SET resume_requested=0,error_code='import_failed',reasons_json='[\"import_failed\"]',updated_at=? WHERE candidate_id=?",params![now()?,item.receipt_id.to_string()]).await?;
        return Ok(());
    }
    match crate::import::start_owned(db.clone(), &operation).await {
        Ok(_) => {
            c.execute("UPDATE download_processing SET next_attempt_at=?,updated_at=? WHERE candidate_id=?",params![now()?+2,now()?,item.receipt_id.to_string()]).await?;
        }
        Err(error) => {
            eprintln!(
                "event=owned_import_start_error receipt_id={} code={}",
                item.receipt_id,
                error.code()
            );
            c.execute("UPDATE download_processing SET resume_requested=0,error_code='import_failed',reasons_json='[\"import_failed\"]',updated_at=? WHERE candidate_id=?",params![now()?,item.receipt_id.to_string()]).await?;
        }
    }
    Ok(())
}
