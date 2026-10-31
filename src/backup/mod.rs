//! Backups and restore (job.002 Backup; scn.008 backup/restore) for the local libSQL database.
//!
//! A backup is a consistent online snapshot (`VACUUM INTO` a private staging file, converted to a
//! self-contained rollback-journal file and integrity/foreign-key/migration-history checked), plus
//! a manifest, packed into a stored ZIP and published atomically into
//! `<folder>/{manual,scheduled,update}/`. Archives are addressed by an opaque id derived from their
//! type and name, never by a client-supplied path. Encrypted credentials stay sealed in the copy;
//! the credential key comes from the environment and is never written. Media is never included.
//!
//! A restore validates an archive completely, stages its database beside the live database and
//! leaves a pending marker; the next startup consumes the marker before opening the database and
//! retains the replaced database as a recoverable original. See `docs/backup.md`.
pub(crate) mod archive;
mod engine;
mod restore;

use crate::{api::ApiPage, db::Database};
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Path, Query, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Deserializer, Serialize};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;

pub use engine::{BackupFailure, BackupLimits, Published, create_backup, publish_update};
pub use restore::{RestoreOutcome, apply_pending_restore};

pub const MANIFEST_FORMAT: u32 = 1;
pub(crate) const MANIFEST_NAME: &str = "manifest.json";
pub(crate) const DATABASE_NAME: &str = "database.db";
pub(crate) const ENTRY_NAMES: [&str; 2] = [MANIFEST_NAME, DATABASE_NAME];
pub(crate) const APPLICATION: &str = "hrrdarr";
pub(crate) const BACKEND: &str = "local-libsql";
pub const KEY_SOURCE: &str = "environment variable HRRDARR_PROVIDER_KEY (never stored in backups)";
pub const MIN_INTERVAL_DAYS: i64 = 1;
pub const MAX_INTERVAL_DAYS: i64 = 7;
pub const MIN_RETENTION_DAYS: i64 = 1;
pub const MAX_RETENTION_DAYS: i64 = 90;
const MAX_REVISION: i64 = 9_007_199_254_740_991;
const MAX_PATH_BYTES: usize = 4096;
const REQUEST_SECONDS: u64 = 10;
static INVENTORY_WORK: Semaphore = Semaphore::const_new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum BackupType {
    Scheduled,
    Manual,
    // Published from the pre-migration snapshot when an upgrade applies migrations.
    Update,
}
impl BackupType {
    pub const ALL: [BackupType; 3] = [Self::Scheduled, Self::Manual, Self::Update];
    pub fn dir(self) -> &'static str {
        match self {
            Self::Scheduled => "scheduled",
            Self::Manual => "manual",
            Self::Update => "update",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ManifestMigration {
    pub version: u32,
    pub name: String,
    pub checksum: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ManifestFile {
    pub name: String,
    pub size: u64,
    pub sha256: String,
}
// What a restore needs besides the archive. Never contains key material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ManifestSecrets {
    // Provider and notification rows whose credentials are stored as AEAD ciphertext.
    pub encrypted_credentials: u64,
    pub key_included: bool,
    pub key_source: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct BackupManifest {
    pub format: u32,
    pub application: String,
    pub application_version: String,
    pub backup_type: BackupType,
    pub created_at: i64,
    pub backend: String,
    pub sqlite_version: String,
    pub schema_version: u32,
    pub migrations: Vec<ManifestMigration>,
    pub files: Vec<ManifestFile>,
    pub includes: Vec<String>,
    pub excludes: Vec<String>,
    pub secrets: ManifestSecrets,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
pub struct BackupFile {
    // Opaque: derived from type and name. Download, delete and restore resolve it by listing.
    pub id: String,
    pub name: String,
    pub backup_type: BackupType,
    pub size_bytes: u64,
    pub created_at: i64,
}
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct BackupDetail {
    pub backup: BackupFile,
    pub manifest: BackupManifest,
}

#[derive(Debug, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields)]
pub struct BackupQuery {
    #[serde(default = "default_limit")]
    #[ts(as = "Option<u16>", optional)]
    pub limit: u16,
    #[serde(default)]
    #[ts(as = "Option<u32>", optional)]
    pub offset: u32,
    #[ts(optional)]
    pub backup_type: Option<BackupType>,
}
fn default_limit() -> u16 {
    50
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
pub struct BackupSettings {
    pub revision: i64,
    // Absent means the default `Backups` folder beside the database file.
    pub folder: Option<String>,
    // Absolute folder backups are written to; absent for remote storage.
    pub effective_folder: Option<String>,
    pub interval_days: i64,
    // Scheduled and update backups older than this are pruned after a successful backup of the
    // same type. Manual backups are only ever deleted explicitly.
    pub retention_days: i64,
    pub next_backup_at: Option<i64>,
}
fn required<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Option<String>, D::Error> {
    Option::<String>::deserialize(d)
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct BackupSettingsUpdate {
    pub revision: i64,
    // Required; null or blank selects the default folder.
    #[serde(deserialize_with = "required")]
    pub folder: Option<String>,
    pub interval_days: i64,
    pub retention_days: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum RestoreSource {
    Backup,
    Upload,
}
// A validated restore staged beside the live database; applied by the next startup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct PendingRestore {
    pub source: RestoreSource,
    pub backup_id: Option<String>,
    pub backup_name: Option<String>,
    pub backup_type: BackupType,
    pub backup_created_at: i64,
    pub schema_version: u32,
    pub application_version: String,
    pub staged_at: i64,
    pub database_sha256: String,
    pub database_bytes: u64,
    // Always true: live state is only replaced at the next startup.
    pub restart_required: bool,
}
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct RestoreStatus {
    pub pending: Option<PendingRestore>,
}

/// HTTP error: status, stable code and a message that never echoes paths or secrets.
#[derive(Debug)]
pub struct Error(
    pub(crate) StatusCode,
    pub(crate) &'static str,
    pub(crate) &'static str,
);
impl Error {
    pub fn code(&self) -> &'static str {
        self.1
    }
    pub fn status(&self) -> StatusCode {
        self.0
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.1)
    }
}
impl std::error::Error for Error {}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (
            self.0,
            Json(crate::api::ApiErrorEnvelope::new(self.1, self.2)),
        )
            .into_response()
    }
}
impl From<libsql::Error> for Error {
    fn from(error: libsql::Error) -> Self {
        eprintln!(
            "event=backup_storage_error kind={:?}",
            std::mem::discriminant(&error)
        );
        storage()
    }
}
pub type Result<T> = std::result::Result<T, Error>;
fn storage() -> Error {
    Error(
        StatusCode::SERVICE_UNAVAILABLE,
        "backup_storage_error",
        "Backup storage is unavailable; nothing was changed",
    )
}
fn invalid(message: &'static str) -> Error {
    Error(StatusCode::BAD_REQUEST, "invalid_backup_request", message)
}
fn local_only() -> Error {
    Error(
        StatusCode::SERVICE_UNAVAILABLE,
        "backup_local_only",
        "Backups and restores require a locally owned database",
    )
}
fn not_found() -> Error {
    Error(
        StatusCode::NOT_FOUND,
        "backup_not_found",
        "No backup with this id exists in the backup folder",
    )
}
fn timeout() -> Error {
    Error(
        StatusCode::SERVICE_UNAVAILABLE,
        "backup_timeout",
        "The backup request timed out",
    )
}
fn io_error() -> Error {
    Error(
        StatusCode::SERVICE_UNAVAILABLE,
        "backup_folder_unavailable",
        "The backup folder could not be read or written",
    )
}

/// Stored policy row.
#[derive(Debug, Clone)]
pub(crate) struct Stored {
    pub revision: i64,
    pub folder: Option<String>,
    pub interval_days: i64,
    pub retention_days: i64,
    pub next_backup_at: Option<i64>,
}
pub(crate) async fn read_settings(c: &libsql::Connection) -> Result<Stored> {
    let row = c
        .query(
            "SELECT revision,folder,interval_days,retention_days,next_backup_at FROM backup_settings WHERE id=1",
            (),
        )
        .await?
        .next()
        .await?
        .ok_or(storage())?;
    let stored = Stored {
        revision: row.get(0)?,
        folder: row.get(1)?,
        interval_days: row.get(2)?,
        retention_days: row.get(3)?,
        next_backup_at: row.get(4)?,
    };
    if normalize(stored.folder.as_deref()).ok().flatten() != stored.folder {
        return Err(Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "backup_settings_invalid",
            "Stored backup settings are invalid",
        ));
    }
    Ok(stored)
}

/// Absolute POSIX folder without traversal, controls or backslashes; repeated and trailing
/// separators collapse. Blank means the default folder.
pub(crate) fn normalize(raw: Option<&str>) -> Result<Option<String>> {
    let Some(raw) = raw else { return Ok(None) };
    if raw.trim().is_empty() {
        return Ok(None);
    }
    if raw.len() > MAX_PATH_BYTES
        || !raw.starts_with('/')
        || raw.contains('\\')
        || raw.chars().any(char::is_control)
        || raw.split('/').any(|s| matches!(s, "." | ".."))
    {
        return Err(invalid(
            "Backup folder must be an absolute POSIX folder path without traversal or control characters",
        ));
    }
    let value = format!(
        "/{}",
        raw.split('/')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("/")
    );
    if value == "/" {
        return Err(invalid("The filesystem root cannot be the backup folder"));
    }
    Ok(Some(value))
}

/// The folder backups are written to: the configured absolute folder, or `Backups` beside the
/// database file.
pub(crate) fn resolve_folder(
    db_path: &std::path::Path,
    configured: Option<&str>,
) -> std::io::Result<std::path::PathBuf> {
    match configured {
        Some(folder) => Ok(std::path::PathBuf::from(folder)),
        None => {
            let parent = db_path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(std::path::Path::new("."));
            Ok(std::path::absolute(parent)?.join("Backups"))
        }
    }
}

/// Component-boundary overlap with any library declaration or recycle bin in either domain.
/// Lexical only: bind mounts and symlink aliases are not detected.
pub(crate) async fn overlaps_media(c: &libsql::Connection, folder: &str) -> Result<bool> {
    Ok(c.query(
        "SELECT EXISTS(SELECT 1 FROM (SELECT path p FROM root_folders UNION ALL SELECT path FROM series UNION ALL SELECT path FROM movies UNION ALL SELECT path FROM recycle_bin_settings WHERE path IS NOT NULL) WHERE rtrim(p,'/')=?1 OR substr(rtrim(p,'/'),1,length(?1)+1)=?1||'/' OR substr(?1,1,length(rtrim(p,'/'))+1)=rtrim(p,'/')||'/')",
        [folder],
    )
    .await?
    .next()
    .await?
    .ok_or(storage())?
    .get::<i64>(0)?
        == 1)
}

fn settings_view(db: &Database, stored: Stored) -> BackupSettings {
    let effective_folder = db.local_path().and_then(|path| {
        resolve_folder(path, stored.folder.as_deref())
            .ok()
            .and_then(|p| p.to_str().map(str::to_owned))
    });
    BackupSettings {
        revision: stored.revision,
        folder: stored.folder,
        effective_folder,
        interval_days: stored.interval_days,
        retention_days: stored.retention_days,
        next_backup_at: stored.next_backup_at,
    }
}

/// Limits applied by the HTTP API (archive reads/uploads and restore validation).
#[derive(Clone, Copy, Debug)]
#[doc(hidden)]
pub struct ApiLimits {
    pub max_upload_bytes: u64,
    pub restore_budget: Duration,
}
impl Default for ApiLimits {
    fn default() -> Self {
        Self {
            max_upload_bytes: archive::MAX_ARCHIVE_BYTES,
            restore_budget: Duration::from_secs(300),
        }
    }
}
#[derive(Clone)]
struct ApiState {
    db: Arc<Database>,
    limits: ApiLimits,
}

pub fn router(db: Arc<Database>) -> Router {
    router_with_limits(db, ApiLimits::default())
}

#[doc(hidden)]
pub fn router_with_limits(db: Arc<Database>, limits: ApiLimits) -> Router {
    let state = ApiState { db, limits };
    let small = Router::new()
        .route("/api/v1/system/backups", get(list))
        .route(
            "/api/v1/system/backups/{id}",
            get(detail).delete(delete_one),
        )
        .route("/api/v1/system/backups/{id}/download", get(download))
        .route("/api/v1/system/backups/{id}/restore", post(restore_backup))
        .route("/api/v1/system/restore", get(pending).delete(cancel))
        .route(
            "/api/v1/system/backup-settings",
            get(fetch_settings).put(update_settings),
        )
        // A legal 4096-byte path can expand sixfold when JSON escaped.
        .layer(DefaultBodyLimit::max(32 * 1024))
        .with_state(state.clone());
    // Uploads are streamed to private staging with their own bound, never buffered whole.
    let upload = Router::new()
        .route("/api/v1/system/restore/upload", post(restore_upload))
        .layer(DefaultBodyLimit::disable())
        .with_state(state);
    small.merge(upload)
}

fn db_path(db: &Database) -> Result<std::path::PathBuf> {
    db.local_path()
        .map(|p| p.to_path_buf())
        .ok_or_else(local_only)
}

fn valid_id(id: &str) -> Result<()> {
    if id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        Ok(())
    } else {
        Err(invalid(
            "Backup id must be 32 lowercase hexadecimal characters",
        ))
    }
}

