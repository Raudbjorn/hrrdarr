//! Durable targeted searches retain private offers separately from authorized submissions.
use super::*;
use crate::{
    db::MediaTarget,
    providers::{RefreshClient, indexer},
    search::{Disposition, ReleaseDecision, SearchContext},
};
#[derive(Clone, Copy, PartialEq, Eq, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    Automatic,
    Interactive,
}
impl SearchMode {
    pub(super) fn operation(self) -> crate::providers::IndexerOperation {
        match self {
            Self::Automatic => crate::providers::IndexerOperation::Automatic,
            Self::Interactive => crate::providers::IndexerOperation::Interactive,
        }
    }
    fn text(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Interactive => "interactive",
        }
    }
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct SearchCommandInput {
    pub request_id: Uuid,
    pub mode: SearchMode,
    pub target: MediaTarget,
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
    pub priority: CommandPriority,
}
#[derive(Serialize, ts_rs::TS)]
pub struct SearchCommand {
    pub id: Uuid,
    pub mode: SearchMode,
    pub target: MediaTarget,
    pub source: rss::RssTarget,
    pub priority: CommandPriority,
    pub status: CommandStatus,
    pub attempts: u8,
    pub next_attempt_at: i64,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub error_code: Option<String>,
    pub fetched: u32,
    pub fetch_complete: bool,
    pub selected_client_id: Option<Uuid>,
    pub selected_client_attempts: Vec<rss::ClientAttempt>,
    pub selected_candidate_id: Option<Uuid>,
    pub selected_candidate_status: Option<String>,
    pub selected_candidate_error_code: Option<String>,
    pub selected_candidate_reasons: Vec<String>,
    // Unix seconds when this command's indexer dispatch phase last completed (any attempt),
    // including a caught indexer failure; `null` until an eligible indexer request was issued.
    pub dispatch_completed_at: Option<i64>,
}
#[derive(Serialize, ts_rs::TS)]
pub struct SearchResult {
    pub id: Uuid,
    pub command_id: Uuid,
    pub metadata: indexer::ReleaseMetadata,
    pub decision: ReleaseDecision,
    pub expires_at: i64,
    pub selected_candidate_id: Option<Uuid>,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct SearchCommandQuery {
    #[ts(optional)]
    pub target_type: Option<String>,
    #[ts(optional)]
    pub target_id: Option<i64>,
    #[ts(optional)]
    pub status: Option<CommandStatus>,
    #[serde(default = "default_limit")]
    #[ts(as = "Option<u16>", optional)]
    pub limit: u16,
    #[serde(default)]
    #[ts(as = "Option<u32>", optional)]
    pub offset: u32,
}
#[derive(Serialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CandidateOrigin {
    Rss,
    Search {
        command_id: Uuid,
        result_id: Uuid,
        mode: SearchMode,
    },
}

impl CandidateOrigin {
    pub(super) fn operation(&self) -> crate::providers::IndexerOperation {
        match self {
            Self::Rss => crate::providers::IndexerOperation::Rss,
            Self::Search { mode, .. } => mode.operation(),
        }
    }
}

