//! Durable RSS decisions and guarded qBittorrent submission receipts.
#[path = "rss_pending.rs"]
mod pending;
use super::*;
use crate::providers::{IndexerOperation, RefreshClient, indexer, qbittorrent};
use crate::search::{Disposition, ReleaseTarget, SearchContext};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum SelectionKind {
    LegacyFixed,
    Default,
    Explicit,
}
impl SelectionKind {
    pub(super) fn text(self) -> &'static str {
        match self {
            Self::LegacyFixed => "legacy_fixed",
            Self::Default => "default",
            Self::Explicit => "explicit",
        }
    }
    pub(super) fn parse(value: &str) -> Result<Self> {
        match value {
            "legacy_fixed" => Ok(Self::LegacyFixed),
            "default" => Ok(Self::Default),
            "explicit" => Ok(Self::Explicit),
            _ => Err(bad()),
        }
    }
}
#[derive(Clone, Copy, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct RssTargetInput {
    pub media_type: MediaDomain,
    pub indexer_id: Uuid,
    pub indexer_revision: i64,
    #[serde(default, deserialize_with = "super::present")]
    #[ts(optional)]
    pub client_selection: Option<crate::providers::ClientSelection>,
    #[serde(default, deserialize_with = "super::present")]
    #[ts(optional)]
    pub client_id: Option<Uuid>,
    #[serde(default, deserialize_with = "super::present")]
    #[ts(optional)]
    pub client_revision: Option<i64>,
}
impl RssTargetInput {
    pub(super) fn normalize(self) -> Result<RssTarget> {
        let (selection_kind, client_id, client_revision) =
            match (self.client_selection, self.client_id, self.client_revision) {
                (None, Some(id), Some(revision)) => {
                    (SelectionKind::LegacyFixed, Some(id), Some(revision))
                }
                (Some(crate::providers::ClientSelection::Default {}), None, None) => {
                    (SelectionKind::Default, None, None)
                }
                (
                    Some(crate::providers::ClientSelection::Explicit {
                        provider_id,
                        provider_revision,
                    }),
                    None,
                    None,
                ) => (
                    SelectionKind::Explicit,
                    Some(provider_id),
                    Some(provider_revision),
                ),
                _ => return Err(bad()),
            };
        if !(1..=MAX_REVISION).contains(&self.indexer_revision)
            || client_revision.is_some_and(|v| !(1..=MAX_REVISION).contains(&v))
        {
            return Err(bad());
        }
        Ok(RssTarget {
            media_type: self.media_type,
            indexer_id: self.indexer_id,
            indexer_revision: self.indexer_revision,
            selection_kind,
            client_id,
            client_revision,
        })
    }
}
#[derive(Clone, Copy, Serialize, ts_rs::TS)]
pub struct RssTarget {
    pub media_type: MediaDomain,
    pub indexer_id: Uuid,
    pub indexer_revision: i64,
    pub selection_kind: SelectionKind,
    pub client_id: Option<Uuid>,
    pub client_revision: Option<i64>,
}
impl RssTarget {
    fn resolved(self) -> Result<(Uuid, i64)> {
        self.client_id.zip(self.client_revision).ok_or_else(bad)
    }
}
#[derive(Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ClientAttemptState {
    Selected,
    Prepared,
    Dispatching,
    DefinitelyNotSubmitted,
    Observed,
    Unknown,
}
#[derive(Deserialize, Serialize, ts_rs::TS)]
pub struct ClientAttempt {
    pub ordinal: u16,
    pub selection_round: u16,
    pub client_id: Uuid,
    pub client_revision: i64,
    pub state: ClientAttemptState,
    pub error_code: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct RssInput {
    pub target: RssTargetInput,
    pub priority: CommandPriority,
}
#[derive(Serialize, ts_rs::TS)]
pub struct RssCommand {
    pub id: Uuid,
    pub target: RssTarget,
    pub priority: CommandPriority,
    pub status: CommandStatus,
    pub attempts: u8,
    pub next_attempt_at: i64,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub error_code: Option<String>,
    pub fetched: u32,
    pub evaluated: u32,
    pub rejected: u32,
    pub pending: u32,
    pub observed: u32,
    pub uncertain: u32,
    pub fetch_complete: bool,
}
#[derive(Serialize, ts_rs::TS)]
pub struct RssCandidate {
    pub client_attempts: Vec<ClientAttempt>,
    pub origin: super::search::CandidateOrigin,
    pub id: Uuid,
    pub command_id: Option<Uuid>,
    pub source: RssTarget,
    pub title: String,
    pub target: Option<ReleaseTarget>,
    pub status: String,
    pub reasons: Vec<String>,
    pub not_before: Option<i64>,
    pub attempts: u8,
    pub created_at: i64,
    pub updated_at: i64,
    pub error_code: Option<String>,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct RssScheduleInput {
    #[ts(optional)]
    pub revision: Option<i64>,
    pub target: RssTargetInput,
    pub interval_seconds: u32,
    pub enabled: bool,
}
#[derive(Serialize, ts_rs::TS)]
pub struct RssSchedule {
    pub id: Uuid,
    pub target: RssTarget,
    pub revision: i64,
    pub interval_seconds: u32,
    pub enabled: bool,
    pub next_run_at: i64,
    pub last_run_at: Option<i64>,
    pub error_code: Option<String>,
    pub created_at: i64,
}
const COLUMNS: &str = "id,media_type,indexer_id,indexer_revision,client_id,client_revision,priority,status,attempts,next_attempt_at,created_at,started_at,completed_at,error_code,fetched,evaluated,(SELECT count(*) FROM rss_candidates rc WHERE rc.command_id=rss_commands.id AND rc.status IN ('rejected','cancelled')),(SELECT count(*) FROM rss_candidates rc WHERE rc.command_id=rss_commands.id AND rc.status='pending'),(SELECT count(*) FROM rss_candidates rc WHERE rc.command_id=rss_commands.id AND rc.status='observed'),(SELECT count(*) FROM rss_candidates rc WHERE rc.command_id=rss_commands.id AND rc.status IN ('submitting','reconciling','needs_attention')),fetch_complete,selection_kind";
fn target_row(r: &libsql::Row, start: i32, kind: i32) -> Result<RssTarget> {
    Ok(RssTarget {
        media_type: MediaDomain::parse(&r.get::<String>(start)?).map_err(|_| bad())?,
        indexer_id: Uuid::parse_str(&r.get::<String>(start + 1)?).map_err(|_| bad())?,
        indexer_revision: r.get(start + 2)?,
        selection_kind: SelectionKind::parse(&r.get::<String>(kind)?)?,
        client_id: r
            .get::<Option<String>>(start + 3)?
            .map(|v| Uuid::parse_str(&v).map_err(|_| bad()))
            .transpose()?,
        client_revision: r.get(start + 4)?,
    })
}
fn row(r: libsql::Row) -> Result<RssCommand> {
    Ok(RssCommand {
        id: Uuid::parse_str(&r.get::<String>(0)?).map_err(|_| bad())?,
        target: target_row(&r, 1, 21)?,
        priority: if r.get::<i64>(6)? == 1 {
            CommandPriority::High
        } else {
            CommandPriority::Normal
        },
        status: CommandStatus::parse(&r.get::<String>(7)?)?,
        attempts: r.get::<i64>(8)? as u8,
        next_attempt_at: r.get(9)?,
        created_at: r.get(10)?,
        started_at: r.get(11)?,
        completed_at: r.get(12)?,
        error_code: r.get(13)?,
        fetched: r.get::<i64>(14)? as u32,
        evaluated: r.get::<i64>(15)? as u32,
        rejected: r.get::<i64>(16)? as u32,
        pending: r.get::<i64>(17)? as u32,
        observed: r.get::<i64>(18)? as u32,
        uncertain: r.get::<i64>(19)? as u32,
        fetch_complete: r.get::<i64>(20)? == 1,
    })
}
async fn read(c: &Connection, id: Uuid) -> Result<RssCommand> {
    row(c
        .query(
            &format!("SELECT {COLUMNS} FROM rss_commands WHERE id=?"),
            [id.to_string()],
        )
        .await?
        .next()
        .await?
        .ok_or(Error(StatusCode::NOT_FOUND, "rss_command_not_found"))?)
}
/// A fixed or explicit client must accept its indexer's release protocol. Default selection
/// filters by protocol instead. This is a visible admission rejection, not an empty target.
pub(super) async fn protocol_mismatch(c: &Connection, t: RssTarget) -> Result<bool> {
    let Some(client) = t.client_id else {
        return Ok(false);
    };
    let row = c.query("SELECT i.protocol,k.protocol FROM providers a JOIN provider_implementations i ON i.implementation=a.implementation, providers b JOIN provider_implementations k ON k.implementation=b.implementation WHERE a.id=? AND b.id=? AND i.kind='indexer' AND k.kind='download_client'",params![t.indexer_id.to_string(),client.to_string()]).await?.next().await?;
    Ok(match row {
        Some(row) => row.get::<String>(0)? != row.get::<String>(1)?,
        None => false,
    })
}
pub(super) fn protocol_rejection() -> Error {
    Error(StatusCode::CONFLICT, "protocol_mismatch")
}
pub(super) async fn valid_target(
    c: &Connection,
    t: RssTarget,
    operation: IndexerOperation,
) -> Result<bool> {
    if !(1..=MAX_REVISION).contains(&t.indexer_revision) {
        return Ok(false);
    }
    let flag = match operation {
        IndexerOperation::Rss => "enable_rss",
        IndexerOperation::Automatic => "enable_automatic_search",
        IndexerOperation::Interactive => "enable_interactive_search",
    };
    let indexer_valid = c.query(&format!("SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE s.{flag}=1 AND p.id=? AND p.revision=? AND p.enabled=1 AND p.implementation IN ('torznab','newznab','torrentrss') AND s.media_type=? AND (?!='legacy_fixed' OR s.download_client_id IS NULL OR s.download_client_id=?)"),params![t.indexer_id.to_string(),t.indexer_revision,domain(t.media_type),t.selection_kind.text(),t.client_id.map(|v|v.to_string())]).await?.next().await?.is_some();
    if !indexer_valid {
        return Ok(false);
    }
    if matches!(t.selection_kind, SelectionKind::Default) {
        return Ok(true);
    }
    let (id, revision) = t.resolved()?;
    if protocol_mismatch(c, t).await? {
        return Ok(false);
    }
    valid_provider(
        c,
        RefreshTarget {
            provider_id: id,
            media_type: t.media_type,
        },
        revision,
    )
    .await
}
pub(super) fn router(db: Arc<Database>) -> Router {
    Router::new()
        .route("/api/v1/rss/commands", get(list).post(create))
        .route("/api/v1/rss/commands/{id}", get(detail).delete(delete))
        .route("/api/v1/rss/commands/{id}/cancel", post(cancel))
        .route("/api/v1/rss/candidates", get(candidates))
        .route(
            "/api/v1/rss/candidates/{id}",
            axum::routing::delete(delete_candidate),
        )
        .route("/api/v1/rss/schedules", get(schedules).post(save_schedule))
        .route(
            "/api/v1/rss/schedules/{id}",
            axum::routing::delete(delete_schedule),
        )
        .layer(DefaultBodyLimit::max(8192))
        .layer(axum::middleware::from_fn(deadline))
        .with_state(db)
}
async fn deadline(request: axum::extract::Request, next: axum::middleware::Next) -> Response {
    match tokio::time::timeout(std::time::Duration::from_secs(5), next.run(request)).await {
        Ok(response) => response,
        Err(_) => Error(StatusCode::SERVICE_UNAVAILABLE, "rss_timeout").into_response(),
    }
}
async fn enqueue(c: &Connection, input: RssInput, timestamp: i64) -> Result<RssCommand> {
    enqueue_target(c, input.target.normalize()?, input.priority, timestamp).await
}
async fn enqueue_target(
    c: &Connection,
    t: RssTarget,
    priority: CommandPriority,
    timestamp: i64,
) -> Result<RssCommand> {
    if protocol_mismatch(c, t).await? {
        return Err(protocol_rejection());
    }
    if !valid_target(c, t, IndexerOperation::Rss).await? {
        return Err(Error(StatusCode::CONFLICT, "provider_changed"));
    }
    if let Some(r)=c.query(&format!("SELECT {COLUMNS} FROM rss_commands WHERE indexer_id=? AND client_id IS ? AND selection_kind=? AND media_type=? AND status IN ('queued','running','retry_wait')"),params![t.indexer_id.to_string(),t.client_id.map(|v|v.to_string()),t.selection_kind.text(),domain(t.media_type)]).await?.next().await?{return row(r)}
    if command_capacity(c).await? >= MAX_COMMANDS {
        return Err(Error(StatusCode::TOO_MANY_REQUESTS, "command_history_full"));
    }
    let id = Uuid::new_v4();
    c.execute("INSERT INTO rss_commands(id,name,media_type,indexer_id,indexer_revision,client_id,client_revision,priority,status,attempts,next_attempt_at,created_at,selection_kind)VALUES(?,'rss_sync',?,?,?,?,?,?,'queued',0,?,?,?)",params![id.to_string(),domain(t.media_type),t.indexer_id.to_string(),t.indexer_revision,t.client_id.map(|v|v.to_string()),t.client_revision,priority.number(),timestamp,timestamp,t.selection_kind.text()]).await?;
    read(c, id).await
}
async fn create(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
    input: std::result::Result<Json<RssInput>, JsonRejection>,
) -> Result<(StatusCode, Json<RssCommand>)> {
    q.map_err(|_| bad())?;
    let input = input.map_err(|_| bad())?.0;
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = async { bounded(enqueue(&tx, input, now()?).await?) }.await;
    Ok((StatusCode::ACCEPTED, finish(tx, outcome).await?))
}
async fn list(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<CommandQuery>, QueryRejection>,
) -> Result<Json<ApiPage<RssCommand>>> {
    let q = q.map_err(|_| bad())?.0;
    if q.limit == 0 || q.limit > 100 || q.offset > 10000 {
        return Err(bad());
    }
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::ReadOnly)
        .await?;
    let media = q.media_type.map(domain);
    let status = q.status.map(CommandStatus::text);
    let outcome=async{
        let filter="(? IS NULL OR media_type=?) AND (? IS NULL OR status=?)";
        let total=tx.query(&format!("SELECT count(*) FROM rss_commands WHERE {filter}"),params![media,media,status,status]).await?.next().await?.ok_or_else(bad)?.get::<i64>(0)?;
        let mut rows=tx.query(&format!("SELECT {COLUMNS} FROM rss_commands WHERE {filter} ORDER BY created_at DESC,id DESC LIMIT ? OFFSET ?"),params![media,media,status,status,i64::from(q.limit),i64::from(q.offset)]).await?;
        let mut items=Vec::new();while let Some(r)=rows.next().await?{items.push(row(r)?)}
        bounded(ApiPage{items,total,limit:q.limit,offset:q.offset})
    }.await;
    finish(tx, outcome).await
}
async fn detail(
    State(db): State<Arc<Database>>,
    Path(id): Path<String>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
) -> Result<Json<RssCommand>> {
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
) -> Result<Json<RssCommand>> {
    q.map_err(|_| bad())?;
    let id = Uuid::parse_str(&id).map_err(|_| bad())?;
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome=async{
        let timestamp=now()?;
        tx.execute("UPDATE rss_client_attempts SET state='definitely_not_submitted',error_code='cancelled',updated_at=max(updated_at,?) WHERE state IN ('selected','prepared') AND EXISTS(SELECT 1 FROM rss_candidates r WHERE r.id=rss_client_attempts.candidate_id AND r.command_id=? AND r.active_attempt=rss_client_attempts.ordinal AND r.status IN ('pending','prepared'))",params![timestamp,id.to_string()]).await?;
        tx.execute("UPDATE rss_candidates SET status='cancelled',private_payload=NULL,updated_at=?,not_before=NULL,error_code=NULL WHERE command_id=? AND status IN ('pending','prepared')",params![timestamp,id.to_string()]).await?;
        tx.execute("UPDATE rss_commands SET status='cancelled',completed_at=?,error_code=NULL WHERE id=? AND status IN ('queued','running','retry_wait')",params![timestamp,id.to_string()]).await?;
        bounded(read(&tx,id).await?)
    }.await;
    finish(tx, outcome).await
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
    let outcome = async {
        if matches!(
            read(&tx, id).await?.status,
            CommandStatus::Queued | CommandStatus::Running | CommandStatus::RetryWait
        ) {
            return Err(conflict());
        }
        tx.execute("DELETE FROM rss_commands WHERE id=?", [id.to_string()])
            .await?;
        Ok(StatusCode::NO_CONTENT)
    }
    .await;
    finish(tx, outcome).await
}
pub(super) fn payload_context(id: Uuid, t: RssTarget) -> String {
    match t.selection_kind {
        SelectionKind::LegacyFixed => format!(
            "{id}/{}/{}/{}/{}/{}",
            t.indexer_id,
            t.indexer_revision,
            t.client_id.map(|id| id.to_string()).unwrap_or_default(),
            t.client_revision.unwrap_or_default(),
            domain(t.media_type)
        ),
        _ => format!(
            "rss-release-v2/{id}/{}/{}/{}/{}",
            t.indexer_id,
            t.indexer_revision,
            domain(t.media_type),
            t.selection_kind.text()
        ),
    }
}
pub(super) fn digest(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|v| format!("{v:02x}"))
        .collect()
}
fn singleton(target: &Option<ReleaseTarget>) -> bool {
    matches!(target, Some(ReleaseTarget::Movies { .. }))
        || matches!(target,Some(ReleaseTarget::Tv{episode_ids,..}) if episode_ids.len()==1)
}
struct CapturedRelease {
    id: Uuid,
    fingerprint: String,
    title: String,
    release: indexer::Release,
}
async fn insert_captured(
    c: &Connection,
    client: &RefreshClient,
    command: &RssCommand,
    item: CapturedRelease,
    timestamp: i64,
    operation: &mut crate::release_profile_terms::OperationEvidence,
) -> Result<()> {
    if c.is_autocommit() {
        return Err(bad());
    }
    let t = command.target;
    let CapturedRelease {
        id,
        fingerprint,
        title,
        release,
    } = item;
    let mut decision = pending::evaluate(
        c,
        client,
        t.media_type,
        t.indexer_id,
        &release,
        SearchContext::Rss,
        timestamp,
        operation,
    )
    .await?;
    if !singleton(&decision.target) {
        decision.disposition = Disposition::Reject;
        decision.reasons.push("unsupported_target".into());
    }
    if matches!(decision.disposition, Disposition::Reject) {
        decision.target = None;
    }
    let payload = if matches!(decision.disposition, Disposition::Reject) {
        None
    } else {
        let mut bytes = indexer::encode_private(release)
            .map_err(|_| Error(StatusCode::BAD_GATEWAY, "invalid_release"))?;
        let sealed = client.seal_release(&payload_context(id, t), &bytes);
        bytes.fill(0);
        Some(sealed.map_err(|e| Error(StatusCode::SERVICE_UNAVAILABLE, e.code))?)
    };
    if let Some(existing)=c.query("SELECT id,status FROM rss_candidates WHERE indexer_id=? AND indexer_revision=? AND selection_kind=? AND (selection_kind='default' OR (client_id IS ? AND client_revision IS ?)) AND media_type=? AND fingerprint=?",params![t.indexer_id.to_string(),t.indexer_revision,t.selection_kind.text(),t.client_id.map(|v|v.to_string()),t.client_revision,domain(t.media_type),fingerprint.clone()]).await?.next().await?{
                let existing_id:String=existing.get(0)?;let status:String=existing.get(1)?;
                if matches!(status.as_str(),"rejected"|"cancelled") {c.execute("DELETE FROM rss_candidates WHERE id=?",[existing_id]).await?;}else{return Ok(())}
            }
    if c.query("SELECT count(*)>=1024 OR COALESCE(sum(length(private_payload)),0)+(SELECT COALESCE(sum(length(private_payload)),0) FROM search_results)+?>16777216 FROM rss_candidates",[payload.as_ref().map_or(0,Vec::len) as i64]).await?.next().await?.ok_or_else(bad)?.get::<i64>(0)?==1{return Err(Error(StatusCode::TOO_MANY_REQUESTS,"candidate_limit"))}
    let (series, movie, episode) = match decision.target {
        Some(ReleaseTarget::Tv {
            series_id,
            episode_ids,
        }) if episode_ids.len() == 1 => (Some(series_id), None, episode_ids.first().copied()),
        Some(ReleaseTarget::Movies { movie_id }) => (None, Some(movie_id), None),
        _ => (None, None, None),
    };
    let status = if matches!(decision.disposition, Disposition::Reject) {
        "rejected"
    } else {
        "pending"
    };
    let reasons = serde_json::to_string(&decision.reasons).map_err(|_| bad())?;
    c.execute("INSERT INTO rss_candidates(id,command_id,indexer_id,indexer_revision,client_id,client_revision,media_type,fingerprint,title,private_payload,series_id,movie_id,status,decision_reasons_json,not_before,attempts,created_at,updated_at,selection_kind)VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,0,?,?,?)",params![id.to_string(),command.id.to_string(),t.indexer_id.to_string(),t.indexer_revision,t.client_id.map(|v|v.to_string()),t.client_revision,domain(t.media_type),fingerprint,title,payload,series,movie,status,reasons,decision.not_before,timestamp,timestamp,t.selection_kind.text()]).await?;
    if let Some(episode) = episode {
        c.execute(
            "INSERT INTO rss_candidate_episodes(candidate_id,series_id,episode_id)VALUES(?,?,?)",
            params![id.to_string(), series, episode],
        )
        .await?;
    }
    Ok(())
}