async fn bounded<T>(future: impl std::future::Future<Output = Result<T>>) -> Result<T> {
    tokio::time::timeout(Duration::from_secs(REQUEST_SECONDS), future)
        .await
        .map_err(|_| timeout())?
}

/// The permit lives in the closure, including after its HTTP waiter times out or disconnects.
async fn inventory<T: Send + 'static>(
    work: impl FnOnce(Instant) -> std::io::Result<T> + Send + 'static,
) -> Result<T> {
    let permit = INVENTORY_WORK.acquire().await.map_err(|_| io_error())?;
    let deadline = Instant::now() + Duration::from_secs(REQUEST_SECONDS);
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        // shortcut: a blocked filesystem syscall cannot be cancelled; process isolation is needed to terminate it.
        work(deadline)
    })
    .await
    .map_err(|_| io_error())?
    .map_err(|error| match error.kind() {
        std::io::ErrorKind::TimedOut => timeout(),
        std::io::ErrorKind::FileTooLarge => Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "backup_inventory_too_large",
            "The backup inventory exceeds the listing limit; known backup ids remain addressable",
        ),
        _ => io_error(),
    })
}

/// Resolves the configured folder and lists it (blocking I/O off the executor).
async fn folder(db: &Database) -> Result<std::path::PathBuf> {
    let path = db_path(db)?;
    let stored = read_settings(&db.connect().await?).await?;
    resolve_folder(&path, stored.folder.as_deref()).map_err(|_| io_error())
}

