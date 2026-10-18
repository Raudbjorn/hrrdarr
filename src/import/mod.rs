//! Initial manual imports and receipt-bound singleton replacements with durable recovery.
mod fs;
mod owned;
pub(crate) mod probe;
mod recycle;
pub(crate) mod scan;
pub(crate) mod scan_cleanup;
mod scan_owned;
pub(crate) use fs::scan as scan_fs;
pub(crate) use owned::{OwnedImport, prepare_owned};
pub(crate) use scan_owned::{ScanChildAdmission, ScanChildStart, prepare_scan, start_scan};

// Share the existing descriptor-relative, no-symlink directory walk with root observations.
pub(crate) fn root_directory(path: &std::path::Path) -> Option<std::fs::File> {
    fs::directory(path).ok()
}
// The same anchored walk, retaining OS error classification for read-only browsing.
pub(crate) fn filesystem_directory(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    fs::directory_io(path)
}
use crate::{
    api::{ApiErrorEnvelope, ImportRequest, Operation},
    db::{Database, MediaTarget},
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
pub(crate) use fs::Plan;
use fs::Stage;
use libsql::{Connection, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use ts_rs::TS;
use uuid::Uuid;

#[derive(Clone, Copy, Debug)]
pub(super) enum PathRole {
    Source,
    Root,
    Destination,
    Stage,
    Retirement,
}
#[derive(Clone, Copy, Debug)]
pub(super) enum PathKind {
    Missing,
    Denied,
    Changed,
    Invalid,
    Io,
}
#[derive(Clone, Copy, Debug)]
enum PathBoundary {
    Precommit,
    Cleanup,
}

#[derive(Debug)]
pub struct Error {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
    diagnostic: Option<String>,
    path_kind: Option<PathKind>,
    path_role: Option<PathRole>,
    path_boundary: Option<PathBoundary>,
    started_attempt: bool,
}
type Result<T> = std::result::Result<T, Error>;
const MAX_SOURCE_GUARD_ROWS: usize = 10_000;
impl Error {
    pub(crate) fn code(&self) -> &'static str {
        self.code
    }

    /// Closed, path-free classification of a measured filesystem failure for callers outside
    /// this module (the downloaded-scan producer). `None` means no filesystem cause was measured.
    pub(crate) fn path_class(&self) -> Option<&'static str> {
        self.path_kind.map(|kind| match kind {
            PathKind::Missing => "missing",
            PathKind::Denied => "denied",
            PathKind::Changed => "changed",
            PathKind::Invalid => "invalid",
            PathKind::Io => "io",
        })
    }

    pub(crate) fn started_attempt(&self) -> bool {
        self.started_attempt
    }
    /// Storage/internal failures may succeed on a later attempt; every other error is a
    /// measured, deterministic refusal of the current facts.
    pub(crate) fn transient(&self) -> bool {
        self.status.is_server_error()
    }
    fn at(mut self, role: PathRole) -> Self {
        if self.path_role.is_none() && self.path_kind.is_some() {
            self.path_role = Some(role);
        }
        self
    }
    fn boundary(mut self, boundary: PathBoundary) -> Self {
        if self.path_boundary.is_none() {
            self.path_boundary = Some(boundary);
        }
        self
    }
    pub(crate) fn path_failure_code(&self) -> Option<&'static str> {
        match (self.path_role?, self.path_kind?, self.path_boundary?) {
            (PathRole::Source, PathKind::Missing, PathBoundary::Precommit) => {
                Some("source_missing_precommit")
            }
            (PathRole::Source, PathKind::Missing, PathBoundary::Cleanup) => {
                Some("source_missing_cleanup")
            }
            (PathRole::Source, PathKind::Denied, PathBoundary::Precommit) => {
                Some("source_denied_precommit")
            }
            (PathRole::Source, PathKind::Denied, PathBoundary::Cleanup) => {
                Some("source_denied_cleanup")
            }
            (PathRole::Source, PathKind::Changed, PathBoundary::Precommit) => {
                Some("source_changed_precommit")
            }
            (PathRole::Source, PathKind::Changed, PathBoundary::Cleanup) => {
                Some("source_changed_cleanup")
            }
            (PathRole::Source, PathKind::Invalid, PathBoundary::Precommit) => {
                Some("source_invalid_precommit")
            }
            (PathRole::Source, PathKind::Invalid, PathBoundary::Cleanup) => {
                Some("source_invalid_cleanup")
            }
            (PathRole::Source, PathKind::Io, PathBoundary::Precommit) => {
                Some("source_io_precommit")
            }
            (PathRole::Source, PathKind::Io, PathBoundary::Cleanup) => Some("source_io_cleanup"),
            (PathRole::Root, PathKind::Missing, PathBoundary::Precommit) => {
                Some("root_missing_precommit")
            }
            (PathRole::Root, PathKind::Missing, PathBoundary::Cleanup) => {
                Some("root_missing_cleanup")
            }
            (PathRole::Root, PathKind::Denied, PathBoundary::Precommit) => {
                Some("root_denied_precommit")
            }
            (PathRole::Root, PathKind::Denied, PathBoundary::Cleanup) => {
                Some("root_denied_cleanup")
            }
            (PathRole::Root, PathKind::Changed, PathBoundary::Precommit) => {
                Some("root_changed_precommit")
            }
            (PathRole::Root, PathKind::Changed, PathBoundary::Cleanup) => {
                Some("root_changed_cleanup")
            }
            (PathRole::Root, PathKind::Invalid, PathBoundary::Precommit) => {
                Some("root_invalid_precommit")
            }
            (PathRole::Root, PathKind::Invalid, PathBoundary::Cleanup) => {
                Some("root_invalid_cleanup")
            }
            (PathRole::Root, PathKind::Io, PathBoundary::Precommit) => Some("root_io_precommit"),
            (PathRole::Root, PathKind::Io, PathBoundary::Cleanup) => Some("root_io_cleanup"),
            (PathRole::Destination, PathKind::Missing, PathBoundary::Precommit) => {
                Some("destination_missing_precommit")
            }
            (PathRole::Destination, PathKind::Missing, PathBoundary::Cleanup) => {
                Some("destination_missing_cleanup")
            }
            (PathRole::Destination, PathKind::Denied, PathBoundary::Precommit) => {
                Some("destination_denied_precommit")
            }
            (PathRole::Destination, PathKind::Denied, PathBoundary::Cleanup) => {
                Some("destination_denied_cleanup")
            }
            (PathRole::Destination, PathKind::Changed, PathBoundary::Precommit) => {
                Some("destination_changed_precommit")
            }
            (PathRole::Destination, PathKind::Changed, PathBoundary::Cleanup) => {
                Some("destination_changed_cleanup")
            }
            (PathRole::Destination, PathKind::Invalid, PathBoundary::Precommit) => {
                Some("destination_invalid_precommit")
            }
            (PathRole::Destination, PathKind::Invalid, PathBoundary::Cleanup) => {
                Some("destination_invalid_cleanup")
            }
            (PathRole::Destination, PathKind::Io, PathBoundary::Precommit) => {
                Some("destination_io_precommit")
            }
            (PathRole::Destination, PathKind::Io, PathBoundary::Cleanup) => {
                Some("destination_io_cleanup")
            }
            (PathRole::Stage, PathKind::Missing, PathBoundary::Precommit) => {
                Some("stage_missing_precommit")
            }
            (PathRole::Stage, PathKind::Missing, PathBoundary::Cleanup) => {
                Some("stage_missing_cleanup")
            }
            (PathRole::Stage, PathKind::Denied, PathBoundary::Precommit) => {
                Some("stage_denied_precommit")
            }
            (PathRole::Stage, PathKind::Denied, PathBoundary::Cleanup) => {
                Some("stage_denied_cleanup")
            }
            (PathRole::Stage, PathKind::Changed, PathBoundary::Precommit) => {
                Some("stage_changed_precommit")
            }
            (PathRole::Stage, PathKind::Changed, PathBoundary::Cleanup) => {
                Some("stage_changed_cleanup")
            }
            (PathRole::Stage, PathKind::Invalid, PathBoundary::Precommit) => {
                Some("stage_invalid_precommit")
            }
            (PathRole::Stage, PathKind::Invalid, PathBoundary::Cleanup) => {
                Some("stage_invalid_cleanup")
            }
            (PathRole::Stage, PathKind::Io, PathBoundary::Precommit) => Some("stage_io_precommit"),
            (PathRole::Stage, PathKind::Io, PathBoundary::Cleanup) => Some("stage_io_cleanup"),
            (PathRole::Retirement, PathKind::Missing, PathBoundary::Cleanup) => {
                Some("retirement_missing_cleanup")
            }
            (PathRole::Retirement, PathKind::Denied, PathBoundary::Cleanup) => {
                Some("retirement_denied_cleanup")
            }
            (PathRole::Retirement, PathKind::Changed, PathBoundary::Cleanup) => {
                Some("retirement_changed_cleanup")
            }
            (PathRole::Retirement, PathKind::Invalid, PathBoundary::Cleanup) => {
                Some("retirement_invalid_cleanup")
            }
            (PathRole::Retirement, PathKind::Io, PathBoundary::Cleanup) => {
                Some("retirement_io_cleanup")
            }
            _ => None,
        }
    }
    pub(crate) fn prepare_failure_code(&self) -> Option<&'static str> {
        match (self.path_role?, self.path_kind?) {
            (PathRole::Source, PathKind::Missing) => Some("hc021_source_missing_prepare"),
            (PathRole::Source, PathKind::Denied) => Some("hc021_source_denied_prepare"),
            (PathRole::Source, PathKind::Changed) => Some("hc021_source_changed_prepare"),
            (PathRole::Source, PathKind::Invalid) => Some("hc021_source_invalid_prepare"),
            (PathRole::Source, PathKind::Io) => Some("hc021_source_io_prepare"),
            (PathRole::Root, PathKind::Missing) => Some("hc021_root_missing_prepare"),
            (PathRole::Root, PathKind::Denied) => Some("hc021_root_denied_prepare"),
            (PathRole::Root, PathKind::Changed) => Some("hc021_root_changed_prepare"),
            (PathRole::Root, PathKind::Invalid) => Some("hc021_root_invalid_prepare"),
            (PathRole::Root, PathKind::Io) => Some("hc021_root_io_prepare"),
            (PathRole::Destination, PathKind::Missing) => Some("hc021_destination_missing_prepare"),
            (PathRole::Destination, PathKind::Denied) => Some("hc021_destination_denied_prepare"),
            (PathRole::Destination, PathKind::Changed) => Some("hc021_destination_changed_prepare"),
            (PathRole::Destination, PathKind::Invalid) => Some("hc021_destination_invalid_prepare"),
            (PathRole::Destination, PathKind::Io) => Some("hc021_destination_io_prepare"),
            _ => None,
        }
    }

    fn bad(message: &'static str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_request",
            message,
            diagnostic: None,
            path_kind: None,
            path_role: None,
            path_boundary: None,
            started_attempt: false,
        }
    }
    fn conflict(code: &'static str, message: &'static str) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code,
            message,
            diagnostic: None,
            path_kind: None,
            path_role: None,
            path_boundary: None,
            started_attempt: false,
        }
    }
    fn missing() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "not_found",
            message: "Import operation or media target was not found",
            diagnostic: None,
            path_kind: None,
            path_role: None,
            path_boundary: None,
            started_attempt: false,
        }
    }
    fn internal() -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "import_error",
            message: "Import state could not be read or persisted; retry using the same operation ID",
            diagnostic: None,
            path_kind: None,
            path_role: None,
            path_boundary: None,
            started_attempt: false,
        }
    }
}
impl From<libsql::Error> for Error {
    fn from(error: libsql::Error) -> Self {
        let mut result = Self::internal();
        result.diagnostic = Some(match error {
            libsql::Error::SqliteFailure(code, _) => format!("sqlite_code={code}"),
            libsql::Error::RemoteSqliteFailure(code, extended, _) => {
                format!("sqlite_code={code},extended={extended}")
            }
            other => format!("database_error_class={:?}", std::mem::discriminant(&other)),
        });
        result
    }
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ApiErrorEnvelope::new(self.code, self.message)),
        )
            .into_response()
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Copy,
    Move,
    Hardlink,
}
impl Mode {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Copy => "copy",
            Self::Move => "move",
            Self::Hardlink => "hardlink",
        }
    }
}
#[derive(Debug, Deserialize, Serialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ManualImportRequest {
    pub target: MediaTarget,
    pub source: String,
    pub mode: Mode,
    pub destination: String,
}
#[derive(Debug, Deserialize, Serialize, TS)]
#[serde(untagged)]
pub enum ImportInput {
    Typed(ManualImportRequest),
    Legacy(ImportRequest),
}
impl ImportInput {
    fn request(self) -> Result<ManualImportRequest> {
        match self {
            Self::Typed(r) => Ok(r),
            Self::Legacy(r) => Ok(ManualImportRequest {
                target: MediaTarget::Episode(r.episode_id),
                source: r.source,
                destination: r.destination,
                mode: match r.mode.as_str() {
                    "copy" => Mode::Copy,
                    "move" => Mode::Move,
                    "hardlink" => Mode::Hardlink,
                    _ => return Err(Error::bad("Mode must be copy, move or hardlink")),
                },
            }),
        }
    }
}
struct Record {
    id: String,
    target: MediaTarget,
    phase: String,
    plan: Option<Plan>,
    stage: Option<Stage>,
    error: Option<String>,
    path_failure: Option<String>,
}
impl Record {
    fn response(self) -> Result<Operation> {
        let recovery_required = self.phase != "complete"
            && (self.error.is_some() || self.path_failure.is_some())
            && !operation_active(&self.id)?;
        Ok(Operation {
            id: Uuid::parse_str(&self.id).map_err(|_| Error::internal())?,
            target: self.target,
            status: self.phase,
            message:
                "Manual import status; execute explicitly authorizes or resumes this operation"
                    .into(),
            error_code: self.error,
            path_failure_code: self.path_failure,
            recovery_required,
        })
    }
}