#[derive(Clone)]
struct Context {
    db: Arc<Database>,
    client: RefreshClient,
}
const COLUMNS: &str = "c.id,c.mode,c.media_type,c.requested_episode_id,c.requested_movie_id,c.indexer_id,c.indexer_revision,c.client_id,c.client_revision,c.priority,c.status,c.attempts,c.next_attempt_at,c.created_at,c.started_at,c.completed_at,c.error_code,c.fetched,c.fetch_complete,(SELECT selected_candidate_id FROM search_results WHERE command_id=c.id AND selected_candidate_id IS NOT NULL),(SELECT r.status FROM search_results s JOIN rss_candidates r ON r.id=s.selected_candidate_id WHERE s.command_id=c.id),(SELECT r.error_code FROM search_results s JOIN rss_candidates r ON r.id=s.selected_candidate_id WHERE s.command_id=c.id),(SELECT r.decision_reasons_json FROM search_results s JOIN rss_candidates r ON r.id=s.selected_candidate_id WHERE s.command_id=c.id),c.selection_kind,(SELECT r.client_id FROM search_results s JOIN rss_candidates r ON r.id=s.selected_candidate_id WHERE s.command_id=c.id),coalesce((SELECT json_group_array(json_object('ordinal',a.ordinal,'selection_round',a.selection_round,'client_id',a.client_id,'client_revision',a.client_revision,'state',a.state,'error_code',a.error_code,'created_at',a.created_at,'updated_at',a.updated_at)) FROM (SELECT a.* FROM rss_client_attempts a JOIN search_results s ON s.selected_candidate_id=a.candidate_id WHERE s.command_id=c.id ORDER BY a.ordinal) a),'[]'),c.dispatch_completed_at";
fn parse_mode(s: &str) -> Result<SearchMode> {
    match s {
        "automatic" => Ok(SearchMode::Automatic),
        "interactive" => Ok(SearchMode::Interactive),
        _ => Err(bad()),
    }
}
fn target_parts(t: &MediaTarget) -> (MediaDomain, Option<i64>, Option<i64>) {
    match t {
        MediaTarget::Episode(id) => (MediaDomain::Tv, Some(*id), None),
        MediaTarget::Movie(id) => (MediaDomain::Movies, None, Some(*id)),
    }
}
fn uuid(s: String) -> Result<Uuid> {
    Uuid::parse_str(&s).map_err(|_| bad())
}
fn command_row(r: libsql::Row) -> Result<SearchCommand> {
    let media = MediaDomain::parse(&r.get::<String>(2)?).map_err(|_| bad())?;
    Ok(SearchCommand {
        id: uuid(r.get(0)?)?,
        mode: parse_mode(&r.get::<String>(1)?)?,
        target: match media {
            MediaDomain::Tv => MediaTarget::Episode(r.get(3)?),
            MediaDomain::Movies => MediaTarget::Movie(r.get(4)?),
        },
        source: rss::RssTarget {
            media_type: media,
            indexer_id: uuid(r.get(5)?)?,
            indexer_revision: r.get(6)?,
            selection_kind: rss::SelectionKind::parse(&r.get::<String>(23)?)?,
            client_id: r.get::<Option<String>>(7)?.map(uuid).transpose()?,
            client_revision: r.get(8)?,
        },
        priority: if r.get::<i64>(9)? == 1 {
            CommandPriority::High
        } else {
            CommandPriority::Normal
        },
        status: CommandStatus::parse(&r.get::<String>(10)?)?,
        attempts: r.get::<i64>(11)? as u8,
        next_attempt_at: r.get(12)?,
        created_at: r.get(13)?,
        started_at: r.get(14)?,
        completed_at: r.get(15)?,
        error_code: r.get(16)?,
        fetched: r.get::<i64>(17)? as u32,
        fetch_complete: r.get::<i64>(18)? == 1,
        selected_client_id: r.get::<Option<String>>(24)?.map(uuid).transpose()?,
        selected_client_attempts: serde_json::from_str(&r.get::<String>(25)?).map_err(|_| bad())?,
        selected_candidate_id: r.get::<Option<String>>(19)?.map(uuid).transpose()?,
        selected_candidate_status: r.get(20)?,
        selected_candidate_error_code: r.get(21)?,
        selected_candidate_reasons: serde_json::from_str(
            &r.get::<Option<String>>(22)?.unwrap_or_else(|| "[]".into()),
        )
        .map_err(|_| bad())?,
        dispatch_completed_at: r.get(26)?,
    })
}
async fn read(c: &Connection, id: Uuid) -> Result<SearchCommand> {
    command_row(
        c.query(
            &format!("SELECT {COLUMNS} FROM search_commands c WHERE c.id=?"),
            [id.to_string()],
        )
        .await?
        .next()
        .await?
        .ok_or(Error(StatusCode::NOT_FOUND, "search_command_not_found"))?,
    )
}
async fn identity(c: &Connection, target: &MediaTarget) -> Result<String> {
    let value = match target {
        MediaTarget::Episode(id) => {
            let r=c.query("SELECT s.id,s.tvdb_id,s.title,e.season,e.number,l.series_type,l.use_scene_numbering FROM episodes e JOIN series s ON s.id=e.series_id LEFT JOIN library_settings l ON l.series_id=s.id WHERE e.id=?",[*id]).await?.next().await?.ok_or(Error(StatusCode::CONFLICT,"target_changed"))?;
            serde_json::json!({"media_type":"tv","series_id":r.get::<i64>(0)?,"episode_id":id,"tvdb_id":r.get::<Option<i64>>(1)?,"title":r.get::<String>(2)?,"season":r.get::<i64>(3)?,"number":r.get::<i64>(4)?,"series_type":r.get::<Option<String>>(5)?,"use_scene_numbering":r.get::<Option<i64>>(6)?})
        }
        MediaTarget::Movie(id) => {
            let r=c.query("SELECT d.id,d.tmdb_id,d.imdb_id,d.title,d.year FROM movies m JOIN movie_metadata d ON d.id=m.metadata_id WHERE m.id=?",[*id]).await?.next().await?.ok_or(Error(StatusCode::CONFLICT,"target_changed"))?;
            serde_json::json!({"media_type":"movies","movie_id":id,"metadata_id":r.get::<i64>(0)?,"tmdb_id":r.get::<Option<i64>>(1)?,"imdb_id":r.get::<Option<String>>(2)?,"title":r.get::<String>(3)?,"year":r.get::<Option<i64>>(4)?})
        }
    };
    serde_json::to_string(&value).map_err(|_| bad())
}
async fn check_identity(c: &Connection, command: &SearchCommand) -> Result<()> {
    let stored = c
        .query(
            "SELECT captured_target_json FROM search_commands WHERE id=?",
            [command.id.to_string()],
        )
        .await?
        .next()
        .await?
        .ok_or_else(bad)?
        .get::<String>(0)?;
    if identity(c, &command.target).await? != stored {
        return Err(Error(StatusCode::CONFLICT, "target_changed"));
    }
    Ok(())
}
pub(super) async fn origin(c: &Connection, id: Uuid) -> Result<CandidateOrigin> {
    let row=c.query("SELECT c.id,s.id,c.mode FROM rss_candidates r JOIN search_results s ON s.id=r.search_result_id JOIN search_commands c ON c.id=s.command_id WHERE r.id=?",[id.to_string()]).await?.next().await?;
    match row {
        Some(r) => Ok(CandidateOrigin::Search {
            command_id: uuid(r.get(0)?)?,
            result_id: uuid(r.get(1)?)?,
            mode: parse_mode(&r.get::<String>(2)?)?,
        }),
        None => Ok(CandidateOrigin::Rss),
    }
}
pub(crate) async fn authority(c: &Connection, id: Uuid) -> Result<SearchContext> {
    match origin(c, id).await? {
        CandidateOrigin::Rss => Ok(SearchContext::Rss),
        CandidateOrigin::Search { command_id, .. } => {
            let command = read(c, command_id).await?;
            check_identity(c, &command).await?;
            Ok(SearchContext::UserSearch)
        }
    }
}
fn matches_target(requested: &MediaTarget, actual: &Option<crate::search::ReleaseTarget>) -> bool {
    match (requested, actual) {
        (MediaTarget::Episode(id), Some(crate::search::ReleaseTarget::Tv { episode_ids, .. })) => {
            episode_ids.as_slice() == [*id]
        }
        (MediaTarget::Movie(id), Some(crate::search::ReleaseTarget::Movies { movie_id })) => {
            id == movie_id
        }
        _ => false,
    }
}
fn decision_error(error: crate::search::SearchError, invalid: &'static str) -> Error {
    if matches!(
        error.0,
        "release_storage_error"
            | "delay_profile_storage_error"
            | "revision_policy_storage_error"
            | "release_profile_storage_error"
    ) {
        Error(StatusCode::INTERNAL_SERVER_ERROR, "storage_error")
    } else if matches!(
        error.0,
        "release_term_busy"
            | "release_term_timeout"
            | "release_term_worker_failed"
            | "release_term_state_changed"
    ) {
        Error(StatusCode::SERVICE_UNAVAILABLE, error.0)
    } else if error.0.starts_with("custom_format_") {
        Error(
            if matches!(
                error.0,
                "custom_format_busy" | "custom_format_timeout" | "custom_format_worker_failed"
            ) {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::CONFLICT
            },
            error.0,
        )
    } else {
        Error(StatusCode::CONFLICT, invalid)
    }
}
async fn evaluate(
    c: &Connection,
    command: &SearchCommand,
    release: &indexer::Release,
    operation: &mut crate::release_profile_terms::OperationEvidence,
) -> Result<ReleaseDecision> {
    let mut decision = crate::search::evaluate_with_evidence(
        c,
        command.source.media_type,
        command.source.indexer_id,
        release,
        SearchContext::UserSearch,
        now()?,
        operation,
    )
    .await
    .map_err(|e| decision_error(e, "invalid_release"))?;
    if !matches_target(&command.target, &decision.target) {
        decision.deny("requested_target_mismatch")
    }
    // Torrent indexers must supply torrent facts; usenet releases are dispatched to usenet clients.
    if release.facts.torrent.is_none()
        && rss::indexer_protocol(c, command.source.indexer_id).await?
            != Some(crate::providers::Protocol::Usenet)
    {
        decision.deny("unsupported_protocol")
    }
    decision.not_before = None;
    Ok(decision)
}
fn envelope(id: Uuid, command: &SearchCommand) -> String {
    if matches!(
        command.source.selection_kind,
        rss::SelectionKind::LegacyFixed
    ) {
        format!(
            "search-result/{}/{}/{}/{}/{}/{}/{}",
            id,
            command.id,
            command.source.indexer_id,
            command.source.indexer_revision,
            command
                .source
                .client_id
                .map(|v| v.to_string())
                .unwrap_or_default(),
            command.source.client_revision.unwrap_or_default(),
            domain(command.source.media_type)
        )
    } else {
        format!(
            "search-result-v2/{}/{}/{}/{}/{}/{}",
            id,
            command.id,
            command.source.indexer_id,
            command.source.indexer_revision,
            domain(command.source.media_type),
            command.source.selection_kind.text()
        )
    }
}
pub(crate) fn router(db: Arc<Database>, client: RefreshClient) -> Router {
    Router::new()
        .route("/api/v1/search/commands", get(list).post(create))
        .route("/api/v1/search/commands/{id}", get(detail).delete(delete))
        .route("/api/v1/search/commands/{id}/cancel", post(cancel))
        .route("/api/v1/search/commands/{id}/results", get(results))
        .route("/api/v1/search/results/{id}/grab", post(grab))
        .layer(DefaultBodyLimit::max(8192))
        .layer(axum::middleware::from_fn(deadline))
        .with_state(Context { db, client })
}
async fn deadline(request: axum::extract::Request, next: axum::middleware::Next) -> Response {
    match tokio::time::timeout(std::time::Duration::from_secs(5), next.run(request)).await {
        Ok(r) => r,
        Err(_) => Error(StatusCode::SERVICE_UNAVAILABLE, "search_request_timeout").into_response(),
    }
}
async fn create(
    State(s): State<Context>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
    input: std::result::Result<Json<SearchCommandInput>, JsonRejection>,
) -> Result<(StatusCode, Json<SearchCommand>)> {
    q.map_err(|_| bad())?;
    let input = input.map_err(|_| bad())?.0;
    let (media, episode, movie) = target_parts(&input.target);
    let id = episode.or(movie).ok_or_else(bad)?;
    if !(1..=MAX_REVISION).contains(&id) || !(1..=MAX_REVISION).contains(&input.indexer_revision) {
        return Err(bad());
    }
    let source = rss::RssTargetInput {
        media_type: media,
        indexer_id: input.indexer_id,
        indexer_revision: input.indexer_revision,
        client_selection: input.client_selection,
        client_id: input.client_id,
        client_revision: input.client_revision,
    }
    .normalize()?;
    let c = connection(&s.db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        match read(&tx, input.request_id).await {
            Ok(existing) => {
                if existing.mode != input.mode
                    || existing.target != input.target
                    || !same_source(&existing, source)
                    || existing.priority.number() != input.priority.number()
                {
                    return Err(conflict());
                }
                return bounded(existing);
            }
            Err(Error(StatusCode::NOT_FOUND, "search_command_not_found")) => (),
            Err(e) => return Err(e),
        }
        preflight(&tx, &s.client, source, input.mode).await?;
        let captured = identity(&tx, &input.target).await?;
        let timestamp = now()?;
        if command_capacity(&tx).await? >= MAX_COMMANDS {
            return Err(Error(StatusCode::TOO_MANY_REQUESTS, "command_history_full"));
        }
        insert(
            &tx,
            input.request_id,
            input.mode,
            &input.target,
            &captured,
            source,
            input.priority,
            timestamp,
        )
        .await?;
        bounded(read(&tx, input.request_id).await?)
    }
    .await;
    Ok((StatusCode::ACCEPTED, finish(tx, outcome).await?))
}
fn same_source(existing: &SearchCommand, source: rss::RssTarget) -> bool {
    existing.source.media_type == source.media_type
        && existing.source.indexer_id == source.indexer_id
        && existing.source.indexer_revision == source.indexer_revision
        && existing.source.selection_kind == source.selection_kind
        && existing.source.client_id == source.client_id
        && existing.source.client_revision == source.client_revision
}
/// Provider scope, protocol and key readiness shared by single and Wanted admission.
async fn preflight(
    c: &Connection,
    client: &RefreshClient,
    source: rss::RssTarget,
    mode: SearchMode,
) -> Result<()> {
    if rss::protocol_mismatch(c, source).await? {
        return Err(rss::protocol_rejection());
    }
    if !rss::valid_target(c, source, mode.operation()).await? {
        return Err(Error(StatusCode::CONFLICT, "provider_changed"));
    }
    // A selected offer must never depend on an unencrypted locator fallback.
    client
        .seal_release("search-key-preflight", b"configured")
        .map_err(|e| Error(StatusCode::SERVICE_UNAVAILABLE, e.code))?;
    Ok(())
}
#[allow(clippy::too_many_arguments)]
async fn insert(
    c: &Connection,
    id: Uuid,
    mode: SearchMode,
    target: &MediaTarget,
    captured: &str,
    source: rss::RssTarget,
    priority: CommandPriority,
    timestamp: i64,
) -> Result<()> {
    let (media, episode, movie) = target_parts(target);
    c.execute("INSERT INTO search_commands(id,mode,decision_context,media_type,requested_episode_id,requested_movie_id,captured_target_json,indexer_id,indexer_revision,client_id,client_revision,priority,status,attempts,next_attempt_at,created_at,fetched,fetch_complete,selection_kind)VALUES(?,?,'user_search',?,?,?,?,?,?,?,?,?,'queued',0,?,?,0,0,?)",params![id.to_string(),mode.text(),domain(media),episode,movie,captured.to_owned(),source.indexer_id.to_string(),source.indexer_revision,source.client_id.map(|v|v.to_string()),source.client_revision,priority.number(),timestamp,timestamp,source.selection_kind.text()]).await?;
    Ok(())
}