async fn list(
    State(state): State<ApiState>,
    q: std::result::Result<Query<BackupQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<ApiPage<BackupFile>>> {
    let q = q
        .map_err(|_| invalid("Query accepts limit (1-100), offset and backup_type only"))?
        .0;
    if !(1..=100).contains(&q.limit) || q.offset > engine::MAX_LISTED as u32 {
        return Err(invalid(
            "Query accepts limit (1-100), offset and backup_type only",
        ));
    }
    bounded(async {
        let folder = folder(&state.db).await?;
        let filter = q.backup_type;
        let all = inventory(move |deadline| engine::list(&folder, filter, deadline)).await?;
        let total = all.len() as i64;
        let items = all
            .into_iter()
            .skip(q.offset as usize)
            .take(usize::from(q.limit))
            .collect();
        Ok(Json(ApiPage {
            items,
            total,
            limit: q.limit,
            offset: q.offset,
        }))
    })
    .await
}

async fn find(db: &Database, id: String) -> Result<(BackupFile, std::path::PathBuf)> {
    valid_id(&id)?;
    let folder = folder(db).await?;
    inventory(move |deadline| engine::find(&folder, &id, deadline))
        .await?
        .ok_or_else(not_found)
}

async fn detail(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<BackupDetail>> {
    bounded(async {
        let (backup, path) = find(&state.db, id).await?;
        let manifest = tokio::task::spawn_blocking(move || restore::read_manifest(&path))
            .await
            .map_err(|_| io_error())??;
        Ok(Json(BackupDetail { backup, manifest }))
    })
    .await
}

async fn delete_one(State(state): State<ApiState>, Path(id): Path<String>) -> Result<StatusCode> {
    bounded(async {
        let (backup, path) = find(&state.db, id).await?;
        tokio::task::spawn_blocking(move || engine::remove(&path))
            .await
            .map_err(|_| io_error())?
            .map_err(|_| io_error())?;
        println!(
            "event=backup_deleted backup_id={} backup_type={}",
            backup.id,
            backup.backup_type.dir()
        );
        Ok(StatusCode::NO_CONTENT)
    })
    .await
}

async fn download(State(state): State<ApiState>, Path(id): Path<String>) -> Result<Response> {
    let (backup, path) = bounded(find(&state.db, id)).await?;
    let response = tower_http::services::ServeFile::new(&path)
        .try_call(axum::http::Request::new(Body::empty()))
        .await
        .map_err(|_| io_error())?;
    if response.status() != StatusCode::OK {
        return Err(not_found());
    }
    let (mut parts, body) = response.into_parts();
    let headers: &mut HeaderMap = &mut parts.headers;
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/zip"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    // Names are generated ([A-Za-z0-9._-]) and validated when listed, so they need no escaping.
    let disposition = HeaderValue::from_str(&format!("attachment; filename=\"{}\"", backup.name))
        .map_err(|_| io_error())?;
    headers.insert(header::CONTENT_DISPOSITION, disposition);
    Ok(Response::from_parts(parts, Body::new(body)))
}

async fn restore_backup(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<PendingRestore>)> {
    let (backup, path) = bounded(find(&state.db, id)).await?;
    let staged = restore::stage(
        &state.db,
        restore::Source::Backup { backup, path },
        state.limits,
    )
    .await?;
    Ok((StatusCode::ACCEPTED, Json(staged)))
}

async fn restore_upload(
    State(state): State<ApiState>,
    headers: HeaderMap,
    body: Body,
) -> Result<(StatusCode, Json<PendingRestore>)> {
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|n| n > state.limits.max_upload_bytes) {
        return Err(restore::too_large());
    }
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let essence = content_type.split(';').next().unwrap_or("").trim();
    if !matches!(
        essence,
        "application/zip" | "application/x-zip-compressed" | "application/octet-stream"
    ) {
        return Err(Error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "backup_upload_unsupported",
            "Upload the backup archive as the raw request body (application/zip)",
        ));
    }
    let staged = restore::stage(&state.db, restore::Source::Upload { body }, state.limits).await?;
    Ok((StatusCode::ACCEPTED, Json(staged)))
}