async fn capture_feed(db: &Database, client: &RefreshClient, command: &RssCommand) -> Result<()> {
    let t = command.target;
    let mut operation = crate::search::operation_evidence()
        .map_err(|e| Error(StatusCode::SERVICE_UNAVAILABLE, e.0))?;
    let mut releases = Vec::new();
    let mut query_index = 0;
    let mut offset = 0;
    let mut release_bytes = 0usize;
    // A generic feed is one document: request it whole instead of refetching per local page.
    let feed = connection(db)
        .await?
        .query(
            "SELECT 1 FROM providers WHERE id=? AND implementation='torrentrss'",
            [t.indexer_id.to_string()],
        )
        .await?
        .next()
        .await?
        .is_some();
    let limit = if feed { indexer::FEED_PAGE_LIMIT } else { 100 };
    for page_number in 0..10 {
        let page = client
            .raw_search(
                &t.indexer_id.to_string(),
                t.indexer_revision,
                IndexerOperation::Rss,
                &indexer::IndexerSearch::Rss {
                    media_type: t.media_type,
                    offset,
                    query_index,
                    limit,
                },
            )
            .await;
        let page = match page {
            Ok(page) => page,
            Err(error) => {
                if let Some(seconds) = error.retry_after_seconds {
                    connection(db).await?.execute("UPDATE rss_commands SET next_attempt_at=? WHERE id=? AND status='running'",params![now()?+i64::from(seconds.clamp(1,86400)),command.id.to_string()]).await?;
                }
                return Err(Error(StatusCode::BAD_GATEWAY, error.code));
            }
        };
        // Native Torznab/Newznab pages fail on any bad item; a generic feed drops bad items one by one.
        if !page.warnings.is_empty() && !page.tolerates_item_warnings {
            return Err(Error(StatusCode::BAD_GATEWAY, "invalid_release"));
        }
        if releases.len() + page.items.len() > 1000 {
            return Err(Error(StatusCode::PAYLOAD_TOO_LARGE, "refresh_limit"));
        }
        for release in page.items {
            let bytes = indexer::encode_private(release)
                .map_err(|_| Error(StatusCode::BAD_GATEWAY, "invalid_release"))?;
            release_bytes = release_bytes.checked_add(bytes.len()).ok_or_else(bad)?;
            if release_bytes > 4 * 1024 * 1024 {
                return Err(Error(StatusCode::PAYLOAD_TOO_LARGE, "refresh_limit"));
            }
            releases.push(bytes);
        }
        let Some(next) = page.next_query else { break };
        if page_number == 9
            || next.query_index < query_index
            || (next.query_index == query_index && next.offset <= offset)
        {
            return Err(Error(StatusCode::BAD_GATEWAY, "refresh_limit"));
        }
        query_index = next.query_index;
        offset = next.offset;
    }
    let c = connection(db).await?;
    let timestamp = now()?;
    let mut captured = Vec::new();
    for mut encoded in releases {
        let release = indexer::decode_private(&encoded)
            .map_err(|_| Error(StatusCode::BAD_GATEWAY, "invalid_release"))?;
        encoded.fill(0);
        // Warm bounded CF work outside the writer; no prewarm decision is durable.
        crate::search::evaluate_with_evidence(
            &c,
            t.media_type,
            t.indexer_id,
            &release,
            SearchContext::Rss,
            timestamp,
            &mut operation,
        )
        .await
        .map_err(|e| Error(StatusCode::INTERNAL_SERVER_ERROR, e.0))?;
        let fingerprint = digest(
            release
                .guid
                .as_deref()
                .filter(|v| !v.is_empty())
                .unwrap_or(&release.download_url)
                .as_bytes(),
        );
        let title = release
            .metadata
            .title
            .clone()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "(untitled release)".into());
        if title.len() > 1024 {
            return Err(Error(StatusCode::BAD_GATEWAY, "invalid_release"));
        }
        captured.push(CapturedRelease {
            id: Uuid::new_v4(),
            fingerprint,
            title,
            release,
        });
    }
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        if !matches!(read(&tx, command.id).await?.status, CommandStatus::Running) {
            return Ok(());
        }
        if !valid_target(&tx, t, IndexerOperation::Rss).await? {
            return Err(Error(StatusCode::CONFLICT, "provider_changed"));
        }
        let count = captured.len() as i64;
        for item in captured {
            insert_captured(&tx, client, command, item, timestamp, &mut operation).await?;
        }
        tx.execute(
            "UPDATE rss_commands SET fetch_complete=1,fetched=?,evaluated=? WHERE id=?",
            params![count, count, command.id.to_string()],
        )
        .await?;
        Ok(())
    }
    .await;
    finish(tx, outcome).await
}
pub(super) async fn recover(c: &Connection, timestamp: i64, code: &str) -> Result<()> {
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let result=async {
        // A persisted dispatch boundary is irrevocable, including process cancellation before its result.
        tx.execute("UPDATE rss_client_attempts SET state='unknown',error_code='submission_unknown',updated_at=max(updated_at,?) WHERE state='dispatching' AND EXISTS(SELECT 1 FROM rss_candidates c WHERE c.id=rss_client_attempts.candidate_id AND c.active_attempt=rss_client_attempts.ordinal AND c.status='submitting')",[timestamp]).await?;
        tx.execute("UPDATE rss_candidates SET status='reconciling',private_payload=NULL,attempts=1,not_before=?,updated_at=?,error_code='submission_unknown' WHERE status='submitting'",params![timestamp,timestamp]).await?;
        tx.execute("UPDATE rss_commands SET status=CASE WHEN attempts<3 THEN 'retry_wait' ELSE 'failed' END,next_attempt_at=max(next_attempt_at,?),completed_at=CASE WHEN attempts<3 THEN NULL ELSE ? END,error_code=? WHERE status='running'",params![timestamp,timestamp,code]).await?;
        Ok(())
    }.await;
    finish(tx, result).await
}