/// Most targets one Wanted request may capture; it equals the shared active-command pool.
pub(crate) const MAX_WANTED_TARGETS: usize = MAX_COMMANDS as usize;
pub(crate) struct WantedChild {
    pub target: MediaTarget,
    pub command_id: Uuid,
}
pub(crate) struct WantedAdmission {
    pub replayed: bool,
    pub matched: Option<u32>,
    pub skipped_active: Option<u32>,
    pub children: Vec<WantedChild>,
}
fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut context = ring::digest::Context::new(&ring::digest::SHA256);
    for part in parts {
        context.update(&(part.len() as u64).to_be_bytes());
        context.update(part);
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(context.finish().as_ref());
    out
}
/// Children of one Wanted request share a 64-bit prefix derived from the request UUID alone,
/// so an exact replay finds its original capture by an indexed range read. The suffix binds
/// the canonical request digest and target; a different body under the same UUID conflicts.
/// Version 1 children (admitted before ordered admission) are only read back for replay.
fn child_id(request: Uuid, digest: &[u8; 32], media: MediaDomain, target: i64) -> Uuid {
    let prefix = sha256(&[b"hrrdarr-wanted-request-v1", request.as_bytes()]);
    let suffix = sha256(&[
        b"hrrdarr-wanted-child-v1",
        request.as_bytes(),
        digest,
        domain(media).as_bytes(),
        &target.to_be_bytes(),
    ]);
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&prefix[..8]);
    bytes[8..].copy_from_slice(&suffix[..8]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}