async fn pending(State(state): State<ApiState>) -> Result<Json<RestoreStatus>> {
    bounded(async {
        let path = db_path(&state.db)?;
        let pending = tokio::task::spawn_blocking(move || restore::read_pending(&path))
            .await
            .map_err(|_| io_error())??;
        Ok(Json(RestoreStatus { pending }))
    })
    .await
}

async fn cancel(State(state): State<ApiState>) -> Result<StatusCode> {
    bounded(async {
        let path = db_path(&state.db)?;
        let _guard = restore::RESTORE_LOCK.lock().await;
        let removed = tokio::task::spawn_blocking(move || restore::cancel_pending(&path))
            .await
            .map_err(|_| io_error())??;
        if removed {
            println!("event=backup_restore_cancelled");
            Ok(StatusCode::NO_CONTENT)
        } else {
            Err(Error(
                StatusCode::NOT_FOUND,
                "restore_not_pending",
                "No restore is pending",
            ))
        }
    })
    .await
}

async fn fetch_settings(State(state): State<ApiState>) -> Result<Json<BackupSettings>> {
    bounded(async {
        let stored = read_settings(&state.db.connect().await?).await?;
        Ok(Json(settings_view(&state.db, stored)))
    })
    .await
}

pub(crate) fn validate(input: &BackupSettingsUpdate) -> Result<Option<String>> {
    if !(1..=MAX_REVISION).contains(&input.revision) {
        return Err(invalid("Revision must be a positive integer"));
    }
    if !(MIN_INTERVAL_DAYS..=MAX_INTERVAL_DAYS).contains(&input.interval_days) {
        return Err(invalid(
            "Backup interval must be a whole number of days from 1 to 7",
        ));
    }
    if !(MIN_RETENTION_DAYS..=MAX_RETENTION_DAYS).contains(&input.retention_days) {
        return Err(invalid(
            "Backup retention must be a whole number of days from 1 to 90",
        ));
    }
    normalize(input.folder.as_deref())
}

