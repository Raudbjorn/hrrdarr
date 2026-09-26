//! Durable per-target library rescans. Adopts on-disk files into existing series/movie
//! associations in place: no move/copy/rename, unlike `crate::import::execute`'s transfer
//! pipeline. Mirrors Sonarr/Radarr `DiskScanService.Scan` (`newDownload=false` decisions):
//! delete any DB file record at a path, then add a fresh record at the file's current path.
//!
//! The walk is read-only and runs in `spawn_blocking` with its own deadline well under the
//! worker's step timeout; every DB write happens afterward, in this module's own directly
//! awaited async code, as small per-file immediate transactions. A dropped future (timeout)
//! therefore never leaves a write in flight -- the leaked walk thread only keeps stat()ing
//! disk, and a blind `recover()` (like `quality_reset`) is safe because a partial write
//! sequence is always self-consistent and a retried scan converges on the same outcome.
use super::*;
use crate::search::parser::{self, Numbering};
use rustix::fs::{self, AtFlags, Mode as Permissions, OFlags};
use std::{
    collections::HashSet,
    fs::File,
    path::{Path as FsPath, PathBuf},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum RescanStatus {
    Queued,
    Running,
    RetryWait,
    Succeeded,
    Skipped,
    Failed,
    Cancelled,
}
impl RescanStatus {
    fn text(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::RetryWait => "retry_wait",
            Self::Succeeded => "succeeded",
            Self::Skipped => "skipped",
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
            "skipped" => Ok(Self::Skipped),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(bad()),
        }
    }
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct RescanInput {
    #[ts(optional)]
    pub target_id: Option<i64>,
    pub priority: CommandPriority,
}
#[derive(Clone, Serialize, ts_rs::TS)]
pub struct RescanCommand {
    pub id: Uuid,
    pub media_type: MediaDomain,
    pub series_id: Option<i64>,
    pub movie_id: Option<i64>,
    pub priority: CommandPriority,
    pub status: RescanStatus,
    pub attempts: u8,
    pub next_attempt_at: i64,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub error_code: Option<String>,
    pub skip_reason: Option<String>,
    pub files_adopted: Option<i64>,
    pub files_removed: Option<i64>,
}
#[derive(Serialize, ts_rs::TS)]
pub struct RescanBatch {
    pub commands: Vec<RescanCommand>,
    // Explicit-target requests conflict instead of appearing here; only a whole-library
    // fan-out skips a busy target and reports it, so the rest of the batch still admits.
    pub busy_target_ids: Vec<i64>,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct RescanQuery {
    #[ts(optional)]
    pub status: Option<RescanStatus>,
    #[serde(default = "default_limit")]
    #[ts(as = "Option<u16>", optional)]
    pub limit: u16,
    #[serde(default)]
    #[ts(as = "Option<u32>", optional)]
    pub offset: u32,
}
const COLUMNS: &str = "id,media_type,series_id,movie_id,priority,status,attempts,next_attempt_at,created_at,started_at,completed_at,error_code,skip_reason,files_adopted,files_removed";
fn row(r: libsql::Row) -> Result<RescanCommand> {
    Ok(RescanCommand {
        id: Uuid::parse_str(&r.get::<String>(0)?).map_err(|_| bad())?,
        media_type: MediaDomain::parse(&r.get::<String>(1)?).map_err(|_| bad())?,
        series_id: r.get(2)?,
        movie_id: r.get(3)?,
        priority: if r.get::<i64>(4)? == 1 {
            CommandPriority::High
        } else {
            CommandPriority::Normal
        },
        status: RescanStatus::parse(&r.get::<String>(5)?)?,
        attempts: r.get::<i64>(6)? as u8,
        next_attempt_at: r.get(7)?,
        created_at: r.get(8)?,
        started_at: r.get(9)?,
        completed_at: r.get(10)?,
        error_code: r.get(11)?,
        skip_reason: r.get(12)?,
        files_adopted: r.get(13)?,
        files_removed: r.get(14)?,
    })
}
async fn read(c: &Connection, id: Uuid) -> Result<RescanCommand> {
    row(c
        .query(
            &format!("SELECT {COLUMNS} FROM rescan_commands WHERE id=?"),
            [id.to_string()],
        )
        .await?
        .next()
        .await?
        .ok_or(Error(StatusCode::NOT_FOUND, "rescan_command_not_found"))?)
}
async fn read_scoped(c: &Connection, id: Uuid, media: MediaDomain) -> Result<RescanCommand> {
    row(c
        .query(
            &format!("SELECT {COLUMNS} FROM rescan_commands WHERE id=? AND media_type=?"),
            params![id.to_string(), domain(media)],
        )
        .await?
        .next()
        .await?
        .ok_or(Error(StatusCode::NOT_FOUND, "rescan_command_not_found"))?)
}

pub(super) fn router(db: Arc<Database>) -> Router {
    Router::new()
        .route("/api/v1/tv/rescan-commands", get(list_tv).post(create_tv))
        .route("/api/v1/tv/rescan-commands/{id}", get(detail_tv))
        .route("/api/v1/tv/rescan-commands/{id}/cancel", post(cancel_tv))
        .route(
            "/api/v1/movies/rescan-commands",
            get(list_movies).post(create_movies),
        )
        .route("/api/v1/movies/rescan-commands/{id}", get(detail_movies))
        .route(
            "/api/v1/movies/rescan-commands/{id}/cancel",
            post(cancel_movies),
        )
        .layer(DefaultBodyLimit::max(8192))
        .layer(axum::middleware::from_fn(deadline))
        .with_state(db)
}
async fn deadline(request: axum::extract::Request, next: axum::middleware::Next) -> Response {
    match tokio::time::timeout(std::time::Duration::from_secs(5), next.run(request)).await {
        Ok(response) => response,
        Err(_) => Error(StatusCode::SERVICE_UNAVAILABLE, "rescan_command_timeout").into_response(),
    }
}
async fn create_tv(
    state: State<Arc<Database>>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
    input: std::result::Result<Json<RescanInput>, JsonRejection>,
) -> Result<(StatusCode, Json<RescanBatch>)> {
    create(state, MediaDomain::Tv, q, input).await
}
async fn create_movies(
    state: State<Arc<Database>>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
    input: std::result::Result<Json<RescanInput>, JsonRejection>,
) -> Result<(StatusCode, Json<RescanBatch>)> {
    create(state, MediaDomain::Movies, q, input).await
}
async fn list_tv(
    state: State<Arc<Database>>,
    q: std::result::Result<Query<RescanQuery>, QueryRejection>,
) -> Result<Json<ApiPage<RescanCommand>>> {
    list(state, MediaDomain::Tv, q).await
}
async fn list_movies(
    state: State<Arc<Database>>,
    q: std::result::Result<Query<RescanQuery>, QueryRejection>,
) -> Result<Json<ApiPage<RescanCommand>>> {
    list(state, MediaDomain::Movies, q).await
}
async fn detail_tv(
    state: State<Arc<Database>>,
    path: Path<String>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
) -> Result<Json<RescanCommand>> {
    detail(state, MediaDomain::Tv, path, q).await
}
async fn detail_movies(
    state: State<Arc<Database>>,
    path: Path<String>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
) -> Result<Json<RescanCommand>> {
    detail(state, MediaDomain::Movies, path, q).await
}
async fn cancel_tv(
    state: State<Arc<Database>>,
    path: Path<String>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
) -> Result<Json<RescanCommand>> {
    cancel(state, MediaDomain::Tv, path, q).await
}
async fn cancel_movies(
    state: State<Arc<Database>>,
    path: Path<String>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
) -> Result<Json<RescanCommand>> {
    cancel(state, MediaDomain::Movies, path, q).await
}
fn target_table(media: MediaDomain) -> &'static str {
    match media {
        MediaDomain::Tv => "series",
        MediaDomain::Movies => "movies",
    }
}
// Mirrors migration 0032's `rescan_admit` in-flight predicates exactly, so a busy target is
// reported cleanly instead of raising and rolling back the whole admission transaction.
async fn busy(c: &Connection, media: MediaDomain, id: i64) -> Result<bool> {
    let import_inflight = match media {
        MediaDomain::Tv => c.query("SELECT 1 FROM import_journal j JOIN operations o ON o.id=j.operation_id WHERE j.phase NOT IN ('preview','complete') AND o.media_type='episode' AND EXISTS(SELECT 1 FROM episodes e WHERE e.id=o.episode_id AND e.series_id=?)",[id]).await?.next().await?.is_some(),
        MediaDomain::Movies => c.query("SELECT 1 FROM import_journal j JOIN operations o ON o.id=j.operation_id WHERE j.phase NOT IN ('preview','complete') AND o.media_type='movie' AND o.movie_id=?",[id]).await?.next().await?.is_some(),
    };
    if import_inflight {
        return Ok(true);
    }
    Ok(match media {
        MediaDomain::Tv => c.query("SELECT 1 FROM download_processing dp JOIN rss_candidates r ON r.id=dp.candidate_id WHERE dp.status IN ('checking','importing') AND r.media_type='tv' AND r.series_id=?",[id]).await?.next().await?.is_some(),
        MediaDomain::Movies => c.query("SELECT 1 FROM download_processing dp JOIN rss_candidates r ON r.id=dp.candidate_id WHERE dp.status IN ('checking','importing') AND r.media_type='movies' AND r.movie_id=?",[id]).await?.next().await?.is_some(),
    })
}
async fn create(
    State(db): State<Arc<Database>>,
    media: MediaDomain,
    q: std::result::Result<Query<Empty>, QueryRejection>,
    input: std::result::Result<Json<RescanInput>, JsonRejection>,
) -> Result<(StatusCode, Json<RescanBatch>)> {
    q.map_err(|_| bad())?;
    let input = input.map_err(|_| bad())?.0;
    if input
        .target_id
        .is_some_and(|id| !(1..=MAX_REVISION).contains(&id))
    {
        return Err(bad());
    }
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        let timestamp = now()?;
        let table = target_table(media);
        let explicit = input.target_id.is_some();
        let ids: Vec<i64> = match input.target_id {
            Some(id) => {
                if tx
                    .query(&format!("SELECT 1 FROM {table} WHERE id=?"), [id])
                    .await?
                    .next()
                    .await?
                    .is_none()
                {
                    return Err(Error(StatusCode::NOT_FOUND, "rescan_target_not_found"));
                }
                vec![id]
            }
            None => {
                let mut rows = tx
                    .query(
                        &format!("SELECT id FROM {table} ORDER BY id LIMIT ?"),
                        [MAX_COMMANDS + 1],
                    )
                    .await?;
                let mut v = Vec::new();
                while let Some(r) = rows.next().await? {
                    v.push(r.get::<i64>(0)?)
                }
                v
            }
        };
        if ids.is_empty() {
            return bounded(RescanBatch {
                commands: vec![],
                busy_target_ids: vec![],
            });
        }
        let mut active = std::collections::HashMap::new();
        let mut rows=tx.query(&format!("SELECT {COLUMNS} FROM rescan_commands WHERE media_type=? AND status IN ('queued','running','retry_wait')"),[domain(media)]).await?;
        while let Some(r) = rows.next().await? {
            let cmd = row(r)?;
            let key = match media {
                MediaDomain::Tv => cmd.series_id,
                MediaDomain::Movies => cmd.movie_id,
            };
            if let Some(key) = key {
                active.insert(key, cmd);
            }
        }
        drop(rows);
        let mut commands = Vec::new();
        let mut busy_target_ids = Vec::new();
        let mut to_insert = Vec::new();
        for id in ids {
            if let Some(existing) = active.get(&id) {
                commands.push(existing.clone());
                continue;
            }
            if busy(&tx, media, id).await? {
                if explicit {
                    return Err(Error(StatusCode::CONFLICT, "rescan_target_busy"));
                }
                busy_target_ids.push(id);
                continue;
            }
            to_insert.push(id);
        }
        let count = command_capacity(&tx).await?;
        if count + to_insert.len() as i64 > MAX_COMMANDS {
            return Err(Error(StatusCode::TOO_MANY_REQUESTS, "command_history_full"));
        }
        for id in to_insert {
            let cid = Uuid::new_v4();
            let (series, movie) = match media {
                MediaDomain::Tv => (Some(id), None),
                MediaDomain::Movies => (None, Some(id)),
            };
            tx.execute("INSERT INTO rescan_commands(id,media_type,series_id,movie_id,priority,status,attempts,next_attempt_at,created_at) VALUES(?,?,?,?,?,'queued',0,?,?)",params![cid.to_string(),domain(media),series,movie,input.priority.number(),timestamp,timestamp]).await?;
            commands.push(read(&tx, cid).await?);
        }
        bounded(RescanBatch {
            commands,
            busy_target_ids,
        })
    }
    .await;
    Ok((StatusCode::ACCEPTED, finish(tx, outcome).await?))
}
async fn list(
    State(db): State<Arc<Database>>,
    media: MediaDomain,
    q: std::result::Result<Query<RescanQuery>, QueryRejection>,
) -> Result<Json<ApiPage<RescanCommand>>> {
    let q = q.map_err(|_| bad())?.0;
    if !(1..=100).contains(&q.limit) || q.offset > MAX_COMMANDS as u32 {
        return Err(bad());
    }
    let status = q.status.map(RescanStatus::text);
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::ReadOnly)
        .await?;
    let outcome=async{
        let predicate="WHERE media_type=? AND (? IS NULL OR status=?)";
        let total=tx.query(&format!("SELECT count(*) FROM rescan_commands {predicate}"),params![domain(media),status,status]).await?.next().await?.ok_or_else(bad)?.get::<i64>(0)?;
        let mut rows=tx.query(&format!("SELECT {COLUMNS} FROM rescan_commands {predicate} ORDER BY created_at DESC,id DESC LIMIT ? OFFSET ?"),params![domain(media),status,status,i64::from(q.limit),i64::from(q.offset)]).await?;
        let mut items=Vec::new();while let Some(r)=rows.next().await?{items.push(row(r)?)}
        bounded(ApiPage{items,total,limit:q.limit,offset:q.offset})
    }.await;
    finish(tx, outcome).await
}
async fn detail(
    State(db): State<Arc<Database>>,
    media: MediaDomain,
    Path(id): Path<String>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
) -> Result<Json<RescanCommand>> {
    q.map_err(|_| bad())?;
    let id = Uuid::parse_str(&id).map_err(|_| bad())?;
    bounded(read_scoped(&connection(&db).await?, id, media).await?)
}
async fn cancel(
    State(db): State<Arc<Database>>,
    media: MediaDomain,
    Path(id): Path<String>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
) -> Result<Json<RescanCommand>> {
    q.map_err(|_| bad())?;
    let id = Uuid::parse_str(&id).map_err(|_| bad())?;
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        let current = read_scoped(&tx, id, media).await?;
        // Matches the manual_import precedent: cancel is only valid before real filesystem work
        // starts, not a silent no-op once the row is running or already terminal.
        if !matches!(current.status, RescanStatus::Queued | RescanStatus::RetryWait) {
            return Err(conflict());
        }
        tx.execute("UPDATE rescan_commands SET status='cancelled',completed_at=? WHERE id=? AND status IN ('queued','retry_wait')",params![now()?,id.to_string()]).await?;
        bounded(read_scoped(&tx, id, media).await?)
    }
    .await;
    finish(tx, outcome).await
}

