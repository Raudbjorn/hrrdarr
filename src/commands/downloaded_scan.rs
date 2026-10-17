//! Durable downloaded-folder scan commands, phase A: bounded read-only discovery.
//!
//! One `POST` names exactly one absolute file or folder. The worker captures an anchored,
//! bounded inventory on a blocking thread, evaluates and observes every candidate, then
//! publishes the COMPLETE inventory (children, episode membership, media observations and the
//! revision 0 -> 1 transition) in one immediate transaction fenced by `run_generation` and
//! `cancel_requested`. Nothing is truncated: any cap, permission, identity or storage failure
//! leaves revision 0 and no child rows. Phase A performs no transfer; a parent that published
//! selected children stays `running` (phase `awaiting_import`) for the import phase, and a
//! parent with nothing to import settles `succeeded` with outcome `unsuccessful`.
//! See docs/downloaded-scan.md.
use super::*;
use crate::{
    import::{
        scan::{self, Observed},
        scan_fs::{Inventory, Kind},
    },
    media_files::FileRevision,
    search::{
        ReleaseTarget, SearchError,
        downloaded::{
            ScanAuthority, ScanFileDecision, ScanFileInput, ScanOwner, ScanPolicyEligible,
            ScanWarmFile, evaluate_scan_file, resolve_scan_owner, scan_extension_supported,
            select_scan_files, warm_scan_file,
        },
        parser,
    },
};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