pub fn router(db: Arc<Database>) -> Router {
    Router::new()
        .route("/api/v1/imports", post(preview_handler))
        .route("/api/v1/imports/{id}", get(status_handler))
        .route("/api/v1/imports/{id}/execute", post(execute_handler))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .with_state(db)
}
async fn preview_handler(
    State(db): State<Arc<Database>>,
    body: std::result::Result<Json<ImportInput>, JsonRejection>,
) -> Result<impl IntoResponse> {
    let body = body
        .map_err(|_| Error::bad("Invalid import request body"))?
        .0;
    Ok((StatusCode::ACCEPTED, Json(preview(db, body).await?)))
}
async fn status_handler(
    State(db): State<Arc<Database>>,
    Path(id): Path<String>,
) -> Result<Json<Operation>> {
    Ok(Json(status(db, &id).await?))
}
async fn execute_handler(
    State(db): State<Arc<Database>>,
    Path(id): Path<String>,
) -> Result<Json<Operation>> {
    Ok(Json(execute(db, &id).await?))
}
fn local(db: &Database) -> Result<()> {
    if db.permits_local_imports() {
        Ok(())
    } else {
        Err(Error {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "local_owner_required",
            message: "Physical imports require a locally owned database",
            diagnostic: None,
            path_kind: None,
            path_role: None,
            path_boundary: None,
            started_attempt: false,
        })
    }
}
fn target(t: &MediaTarget) -> (&'static str, Option<i64>, Option<i64>) {
    match t {
        MediaTarget::Episode(id) => ("episode", Some(*id), None),
        MediaTarget::Movie(id) => ("movie", None, Some(*id)),
    }
}
fn id(t: &MediaTarget) -> i64 {
    match t {
        MediaTarget::Episode(v) | MediaTarget::Movie(v) => *v,
    }
}
async fn owner(c: &Connection, t: &MediaTarget) -> Result<(i64, String, Option<i64>)> {
    if id(t) <= 0 {
        return Err(Error::bad("Target ID must be positive"));
    }
    let sql = match t {
        MediaTarget::Episode(_) => {
            "SELECT s.id,s.path,e.season,e.episode_file_id FROM episodes e JOIN series s ON s.id=e.series_id WHERE e.id=?1"
        }
        MediaTarget::Movie(_) => {
            "SELECT m.id,m.path,NULL,f.id FROM movies m LEFT JOIN movie_files f ON f.movie_id=m.id WHERE m.id=?1"
        }
    };
    let r = c
        .query(sql, [id(t)])
        .await?
        .next()
        .await?
        .ok_or_else(Error::missing)?;
    if r.get::<Option<i64>>(3)?.is_some() {
        return Err(Error::conflict(
            "replacement_unsupported",
            "Target already has a file; replacement imports are not implemented",
        ));
    }
    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
}
async fn destination_free(c: &Connection, path: &str) -> Result<()> {
    if c.query("SELECT 1 FROM episode_files WHERE path=?1 UNION ALL SELECT 1 FROM movie_files WHERE path=?1 LIMIT 1",[path]).await?.next().await?.is_some(){return Err(Error::conflict("destination_claimed","Destination is already associated with a library file"));}
    Ok(())
}
async fn source_unmanaged(c: &Connection, path: &str) -> Result<()> {
    fs::validate_path(path)?;
    // Compare path components, including legacy root declarations with redundant separators.
    // ponytail: scan at most 10,000 root/file rows; indexed canonical ownership when larger libraries need it.
    let mut rows=c.query("SELECT path,1 FROM series UNION ALL SELECT path,1 FROM movies UNION ALL SELECT path,0 FROM episode_files UNION ALL SELECT path,0 FROM movie_files LIMIT 10001",()).await?;
    let mut count = 0;
    while let Some(row) = rows.next().await? {
        count += 1;
        if count > MAX_SOURCE_GUARD_ROWS {
            return Err(Error::conflict(
                "library_guard_limit",
                "Library is too large for the current bounded source ownership check",
            ));
        }
        let stored: String = row.get(0)?;
        let root: i64 = row.get(1)?;
        if (root == 1 && std::path::Path::new(path).starts_with(&stored))
            || (root == 0 && std::path::Path::new(path) == std::path::Path::new(&stored))
        {
            return Err(Error::conflict(
                "source_is_library",
                "Source is inside a managed library or is a tracked media path",
            ));
        }
    }
    Ok(())
}
// Mirrors migration 0032's `rescan_blocks_import_update` trigger, which only fires on the
// preview->non-preview transition: a pre-check here turns that RAISE into a clean 409 instead
// of the generic 500 `Error::from(libsql::Error)` would otherwise produce.
async fn rescan_active(c: &Connection, t: &MediaTarget) -> Result<bool> {
    Ok(match t {
        MediaTarget::Episode(id) => c.query("SELECT 1 FROM rescan_commands r JOIN episodes e ON e.series_id=r.series_id WHERE r.status IN ('queued','running','retry_wait') AND r.media_type='tv' AND e.id=?",[*id]).await?.next().await?.is_some(),
        MediaTarget::Movie(id) => c.query("SELECT 1 FROM rescan_commands r WHERE r.status IN ('queued','running','retry_wait') AND r.media_type='movies' AND r.movie_id=?",[*id]).await?.next().await?.is_some(),
    })
}
async fn committed_owner(c: &Connection, opid: &str, t: &MediaTarget, p: &Plan) -> Result<()> {
    let sql = match t {
        MediaTarget::Episode(_) => {
            "SELECT s.id,s.path,f.path FROM import_history h JOIN episodes e ON e.id=h.episode_id JOIN series s ON s.id=e.series_id JOIN episode_files f ON f.id=h.episode_file_id AND f.id=e.episode_file_id WHERE h.operation_id=?"
        }
        MediaTarget::Movie(_) => {
            "SELECT m.id,m.path,f.path FROM import_history h JOIN movies m ON m.id=h.movie_id JOIN movie_files f ON f.id=h.movie_file_id AND f.movie_id=m.id WHERE h.operation_id=?"
        }
    };
    let row = c.query(sql, [opid]).await?.next().await?.ok_or_else(|| {
        Error::conflict(
            "target_changed",
            "Committed file association changed; source cleanup is paused",
        )
    })?;
    if row.get::<i64>(0)? != p.owner_id
        || row.get::<String>(1)? != p.root
        || row.get::<String>(2)? != p.destination
    {
        return Err(Error::conflict(
            "target_changed",
            "Committed library ownership changed; source cleanup is paused",
        ));
    }
    scan_owned::committed_members(c, opid).await?;
    source_unmanaged(c, &p.source).await
}
fn json<T: Serialize>(v: &T) -> Result<String> {
    serde_json::to_string(v).map_err(|_| Error::internal())
}
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| Error::internal())?
}
pub async fn preview(db: Arc<Database>, input: ImportInput) -> Result<Operation> {
    local(&db)?;
    let req = input.request()?;
    let c = db.connect().await?;
    let (owner_id, root, _) = owner(&c, &req.target).await?;
    destination_free(&c, &req.destination).await?;
    source_unmanaged(&c, &req.source).await?;
    let opid = Uuid::new_v4();
    let text = opid.to_string();
    let sid = text.clone();
    let plan = blocking(move || {
        Plan::preview(owner_id, root, req.source, req.destination, req.mode, &sid)
    })
    .await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let current = owner(&tx, &req.target).await?;
    if current.0 != plan.owner_id || current.1 != plan.root {
        tx.rollback().await?;
        return Err(Error::conflict(
            "target_changed",
            "Library path or ownership changed during preview",
        ));
    }
    let (domain, episode, movie) = target(&req.target);
    tx.execute("INSERT INTO operations(id,media_type,episode_id,movie_id,source,mode,destination,status,message) VALUES(?,?,?,?,?,?,?,'preview','Manual import preview')",params![text.clone(),domain,episode,movie,plan.source.clone(),plan.mode.name(),plan.destination.clone()]).await?;
    tx.execute(
        "INSERT INTO import_journal(operation_id,plan_json,phase)VALUES(?,?,'preview')",
        params![text.clone(), json(&plan)?],
    )
    .await?;
    tx.commit().await?;
    status(db, &text).await
}
fn checked_text(row: &libsql::Row, index: i32, limit: usize) -> Result<Option<String>> {
    row.get::<Option<Vec<u8>>>(index)?
        .map(|bytes| {
            if bytes.len() > limit || bytes.contains(&0) {
                return Err(Error::internal());
            }
            String::from_utf8(bytes).map_err(|_| Error::internal())
        })
        .transpose()
}
pub(crate) fn checked_path_failure(row: &libsql::Row, index: i32) -> Result<Option<String>> {
    let code = checked_text(row, index, 64)?;
    if let Some(code) = code.as_deref() {
        let valid = code == "unresolved_attempt" || {
            let parts: Vec<_> = code.split('_').collect();
            parts.len() == 3
                && matches!(
                    parts[0],
                    "source" | "root" | "destination" | "stage" | "retirement"
                )
                && matches!(
                    parts[1],
                    "missing" | "denied" | "changed" | "invalid" | "io"
                )
                && (parts[2] == "cleanup" || (parts[2] == "precommit" && parts[0] != "retirement"))
        };
        if !valid {
            return Err(Error::internal());
        }
    }
    Ok(code)
}
fn media(target: &MediaTarget) -> crate::api::MediaDomain {
    match target {
        MediaTarget::Episode(_) => crate::api::MediaDomain::Tv,
        MediaTarget::Movie(_) => crate::api::MediaDomain::Movies,
    }
}
async fn diagnostic_changed(
    c: &Connection,
    opid: &str,
    change: crate::health::PathDiagnosticChange,
) -> Result<()> {
    let target = load(c, opid).await?.target;
    crate::health::path_diagnostics_changed(c, media(&target), change)
        .await
        .map_err(|_| Error::internal())
}
async fn load(c: &Connection, opid: &str) -> Result<Record> {
    Uuid::parse_str(opid).map_err(|_| Error::bad("Operation ID must be a UUID"))?;
    let row=c.query("SELECT o.media_type,o.episode_id,o.movie_id,coalesce(j.phase,o.status),CASE WHEN j.plan_json IS NULL THEN NULL WHEN typeof(j.plan_json)='text' THEN substr(CAST(j.plan_json AS BLOB),1,16385) ELSE x'00' END,CASE WHEN j.stage_json IS NULL THEN NULL WHEN typeof(j.stage_json)='text' THEN substr(CAST(j.stage_json AS BLOB),1,4097) ELSE x'00' END,j.error_code,CASE WHEN j.path_failure_code IS NULL THEN NULL WHEN typeof(j.path_failure_code)='text' THEN substr(CAST(j.path_failure_code AS BLOB),1,65) ELSE x'00' END FROM operations o LEFT JOIN import_journal j ON j.operation_id=o.id WHERE o.id=?",[opid]).await?.next().await?.ok_or_else(Error::missing)?;
    Ok(Record {
        id: opid.into(),
        target: match row.get::<String>(0)?.as_str() {
            "episode" => MediaTarget::Episode(row.get(1)?),
            "movie" => MediaTarget::Movie(row.get(2)?),
            _ => return Err(Error::internal()),
        },
        phase: row.get(3)?,
        plan: checked_text(&row, 4, 16384)?
            .map(|s| serde_json::from_str(&s).map_err(|_| Error::internal()))
            .transpose()?,
        stage: checked_text(&row, 5, 4096)?
            .map(|s| serde_json::from_str(&s).map_err(|_| Error::internal()))
            .transpose()?,
        error: row.get(6)?,
        path_failure: checked_path_failure(&row, 7)?,
    })
}
pub async fn status(db: Arc<Database>, opid: &str) -> Result<Operation> {
    let c = db.connect().await?;
    status_in(&c, opid).await
}
// Reuse the canonical projection inside a caller-owned read snapshot.
pub(crate) async fn status_in(c: &Connection, opid: &str) -> Result<Operation> {
    let mut response = load(c, opid).await?.response()?;
    if scan_owned::is_scan(c, opid).await? {
        response.message =
            "Downloaded scan import; execution and recovery belong to its scan command".into();
        return Ok(response);
    }
    if let Some(row)=c.query("SELECT old_file_json IS NOT NULL,retirement_state FROM rss_candidate_imports WHERE operation_id=?",[opid]).await?.next().await? {
        response.message=if row.get::<i64>(0)?==0 {"Owned download import; source bytes retained"} else {match row.get::<String>(1)?.as_str(){"quarantined"=>"Replacement imported; original bytes retained in private recovery artifact","shared_retained"=>"Replacement imported; original still serves other episodes",_=>"Owned replacement; original retirement pending and bytes retained"}}.into();
        // The recycle outcome refines the quarantined message; the code names a retained cause.
        if let Some((phase, _)) = recycle::summary(c, opid).await? {
            response.message = match phase.as_str() {
                "complete" => "Replacement imported; original moved to the recycle bin",
                "retained" => "Replacement imported; recycling the original failed, bytes retained in private recovery artifact",
                _ => "Replacement imported; moving the original to the recycle bin is pending, bytes retained",
            }
            .into();
        }
    }
    Ok(response)
}
// ponytail: one import at a time per process; per-library workers when throughput requires it.
static EXECUTING: AtomicBool = AtomicBool::new(false);
const MAX_FAILED_OWNED: usize = 1024; // Same cap as durable RSS receipts; no eviction of failed work.
static FAILED_OWNED: std::sync::Mutex<std::collections::BTreeSet<String>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());
static FAILED_LATCH_UNAVAILABLE: AtomicBool = AtomicBool::new(false);
fn failed_owned(opid: &str) -> Result<bool> {
    Ok(FAILED_LATCH_UNAVAILABLE.load(Ordering::Acquire)
        || FAILED_OWNED
            .lock()
            .map_err(|_| Error::internal())?
            .contains(opid))
}
fn latch_owned_failure(opid: &str) {
    match FAILED_OWNED.lock() {
        Ok(mut failed) if failed.contains(opid) || failed.len() < MAX_FAILED_OWNED => {
            failed.insert(opid.into());
        }
        _ => {
            FAILED_LATCH_UNAVAILABLE.store(true, Ordering::Release);
            eprintln!(
                "{}",
                serde_json::json!({"level":"ERROR","component":"owned_import","condition":"failure_latch_unavailable","operation_id":opid})
            );
        }
    }
}
static ACTIVE_OPERATION: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StartOwned {
    Started,
    Active,
    Busy,
    Complete,
}
struct Permit {
    _db: Option<Arc<Database>>,
}
impl Drop for Permit {
    fn drop(&mut self) {
        // Release the worker's database ownership before publishing that it has finished.
        drop(self._db.take());
        if let Ok(mut active) = ACTIVE_OPERATION.lock() {
            *active = None;
        }
        EXECUTING.store(false, Ordering::Release);
    }
}
fn acquire(db: &Arc<Database>, opid: &str) -> Option<Arc<Permit>> {
    let mut active = ACTIVE_OPERATION.lock().ok()?;
    EXECUTING
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .ok()?;
    *active = Some(opid.into());
    Some(Arc::new(Permit {
        _db: Some(db.clone()),
    }))
}
pub(crate) fn operation_active(operation_id: &str) -> Result<bool> {
    Ok(ACTIVE_OPERATION
        .lock()
        .map_err(|_| Error::internal())?
        .as_deref()
        == Some(operation_id))
}
pub(crate) struct ResumeGuard {
    _permit: Arc<Permit>,
}
pub(crate) fn acquire_resume_guard(db: &Arc<Database>) -> Result<ResumeGuard> {
    // An empty owner cannot equal an operation UUID; reservation still excludes execution.
    acquire(db, "")
        .map(|permit| ResumeGuard { _permit: permit })
        .ok_or_else(|| {
            Error::conflict(
                "import_busy",
                "Another import is executing; retry this operation",
            )
        })
}
/// Downloaded-scan directory cleanup runs under the same single global effect permit as every
/// import, with an owner key that can never equal an operation UUID.
pub(crate) struct CleanupGuard {
    _permit: Arc<Permit>,
}
pub(crate) fn acquire_cleanup_guard(db: &Arc<Database>, command_id: &str) -> Result<CleanupGuard> {
    acquire(db, &format!("scan-cleanup-{command_id}"))
        .map(|permit| CleanupGuard { _permit: permit })
        .ok_or_else(|| {
            Error::conflict(
                "import_busy",
                "Another import is executing; retry this cleanup",
            )
        })
}
/// Explicit library-file deletion and recycle-bin cleanup share the single global effect
/// permit, so no import, scan cleanup or other deletion touches files while they run.
pub(crate) struct DeletionGuard {
    _permit: Arc<Permit>,
}
/// Worker-startup roll-forward of upgrade recycles interrupted between artifact and bin.
pub(crate) async fn recover_upgrade_recycles(db: &Database, guard: &DeletionGuard) -> Result<()> {
    recycle::recover(db, guard).await
}
pub(crate) fn acquire_deletion_guard(db: &Arc<Database>, key: &str) -> Result<DeletionGuard> {
    acquire(db, &format!("file-deletion-{key}"))
        .map(|permit| DeletionGuard { _permit: permit })
        .ok_or_else(|| {
            Error::conflict(
                "import_busy",
                "Another import or file operation is executing; retry",
            )
        })
}
fn launch(
    db: Arc<Database>,
    opid: String,
    permit: Arc<Permit>,
    owned: bool,
) -> tokio::task::JoinHandle<Result<Operation>> {
    tokio::spawn(async move {
        let result = run(db.clone(), &opid, &permit).await;
        if let Err(e) = &result {
            if owned && !matches!(e.code, "rescan_active" | "import_busy") {
                latch_owned_failure(&opid);
            }
            eprintln!(
                "{}",
                serde_json::json!({"level":"ERROR","component":"manual_import","operation_id":opid,"code":e.code,"diagnostic":e.diagnostic})
            );
            if e.started_attempt || !matches!(e.code, "rescan_active" | "import_busy") {
                let persisted = async {
                    let c = db.connect().await?;
                    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate).await?;
                    tx.execute("UPDATE import_journal SET error_code=?,path_failure_code=coalesce(?,path_failure_code),updated_at=CURRENT_TIMESTAMP WHERE operation_id=?", params![e.code,e.path_failure_code(),opid.clone()]).await?;
                    diagnostic_changed(&tx, &opid, crate::health::PathDiagnosticChange::Failure).await?;
                    tx.commit().await?;
                    Ok::<(),Error>(())
                }.await;
                if let Err(error) = persisted {
                    eprintln!(
                        "{}",
                        serde_json::json!({"level":"ERROR","component":"manual_import","operation_id":opid,"condition":"error_status_persistence_failed","diagnostic":error.diagnostic})
                    );
                }
            }
        }
        result
    })
}
async fn consume_owned_start(c: &Connection, opid: &str, explicit_manual: bool) -> Result<()> {
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let r=tx.query("SELECT j.error_code,d.resume_requested,CASE WHEN j.path_failure_code IS NULL THEN NULL WHEN typeof(j.path_failure_code)='text' THEN substr(CAST(j.path_failure_code AS BLOB),1,65) ELSE x'00' END,CASE WHEN typeof(d.reasons_json)='text' THEN substr(CAST(d.reasons_json AS BLOB),1,8193) ELSE x'00' END FROM import_journal j JOIN rss_candidate_imports i ON i.operation_id=j.operation_id JOIN download_processing d ON d.candidate_id=i.candidate_id WHERE j.operation_id=?",[opid]).await?.next().await?.ok_or_else(Error::missing)?;
    if (r.get::<Option<String>>(0)?.is_some()
        || checked_path_failure(&r, 2)?.is_some()
        || failed_owned(opid)?)
        && r.get::<i64>(1)? == 0
        && !explicit_manual
    {
        return Err(Error::conflict(
            "resume_required",
            "Failed owned import requires an explicit retry",
        ));
    }
    let previous: Vec<String> =
        serde_json::from_str(&checked_text(&r, 3, 8192)?.ok_or_else(Error::internal)?)
            .map_err(|_| Error::internal())?;
    let reasons = crate::commands::processing::path_reasons(
        &previous,
        &[],
        crate::commands::processing::PathReasonChange::Preserve,
    )
    .map_err(|_| Error::internal())?;
    tx.execute("UPDATE download_processing SET resume_requested=0,error_code=NULL,reasons_json=? WHERE candidate_id=(SELECT candidate_id FROM rss_candidate_imports WHERE operation_id=?) AND status='importing'",params![json(&reasons)?,opid]).await?;
    tx.execute(
        "UPDATE import_journal SET error_code=NULL WHERE operation_id=?",
        [opid],
    )
    .await?;
    tx.commit().await?;
    FAILED_OWNED
        .lock()
        .map_err(|_| Error::internal())?
        .remove(opid);
    Ok(())
}
pub(crate) async fn start_owned(db: Arc<Database>, opid: &str) -> Result<StartOwned> {
    local(&db)?;
    let c = db.connect().await?;
    if owned::facts(&c, opid).await?.is_none() {
        return Err(Error::missing());
    }
    let record = load(&c, opid).await?;
    if record.phase == "complete" {
        FAILED_OWNED
            .lock()
            .map_err(|_| Error::internal())?
            .remove(opid);
        return Ok(StartOwned::Complete);
    }
    let Some(permit) = acquire(&db, opid) else {
        let active = ACTIVE_OPERATION.lock().map_err(|_| Error::internal())?;
        return Ok(if active.as_deref() == Some(opid) {
            StartOwned::Active
        } else {
            StartOwned::Busy
        });
    };
    consume_owned_start(&c, opid, false).await?;
    // Lease remains inside the task and each blocking closure after its caller is dropped.
    drop(launch(db, opid.into(), permit, true));
    Ok(StartOwned::Started)
}
pub async fn execute(db: Arc<Database>, opid: &str) -> Result<Operation> {
    execute_with_authority(db, opid, true).await
}
// A queued command authorizes an initial attempt, not recovery of a failure that raced its read.
pub(crate) async fn execute_command(db: Arc<Database>, opid: &str) -> Result<Operation> {
    execute_with_authority(db, opid, false).await
}
async fn execute_with_authority(
    db: Arc<Database>,
    opid: &str,
    explicit_recovery: bool,
) -> Result<Operation> {
    local(&db)?;
    Uuid::parse_str(opid).map_err(|_| Error::bad("Operation ID must be a UUID"))?;
    // Scan ownership is immutable once the operation exists: its child link, parent
    // cancellation and resume generations are the only authority that may start it.
    if scan_owned::is_scan(&db.connect().await?, opid).await? {
        return Err(Error::conflict(
            "scan_recovery_required",
            "This import belongs to a downloaded-folder scan; resume it through the scan command",
        ));
    }
    let permit = acquire(&db, opid).ok_or_else(|| {
        Error::conflict(
            "import_busy",
            "Another import is executing; retry this operation",
        )
    })?;
    let c = db.connect().await?;
    if !explicit_recovery {
        // Retain this permit through launch: a separate read followed by execute could race
        // a direct invocation that arms, fails, and releases its lease between those steps.
        let record = load(&c, opid).await?;
        if record.phase != "complete" && (record.error.is_some() || record.path_failure.is_some()) {
            return Err(Error::conflict(
                "resume_required",
                "An admitted or failed import requires explicit recovery",
            ));
        }
    }
    let is_owned = owned::facts(&c, opid).await?.is_some();
    if is_owned {
        consume_owned_start(&c, opid, explicit_recovery).await?;
    }
    launch(db, opid.into(), permit, is_owned)
        .await
        .map_err(|_| Error::internal())?
}
async fn checkpoint(c: &Connection, opid: &str, phase: &str, stage: Option<&Stage>) -> Result<()> {
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    tx.execute("UPDATE import_journal SET phase=?,stage_json=?,error_code=NULL,path_failure_code=CASE WHEN ?='complete' THEN NULL ELSE path_failure_code END,updated_at=CURRENT_TIMESTAMP WHERE operation_id=?",params![phase,stage.map(json).transpose()?,phase,opid]).await?;
    tx.execute(
        "UPDATE operations SET status=?,message='Manual import progress' WHERE id=?",
        params![phase, opid],
    )
    .await?;
    diagnostic_changed(
        &tx,
        opid,
        if phase == "complete" {
            crate::health::PathDiagnosticChange::Completion
        } else {
            crate::health::PathDiagnosticChange::Progress
        },
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
async fn leased<T: Send + 'static>(
    lease: &Arc<Permit>,
    f: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    let lease = lease.clone();
    blocking(move || {
        let _lease = lease;
        f()
    })
    .await
}
async fn run(db: Arc<Database>, opid: &str, lease: &Arc<Permit>) -> Result<Operation> {
    let rec = arm(&db, opid).await?;
    if rec.phase == "complete" {
        return status(db, opid).await;
    }
    let outcome = run_inner(db.clone(), opid, lease, rec)
        .await
        .map_err(|mut error| {
            error.started_attempt = true;
            error.boundary(PathBoundary::Precommit)
        });
    if outcome.is_err() {
        if let Err(error) = owned::restore_uncommitted(&db, opid, lease).await {
            eprintln!(
                "event=import_restore_failed operation_id={} code={} diagnostic={:?}",
                opid, error.code, error.diagnostic
            );
        }
    }
    outcome
}
async fn arm(db: &Database, opid: &str) -> Result<Record> {
    let c = db.connect().await?;
    let mut rec = load(&c, opid).await?;
    if rec.phase == "complete" {
        return Ok(rec);
    }
    let plan = rec.plan.as_ref().ok_or_else(|| {
        Error::conflict(
            "preview_required",
            "Legacy preview lacks validated identities; create a new preview",
        )
    })?;
    if matches!(
        rec.phase.as_str(),
        "preview" | "staging" | "staged" | "published"
    ) {
        source_unmanaged(&c, &plan.source).await?;
    }
    if rec.phase == "preview" {
        let own = owned::before(&c, opid, &rec.target).await?;
        if own.0 != plan.owner_id || own.1 != plan.root {
            return Err(Error::conflict(
                "target_changed",
                "Library path changed since preview",
            ));
        }
        owned::available(&c, opid, &rec.target, &plan).await?;
        // Reserve the non-preview phase with the marker so no rescan can enter after admission.
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        if rescan_active(&tx, &rec.target).await? {
            tx.rollback().await?;
            return Err(Error::conflict(
                "rescan_active",
                "A library rescan is in progress for this target; retry once it completes",
            ));
        }
        tx.execute(
            "UPDATE import_journal SET phase='staging',stage_json=NULL,error_code=NULL,path_failure_code=coalesce(path_failure_code,'unresolved_attempt'),updated_at=CURRENT_TIMESTAMP WHERE operation_id=?",
            [opid],
        )
        .await?;
        tx.execute(
            "UPDATE operations SET status='staging',message='Manual import progress' WHERE id=?",
            [opid],
        )
        .await?;
        diagnostic_changed(&tx, opid, crate::health::PathDiagnosticChange::Admission).await?;
        tx.commit().await?;
        rec.phase = "staging".into();
    } else {
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        tx.execute("UPDATE import_journal SET path_failure_code=coalesce(path_failure_code,'unresolved_attempt') WHERE operation_id=?", [opid]).await?;
        diagnostic_changed(&tx, opid, crate::health::PathDiagnosticChange::Admission).await?;
        tx.commit().await?;
    }
    Ok(rec)
}
async fn run_inner(
    db: Arc<Database>,
    opid: &str,
    lease: &Arc<Permit>,
    mut rec: Record,
) -> Result<Operation> {
    let c = db.connect().await?;
    let plan = rec.plan.take().ok_or_else(Error::internal)?;
    if rec.phase == "staging" {
        if rec.stage.is_none() {
            let p = plan.clone();
            let stage = leased(lease, move || p.create_stage()).await?;
            checkpoint(&c, opid, "staging", Some(&stage)).await?;
            rec.stage = Some(stage);
        }
        let p = plan.clone();
        let stage = rec.stage.take().ok_or_else(Error::internal)?;
        let stage = leased(lease, move || p.transfer(stage)).await?;
        checkpoint(&c, opid, "staged", Some(&stage)).await?;
        rec.stage = Some(stage);
        rec.phase = "staged".into();
    }
    let mut stage = rec.stage.ok_or_else(Error::internal)?;
    let same_path = owned::placement_old(&c, opid, &plan).await?.is_some();
    if same_path && matches!(rec.phase.as_str(), "staged" | "published") {
        owned::before(&c, opid, &rec.target).await?;
        owned::install(&c, opid, &rec.target, &plan, &stage, lease).await?;
    }
    if rec.phase == "staged" {
        let own = owned::before(&c, opid, &rec.target).await?;
        if own.0 != plan.owner_id || own.1 != plan.root {
            return Err(Error::conflict(
                "target_changed",
                "Library path changed since preview",
            ));
        }
        owned::available(&c, opid, &rec.target, &plan).await?;
        let p = plan.clone();
        let s = stage.clone();
        if !same_path {
            leased(lease, move || p.publish(&s)).await?;
        }
        checkpoint(&c, opid, "published", Some(&stage)).await?;
        rec.phase = "published".into();
    }
    if rec.phase == "published" {
        let p = plan.clone();
        let s = stage.clone();
        leased(lease, move || p.verify_destination(&s)).await?;
        // Recovery also warms current scoring before acquiring the writer lock.
        owned::before(&c, opid, &rec.target).await?;
        commit(&c, opid, &rec.target, &plan, &stage).await?;
        rec.phase = "committed".into();
    }
    if rec.phase == "committed" {
        committed_owner(&c, opid, &rec.target, &plan).await?;
        let p = plan.clone();
        stage = leased(lease, move || p.prepare_cleanup(stage))
            .await
            .map_err(|e| e.boundary(PathBoundary::Cleanup))?;
        checkpoint(&c, opid, "committed", Some(&stage)).await?;
        committed_owner(&c, opid, &rec.target, &plan).await?;
        let p = plan.clone();
        stage = leased(lease, move || p.retire_source(stage))
            .await
            .map_err(|e| e.boundary(PathBoundary::Cleanup))?;
        checkpoint(&c, opid, "committed", Some(&stage)).await?;
        committed_owner(&c, opid, &rec.target, &plan).await?;
        owned::retire(&c, opid, &rec.target, &plan, &stage, lease)
            .await
            .map_err(|e| e.boundary(PathBoundary::Cleanup))?;
        // Journaled part of retirement: with a configured bin, each quarantined original moves
        // into it (or is recorded as retained) before the import can complete.
        committed_owner(&c, opid, &rec.target, &plan).await?;
        recycle::settle(&c, opid, &rec.target, &plan, lease)
            .await
            .map_err(|e| e.boundary(PathBoundary::Cleanup))?;
        let p = plan.clone();
        let s = stage.clone();
        leased(lease, move || p.cleanup(&s))
            .await
            .map_err(|e| e.boundary(PathBoundary::Cleanup))?;
        checkpoint(&c, opid, "complete", Some(&stage)).await?;
    }
    status(db, opid).await
}
async fn commit(
    c: &Connection,
    opid: &str,
    t: &MediaTarget,
    plan: &Plan,
    stage: &Stage,
) -> Result<()> {
    if scan_owned::is_scan(c, opid).await? {
        return scan_owned::commit(c, opid, t, plan, stage).await;
    }
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome=async {
        let (owner_id,root,season)=owned::before(&tx,opid,t).await?;let facts=owned::facts(&tx,opid).await?;let old_id=facts.as_ref().and_then(|f|f.old.as_ref()).map(|o|o.file_id);if owner_id!=plan.owner_id || root!=plan.root{return Err(Error::conflict("target_changed","Library ownership changed before commit"));}
        owned::available(&tx,opid,t,plan).await?;
        let same_path=facts.as_ref().and_then(|f|f.old.as_ref()).is_some_and(|o|o.path==plan.destination);
        let mut expected_old=old_id;
        if same_path && matches!(t,MediaTarget::Episode(_)) {
            let old=facts.as_ref().and_then(|f|f.old.as_ref()).ok_or_else(Error::internal)?;
            if tx.execute("UPDATE episodes SET episode_file_id=NULL WHERE id=? AND episode_file_id=?",params![id(t),old.file_id]).await?!=1{return Err(Error::conflict("target_changed","Episode association changed"));}
            if tx.execute("UPDATE episode_files SET path=? WHERE id=? AND path=?",params![old.recovery_path()?,old.file_id,old.path.clone()]).await?!=1{return Err(Error::conflict("target_changed","Old file path changed"));}
            expected_old=None;
        }
        let size=i64::try_from(stage.size()).map_err(|_|Error::internal())?;
        let (domain,episode,movie)=target(t);
        let (file_episode,file_movie)=match t {
            MediaTarget::Episode(episode)=>{
                tx.execute("INSERT INTO episode_files(series_id,path)VALUES(?,?)",params![owner_id,plan.destination.clone()]).await?;let fid=tx.last_insert_rowid();
                tx.execute("INSERT INTO file_metadata(media_type,episode_file_id,size,date_added,season_number)VALUES('tv',?,?,strftime('%Y-%m-%dT%H:%M:%SZ','now'),?)",params![fid,size,season]).await?;
                if tx.execute("UPDATE episodes SET episode_file_id=? WHERE id=? AND episode_file_id IS ?",params![fid,*episode,expected_old]).await?!=1{return Err(Error::conflict("target_changed","Episode association changed"));}(Some(fid),None)
            },MediaTarget::Movie(movie)=>{
                let fid=if let Some(old)=old_id {tx.execute("UPDATE movie_files SET path=?,edition=NULL WHERE id=? AND movie_id=?",params![plan.destination.clone(),old,*movie]).await?;tx.execute("DELETE FROM file_metadata WHERE movie_file_id=?",[old]).await?;old}else{tx.execute("INSERT INTO movie_files(movie_id,path)VALUES(?,?)",params![*movie,plan.destination.clone()]).await?;tx.last_insert_rowid()};
                tx.execute("INSERT INTO file_metadata(media_type,movie_file_id,size,date_added,original_file_path)VALUES('movies',?,?,strftime('%Y-%m-%dT%H:%M:%SZ','now'),?)",params![fid,size,plan.source.clone()]).await?;(None,Some(fid))
            }
        };
        if let Some(facts)=facts { let (scope,column,fid)=match t {MediaTarget::Episode(_)=>("tv","episode_file_id",file_episode),MediaTarget::Movie(_)=>("movies","movie_file_id",file_movie)};
            tx.execute(&format!("UPDATE file_metadata SET quality_id=?,revision_json=? WHERE media_type=? AND {column}=?"),params![facts.quality_id,facts.revision_json,scope,fid]).await?;
            if let Some(evidence)=facts.evidence {tx.execute(&format!("UPDATE file_metadata SET original_release_title=?,languages_json=?,release_group=?,indexer_flags=?,release_type=? WHERE media_type=? AND {column}=?"),params![evidence.title,evidence.languages.as_ref().map(json).transpose()?,evidence.release_group,evidence.indexer_flags,evidence.release_type,scope,fid]).await?;}
            if let Some(fid)=file_movie {tx.execute("UPDATE movie_files SET edition=? WHERE id=?",params![facts.edition,fid]).await?;}
        }
        // Explicit manual target mappings retain their established override semantics.
        // Preserve source facts for future current-definition comparisons even for an
        // arbitrary filename; only facts the native parser recognizes are populated.
        if owned::facts(&tx,opid).await?.is_none() {
            let basename=plan.source.rsplit(['/', '\\']).next().unwrap_or(&plan.source);
            let stem=basename.rsplit_once('.').map_or(basename,|(s,_)|if s.is_empty(){basename}else{s});
            let tv=matches!(t,MediaTarget::Episode(_));
            let (scope,column,fid)=if tv {("tv","episode_file_id",file_episode)}else{("movies","movie_file_id",file_movie)};
            tx.execute(&format!("UPDATE file_metadata SET original_release_title=? WHERE media_type=? AND {column}=?"),params![stem,scope,fid]).await?;
            if let Ok(parsed)=crate::search::parser::parse(stem,tv){
                let evidence=crate::custom_formats::parsed(stem,&parsed,tv,Some(stage.size()));
                let quality=if let Some(name)=parsed.quality_name {tx.query("SELECT quality_id FROM quality_definitions WHERE media_type=? AND name=?",params![scope,name]).await?.next().await?.map(|r|r.get::<i64>(0)).transpose()?}else{None};
                tx.execute(&format!("UPDATE file_metadata SET quality_id=?,languages_json=?,release_group=?,release_type=?,revision_json=? WHERE media_type=? AND {column}=?"),params![quality,evidence.languages.as_ref().map(json).transpose()?,evidence.release_group,evidence.release_type,if parsed.revision_marker { parsed.revision.as_ref().map(json).transpose()? } else { None },scope,fid]).await?;
                if let Some(fid)=file_movie {tx.execute("UPDATE movie_files SET edition=? WHERE id=?",params![evidence.edition,fid]).await?;}
            }
        }
        tx.execute("INSERT INTO import_history(operation_id,media_type,episode_id,movie_id,episode_file_id,movie_file_id,source,destination,size,sha256)VALUES(?,?,?,?,?,?,?,?,?,?)",params![opid,domain,episode,movie,file_episode,file_movie,plan.source.clone(),plan.destination.clone(),size,stage.sha256.clone().ok_or_else(Error::internal)?]).await?;
        tx.execute("UPDATE import_journal SET phase='committed',error_code=NULL,updated_at=CURRENT_TIMESTAMP WHERE operation_id=?",[opid]).await?;
        tx.execute("UPDATE operations SET status='committed' WHERE id=?",[opid]).await?;diagnostic_changed(&tx,opid,crate::health::PathDiagnosticChange::Progress).await?;Ok(())
    }.await;
    match outcome {
        Ok(()) => {
            tx.commit().await?;
            Ok(())
        }
        Err(e) => {
            tx.rollback().await?;
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests;