pub(super) async fn claim(c: &Connection, id: Uuid, timestamp: i64) -> Result<RssCommand> {
    let current = read(c, id).await?;
    if !matches!(current.status, CommandStatus::Running) {
        c.execute("UPDATE rss_commands SET status='running',attempts=attempts+1,started_at=?,error_code=NULL WHERE id=?",params![timestamp,id.to_string()]).await?;
    }
    read(c, id).await
}
async fn settle_error(db: &Database, command: &RssCommand, code: &str) -> Result<()> {
    let raw = code;
    let code = match code {
        "provider_changed"
        | "provider_unavailable"
        | "refresh_timeout"
        | "refresh_limit"
        | "refresh_failed"
        | "key_unavailable"
        | "invalid_release"
        | "candidate_limit" => code,
        _ => "storage_error",
    };
    if raw != code {
        // The stored code is a closed set, so the originating internal code would otherwise be lost.
        eprintln!(
            "event=rss_command_error_mapped command_id={} raw_code={raw} stored_code={code}",
            command.id
        );
    }
    let retry = matches!(code, "refresh_timeout" | "refresh_failed" | "storage_error")
        && command.attempts < 3;
    let c = connection(db).await?;
    let timestamp = now()?;
    c.execute("UPDATE rss_commands SET status=?,next_attempt_at=max(next_attempt_at,?),completed_at=?,error_code=? WHERE id=? AND status='running'",params![if retry{"retry_wait"}else{"failed"},timestamp+30,if retry{None}else{Some(timestamp)},code,command.id.to_string()]).await?;
    Ok(())
}
struct CandidateWork {
    public: RssCandidate,
    payload: Option<Vec<u8>>,
    identity: Option<qbittorrent::SubmissionIdentity>,
}
pub(super) async fn client_attempts(c: &Connection, id: Uuid) -> Result<Vec<ClientAttempt>> {
    let mut rows=c.query("SELECT ordinal,client_id,client_revision,state,error_code,created_at,updated_at,selection_round FROM rss_client_attempts WHERE candidate_id=? ORDER BY ordinal",[id.to_string()]).await?;
    let mut result = Vec::new();
    while let Some(row) = rows.next().await? {
        if result.len() >= 128 {
            return Err(bad());
        }
        result.push(ClientAttempt {
            ordinal: row.get::<i64>(0)? as u16,
            selection_round: row.get::<i64>(7)? as u16,
            client_id: Uuid::parse_str(&row.get::<String>(1)?).map_err(|_| bad())?,
            client_revision: row.get(2)?,
            state: serde_json::from_value(serde_json::Value::String(row.get(3)?))
                .map_err(|_| bad())?,
            error_code: row.get(4)?,
            created_at: row.get(5)?,
            updated_at: row.get(6)?,
        });
    }
    Ok(result)
}
async fn candidate(c: &Connection, id: Uuid) -> Result<CandidateWork> {
    let row=c.query("SELECT command_id,media_type,indexer_id,indexer_revision,client_id,client_revision,title,series_id,movie_id,status,decision_reasons_json,not_before,attempts,created_at,updated_at,error_code,private_payload,submission_identity_json,selection_kind FROM rss_candidates WHERE id=?",[id.to_string()]).await?.next().await?.ok_or(Error(StatusCode::NOT_FOUND,"rss_candidate_not_found"))?;
    let source = target_row(&row, 1, 18)?;
    let series: Option<i64> = row.get(7)?;
    let movie: Option<i64> = row.get(8)?;
    let mut public = RssCandidate {
        client_attempts: client_attempts(c, id).await?,
        origin: super::search::origin(c, id).await?,
        id,
        command_id: row
            .get::<Option<String>>(0)?
            .map(|v| Uuid::parse_str(&v).map_err(|_| bad()))
            .transpose()?,
        source,
        title: row.get(6)?,
        target: movie.map(|movie_id| ReleaseTarget::Movies { movie_id }),
        status: row.get(9)?,
        reasons: serde_json::from_str(&row.get::<String>(10)?).map_err(|_| bad())?,
        not_before: row.get(11)?,
        attempts: row.get::<i64>(12)? as u8,
        created_at: row.get(13)?,
        updated_at: row.get(14)?,
        error_code: row.get(15)?,
    };
    let payload = row.get(16)?;
    let identity = row
        .get::<Option<String>>(17)?
        .map(|v| serde_json::from_str(&v).map_err(|_| bad()))
        .transpose()?;
    if let Some(series_id) = series {
        let mut rows=c.query("SELECT episode_id FROM rss_candidate_episodes WHERE candidate_id=? ORDER BY episode_id",[id.to_string()]).await?;
        let mut ids = Vec::new();
        while let Some(r) = rows.next().await? {
            ids.push(r.get(0)?)
        }
        if !ids.is_empty() {
            public.target = Some(ReleaseTarget::Tv {
                series_id,
                episode_ids: ids,
            });
        }
    }
    Ok(CandidateWork {
        public,
        payload,
        identity,
    })
}
pub(super) async fn public_candidate(c: &Connection, id: Uuid) -> Result<RssCandidate> {
    Ok(candidate(c, id).await?.public)
}
async fn candidate_state(
    c: &Connection,
    id: Uuid,
    status: &str,
    code: Option<&str>,
    not_before: Option<i64>,
) -> Result<()> {
    if c.is_autocommit() {
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        let result = candidate_state_in_transaction(&tx, id, status, code, not_before).await;
        return finish(tx, result).await;
    }
    candidate_state_in_transaction(c, id, status, code, not_before).await
}
async fn candidate_state_in_transaction(
    c: &Connection,
    id: Uuid,
    status: &str,
    code: Option<&str>,
    not_before: Option<i64>,
) -> Result<()> {
    let allowed = match status {
        "rejected" | "cancelled" => "'pending','prepared'",
        "observed" => "'submitting'",
        "needs_attention" => "'submitting','reconciling'",
        _ => return Err(bad()),
    };
    if matches!(status, "rejected" | "cancelled") {
        c.execute("UPDATE rss_client_attempts SET state='definitely_not_submitted',error_code=?,updated_at=? WHERE candidate_id=? AND state IN ('selected','prepared') AND EXISTS(SELECT 1 FROM rss_candidates WHERE id=? AND status IN ('pending','prepared'))",params![code.unwrap_or("target_changed"),now()?,id.to_string(),id.to_string()]).await?;
    }
    c.execute(&format!("UPDATE rss_candidates SET status=?,private_payload=CASE WHEN ? IN ('rejected','cancelled') THEN NULL ELSE private_payload END,error_code=?,not_before=?,updated_at=? WHERE id=? AND status IN ({allowed})"),params![status,status,code,not_before,now()?,id.to_string()]).await?;
    Ok(())
}

