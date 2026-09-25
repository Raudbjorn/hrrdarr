//! Initial manual imports and receipt-bound singleton replacements with durable recovery.
mod fs;
mod owned;
pub(crate) use owned::{OwnedImport, prepare_owned};

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
use fs::{Plan, Stage};
use libsql::{Connection, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use ts_rs::TS;
use uuid::Uuid;

#[derive(Debug)]
pub struct Error {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
    diagnostic: Option<String>,
}
type Result<T> = std::result::Result<T, Error>;
const MAX_SOURCE_GUARD_ROWS: usize = 10_000;
impl Error {
    pub(crate) fn code(&self) -> &'static str {
        self.code
    }

    fn bad(message: &'static str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_request",
            message,
            diagnostic: None,
        }
    }
    fn conflict(code: &'static str, message: &'static str) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code,
            message,
            diagnostic: None,
        }
    }
    fn missing() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "not_found",
            message: "Import operation or media target was not found",
            diagnostic: None,
        }
    }
    fn internal() -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "import_error",
            message: "Import state could not be read or persisted; retry using the same operation ID",
            diagnostic: None,
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
}
impl Record {
    fn response(self) -> Result<Operation> {
        Ok(Operation {
            id: Uuid::parse_str(&self.id).map_err(|_| Error::internal())?,
            target: self.target,
            status: self.phase,
            message:
                "Manual import status; execute explicitly authorizes or resumes this operation"
                    .into(),
            error_code: self.error,
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
async fn load(c: &Connection, opid: &str) -> Result<Record> {
    Uuid::parse_str(opid).map_err(|_| Error::bad("Operation ID must be a UUID"))?;
    let row=c.query("SELECT o.media_type,o.episode_id,o.movie_id,coalesce(j.phase,o.status),j.plan_json,j.stage_json,j.error_code FROM operations o LEFT JOIN import_journal j ON j.operation_id=o.id WHERE o.id=?",[opid]).await?.next().await?.ok_or_else(Error::missing)?;
    Ok(Record {
        id: opid.into(),
        target: match row.get::<String>(0)?.as_str() {
            "episode" => MediaTarget::Episode(row.get(1)?),
            "movie" => MediaTarget::Movie(row.get(2)?),
            _ => return Err(Error::internal()),
        },
        phase: row.get(3)?,
        plan: row
            .get::<Option<String>>(4)?
            .map(|s| serde_json::from_str(&s).map_err(|_| Error::internal()))
            .transpose()?,
        stage: row
            .get::<Option<String>>(5)?
            .map(|s| serde_json::from_str(&s).map_err(|_| Error::internal()))
            .transpose()?,
        error: row.get(6)?,
    })
}
pub async fn status(db: Arc<Database>, opid: &str) -> Result<Operation> {
    let c = db.connect().await?;
    let mut response = load(&c, opid).await?.response()?;
    if let Some(row)=c.query("SELECT old_file_json IS NOT NULL,retirement_state FROM rss_candidate_imports WHERE operation_id=?",[opid]).await?.next().await? {
        response.message=if row.get::<i64>(0)?==0 {"Owned download import; source bytes retained"} else {match row.get::<String>(1)?.as_str(){"quarantined"=>"Replacement imported; original bytes retained in private recovery artifact","shared_retained"=>"Replacement imported; original still serves other episodes",_=>"Owned replacement; original retirement pending and bytes retained"}}.into();
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
fn launch(
    db: Arc<Database>,
    opid: String,
    permit: Arc<Permit>,
    owned: bool,
) -> tokio::task::JoinHandle<Result<Operation>> {
    tokio::spawn(async move {
        let result = run(db.clone(), &opid, &permit).await;
        if let Err(e) = &result {
            if owned {
                latch_owned_failure(&opid);
            }
            eprintln!(
                "{}",
                serde_json::json!({"level":"ERROR","component":"manual_import","operation_id":opid,"code":e.code,"diagnostic":e.diagnostic})
            );
            let persisted=async{let c=db.connect().await?;c.execute("UPDATE import_journal SET error_code=?,updated_at=CURRENT_TIMESTAMP WHERE operation_id=?",params![e.code,opid.clone()]).await?;Ok::<(),libsql::Error>(())}.await;
            if let Err(error) = persisted {
                let error = Error::from(error);
                eprintln!(
                    "{}",
                    serde_json::json!({"level":"ERROR","component":"manual_import","operation_id":opid,"condition":"error_status_persistence_failed","diagnostic":error.diagnostic})
                );
            }
        }
        result
    })
}
async fn consume_owned_start(c: &Connection, opid: &str, explicit_manual: bool) -> Result<()> {
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let r=tx.query("SELECT j.error_code,d.resume_requested FROM import_journal j JOIN rss_candidate_imports i ON i.operation_id=j.operation_id JOIN download_processing d ON d.candidate_id=i.candidate_id WHERE j.operation_id=?",[opid]).await?.next().await?.ok_or_else(Error::missing)?;
    if (r.get::<Option<String>>(0)?.is_some() || failed_owned(opid)?)
        && r.get::<i64>(1)? == 0
        && !explicit_manual
    {
        return Err(Error::conflict(
            "resume_required",
            "Failed owned import requires an explicit retry",
        ));
    }
    tx.execute("UPDATE download_processing SET resume_requested=0,error_code=NULL,reasons_json='[]' WHERE candidate_id=(SELECT candidate_id FROM rss_candidate_imports WHERE operation_id=?) AND status='importing'",[opid]).await?;
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
    local(&db)?;
    Uuid::parse_str(opid).map_err(|_| Error::bad("Operation ID must be a UUID"))?;
    let permit = acquire(&db, opid).ok_or_else(|| {
        Error::conflict(
            "import_busy",
            "Another import is executing; retry this operation",
        )
    })?;
    let c = db.connect().await?;
    let is_owned = owned::facts(&c, opid).await?.is_some();
    if is_owned {
        consume_owned_start(&c, opid, true).await?;
    }
    launch(db, opid.into(), permit, is_owned)
        .await
        .map_err(|_| Error::internal())?
}
async fn checkpoint(c: &Connection, opid: &str, phase: &str, stage: Option<&Stage>) -> Result<()> {
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    tx.execute("UPDATE import_journal SET phase=?,stage_json=?,error_code=NULL,updated_at=CURRENT_TIMESTAMP WHERE operation_id=?",params![phase,stage.map(json).transpose()?,opid]).await?;
    tx.execute(
        "UPDATE operations SET status=?,message='Manual import progress' WHERE id=?",
        params![phase, opid],
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
    let c = db.connect().await?;
    let mut rec = load(&c, opid).await?;
    let plan = rec.plan.take().ok_or_else(|| {
        Error::conflict(
            "preview_required",
            "Legacy preview lacks validated identities; create a new preview",
        )
    })?;
    if rec.phase == "complete" {
        return status(db, opid).await;
    }
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
        destination_free(&c, &plan.destination).await?;
        checkpoint(&c, opid, "staging", None).await?;
        rec.phase = "staging".into();
    }
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
    if rec.phase == "staged" {
        let own = owned::before(&c, opid, &rec.target).await?;
        if own.0 != plan.owner_id || own.1 != plan.root {
            return Err(Error::conflict(
                "target_changed",
                "Library path changed since preview",
            ));
        }
        destination_free(&c, &plan.destination).await?;
        let p = plan.clone();
        let s = stage.clone();
        leased(lease, move || p.publish(&s)).await?;
        checkpoint(&c, opid, "published", Some(&stage)).await?;
        rec.phase = "published".into();
    }
    if rec.phase == "published" {
        let p = plan.clone();
        let s = stage.clone();
        leased(lease, move || p.verify_destination(&s)).await?;
        commit(&c, opid, &rec.target, &plan, &stage).await?;
        rec.phase = "committed".into();
    }
    if rec.phase == "committed" {
        committed_owner(&c, opid, &rec.target, &plan).await?;
        let p = plan.clone();
        stage = leased(lease, move || p.prepare_cleanup(stage)).await?;
        checkpoint(&c, opid, "committed", Some(&stage)).await?;
        committed_owner(&c, opid, &rec.target, &plan).await?;
        let p = plan.clone();
        stage = leased(lease, move || p.retire_source(stage)).await?;
        checkpoint(&c, opid, "committed", Some(&stage)).await?;
        committed_owner(&c, opid, &rec.target, &plan).await?;
        owned::retire(&c, opid, &rec.target, &plan, &stage, lease).await?;
        let p = plan.clone();
        let s = stage.clone();
        leased(lease, move || p.cleanup(&s)).await?;
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
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome=async {
        let (owner_id,root,season)=owned::before(&tx,opid,t).await?;let facts=owned::facts(&tx,opid).await?;let old_id=facts.as_ref().and_then(|f|f.old.as_ref()).map(|o|o.file_id);if owner_id!=plan.owner_id || root!=plan.root{return Err(Error::conflict("target_changed","Library ownership changed before commit"));}
        destination_free(&tx,&plan.destination).await?;
        let size=i64::try_from(stage.size()).map_err(|_|Error::internal())?;
        let (domain,episode,movie)=target(t);
        let (file_episode,file_movie)=match t {
            MediaTarget::Episode(episode)=>{
                tx.execute("INSERT INTO episode_files(series_id,path)VALUES(?,?)",params![owner_id,plan.destination.clone()]).await?;let fid=tx.last_insert_rowid();
                tx.execute("INSERT INTO file_metadata(media_type,episode_file_id,size,date_added,season_number)VALUES('tv',?,?,strftime('%Y-%m-%dT%H:%M:%SZ','now'),?)",params![fid,size,season]).await?;
                if tx.execute("UPDATE episodes SET episode_file_id=? WHERE id=? AND episode_file_id IS ?",params![fid,*episode,old_id]).await?!=1{return Err(Error::conflict("target_changed","Episode association changed"));}(Some(fid),None)
            },MediaTarget::Movie(movie)=>{
                let fid=if let Some(old)=old_id {tx.execute("UPDATE movie_files SET path=?,edition=NULL WHERE id=? AND movie_id=?",params![plan.destination.clone(),old,*movie]).await?;tx.execute("DELETE FROM file_metadata WHERE movie_file_id=?",[old]).await?;old}else{tx.execute("INSERT INTO movie_files(movie_id,path)VALUES(?,?)",params![*movie,plan.destination.clone()]).await?;tx.last_insert_rowid()};
                tx.execute("INSERT INTO file_metadata(media_type,movie_file_id,size,date_added,original_file_path)VALUES('movies',?,?,strftime('%Y-%m-%dT%H:%M:%SZ','now'),?)",params![fid,size,plan.source.clone()]).await?;(None,Some(fid))
            }
        };
        if let Some(facts)=facts { let (scope,column,fid)=match t {MediaTarget::Episode(_)=>("tv","episode_file_id",file_episode),MediaTarget::Movie(_)=>("movies","movie_file_id",file_movie)};
            tx.execute(&format!("UPDATE file_metadata SET quality_id=?,revision_json=? WHERE media_type=? AND {column}=?"),params![facts.quality_id,facts.revision_json,scope,fid]).await?;
            if let Some(fid)=file_movie {tx.execute("UPDATE movie_files SET edition=? WHERE id=?",params![facts.edition,fid]).await?;}
        }
        tx.execute("INSERT INTO import_history(operation_id,media_type,episode_id,movie_id,episode_file_id,movie_file_id,source,destination,size,sha256)VALUES(?,?,?,?,?,?,?,?,?,?)",params![opid,domain,episode,movie,file_episode,file_movie,plan.source.clone(),plan.destination.clone(),size,stage.sha256.clone().ok_or_else(Error::internal)?]).await?;
        tx.execute("UPDATE import_journal SET phase='committed',error_code=NULL,updated_at=CURRENT_TIMESTAMP WHERE operation_id=?",[opid]).await?;
        tx.execute("UPDATE operations SET status='committed' WHERE id=?",[opid]).await?;Ok(())
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