const DISCOVERY_BUDGET: Duration = Duration::from_secs(30);
const DISCOVERY_DEADLINE: Duration = Duration::from_secs(40);
const MAX_ATTEMPTS: i64 = 3;
const PAGE_DEFAULT: u16 = 25;
const PAGE_MAX: u16 = 100;
const OFFSET_MAX: u32 = 100_000;
const PATH_MAX_BYTES: usize = 4096;
const DECISION_MAX_BYTES: usize = 16_000;
const REASON_MAX_BYTES: usize = 128;
const REASONS_MAX: usize = 64;
const INVENTORY_MAX_BYTES: i64 = 64 * 1024 * 1024;
const HISTORY_MAX_ROWS: usize = 20_001;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ScanMode {
    Auto,
    Move,
    Copy,
}
impl ScanMode {
    fn text(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Move => "move",
            Self::Copy => "copy",
        }
    }
    fn parse(value: &str) -> Result<Self> {
        match value {
            "auto" => Ok(Self::Auto),
            "move" => Ok(Self::Move),
            "copy" => Ok(Self::Copy),
            _ => Err(bad()),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ScanTracked {
    pub provider_id: Uuid,
    // Lowercase 40- or 64-character hexadecimal download identifier.
    pub remote_id: String,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct DownloadedScanInput {
    // Client-generated immutable identity; an exact replay returns the existing command.
    pub id: Uuid,
    // Absolute, normalized UTF-8 file or folder path of at most 4096 bytes.
    pub path: String,
    pub mode: ScanMode,
    pub priority: CommandPriority,
    #[serde(default)]
    #[ts(optional)]
    pub tracked: Option<ScanTracked>,
}
// Derived from status and inventory revision; never grants execution permission.
#[derive(Clone, Copy, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ScanPhase {
    Queued,
    Inventory,
    AwaitingImport,
    Settled,
}
#[derive(Clone, Copy, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ScanOutcome {
    Success,
    Unsuccessful,
}
#[derive(Clone, Copy, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ScanSource {
    Found,
    Missing,
}
#[derive(Clone, Copy, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ScanTracking {
    Untracked,
    Pending,
    UnknownFallback,
}
#[derive(Serialize, ts_rs::TS)]
pub struct ScanCounts {
    pub entries: i64,
    pub ready: i64,
    pub importing: i64,
    pub imported: i64,
    pub already_imported: i64,
    pub rejected: i64,
    pub failed: i64,
    pub cancelled: i64,
}
#[derive(Serialize, ts_rs::TS)]
pub struct DownloadedScanCommand {
    pub id: Uuid,
    pub media_type: MediaDomain,
    pub path: String,
    pub mode: ScanMode,
    pub priority: CommandPriority,
    pub tracked: Option<ScanTracked>,
    pub tracking: ScanTracking,
    pub status: CommandStatus,
    pub phase: ScanPhase,
    pub outcome: Option<ScanOutcome>,
    pub error_code: Option<String>,
    pub attempts: u8,
    pub run_generation: i64,
    pub cancel_requested: bool,
    pub inventory_revision: u8,
    // Present once the inventory is published.
    pub source: Option<ScanSource>,
    pub next_attempt_at: i64,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub counts: ScanCounts,
}
#[derive(Clone, Copy, PartialEq, Eq, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ScanDisposition {
    Ready,
    Importing,
    Imported,
    AlreadyImported,
    Rejected,
    Failed,
    Cancelled,
}
impl ScanDisposition {
    fn text(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Importing => "importing",
            Self::Imported => "imported",
            Self::AlreadyImported => "already_imported",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
    fn parse(value: &str) -> Result<Self> {
        Ok(match value {
            "ready" => Self::Ready,
            "importing" => Self::Importing,
            "imported" => Self::Imported,
            "already_imported" => Self::AlreadyImported,
            "rejected" => Self::Rejected,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => return Err(bad()),
        })
    }
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ScanEntryKind {
    File,
    Directory,
    Symlink,
    Other,
}
#[derive(Clone, Copy, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ScanResolvedMode {
    Copy,
    Move,
}
#[derive(Clone, Copy, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ScanObservationState {
    Measured,
    Unavailable,
}
#[derive(Serialize, ts_rs::TS)]
pub struct ScanChild {
    pub id: Uuid,
    pub ordinal: i64,
    pub relative_path: String,
    pub source_path: String,
    pub kind: ScanEntryKind,
    pub disposition: ScanDisposition,
    pub selected: bool,
    // Static lowercase reason codes; never arbitrary text or raw errors.
    pub reasons: Vec<String>,
    pub series_id: Option<i64>,
    pub movie_id: Option<i64>,
    pub anchor_episode_id: Option<i64>,
    pub member_count: i64,
    pub expected_old_movie_file_id: Option<i64>,
    pub destination: Option<String>,
    pub resolved_mode: Option<ScanResolvedMode>,
    pub quality_id: Option<i64>,
    pub revision: Option<FileRevision>,
    pub edition: Option<String>,
    pub size: Option<i64>,
    pub observation: Option<ScanObservationState>,
    pub error_code: Option<String>,
    pub operation_id: Option<Uuid>,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct DownloadedScanQuery {
    #[ts(optional)]
    pub status: Option<CommandStatus>,
    #[serde(default = "page_default")]
    #[ts(as = "Option<u16>", optional)]
    pub limit: u16,
    #[serde(default)]
    #[ts(as = "Option<u32>", optional)]
    pub offset: u32,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct DownloadedScanChildQuery {
    #[ts(optional)]
    pub disposition: Option<ScanDisposition>,
    #[serde(default = "page_default")]
    #[ts(as = "Option<u16>", optional)]
    pub limit: u16,
    #[serde(default)]
    #[ts(as = "Option<u32>", optional)]
    pub offset: u32,
}
fn page_default() -> u16 {
    PAGE_DEFAULT
}
fn not_found() -> Error {
    Error(StatusCode::NOT_FOUND, "downloaded_scan_not_found")
}
fn scan_conflict() -> Error {
    Error(StatusCode::CONFLICT, "downloaded_scan_conflict")
}

type Q<T> = std::result::Result<Query<T>, QueryRejection>;
type J<T> = std::result::Result<Json<T>, JsonRejection>;

macro_rules! scoped {
    ($media:expr, $create:ident, $list:ident, $detail:ident, $children:ident, $cancel:ident) => {
        async fn $create(
            state: State<Arc<Database>>,
            q: Q<Empty>,
            input: J<DownloadedScanInput>,
        ) -> Result<(StatusCode, Json<DownloadedScanCommand>)> {
            create(state, $media, q, input).await
        }
        async fn $list(
            state: State<Arc<Database>>,
            q: Q<DownloadedScanQuery>,
        ) -> Result<Json<ApiPage<DownloadedScanCommand>>> {
            list(state, $media, q).await
        }
        async fn $detail(
            state: State<Arc<Database>>,
            path: Path<String>,
            q: Q<Empty>,
        ) -> Result<Json<DownloadedScanCommand>> {
            detail(state, $media, path, q).await
        }
        async fn $children(
            state: State<Arc<Database>>,
            path: Path<String>,
            q: Q<DownloadedScanChildQuery>,
        ) -> Result<Json<ApiPage<ScanChild>>> {
            children(state, $media, path, q).await
        }
        async fn $cancel(
            state: State<Arc<Database>>,
            path: Path<String>,
            q: Q<Empty>,
        ) -> Result<Json<DownloadedScanCommand>> {
            cancel(state, $media, path, q).await
        }
    };
}
scoped!(
    MediaDomain::Tv,
    create_tv,
    list_tv,
    detail_tv,
    children_tv,
    cancel_tv
);
scoped!(
    MediaDomain::Movies,
    create_movies,
    list_movies,
    detail_movies,
    children_movies,
    cancel_movies
);

pub(super) fn router(db: Arc<Database>) -> Router {
    Router::new()
        .route(
            "/api/v1/tv/downloaded-scan-commands",
            get(list_tv).post(create_tv),
        )
        .route("/api/v1/tv/downloaded-scan-commands/{id}", get(detail_tv))
        .route(
            "/api/v1/tv/downloaded-scan-commands/{id}/children",
            get(children_tv),
        )
        .route(
            "/api/v1/tv/downloaded-scan-commands/{id}/cancel",
            post(cancel_tv),
        )
        .route(
            "/api/v1/movies/downloaded-scan-commands",
            get(list_movies).post(create_movies),
        )
        .route(
            "/api/v1/movies/downloaded-scan-commands/{id}",
            get(detail_movies),
        )
        .route(
            "/api/v1/movies/downloaded-scan-commands/{id}/children",
            get(children_movies),
        )
        .route(
            "/api/v1/movies/downloaded-scan-commands/{id}/cancel",
            post(cancel_movies),
        )
        .layer(DefaultBodyLimit::max(8192))
        .layer(axum::middleware::from_fn(deadline))
        .with_state(db)
}
async fn deadline(request: axum::extract::Request, next: axum::middleware::Next) -> Response {
    match tokio::time::timeout(Duration::from_secs(5), next.run(request)).await {
        Ok(response) => response,
        Err(_) => Error(StatusCode::SERVICE_UNAVAILABLE, "downloaded_scan_timeout").into_response(),
    }
}

/// Same rules as the importer: absolute, normalized, no NUL, at most 4096 bytes, root excluded.
fn valid_path(path: &str) -> bool {
    let p = std::path::Path::new(path);
    !path.is_empty()
        && path.len() <= PATH_MAX_BYTES
        && !path.contains('\0')
        && path != "/"
        && p.is_absolute()
        && p.components().all(|c| {
            matches!(
                c,
                std::path::Component::RootDir | std::path::Component::Normal(_)
            )
        })
        && p.components().collect::<std::path::PathBuf>().to_str() == Some(path)
}
fn valid_remote(remote: &str) -> bool {
    matches!(remote.len(), 40 | 64)
        && remote
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
struct Request {
    id: String,
    media: MediaDomain,
    path: String,
    mode: ScanMode,
    priority: i64,
    provider: Option<String>,
    remote: Option<String>,
}
impl Request {
    fn new(media: MediaDomain, input: DownloadedScanInput) -> Result<Self> {
        if !valid_path(&input.path) {
            return Err(bad());
        }
        let (provider, remote) = match input.tracked {
            Some(t) if valid_remote(&t.remote_id) => (
                Some(t.provider_id.hyphenated().to_string()),
                Some(t.remote_id),
            ),
            Some(_) => return Err(bad()),
            None => (None, None),
        };
        Ok(Self {
            id: input.id.hyphenated().to_string(),
            media,
            path: input.path,
            mode: input.mode,
            priority: input.priority.number(),
            provider,
            remote,
        })
    }
}

const COLUMNS: &str = "id,media_type,source_path,requested_mode,priority,requested_provider_id,requested_remote_id,candidate_id,status,outcome,error_code,attempts,run_generation,cancel_requested,inventory_revision,CASE WHEN json_type(inventory_json,'$.missing')='true' THEN 1 ELSE 0 END,next_attempt_at,created_at,started_at,completed_at";
struct Parent {
    id: Uuid,
    media: MediaDomain,
    path: String,
    mode: ScanMode,
    priority: i64,
    provider: Option<String>,
    remote: Option<String>,
    status: CommandStatus,
    outcome: Option<String>,
    error_code: Option<String>,
    attempts: i64,
    run_generation: i64,
    cancel_requested: bool,
    revision: i64,
    missing: bool,
    next_attempt_at: i64,
    created_at: i64,
    started_at: Option<i64>,
    completed_at: Option<i64>,
}
fn parent(r: libsql::Row) -> Result<Parent> {
    Ok(Parent {
        id: Uuid::parse_str(&r.get::<String>(0)?).map_err(|_| bad())?,
        media: MediaDomain::parse(&r.get::<String>(1)?).map_err(|_| bad())?,
        path: r.get(2)?,
        mode: ScanMode::parse(&r.get::<String>(3)?)?,
        priority: r.get(4)?,
        provider: r.get(5)?,
        remote: r.get(6)?,
        status: CommandStatus::parse(&r.get::<String>(8)?)?,
        outcome: r.get(9)?,
        error_code: r.get(10)?,
        attempts: r.get(11)?,
        run_generation: r.get(12)?,
        cancel_requested: r.get::<i64>(13)? == 1,
        revision: r.get(14)?,
        missing: r.get::<i64>(15)? == 1,
        next_attempt_at: r.get(16)?,
        created_at: r.get(17)?,
        started_at: r.get(18)?,
        completed_at: r.get(19)?,
    })
}
async fn read_parent(c: &Connection, id: Uuid) -> Result<Option<Parent>> {
    c.query(
        &format!("SELECT {COLUMNS} FROM downloaded_scan_commands WHERE id=?"),
        [id.hyphenated().to_string()],
    )
    .await?
    .next()
    .await?
    .map(parent)
    .transpose()
}
async fn scoped_parent(c: &Connection, id: Uuid, media: MediaDomain) -> Result<Parent> {
    read_parent(c, id)
        .await?
        .filter(|p| p.media == media)
        .ok_or_else(not_found)
}
async fn counts(c: &Connection, id: Uuid) -> Result<ScanCounts> {
    let mut rows = c
        .query(
            "SELECT disposition,count(*) FROM downloaded_scan_children WHERE command_id=? GROUP BY disposition",
            [id.hyphenated().to_string()],
        )
        .await?;
    let mut counts = ScanCounts {
        entries: 0,
        ready: 0,
        importing: 0,
        imported: 0,
        already_imported: 0,
        rejected: 0,
        failed: 0,
        cancelled: 0,
    };
    while let Some(r) = rows.next().await? {
        let n: i64 = r.get(1)?;
        counts.entries += n;
        match ScanDisposition::parse(&r.get::<String>(0)?)? {
            ScanDisposition::Ready => counts.ready = n,
            ScanDisposition::Importing => counts.importing = n,
            ScanDisposition::Imported => counts.imported = n,
            ScanDisposition::AlreadyImported => counts.already_imported = n,
            ScanDisposition::Rejected => counts.rejected = n,
            ScanDisposition::Failed => counts.failed = n,
            ScanDisposition::Cancelled => counts.cancelled = n,
        }
    }
    Ok(counts)
}
async fn render(c: &Connection, p: Parent) -> Result<DownloadedScanCommand> {
    let counts = counts(c, p.id).await?;
    let tracked = match (&p.provider, &p.remote) {
        (Some(provider), Some(remote)) => Some(ScanTracked {
            provider_id: Uuid::parse_str(provider).map_err(|_| bad())?,
            remote_id: remote.clone(),
        }),
        (None, None) => None,
        _ => return Err(bad()),
    };
    let published = p.revision == 1;
    Ok(DownloadedScanCommand {
        id: p.id,
        media_type: p.media,
        path: p.path,
        mode: p.mode,
        priority: if p.priority == 1 {
            CommandPriority::High
        } else {
            CommandPriority::Normal
        },
        tracking: match (&tracked, published) {
            (None, _) => ScanTracking::Untracked,
            (Some(_), false) => ScanTracking::Pending,
            (Some(_), true) => ScanTracking::UnknownFallback,
        },
        tracked,
        phase: match (p.status, p.revision) {
            (CommandStatus::Queued | CommandStatus::RetryWait, _) => ScanPhase::Queued,
            (CommandStatus::Running, 0) => ScanPhase::Inventory,
            (CommandStatus::Running, _) => ScanPhase::AwaitingImport,
            _ => ScanPhase::Settled,
        },
        status: p.status,
        outcome: match p.outcome.as_deref() {
            None => None,
            Some("success") => Some(ScanOutcome::Success),
            Some("unsuccessful") => Some(ScanOutcome::Unsuccessful),
            Some(_) => return Err(bad()),
        },
        error_code: p.error_code,
        attempts: u8::try_from(p.attempts).map_err(|_| bad())?,
        run_generation: p.run_generation,
        cancel_requested: p.cancel_requested,
        inventory_revision: u8::try_from(p.revision).map_err(|_| bad())?,
        source: published.then_some(if p.missing {
            ScanSource::Missing
        } else {
            ScanSource::Found
        }),
        next_attempt_at: p.next_attempt_at,
        created_at: p.created_at,
        started_at: p.started_at,
        completed_at: p.completed_at,
        counts,
    })
}

async fn create(
    State(db): State<Arc<Database>>,
    media: MediaDomain,
    q: Q<Empty>,
    input: J<DownloadedScanInput>,
) -> Result<(StatusCode, Json<DownloadedScanCommand>)> {
    q.map_err(|_| bad())?;
    let request = Request::new(media, input.map_err(|_| bad())?.0)?;
    let id = Uuid::parse_str(&request.id).map_err(|_| bad())?;
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        // The client UUID is the idempotency key: it is checked before capacity or authority.
        if let Some(existing) = read_parent(&tx, id).await? {
            let same = existing.media == request.media
                && existing.path == request.path
                && existing.mode == request.mode
                && existing.priority == request.priority
                && existing.provider == request.provider
                && existing.remote == request.remote;
            if !same {
                return Err(scan_conflict());
            }
            return Ok((StatusCode::OK, bounded(render(&tx, existing).await?)?));
        }
        if let Some(provider) = &request.provider {
            // A tracked request needs a known provider scoped to this domain, even when the
            // remote item turns out to be unknown; the receipt itself is resolved at discovery.
            let known = tx
                .query(
                    "SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id AND s.media_type=? WHERE p.id=?",
                    params![domain(media), provider.clone()],
                )
                .await?
                .next()
                .await?
                .is_some();
            if !known {
                return Err(Error(
                    StatusCode::CONFLICT,
                    "downloaded_scan_provider_unknown",
                ));
            }
        }
        if command_capacity(&tx).await? >= MAX_COMMANDS {
            return Err(Error(StatusCode::TOO_MANY_REQUESTS, "command_history_full"));
        }
        let timestamp = now()?;
        tx.execute(
            "INSERT INTO downloaded_scan_commands(id,media_type,source_path,requested_mode,requested_provider_id,requested_remote_id,priority,ready_order_at,next_attempt_at,created_at) VALUES(?,?,?,?,?,?,?,?,?,?)",
            params![
                request.id.clone(),
                domain(media),
                request.path.clone(),
                request.mode.text(),
                request.provider.clone(),
                request.remote.clone(),
                request.priority,
                timestamp,
                timestamp,
                timestamp
            ],
        )
        .await?;
        let row = read_parent(&tx, id).await?.ok_or_else(not_found)?;
        Ok((StatusCode::ACCEPTED, bounded(render(&tx, row).await?)?))
    }
    .await;
    finish(tx, outcome).await
}
async fn list(
    State(db): State<Arc<Database>>,
    media: MediaDomain,
    q: Q<DownloadedScanQuery>,
) -> Result<Json<ApiPage<DownloadedScanCommand>>> {
    let q = q.map_err(|_| bad())?.0;
    if !(1..=PAGE_MAX).contains(&q.limit) || q.offset > OFFSET_MAX {
        return Err(bad());
    }
    let status = q.status.map(CommandStatus::text);
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::ReadOnly)
        .await?;
    let outcome = async {
        let total = tx
            .query(
                "SELECT count(*) FROM downloaded_scan_commands WHERE media_type=? AND (? IS NULL OR status=?)",
                params![domain(media), status, status],
            )
            .await?
            .next()
            .await?
            .ok_or_else(bad)?
            .get(0)?;
        let mut rows = tx
            .query(
                &format!("SELECT {COLUMNS} FROM downloaded_scan_commands WHERE media_type=? AND (? IS NULL OR status=?) ORDER BY created_at DESC,id DESC LIMIT ? OFFSET ?"),
                params![domain(media), status, status, i64::from(q.limit), i64::from(q.offset)],
            )
            .await?;
        let mut parents = Vec::new();
        while let Some(r) = rows.next().await? {
            parents.push(parent(r)?);
        }
        drop(rows);
        let mut items = Vec::with_capacity(parents.len());
        for p in parents {
            items.push(render(&tx, p).await?);
        }
        bounded(ApiPage {
            items,
            total,
            limit: q.limit,
            offset: q.offset,
        })
    }
    .await;
    finish(tx, outcome).await
}
fn parse_id(id: &str) -> Result<Uuid> {
    // Only canonical lowercase hyphenated identifiers can name a stored command.
    let parsed = Uuid::parse_str(id).map_err(|_| bad())?;
    if parsed.hyphenated().to_string() != id {
        return Err(bad());
    }
    Ok(parsed)
}
async fn detail(
    State(db): State<Arc<Database>>,
    media: MediaDomain,
    Path(id): Path<String>,
    q: Q<Empty>,
) -> Result<Json<DownloadedScanCommand>> {
    q.map_err(|_| bad())?;
    let id = parse_id(&id)?;
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::ReadOnly)
        .await?;
    let outcome = async {
        let p = scoped_parent(&tx, id, media).await?;
        bounded(render(&tx, p).await?)
    }
    .await;
    finish(tx, outcome).await
}
const CHILD_COLUMNS: &str = "c.id,c.ordinal,c.relative_path,c.source_path,json_extract(c.decision_json,'$.entry_kind'),c.disposition,c.selected,c.reasons_json,c.series_id,c.movie_id,(SELECT min(m.episode_id) FROM downloaded_scan_child_episodes m WHERE m.child_id=c.id),(SELECT count(*) FROM downloaded_scan_child_episodes m WHERE m.child_id=c.id),c.expected_old_movie_file_id,c.destination,c.resolved_mode,c.quality_id,c.revision_json,c.edition,json_extract(c.source_identity_json,'$.size'),json_extract(c.decision_json,'$.observation.state'),c.error_code,c.operation_id";
fn child(r: libsql::Row) -> Result<ScanChild> {
    Ok(ScanChild {
        id: Uuid::parse_str(&r.get::<String>(0)?).map_err(|_| bad())?,
        ordinal: r.get(1)?,
        relative_path: r.get(2)?,
        source_path: r.get(3)?,
        kind: match r.get::<Option<String>>(4)?.as_deref() {
            Some("file") => ScanEntryKind::File,
            Some("directory") => ScanEntryKind::Directory,
            Some("symlink") => ScanEntryKind::Symlink,
            Some("other") => ScanEntryKind::Other,
            _ => return Err(bad()),
        },
        disposition: ScanDisposition::parse(&r.get::<String>(5)?)?,
        selected: r.get::<i64>(6)? == 1,
        reasons: serde_json::from_str(&r.get::<String>(7)?).map_err(|_| bad())?,
        series_id: r.get(8)?,
        movie_id: r.get(9)?,
        anchor_episode_id: r.get(10)?,
        member_count: r.get(11)?,
        expected_old_movie_file_id: r.get(12)?,
        destination: r.get(13)?,
        resolved_mode: match r.get::<Option<String>>(14)?.as_deref() {
            None => None,
            Some("copy") => Some(ScanResolvedMode::Copy),
            Some("move") => Some(ScanResolvedMode::Move),
            Some(_) => return Err(bad()),
        },
        quality_id: r.get(15)?,
        revision: r
            .get::<Option<String>>(16)?
            .map(|v| serde_json::from_str(&v).map_err(|_| bad()))
            .transpose()?,
        edition: r.get(17)?,
        size: r.get(18)?,
        observation: match r.get::<Option<String>>(19)?.as_deref() {
            None => None,
            Some("measured") => Some(ScanObservationState::Measured),
            Some("unavailable") => Some(ScanObservationState::Unavailable),
            Some(_) => return Err(bad()),
        },
        error_code: r.get(20)?,
        operation_id: r
            .get::<Option<String>>(21)?
            .map(|v| Uuid::parse_str(&v).map_err(|_| bad()))
            .transpose()?,
    })
}
async fn children(
    State(db): State<Arc<Database>>,
    media: MediaDomain,
    Path(id): Path<String>,
    q: Q<DownloadedScanChildQuery>,
) -> Result<Json<ApiPage<ScanChild>>> {
    let q = q.map_err(|_| bad())?.0;
    if !(1..=PAGE_MAX).contains(&q.limit) || q.offset > OFFSET_MAX {
        return Err(bad());
    }
    let id = parse_id(&id)?;
    let disposition = q.disposition.map(ScanDisposition::text);
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::ReadOnly)
        .await?;
    let outcome = async {
        scoped_parent(&tx, id, media).await?;
        let key = id.hyphenated().to_string();
        let total = tx
            .query(
                "SELECT count(*) FROM downloaded_scan_children WHERE command_id=? AND (? IS NULL OR disposition=?)",
                params![key.clone(), disposition, disposition],
            )
            .await?
            .next()
            .await?
            .ok_or_else(bad)?
            .get(0)?;
        let mut rows = tx
            .query(
                &format!("SELECT {CHILD_COLUMNS} FROM downloaded_scan_children c WHERE c.command_id=? AND (? IS NULL OR c.disposition=?) ORDER BY c.ordinal LIMIT ? OFFSET ?"),
                params![key, disposition, disposition, i64::from(q.limit), i64::from(q.offset)],
            )
            .await?;
        let mut items = Vec::new();
        while let Some(r) = rows.next().await? {
            items.push(child(r)?);
        }
        bounded(ApiPage {
            items,
            total,
            limit: q.limit,
            offset: q.offset,
        })
    }
    .await;
    finish(tx, outcome).await
}
/// Idempotent stop. `cancel_requested` is monotone and fences publication. Phase A has no
/// effectful child, so every state can settle without an uncertain external effect.
async fn cancel(
    State(db): State<Arc<Database>>,
    media: MediaDomain,
    Path(id): Path<String>,
    q: Q<Empty>,
) -> Result<Json<DownloadedScanCommand>> {
    q.map_err(|_| bad())?;
    let id = parse_id(&id)?;
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        let p = scoped_parent(&tx, id, media).await?;
        let key = id.hyphenated().to_string();
        let timestamp = now()?;
        match (p.status, p.revision) {
            (CommandStatus::Succeeded | CommandStatus::Failed | CommandStatus::Cancelled, _) => {}
            (CommandStatus::Queued | CommandStatus::RetryWait, _) => {
                tx.execute(
                    "UPDATE downloaded_scan_commands SET cancel_requested=1,status='cancelled',completed_at=?,error_code=NULL WHERE id=? AND status IN ('queued','retry_wait')",
                    params![timestamp, key],
                )
                .await?;
            }
            (CommandStatus::Running, 0) => {
                // Discovery is in flight (or abandoned): the publication fence settles it.
                tx.execute(
                    "UPDATE downloaded_scan_commands SET cancel_requested=1 WHERE id=? AND status='running'",
                    [key],
                )
                .await?;
            }
            (CommandStatus::Running, _) => {
                let effectful = tx
                    .query(
                        "SELECT 1 FROM downloaded_scan_children WHERE command_id=? AND (operation_id IS NOT NULL OR disposition='importing') LIMIT 1",
                        [key.clone()],
                    )
                    .await?
                    .next()
                    .await?
                    .is_some();
                if effectful {
                    // Never relabel an armed or uncertain child; only the fence is recorded.
                    tx.execute(
                        "UPDATE downloaded_scan_commands SET cancel_requested=1 WHERE id=? AND status='running'",
                        [key],
                    )
                    .await?;
                } else {
                    tx.execute(
                        "UPDATE downloaded_scan_children SET disposition='cancelled' WHERE command_id=? AND disposition='ready'",
                        [key.clone()],
                    )
                    .await?;
                    tx.execute(
                        "UPDATE downloaded_scan_commands SET cancel_requested=1,status='cancelled',completed_at=?,error_code=NULL WHERE id=? AND status='running'",
                        params![timestamp, key],
                    )
                    .await?;
                }
            }
        }
        let p = scoped_parent(&tx, id, media).await?;
        bounded(render(&tx, p).await?)
    }
    .await;
    finish(tx, outcome).await
}