async fn update_settings(
    State(state): State<ApiState>,
    body: std::result::Result<Json<BackupSettingsUpdate>, JsonRejection>,
) -> Result<Json<BackupSettings>> {
    let Json(input) = body.map_err(|_| {
        invalid("Body must contain revision, folder, interval_days and retention_days only")
    })?;
    let folder = validate(&input)?;
    let now = crate::commands::now().map_err(|_| timeout())?;
    bounded(async {
        db_path(&state.db)?;
        let c = state.db.connect().await?;
        if let Some(folder) = &folder {
            if overlaps_media(&c, folder).await? {
                return Err(Error(
                    StatusCode::CONFLICT,
                    "backup_folder_overlaps_library",
                    "The backup folder cannot be, contain, or be inside a root folder, library path or recycle bin",
                ));
            }
            let probe = std::path::PathBuf::from(folder);
            tokio::task::spawn_blocking(move || engine::probe_writable(&probe))
                .await
                .map_err(|_| io_error())?
                .map_err(|_| {
                    Error(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "backup_folder_unwritable",
                        "The backup folder could not be created or written",
                    )
                })?;
        }
        let tx = c
            .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
            .await?;
        let result = async {
            // Recheck under the write lock so a concurrent library change cannot slip in.
            if let Some(folder) = &folder {
                if overlaps_media(&tx, folder).await? {
                    return Err(Error(
                        StatusCode::CONFLICT,
                        "backup_folder_overlaps_library",
                        "The backup folder cannot be, contain, or be inside a root folder, library path or recycle bin",
                    ));
                }
            }
            let changed = tx
                .execute(
                    "UPDATE backup_settings SET folder=?1,interval_days=?2,retention_days=?3,revision=revision+1,next_backup_at=CASE WHEN interval_days=?2 AND next_backup_at IS NOT NULL THEN next_backup_at ELSE ?4 END WHERE id=1 AND revision=?5 AND revision<?6",
                    libsql::params![
                        folder.clone(),
                        input.interval_days,
                        input.retention_days,
                        now + input.interval_days * 86_400,
                        input.revision,
                        MAX_REVISION
                    ],
                )
                .await?;
            if changed != 1 {
                return Err(Error(
                    StatusCode::CONFLICT,
                    "backup_settings_conflict",
                    "Backup settings changed; reload before saving",
                ));
            }
            read_settings(&tx).await
        }
        .await;
        match result {
            Ok(stored) => {
                tx.commit().await?;
                Ok(Json(settings_view(&state.db, stored)))
            }
            Err(error) => {
                tx.rollback().await?;
                Err(error)
            }
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_normalization_rejects_relative_traversal_and_root() {
        assert_eq!(normalize(None).unwrap(), None);
        assert_eq!(normalize(Some("  ")).unwrap(), None);
        assert_eq!(
            normalize(Some("/srv//backups/")).unwrap().as_deref(),
            Some("/srv/backups")
        );
        for bad in [
            "relative/x",
            "/a/../b",
            "/a/./b",
            "/",
            "//",
            "/a\\b",
            "/a\u{7}b",
        ] {
            assert!(normalize(Some(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn ids_are_strict_lowercase_hex() {
        assert!(valid_id(&"a".repeat(32)).is_ok());
        for bad in [
            "",
            "../../etc/passwd",
            &"A".repeat(32),
            &"a".repeat(31),
            &"g".repeat(32),
        ] {
            assert!(valid_id(bad).is_err(), "{bad}");
        }
    }
    #[tokio::test]
    async fn timed_out_inventory_holds_admission_until_blocking_work_finishes() {
        let (entered, started) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let request = tokio::spawn(async move {
            tokio::time::timeout(
                Duration::from_millis(20),
                inventory(move |_| {
                    entered.send(()).unwrap();
                    blocked.recv_timeout(Duration::from_secs(2)).unwrap();
                    Ok(())
                }),
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(1), started)
            .await
            .unwrap()
            .unwrap();
        assert!(request.await.unwrap().is_err(), "the HTTP waiter timed out");
        let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        for _ in 0..3 {
            let called = called.clone();
            assert!(
                tokio::time::timeout(
                    Duration::from_millis(10),
                    inventory(move |_| {
                        called.store(true, std::sync::atomic::Ordering::SeqCst);
                        Ok(())
                    })
                )
                .await
                .is_err(),
                "stalled work still owns the only slot"
            );
        }
        assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), inventory(|_| Ok(())))
            .await
            .unwrap()
            .unwrap();
    }
}