async fn reconcile(db: &Database, client: &RefreshClient, work: CandidateWork) -> Result<()> {
    let p = work.public;
    let c = connection(db).await?;
    let identity = work.identity.ok_or_else(bad)?;
    let (client_id, client_revision) = p.source.resolved()?;
    let result = client
        .reconcile_download(&client_id.to_string(), client_revision, &identity)
        .await;
    match result {
        Ok(qbittorrent::Reconciliation::Observed { .. }) => {
            candidate_state(
                &c,
                p.id,
                "needs_attention",
                Some("presence_unconfirmed"),
                None,
            )
            .await
        }
        Ok(qbittorrent::Reconciliation::NotObserved) if p.attempts >= 3 => {
            candidate_state(&c, p.id, "needs_attention", Some("not_observed"), None).await
        }
        Err(ref error) if !error.retryable || p.attempts >= 3 => {
            candidate_state(&c, p.id, "needs_attention", Some(error.code), None).await
        }
        result => {
            let delay = result
                .err()
                .and_then(|e| e.retry_after_seconds)
                .unwrap_or(30)
                .clamp(30, 86400);
            c.execute("UPDATE rss_candidates SET status='reconciling',attempts=attempts+1,error_code='submission_unknown',not_before=?,updated_at=? WHERE id=?",params![now()?+i64::from(delay),now()?,p.id.to_string()]).await?;
            Ok(())
        }
    }
}
/// Only the writer's fresh decision can discard a payload or park a pending row.
async fn settle_local(
    c: &Connection,
    client: &RefreshClient,
    expected: &RssCandidate,
    release: &indexer::Release,
    timestamp: i64,
    operation: &mut crate::release_profile_terms::OperationEvidence,
) -> Result<Option<crate::search::ReleaseDecision>> {
    if c.is_autocommit() {
        return Err(bad());
    }
    let current = candidate(c, expected.id).await?;
    if current.public.status != expected.status
        || !matches!(current.public.status.as_str(), "pending" | "prepared")
    {
        return Ok(None);
    }
    if !valid_target(c, current.public.source, current.public.origin.operation()).await? {
        candidate_state(c, expected.id, "rejected", Some("provider_changed"), None).await?;
        return Ok(None);
    }
    let decision = pending::evaluate(
        c,
        client,
        current.public.source.media_type,
        current.public.source.indexer_id,
        release,
        super::search::authority(c, expected.id).await?,
        timestamp,
        operation,
    )
    .await?;
    if !singleton(&decision.target) || matches!(decision.disposition, Disposition::Reject) {
        let reasons = if singleton(&decision.target) {
            decision.reasons
        } else {
            vec!["unsupported_target".into()]
        };
        c.execute("UPDATE rss_client_attempts SET state='definitely_not_submitted',error_code='target_changed',updated_at=? WHERE candidate_id=? AND state IN ('selected','prepared')",params![timestamp,expected.id.to_string()]).await?;
        c.execute("UPDATE rss_candidates SET status='rejected',private_payload=NULL,decision_reasons_json=?,not_before=NULL,updated_at=? WHERE id=? AND status IN ('pending','prepared')",params![serde_json::to_string(&reasons).map_err(|_|bad())?,timestamp,expected.id.to_string()]).await?;
        return Ok(None);
    }
    if current.public.target != decision.target {
        candidate_state(c, expected.id, "rejected", Some("target_changed"), None).await?;
        return Ok(None);
    }
    if matches!(decision.disposition, Disposition::Delay) {
        if current.public.status == "prepared" {
            candidate_state(c, expected.id, "rejected", Some("target_changed"), None).await?;
        } else {
            c.execute("UPDATE rss_candidates SET decision_reasons_json=?,not_before=?,updated_at=? WHERE id=? AND status='pending'",params![serde_json::to_string(&decision.reasons).map_err(|_|bad())?,decision.not_before,timestamp,expected.id.to_string()]).await?;
        }
        return Ok(None);
    }
    Ok(Some(decision))
}