/// Version 2 children additionally carry their admission ordinal (oldest-searched-first,
/// 0..1023) in the ten bits after the variant. Children of one request share the prefix, so
/// the worker's `created_at,id` claim order runs them in admission order, and replay returns
/// them in that order. The hashed suffix also binds the ordinal.
fn ordered_child_id(
    request: Uuid,
    digest: &[u8; 32],
    media: MediaDomain,
    target: i64,
    ordinal: u16,
) -> Uuid {
    let prefix = sha256(&[b"hrrdarr-wanted-request-v2", request.as_bytes()]);
    let suffix = sha256(&[
        b"hrrdarr-wanted-child-v2",
        request.as_bytes(),
        digest,
        domain(media).as_bytes(),
        &target.to_be_bytes(),
        &ordinal.to_be_bytes(),
    ]);
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&prefix[..8]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = 0x80 | ((ordinal >> 4) as u8 & 0x3f);
    bytes[9] = (((ordinal & 0x0f) as u8) << 4) | (suffix[0] & 0x0f);
    bytes[10..].copy_from_slice(&suffix[1..7]);
    Uuid::from_bytes(bytes)
}
fn child_ordinal(id: Uuid) -> u16 {
    let bytes = id.as_bytes();
    (u16::from(bytes[8] & 0x3f) << 4) | u16::from(bytes[9] >> 4)
}
fn child_range(request: Uuid, ordered: bool) -> (String, String) {
    let id = if ordered {
        ordered_child_id(request, &[0; 32], MediaDomain::Tv, 0, 0)
    } else {
        child_id(request, &[0; 32], MediaDomain::Tv, 0)
    };
    let prefix = id.to_string()[..18].to_owned();
    (
        format!("{prefix}-0000-000000000000"),
        format!("{prefix}-ffff-ffffffffffff"),
    )
}
fn target_id(target: &MediaTarget) -> i64 {
    match target {
        MediaTarget::Episode(id) | MediaTarget::Movie(id) => *id,
    }
}
/// Atomically admits every target `select` captures inside one Immediate transaction as
/// ordinary durable user-search commands (no batch table, no second worker). Whole requests
/// beyond the target bound or the remaining shared pool are refused; nothing is partially
/// admitted. `skip_active` omits targets that already have an active search command.
/// `select` returns targets in admission order (Wanted: oldest-searched-first); duplicates
/// keep their first position, and children are admitted, claimed and returned in that order.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn admit_wanted<F>(
    db: &Database,
    client: &RefreshClient,
    request_id: Uuid,
    digest: [u8; 32],
    source: rss::RssTargetInput,
    priority: CommandPriority,
    skip_active: bool,
    select: F,
) -> Result<WantedAdmission>
where
    F: AsyncFnOnce(&Connection, i64) -> Result<Vec<i64>>,
{
    let media = source.media_type;
    let source = source.normalize()?;
    let mode = SearchMode::Automatic;
    let c = connection(db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        let mut existing = Vec::new();
        let mut ordered = true;
        for version in [true, false] {
            let (low, high) = child_range(request_id, version);
            let mut rows = tx
                .query(
                    &format!("SELECT {COLUMNS} FROM search_commands c WHERE c.id BETWEEN ? AND ? ORDER BY c.id LIMIT ?"),
                    params![low, high, MAX_WANTED_TARGETS as i64 + 1],
                )
                .await?;
            while let Some(row) = rows.next().await? {
                existing.push(command_row(row)?);
            }
            drop(rows);
            if !existing.is_empty() {
                ordered = version;
                break;
            }
        }
        if !existing.is_empty() {
            let mut children = Vec::with_capacity(existing.len());
            for command in existing {
                let target = target_id(&command.target);
                let expected = if ordered {
                    ordered_child_id(request_id, &digest, media, target, child_ordinal(command.id))
                } else {
                    child_id(request_id, &digest, media, target)
                };
                if expected != command.id
                    || command.mode != mode
                    || !same_source(&command, source)
                    || command.priority.number() != priority.number()
                {
                    return Err(conflict());
                }
                children.push(WantedChild {
                    target: command.target,
                    command_id: command.id,
                });
            }
            // Ordered children already come back in admission order (`ORDER BY c.id`).
            if !ordered {
                children.sort_by_key(|child| target_id(&child.target));
            }
            return Ok(WantedAdmission {
                replayed: true,
                matched: None,
                skipped_active: None,
                children,
            });
        }
        preflight(&tx, client, source, mode).await?;
        let timestamp = now()?;
        let mut ids = select(&*tx, timestamp).await?;
        let mut seen = std::collections::BTreeSet::new();
        ids.retain(|id| seen.insert(*id));
        if ids.len() > MAX_WANTED_TARGETS {
            return Err(Error(StatusCode::PAYLOAD_TOO_LARGE, "wanted_search_target_limit"));
        }
        if ids.iter().any(|id| !(1..=MAX_REVISION).contains(id)) {
            return Err(bad());
        }
        let matched = ids.len();
        if skip_active {
            let column = match media {
                MediaDomain::Tv => "requested_episode_id",
                MediaDomain::Movies => "requested_movie_id",
            };
            let mut rows = tx
                .query(
                    &format!("SELECT DISTINCT {column} FROM search_commands WHERE media_type=? AND status IN ('queued','running','retry_wait') AND {column} IS NOT NULL LIMIT ?"),
                    params![domain(media), MAX_COMMANDS + 1],
                )
                .await?;
            let mut active = std::collections::BTreeSet::new();
            while let Some(row) = rows.next().await? {
                active.insert(row.get::<i64>(0)?);
            }
            ids.retain(|id| !active.contains(id));
        }
        if command_capacity(&tx).await? + ids.len() as i64 > MAX_COMMANDS {
            return Err(Error(StatusCode::TOO_MANY_REQUESTS, "command_history_full"));
        }
        let mut children = Vec::with_capacity(ids.len());
        for (ordinal, id) in ids.iter().enumerate() {
            let target = match media {
                MediaDomain::Tv => MediaTarget::Episode(*id),
                MediaDomain::Movies => MediaTarget::Movie(*id),
            };
            let captured = identity(&tx, &target).await?;
            // Bounded above: at most MAX_WANTED_TARGETS (1024) children, so ordinal <= 1023.
            let ordinal = u16::try_from(ordinal).map_err(|_| bad())?;
            let command_id = ordered_child_id(request_id, &digest, media, *id, ordinal);
            insert(&tx, command_id, mode, &target, &captured, source, priority, timestamp).await?;
            children.push(WantedChild { target, command_id });
        }
        Ok(WantedAdmission {
            replayed: false,
            matched: Some(matched as u32),
            skipped_active: Some((matched - ids.len()) as u32),
            children,
        })
    }
    .await;
    finish(tx, outcome).await
}