// ---------------------------------------------------------------------------------------
// Worker: claim, bounded discovery, complete publication
// ---------------------------------------------------------------------------------------

/// A claim is a scheduling generation. Every callback compares id AND run_generation.
pub(super) struct ScanClaim {
    pub(super) id: Uuid,
    media: MediaDomain,
    path: String,
    mode: ScanMode,
    attempts: i64,
    run_generation: i64,
    requested: Option<(String, String)>,
}
pub(super) async fn claim(c: &Connection, id: Uuid, timestamp: i64) -> Result<Option<ScanClaim>> {
    let Some(p) = read_parent(c, id).await? else {
        return Ok(None);
    };
    if !matches!(p.status, CommandStatus::Queued | CommandStatus::RetryWait) {
        return Ok(None);
    }
    let key = id.hyphenated().to_string();
    if p.cancel_requested {
        c.execute(
            "UPDATE downloaded_scan_commands SET status='cancelled',completed_at=?,error_code=NULL WHERE id=? AND status IN ('queued','retry_wait')",
            params![timestamp, key],
        )
        .await?;
        return Ok(None);
    }
    c.execute(
        "UPDATE downloaded_scan_commands SET status='running',attempts=attempts+1,run_generation=run_generation+1,started_at=?1,ready_order_at=max(ready_order_at,?1),error_code=NULL WHERE id=?2 AND status IN ('queued','retry_wait')",
        params![timestamp, key],
    )
    .await?;
    let p = read_parent(c, id).await?.ok_or_else(not_found)?;
    Ok(Some(ScanClaim {
        id,
        media: p.media,
        path: p.path,
        mode: p.mode,
        attempts: p.attempts,
        run_generation: p.run_generation,
        requested: p.provider.zip(p.remote),
    }))
}
/// Only unpublished running parents are reset. A published parent (revision 1) owns durable
/// children and is never touched by startup or per-tick recovery.
pub(super) async fn recover(c: &Connection, timestamp: i64, code: &str) -> Result<()> {
    c.execute(
        "UPDATE downloaded_scan_commands SET status=CASE WHEN cancel_requested=1 THEN 'cancelled' WHEN attempts<3 THEN 'retry_wait' ELSE 'failed' END,next_attempt_at=?1,completed_at=CASE WHEN cancel_requested=1 OR attempts>=3 THEN ?1 ELSE NULL END,error_code=CASE WHEN cancel_requested=1 THEN NULL ELSE ?2 END WHERE status='running' AND inventory_revision=0",
        params![timestamp, code],
    )
    .await?;
    Ok(())
}