fn selected_target(target: &ReleaseTarget) -> Result<crate::db::MediaTarget> {
    match target {
        ReleaseTarget::Tv { episode_ids, .. } if episode_ids.len() == 1 => {
            Ok(crate::db::MediaTarget::Episode(episode_ids[0]))
        }
        ReleaseTarget::Movies { movie_id } => Ok(crate::db::MediaTarget::Movie(*movie_id)),
        _ => Err(bad()),
    }
}
fn selection_intent(source: RssTarget) -> Result<crate::providers::ClientSelection> {
    match source.selection_kind {
        SelectionKind::Default => Ok(crate::providers::ClientSelection::Default {}),
        SelectionKind::Explicit => {
            let (id, revision) = source.resolved()?;
            Ok(crate::providers::ClientSelection::Explicit {
                provider_id: id,
                provider_revision: revision,
            })
        }
        SelectionKind::LegacyFixed => Err(bad()),
    }
}
fn selection_error(code: &'static str) -> &'static str {
    match code {
        "download_client_unavailable"
        | "selection_limit"
        | "provider_changed"
        | "provider_unavailable"
        | "refresh_timeout"
        | "refresh_limit"
        | "key_unavailable"
        | "invalid_release"
        | "unsupported_target"
        | "release_changed"
        | "payload_expired" => code,
        // Durable candidate/command error codes are a closed vocabulary (0052 CHECKs); a
        // client of the wrong protocol is stored as the existing unsupported-target outcome.
        // Admission APIs still answer 409 `protocol_mismatch` before anything is stored.
        "unsupported_protocol" | "protocol_mismatch" => "unsupported_target",
        _ => "refresh_failed",
    }
}
// Only an eligibility/configuration failure grants another client choice.
// Bounds and unsupported targets terminate; internal lookup failures preserve
// the transaction's pending evidence for the existing local-error retry path.
fn selection_fallback(code: &'static str) -> Result<bool> {
    match code {
        "download_client_unavailable" | "provider_changed" | "provider_unavailable" => Ok(true),
        "refresh_limit" | "selection_limit" | "unsupported_target" | "protocol_mismatch" => {
            Ok(false)
        }
        _ => Err(Error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "command_storage_error",
        )),
    }
}
fn automatic_selection(p: &RssCandidate) -> bool {
    matches!(p.source.selection_kind, SelectionKind::Default)
        && matches!(
            p.origin,
            super::search::CandidateOrigin::Rss
                | super::search::CandidateOrigin::Search {
                    mode: super::search::SearchMode::Automatic,
                    ..
                }
        )
}
// A protocol failure belongs to this RSS decision batch. Preserve delayed or
// rejected siblings and never infer permission to retry another command.
async fn park_batch(
    c: &Connection,
    client: &RefreshClient,
    failed: &RssCandidate,
    timestamp: i64,
    operation: &mut crate::release_profile_terms::OperationEvidence,
) -> Result<()> {
    let Some(command_id) = failed.command_id else {
        return Ok(());
    };
    if !matches!(failed.origin, super::search::CandidateOrigin::Rss) {
        return Ok(());
    }
    let failed_protocol = indexer_protocol(c, failed.source.indexer_id).await?;
    let mut rows=c.query("SELECT id FROM rss_candidates WHERE command_id=? AND id!=? AND status='pending' AND selection_kind='default' AND selection_round=0 AND selection_plan_json IS NULL AND active_attempt IS NULL ORDER BY created_at,id LIMIT 1024",params![command_id.to_string(),failed.id.to_string()]).await?;
    let mut ids = Vec::new();
    while let Some(row) = rows.next().await? {
        ids.push(Uuid::parse_str(&row.get::<String>(0)?).map_err(|_| bad())?);
    }
    drop(rows);
    for id in ids {
        let work = candidate(c, id).await?;
        let p = &work.public;
        if !valid_target(c, p.source, p.origin.operation()).await? {
            continue;
        }
        let mut bytes = client
            .open_release(
                &payload_context(id, p.source),
                work.payload.as_deref().ok_or_else(bad)?,
            )
            .map_err(|e| Error(StatusCode::SERVICE_UNAVAILABLE, e.code))?;
        let decoded = indexer::decode_private(&bytes);
        bytes.fill(0);
        let release = decoded.map_err(|_| bad())?;
        // Releases of another protocol do not inherit this batch's client failure.
        if Some(release_protocol(&release)) != failed_protocol {
            continue;
        }
        let decision = pending::evaluate(
            c,
            client,
            p.source.media_type,
            p.source.indexer_id,
            &release,
            crate::search::SearchContext::Rss,
            timestamp,
            operation,
        )
        .await?;
        if !matches!(decision.disposition, Disposition::Accept)
            || !singleton(&decision.target)
            || decision.target != p.target
        {
            continue;
        }
        let collision=match decision.target.as_ref().ok_or_else(bad)? {
            ReleaseTarget::Movies{movie_id}=>c.query("SELECT 1 FROM rss_candidates c WHERE movie_id=? AND id!=? AND status IN ('prepared','submitting','reconciling','observed','needs_attention') AND NOT EXISTS(SELECT 1 FROM rss_candidate_imports i JOIN import_journal j ON j.operation_id=i.operation_id JOIN import_history h ON h.operation_id=j.operation_id WHERE i.candidate_id=c.id AND j.phase='complete') LIMIT 1",params![*movie_id,id.to_string()]).await?.next().await?.is_some(),
            ReleaseTarget::Tv{episode_ids,..}=>c.query("SELECT 1 FROM rss_candidate_episodes e JOIN rss_candidates c ON c.id=e.candidate_id WHERE e.episode_id=? AND c.id!=? AND c.status IN ('prepared','submitting','reconciling','observed','needs_attention') AND NOT EXISTS(SELECT 1 FROM rss_candidate_imports i JOIN import_journal j ON j.operation_id=i.operation_id JOIN import_history h ON h.operation_id=j.operation_id WHERE i.candidate_id=c.id AND j.phase='complete') LIMIT 1",params![episode_ids[0],id.to_string()]).await?.next().await?.is_some(),
        };
        if collision {
            continue;
        }
        c.execute("UPDATE rss_candidates SET selection_strict=1,error_code='download_client_unavailable',decision_reasons_json='[\"download_client_unavailable\"]',not_before=max(coalesce(not_before,0),?),updated_at=? WHERE id=?",params![timestamp+60,timestamp,id.to_string()]).await?;
    }
    Ok(())
}
fn release_protocol(release: &indexer::Release) -> crate::providers::Protocol {
    if release.facts.torrent.is_some() {
        crate::providers::Protocol::Torrent
    } else {
        crate::providers::Protocol::Usenet
    }
}
pub(super) async fn indexer_protocol(
    c: &Connection,
    indexer: Uuid,
) -> Result<Option<crate::providers::Protocol>> {
    Ok(c.query(
        "SELECT implementation FROM providers WHERE id=?",
        [indexer.to_string()],
    )
    .await?
    .next()
    .await?
    .map(|row| row.get::<String>(0))
    .transpose()?
    .as_deref()
    .and_then(crate::providers::Protocol::of_implementation))
}
/// Sonarr's recent-episode window (air date within 14 days) and Radarr's recent-movie window
/// (physical/digital release within 21 days or cinema release within 120 days).
async fn recent_target(c: &Connection, target: &crate::db::MediaTarget) -> Result<bool> {
    let row = match target {
        crate::db::MediaTarget::Episode(id) => c.query("SELECT coalesce(julianday(air_date_utc)>=julianday('now','start of day','-14 days'),0) FROM episodes WHERE id=?",[*id]).await?.next().await?,
        crate::db::MediaTarget::Movie(id) => c.query("SELECT coalesce(julianday(d.physical_release)>=julianday('now','-21 days'),0) OR coalesce(julianday(d.digital_release)>=julianday('now','-21 days'),0) OR coalesce(julianday(d.in_cinemas)>=julianday('now','-120 days'),0) FROM movies m JOIN movie_metadata d ON d.id=m.metadata_id WHERE m.id=?",[*id]).await?.next().await?,
    };
    Ok(row.map(|r| r.get::<i64>(0)).transpose()?.unwrap_or(0) == 1)
}
async fn park_unavailable(
    c: &Connection,
    client: &RefreshClient,
    p: &RssCandidate,
    timestamp: i64,
    operation: &mut crate::release_profile_terms::OperationEvidence,
) -> Result<()> {
    c.execute("UPDATE rss_candidates SET status='pending',selection_plan_json=NULL,selection_strict=1,client_id=NULL,client_revision=NULL,active_attempt=NULL,submission_identity_json=NULL,comparison_facts_json=NULL,error_code='download_client_unavailable',decision_reasons_json='[\"download_client_unavailable\"]',not_before=?,updated_at=? WHERE id=?",params![timestamp+60,timestamp,p.id.to_string()]).await?;
    park_batch(c, client, p, timestamp, operation).await
}
async fn fail_selected(
    c: &Connection,
    client: &RefreshClient,
    p: &RssCandidate,
    code: &'static str,
    attributable: bool,
    fallback: bool,
    retry_round: bool,
    operation: &mut crate::release_profile_terms::OperationEvidence,
) -> Result<()> {
    let timestamp = now()?;
    let accepted=c.execute("UPDATE rss_client_attempts SET state='definitely_not_submitted',error_code=?,updated_at=? WHERE candidate_id=? AND ordinal=(SELECT active_attempt FROM rss_candidates WHERE id=? AND status=? AND client_id=? AND client_revision=?) AND state IN ('selected','prepared','dispatching')",params![selection_error(code),timestamp,p.id.to_string(),p.id.to_string(),p.status.clone(),p.source.client_id.map(|v|v.to_string()),p.source.client_revision]).await?;
    if accepted != 1 {
        return Ok(());
    }
    let (id, revision) = p.source.resolved()?;
    if attributable
        && valid_provider(
            c,
            RefreshTarget {
                provider_id: id,
                media_type: p.source.media_type,
            },
            revision,
        )
        .await?
    {
        crate::providers::backoff::apply(
            c,
            &id.to_string(),
            revision,
            p.source.media_type,
            crate::providers::backoff::Observation::Failure,
            timestamp,
            client.backoff_grace_elapsed(),
        )
        .await
        .map_err(|_| {
            Error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "client_backoff_storage_error",
            )
        })?;
    }
    c.execute(
        "DELETE FROM rss_hash_claims WHERE candidate_id=? AND client_id=?",
        params![p.id.to_string(), id.to_string()],
    )
    .await?;
    let remaining=c.query("SELECT EXISTS(SELECT 1 FROM rss_candidates c,json_each(c.selection_plan_json) p WHERE c.id=? AND NOT EXISTS(SELECT 1 FROM rss_client_attempts a WHERE a.candidate_id=c.id AND a.selection_round=c.selection_round AND a.client_id=json_extract(p.value,'$.client_id')))",[p.id.to_string()]).await?.next().await?.ok_or_else(bad)?.get::<i64>(0)?==1;
    if fallback
        && remaining
        && matches!(p.source.selection_kind, SelectionKind::Default)
        && matches!(p.source.media_type, MediaDomain::Tv)
    {
        c.execute("UPDATE rss_candidates SET status='pending',client_id=NULL,client_revision=NULL,active_attempt=NULL,submission_identity_json=NULL,comparison_facts_json=NULL,error_code=NULL,not_before=?,updated_at=? WHERE id=?",params![timestamp+1,timestamp,p.id.to_string()]).await?;
    } else if retry_round && !remaining && automatic_selection(p) {
        let row=c.query("SELECT payload_expires_at,(SELECT count(*) FROM rss_client_attempts WHERE candidate_id=c.id) FROM rss_candidates c WHERE id=?",[p.id.to_string()]).await?.next().await?.ok_or_else(bad)?;
        let terminal = if row.get::<i64>(0)? <= timestamp {
            Some("payload_expired")
        } else if row.get::<i64>(1)? >= 128 {
            Some("selection_limit")
        } else {
            None
        };
        if let Some(code) = terminal {
            c.execute("UPDATE rss_candidates SET status='rejected',private_payload=NULL,error_code=?,not_before=NULL,updated_at=? WHERE id=?",params![code,timestamp,p.id.to_string()]).await?;
        } else {
            park_unavailable(c, client, p, timestamp, operation).await?;
        }
    } else {
        c.execute("UPDATE rss_candidates SET status='rejected',private_payload=NULL,error_code=?,not_before=NULL,updated_at=? WHERE id=?",params![selection_error(code),timestamp,p.id.to_string()]).await?;
    }
    Ok(())
}
// Even a stale local client configuration must not discard bytes already fetched
// from a release URL. Retain their non-secret identity before closing the attempt.
async fn fail_preparation(
    c: &Connection,
    client: &RefreshClient,
    p: &RssCandidate,
    code: &'static str,
    fallback: bool,
    identity: Option<&qbittorrent::SubmissionIdentity>,
    operation: &mut crate::release_profile_terms::OperationEvidence,
) -> Result<()> {
    if let Some(identity) = identity {
        if c.query("SELECT 1 FROM rss_client_attempts WHERE candidate_id=? AND identity_json IS NOT NULL AND json_extract(identity_json,'$.payload_sha256')!=?",params![p.id.to_string(),identity.payload_sha256()]).await?.next().await?.is_some() {
            return fail_selected(c,client,p,"release_changed",false,false,false,operation).await;
        }
        c.execute("UPDATE rss_client_attempts SET state='prepared',identity_json=?,updated_at=? WHERE candidate_id=? AND ordinal=(SELECT active_attempt FROM rss_candidates WHERE id=? AND status=? AND client_id=? AND client_revision=?) AND state='selected'",params![serde_json::to_string(identity).map_err(|_|bad())?,now()?,p.id.to_string(),p.id.to_string(),p.status.clone(),p.source.client_id.map(|id|id.to_string()),p.source.client_revision]).await?;
    }
    fail_selected(c, client, p, code, false, fallback, fallback, operation).await
}
// Persist fetched-byte evidence independently of later provider lookups. A
// local lookup failure must not erase the anchor or authorize another client.
async fn retain_prepared_identity(
    c: &Connection,
    client: &RefreshClient,
    p: &RssCandidate,
    identity: &qbittorrent::SubmissionIdentity,
    operation: &mut crate::release_profile_terms::OperationEvidence,
) -> Result<bool> {
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome=async {
        let current=candidate(&tx,p.id).await?;
        if current.public.status!=p.status {return Ok(false)}
        if matches!(p.source.selection_kind,SelectionKind::LegacyFixed) {
            if current.identity.as_ref().is_some_and(|prior|prior.payload_sha256()!=identity.payload_sha256()) {
                candidate_state(&tx,p.id,"rejected",Some("release_changed"),None).await?;return Ok(false)
            }
            tx.execute("UPDATE rss_candidates SET status='prepared',submission_identity_json=?,not_before=NULL,updated_at=? WHERE id=?",params![serde_json::to_string(identity).map_err(|_|bad())?,now()?,p.id.to_string()]).await?;
        } else {
            if tx.query("SELECT 1 FROM rss_client_attempts WHERE candidate_id=? AND identity_json IS NOT NULL AND json_extract(identity_json,'$.payload_sha256')!=?",params![p.id.to_string(),identity.payload_sha256()]).await?.next().await?.is_some() {
                fail_selected(&tx,client,p,"release_changed",false,false,false,operation).await?;return Ok(false)
            }
            tx.execute("UPDATE rss_client_attempts SET state='prepared',identity_json=?,updated_at=? WHERE candidate_id=? AND ordinal=(SELECT active_attempt FROM rss_candidates WHERE id=? AND status=? AND client_id=? AND client_revision=?) AND state='selected'",params![serde_json::to_string(identity).map_err(|_|bad())?,now()?,p.id.to_string(),p.id.to_string(),p.status.clone(),p.source.client_id.map(|id|id.to_string()),p.source.client_revision]).await?;
        }
        Ok(true)
    }.await;
    finish(tx, outcome).await
}
async fn select_attempt(
    c: &Connection,
    client: &RefreshClient,
    p: &RssCandidate,
    target: &crate::db::MediaTarget,
    timestamp: i64,
    operation: &mut crate::release_profile_terms::OperationEvidence,
) -> Result<(bool, Option<String>)> {
    if matches!(p.source.selection_kind, SelectionKind::LegacyFixed) {
        return Ok((true, None));
    }
    let row=c.query("SELECT selection_plan_json,active_attempt,selection_strict,payload_expires_at,selection_round FROM rss_candidates WHERE id=?",[p.id.to_string()]).await?.next().await?.ok_or_else(bad)?;
    let plan = row.get::<Option<String>>(0)?;
    let active = row.get::<Option<i64>>(1)?;
    let strict = row.get::<i64>(2)? == 1;
    let expiry = row.get::<Option<i64>>(3)?;
    let mut round = row.get::<i64>(4)?;
    drop(row);
    if expiry.is_some_and(|v| v <= timestamp) {
        if active.is_some() {
            fail_selected(
                c,
                client,
                p,
                "payload_expired",
                false,
                false,
                false,
                operation,
            )
            .await?
        } else {
            candidate_state(c, p.id, "rejected", Some("payload_expired"), None).await?
        }
        return Ok((false, None));
    }
    let mut cursor = None;
    let plan: Vec<crate::providers::selection::ClientRef> = match plan {
        Some(plan) => serde_json::from_str(&plan).map_err(|_| bad())?,
        None => {
            let plan = match client
                .selection_plan(
                    c,
                    p.source.indexer_id,
                    target,
                    selection_intent(p.source)?,
                    strict,
                    timestamp,
                )
                .await
            {
                Ok(plan) => plan,
                Err(error)
                    if error.code == "download_client_unavailable" && automatic_selection(p) =>
                {
                    park_unavailable(c, client, p, timestamp, operation).await?;
                    return Ok((false, None));
                }
                Err(error) => {
                    selection_fallback(error.code)?;
                    candidate_state(c, p.id, "rejected", Some(selection_error(error.code)), None)
                        .await?;
                    return Ok((false, None));
                }
            };
            let previous_count = client_attempts(c, p.id).await?.len();
            if plan.is_empty() || plan.len() > 128 - previous_count {
                candidate_state(c, p.id, "rejected", Some("selection_limit"), None).await?;
                return Ok((false, None));
            }
            round += 1;
            c.execute("UPDATE rss_candidates SET selection_plan_json=?,selection_round=?,payload_expires_at=coalesce(payload_expires_at,?),updated_at=? WHERE id=?",params![serde_json::to_string(&plan).map_err(|_|bad())?,round,timestamp+1800,timestamp,p.id.to_string()]).await?;
            // Radarr's bound-client path returns before consuming a rotation turn.
            // Read the binding in the same transaction that captured this plan.
            if matches!(p.source.selection_kind, SelectionKind::Default)
                && matches!(p.source.media_type, MediaDomain::Movies)
                && c.query("SELECT 1 FROM provider_scopes WHERE provider_id=? AND media_type='movies' AND download_client_id IS NULL",[p.source.indexer_id.to_string()]).await?.next().await?.is_some()
            {
                cursor = Some(plan[0].client_id.clone())
            }
            plan
        }
    };
    let selected = if active.is_some() {
        let (id, revision) = p.source.resolved()?;
        crate::providers::selection::ClientRef {
            client_id: id.to_string(),
            client_revision: revision,
        }
    } else {
        let attempts = client_attempts(c, p.id).await?;
        let Some(selected) = plan.into_iter().find(|v| {
            !attempts.iter().any(|a| {
                i64::from(a.selection_round) == round && a.client_id.to_string() == v.client_id
            })
        }) else {
            candidate_state(
                c,
                p.id,
                "rejected",
                Some("client_selection_exhausted"),
                None,
            )
            .await?;
            return Ok((false, cursor));
        };
        let ordinal = attempts.len() as i64 + 1;
        c.execute("INSERT INTO rss_client_attempts(candidate_id,ordinal,selection_round,client_id,client_revision,state,created_at,updated_at) VALUES(?,?,?,?,?,'selected',?,?)",params![p.id.to_string(),ordinal,round,selected.client_id.clone(),selected.client_revision,timestamp,timestamp]).await?;
        c.execute("UPDATE rss_candidates SET client_id=?,client_revision=?,active_attempt=?,error_code=NULL,not_before=NULL,updated_at=? WHERE id=?",params![selected.client_id.clone(),selected.client_revision,ordinal,timestamp,p.id.to_string()]).await?;
        selected
    };
    if let Err(error) = client
        .revalidate_selection(
            c,
            p.source.indexer_id,
            target,
            selection_intent(p.source)?,
            strict,
            timestamp,
            &selected,
        )
        .await
    {
        let fallback = selection_fallback(error.code)?;
        let current = candidate(c, p.id).await?.public;
        fail_selected(
            c,
            client,
            &current,
            selection_error(error.code),
            false,
            fallback,
            fallback,
            operation,
        )
        .await?;
        return Ok((false, cursor));
    }
    Ok((true, cursor))
}
async fn defer_local_storage(
    c: &Connection,
    client: &RefreshClient,
    p: &RssCandidate,
    operation: &mut crate::release_profile_terms::OperationEvidence,
) -> Result<()> {
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome=async {
        let timestamp=now()?;
        let expiry=tx.query("SELECT payload_expires_at FROM rss_candidates WHERE id=?",[p.id.to_string()]).await?.next().await?.ok_or_else(bad)?.get::<i64>(0)?;
        if expiry<=timestamp {return fail_selected(&tx,client,p,"payload_expired",false,false,false,operation).await}
        let accepted=tx.execute("UPDATE rss_client_attempts SET state='prepared',error_code='local_storage_retry',updated_at=? WHERE candidate_id=? AND ordinal=(SELECT active_attempt FROM rss_candidates WHERE id=? AND status='submitting' AND client_id=? AND client_revision=?) AND state='dispatching'",params![timestamp,p.id.to_string(),p.id.to_string(),p.source.client_id.map(|id|id.to_string()),p.source.client_revision]).await?;
        if accepted!=1 {return Ok(())}
        tx.execute("DELETE FROM rss_hash_claims WHERE candidate_id=? AND client_id=?",params![p.id.to_string(),p.source.resolved()?.0.to_string()]).await?;
        let accepted=tx.execute("UPDATE rss_candidates SET status='pending',error_code='local_storage_retry',decision_reasons_json='[\"local_storage_retry\"]',not_before=?,updated_at=?,comparison_facts_json=NULL WHERE id=? AND status='submitting'",params![timestamp+30,timestamp,p.id.to_string()]).await?;
        if accepted!=1 {return Err(Error(StatusCode::INTERNAL_SERVER_ERROR,"command_storage_error"))}
        Ok(())
    }.await;
    finish(tx, outcome).await
}
async fn settle_unknown(
    c: &Connection,
    client: &RefreshClient,
    p: &RssCandidate,
    attributable: bool,
    code: &'static str,
    delay: Option<i64>,
) -> Result<()> {
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let result=async {
        let timestamp=now()?;
        if !matches!(p.source.selection_kind,SelectionKind::LegacyFixed) {
            let accepted=tx.execute("UPDATE rss_client_attempts SET state='unknown',error_code=?,updated_at=? WHERE candidate_id=? AND ordinal=(SELECT active_attempt FROM rss_candidates WHERE id=? AND status='submitting') AND state='dispatching'",params![code,timestamp,p.id.to_string(),p.id.to_string()]).await?;
            if accepted!=1 {return Ok(())}
        }
        let accepted=tx.execute("UPDATE rss_candidates SET status=?,private_payload=NULL,attempts=CASE WHEN ? THEN 1 ELSE attempts END,error_code=?,not_before=?,updated_at=? WHERE id=? AND status='submitting'",params![if delay.is_some(){"reconciling"}else{"needs_attention"},delay.is_some(),code,delay.map(|d|timestamp+d),timestamp,p.id.to_string()]).await?;
        let (id,revision)=p.source.resolved()?;
        if accepted==1 && attributable && valid_provider(&tx,RefreshTarget{provider_id:id,media_type:p.source.media_type},revision).await? {
            crate::providers::backoff::apply(&tx,&id.to_string(),revision,p.source.media_type,crate::providers::backoff::Observation::Failure,timestamp,client.backoff_grace_elapsed()).await.map_err(|_|Error(StatusCode::INTERNAL_SERVER_ERROR,"client_backoff_storage_error"))?;
        }
        Ok(())
    }.await;
    finish(tx, result).await
}