#[cfg(test)]
mod wanted_child_tests {
    use super::*;
    #[test]
    fn ordered_children_sort_by_ordinal_and_bind_body_target_and_ordinal() {
        let request = Uuid::new_v4();
        let (low, high) = child_range(request, true);
        let ids: Vec<Uuid> = [(900, 0), (3, 1), (7, 15), (1, 16), (2, 1023)]
            .into_iter()
            .map(|(target, ordinal)| {
                ordered_child_id(request, &[1; 32], MediaDomain::Tv, target, ordinal)
            })
            .collect();
        let text: Vec<String> = ids.iter().map(Uuid::to_string).collect();
        let mut sorted = text.clone();
        sorted.sort();
        assert_eq!(sorted, text, "string order is admission order");
        for (id, ordinal) in ids.iter().zip([0, 1, 15, 16, 1023]) {
            assert_eq!(child_ordinal(*id), ordinal);
            let s = id.to_string();
            assert!(low.as_str() <= s.as_str() && s.as_str() <= high.as_str());
            assert_eq!(&s[14..15], "8");
            assert!(
                matches!(&s[19..20], "8" | "9" | "a" | "b"),
                "RFC variant kept"
            );
        }
        assert_ne!(
            ordered_child_id(request, &[1; 32], MediaDomain::Tv, 3, 1),
            ordered_child_id(request, &[2; 32], MediaDomain::Tv, 3, 1)
        );
        assert_ne!(
            ordered_child_id(request, &[1; 32], MediaDomain::Tv, 3, 1),
            ordered_child_id(request, &[1; 32], MediaDomain::Movies, 3, 1)
        );
        let (legacy_low, _) = child_range(request, false);
        assert_ne!(
            &legacy_low[..18],
            &low[..18],
            "v1 and v2 ranges never overlap"
        );
    }
    #[test]
    fn children_share_a_request_prefix_and_bind_body_and_target() {
        let request = Uuid::new_v4();
        let (low, high) = child_range(request, false);
        let a = child_id(request, &[1; 32], MediaDomain::Tv, 7).to_string();
        let b = child_id(request, &[1; 32], MediaDomain::Tv, 8).to_string();
        let c = child_id(request, &[2; 32], MediaDomain::Tv, 7).to_string();
        let movie = child_id(request, &[1; 32], MediaDomain::Movies, 7).to_string();
        for id in [&a, &b, &c, &movie] {
            assert!(
                low.as_str() <= id.as_str() && id.as_str() <= high.as_str(),
                "{id}"
            );
            assert_eq!(
                &id[14..15],
                "8",
                "version 8 never collides with client v4 ids"
            );
        }
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, movie);
        assert_eq!(
            a,
            child_id(request, &[1; 32], MediaDomain::Tv, 7).to_string()
        );
        let other = child_id(Uuid::new_v4(), &[1; 32], MediaDomain::Tv, 7).to_string();
        assert_ne!(&a[..18], &other[..18]);
    }
}

async fn detail(
    State(s): State<Context>,
    Path(id): Path<String>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
) -> Result<Json<SearchCommand>> {
    q.map_err(|_| bad())?;
    bounded(read(&connection(&s.db).await?, uuid(id)?).await?)
}
async fn list(
    State(s): State<Context>,
    q: std::result::Result<Query<SearchCommandQuery>, QueryRejection>,
) -> Result<Json<ApiPage<SearchCommand>>> {
    let q = q.map_err(|_| bad())?.0;
    if q.limit == 0
        || q.limit > 100
        || q.offset > 10000
        || q.target_id.is_some() != q.target_type.is_some()
        || q.target_id
            .is_some_and(|id| !(1..=MAX_REVISION).contains(&id))
        || q.target_type
            .as_deref()
            .is_some_and(|t| !matches!(t, "episode" | "movie"))
    {
        return Err(bad());
    }
    let c = connection(&s.db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::ReadOnly)
        .await?;
    let outcome=async{let status=q.status.map(CommandStatus::text);let filter="(? IS NULL OR c.status=?) AND (? IS NULL OR (?='episode' AND c.requested_episode_id=?) OR (?='movie' AND c.requested_movie_id=?))";let params=||params![status,status,q.target_type.clone(),q.target_type.clone(),q.target_id,q.target_type.clone(),q.target_id];
 let total=tx.query(&format!("SELECT count(*) FROM search_commands c WHERE {filter}"),params()).await?.next().await?.ok_or_else(bad)?.get::<i64>(0)?;
 let mut rows=tx.query(&format!("SELECT {COLUMNS} FROM search_commands c WHERE {filter} ORDER BY created_at DESC,id DESC LIMIT ? OFFSET ?"),params![status,status,q.target_type.clone(),q.target_type.clone(),q.target_id,q.target_type.clone(),q.target_id,i64::from(q.limit),i64::from(q.offset)]).await?;let mut items=Vec::new();while let Some(row)=rows.next().await?{items.push(command_row(row)?)}bounded(ApiPage{items,total,limit:q.limit,offset:q.offset})}.await;
    finish(tx, outcome).await
}
async fn cancel(
    State(s): State<Context>,
    Path(id): Path<String>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
) -> Result<Json<SearchCommand>> {
    q.map_err(|_| bad())?;
    let id = uuid(id)?;
    let c = connection(&s.db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome=async{let command=read(&tx,id).await?;if !matches!(command.status,CommandStatus::Queued|CommandStatus::Running|CommandStatus::RetryWait){return Err(conflict())}tx.execute("UPDATE search_commands SET status='cancelled',completed_at=?,error_code=NULL WHERE id=?",params![now()?,id.to_string()]).await?;bounded(read(&tx,id).await?)}.await;
    finish(tx, outcome).await
}
async fn delete(
    State(s): State<Context>,
    Path(id): Path<String>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
) -> Result<StatusCode> {
    q.map_err(|_| bad())?;
    let id = uuid(id)?;
    let c = connection(&s.db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        let command = read(&tx, id).await?;
        if matches!(
            command.status,
            CommandStatus::Queued | CommandStatus::Running | CommandStatus::RetryWait
        ) || command.selected_candidate_id.is_some()
        {
            return Err(conflict());
        }
        tx.execute(
            "DELETE FROM search_results WHERE command_id=?",
            [id.to_string()],
        )
        .await?;
        tx.execute("DELETE FROM search_commands WHERE id=?", [id.to_string()])
            .await?;
        Ok(StatusCode::NO_CONTENT)
    }
    .await;
    finish(tx, outcome).await
}
fn result_row(r: libsql::Row) -> Result<SearchResult> {
    Ok(SearchResult {
        id: uuid(r.get(0)?)?,
        command_id: uuid(r.get(1)?)?,
        metadata: serde_json::from_str(&r.get::<String>(2)?).map_err(|_| bad())?,
        decision: serde_json::from_str(&r.get::<String>(3)?).map_err(|_| bad())?,
        expires_at: r.get(4)?,
        selected_candidate_id: r.get::<Option<String>>(5)?.map(uuid).transpose()?,
    })
}
async fn results(
    State(s): State<Context>,
    Path(id): Path<String>,
    q: std::result::Result<Query<CommandQuery>, QueryRejection>,
) -> Result<Json<ApiPage<SearchResult>>> {
    let id = uuid(id)?;
    let q = q.map_err(|_| bad())?.0;
    if q.limit == 0
        || q.limit > 100
        || q.offset > 10000
        || q.media_type.is_some()
        || q.status.is_some()
    {
        return Err(bad());
    }
    let c = connection(&s.db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::ReadOnly)
        .await?;
    let outcome=async{read(&tx,id).await?;let total=tx.query("SELECT count(*) FROM search_results WHERE command_id=?",[id.to_string()]).await?.next().await?.ok_or_else(bad)?.get::<i64>(0)?;let mut rows=tx.query("SELECT id,command_id,metadata_json,decision_json,expires_at,selected_candidate_id FROM search_results WHERE command_id=? ORDER BY ordinal,id LIMIT ? OFFSET ?",params![id.to_string(),i64::from(q.limit),i64::from(q.offset)]).await?;let mut items=Vec::new();while let Some(r)=rows.next().await?{items.push(result_row(r)?)}bounded(ApiPage{items,total,limit:q.limit,offset:q.offset})}.await;
    finish(tx, outcome).await
}
async fn grab(
    State(s): State<Context>,
    Path(id): Path<String>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
    body: std::result::Result<Json<Empty>, JsonRejection>,
) -> Result<(StatusCode, Json<rss::RssCandidate>)> {
    q.map_err(|_| bad())?;
    let _ = body.map_err(|_| bad())?;
    let id = uuid(id)?;
    let c = connection(&s.db).await?;
    let mut operation = crate::search::operation_evidence()
        .map_err(|e| Error(StatusCode::SERVICE_UNAVAILABLE, e.0))?;
    if let Some(row)=c.query("SELECT command_id,private_payload FROM search_results WHERE id=? AND selected_candidate_id IS NULL",[id.to_string()]).await?.next().await? {
        if let Some(payload)=row.get::<Option<Vec<u8>>>(1)? {
            let command=read(&c,uuid(row.get::<String>(0)?)?).await?;
            let mut bytes=s.client.open_release(&envelope(id,&command),&payload).map_err(|e|Error(StatusCode::CONFLICT,e.code))?;
            let release=indexer::decode_private(&bytes);bytes.fill(0);let release=release.map_err(|_|Error(StatusCode::CONFLICT,"invalid_release"))?;
            evaluate(&c,&command,&release, &mut operation).await?;
        }
    }
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        let command_id = tx
            .query(
                "SELECT command_id FROM search_results WHERE id=?",
                [id.to_string()],
            )
            .await?
            .next()
            .await?
            .ok_or(Error(StatusCode::NOT_FOUND, "search_result_not_found"))?
            .get::<String>(0)?;
        let command = read(&tx, uuid(command_id)?).await?;
        if command.mode != SearchMode::Interactive {
            return Err(conflict());
        }
        let candidate = select(&tx, &s.client, &command, id, &mut operation).await?;
        bounded(rss::public_candidate(&tx, candidate).await?)
    }
    .await;
    Ok((StatusCode::ACCEPTED, finish(tx, outcome).await?))
}