enum Failure {
    /// Transient before publication; consumes the parent's three-attempt budget.
    Retry(&'static str),
    Fail(&'static str),
    Cancelled,
    /// Another claim superseded this generation; nothing may be written.
    Stale,
}
impl From<libsql::Error> for Failure {
    fn from(error: libsql::Error) -> Self {
        let message = error.to_string();
        eprintln!(
            "event=downloaded_scan_storage_error kind={:?}",
            std::mem::discriminant(&error)
        );
        if message.contains("capacity reached") {
            Failure::Fail("scan_inventory_limit")
        } else if message.contains("reserved")
            || message.contains("active rescan")
            || message.contains("existing transfer")
            || message.contains("unrelated receipt owner")
        {
            Failure::Retry("scan_target_busy")
        } else {
            Failure::Retry("scan_storage_error")
        }
    }
}
impl From<Error> for Failure {
    fn from(_: Error) -> Self {
        Failure::Retry("scan_storage_error")
    }
}
/// Storage, timeout and cap failures are infrastructure, not per-file decisions. Everything
/// else the evaluator reports is a deterministic, visible rejection of that file.
fn infrastructure(code: &'static str) -> Option<Failure> {
    match code {
        "release_storage_error" | "scan_decision_timeout" => {
            Some(Failure::Retry("scan_storage_error"))
        }
        "scan_facts_limit"
        | "library_match_limit"
        | "episode_match_limit"
        | "scan_file_limit"
        | "scan_proof_limit" => Some(Failure::Fail("scan_decision_limit")),
        "scan_facts_invalid"
        | "scan_proof_invalid"
        | "import_policy_unavailable"
        | "invalid_stored_target"
        | "custom_format_facts_invalid"
        | "release_authority_changed" => Some(Failure::Fail("scan_facts_invalid")),
        _ => None,
    }
}
fn reason(code: &str) -> String {
    let clean: String = code
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(REASON_MAX_BYTES)
        .collect();
    if clean.is_empty() {
        "rejected".into()
    } else {
        clean
    }
}
fn reasons_json(reasons: &[String]) -> String {
    let mut seen = BTreeSet::new();
    let unique: Vec<&String> = reasons
        .iter()
        .filter(|r| seen.insert(r.as_str()))
        .take(REASONS_MAX)
        .collect();
    serde_json::to_string(&unique).unwrap_or_else(|_| "[]".into())
}
fn path_failure(error: &crate::import::Error) -> Failure {
    match error.path_class() {
        Some("denied") => Failure::Fail("scan_source_denied"),
        Some("invalid") => Failure::Fail("scan_source_invalid"),
        Some("changed") | Some("missing") => Failure::Retry("scan_source_changed"),
        Some(_) => Failure::Retry("scan_source_io"),
        None => match error.code() {
            "scan_inventory_limit" => Failure::Fail("scan_inventory_limit"),
            "scan_cancelled" => Failure::Cancelled,
            "scan_observation_timeout" => Failure::Retry("scan_discovery_timeout"),
            _ => Failure::Retry("scan_source_io"),
        },
    }
}

enum Verdict {
    Rejected(Vec<String>),
    AlreadyImported,
    Candidate,
    Selected(usize),
}
struct Item {
    relative_path: String,
    source_path: String,
    kind: Kind,
    identity_json: String,
    dev: u64,
    ino: u64,
    size: u64,
    state: Verdict,
    observed: Option<Observed>,
}
struct Chosen {
    item: usize,
    eligible: ScanPolicyEligible,
    warm: ScanWarmFile,
    authority: ScanAuthority,
}
struct Discovery {
    /// None when the valid source path does not exist.
    envelope: Option<String>,
    folder: Option<String>,
    items: Vec<Item>,
    chosen: Vec<Chosen>,
}
struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
enum Fence {
    Current,
    Cancel,
    Stale,
}
async fn fence(c: &Connection, claim: &ScanClaim) -> Result<Fence> {
    let Some(row) = c
        .query(
            "SELECT status,run_generation,cancel_requested,inventory_revision FROM downloaded_scan_commands WHERE id=?",
            [claim.id.hyphenated().to_string()],
        )
        .await?
        .next()
        .await?
    else {
        return Ok(Fence::Stale);
    };
    if row.get::<String>(0)? != "running"
        || row.get::<i64>(1)? != claim.run_generation
        || row.get::<i64>(3)? != 0
    {
        return Ok(Fence::Stale);
    }
    Ok(if row.get::<i64>(2)? == 1 {
        Fence::Cancel
    } else {
        Fence::Current
    })
}
async fn live(c: &Connection, claim: &ScanClaim) -> std::result::Result<(), Failure> {
    match fence(c, claim).await? {
        Fence::Current => Ok(()),
        Fence::Cancel => Err(Failure::Cancelled),
        Fence::Stale => Err(Failure::Stale),
    }
}
/// A tracked request resolves strictly by provider + domain + remote id. Phase A cannot
/// reserve or hand back a known receipt, and a known receipt must never degrade into an
/// ordinary untracked scan, so it fails visibly. Only an UNKNOWN remote falls back.
async fn resolve_tracking(c: &Connection, claim: &ScanClaim) -> std::result::Result<(), Failure> {
    let Some((provider, remote)) = &claim.requested else {
        return Ok(());
    };
    let known = c
        .query(
            "SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id AND s.media_type=? WHERE p.id=?",
            params![domain(claim.media), provider.clone()],
        )
        .await?
        .next()
        .await?
        .is_some();
    if !known {
        return Err(Failure::Fail("scan_tracked_provider_unavailable"));
    }
    let receipt = c
        .query(
            "SELECT status FROM rss_candidates WHERE client_id=? AND observed_hash=? AND media_type=? LIMIT 1",
            params![provider.clone(), remote.clone(), domain(claim.media)],
        )
        .await?
        .next()
        .await?;
    match receipt {
        None => Ok(()),
        Some(r) if r.get::<String>(0)? == "observed" => {
            Err(Failure::Fail("scan_tracked_receipt_unsupported"))
        }
        Some(_) => Err(Failure::Fail("scan_tracked_receipt_unavailable")),
    }
}
/// Already-imported sources are historical coverage; one scan of history per discovery.
async fn imported_sources(
    c: &Connection,
    path: &str,
) -> std::result::Result<HashSet<String>, Failure> {
    let escaped = path
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    let mut rows = c
        .query(
            "SELECT DISTINCT h.source FROM import_history h JOIN import_journal j ON j.operation_id=h.operation_id WHERE j.phase='complete' AND (h.source=?1 OR h.source LIKE ?2 ESCAPE '\\') LIMIT ?3",
            params![path, format!("{escaped}/%"), HISTORY_MAX_ROWS as i64],
        )
        .await?;
    let mut sources = HashSet::new();
    while let Some(r) = rows.next().await? {
        sources.insert(r.get::<String>(0)?);
    }
    if sources.len() >= HISTORY_MAX_ROWS {
        return Err(Failure::Fail("scan_history_limit"));
    }
    Ok(sources)
}
fn owner_key(raw: &str, tv: bool) -> (String, Option<i64>) {
    let stripped = raw
        .strip_prefix("_UNPACK_")
        .or_else(|| raw.strip_prefix("_FAILED_"))
        .unwrap_or(raw);
    match parser::parse_downloaded(stripped, tv) {
        Ok(p) => (parser::normalize(&p.parsed.title), p.parsed.year),
        Err(_) => (parser::normalize(stripped), None),
    }
}
fn entry_kind_text(kind: Kind) -> &'static str {
    match kind {
        Kind::File => "file",
        Kind::Directory => "directory",
        Kind::Symlink => "symlink",
        Kind::Other => "other",
    }
}

async fn discover(
    db: &Arc<Database>,
    claim: &ScanClaim,
) -> std::result::Result<Discovery, Failure> {
    let cancel = Arc::new(AtomicBool::new(false));
    let _guard = CancelOnDrop(cancel.clone());
    let deadline = Instant::now() + DISCOVERY_BUDGET;
    let c = connection(db).await?;
    live(&c, claim).await?;
    resolve_tracking(&c, claim).await?;
    let path = claim.path.clone();
    let captured = tokio::task::spawn_blocking(move || Inventory::capture(&path))
        .await
        .map_err(|_| Failure::Retry("scan_source_io"))?;
    let inventory = match captured {
        Ok(inventory) => Arc::new(inventory),
        // A valid path that does not exist is a recorded, unsuccessful outcome, not an error.
        Err(e) if e.path_class() == Some("missing") => {
            return Ok(Discovery {
                envelope: None,
                folder: None,
                items: Vec::new(),
                chosen: Vec::new(),
            });
        }
        Err(e) => return Err(path_failure(&e)),
    };
    let envelope = inventory
        .envelope_json(claim.media)
        .map_err(|e| path_failure(&e))?;
    let tv = claim.media == MediaDomain::Tv;
    let history = imported_sources(&c, &claim.path).await?;
    let folder_root = std::path::Path::new(&claim.path)
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_owned);
    let mut folder: Option<String> = None;
    let mut owners: HashMap<(String, Option<i64>), std::result::Result<ScanOwner, &'static str>> =
        HashMap::new();
    let mut items: Vec<Item> = Vec::with_capacity(inventory.entries().len());
    let mut candidates: Vec<(usize, ScanPolicyEligible, ScanWarmFile, ScanAuthority)> = Vec::new();
    for index in 0..inventory.entries().len() {
        if Instant::now() >= deadline {
            return Err(Failure::Retry("scan_discovery_timeout"));
        }
        live(&c, claim).await?;
        let entry = &inventory.entries()[index];
        let source_path = inventory.source_path(index).map_err(|e| path_failure(&e))?;
        let (dev, ino) = entry.physical_key();
        if i64::try_from(dev).is_err() || i64::try_from(ino).is_err() {
            return Err(Failure::Fail("scan_identity_unsupported"));
        }
        let mut item = Item {
            relative_path: entry.relative_path.clone(),
            identity_json: entry.identity_json().map_err(|e| path_failure(&e))?,
            kind: entry.kind,
            source_path,
            dev,
            ino,
            size: entry.size(),
            state: Verdict::Rejected(Vec::new()),
            observed: None,
        };
        if item.kind != Kind::File {
            item.state = Verdict::Rejected(vec![
                match item.kind {
                    Kind::Directory => "directory",
                    Kind::Symlink => "symlink_not_followed",
                    _ => "special_file",
                }
                .into(),
            ]);
            items.push(item);
            continue;
        }
        // A directory source names the release; a file source has no identity-bearing folder.
        let file_source = item.source_path == claim.path;
        if folder.is_none() && !file_source {
            folder = folder_root.clone();
        }
        if history.contains(&item.source_path) {
            item.state = Verdict::AlreadyImported;
            items.push(item);
            continue;
        }
        let basename = item
            .relative_path
            .rsplit('/')
            .next()
            .unwrap_or(&item.relative_path)
            .to_owned();
        let Some((stem, extension)) = basename.rsplit_once('.') else {
            item.state = Verdict::Rejected(vec!["unsupported_file_type".into()]);
            items.push(item);
            continue;
        };
        if !scan_extension_supported(extension, tv) {
            item.state = Verdict::Rejected(vec!["unsupported_file_type".into()]);
            items.push(item);
            continue;
        }
        let folder_name = if file_source {
            None
        } else {
            folder_root.clone()
        };
        let identified = match parser::parse_downloaded(stem, tv) {
            Ok(p) if !p.parsed.title.is_empty() => Ok(stem.to_owned()),
            Ok(_) | Err(_) => folder_name.clone().ok_or("missing_owner_identification"),
        };
        let raw = match identified {
            Ok(raw) => raw,
            Err(code) => {
                item.state = Verdict::Rejected(vec![reason(code)]);
                items.push(item);
                continue;
            }
        };
        let key = owner_key(&raw, tv);
        let owner = match owners.get(&key) {
            Some(cached) => cached.clone(),
            None => {
                let resolved = match resolve_scan_owner(&c, claim.media, &raw).await {
                    Ok(owner) => Ok(owner),
                    Err(SearchError(code)) => match infrastructure(code) {
                        Some(failure) => return Err(failure),
                        None => Err(code),
                    },
                };
                owners.insert(key, resolved.clone());
                resolved
            }
        };
        let owner = match owner {
            Ok(owner) => owner,
            Err(code) => {
                item.state = Verdict::Rejected(vec![reason(code)]);
                items.push(item);
                continue;
            }
        };
        let authority = ScanAuthority::Local { owner };
        let input = ScanFileInput {
            basename: &basename,
            relative_path: &item.relative_path,
            size: item.size,
            source_identity_json: &item.identity_json,
            folder_name: folder_name.as_deref(),
            download_name: None,
        };
        let decided = async {
            let warm = warm_scan_file(&c, &authority, input).await?;
            let snapshot = c
                .transaction_with_behavior(TransactionBehavior::ReadOnly)
                .await
                .map_err(|_| SearchError("release_storage_error"))?;
            let decision = evaluate_scan_file(&snapshot, &authority, input, None, &warm).await;
            let _ = snapshot.rollback().await;
            decision.map(|d| (d, warm))
        }
        .await;
        let (decision, warm) = match decided {
            Ok(v) => v,
            Err(SearchError(code)) => match infrastructure(code) {
                Some(failure) => return Err(failure),
                None => {
                    item.state = Verdict::Rejected(vec![reason(code)]);
                    items.push(item);
                    continue;
                }
            },
        };
        let eligible = match decision {
            ScanFileDecision::Rejected { reasons } => {
                item.state = Verdict::Rejected(reasons.iter().map(|r| reason(r)).collect());
                items.push(item);
                continue;
            }
            ScanFileDecision::NeedsObservation { .. } => {
                item.state = Verdict::Rejected(vec!["quality_observation_required".into()]);
                items.push(item);
                continue;
            }
            ScanFileDecision::PolicyEligible(eligible) => eligible,
        };
        let observer = inventory.clone();
        let flag = cancel.clone();
        let (eligible, observed) = tokio::task::spawn_blocking(move || {
            let observed = scan::observe(&observer, index, &eligible, deadline, &flag);
            (eligible, observed)
        })
        .await
        .map_err(|_| Failure::Retry("scan_source_io"))?;
        let observed = observed.map_err(|e| path_failure(&e))?;
        if !observed.reasons.is_empty() {
            item.state = Verdict::Rejected(observed.reasons.iter().map(|r| reason(r)).collect());
            item.observed = Some(observed);
            items.push(item);
            continue;
        }
        item.state = Verdict::Candidate;
        item.observed = Some(observed);
        candidates.push((items.len(), eligible, warm, authority));
        items.push(item);
    }
    live(&c, claim).await?;
    // The complete inventory must still be what was observed before anything is published.
    let verify = inventory.clone();
    tokio::task::spawn_blocking(move || verify.verify_before_publish())
        .await
        .map_err(|_| Failure::Retry("scan_source_io"))?
        .map_err(|e| path_failure(&e))?;

