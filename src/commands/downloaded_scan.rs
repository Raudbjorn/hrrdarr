//! Durable downloaded-folder scan commands: phase A bounded read-only discovery, then the
//! phase B import continuation in `continuation.rs`.
//!
//! One `POST` names exactly one absolute file or folder. The worker captures an anchored,
//! bounded inventory on a blocking thread, evaluates and observes every candidate, then
//! publishes the COMPLETE inventory (children, episode membership, media observations and the
//! revision 0 -> 1 transition) in one immediate transaction fenced by `run_generation` and
//! `cancel_requested`. Nothing is truncated: any cap, permission, identity or storage failure
//! leaves revision 0 and no child rows. Discovery performs no transfer; a parent that published
//! selected children stays `running` and the worker imports its ready children one at a time
//! (see `continuation.rs`). A parent with nothing to import settles `succeeded` with outcome
//! `unsuccessful`. See docs/downloaded-scan.md.
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
    // Explicit coverage-aware successor of a settled scan whose tracked receipt kept incomplete
    // coverage. Requires the same domain, path and tracked identity; never reopens that scan.
    #[serde(default)]
    #[ts(optional)]
    pub supersedes_command_id: Option<Uuid>,
}
// Derived from status and inventory revision; never grants execution permission.
#[derive(Clone, Copy, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ScanPhase {
    Queued,
    Inventory,
    // A captured tracked receipt's linked singleton import is still finishing under its own
    // authority; discovery publishes only after that predecessor completes.
    WaitingPredecessor,
    AwaitingImport,
    Importing,
    // A failed child holds an unresolved operation, or directory cleanup failed after its
    // removal intent; only an explicit child resume or cleanup resume continues it.
    RecoveryRequired,
    // Move-mode source-directory cleanup is checkpointed (observed or remove_intent).
    Cleanup,
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
    // The exact provider/domain/remote receipt was captured and reserved by this parent.
    Receipt,
}
#[derive(Clone, Copy, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ScanCoverageState {
    Untracked,
    Pending,
    Complete,
    Incomplete,
}
/// Typed expected coverage of a captured receipt: its episodes (TV) or its movie.
#[derive(Serialize, ts_rs::TS)]
pub struct ScanCoverage {
    pub state: ScanCoverageState,
    pub expected: i64,
    pub covered: i64,
    pub missing: i64,
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ScanCleanupState {
    NotNeeded,
    Pending,
    Complete,
    Retained,
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ScanCleanupPhase {
    Observed,
    RemoveIntent,
    Settled,
}
#[derive(Serialize, ts_rs::TS)]
pub struct ScanCleanup {
    pub state: ScanCleanupState,
    pub phase: Option<ScanCleanupPhase>,
    // Static retained reason (`unimported_media`, `unknown_leftovers`, ...); null otherwise.
    pub reason: Option<String>,
    // Directories removed by the cleanup checkpoints so far.
    pub removed: i64,
    // Static code of the last uncertain post-intent failure, if any.
    pub error_code: Option<String>,
    pub resume_generation: i64,
    pub resume_requested: bool,
    // Removal intent is recorded but its effect is unknown: needs an explicit cleanup resume.
    pub recovery_required: bool,
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
    // Failed children whose operation is not complete; they keep the parent running.
    pub recovery_required: i64,
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
    // Captured tracked receipt (download processing receipt id), once reserved.
    pub receipt_id: Option<Uuid>,
    // Linked singleton import of that receipt, captured at reservation; read, never re-owned.
    pub predecessor_operation_id: Option<Uuid>,
    pub coverage: ScanCoverage,
    pub cleanup: ScanCleanup,
    // The settled scan this request explicitly succeeds (`supersedes_command_id` input).
    pub supersedes_command_id: Option<Uuid>,
    // Newest explicit successor naming this scan, whatever its state.
    pub successor_command_id: Option<Uuid>,
    // A "scan remaining files" successor may currently be admitted for this scan: it settled
    // with incomplete tracked coverage, has no unresolved effect and no successor holds its
    // lineage. Receipt/provider validity is re-checked at admission; this grants nothing.
    pub successor_allowed: bool,
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
    // Journal phase of the linked operation; never grants execution permission.
    pub operation_phase: Option<String>,
    pub path_failure_code: Option<String>,
    pub resume_generation: i64,
    pub resume_requested: bool,
    // Failed with an unresolved operation and no active execution: needs an explicit resume.
    pub recovery_required: bool,
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
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ScanMemberQuery {
    #[serde(default = "page_default")]
    #[ts(as = "Option<u16>", optional)]
    pub limit: u16,
    #[serde(default)]
    #[ts(as = "Option<u32>", optional)]
    pub offset: u32,
}
/// One episode served by a TV child's single physical file. Movie children have no members.
#[derive(Serialize, ts_rs::TS)]
pub struct ScanChildMember {
    pub episode_id: i64,
    pub series_id: i64,
    pub season_number: i64,
    pub episode_number: i64,
    // The episode's file when the inventory was published; null when it had none.
    pub expected_old_episode_file_id: Option<i64>,
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
            "/api/v1/tv/downloaded-scan-commands/{id}/children/{child_id}/resume",
            post(continuation::resume_tv),
        )
        .route(
            "/api/v1/tv/downloaded-scan-commands/{id}/children/{child_id}/members",
            get(members_tv),
        )
        .route(
            "/api/v1/tv/downloaded-scan-commands/{id}/resume-cleanup",
            post(cleanup::resume_tv),
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
        .route(
            "/api/v1/movies/downloaded-scan-commands/{id}/children/{child_id}/resume",
            post(continuation::resume_movies),
        )
        .route(
            "/api/v1/movies/downloaded-scan-commands/{id}/children/{child_id}/members",
            get(members_movies),
        )
        .route(
            "/api/v1/movies/downloaded-scan-commands/{id}/resume-cleanup",
            post(cleanup::resume_movies),
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
    supersedes: Option<String>,
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
            supersedes: input
                .supersedes_command_id
                .map(|id| id.hyphenated().to_string()),
        })
    }
}

/// The last column mirrors the successor admission's own-state checks (0054
/// `downloaded_scan_admit`, 0058 `scan_successor_unique`); receipt and provider validity are
/// re-checked only at admission, so `successor_allowed` never grants authority.
const COLUMNS: &str = "id,media_type,source_path,requested_mode,priority,requested_provider_id,requested_remote_id,candidate_id,status,outcome,error_code,attempts,run_generation,cancel_requested,inventory_revision,CASE WHEN json_type(inventory_json,'$.missing')='true' THEN 1 ELSE 0 END,next_attempt_at,created_at,started_at,completed_at,predecessor_operation_id,coverage_state,cleanup_state,json_extract(cleanup_json,'$.phase'),CASE WHEN length(CAST(json_extract(cleanup_json,'$.reason') AS BLOB))<=64 THEN json_extract(cleanup_json,'$.reason') END,coalesce(json_extract(cleanup_json,'$.removed'),0),CASE WHEN length(CAST(json_extract(cleanup_json,'$.error') AS BLOB))<=64 THEN json_extract(cleanup_json,'$.error') END,cleanup_resume_generation,cleanup_resume_requested,predecessor_operation_id IS NULL OR EXISTS(SELECT 1 FROM import_journal j JOIN import_history h ON h.operation_id=j.operation_id WHERE j.operation_id=downloaded_scan_commands.predecessor_operation_id AND j.phase='complete'),supersedes_command_id,(SELECT s.id FROM downloaded_scan_commands s WHERE s.supersedes_command_id=downloaded_scan_commands.id ORDER BY s.created_at DESC,s.id DESC LIMIT 1),(status IN ('succeeded','failed','cancelled') AND coverage_state='incomplete' AND candidate_id IS NOT NULL AND requested_provider_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM downloaded_scan_reserved_children c WHERE c.command_id=downloaded_scan_commands.id) AND (predecessor_operation_id IS NULL OR EXISTS(SELECT 1 FROM import_journal j JOIN import_history h ON h.operation_id=j.operation_id WHERE j.operation_id=downloaded_scan_commands.predecessor_operation_id AND j.phase='complete')) AND NOT EXISTS(SELECT 1 FROM downloaded_scan_commands s WHERE s.supersedes_command_id=downloaded_scan_commands.id AND (s.status IN ('queued','running','retry_wait') OR s.candidate_id IS NOT NULL)))";
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
    candidate: Option<String>,
    predecessor: Option<String>,
    coverage_state: String,
    cleanup_state: String,
    cleanup_phase: Option<String>,
    cleanup_reason: Option<String>,
    cleanup_removed: i64,
    cleanup_error: Option<String>,
    cleanup_resume_generation: i64,
    cleanup_resume_requested: bool,
    predecessor_complete: bool,
    supersedes: Option<String>,
    successor: Option<String>,
    successor_allowed: bool,
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
        candidate: r.get(7)?,
        predecessor: r.get(20)?,
        coverage_state: r.get(21)?,
        cleanup_state: r.get(22)?,
        cleanup_phase: r.get(23)?,
        cleanup_reason: r.get(24)?,
        cleanup_removed: r.get(25)?,
        cleanup_error: r.get(26)?,
        cleanup_resume_generation: r.get(27)?,
        cleanup_resume_requested: r.get::<i64>(28)? == 1,
        predecessor_complete: r.get::<i64>(29)? == 1,
        supersedes: r.get(30)?,
        successor: r.get(31)?,
        successor_allowed: r.get::<i64>(32)? == 1,
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
        recovery_required: 0,
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
    drop(rows);
    counts.recovery_required = c
        .query(
            "SELECT count(*) FROM downloaded_scan_children c JOIN import_journal j ON j.operation_id=c.operation_id WHERE c.command_id=? AND c.disposition='failed' AND j.phase!='complete'",
            [id.hyphenated().to_string()],
        )
        .await?
        .next()
        .await?
        .ok_or_else(bad)?
        .get(0)?;
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
    let (expected, covered): (i64, i64) = {
        let row = c
            .query(
                "SELECT count(*),count(credited_operation_id) FROM downloaded_scan_coverage WHERE command_id=?",
                [p.id.hyphenated().to_string()],
            )
            .await?
            .next()
            .await?
            .ok_or_else(bad)?;
        (row.get(0)?, row.get(1)?)
    };
    let cleanup_phase = match p.cleanup_phase.as_deref() {
        None => None,
        Some("observed") => Some(ScanCleanupPhase::Observed),
        Some("remove_intent") => Some(ScanCleanupPhase::RemoveIntent),
        Some("settled") => Some(ScanCleanupPhase::Settled),
        Some(_) => return Err(bad()),
    };
    let cleanup_state = match p.cleanup_state.as_str() {
        "not_needed" => ScanCleanupState::NotNeeded,
        "pending" => ScanCleanupState::Pending,
        "complete" => ScanCleanupState::Complete,
        "retained" => ScanCleanupState::Retained,
        _ => return Err(bad()),
    };
    let cleanup_recovery = cleanup_state == ScanCleanupState::Pending
        && cleanup_phase == Some(ScanCleanupPhase::RemoveIntent)
        && !p.cleanup_resume_requested;
    let uuid = |v: &Option<String>| {
        v.as_deref()
            .map(|v| Uuid::parse_str(v).map_err(|_| bad()))
            .transpose()
    };
    let receipt_id = uuid(&p.candidate)?;
    let predecessor_operation_id = uuid(&p.predecessor)?;
    let supersedes_command_id = uuid(&p.supersedes)?;
    let successor_command_id = uuid(&p.successor)?;
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
        tracking: match (&tracked, receipt_id, published) {
            (None, _, _) => ScanTracking::Untracked,
            (Some(_), Some(_), _) => ScanTracking::Receipt,
            (Some(_), None, false) => ScanTracking::Pending,
            (Some(_), None, true) => ScanTracking::UnknownFallback,
        },
        tracked,
        phase: match (p.status, p.revision) {
            (CommandStatus::Queued | CommandStatus::RetryWait, _) => ScanPhase::Queued,
            (CommandStatus::Running, 0) if receipt_id.is_some() && !p.predecessor_complete => {
                ScanPhase::WaitingPredecessor
            }
            (CommandStatus::Running, 0) => ScanPhase::Inventory,
            (CommandStatus::Running, _) if counts.importing > 0 => ScanPhase::Importing,
            (CommandStatus::Running, _) if counts.ready == 0 && counts.recovery_required > 0 => {
                ScanPhase::RecoveryRequired
            }
            (CommandStatus::Running, _) if cleanup_recovery => ScanPhase::RecoveryRequired,
            (CommandStatus::Running, _) if cleanup_state == ScanCleanupState::Pending => {
                ScanPhase::Cleanup
            }
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
        receipt_id,
        predecessor_operation_id,
        coverage: ScanCoverage {
            state: match p.coverage_state.as_str() {
                "untracked" => ScanCoverageState::Untracked,
                "pending" => ScanCoverageState::Pending,
                "complete" => ScanCoverageState::Complete,
                "incomplete" => ScanCoverageState::Incomplete,
                _ => return Err(bad()),
            },
            expected,
            covered,
            missing: expected - covered,
        },
        cleanup: ScanCleanup {
            state: cleanup_state,
            phase: cleanup_phase,
            reason: p.cleanup_reason,
            removed: p.cleanup_removed,
            error_code: p.cleanup_error,
            resume_generation: p.cleanup_resume_generation,
            resume_requested: p.cleanup_resume_requested,
            recovery_required: cleanup_recovery,
        },
        supersedes_command_id,
        successor_command_id,
        successor_allowed: p.successor_allowed,
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
                && existing.remote == request.remote
                && existing.supersedes == request.supersedes;
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
        if let Some(previous) = &request.supersedes {
            // Explicit lineage is validated and reserved under this same writer lock.
            successor_admission(&tx, &request, previous).await?;
        }
        if command_capacity(&tx).await? >= MAX_COMMANDS {
            return Err(Error(StatusCode::TOO_MANY_REQUESTS, "command_history_full"));
        }
        let timestamp = now()?;
        tx.execute(
            "INSERT INTO downloaded_scan_commands(id,media_type,source_path,requested_mode,requested_provider_id,requested_remote_id,priority,ready_order_at,next_attempt_at,created_at,supersedes_command_id) VALUES(?,?,?,?,?,?,?,?,?,?,?)",
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
                timestamp,
                request.supersedes.clone()
            ],
        )
        .await?;
        if let Some(previous) = &request.supersedes {
            eprintln!(
                "event=downloaded_scan_successor_admitted command_id={} media_type={} supersedes={previous}",
                request.id,
                domain(media)
            );
        }
        let row = read_parent(&tx, id).await?.ok_or_else(not_found)?;
        Ok((StatusCode::ACCEPTED, bounded(render(&tx, row).await?)?))
    }
    .await;
    finish(tx, outcome).await
}
fn successor_error(code: &'static str) -> Error {
    Error(StatusCode::CONFLICT, code)
}
/// Validates an explicit `supersedes_command_id` inside the admission transaction. The
/// predecessor must be a settled scan of this domain and source with retained incomplete
/// tracked coverage and no unresolved effect; the request must name the same tracked receipt;
/// that receipt must still be the current observed authority of an enabled, scoped client; and
/// no other successor may hold the lineage. The 0054/0058 admission triggers repeat the
/// own-state checks, and the receipt capture triggers repeat the authority checks later.
async fn successor_admission(tx: &Connection, request: &Request, previous: &str) -> Result<()> {
    let invalid = || successor_error("downloaded_scan_successor_invalid");
    let row = tx
        .query(
            "SELECT p.media_type,p.source_path,p.requested_provider_id,p.requested_remote_id,p.status,p.coverage_state,p.candidate_id,
             EXISTS(SELECT 1 FROM downloaded_scan_reserved_children c WHERE c.command_id=p.id) OR p.cleanup_state='pending' OR NOT (p.predecessor_operation_id IS NULL OR EXISTS(SELECT 1 FROM import_journal j JOIN import_history h ON h.operation_id=j.operation_id WHERE j.operation_id=p.predecessor_operation_id AND j.phase='complete')),
             EXISTS(SELECT 1 FROM downloaded_scan_commands s WHERE s.supersedes_command_id=p.id AND (s.status IN ('queued','running','retry_wait') OR s.candidate_id IS NOT NULL))
             FROM downloaded_scan_commands p WHERE p.id=?",
            [previous],
        )
        .await?
        .next()
        .await?
        .ok_or_else(invalid)?;
    let media: String = row.get(0)?;
    let path: String = row.get(1)?;
    let provider: Option<String> = row.get(2)?;
    let remote: Option<String> = row.get(3)?;
    let status = CommandStatus::parse(&row.get::<String>(4)?)?;
    let coverage: String = row.get(5)?;
    let candidate: Option<String> = row.get(6)?;
    let unresolved = row.get::<i64>(7)? == 1;
    let held = row.get::<i64>(8)? == 1;
    drop(row);
    // The same typed authority: domain, source path and the exact tracked receipt request.
    if media != domain(request.media)
        || path != request.path
        || request.provider.is_none()
        || provider != request.provider
        || remote != request.remote
    {
        return Err(invalid());
    }
    if !matches!(
        status,
        CommandStatus::Succeeded | CommandStatus::Failed | CommandStatus::Cancelled
    ) || unresolved
    {
        return Err(successor_error("downloaded_scan_successor_not_settled"));
    }
    let Some(candidate) = candidate.filter(|_| coverage == "incomplete") else {
        return Err(successor_error("downloaded_scan_successor_not_incomplete"));
    };
    if held {
        return Err(successor_error("downloaded_scan_successor_conflict"));
    }
    // The receipt must still be the observed authority of an enabled, scoped, unchanged client,
    // and its processing row (if any) must carry no handback or one from this lineage.
    let current = tx
        .query(
            "WITH RECURSIVE ancestry(id,previous,depth) AS (SELECT id,supersedes_command_id,1 FROM downloaded_scan_commands WHERE id=?5 UNION ALL SELECT a.id,a.supersedes_command_id,x.depth+1 FROM downloaded_scan_commands a JOIN ancestry x ON a.id=x.previous WHERE x.depth<1024)
             SELECT EXISTS(SELECT 1 FROM rss_candidates r JOIN providers v ON v.id=r.client_id JOIN provider_scopes s ON s.provider_id=v.id AND s.media_type=r.media_type WHERE r.id=?1 AND r.status='observed' AND r.media_type=?2 AND r.client_id=?3 AND r.observed_hash=?4 AND v.enabled=1 AND v.revision=r.client_revision),
             coalesce((SELECT d.status='checking' OR (d.scan_command_id IS NOT NULL AND d.scan_command_id NOT IN (SELECT id FROM ancestry)) FROM download_processing d WHERE d.candidate_id=?1),0),
             EXISTS(SELECT 1 FROM downloaded_scan_commands s WHERE s.candidate_id=?1 AND s.status IN ('queued','running','retry_wait'))",
            params![
                candidate,
                domain(request.media),
                request.provider.clone(),
                request.remote.clone(),
                previous
            ],
        )
        .await?
        .next()
        .await?
        .ok_or_else(invalid)?;
    if current.get::<i64>(0)? != 1 {
        return Err(successor_error(
            "downloaded_scan_successor_receipt_unavailable",
        ));
    }
    if current.get::<i64>(1)? == 1 || current.get::<i64>(2)? == 1 {
        return Err(successor_error("downloaded_scan_successor_conflict"));
    }
    Ok(())
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
const CHILD_COLUMNS: &str = "c.id,c.ordinal,c.relative_path,c.source_path,json_extract(c.decision_json,'$.entry_kind'),c.disposition,c.selected,c.reasons_json,c.series_id,c.movie_id,(SELECT min(m.episode_id) FROM downloaded_scan_child_episodes m WHERE m.child_id=c.id),(SELECT count(*) FROM downloaded_scan_child_episodes m WHERE m.child_id=c.id),c.expected_old_movie_file_id,c.destination,c.resolved_mode,c.quality_id,c.revision_json,c.edition,json_extract(c.source_identity_json,'$.size'),json_extract(c.decision_json,'$.observation.state'),c.error_code,c.operation_id,c.resume_generation,c.resume_requested,j.phase,CASE WHEN length(CAST(j.path_failure_code AS BLOB))<=64 THEN j.path_failure_code END,j.error_code IS NOT NULL";
/// Child rows always join their (optional) linked journal for the operation projection.
const CHILD_FROM: &str =
    "downloaded_scan_children c LEFT JOIN import_journal j ON j.operation_id=c.operation_id";
fn child(r: libsql::Row) -> Result<ScanChild> {
    let operation_id: Option<String> = r.get(21)?;
    let operation_phase: Option<String> = r.get(24)?;
    let path_failure_code: Option<String> = r.get(25)?;
    let disposition = ScanDisposition::parse(&r.get::<String>(5)?)?;
    let unresolved = r.get::<Option<i64>>(26)? == Some(1) || path_failure_code.is_some();
    let recovery_required = disposition == ScanDisposition::Failed
        && operation_phase
            .as_deref()
            .is_some_and(|phase| phase != "complete")
        && unresolved
        && match operation_id.as_deref() {
            Some(op) => !crate::import::operation_active(op).map_err(|_| bad())?,
            None => false,
        };
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
        disposition,
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
        operation_id: operation_id
            .map(|v| Uuid::parse_str(&v).map_err(|_| bad()))
            .transpose()?,
        operation_phase,
        path_failure_code,
        resume_generation: r.get(22)?,
        resume_requested: r.get::<i64>(23)? == 1,
        recovery_required,
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
                &format!("SELECT {CHILD_COLUMNS} FROM {CHILD_FROM} WHERE c.command_id=? AND (? IS NULL OR c.disposition=?) ORDER BY c.ordinal LIMIT ? OFFSET ?"),
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
async fn members_tv(
    state: State<Arc<Database>>,
    path: Path<(String, String)>,
    q: Q<ScanMemberQuery>,
) -> Result<Json<ApiPage<ScanChildMember>>> {
    members(state, MediaDomain::Tv, path, q).await
}
async fn members_movies(
    state: State<Arc<Database>>,
    path: Path<(String, String)>,
    q: Q<ScanMemberQuery>,
) -> Result<Json<ApiPage<ScanChildMember>>> {
    members(state, MediaDomain::Movies, path, q).await
}
/// Complete paginated membership of one child, ordered by episode ID.
async fn members(
    State(db): State<Arc<Database>>,
    media: MediaDomain,
    Path((id, child_id)): Path<(String, String)>,
    q: Q<ScanMemberQuery>,
) -> Result<Json<ApiPage<ScanChildMember>>> {
    let q = q.map_err(|_| bad())?.0;
    if !(1..=PAGE_MAX).contains(&q.limit) || q.offset > OFFSET_MAX {
        return Err(bad());
    }
    let id = parse_id(&id)?;
    let child_id = parse_id(&child_id)?;
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::ReadOnly)
        .await?;
    let outcome = async {
        scoped_parent(&tx, id, media).await?;
        let (key, child) = (id.hyphenated().to_string(), child_id.hyphenated().to_string());
        if tx
            .query(
                "SELECT 1 FROM downloaded_scan_children WHERE id=? AND command_id=?",
                params![child.clone(), key.clone()],
            )
            .await?
            .next()
            .await?
            .is_none()
        {
            return Err(not_found());
        }
        let total = tx
            .query(
                "SELECT count(*) FROM downloaded_scan_child_episodes WHERE child_id=? AND command_id=?",
                params![child.clone(), key.clone()],
            )
            .await?
            .next()
            .await?
            .ok_or_else(bad)?
            .get(0)?;
        let mut rows = tx
            .query(
                "SELECT m.episode_id,m.series_id,e.season,e.number,m.expected_old_episode_file_id FROM downloaded_scan_child_episodes m JOIN episodes e ON e.id=m.episode_id WHERE m.child_id=? AND m.command_id=? ORDER BY m.episode_id LIMIT ? OFFSET ?",
                params![child, key, i64::from(q.limit), i64::from(q.offset)],
            )
            .await?;
        let mut items = Vec::new();
        while let Some(r) = rows.next().await? {
            items.push(ScanChildMember {
                episode_id: r.get(0)?,
                series_id: r.get(1)?,
                season_number: r.get(2)?,
                episode_number: r.get(3)?,
                expected_old_episode_file_id: r.get(4)?,
            });
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
/// Idempotent stop-after-current. `cancel_requested` is monotone, fences publication and
/// prevents every later child preparation or start. Untouched ready children are cancelled at
/// once; an importing or unresolved child keeps the parent running until it settles.
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
                    &format!("UPDATE downloaded_scan_commands SET cancel_requested=1,status='cancelled',completed_at=?,error_code=NULL,{UNPUBLISHED_COVERAGE} WHERE id=? AND status IN ('queued','retry_wait')"),
                    params![timestamp, key],
                )
                .await?;
            }
            (CommandStatus::Running, 0) => {
                // Discovery is in flight, abandoned or waiting for a captured predecessor: the
                // publication fence settles it; a waiting parent is woken to observe the cancel.
                tx.execute(
                    "UPDATE downloaded_scan_commands SET cancel_requested=1,next_attempt_at=min(next_attempt_at,?) WHERE id=? AND status='running'",
                    params![timestamp, key],
                )
                .await?;
            }
            (CommandStatus::Running, _) => {
                // Record the fence and wake the worker so an in-flight child settles promptly.
                tx.execute(
                    "UPDATE downloaded_scan_commands SET cancel_requested=1,next_attempt_at=? WHERE id=? AND status='running'",
                    params![timestamp, key.clone()],
                )
                .await?;
                continuation::settle_cancelled(&tx, &key, timestamp, None).await?;
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
    /// A published (`inventory_revision` 1) parent: the import continuation, not discovery.
    continuation: bool,
    media: MediaDomain,
    path: String,
    mode: ScanMode,
    attempts: i64,
    run_generation: i64,
    requested: Option<(String, String)>,
    /// Explicit predecessor whose retained incomplete coverage this scan continues.
    supersedes: Option<String>,
}
pub(super) async fn claim(c: &Connection, id: Uuid, timestamp: i64) -> Result<Option<ScanClaim>> {
    let Some(p) = read_parent(c, id).await? else {
        return Ok(None);
    };
    let key = id.hyphenated().to_string();
    if matches!(p.status, CommandStatus::Running) && p.revision == 1 {
        // A continuation is a new scheduling generation, never another attempt.
        c.execute(
            "UPDATE downloaded_scan_commands SET run_generation=run_generation+1,ready_order_at=max(ready_order_at,?1) WHERE id=?2 AND status='running' AND inventory_revision=1",
            params![timestamp, key],
        )
        .await?;
        let p = read_parent(c, id).await?.ok_or_else(not_found)?;
        return Ok(Some(ScanClaim {
            id,
            continuation: true,
            media: p.media,
            path: p.path,
            mode: p.mode,
            attempts: p.attempts,
            run_generation: p.run_generation,
            requested: p.provider.zip(p.remote),
            supersedes: p.supersedes,
        }));
    }
    if matches!(p.status, CommandStatus::Running) && p.revision == 0 && p.candidate.is_some() {
        // A captured receipt waiting for its predecessor (or an abandoned discovery after the
        // capture): a new scheduling generation, never another attempt and never a reset.
        c.execute(
            "UPDATE downloaded_scan_commands SET run_generation=run_generation+1,ready_order_at=max(ready_order_at,?1) WHERE id=?2 AND status='running' AND inventory_revision=0 AND candidate_id IS NOT NULL",
            params![timestamp, key],
        )
        .await?;
        let p = read_parent(c, id).await?.ok_or_else(not_found)?;
        return Ok(Some(ScanClaim {
            id,
            continuation: false,
            media: p.media,
            path: p.path,
            mode: p.mode,
            attempts: p.attempts,
            run_generation: p.run_generation,
            requested: p.provider.zip(p.remote),
            supersedes: p.supersedes,
        }));
    }
    if !matches!(p.status, CommandStatus::Queued | CommandStatus::RetryWait) {
        return Ok(None);
    }
    if p.cancel_requested {
        c.execute(
            &format!("UPDATE downloaded_scan_commands SET status='cancelled',completed_at=?,error_code=NULL,{UNPUBLISHED_COVERAGE} WHERE id=? AND status IN ('queued','retry_wait')"),
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
        continuation: false,
        media: p.media,
        path: p.path,
        mode: p.mode,
        attempts: p.attempts,
        run_generation: p.run_generation,
        requested: p.provider.zip(p.remote),
        supersedes: p.supersedes,
    }))
}
/// Only unpublished, uncaptured running parents are reset. A published parent (revision 1)
/// owns durable children, and a parent that captured a tracked receipt waits for its
/// predecessor; neither is touched by startup or per-tick recovery (the worker re-claims them).
pub(super) async fn recover(c: &Connection, timestamp: i64, code: &str) -> Result<()> {
    c.execute(
        "UPDATE downloaded_scan_commands SET status=CASE WHEN cancel_requested=1 THEN 'cancelled' WHEN attempts<3 THEN 'retry_wait' ELSE 'failed' END,next_attempt_at=?1,completed_at=CASE WHEN cancel_requested=1 OR attempts>=3 THEN ?1 ELSE NULL END,error_code=CASE WHEN cancel_requested=1 THEN NULL ELSE ?2 END WHERE status='running' AND inventory_revision=0 AND candidate_id IS NULL",
        params![timestamp, code],
    )
    .await?;
    Ok(())
}

/// Unpublished parents settle with no credited coverage: a captured receipt is incomplete.
const UNPUBLISHED_COVERAGE: &str =
    "coverage_state=CASE WHEN candidate_id IS NULL THEN 'untracked' ELSE 'incomplete' END";
enum Failure {
    /// Transient before publication; consumes the parent's three-attempt budget.
    Retry(&'static str),
    Fail(&'static str),
    Cancelled,
    /// Another claim superseded this generation; nothing may be written.
    Stale,
    /// The captured receipt's predecessor is still finishing; poll again after this delay
    /// without consuming an attempt.
    Waiting(i64),
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
        } else if message.contains("explicit scan lineage") {
            Failure::Fail("scan_tracked_receipt_reserved")
        } else if message.contains("busy scan receipt authority")
            || message.contains("exact processing fence")
        {
            Failure::Retry("scan_tracked_receipt_busy")
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
    /// The exact tracked receipt this parent captures at publication (or already captured).
    receipt: Option<receipt::Receipt>,
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
    let tracking = receipt::resolve(&c, claim).await?;
    let carried = receipt::carried_members(&c, claim).await?;
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
                receipt: tracking,
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
        // A captured receipt fixes the owner and expected members; the evaluator validates the
        // filename/folder/receipt identities against it instead of resolving a local owner.
        let authority = if let Some(r) = &tracking {
            r.authority(claim.media)
        } else {
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
                Some(cached) => *cached,
                None => {
                    let resolved = match resolve_scan_owner(&c, claim.media, &raw).await {
                        Ok(owner) => Ok(owner),
                        Err(SearchError(code)) => match infrastructure(code) {
                            Some(failure) => return Err(failure),
                            None => Err(code),
                        },
                    };
                    owners.insert(key, resolved);
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
            ScanAuthority::Local { owner }
        };
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
            Err(SearchError(code)) if tracking.is_some() && receipt::receipt_code(code) => {
                return Err(Failure::Fail("scan_tracked_receipt_unavailable"));
            }
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
        if fully_carried(&eligible, &carried) {
            // An explicit predecessor already proved every member of this file against the
            // current association: it is not remaining work for this successor.
            item.state = Verdict::Rejected(vec!["coverage_already_credited".into()]);
            items.push(item);
            continue;
        }
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
        receipt: tracking,
        folder,
        items,
        chosen,
    })
}

/// Whether every typed member of an eligible file is already a carried predecessor credit.
fn fully_carried(eligible: &ScanPolicyEligible, carried: &HashSet<i64>) -> bool {
    if carried.is_empty() {
        return false;
    }
    match eligible.target {
        ReleaseTarget::Movies { movie_id } => carried.contains(&movie_id),
        ReleaseTarget::Tv { .. } => {
            !eligible.members.is_empty()
                && eligible
                    .members
                    .iter()
                    .all(|m| carried.contains(&m.episode_id))
        }
    }
}
/// A publication re-observes its source: measured prepare refusals recorded by EARLIER scans of
/// the same domain and source stop being current Health evidence, so the Health generation is
/// invalidated in this publication transaction when any such evidence exists.
async fn supersede_prepare_evidence(
    tx: &Connection,
    claim: &ScanClaim,
    command: &str,
) -> std::result::Result<(), Failure> {
    let superseded = tx
        .query(
            "SELECT 1 FROM downloaded_scan_children c JOIN downloaded_scan_commands p ON p.id=c.command_id WHERE c.disposition='failed' AND c.operation_id IS NULL AND substr(c.error_code,1,6)='hc021_' AND p.media_type=? AND p.source_path=? AND p.id!=? LIMIT 1",
            params![domain(claim.media), claim.path.clone(), command],
        )
        .await?
        .next()
        .await?
        .is_some();
    if superseded {
        crate::health::path_diagnostics_changed(
            tx,
            claim.media,
            crate::health::PathDiagnosticChange::Completion,
        )
        .await
        .map_err(|_| Failure::Retry("scan_storage_error"))?;
    }
    Ok(())
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
fn resolved_mode(mode: ScanMode, tracked: bool) -> &'static str {
    // Untracked auto moves. A tracked receipt's client move capability is not known here, and
    // unknown capability never implies a move, so tracked auto copies. Explicit modes stay.
    match mode {
        ScanMode::Auto if tracked => "copy",
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
    // Reserve the exact receipt before any child: the child/member triggers treat this
    // parent's own captured receipt as related, every other receipt as a foreign owner.
    if let Some(r) = discovery.receipt.as_ref().filter(|r| !r.captured) {
        receipt::capture(tx, claim, r).await?;
    }
    let tracked = discovery.receipt.is_some();
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
                chosen.map(|_| resolved_mode(claim.mode, tracked)),
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
    if let Some(r) = &discovery.receipt {
        receipt::expect_coverage(tx, &command, claim.media, r).await?;
    }
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
    supersede_prepare_evidence(tx, claim, &command).await?;
    if tracked {
        // A completed predecessor's typed history (and any still-valid credit carried from an
        // explicit predecessor scan) counts before any child imports.
        receipt::credit(tx, &command).await?;
    }
    if selected == 0 {
        // Completed evaluation with nothing to import is a successful, unsuccessful-outcome run;
        // a captured receipt is handed back in this same transaction.
        receipt::finalize(
            tx,
            &command,
            receipt::Terminal::Succeeded,
            Some(claim.run_generation),
            timestamp,
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
        Failure::Waiting(delay) => {
            c.execute(
                &format!("UPDATE downloaded_scan_commands SET next_attempt_at=? {guard}"),
                params![timestamp + delay, key, claim.run_generation],
            )
            .await?;
        }
        Failure::Cancelled => {
            c.execute(
                &format!("UPDATE downloaded_scan_commands SET status='cancelled',completed_at=?,error_code=NULL,{UNPUBLISHED_COVERAGE} {guard} AND cancel_requested=1"),
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
                &format!("UPDATE downloaded_scan_commands SET status='failed',completed_at=?,error_code=?,{UNPUBLISHED_COVERAGE} {guard}"),
                params![timestamp, code, key, claim.run_generation],
            )
            .await?;
        }
    }
    Ok(())
}
pub(super) async fn run(db: &Arc<Database>, claim: ScanClaim) -> Result<()> {
    if claim.continuation {
        return continuation::step(db, claim).await;
    }
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
                Failure::Waiting(_) => "waiting_predecessor",
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

mod cleanup;
mod continuation;
mod receipt;
pub use cleanup::ScanCleanupResumeInput;
pub use continuation::ScanChildResumeInput;

#[cfg(test)]
mod tests;