async fn process_candidate(
    db: &Database,
    client: &RefreshClient,
    work: CandidateWork,
    operation: &mut crate::release_profile_terms::OperationEvidence,
) -> Result<()> {
    if work.public.status == "reconciling" {
        return reconcile(db, client, work).await;
    }
    if !matches!(work.public.status.as_str(), "pending" | "prepared") {
        return Ok(());
    }
    let mut p = work.public;
    let c = connection(db).await?;
    if !valid_target(&c, p.source, p.origin.operation()).await? {
        return candidate_state(&c, p.id, "rejected", Some("provider_changed"), None).await;
    }
    let mut bytes = client
        .open_release(
            &payload_context(p.id, p.source),
            work.payload.as_deref().ok_or_else(bad)?,
        )
        .map_err(|e| Error(StatusCode::SERVICE_UNAVAILABLE, e.code))?;
    let decoded = indexer::decode_private(&bytes);
    bytes.fill(0);
    let release = decoded.map_err(|_| bad())?;
    // Prewarm may compile bounded CF expressions; current policy is read again
    // together with every early state/deadline write under the Immediate lock.
    crate::search::evaluate_with_evidence(
        &c,
        p.source.media_type,
        p.source.indexer_id,
        &release,
        super::search::authority(&c, p.id).await?,
        now()?,
        operation,
    )
    .await
    .map_err(|e| Error(StatusCode::INTERNAL_SERVER_ERROR, e.0))?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        let timestamp = now()?;
        let Some(decision) = settle_local(&tx, client, &p, &release, timestamp, operation).await?
        else {
            return Ok((None, None));
        };
        let target = selected_target(decision.target.as_ref().ok_or_else(bad)?)?;
        let (ready, cursor) =
            select_attempt(&tx, client, &p, &target, timestamp, operation).await?;
        Ok((ready.then_some(decision), cursor))
    }
    .await;
    let (decision, cursor) = finish(tx, outcome).await?;
    if let Some(cursor) = cursor {
        client.selection_committed(p.source.media_type, &cursor);
    }
    let Some(decision) = decision else {
        return Ok(());
    };
    p = candidate(&c, p.id).await?.public;
    let target = decision.target.ok_or_else(bad)?;
    let (media_target, series, movie, episode) = match &target {
        ReleaseTarget::Tv {
            series_id,
            episode_ids,
        } => (
            crate::db::MediaTarget::Episode(episode_ids[0]),
            Some(*series_id),
            None,
            Some(episode_ids[0]),
        ),
        ReleaseTarget::Movies { movie_id } => (
            crate::db::MediaTarget::Movie(*movie_id),
            None,
            Some(*movie_id),
            None,
        ),
    };
    let source = match &release.facts.torrent {
        Some(facts) => {
            let mut options = qbittorrent::QbitOptions::default();
            options.ratio_limit = facts.minimum_ratio;
            options.seeding_minutes = facts
                .minimum_seed_seconds
                .and_then(|v| v.checked_add(59))
                .and_then(|v| i64::try_from(v / 60).ok());
            crate::providers::download::ReleaseSource::Torrent {
                source: match &facts.magnet_url {
                    Some(value) => qbittorrent::AddSource::Magnet(value.clone()),
                    None => qbittorrent::AddSource::Url(release.download_url.clone()),
                },
                options,
            }
        }
        None => crate::providers::download::ReleaseSource::Usenet {
            url: release.download_url.clone(),
            title: release
                .metadata
                .title
                .clone()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| p.title.clone()),
        },
    };
    // Clients configured with distinct recent/older priorities need the target's recency.
    let recent = recent_target(&c, &media_target).await?;
    let (client_id, client_revision) = p.source.resolved()?;
    let prepared = match client
        .prepare_download(
            &client_id.to_string(),
            client_revision,
            media_target,
            source,
            recent,
        )
        .await
    {
        Ok(prepared) => prepared,
        Err(error) => {
            if error.local_storage_failure {
                if let Some(identity) = error.identity.as_ref()
                    && !retain_prepared_identity(&c, client, &p, identity, operation).await?
                {
                    return Ok(());
                }
                return Err(Error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "command_storage_error",
                ));
            }
            if matches!(p.source.selection_kind, SelectionKind::LegacyFixed) {
                return candidate_state(&c, p.id, "rejected", Some(error.error.code), None).await;
            }
            let tx = c
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .await?;
            let result = fail_preparation(
                &tx,
                client,
                &p,
                selection_error(error.error.code),
                error.client_changed,
                error.identity.as_ref(),
                operation,
            )
            .await;
            return finish(tx, result).await;
        }
    };
    if !matches!(p.source.selection_kind, SelectionKind::LegacyFixed)
        && !retain_prepared_identity(&c, client, &p, prepared.identity(), operation).await?
    {
        return Ok(());
    }
    let identity = serde_json::to_string(prepared.identity()).map_err(|_| bad())?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome=async{
        let current=candidate(&tx,p.id).await?;
        if current.public.status!=p.status{return Ok(false)}
        if !valid_target(&tx,p.source,current.public.origin.operation()).await?{candidate_state(&tx,p.id,"rejected",Some("provider_changed"),None).await?;return Ok(false)}
        // Recheck all current local decision facts after preparation's network reads.
        let timestamp=now()?;
        if !matches!(p.source.selection_kind,SelectionKind::LegacyFixed) {
            let selected=crate::providers::selection::ClientRef{client_id:client_id.to_string(),client_revision};
            let strict=tx.query("SELECT selection_strict FROM rss_candidates WHERE id=?",[p.id.to_string()]).await?.next().await?.ok_or_else(bad)?.get::<i64>(0)?==1;
            if let Err(error)=client.revalidate_selection(&tx,p.source.indexer_id,prepared.identity().target(),selection_intent(p.source)?,strict,timestamp,&selected).await {
                fail_preparation(&tx,client,&p,selection_error(error.code),selection_fallback(error.code)?,Some(prepared.identity()),operation).await?;return Ok(false)
            }
            if tx.query("SELECT 1 FROM rss_client_attempts WHERE candidate_id=? AND identity_json IS NOT NULL AND json_extract(identity_json,'$.payload_sha256')!=?",params![p.id.to_string(),prepared.identity().payload_sha256()]).await?.next().await?.is_some() {
                fail_selected(&tx,client,&p,"release_changed",false,false,false,operation).await?;return Ok(false)
            }
        }
        let Some(latest)=settle_local(&tx,client,&p,&release,timestamp, operation).await? else {return Ok(false)};
        if p.status=="pending" && matches!(p.origin,super::search::CandidateOrigin::Rss) {
            let items=pending::cohort(&tx,client,&target).await?;
            if pending::best(&tx,&items,timestamp, operation).await?.is_some_and(|winner|winner!=p.id){return Ok(false)}
        }
        if !matches!(latest.disposition,Disposition::Accept) || latest.target.as_ref()!=Some(&target) {
            candidate_state(&tx,p.id,"rejected",Some("target_changed"),None).await?;return Ok(false)
        }
        if p.status=="pending" {
            let collision=if let Some(movie)=movie {
                tx.query("SELECT 1 FROM rss_candidates c WHERE movie_id=? AND id!=? AND status IN ('prepared','submitting','reconciling','observed','needs_attention') AND NOT EXISTS(SELECT 1 FROM rss_candidate_imports i JOIN import_journal j ON j.operation_id=i.operation_id JOIN import_history h ON h.operation_id=j.operation_id WHERE i.candidate_id=c.id AND j.phase='complete') LIMIT 1",params![movie,p.id.to_string()]).await?.next().await?.is_some()
            }else{
                tx.query("SELECT 1 FROM rss_candidate_episodes e JOIN rss_candidates c ON c.id=e.candidate_id WHERE e.episode_id=? AND c.id!=? AND c.status IN ('prepared','submitting','reconciling','observed','needs_attention') AND NOT EXISTS(SELECT 1 FROM rss_candidate_imports i JOIN import_journal j ON j.operation_id=i.operation_id JOIN import_history h ON h.operation_id=j.operation_id WHERE i.candidate_id=c.id AND j.phase='complete') LIMIT 1",params![episode,p.id.to_string()]).await?.next().await?.is_some()
            };
            if collision{candidate_state(&tx,p.id,"rejected",Some("target_conflict"),None).await?;return Ok(false)}
            if matches!(p.source.selection_kind,SelectionKind::LegacyFixed) {
            tx.execute("DELETE FROM rss_candidate_episodes WHERE candidate_id=?",[p.id.to_string()]).await?;
            tx.execute("UPDATE rss_candidates SET series_id=?,movie_id=?,updated_at=? WHERE id=?",params![series,movie,now()?,p.id.to_string()]).await?;
            if let Some(episode)=episode{tx.execute("INSERT INTO rss_candidate_episodes(candidate_id,series_id,episode_id)VALUES(?,?,?)",params![p.id.to_string(),series,episode]).await?;}
            } else {
                tx.execute("UPDATE rss_client_attempts SET state='prepared',identity_json=?,updated_at=? WHERE candidate_id=? AND ordinal=(SELECT active_attempt FROM rss_candidates WHERE id=?) AND state='selected'",params![identity.clone(),timestamp,p.id.to_string(),p.id.to_string()]).await?;
            }
            tx.execute("UPDATE rss_candidates SET status='prepared',submission_identity_json=?,not_before=NULL,decision_reasons_json='[]',updated_at=? WHERE id=?",params![identity.clone(),now()?,p.id.to_string()]).await?;
            return Ok(false)
        }
        if serde_json::to_string(current.identity.as_ref().ok_or_else(bad)?).map_err(|_|bad())?!=identity {
            candidate_state(&tx,p.id,"rejected",Some("target_changed"),None).await?;return Ok(false)
        }
        for hash in prepared.identity().hashes(){
            if tx.query("SELECT 1 FROM rss_hash_claims WHERE client_id=? AND hash=?",params![client_id.to_string(),hash.clone()]).await?.next().await?.is_some(){candidate_state(&tx,p.id,"rejected",Some("hash_conflict"),None).await?;return Ok(false)}
        }
        for hash in prepared.identity().hashes(){tx.execute("INSERT INTO rss_hash_claims(client_id,hash,candidate_id)VALUES(?,?,?)",params![client_id.to_string(),hash.clone(),p.id.to_string()]).await?;}
        let parsed=latest.parsed.as_ref().ok_or_else(bad)?;
        let mut evidence=crate::custom_formats::release(&release,parsed,matches!(p.source.media_type,MediaDomain::Tv));
        crate::custom_formats::populate_quality(&tx,&mut evidence,parsed.quality_name.as_deref(),matches!(p.source.media_type,MediaDomain::Tv)).await.map_err(|e|Error(StatusCode::INTERNAL_SERVER_ERROR,e.0))?;
        let evidence=serde_json::to_string(&evidence).map_err(|_|bad())?;
        if evidence.len()>16384{return Err(Error(StatusCode::CONFLICT,"invalid_release"))}
        if !matches!(p.source.selection_kind,SelectionKind::LegacyFixed) {
            tx.execute("UPDATE rss_client_attempts SET state='dispatching',error_code=NULL,updated_at=? WHERE candidate_id=? AND ordinal=(SELECT active_attempt FROM rss_candidates WHERE id=?) AND state='prepared'",params![timestamp,p.id.to_string(),p.id.to_string()]).await?;
        }
        tx.execute("UPDATE rss_candidates SET status='submitting',private_payload=CASE WHEN selection_kind='legacy_fixed' THEN NULL ELSE private_payload END,comparison_facts_json=?,updated_at=? WHERE id=?",params![evidence,now()?,p.id.to_string()]).await?;Ok(true)
    }.await;
    if !finish(tx, outcome).await? {
        return Ok(());
    }
    // Only a classified response proving no add dispatch permits a new client attempt.
    p = candidate(&c, p.id).await?.public;
    match client
        .submit_download(&client_id.to_string(), client_revision, prepared)
        .await
    {
        Ok(qbittorrent::AddOutcome::Observed { hash }) => {
            let accepted =
                accept_observed(&c, p.id, p.source, hash, client.backoff_grace_elapsed()).await?;
            if accepted && matches!(p.source.selection_kind, SelectionKind::Default) {
                client.selection_succeeded(p.source.media_type, &client_id.to_string());
            }
            Ok(())
        }
        Ok(qbittorrent::AddOutcome::AlreadyPresent { .. }) => {
            settle_unknown(&c, client, &p, false, "preexisting_download", None).await
        }
        Err(error)
            if error.definitely_not_submitted
                && error.code == "provider_database_error"
                && !matches!(p.source.selection_kind, SelectionKind::LegacyFixed) =>
        {
            defer_local_storage(&c, client, &p, operation).await
        }
        Err(error)
            if error.definitely_not_submitted
                && !matches!(p.source.selection_kind, SelectionKind::LegacyFixed) =>
        {
            let tx = c
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .await?;
            let result = fail_selected(
                &tx,
                client,
                &p,
                selection_error(error.code),
                error.client_failure,
                error.client_retry_eligible,
                error.client_retry_eligible
                    && (matches!(p.source.media_type, MediaDomain::Tv)
                        || matches!(
                            error.client_retry_kind,
                            crate::providers::ClientRetryKind::Unavailable
                                | crate::providers::ClientRetryKind::Authentication
                        )),
                operation,
            )
            .await;
            finish(tx, result).await
        }
        result => {
            let attributable = result.as_ref().is_err_and(|error| error.client_failure);
            let delay = result
                .err()
                .and_then(|e| e.retry_after_seconds)
                .unwrap_or(30)
                .clamp(30, 86400);
            settle_unknown(
                &c,
                client,
                &p,
                attributable,
                "submission_unknown",
                Some(i64::from(delay)),
            )
            .await
        }
    }
}
// Recovery belongs to this receipt's first fresh accepted observation, never reconciliation.
pub(super) async fn accept_observed(
    c: &Connection,
    id: Uuid,
    source: RssTarget,
    hash: String,
    grace_elapsed: bool,
) -> Result<bool> {
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        let timestamp = now()?;
        if !matches!(source.selection_kind,SelectionKind::LegacyFixed) {
            let accepted=tx.execute("UPDATE rss_client_attempts SET state='observed',updated_at=? WHERE candidate_id=? AND ordinal=(SELECT active_attempt FROM rss_candidates WHERE id=? AND status='submitting' AND client_id=? AND client_revision=?) AND state='dispatching'",params![timestamp,id.to_string(),id.to_string(),source.client_id.map(|v|v.to_string()),source.client_revision]).await?;
            if accepted!=1 {return Ok(false)}
        }
        let accepted = tx.execute("UPDATE rss_candidates SET status='observed',private_payload=NULL,observed_hash=?,error_code=NULL,not_before=NULL,updated_at=? WHERE id=? AND status='submitting'",params![hash,timestamp,id.to_string()]).await?;
        if accepted == 1 && valid_provider(&tx, RefreshTarget {
            provider_id: source.resolved()?.0,
            media_type: source.media_type,
        }, source.resolved()?.1).await? {
            crate::providers::backoff::apply(
                &tx, &source.resolved()?.0.to_string(), source.resolved()?.1,
                source.media_type, crate::providers::backoff::Observation::Success,
                timestamp, grace_elapsed,
            ).await.map_err(|_|Error(StatusCode::INTERNAL_SERVER_ERROR,"client_backoff_storage_error"))?;
        }
        Ok(accepted==1)
    }.await;
    finish(tx, outcome).await
}