    // Rank deterministically; physical aliases of one inode keep only the best candidate.
    let mut order: Vec<usize> = (0..candidates.len()).collect();
    order.sort_by(|a, b| {
        candidates[*b]
            .1
            .preference
            .cmp(&candidates[*a].1.preference)
    });
    let mut seen: HashSet<(u64, u64)> = HashSet::new();
    let mut dropped: HashSet<usize> = HashSet::new();
    for k in order {
        let item = &items[candidates[k].0];
        if !seen.insert((item.dev, item.ino)) {
            dropped.insert(k);
        }
    }
    let mut kept = Vec::new();
    for (k, candidate) in candidates.into_iter().enumerate() {
        if dropped.contains(&k) {
            items[candidate.0].state = Verdict::Rejected(vec!["duplicate_source_identity".into()]);
        } else {
            kept.push(candidate);
        }
    }
    let mut eligibles = Vec::with_capacity(kept.len());
    let mut meta = Vec::with_capacity(kept.len());
    for (item, eligible, warm, authority) in kept {
        eligibles.push(eligible);
        meta.push((item, warm, authority));
    }
    let selected: BTreeSet<usize> = select_scan_files(&eligibles)
        .map_err(|_| Failure::Fail("scan_file_limit"))?
        .into_iter()
        .collect();
    let mut chosen = Vec::new();
    for (k, (eligible, (item, warm, authority))) in eligibles.into_iter().zip(meta).enumerate() {
        if selected.contains(&k) {
            chosen.push(Chosen {
                item,
                eligible,
                warm,
                authority,
            });
        } else {
            items[item].state = Verdict::Rejected(vec!["superseded_by_better_file".into()]);
        }
    }
    // The published inventory binds one parent to exactly one library owner. A folder that
    // names several owners is ambiguous authority, not something to import partially.
    let owners_selected: BTreeSet<i64> = chosen
        .iter()
        .map(|c| match c.eligible.proof.owner {
            ScanOwner::Tv { series_id } => series_id,
            ScanOwner::Movies { movie_id } => movie_id,
        })
        .collect();
    if owners_selected.len() > 1 {
        return Err(Failure::Fail("scan_multiple_owners"));
    }
    chosen.sort_by_key(|c| c.item);
    for (k, c) in chosen.iter().enumerate() {
        items[c.item].state = Verdict::Selected(k);
    }
    Ok(Discovery {
        envelope: Some(envelope),
        folder,
        items,
        chosen,
    })
}