async fn select(
    c: &Connection,
    client: &RefreshClient,
    command: &SearchCommand,
    id: Uuid,
    operation: &mut crate::release_profile_terms::OperationEvidence,
) -> Result<Uuid> {
    let row=c.query("SELECT selected_candidate_id,expires_at,private_payload,fingerprint,title FROM search_results WHERE id=? AND command_id=?",params![id.to_string(),command.id.to_string()]).await?.next().await?.ok_or(Error(StatusCode::NOT_FOUND,"search_result_not_found"))?;
    // Receipt readback survives expiry and mutable provider/library policy changes.
    if let Some(existing) = row.get::<Option<String>>(0)? {
        return uuid(existing);
    }
    if row.get::<i64>(1)? <= now()? {
        return Err(Error(StatusCode::CONFLICT, "result_expired"));
    }
    let payload: Vec<u8> = row
        .get::<Option<Vec<u8>>>(2)?
        .ok_or(Error(StatusCode::CONFLICT, "release_rejected"))?;
    let fingerprint: String = row.get(3)?;
    let title: String = row.get(4)?;
    drop(row);
    let current = read(c, command.id).await?;
    if !current.fetch_complete
        || current.selected_candidate_id.is_some()
        || !matches!(
            (current.mode, current.status),
            (SearchMode::Automatic, CommandStatus::Running)
                | (SearchMode::Interactive, CommandStatus::Succeeded)
        )
    {
        return Err(conflict());
    }
    if !rss::valid_target(c, command.source, command.mode.operation()).await? {
        return Err(Error(StatusCode::CONFLICT, "provider_changed"));
    }
    check_identity(c, command).await?;
    let mut bytes = client
        .open_release(&envelope(id, command), &payload)
        .map_err(|e| Error(StatusCode::CONFLICT, e.code))?;
    let release =
        indexer::decode_private(&bytes).map_err(|_| Error(StatusCode::CONFLICT, "invalid_release"));
    bytes.fill(0);
    let release = release?;
    let decision = evaluate(c, command, &release, operation).await?;
    if !matches!(decision.disposition, Disposition::Accept) {
        return Err(Error(StatusCode::CONFLICT, "release_rejected"));
    }
    let candidate_id = Uuid::new_v4();
    let mut bytes = indexer::encode_private(release)
        .map_err(|_| Error(StatusCode::CONFLICT, "invalid_release"))?;
    let sealed = client.seal_release(&rss::payload_context(candidate_id, command.source), &bytes);
    bytes.fill(0);
    let sealed = sealed.map_err(|e| Error(StatusCode::CONFLICT, e.code))?;
    let (series, movie, episode) = match decision.target.ok_or_else(bad)? {
        crate::search::ReleaseTarget::Tv {
            series_id,
            episode_ids,
        } if episode_ids.len() == 1 => (Some(series_id), None, Some(episode_ids[0])),
        crate::search::ReleaseTarget::Movies { movie_id } => (None, Some(movie_id), None),
        _ => return Err(Error(StatusCode::CONFLICT, "unsupported_target")),
    };
    let timestamp = now()?;
    if c.query("SELECT count(*)>=1024 FROM rss_candidates", ())
        .await?
        .next()
        .await?
        .ok_or_else(bad)?
        .get::<i64>(0)?
        == 1
    {
        return Err(Error(StatusCode::TOO_MANY_REQUESTS, "candidate_limit"));
    }
    c.execute("INSERT INTO rss_candidates(id,command_id,search_result_id,media_type,indexer_id,indexer_revision,client_id,client_revision,fingerprint,title,private_payload,series_id,movie_id,status,decision_reasons_json,created_at,updated_at,selection_kind)VALUES(?,NULL,?,?,?,?,?,?,?,?,?,?,?,'pending','[]',?,?,?)",params![candidate_id.to_string(),id.to_string(),domain(command.source.media_type),command.source.indexer_id.to_string(),command.source.indexer_revision,command.source.client_id.map(|v|v.to_string()),command.source.client_revision,fingerprint,title,sealed,series,movie,timestamp,timestamp,command.source.selection_kind.text()]).await?;
    if let Some(episode) = episode {
        c.execute(
            "INSERT INTO rss_candidate_episodes(candidate_id,series_id,episode_id)VALUES(?,?,?)",
            params![candidate_id.to_string(), series, episode],
        )
        .await?;
    }
    Ok(candidate_id)
}
pub(super) async fn claim(c: &Connection, id: Uuid, timestamp: i64) -> Result<SearchCommand> {
    c.execute("UPDATE search_commands SET status='running',attempts=attempts+1,started_at=?,error_code=NULL WHERE id=? AND status IN ('queued','retry_wait')",params![timestamp,id.to_string()]).await?;
    read(c, id).await
}
pub(super) async fn recover(c: &Connection, timestamp: i64, code: &str) -> Result<()> {
    c.execute("DELETE FROM search_results WHERE id IN (SELECT s.id FROM search_results s JOIN search_commands c ON c.id=s.command_id WHERE s.selected_candidate_id IS NULL AND s.expires_at<=? AND c.status IN ('succeeded','failed','cancelled') ORDER BY s.expires_at,s.id LIMIT 1024)",[timestamp]).await?;
    c.execute("UPDATE search_commands SET status=CASE WHEN attempts<3 THEN 'retry_wait' ELSE 'failed' END,next_attempt_at=?,completed_at=CASE WHEN attempts<3 THEN NULL ELSE ? END,error_code=? WHERE status='running'",params![timestamp,timestamp,code]).await?;
    Ok(())
}
async fn fetch(
    db: &Database,
    client: &RefreshClient,
    command: &SearchCommand,
    issued: &std::sync::atomic::AtomicBool,
) -> Result<Vec<Vec<u8>>> {
    let c = connection(db).await?;
    check_identity(&c, command).await?;
    if !rss::valid_target(&c, command.source, command.mode.operation()).await? {
        return Err(Error(StatusCode::CONFLICT, "provider_changed"));
    }
    let mut query_index = 0;
    let mut offset = 0;
    let mut releases = Vec::new();
    let mut bytes_total = 0usize;
    for page_number in 0..10 {
        let (request, _) =
            crate::search::target_request(&c, &command.target, offset, query_index, 100)
                .await
                .map_err(|e| decision_error(e, "invalid_search_target"))?;
        let page = match client
            .raw_search_observed(
                &command.source.indexer_id.to_string(),
                command.source.indexer_revision,
                command.mode.operation(),
                &request,
                issued,
            )
            .await
        {
            Ok(p) => p,
            Err(e) => {
                if let Some(seconds) = e.retry_after_seconds {
                    c.execute("UPDATE search_commands SET next_attempt_at=? WHERE id=? AND status='running'",params![now()?+i64::from(seconds.clamp(1,86400)),command.id.to_string()]).await?;
                }
                return Err(Error(StatusCode::BAD_GATEWAY, e.code));
            }
        };
        if !page.warnings.is_empty() {
            return Err(Error(StatusCode::BAD_GATEWAY, "invalid_release"));
        }
        if releases.len() + page.items.len() > 1000 {
            return Err(Error(StatusCode::BAD_GATEWAY, "search_limit"));
        }
        for release in page.items {
            let encoded = indexer::encode_private(release)
                .map_err(|_| Error(StatusCode::BAD_GATEWAY, "invalid_release"))?;
            bytes_total += encoded.len();
            if bytes_total > 4 * 1024 * 1024 {
                return Err(Error(StatusCode::PAYLOAD_TOO_LARGE, "search_limit"));
            }
            releases.push(encoded);
        }
        let Some(next) = page.next_query else {
            return Ok(releases);
        };
        if page_number == 9
            || next.query_index < query_index
            || (next.query_index == query_index && next.offset <= offset)
        {
            return Err(Error(StatusCode::BAD_GATEWAY, "search_limit"));
        }
        query_index = next.query_index;
        offset = next.offset;
    }
    Err(Error(StatusCode::BAD_GATEWAY, "search_limit"))
}
async fn publish(
    db: &Database,
    client: &RefreshClient,
    command: &SearchCommand,
    releases: Vec<Vec<u8>>,
) -> Result<()> {
    let c = connection(db).await?;
    let mut operation = crate::search::operation_evidence()
        .map_err(|e| Error(StatusCode::SERVICE_UNAVAILABLE, e.0))?;
    // Prepare bounded CPU decisions before taking the writer; exact-input cache hits guard inside.
    for bytes in &releases {
        let release = indexer::decode_private(bytes)
            .map_err(|_| Error(StatusCode::BAD_GATEWAY, "invalid_release"))?;
        evaluate(&c, command, &release, &mut operation).await?;
    }
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome=async{
 if !matches!(read(&tx,command.id).await?.status,CommandStatus::Running){return Ok(())}
 if !rss::valid_target(&tx,command.source, command.mode.operation()).await?{return Err(Error(StatusCode::CONFLICT,"provider_changed"))}check_identity(&tx,command).await?;
 let timestamp=now()?;let mut seen=std::collections::BTreeSet::new();let mut best:Option<(crate::search::revision::ReleasePreference,String,Uuid)>=None;let mut count=0;
 for mut bytes in releases {
 let release=indexer::decode_private(&bytes);bytes.fill(0);let release=release.map_err(|_|Error(StatusCode::BAD_GATEWAY,"invalid_release"))?;
 let identity=release.guid.as_deref().filter(|s|!s.is_empty()).unwrap_or(&release.download_url);
 let fingerprint=rss::digest(format!("user_search/{}/{}/{}",command.id,serde_json::to_string(&command.target).map_err(|_|bad())?,identity).as_bytes());if !seen.insert(fingerprint.clone()){continue}
 let decision=evaluate(&tx,command,&release, &mut operation).await?;let id=Uuid::new_v4();
 let title=release.metadata.title.clone().filter(|s|!s.is_empty()).unwrap_or_else(||"(untitled release)".into());if title.len()>1024{return Err(Error(StatusCode::BAD_GATEWAY,"invalid_release"))}
 let metadata=serde_json::to_string(&release.metadata).map_err(|_|bad())?;let decision_json=serde_json::to_string(&decision).map_err(|_|bad())?;
 let payload=if matches!(decision.disposition,Disposition::Accept){
 let preference=crate::search::revision::release_preference(&tx,command.source.media_type,&release,&decision).await.map_err(|error|decision_error(error,"invalid_release"))?;
 if best.as_ref().is_none_or(|(prior,f,_)|preference>*prior||(preference==*prior&&fingerprint<*f)){best=Some((preference,fingerprint.clone(),id));}
 let mut bytes=indexer::encode_private(release).map_err(|_|Error(StatusCode::BAD_GATEWAY,"invalid_release"))?;let payload=client.seal_release(&envelope(id,command),&bytes);bytes.fill(0);Some(payload.map_err(|e|Error(StatusCode::SERVICE_UNAVAILABLE,e.code))?)
 }else{None};
 if tx.query("SELECT (SELECT count(*) FROM search_results)>=1024 OR (SELECT COALESCE(sum(length(private_payload)),0) FROM search_results)+(SELECT COALESCE(sum(length(private_payload)),0) FROM rss_candidates)+?>16777216",[payload.as_ref().map_or(0,Vec::len) as i64]).await?.next().await?.ok_or_else(bad)?.get::<i64>(0)?==1{return Err(Error(StatusCode::TOO_MANY_REQUESTS,"candidate_limit"))}
 tx.execute("INSERT INTO search_results(id,command_id,ordinal,fingerprint,title,metadata_json,decision_json,private_payload,created_at,expires_at)VALUES(?,?,?,?,?,?,?,?,?,?)",params![id.to_string(),command.id.to_string(),count,fingerprint,title,metadata,decision_json,payload,timestamp,timestamp+1800]).await?;count+=1;
 }
 tx.execute("UPDATE search_commands SET fetched=?,fetch_complete=1 WHERE id=?",params![count,command.id.to_string()]).await?;
 if command.mode==SearchMode::Automatic {
 if let Some((_,_,id))=best{select(&tx,client,command,id, &mut operation).await?;}else{tx.execute("UPDATE search_commands SET status='failed',completed_at=?,error_code='no_eligible_release' WHERE id=?",params![timestamp,command.id.to_string()]).await?;return Ok(())}
 }
 tx.execute("UPDATE search_commands SET status='succeeded',completed_at=?,error_code=NULL WHERE id=?",params![timestamp,command.id.to_string()]).await?;Ok(())
 }.await;
    finish(tx, outcome).await
}
/// Native last-search producer. The dispatch phase is over once `fetch` returned or hit the
/// command deadline; if at least one eligible indexer request was issued (success or a caught,
/// classified indexer failure), the target's last-search fact and this command's dispatch
/// progress are written in one transaction, before any release decision or grab. A command
/// that is no longer running (cancelled while dispatching) records nothing.
async fn record_dispatch(db: &Database, command: &SearchCommand) -> Result<bool> {
    let c = connection(db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        let timestamp = now()?;
        let progressed = tx
            .execute(
                "UPDATE search_commands SET dispatch_completed_at=max(coalesce(dispatch_completed_at,0),?) WHERE id=? AND status='running'",
                params![timestamp, command.id.to_string()],
            )
            .await?;
        if progressed != 1 {
            return Ok(false);
        }
        let (table, id) = match command.target {
            MediaTarget::Episode(id) => ("episodes", id),
            MediaTarget::Movie(id) => ("movies", id),
        };
        tx.execute(
            &format!("UPDATE {table} SET native_last_search_at=max(coalesce(native_last_search_at,0),?) WHERE id=?"),
            params![timestamp, id],
        )
        .await?;
        Ok(true)
    }
    .await;
    finish(tx, outcome).await
}
pub(super) async fn run(
    db: &Database,
    client: &RefreshClient,
    command: SearchCommand,
) -> Result<()> {
    let issued = std::sync::atomic::AtomicBool::new(false);
    let fetched = match tokio::time::timeout(
        std::time::Duration::from_secs(35),
        fetch(db, client, &command, &issued),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(Error(StatusCode::GATEWAY_TIMEOUT, "refresh_timeout")),
    };
    // Pre-dispatch refusals (identity, provider revision/policy, backoff) issued nothing.
    let recorded = if issued.load(std::sync::atomic::Ordering::SeqCst) {
        record_dispatch(db, &command).await.map(|recorded| {
            eprintln!(
                "event=search_dispatch_completed command_id={} recorded={recorded} fetch_ok={}",
                command.id,
                fetched.is_ok()
            );
        })
    } else {
        Ok(())
    };
    let outcome = match (recorded, fetched) {
        // The fact could not be stored with the command's progress: a retryable storage failure.
        (Err(e), _) => Err(e),
        (Ok(()), Ok(releases)) => publish(db, client, &command, releases).await,
        (Ok(()), Err(e)) => Err(e),
    };
    if let Err(Error(_, code)) = outcome {
        settle_failure(db, &command, code).await?;
    }
    Ok(())
}
async fn settle_failure(db: &Database, command: &SearchCommand, code: &str) -> Result<()> {
    // Durable enum remains coarse; exact static diagnostics and HTTP errors retain
    // matcher classification. A cold cache is retryable, never an invalid release.
    let code = if matches!(
        code,
        "release_term_busy"
            | "release_term_timeout"
            | "release_term_worker_failed"
            | "release_term_state_changed"
    ) {
        eprintln!(
            "event=search_admission_retry command_id={} code={}",
            command.id, code
        );
        "storage_error"
    } else {
        code
    };
    let code = match code {
        "command_storage_error" => "storage_error",
        "invalid_command_request" => "invalid_release",
        v @ ("interrupted"
        | "storage_error"
        | "provider_changed"
        | "provider_unavailable"
        | "refresh_timeout"
        | "refresh_limit"
        | "refresh_failed"
        | "key_unavailable"
        | "invalid_release"
        | "candidate_limit"
        | "target_changed"
        | "search_limit"
        | "no_eligible_release"
        | "result_expired"
        | "invalid_search_target") => v,
        _ => "invalid_release",
    };
    let retry = command.attempts < 3
        && matches!(
            code,
            "storage_error" | "refresh_timeout" | "refresh_failed" | "provider_unavailable"
        );
    let c = connection(db).await?;
    let timestamp = now()?;
    c.execute("UPDATE search_commands SET status=?,next_attempt_at=MAX(next_attempt_at,?),completed_at=?,error_code=? WHERE id=? AND status='running'",params![if retry{"retry_wait"}else{"failed"},timestamp+(1i64<<command.attempts),if retry{None}else{Some(timestamp)},code,command.id.to_string()]).await?;
    Ok(())
}

#[cfg(test)]
#[path = "search_restriction_tests.rs"]
mod restriction_tests;