#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct RssCandidateQuery {
    #[ts(optional)]
    pub media_type: Option<MediaDomain>,
    #[ts(optional)]
    pub command_id: Option<Uuid>,
    #[ts(optional)]
    pub status: Option<String>,
    #[serde(default = "default_limit")]
    #[ts(as = "Option<u16>", optional)]
    pub limit: u16,
    #[serde(default)]
    #[ts(as = "Option<u32>", optional)]
    pub offset: u32,
}
async fn candidates(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<RssCandidateQuery>, QueryRejection>,
) -> Result<Json<ApiPage<RssCandidate>>> {
    let q = q.map_err(|_| bad())?.0;
    if q.limit == 0
        || q.limit > 100
        || q.offset > 10000
        || q.status.as_ref().is_some_and(|s| {
            !matches!(
                s.as_str(),
                "pending"
                    | "rejected"
                    | "prepared"
                    | "submitting"
                    | "reconciling"
                    | "observed"
                    | "needs_attention"
                    | "cancelled"
            )
        })
    {
        return Err(bad());
    }
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::ReadOnly)
        .await?;
    let outcome=async{
        let media=q.media_type.map(domain);let command=q.command_id.map(|v|v.to_string());
        let filter="(? IS NULL OR media_type=?) AND (? IS NULL OR command_id=?) AND (? IS NULL OR status=?)";
        let total=tx.query(&format!("SELECT count(*) FROM rss_candidates WHERE {filter}"),params![media,media,command.clone(),command.clone(),q.status.clone(),q.status.clone()]).await?.next().await?.ok_or_else(bad)?.get::<i64>(0)?;
        let mut rows=tx.query(&format!("SELECT id FROM rss_candidates WHERE {filter} ORDER BY created_at DESC,id DESC LIMIT ? OFFSET ?"),params![media,media,command.clone(),command,q.status.clone(),q.status,i64::from(q.limit),i64::from(q.offset)]).await?;
        let mut ids=Vec::new();while let Some(r)=rows.next().await?{ids.push(Uuid::parse_str(&r.get::<String>(0)?).map_err(|_|bad())?)}drop(rows);
        let mut items=Vec::new();for id in ids{items.push(candidate(&tx,id).await?.public)}
        bounded(ApiPage{items,total,limit:q.limit,offset:q.offset})
    }.await;
    finish(tx, outcome).await
}
async fn delete_candidate(
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
    let outcome = async {
        if !matches!(
            candidate(&tx, id).await?.public.status.as_str(),
            "rejected" | "cancelled"
        ) {
            return Err(conflict());
        }
        if !matches!(
            super::search::origin(&tx, id).await?,
            super::search::CandidateOrigin::Rss
        ) {
            return Err(conflict());
        }
        tx.execute("DELETE FROM rss_candidates WHERE id=?", [id.to_string()])
            .await?;
        Ok(StatusCode::NO_CONTENT)
    }
    .await;
    finish(tx, outcome).await
}
const SCHEDULE_COLUMNS: &str = "id,media_type,indexer_id,indexer_revision,client_id,client_revision,revision,interval_seconds,enabled,next_run_at,last_run_at,error_code,created_at,selection_kind";
fn schedule_row(r: libsql::Row) -> Result<RssSchedule> {
    Ok(RssSchedule {
        id: Uuid::parse_str(&r.get::<String>(0)?).map_err(|_| bad())?,
        target: target_row(&r, 1, 13)?,
        revision: r.get(6)?,
        interval_seconds: r.get::<i64>(7)? as u32,
        enabled: r.get::<i64>(8)? == 1,
        next_run_at: r.get(9)?,
        last_run_at: r.get(10)?,
        error_code: r.get(11)?,
        created_at: r.get(12)?,
    })
}
async fn schedules(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
) -> Result<Json<Vec<RssSchedule>>> {
    q.map_err(|_| bad())?;
    let c = connection(&db).await?;
    let mut rows = c
        .query(
            &format!(
                "SELECT {SCHEDULE_COLUMNS} FROM rss_schedules ORDER BY created_at,id LIMIT 64"
            ),
            (),
        )
        .await?;
    let mut items = Vec::new();
    while let Some(r) = rows.next().await? {
        items.push(schedule_row(r)?)
    }
    bounded(items)
}
async fn save_schedule(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
    input: std::result::Result<Json<RssScheduleInput>, JsonRejection>,
) -> Result<Json<RssSchedule>> {
    q.map_err(|_| bad())?;
    let input = input.map_err(|_| bad())?.0;
    if !(60..=86400).contains(&input.interval_seconds) {
        return Err(bad());
    }
    let t = input.target.normalize()?;
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome=async{
        if protocol_mismatch(&tx,t).await?{return Err(protocol_rejection())}
        if !valid_target(&tx,t,IndexerOperation::Rss).await?{return Err(Error(StatusCode::CONFLICT,"provider_changed"))}
        let prior=tx.query(&format!("SELECT {SCHEDULE_COLUMNS} FROM rss_schedules WHERE indexer_id=? AND client_id IS ? AND selection_kind=? AND media_type=?"),params![t.indexer_id.to_string(),t.client_id.map(|v|v.to_string()),t.selection_kind.text(),domain(t.media_type)]).await?.next().await?.map(schedule_row).transpose()?;
        let timestamp=now()?;
        let id=match prior {
            Some(prior)=>{
                if input.revision!=Some(prior.revision)||prior.revision>=MAX_REVISION{return Err(conflict())}
                tx.execute("UPDATE rss_schedules SET indexer_revision=?,client_revision=?,revision=revision+1,interval_seconds=?,enabled=?,next_run_at=?,error_code=NULL WHERE id=?",params![t.indexer_revision,t.client_revision,i64::from(input.interval_seconds),i64::from(input.enabled),timestamp+i64::from(input.interval_seconds),prior.id.to_string()]).await?;prior.id
            },None=>{
                if input.revision.is_some(){return Err(conflict())}
                if tx.query("SELECT count(*) FROM rss_schedules",()).await?.next().await?.ok_or_else(bad)?.get::<i64>(0)? >=64{return Err(Error(StatusCode::TOO_MANY_REQUESTS,"rss_schedule_limit"))}
                let id=Uuid::new_v4();tx.execute("INSERT INTO rss_schedules(id,media_type,indexer_id,indexer_revision,client_id,client_revision,interval_seconds,enabled,next_run_at,created_at,selection_kind)VALUES(?,?,?,?,?,?,?,?,?,?,?)",params![id.to_string(),domain(t.media_type),t.indexer_id.to_string(),t.indexer_revision,t.client_id.map(|v|v.to_string()),t.client_revision,i64::from(input.interval_seconds),i64::from(input.enabled),timestamp+i64::from(input.interval_seconds),timestamp,t.selection_kind.text()]).await?;id
            }
        };
        bounded(schedule_row(tx.query(&format!("SELECT {SCHEDULE_COLUMNS} FROM rss_schedules WHERE id=?"),[id.to_string()]).await?.next().await?.ok_or_else(bad)?)?)
    }.await;
    finish(tx, outcome).await
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct RssScheduleDelete {
    pub revision: i64,
}
async fn delete_schedule(
    State(db): State<Arc<Database>>,
    Path(id): Path<String>,
    q: std::result::Result<Query<RssScheduleDelete>, QueryRejection>,
) -> Result<StatusCode> {
    let q = q.map_err(|_| bad())?.0;
    let id = Uuid::parse_str(&id).map_err(|_| bad())?;
    let c = connection(&db).await?;
    if c.execute(
        "DELETE FROM rss_schedules WHERE id=? AND revision=?",
        params![id.to_string(), q.revision],
    )
    .await?
        != 1
    {
        return Err(conflict());
    }
    Ok(StatusCode::NO_CONTENT)
}
pub(super) async fn schedule_due(c: &Connection, timestamp: i64) -> Result<()> {
    // Expiry only retires undispatched payloads; unknown attempts retain reconciliation identity.
    c.execute("UPDATE rss_client_attempts SET state='definitely_not_submitted',error_code='payload_expired',updated_at=max(updated_at,?) WHERE state IN ('selected','prepared') AND EXISTS(SELECT 1 FROM rss_candidates r WHERE r.id=rss_client_attempts.candidate_id AND r.active_attempt=rss_client_attempts.ordinal AND r.status IN ('pending','prepared') AND r.payload_expires_at<=?)",params![timestamp,timestamp]).await?;
    c.execute("UPDATE rss_candidates SET status='rejected',private_payload=NULL,error_code='payload_expired',not_before=NULL,updated_at=? WHERE status IN ('pending','prepared') AND payload_expires_at<=?",params![timestamp,timestamp]).await?;
    let mut rows=c.query(&format!("SELECT {SCHEDULE_COLUMNS} FROM rss_schedules WHERE enabled=1 AND next_run_at<=? ORDER BY next_run_at,id LIMIT 64"),[timestamp]).await?;
    let mut due = Vec::new();
    while let Some(r) = rows.next().await? {
        due.push(schedule_row(r)?)
    }
    drop(rows);
    for schedule in due {
        let error =
            match enqueue_target(c, schedule.target, CommandPriority::Normal, timestamp).await {
                Ok(_) => None,
                Err(Error(
                    _,
                    code @ ("command_history_full" | "command_conflict" | "provider_changed"),
                )) => Some(code),
                Err(e) => return Err(e),
            };
        c.execute("UPDATE rss_schedules SET next_run_at=?,last_run_at=CASE WHEN ? IS NULL THEN ? ELSE last_run_at END,error_code=?,enabled=CASE WHEN ?='provider_changed' THEN 0 ELSE enabled END WHERE id=?",params![timestamp+i64::from(schedule.interval_seconds),error,timestamp,error,error,schedule.id.to_string()]).await?;
    }
    Ok(())
}
pub(super) async fn run(db: &Database, client: &RefreshClient, command: RssCommand) -> Result<()> {
    if !command.fetch_complete {
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(35),
            capture_feed(db, client, &command),
        )
        .await;
        return match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => settle_error(db, &command, error.1).await,
            Err(_) => settle_error(db, &command, "refresh_timeout").await,
        };
    }
    let c = connection(db).await?;
    let id=c.query("SELECT id FROM rss_candidates WHERE command_id=? AND (status='prepared' OR (status='pending' AND (not_before IS NULL OR not_before<=?))) ORDER BY created_at,id LIMIT 1",params![command.id.to_string(),now()?]).await?.next().await?.map(|r|r.get::<String>(0)).transpose()?;
    if let Some(id) = id {
        let id = Uuid::parse_str(&id).map_err(|_| bad())?;
        let mut operation = match crate::search::operation_evidence() {
            Ok(value) => value,
            Err(error) => return settle_error(db, &command, error.0).await,
        };
        let work = match pending::select_work(db, client, candidate(&c, id).await?, &mut operation)
            .await
        {
            Ok(work) => work,
            Err(error) => return settle_error(db, &command, error.1).await,
        };
        // A cohort winner may belong to another RSS command. Ownership/error reads
        // use the selected candidate; this command retries the attempted cohort work.
        let id = work.public.id;
        if let Err(error) = process_candidate(db, client, work, &mut operation).await {
            let status: String = c
                .query(
                    "SELECT status FROM rss_candidates WHERE id=?",
                    [id.to_string()],
                )
                .await?
                .next()
                .await?
                .ok_or_else(bad)?
                .get(0)?;
            if matches!(status.as_str(), "submitting" | "reconciling") {
                return Err(error);
            }
            return settle_error(db, &command, error.1).await;
        }
    }
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome=async{
        if !matches!(read(&tx,command.id).await?.status,CommandStatus::Running){return Ok(())}
        let counts=tx.query("SELECT sum(status IN ('rejected','cancelled')),sum(status='pending'),sum(status='observed'),sum(status IN ('submitting','reconciling','needs_attention')),sum(status='prepared' OR (status='pending' AND (not_before IS NULL OR not_before<=?))) FROM rss_candidates WHERE command_id=?",params![now()?,command.id.to_string()]).await?.next().await?.ok_or_else(bad)?;
        let rejected=counts.get::<Option<i64>>(0)?.unwrap_or(0);let pending=counts.get::<Option<i64>>(1)?.unwrap_or(0);let observed=counts.get::<Option<i64>>(2)?.unwrap_or(0);let uncertain=counts.get::<Option<i64>>(3)?.unwrap_or(0);let immediate=counts.get::<Option<i64>>(4)?.unwrap_or(0);
        tx.execute("UPDATE rss_commands SET rejected=?,pending=?,observed=?,uncertain=?,next_attempt_at=? WHERE id=?",params![rejected,pending,observed,uncertain,now()?+1,command.id.to_string()]).await?;
        if immediate==0 {
            tx.execute("UPDATE rss_commands SET status=?,completed_at=?,error_code=? WHERE id=?",params![if uncertain>0{"failed"}else{"succeeded"},now()?,if uncertain>0{Some("submission_unknown")}else{None},command.id.to_string()]).await?;
        }
        Ok(())
    }.await;
    finish(tx, outcome).await
}
fn transient_local_error(code: &str) -> bool {
    matches!(
        code,
        "custom_format_state_changed"
            | "custom_format_busy"
            | "custom_format_timeout"
            | "custom_format_worker_failed"
            | "delay_profile_storage_error"
            | "revision_policy_storage_error"
            | "release_profile_storage_error"
            | "release_term_busy"
            | "release_term_timeout"
            | "release_term_worker_failed"
            | "release_term_state_changed"
            | "release_storage_error"
            | "command_storage_error"
    )
}
async fn settle_due_error(c: &Connection, id: Uuid, error: Error) -> Result<()> {
    // Exact transient classes only. Malformed durable facts/configuration retain
    // permanent rejection. For standalone run_due work the one-second worker tick
    // retries these. Parent-command/capture paths instead use settle_error retry_wait.
    if transient_local_error(error.1) {
        return Err(error);
    }
    let code = match error.1 {
        "key_unavailable"
        | "target_changed"
        | "provider_changed"
        | "provider_unavailable"
        | "refresh_timeout"
        | "refresh_failed"
        | "invalid_release" => error.1,
        _ => "storage_error",
    };
    eprintln!("event=rss_candidate_error candidate_id={id} code={code}");
    let current: String = c
        .query(
            "SELECT status FROM rss_candidates WHERE id=?",
            [id.to_string()],
        )
        .await?
        .next()
        .await?
        .ok_or_else(bad)?
        .get(0)?;
    if matches!(current.as_str(), "submitting" | "reconciling") {
        return Err(error);
    }
    candidate_state(c, id, "rejected", Some(code), None).await?;
    Ok(())
}
pub(super) async fn run_due(db: &Database, client: &RefreshClient, id: Uuid) -> Result<()> {
    let c = connection(db).await?;
    let mut operation = crate::search::operation_evidence()
        .map_err(|e| Error(StatusCode::SERVICE_UNAVAILABLE, e.0))?;
    let work =
        match pending::select_work(db, client, candidate(&c, id).await?, &mut operation).await {
            Ok(work) => work,
            Err(error) => return settle_due_error(&c, id, error).await,
        };
    let id = work.public.id;
    if let Err(error) = process_candidate(db, client, work, &mut operation).await {
        settle_due_error(&c, id, error).await?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "rss_revision_tests.rs"]
mod tests;