struct Published {
    children: usize,
    selected: usize,
}
fn decision_json(
    kind: Kind,
    result: &str,
    observed: Option<&Observed>,
    chosen: Option<&Chosen>,
) -> std::result::Result<String, Failure> {
    let mut value =
        serde_json::json!({"version":1,"entry_kind":entry_kind_text(kind),"result":result});
    let object = value
        .as_object_mut()
        .ok_or(Failure::Fail("scan_decision_limit"))?;
    if let Some(observed) = observed {
        object.insert(
            "observation".into(),
            serde_json::to_value(&observed.media)
                .map_err(|_| Failure::Fail("scan_decision_limit"))?,
        );
        object.insert(
            "space".into(),
            serde_json::to_value(&observed.space)
                .map_err(|_| Failure::Fail("scan_decision_limit"))?,
        );
    }
    if let Some(chosen) = chosen {
        let e = &chosen.eligible;
        object.insert(
            "owner".into(),
            serde_json::to_value(e.proof.owner)
                .map_err(|_| Failure::Fail("scan_decision_limit"))?,
        );
        object.insert(
            "quality".into(),
            serde_json::json!({"quality_id":e.quality.quality_id,"rank":e.quality.rank,"score":e.quality.score}),
        );
        object.insert(
            "members".into(),
            serde_json::json!(e.members.iter().map(|m| m.episode_id).collect::<Vec<_>>()),
        );
        object.insert(
            "proof".into(),
            serde_json::to_value(&e.proof).map_err(|_| Failure::Fail("scan_decision_limit"))?,
        );
    }
    let text = serde_json::to_string(&value).map_err(|_| Failure::Fail("scan_decision_limit"))?;
    if text.len() > DECISION_MAX_BYTES {
        return Err(Failure::Fail("scan_decision_limit"));
    }
    Ok(text)
}
fn resolved_mode(mode: ScanMode) -> &'static str {
    // Untracked auto moves; tracked move capability is a phase B fact, so unknown never moves.
    match mode {
        ScanMode::Auto | ScanMode::Move => "move",
        ScanMode::Copy => "copy",
    }
}