// --- Worker integration -----------------------------------------------------------------

pub(super) async fn recover(c: &Connection, timestamp: i64, code: &str) -> Result<()> {
    c.execute("UPDATE rescan_commands SET status=CASE WHEN attempts<3 THEN 'retry_wait' ELSE 'failed' END,next_attempt_at=?,completed_at=CASE WHEN attempts<3 THEN NULL ELSE ? END,error_code=? WHERE status='running'",params![timestamp,timestamp,code]).await?;
    Ok(())
}
// The defensive in-flight checks in worker.rs's own SELECT should already keep a genuinely
// busy target from being picked; a target that vanished entirely (no FK on series_id/movie_id)
// is handled here directly, mirroring the "Downloads" (kind=0) precedent: settle it without
// ever attempting the running-transition UPDATE the schema's own trigger would reject.
pub(super) async fn claim(
    c: &Connection,
    id: Uuid,
    timestamp: i64,
) -> Result<Option<RescanCommand>> {
    let current = read(c, id).await?;
    let exists = match (current.series_id, current.movie_id) {
        (Some(sid), None) => c
            .query("SELECT 1 FROM series WHERE id=?", [sid])
            .await?
            .next()
            .await?
            .is_some(),
        (None, Some(mid)) => c
            .query("SELECT 1 FROM movies WHERE id=?", [mid])
            .await?
            .next()
            .await?
            .is_some(),
        _ => false,
    };
    if !exists {
        c.execute("UPDATE rescan_commands SET status='failed',completed_at=?,error_code='target_changed' WHERE id=? AND status IN ('queued','retry_wait')",params![timestamp,id.to_string()]).await?;
        return Ok(None);
    }
    c.execute("UPDATE rescan_commands SET status='running',attempts=attempts+1,started_at=?,error_code=NULL WHERE id=?",params![timestamp,id.to_string()]).await?;
    Ok(Some(read(c, id).await?))
}
enum Settlement {
    Succeeded { adopted: i64, removed: i64 },
    Skipped(&'static str),
    Failed(&'static str),
}
async fn settle_failed(
    c: &Connection,
    command: &RescanCommand,
    timestamp: i64,
    code: &'static str,
) -> Result<()> {
    // Retrying against a target that changed identity mid-scan can't self-heal; every other
    // code is a transient scan failure and follows the same attempt-counted backoff as
    // quality_reset's fail() (matching commands::mod.rs's exponential-plus-jitter shape).
    let retry = code != "target_changed" && command.attempts < 3;
    let jitter = i64::from(Uuid::new_v4().as_bytes()[0] % 3);
    let delay = (1i64 << command.attempts) + jitter;
    c.execute("UPDATE rescan_commands SET status=?,next_attempt_at=?,completed_at=?,error_code=? WHERE id=? AND status='running'",params![if retry{"retry_wait"}else{"failed"},timestamp+delay,if retry{None}else{Some(timestamp)},code,command.id.to_string()]).await?;
    Ok(())
}
async fn settle(
    c: &Connection,
    command: &RescanCommand,
    timestamp: i64,
    settlement: Settlement,
) -> Result<()> {
    match settlement {
        Settlement::Succeeded { adopted, removed } => {
            // A target deleted between claim() and here fails the succeeded-transition's own
            // "stale rescan target" trigger check; fall back to a settle path that doesn't
            // require the target to still exist, rather than let a raw DB error surface.
            if c.execute("UPDATE rescan_commands SET status='succeeded',completed_at=?,files_adopted=?,files_removed=? WHERE id=? AND status='running'",params![timestamp,adopted,removed,command.id.to_string()]).await.is_err() {
                return settle_failed(c, command, timestamp, "target_changed").await;
            }
        }
        Settlement::Skipped(reason) => {
            if c.execute("UPDATE rescan_commands SET status='skipped',completed_at=?,skip_reason=? WHERE id=? AND status='running'",params![timestamp,reason,command.id.to_string()]).await.is_err() {
                return settle_failed(c, command, timestamp, "target_changed").await;
            }
        }
        Settlement::Failed(code) => return settle_failed(c, command, timestamp, code).await,
    }
    Ok(())
}
pub(super) async fn run(db: &Arc<Database>, command: RescanCommand) -> Result<()> {
    let settlement = scan(db, &command).await;
    let c = connection(db).await?;
    let timestamp = now()?;
    settle(&c, &command, timestamp, settlement).await
}

// --- Scan: read-only walk (spawn_blocking, bounded) then small transactional writes -------

const SCAN_BUDGET: Duration = Duration::from_secs(30);
const MAX_WALK_ENTRIES: usize = 20_000;
const MAX_WALK_DEPTH: usize = 32;

struct WalkFile {
    relative: String,
    absolute: String,
    size: u64,
}
enum ScanOutcome {
    Skip(&'static str),
    Files(Vec<WalkFile>),
}
async fn target_info(c: &Connection, command: &RescanCommand) -> Result<Option<PathBuf>> {
    Ok(match (command.series_id, command.movie_id) {
        (Some(sid), None) => c
            .query("SELECT path FROM series WHERE id=?", [sid])
            .await?
            .next()
            .await?
            .map(|r| r.get::<String>(0))
            .transpose()?
            .map(PathBuf::from),
        (None, Some(mid)) => c
            .query("SELECT path FROM movies WHERE id=?", [mid])
            .await?
            .next()
            .await?
            .map(|r| r.get::<String>(0))
            .transpose()?
            .map(PathBuf::from),
        _ => None,
    })
}
// Sonarr/Radarr `RootFolderService.GetBestRootFolderPathInternal`: the longest configured root
// whose path is a parent of the target, falling back to the target's own parent directory when
// no configured root matches.
async fn root_for_failsafe(c: &Connection, media: MediaDomain, target: &FsPath) -> Result<PathBuf> {
    let mut rows = c
        .query(
            "SELECT path FROM root_folders WHERE media_type=?",
            [domain(media)],
        )
        .await?;
    let mut best: Option<PathBuf> = None;
    while let Some(r) = rows.next().await? {
        let candidate = PathBuf::from(r.get::<String>(0)?);
        if target.starts_with(&candidate) {
            let better = match &best {
                Some(b) => candidate.as_os_str().len() > b.as_os_str().len(),
                None => true,
            };
            if better {
                best = Some(candidate);
            }
        }
    }
    Ok(match best {
        Some(b) => b,
        None => target
            .parent()
            .map(FsPath::to_path_buf)
            .unwrap_or_else(|| target.to_path_buf()),
    })
}
fn io_exists(path: &FsPath) -> std::result::Result<bool, &'static str> {
    match crate::import::filesystem_directory(path) {
        Ok(_) => Ok(true),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(false)
        }
        Err(_) => Err("storage_error"),
    }
}
fn io_folder_empty(path: &FsPath) -> std::result::Result<bool, &'static str> {
    let fd = crate::import::filesystem_directory(path).map_err(|_| "storage_error")?;
    for entry in fs::Dir::read_from(&fd).map_err(|_| "storage_error")? {
        let entry = entry.map_err(|_| "storage_error")?;
        let name = entry.file_name().to_str().map_err(|_| "storage_error")?;
        if name != "." && name != ".." {
            return Ok(false);
        }
    }
    Ok(true)
}
fn recognized_extension(name: &str) -> bool {
    matches!(
        name.rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase())
            .as_deref(),
        Some("mkv" | "mp4" | "avi" | "m4v" | "ts" | "m2ts")
    )
}
// Upstream's ExcludedSubFoldersRegex/ExcludedExtrasSubFolderRegex names, by exact directory
// name rather than full regex; every hidden (dot-prefixed) directory is also always excluded,
// which incidentally keeps this walk out of every `.hrrdarr-*` staging/quarantine directory.
fn hidden_or_special_dir(name: &str) -> bool {
    if name.starts_with('.') {
        return true;
    }
    matches!(
        name.to_ascii_lowercase().as_str(),
        "extras"
            | "extrafanart"
            | "behind the scenes"
            | "deleted scenes"
            | "featurettes"
            | "interviews"
            | "other"
            | "scenes"
            | "samples"
            | "shorts"
            | "trailers"
            | "theme music"
            | "backdrops"
            | "@eadir"
            | "plex versions"
    )
}
fn skip_file(name: &str) -> bool {
    if name.starts_with('.') {
        return true;
    }
    let lower = name.to_ascii_lowercase();
    if lower
        .split(|c: char| !c.is_alphanumeric())
        .any(|token| token == "sample")
    {
        return true;
    }
    let stem = lower.rsplit_once('.').map_or(lower.as_str(), |(s, _)| s);
    [
        "trailer",
        "other",
        "behindthescenes",
        "deleted",
        "featurette",
        "interview",
        "scene",
        "short",
    ]
    .iter()
    .any(|suffix| stem.ends_with(&format!("-{suffix}")))
}
struct WalkState {
    visited: usize,
    deadline: Instant,
}
impl WalkState {
    fn visit(&mut self) -> std::result::Result<(), &'static str> {
        self.visited += 1;
        if self.visited > MAX_WALK_ENTRIES || Instant::now() >= self.deadline {
            return Err("storage_error");
        }
        Ok(())
    }
}
// Descriptor-relative, NOFOLLOW, ancestor-loop-checked -- the same safety properties as
// `filesystem.rs::walk`/`import/fs.rs`, reimplemented here since those are private to their
// own modules. Any cap, error or non-UTF8 name aborts the whole walk (fail closed): a partial
// file list must never be treated as authoritative, since cleanup would then delete
// associations for files this attempt simply never reached.
fn walk_dir(
    fd: &File,
    base: &FsPath,
    relative: &FsPath,
    state: &mut WalkState,
    ancestors: &mut Vec<(u64, u64)>,
    out: &mut Vec<WalkFile>,
) -> std::result::Result<(), &'static str> {
    if ancestors.len() > MAX_WALK_DEPTH {
        return Err("storage_error");
    }
    for entry in fs::Dir::read_from(fd).map_err(|_| "storage_error")? {
        state.visit()?;
        let entry = entry.map_err(|_| "storage_error")?;
        let name = entry.file_name().to_str().map_err(|_| "storage_error")?;
        if name == "." || name == ".." {
            continue;
        }
        let stat = fs::statat(fd, entry.file_name(), AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| "storage_error")?;
        match fs::FileType::from_raw_mode(stat.st_mode) {
            fs::FileType::Directory => {
                if hidden_or_special_dir(name) {
                    continue;
                }
                let child: File = fs::openat(
                    fd,
                    entry.file_name(),
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Permissions::empty(),
                )
                .map_err(|_| "storage_error")?
                .into();
                let actual = fs::fstat(&child).map_err(|_| "storage_error")?;
                let id = (actual.st_dev, actual.st_ino);
                if id != (stat.st_dev, stat.st_ino) || ancestors.contains(&id) {
                    return Err("storage_error");
                }
                ancestors.push(id);
                walk_dir(&child, base, &relative.join(name), state, ancestors, out)?;
                ancestors.pop();
            }
            fs::FileType::RegularFile => {
                if skip_file(name) || !recognized_extension(name) {
                    continue;
                }
                let rel = relative.join(name);
                let full = base.join(&rel);
                out.push(WalkFile {
                    relative: rel.to_str().ok_or("storage_error")?.to_string(),
                    absolute: full.to_str().ok_or("storage_error")?.to_string(),
                    size: u64::try_from(stat.st_size).map_err(|_| "storage_error")?,
                });
            }
            _ => {}
        }
    }
    Ok(())
}
fn blocking_scan(
    root: PathBuf,
    target: PathBuf,
    deadline: Instant,
) -> std::result::Result<ScanOutcome, &'static str> {
    if !io_exists(&target)? {
        if !io_exists(&root)? {
            return Ok(ScanOutcome::Skip("root_missing"));
        }
        if io_folder_empty(&root)? {
            return Ok(ScanOutcome::Skip("root_empty"));
        }
        return Ok(ScanOutcome::Files(Vec::new()));
    }
    let fd = crate::import::filesystem_directory(&target).map_err(|_| "storage_error")?;
    let root_stat = fs::fstat(&fd).map_err(|_| "storage_error")?;
    let mut ancestors = vec![(root_stat.st_dev, root_stat.st_ino)];
    let mut out = Vec::new();
    let mut state = WalkState {
        visited: 0,
        deadline,
    };
    walk_dir(
        &fd,
        &target,
        FsPath::new(""),
        &mut state,
        &mut ancestors,
        &mut out,
    )?;
    Ok(ScanOutcome::Files(out))
}
async fn quality_for(c: &Connection, media: &str, name: Option<&str>) -> Result<Option<i64>> {
    let Some(name) = name else { return Ok(None) };
    Ok(c.query(
        "SELECT quality_id FROM quality_definitions WHERE media_type=? AND name=?",
        params![media, name],
    )
    .await?
    .next()
    .await?
    .map(|r| r.get::<i64>(0))
    .transpose()?)
}
fn storage_size(size: u64) -> Result<i64> {
    i64::try_from(size)
        .map_err(|_| Error(StatusCode::INTERNAL_SERVER_ERROR, "command_storage_error"))
}
fn stem_of(relative: &str) -> Option<&str> {
    let name = relative.rsplit('/').next().unwrap_or(relative);
    name.rsplit_once('.').map(|(stem, _)| stem)
}
// Deletes both the file record and its metadata row in one transaction (foreign_keys=ON means
// file_metadata's ON DELETE RESTRICT must be cleared first). A row still protected by a live
// `rss_candidate_imports` replacement claim (FK RESTRICT) is left untouched -- that protection
// is intentional, not a bug, so this reports "not removed" rather than failing the whole scan.
async fn delete_episode_file(c: &Connection, file_id: i64) -> Result<bool> {
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome: std::result::Result<(), libsql::Error> = async {
        tx.execute(
            "UPDATE episodes SET episode_file_id=NULL WHERE episode_file_id=?",
            [file_id],
        )
        .await?;
        tx.execute(
            "DELETE FROM file_metadata WHERE episode_file_id=?",
            [file_id],
        )
        .await?;
        tx.execute("DELETE FROM episode_files WHERE id=?", [file_id])
            .await?;
        Ok(())
    }
    .await;
    match outcome {
        Ok(()) => {
            tx.commit().await?;
            Ok(true)
        }
        Err(_) => {
            let _ = tx.rollback().await;
            Ok(false)
        }
    }
}
async fn delete_movie_file(c: &Connection, file_id: i64) -> Result<bool> {
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome: std::result::Result<(), libsql::Error> = async {
        tx.execute("DELETE FROM file_metadata WHERE movie_file_id=?", [file_id])
            .await?;
        tx.execute("DELETE FROM movie_files WHERE id=?", [file_id])
            .await?;
        Ok(())
    }
    .await;
    match outcome {
        Ok(()) => {
            tx.commit().await?;
            Ok(true)
        }
        Err(_) => {
            let _ = tx.rollback().await;
            Ok(false)
        }
    }
}
async fn adopt_episode_file(
    c: &Connection,
    series_id: i64,
    path: &str,
    size: i64,
    season: i64,
    quality_id: Option<i64>,
    episode_ids: &[i64],
) -> Result<()> {
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome: std::result::Result<(), libsql::Error> = async {
        tx.execute(
            "INSERT INTO episode_files(series_id,path) VALUES(?,?)",
            params![series_id, path],
        )
        .await?;
        let fid = tx.last_insert_rowid();
        tx.execute("INSERT INTO file_metadata(media_type,episode_file_id,size,date_added,season_number,quality_id) VALUES('tv',?,?,strftime('%Y-%m-%dT%H:%M:%SZ','now'),?,?)",params![fid,size,season,quality_id]).await?;
        for eid in episode_ids {
            tx.execute(
                "UPDATE episodes SET episode_file_id=? WHERE id=?",
                params![fid, *eid],
            )
            .await?;
        }
        Ok(())
    }
    .await;
    match outcome {
        Ok(()) => {
            tx.commit().await?;
            Ok(())
        }
        Err(e) => {
            let _ = tx.rollback().await;
            Err(Error::from(e))
        }
    }
}
async fn adopt_movie_file(
    c: &Connection,
    movie_id: i64,
    path: &str,
    size: i64,
    quality_id: Option<i64>,
    edition: Option<&str>,
) -> Result<()> {
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome: std::result::Result<(), libsql::Error> = async {
        tx.execute(
            "INSERT INTO movie_files(movie_id,path,edition) VALUES(?,?,?)",
            params![movie_id, path, edition],
        )
        .await?;
        let fid = tx.last_insert_rowid();
        tx.execute("INSERT INTO file_metadata(media_type,movie_file_id,size,date_added,quality_id) VALUES('movies',?,?,strftime('%Y-%m-%dT%H:%M:%SZ','now'),?)",params![fid,size,quality_id]).await?;
        Ok(())
    }
    .await;
    match outcome {
        Ok(()) => {
            tx.commit().await?;
            Ok(())
        }
        Err(e) => {
            let _ = tx.rollback().await;
            Err(Error::from(e))
        }
    }
}
// Mirrors upstream's `CleanMediaFiles` (files gone from disk entirely) then
// `GetImportDecisions`/`Import` for `newDownload=false` (delete-at-path, then add). Deliberately
// does not rank duplicate candidates by quality: if two present files resolve to the same
// episode(s), the first in deterministic (sorted-by-relative-path) order wins and the other is
// left unmatched, rather than this module guessing a quality preference it doesn't own.
async fn reconcile_series(
    c: &Connection,
    series_id: i64,
    mut files: Vec<WalkFile>,
) -> Result<(i64, i64)> {
    let present: HashSet<String> = files.iter().map(|f| f.absolute.clone()).collect();
    let mut removed = 0i64;
    let mut stale = Vec::new();
    {
        let mut rows = c
            .query(
                "SELECT id,path FROM episode_files WHERE series_id=?",
                [series_id],
            )
            .await?;
        while let Some(r) = rows.next().await? {
            let id: i64 = r.get(0)?;
            let path: String = r.get(1)?;
            if !present.contains(&path) {
                stale.push(id);
            }
        }
    }
    for id in stale {
        if delete_episode_file(c, id).await? {
            removed += 1;
        }
    }
    files.sort_by(|a, b| a.relative.cmp(&b.relative));
    let mut adopted = 0i64;
    let mut claimed: HashSet<i64> = HashSet::new();
    for file in &files {
        let Some(stem) = stem_of(&file.relative) else {
            continue;
        };
        let Ok(parsed) = parser::parse(stem, true) else {
            continue;
        };
        let Some(Numbering::Episodes { season, episodes }) = parsed.numbering else {
            continue; // scn.002: season/daily/absolute numbering unsupported here, reported unmatched
        };
        let mut ids = Vec::with_capacity(episodes.len());
        let mut ok = true;
        for n in &episodes {
            match c
                .query(
                    "SELECT id FROM episodes WHERE series_id=? AND season=? AND number=?",
                    params![series_id, season, *n],
                )
                .await?
                .next()
                .await?
            {
                Some(r) => ids.push(r.get::<i64>(0)?),
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if !ok || ids.iter().any(|id| claimed.contains(id)) {
            continue;
        }
        if let Some(r) = c
            .query(
                "SELECT id,series_id FROM episode_files WHERE path=?",
                [file.absolute.clone()],
            )
            .await?
            .next()
            .await?
        {
            let existing_id: i64 = r.get(0)?;
            let existing_series: i64 = r.get(1)?;
            if existing_series != series_id {
                continue; // path already owned by another series: report unmatched, never steal it
            }
            let mut all_target_match = true;
            let mut only_target = true;
            let mut erows = c
                .query(
                    "SELECT id,episode_file_id FROM episodes WHERE series_id=?",
                    [series_id],
                )
                .await?;
            while let Some(er) = erows.next().await? {
                let eid: i64 = er.get(0)?;
                let fid: Option<i64> = er.get(1)?;
                let is_target = ids.contains(&eid);
                let points_here = fid == Some(existing_id);
                if is_target && !points_here {
                    all_target_match = false;
                }
                if !is_target && points_here {
                    only_target = false;
                }
            }
            drop(erows);
            if all_target_match && only_target {
                for id in &ids {
                    claimed.insert(*id);
                }
                continue; // already correctly associated
            }
            if !delete_episode_file(c, existing_id).await? {
                continue; // FK-protected; leave this candidate unmatched this pass
            }
        }
        let mut displaced = false;
        for eid in &ids {
            if let Some(r) = c
                .query(
                    "SELECT f.path FROM episodes e JOIN episode_files f ON f.id=e.episode_file_id WHERE e.id=?",
                    [*eid],
                )
                .await?
                .next()
                .await?
            {
                let existing_path: String = r.get(0)?;
                if existing_path != file.absolute && present.contains(&existing_path) {
                    displaced = true;
                }
            }
        }
        if displaced {
            continue;
        }
        let quality_id = quality_for(c, "tv", parsed.quality_name.as_deref()).await?;
        let size = storage_size(file.size)?;
        adopt_episode_file(c, series_id, &file.absolute, size, season, quality_id, &ids).await?;
        for id in &ids {
            claimed.insert(*id);
        }
        adopted += 1;
    }
    Ok((adopted, removed))
}
async fn reconcile_movie(
    c: &Connection,
    movie_id: i64,
    mut files: Vec<WalkFile>,
) -> Result<(i64, i64)> {
    let present: HashSet<String> = files.iter().map(|f| f.absolute.clone()).collect();
    let mut removed = 0i64;
    let mut have_file = false;
    if let Some(r) = c
        .query(
            "SELECT id,path FROM movie_files WHERE movie_id=?",
            [movie_id],
        )
        .await?
        .next()
        .await?
    {
        let id: i64 = r.get(0)?;
        let path: String = r.get(1)?;
        if present.contains(&path) {
            have_file = true;
        } else if delete_movie_file(c, id).await? {
            removed = 1;
        } else {
            have_file = true; // FK-protected; the existing association remains authoritative
        }
    }
    let mut adopted = 0i64;
    if !have_file {
        files.sort_by(|a, b| a.relative.cmp(&b.relative));
        for file in &files {
            let Some(stem) = stem_of(&file.relative) else {
                continue;
            };
            let Ok(parsed) = parser::parse(stem, false) else {
                continue; // includes files without a year token: reported unmatched
            };
            let owned_elsewhere = c.query("SELECT 1 FROM episode_files WHERE path=? UNION ALL SELECT 1 FROM movie_files WHERE path=? AND movie_id!=? LIMIT 1",params![file.absolute.clone(),file.absolute.clone(),movie_id]).await?.next().await?.is_some();
            if owned_elsewhere {
                continue;
            }
            let quality_id = quality_for(c, "movies", parsed.quality_name.as_deref()).await?;
            let size = storage_size(file.size)?;
            adopt_movie_file(
                c,
                movie_id,
                &file.absolute,
                size,
                quality_id,
                parsed.edition.as_deref(),
            )
            .await?;
            adopted = 1;
            break;
        }
    }
    Ok((adopted, removed))
}
async fn scan(db: &Database, command: &RescanCommand) -> Settlement {
    let c = match connection(db).await {
        Ok(c) => c,
        Err(_) => return Settlement::Failed("storage_error"),
    };
    let media = command.media_type;
    let target = match target_info(&c, command).await {
        Ok(Some(v)) => v,
        Ok(None) => return Settlement::Failed("target_changed"),
        Err(_) => return Settlement::Failed("storage_error"),
    };
    let root = match root_for_failsafe(&c, media, &target).await {
        Ok(v) => v,
        Err(_) => return Settlement::Failed("storage_error"),
    };
    let deadline = Instant::now() + SCAN_BUDGET;
    let outcome = tokio::task::spawn_blocking(move || blocking_scan(root, target, deadline)).await;
    let files = match outcome {
        Ok(Ok(ScanOutcome::Skip(reason))) => return Settlement::Skipped(reason),
        Ok(Ok(ScanOutcome::Files(files))) => files,
        Ok(Err(code)) => return Settlement::Failed(code),
        Err(_) => return Settlement::Failed("storage_error"),
    };
    let result = match (command.series_id, command.movie_id) {
        (Some(sid), None) => reconcile_series(&c, sid, files).await,
        (None, Some(mid)) => reconcile_movie(&c, mid, files).await,
        _ => return Settlement::Failed("storage_error"),
    };
    match result {
        Ok((adopted, removed)) => Settlement::Succeeded { adopted, removed },
        Err(_) => Settlement::Failed("storage_error"),
    }
}

#[cfg(test)]
mod tests;