/// Publishes the complete inventory or nothing. All reads that decide selection are repeated
/// in this transaction (proof equality), and the parent fence is checked under the writer lock.
async fn publish(
    db: &Arc<Database>,
    claim: &ScanClaim,
    discovery: Discovery,
) -> std::result::Result<Published, Failure> {
    let c = connection(db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let result = publish_in(&tx, claim, discovery).await;
    match result {
        Ok(published) => {
            tx.commit().await?;
            Ok(published)
        }
        Err(failure) => {
            tx.rollback().await?;
            Err(failure)
        }
    }
}
async fn publish_in(
    tx: &Connection,
    claim: &ScanClaim,
    discovery: Discovery,
) -> std::result::Result<Published, Failure> {
    live(tx, claim).await?;
    let command = claim.id.hyphenated().to_string();
    let timestamp = now()?;
    let mut destinations: HashSet<String> = HashSet::new();
    let mut selected = 0usize;
    let tv = claim.media == MediaDomain::Tv;
    for ordinal in 0..discovery.items.len() {
        let child = Uuid::new_v4().hyphenated().to_string();
        let item = &discovery.items[ordinal];
        let mut reasons: Vec<String> = Vec::new();
        let mut disposition = "rejected";
        let mut wanted: Option<usize> = None;
        match &item.state {
            Verdict::Rejected(r) => reasons = r.clone(),
            Verdict::AlreadyImported => {
                disposition = "already_imported";
                reasons = vec!["already_imported".into()];
            }
            // A candidate that never reached selection cannot be published as importable.
            Verdict::Candidate => return Err(Failure::Fail("scan_facts_invalid")),
            Verdict::Selected(k) => wanted = Some(*k),
        }
        // Selected: reread every fact and require the exact proof, then resolve the destination.
        let mut destination: Option<String> = None;
        let mut selection = None;
        if let Some(k) = wanted {
            let chosen = &discovery.chosen[k];
            let basename = item
                .relative_path
                .rsplit('/')
                .next()
                .unwrap_or(&item.relative_path);
            let input = ScanFileInput {
                basename,
                relative_path: &item.relative_path,
                size: item.size,
                source_identity_json: &item.identity_json,
                folder_name: discovery
                    .folder
                    .as_deref()
                    .filter(|_| item.source_path != claim.path),
                download_name: None,
            };
            match evaluate_scan_file(
                tx,
                &chosen.authority,
                input,
                Some(&chosen.eligible.proof),
                &chosen.warm,
            )
            .await
            {
                Ok(ScanFileDecision::PolicyEligible(_)) => {}
                Ok(_) | Err(SearchError("scan_preflight_changed")) => {
                    return Err(Failure::Retry("scan_facts_changed"));
                }
                Err(SearchError(code)) => {
                    return Err(
                        infrastructure(code).unwrap_or(Failure::Retry("scan_facts_changed"))
                    );
                }
            }
            let e = &chosen.eligible;
            let resolved = if tv {
                let ids: Vec<i64> = e.members.iter().map(|m| m.episode_id).collect();
                crate::naming::destination::resolve_episode_set_destination(
                    tx,
                    &ids,
                    &e.root,
                    basename,
                    e.quality.quality_id,
                    &e.revision.effective,
                    &e.evidence,
                )
                .await
                .map(|d| d.destination)
            } else {
                let ReleaseTarget::Movies { movie_id } = e.target else {
                    return Err(Failure::Fail("scan_facts_invalid"));
                };
                crate::naming::destination::resolve_owned_destination(
                    tx,
                    &crate::db::MediaTarget::Movie(movie_id),
                    &e.root,
                    basename,
                    e.quality.quality_id,
                    &e.revision.effective,
                    e.evidence.edition.as_deref(),
                    &e.evidence,
                )
                .await
            };
            match resolved {
                Ok(path) if destinations.contains(&path) => {
                    reasons = vec!["duplicate_destination".into()];
                }
                Ok(path) if path.len() > PATH_MAX_BYTES => {
                    reasons = vec!["destination_too_long".into()];
                }
                Ok(path) => {
                    destinations.insert(path.clone());
                    destination = Some(path);
                    selection = Some(k);
                }
                Err(crate::naming::destination::DestinationError::Render(failure)) => {
                    reasons = vec![reason(failure.class)];
                }
                Err(crate::naming::destination::DestinationError::Db(error)) => {
                    return Err(error.into());
                }
                Err(crate::naming::destination::DestinationError::Internal(_)) => {
                    return Err(Failure::Fail("scan_facts_invalid"));
                }
            }
        }
        let chosen = selection.map(|k| &discovery.chosen[k]);
        if chosen.is_some() {
            disposition = "ready";
        }
        let reasons = reasons_json(&reasons);
        let result = if chosen.is_some() {
            "selected"
        } else {
            disposition
        };
        let decision = decision_json(item.kind, result, item.observed.as_ref(), chosen)?;
        let (series_id, movie_id, old_movie, quality, revision, edition) = match chosen {
            Some(chosen) => {
                let e = &chosen.eligible;
                (
                    match e.proof.owner {
                        ScanOwner::Tv { series_id } => Some(series_id),
                        ScanOwner::Movies { .. } => None,
                    },
                    match e.proof.owner {
                        ScanOwner::Movies { movie_id } => Some(movie_id),
                        ScanOwner::Tv { .. } => None,
                    },
                    if tv {
                        None
                    } else {
                        e.quality.originals.first().map(|o| o.file_id)
                    },
                    Some(e.quality.quality_id),
                    Some(
                        serde_json::to_string(&e.revision.effective)
                            .map_err(|_| Failure::Fail("scan_facts_invalid"))?,
                    ),
                    if tv { None } else { e.evidence.edition.clone() },
                )
            }
            None => (None, None, None, None, None, None),
        };
        tx.execute(
            "INSERT INTO downloaded_scan_children(id,command_id,ordinal,relative_path,source_path,source_identity_json,source_dev,source_ino,decision_json,selected,series_id,movie_id,expected_old_movie_file_id,destination,resolved_mode,quality_id,revision_json,edition,reasons_json,disposition) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            params![
                child.clone(),
                command.clone(),
                ordinal as i64,
                item.relative_path.clone(),
                item.source_path.clone(),
                item.identity_json.clone(),
                item.dev.to_string(),
                item.ino.to_string(),
                decision,
                i64::from(chosen.is_some()),
                series_id,
                movie_id,
                old_movie,
                destination,
                chosen.map(|_| resolved_mode(claim.mode)),
                quality,
                revision,
                edition,
                reasons,
                disposition
            ],
        )
        .await?;
        if let Some(chosen) = chosen {
            selected += 1;
            if let ScanOwner::Tv { series_id } = chosen.eligible.proof.owner {
                for member in &chosen.eligible.members {
                    tx.execute(
                        "INSERT INTO downloaded_scan_child_episodes(child_id,command_id,episode_id,series_id,expected_old_episode_file_id) VALUES(?,?,?,?,?)",
                        params![child.clone(), command.clone(), member.episode_id, series_id, member.old_file_id],
                    )
                    .await?;
                }
            }
        }
        if let Some(observed) = &item.observed {
            let child_id =
                Uuid::parse_str(&child).map_err(|_| Failure::Fail("scan_facts_invalid"))?;
            observed
                .persist_media(tx, child_id, claim.id)
                .await
                .map_err(|_| Failure::Fail("scan_decision_limit"))?;
        }
    }
    let children = discovery.items.len();
    let inventory_json = match &discovery.envelope {
        Some(envelope) => envelope.clone(),
        None => serde_json::json!({"version":1,"missing":true}).to_string(),
    };
    let measured: i64 = tx
        .query(
            "SELECT bytes FROM downloaded_scan_inventory_size WHERE id=?",
            [command.clone()],
        )
        .await?
        .next()
        .await?
        .ok_or(Failure::Retry("scan_storage_error"))?
        .get(0)?;
    let bytes = measured + inventory_json.len() as i64;
    if bytes > INVENTORY_MAX_BYTES {
        return Err(Failure::Fail("scan_inventory_limit"));
    }
    let changed = tx
        .execute(
            "UPDATE downloaded_scan_commands SET inventory_revision=1,inventory_json=?,inventory_count=?,inventory_bytes=? WHERE id=? AND status='running' AND run_generation=? AND cancel_requested=0 AND inventory_revision=0",
            params![inventory_json, children as i64, bytes, command.clone(), claim.run_generation],
        )
        .await?;
    if changed != 1 {
        return Err(Failure::Stale);
    }
    if selected == 0 {
        // Completed evaluation with nothing to import is a successful, unsuccessful-outcome run.
        tx.execute(
            "UPDATE downloaded_scan_commands SET status='succeeded',outcome='unsuccessful',completed_at=? WHERE id=? AND status='running' AND run_generation=?",
            params![timestamp, command, claim.run_generation],
        )
        .await?;
    }
    Ok(Published { children, selected })
}

async fn settle(db: &Arc<Database>, claim: &ScanClaim, failure: Failure) -> Result<()> {
    let c = connection(db).await?;
    let timestamp = now()?;
    let key = claim.id.hyphenated().to_string();
    let guard = "WHERE id=? AND status='running' AND run_generation=? AND inventory_revision=0";
    match failure {
        Failure::Stale => {}
        Failure::Cancelled => {
            c.execute(
                &format!("UPDATE downloaded_scan_commands SET status='cancelled',completed_at=?,error_code=NULL {guard} AND cancel_requested=1"),
                params![timestamp, key, claim.run_generation],
            )
            .await?;
        }
        Failure::Retry(code) if claim.attempts < MAX_ATTEMPTS => {
            let jitter = i64::from(Uuid::new_v4().as_bytes()[0] % 3);
            let delay = (1i64 << claim.attempts) + jitter;
            c.execute(
                &format!("UPDATE downloaded_scan_commands SET status='retry_wait',next_attempt_at=?,error_code=? {guard}"),
                params![timestamp + delay, code, key, claim.run_generation],
            )
            .await?;
        }
        Failure::Retry(code) | Failure::Fail(code) => {
            c.execute(
                &format!("UPDATE downloaded_scan_commands SET status='failed',completed_at=?,error_code=? {guard}"),
                params![timestamp, code, key, claim.run_generation],
            )
            .await?;
        }
    }
    Ok(())
}
pub(super) async fn run(db: &Arc<Database>, claim: ScanClaim) -> Result<()> {
    let found = match tokio::time::timeout(DISCOVERY_DEADLINE, discover(db, &claim)).await {
        Ok(found) => found,
        Err(_) => Err(Failure::Retry("scan_discovery_timeout")),
    };
    let outcome = match found {
        Ok(discovery) => publish(db, &claim, discovery).await,
        Err(failure) => Err(failure),
    };
    match outcome {
        Ok(published) => {
            eprintln!(
                "event=downloaded_scan_published command_id={} media_type={} children={} selected={}",
                claim.id,
                domain(claim.media),
                published.children,
                published.selected
            );
            Ok(())
        }
        Err(failure) => {
            let code = match &failure {
                Failure::Retry(c) | Failure::Fail(c) => c,
                Failure::Cancelled => "cancelled",
                Failure::Stale => "stale_generation",
            };
            eprintln!(
                "event=downloaded_scan_unpublished command_id={} media_type={} code={}",
                claim.id,
                domain(claim.media),
                code
            );
            settle(db, &claim, failure).await
        }
    }
}

#[cfg(test)]
mod tests;
