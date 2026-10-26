//! Explicit, journaled deletion of TV episode files and movie files (Sonarr/Radarr
//! `DELETE /episodefile`, `/moviefile` and their bulk forms).
//!
//! Order per file: preflight (no DB transaction) → admitted journal row (intent + captured source
//! identity) → one filesystem effect → one transaction that removes the file record and every
//! association and records history → cross-device source cleanup. Any interruption is reconciled
//! from the journal and the observed disk state; a file is never removed by name alone.
use super::{
    Error, Result, domain,
    fsops::{self, FsError, Identity, Moved, Source},
};
use crate::{
    api::MediaDomain,
    db::{Database, MediaTarget},
};
use axum::{
    Extension, Json,
    extract::{Path, State, rejection::JsonRejection},
    http::StatusCode,
};
use libsql::{Connection, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

/// Bulk requests are synchronous filesystem work; keep each one short and reviewable.
pub const MAX_DELETE_IDS: usize = 50;
/// Unfinished journal rows reconciled per recovery pass.
const RECOVERY_BATCH: i64 = 64;
/// A shared TV file can serve many episodes; the detach is still one bounded statement.
const MAX_TARGETS: i64 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename = "FileDeletionMode")]
pub enum Mode {
    // Move into the configured recycle bin. Requires a configured bin.
    Recycle,
    // Remove without a recycle bin. Accepted only when no bin is configured.
    Permanent,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "FileDeleteInput")]
pub struct DeleteInput {
    pub mode: Mode,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "FileBulkDeleteInput")]
pub struct BulkDeleteInput {
    pub file_ids: Vec<i64>,
    pub mode: Mode,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename = "FileDeletionEffect")]
pub enum Effect {
    Recycled,
    Deleted,
    // The file was already missing; only the record and associations were removed.
    AlreadyMissing,
}
#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(rename = "FileDeletion")]
pub struct Deletion {
    pub id: Uuid,
    pub media_type: MediaDomain,
    pub file_id: i64,
    pub path: String,
    pub effect: Effect,
    pub recycle_path: Option<String>,
    // Episodes (several for one multi-episode file) or the movie that lost this file.
    pub targets: Vec<MediaTarget>,
}
#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(rename = "FileDeletionBatch")]
pub struct Batch {
    pub batch_id: Uuid,
    pub deletions: Vec<Deletion>,
}

fn err(status: StatusCode, code: &'static str, message: &'static str) -> Error {
    Error(status, code, message)
}
fn bad(message: &'static str) -> Error {
    err(StatusCode::BAD_REQUEST, "invalid_file_deletion", message)
}
fn internal() -> Error {
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "file_delete_internal",
        "File deletion failed; the journal retains recovery state",
    )
}
fn fs_error(error: FsError) -> Error {
    match error {
        FsError::Missing => err(
            StatusCode::CONFLICT,
            "path_missing",
            "A required folder is missing; nothing was deleted",
        ),
        FsError::Denied => err(
            StatusCode::CONFLICT,
            "permission_denied",
            "Permission denied; the file and its record were kept",
        ),
        FsError::NotRegular => err(
            StatusCode::CONFLICT,
            "file_not_regular",
            "The stored path is not a regular file (for example a symlink); nothing was deleted",
        ),
        FsError::Unsafe => err(
            StatusCode::CONFLICT,
            "path_unsafe",
            "The path traverses a symlink or is not a normalized absolute path; nothing was deleted",
        ),
        FsError::Changed => err(
            StatusCode::CONFLICT,
            "file_changed",
            "The file changed during deletion; it and its record were kept",
        ),
        FsError::Exists => err(
            StatusCode::CONFLICT,
            "recycle_destination_exists",
            "The recycle bin destination is occupied; nothing was deleted",
        ),
        FsError::Limit => err(
            StatusCode::CONFLICT,
            "transfer_limit",
            "The file exceeds the transfer size or time limit; it and its record were kept",
        ),
        FsError::Io => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "filesystem_error",
            "A filesystem operation failed; the file and its record were kept",
        ),
    }
}
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> std::result::Result<T, FsError> + Send + 'static,
) -> Result<std::result::Result<T, FsError>> {
    tokio::task::spawn_blocking(f).await.map_err(|_| internal())
}
fn now() -> Result<i64> {
    crate::commands::now().map_err(|_| internal())
}

#[derive(Clone, Copy)]
struct Tables {
    media: MediaDomain,
    table: &'static str,
    owner: &'static str,
    owners: &'static str,
}
fn tables(media: MediaDomain) -> Tables {
    match media {
        MediaDomain::Tv => Tables {
            media,
            table: "episode_files",
            owner: "series_id",
            owners: "series",
        },
        MediaDomain::Movies => Tables {
            media,
            table: "movie_files",
            owner: "movie_id",
            owners: "movies",
        },
    }
}

// ---------- HTTP ----------

fn parse_media(media: &str) -> Result<MediaDomain> {
    MediaDomain::parse(media).map_err(|_| bad("Media domain must be tv or movies"))
}
pub(crate) async fn delete_one(
    State(db): State<Arc<Database>>,
    Extension(media): Extension<String>,
    Path(id): Path<String>,
    body: std::result::Result<Json<DeleteInput>, JsonRejection>,
) -> Result<Json<Deletion>> {
    let media = parse_media(&media)?;
    let id = id
        .parse::<i64>()
        .ok()
        .filter(|v| *v > 0)
        .ok_or_else(|| bad("File id must be positive"))?;
    let Json(input) = body.map_err(|_| {
        bad("An explicit JSON body {\"mode\":\"recycle\"|\"permanent\"} is required")
    })?;
    let mut batch = delete_files(db, media, vec![id], input.mode).await?;
    batch.deletions.pop().map(Json).ok_or_else(internal)
}
pub(crate) async fn delete_bulk(
    State(db): State<Arc<Database>>,
    Extension(media): Extension<String>,
    body: std::result::Result<Json<BulkDeleteInput>, JsonRejection>,
) -> Result<Json<Batch>> {
    let media = parse_media(&media)?;
    let Json(input) =
        body.map_err(|_| bad("Body must contain file_ids and an explicit mode only"))?;
    delete_files(db, media, input.file_ids, input.mode)
        .await
        .map(Json)
}

fn validate_ids(ids: &[i64]) -> Result<()> {
    if ids.is_empty() || ids.len() > MAX_DELETE_IDS {
        return Err(bad("Delete between 1 and 50 files per request"));
    }
    let mut seen = std::collections::BTreeSet::new();
    if ids
        .iter()
        .any(|id| !(1..=9_007_199_254_740_991).contains(id) || !seen.insert(*id))
    {
        return Err(bad("File ids must be distinct positive integers"));
    }
    Ok(())
}

/// Validates the whole request, then deletes files in order under the global file-effect permit.
/// The work runs in an owned task so a dropped HTTP request cannot interrupt a transfer midway.
pub(crate) async fn delete_files(
    db: Arc<Database>,
    media: MediaDomain,
    ids: Vec<i64>,
    mode: Mode,
) -> Result<Batch> {
    if !db.permits_local_imports() {
        return Err(err(
            StatusCode::SERVICE_UNAVAILABLE,
            "file_delete_local_ownership_required",
            "File deletion requires a locally owned database",
        ));
    }
    validate_ids(&ids)?;
    let batch_id = Uuid::new_v4();
    let guard =
        crate::import::acquire_deletion_guard(&db, &batch_id.to_string()).map_err(|_| {
            err(
                StatusCode::CONFLICT,
                "import_busy",
                "Another import or file operation is executing; retry",
            )
        })?;
    tokio::spawn(async move {
        let _guard = guard;
        run_batch(&db, media, &ids, mode, batch_id).await
    })
    .await
    .map_err(|_| internal())?
}

struct Plan {
    file_id: i64,
    owner_id: i64,
    path: String,
    owner_path: String,
    root: String,
    subfolder: Vec<String>,
    leaf: String,
}
pub(crate) fn layout(owner_path: &str, path: &str) -> Result<(Vec<String>, String)> {
    let owner = owner_path.trim_end_matches('/');
    let rest = path
        .strip_prefix(owner)
        .and_then(|rest| rest.strip_prefix('/'))
        .filter(|rest| !rest.is_empty())
        .ok_or(err(
            StatusCode::CONFLICT,
            "file_outside_library",
            "The file is not inside its library folder; nothing was deleted",
        ))?;
    let owner_name = owner
        .rsplit('/')
        .next()
        .filter(|n| !n.is_empty())
        .ok_or_else(|| fs_error(FsError::Unsafe))?;
    let mut parts: Vec<String> = rest
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    if parts
        .iter()
        .any(|p| matches!(p.as_str(), "." | "..") || p.chars().any(char::is_control))
    {
        return Err(fs_error(FsError::Unsafe));
    }
    let leaf = parts.pop().ok_or_else(|| fs_error(FsError::Unsafe))?;
    // Upstream layout: bin/<library folder>/<sub-folders>/<file>.
    let mut subfolder = vec![owner_name.to_owned()];
    subfolder.extend(parts);
    Ok((subfolder, leaf))
}
async fn scalar(c: &Connection, sql: &str, values: impl libsql::params::IntoParams) -> Result<i64> {
    Ok(c.query(sql, values)
        .await?
        .next()
        .await?
        .ok_or_else(internal)?
        .get::<i64>(0)?)
}
/// Files claimed by any import or scan journal are retained: their RESTRICT references are the
/// recovery authority for replacements and cannot be removed by an unrelated deletion.
async fn referenced(c: &Connection, t: Tables, file_id: i64) -> Result<bool> {
    let sql = match t.media {
        MediaDomain::Tv => {
            "SELECT EXISTS(SELECT 1 FROM rss_candidate_imports WHERE old_episode_file_id=?1) OR EXISTS(SELECT 1 FROM scan_import_replacements WHERE old_episode_file_id=?1) OR EXISTS(SELECT 1 FROM downloaded_scan_child_episodes WHERE expected_old_episode_file_id=?1)"
        }
        MediaDomain::Movies => {
            "SELECT EXISTS(SELECT 1 FROM rss_candidate_imports WHERE old_movie_file_id=?1) OR EXISTS(SELECT 1 FROM scan_import_replacements WHERE old_movie_file_id=?1) OR EXISTS(SELECT 1 FROM downloaded_scan_children WHERE expected_old_movie_file_id=?1)"
        }
    };
    Ok(scalar(c, sql, [file_id]).await? == 1)
}
/// In-flight imports, running rescans, reserved scans or download processing for the library item.
async fn owner_busy(c: &Connection, t: Tables, owner: i64) -> Result<bool> {
    let sql = match t.media {
        MediaDomain::Tv => {
            "SELECT EXISTS(SELECT 1 FROM import_journal j JOIN operations o ON o.id=j.operation_id JOIN episodes e ON e.id=o.episode_id WHERE o.media_type='episode' AND e.series_id=?1 AND j.phase NOT IN ('preview','complete')) OR EXISTS(SELECT 1 FROM rescan_commands WHERE media_type='tv' AND series_id=?1 AND status='running') OR EXISTS(SELECT 1 FROM downloaded_scan_reserved_children WHERE series_id=?1) OR EXISTS(SELECT 1 FROM download_processing dp JOIN rss_candidates r ON r.id=dp.candidate_id WHERE dp.status IN ('checking','importing') AND r.media_type='tv' AND r.series_id=?1)"
        }
        MediaDomain::Movies => {
            "SELECT EXISTS(SELECT 1 FROM import_journal j JOIN operations o ON o.id=j.operation_id WHERE o.media_type='movie' AND o.movie_id=?1 AND j.phase NOT IN ('preview','complete')) OR EXISTS(SELECT 1 FROM rescan_commands WHERE media_type='movies' AND movie_id=?1 AND status='running') OR EXISTS(SELECT 1 FROM downloaded_scan_reserved_children WHERE movie_id=?1) OR EXISTS(SELECT 1 FROM download_processing dp JOIN rss_candidates r ON r.id=dp.candidate_id WHERE dp.status IN ('checking','importing') AND r.media_type='movies' AND r.movie_id=?1)"
        }
    };
    Ok(scalar(c, sql, [owner]).await? == 1)
}
async fn guard_file(c: &Connection, t: Tables, file_id: i64, owner: i64) -> Result<()> {
    if referenced(c, t, file_id).await? {
        return Err(err(
            StatusCode::CONFLICT,
            "file_reserved_by_import",
            "An import or scan journal still references this file; it cannot be deleted",
        ));
    }
    if owner_busy(c, t, owner).await? {
        return Err(err(
            StatusCode::CONFLICT,
            "library_busy",
            "An import, rescan or scan is active for this library item; retry when it finishes",
        ));
    }
    if scalar(
        c,
        "SELECT EXISTS(SELECT 1 FROM file_deletions WHERE media_type=? AND file_id=? AND phase IN ('admitted','copied','committed'))",
        params![domain(t.media), file_id],
    )
    .await?
        == 1
    {
        return Err(err(
            StatusCode::CONFLICT,
            "file_deletion_pending",
            "An earlier deletion of this file is awaiting recovery",
        ));
    }
    Ok(())
}
async fn plan(c: &Connection, t: Tables, file_id: i64) -> Result<Plan> {
    let row = c
        .query(
            &format!(
                "SELECT f.{owner},f.path,p.path FROM {table} f JOIN {owners} p ON p.id=f.{owner} WHERE f.id=?",
                owner = t.owner,
                table = t.table,
                owners = t.owners
            ),
            [file_id],
        )
        .await?
        .next()
        .await?
        .ok_or(err(
            StatusCode::NOT_FOUND,
            "file_not_found",
            "One or more files do not exist; nothing was deleted",
        ))?;
    let owner_id: i64 = row.get(0)?;
    let path: String = row.get(1)?;
    let owner_path: String = row.get(2)?;
    drop(row);
    guard_file(c, t, file_id, owner_id).await?;
    let (subfolder, leaf) = layout(&owner_path, &path)?;
    let owner_trimmed = owner_path.trim_end_matches('/');
    let best = c
        .query(
            "SELECT path FROM root_folders WHERE media_type=?1 AND (path=?2 OR substr(?2,1,length(path)+1)=path||'/') ORDER BY length(path) DESC LIMIT 1",
            params![domain(t.media), owner_trimmed],
        )
        .await?
        .next()
        .await?
        .map(|r| r.get::<String>(0))
        .transpose()?;
    let root = match best {
        Some(root) => root,
        None => {
            let parent = std::path::Path::new(owner_trimmed)
                .parent()
                .and_then(|p| p.to_str())
                .ok_or_else(|| fs_error(FsError::Unsafe))?;
            parent.to_owned()
        }
    };
    Ok(Plan {
        file_id,
        owner_id,
        path,
        owner_path,
        root,
        subfolder,
        leaf,
    })
}

async fn run_batch(
    db: &Arc<Database>,
    media: MediaDomain,
    ids: &[i64],
    mode: Mode,
    batch_id: Uuid,
) -> Result<Batch> {
    // The caller holds the permit, so any unfinished row belongs to an interrupted attempt.
    recover_unfinished(db).await?;
    let t = tables(media);
    let c = db.connect().await?;
    let settings = super::read(&c, media).await?;
    match (mode, &settings.path) {
        (Mode::Recycle, None) => {
            return Err(err(
                StatusCode::CONFLICT,
                "recycle_bin_not_configured",
                "No recycle bin is configured; confirm permanent deletion instead",
            ));
        }
        (Mode::Permanent, Some(_)) => {
            return Err(err(
                StatusCode::CONFLICT,
                "recycle_bin_configured",
                "A recycle bin is configured; files are recycled, not permanently deleted",
            ));
        }
        _ => {}
    }
    // Whole-request validation precedes the first filesystem effect.
    let mut plans = Vec::with_capacity(ids.len());
    for id in ids {
        plans.push(plan(&c, t, *id).await?);
    }
    drop(c);
    let mut deletions = Vec::with_capacity(plans.len());
    for (index, plan) in plans.into_iter().enumerate() {
        match delete_file(db, t, plan, settings.path.clone(), batch_id).await {
            Ok(deletion) => deletions.push(deletion),
            Err(error) if index > 0 => {
                eprintln!(
                    "event=file_delete_batch_stopped batch_id={batch_id} completed={index} code={}",
                    error.code()
                );
                return Err(err(
                    error.0,
                    error.1,
                    "File deletion stopped; earlier files in this request were deleted. Reload the file list",
                ));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(Batch {
        batch_id,
        deletions,
    })
}

enum Kind {
    Absent,
    Permanent(fsops::Present),
    Recycle(fsops::Present, fsops::Destination),
}
struct Journal {
    id: Uuid,
    media: MediaDomain,
    file_id: i64,
    owner_id: i64,
    path: String,
    mode: String,
    destination: Option<String>,
    source: Option<Identity>,
    dest: Option<(u64, u64)>,
    phase: String,
}

async fn delete_file(
    db: &Arc<Database>,
    t: Tables,
    plan: Plan,
    bin: Option<String>,
    batch_id: Uuid,
) -> Result<Deletion> {
    let id = Uuid::new_v4();
    let root = plan.root.clone();
    let path = plan.path.clone();
    let subfolder = plan.subfolder.clone();
    let leaf = plan.leaf.clone();
    let bin_configured = bin.is_some();
    let kind = blocking(move || {
        match fsops::root_available(&root) {
            Ok(true) => {}
            Ok(false) => return Ok(Err("library_root_empty")),
            Err(FsError::Missing) => return Ok(Err("library_root_missing")),
            Err(e) => return Err(e),
        }
        Ok(Ok(match fsops::inspect(&path)? {
            Source::Absent => Kind::Absent,
            Source::Present(present) => match bin {
                None => Kind::Permanent(present),
                Some(bin) => match fsops::prepare_destination(&bin, &subfolder, &leaf) {
                    Ok(destination) => Kind::Recycle(present, destination),
                    Err(FsError::Missing) => return Ok(Err("recycle_bin_unavailable")),
                    Err(e) => return Err(e),
                },
            },
        }))
    })
    .await?
    .map_err(fs_error)?
    .map_err(|code| match code {
        "library_root_empty" => err(
            StatusCode::CONFLICT,
            "library_root_empty",
            "The library root folder has no subfolders (possibly unmounted); nothing was deleted",
        ),
        "library_root_missing" => err(
            StatusCode::CONFLICT,
            "library_root_missing",
            "The library root folder is missing; nothing was deleted",
        ),
        _ => err(
            StatusCode::CONFLICT,
            "recycle_bin_unavailable",
            "The configured recycle bin folder does not exist; nothing was deleted",
        ),
    })?;
    let (mode, source, destination) = match &kind {
        Kind::Absent => ("absent", None, None),
        Kind::Permanent(p) => ("permanent", Some(p.identity), None),
        Kind::Recycle(p, d) => ("recycle", Some(p.identity), Some(d.path.clone())),
    };
    // Whether a bin was configured at preflight; admission re-reads and must agree.
    let configured = bin_configured;
    let journal = Journal {
        id,
        media: t.media,
        file_id: plan.file_id,
        owner_id: plan.owner_id,
        path: plan.path.clone(),
        mode: mode.into(),
        destination: destination.clone(),
        source,
        dest: None,
        phase: "admitted".into(),
    };
    admit(db, t, &journal, batch_id, configured, &plan).await?;
    let effect = match kind {
        Kind::Absent => Ok(("none", None)),
        Kind::Permanent(present) => blocking(move || fsops::unlink_source(&present))
            .await?
            .map(|()| ("unlink", None)),
        Kind::Recycle(present, dest) => {
            let deletion = id.to_string();
            blocking(move || fsops::move_into_bin(&present, &dest, &deletion))
                .await?
                .map(|moved| match moved {
                    Moved::Rename => ("rename", None),
                    Moved::Link => ("link", None),
                    Moved::Copy { dev, ino } => ("copy", Some((dev, ino))),
                })
        }
    };
    let (transfer, dest) = match effect {
        Ok(value) => value,
        Err(error) => return settle_failure(db, journal, Some(fs_error(error))).await,
    };
    let mut journal = journal;
    if let Some((dev, ino)) = dest {
        journal.dest = Some((dev, ino));
        if let Err(error) = checkpoint_copied(db, &journal).await {
            eprintln!(
                "event=file_delete_checkpoint_failed deletion_id={id} code={}",
                error.code()
            );
            return settle_failure(db, journal, Some(error)).await;
        }
        journal.phase = "copied".into();
    }
    #[cfg(test)]
    if super::tests::crash_point(&plan.path) {
        // Simulated process death after the filesystem effect (and any copy checkpoint) but
        // before the association commit.
        return Err(internal());
    }
    let final_phase = if transfer == "copy" {
        "committed"
    } else {
        "complete"
    };
    let targets = match commit(db, &journal, transfer, final_phase).await {
        Ok(targets) => targets,
        Err(error) => {
            eprintln!(
                "event=file_delete_commit_failed deletion_id={id} code={}",
                error.code()
            );
            return settle_failure(db, journal, Some(error)).await;
        }
    };
    if transfer == "copy" {
        journal.phase = "committed".into();
        finish_source(db, &journal).await?;
        let phase = read_journal(&db.connect().await?, id).await?.phase;
        if phase != "complete" {
            return Err(err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "recycle_source_cleanup_pending",
                "The file was copied to the recycle bin and detached, but removing the original failed; it will be retried",
            ));
        }
    }
    Ok(Deletion {
        id,
        media_type: t.media,
        file_id: journal.file_id,
        path: journal.path,
        effect: match transfer {
            "none" => Effect::AlreadyMissing,
            "unlink" => Effect::Deleted,
            _ => Effect::Recycled,
        },
        recycle_path: destination,
        targets,
    })
}

/// After a failed effect or commit, reconcile from disk immediately (we still hold the permit).
/// A completed roll-forward is reported as success; otherwise the original error is returned.
async fn settle_failure(
    db: &Arc<Database>,
    journal: Journal,
    error: Option<Error>,
) -> Result<Deletion> {
    let code = error.as_ref().map(|e| e.code()).unwrap_or("interrupted");
    let id = journal.id;
    if let Err(recovery) = reconcile(db, id, Some(code)).await {
        eprintln!(
            "event=file_delete_recovery_failed deletion_id={id} code={}",
            recovery.code()
        );
    }
    let c = db.connect().await?;
    let row = read_journal(&c, id).await?;
    if row.phase == "complete" {
        return completed(&c, row).await;
    }
    Err(match (row.phase.as_str(), error) {
        ("aborted", Some(error)) => error,
        ("aborted", None) => internal(),
        (_, _) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "file_delete_recovery_pending",
            "File deletion was interrupted; recovery will finish or undo it on the next attempt or restart",
        ),
    })
}
async fn completed(c: &Connection, row: Journal) -> Result<Deletion> {
    let mut rows = c
        .query(
            "SELECT media_type,coalesce(episode_id,movie_id) FROM file_deletion_history WHERE deletion_id=? ORDER BY coalesce(episode_id,movie_id)",
            [row.id.to_string()],
        )
        .await?;
    let mut targets = Vec::new();
    while let Some(r) = rows.next().await? {
        let id: i64 = r.get(1)?;
        targets.push(if r.get::<String>(0)? == "episode" {
            MediaTarget::Episode(id)
        } else {
            MediaTarget::Movie(id)
        });
    }
    let transfer = scalar_text(
        c,
        "SELECT transfer FROM file_deletions WHERE id=?",
        row.id.to_string(),
    )
    .await?;
    Ok(Deletion {
        id: row.id,
        media_type: row.media,
        file_id: row.file_id,
        path: row.path,
        effect: match transfer.as_deref() {
            Some("none") => Effect::AlreadyMissing,
            Some("unlink") => Effect::Deleted,
            _ => Effect::Recycled,
        },
        recycle_path: row.destination,
        targets,
    })
}
async fn scalar_text(c: &Connection, sql: &str, value: String) -> Result<Option<String>> {
    Ok(c.query(sql, [value])
        .await?
        .next()
        .await?
        .ok_or_else(internal)?
        .get::<Option<String>>(0)?)
}

async fn admit(
    db: &Database,
    t: Tables,
    j: &Journal,
    batch_id: Uuid,
    recycle: bool,
    plan: &Plan,
) -> Result<()> {
    let c = db.connect().await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let result = async {
        // Re-verify under the write lock: the record, its owner and stored path are unchanged.
        let unchanged = scalar(
            &tx,
            &format!(
                "SELECT EXISTS(SELECT 1 FROM {table} f JOIN {owners} p ON p.id=f.{owner} WHERE f.id=?1 AND f.{owner}=?2 AND f.path=?3 AND p.path=?4)",
                table = t.table,
                owners = t.owners,
                owner = t.owner
            ),
            params![j.file_id, j.owner_id, j.path.clone(), plan.owner_path.clone()],
        )
        .await?;
        if unchanged != 1 {
            return Err(err(
                StatusCode::CONFLICT,
                "file_changed",
                "The file record changed during deletion; nothing was deleted",
            ));
        }
        guard_file(&tx, t, j.file_id, j.owner_id).await?;
        let settings = super::read(&tx, t.media).await?;
        if settings.path.is_some() != recycle
            || j.destination.as_ref().is_some_and(|d| {
                !settings
                    .path
                    .as_ref()
                    .is_some_and(|b| d.starts_with(&format!("{b}/")))
            })
        {
            return Err(err(
                StatusCode::CONFLICT,
                "recycle_bin_changed",
                "Recycle bin settings changed during deletion; nothing was deleted",
            ));
        }
        let timestamp = now()?;
        tx.execute(
            "INSERT INTO file_deletions(id,batch_id,media_type,file_id,owner_id,path,mode,bin_path,destination,source_dev,source_ino,source_size,source_mtime,source_mtime_ns,phase,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,'admitted',?,?)",
            params![
                j.id.to_string(),
                batch_id.to_string(),
                domain(t.media),
                j.file_id,
                j.owner_id,
                j.path.clone(),
                j.mode.clone(),
                if j.destination.is_some() { settings.path.clone() } else { None },
                j.destination.clone(),
                j.source.map(|s| s.dev.to_string()),
                j.source.map(|s| s.ino.to_string()),
                j.source.map(|s| s.size as i64),
                j.source.map(|s| s.mtime),
                j.source.map(|s| s.mtime_ns),
                timestamp,
                timestamp
            ],
        )
        .await?;
        Ok(())
    }
    .await;
    match result {
        Ok(()) => {
            tx.commit().await?;
            Ok(())
        }
        Err(error) => {
            tx.rollback().await?;
            Err(error)
        }
    }
}
async fn checkpoint_copied(db: &Database, j: &Journal) -> Result<()> {
    let (dev, ino) = j.dest.ok_or_else(internal)?;
    let changed = db.connect().await?.execute(
        "UPDATE file_deletions SET phase='copied',transfer='copy',dest_dev=?,dest_ino=?,updated_at=? WHERE id=? AND phase='admitted'",
        params![dev.to_string(), ino.to_string(), now()?, j.id.to_string()],
    )
    .await?;
    if changed != 1 {
        return Err(internal());
    }
    Ok(())
}

/// Removes the record and every association and records history in one transaction.
async fn commit(
    db: &Database,
    j: &Journal,
    transfer: &str,
    final_phase: &str,
) -> Result<Vec<MediaTarget>> {
    let t = tables(j.media);
    let c = db.connect().await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let result = async {
        let current = read_journal(&tx, j.id).await?;
        if !matches!(current.phase.as_str(), "admitted" | "copied") {
            return Err(internal());
        }
        let present = scalar(
            &tx,
            &format!(
                "SELECT EXISTS(SELECT 1 FROM {table} WHERE id=?1 AND {owner}=?2 AND path=?3)",
                table = t.table,
                owner = t.owner
            ),
            params![j.file_id, j.owner_id, j.path.clone()],
        )
        .await?
            == 1;
        let mut targets = Vec::new();
        let mut note: Option<&str> = None;
        if present {
            match j.media {
                MediaDomain::Tv => {
                    let count = scalar(
                        &tx,
                        "SELECT count(*) FROM episodes WHERE episode_file_id=?",
                        [j.file_id],
                    )
                    .await?;
                    if count > MAX_TARGETS {
                        return Err(internal());
                    }
                    let mut rows = tx
                        .query(
                            "SELECT id FROM episodes WHERE episode_file_id=? ORDER BY id",
                            [j.file_id],
                        )
                        .await?;
                    while let Some(r) = rows.next().await? {
                        targets.push(MediaTarget::Episode(r.get(0)?));
                    }
                    drop(rows);
                    tx.execute(
                        "UPDATE episodes SET episode_file_id=NULL WHERE episode_file_id=?",
                        [j.file_id],
                    )
                    .await?;
                    tx.execute(
                        "DELETE FROM file_metadata WHERE episode_file_id=?",
                        [j.file_id],
                    )
                    .await?;
                    tx.execute("DELETE FROM episode_files WHERE id=?", [j.file_id])
                        .await?;
                }
                MediaDomain::Movies => {
                    targets.push(MediaTarget::Movie(j.owner_id));
                    tx.execute(
                        "DELETE FROM file_metadata WHERE movie_file_id=?",
                        [j.file_id],
                    )
                    .await?;
                    tx.execute("DELETE FROM movie_files WHERE id=?", [j.file_id])
                        .await?;
                }
            }
            for target in &targets {
                let (kind, episode, movie) = match target {
                    MediaTarget::Episode(id) => ("episode", Some(*id), None),
                    MediaTarget::Movie(id) => ("movie", None, Some(*id)),
                };
                tx.execute(
                    "INSERT INTO file_deletion_history(id,deletion_id,media_type,episode_id,movie_id,file_id,path,recycle_path,reason) VALUES(?,?,?,?,?,?,?,?,'manual')",
                    params![
                        Uuid::new_v4().to_string(),
                        j.id.to_string(),
                        kind,
                        episode,
                        movie,
                        j.file_id,
                        j.path.clone(),
                        j.destination.clone()
                    ],
                )
                .await?;
            }
        } else {
            // Another writer (for example a rescan of a missing file) already removed it.
            note = Some("file_record_changed");
        }
        let updated = tx
            .execute(
                "UPDATE file_deletions SET phase=?,transfer=coalesce(transfer,?),error_code=?,updated_at=? WHERE id=?",
                params![final_phase, transfer, note, now()?, j.id.to_string()],
            )
            .await?;
        if updated != 1 {
            return Err(internal());
        }
        Ok(targets)
    }
    .await;
    match result {
        Ok(targets) => {
            tx.commit().await?;
            eprintln!(
                "event=file_deleted deletion_id={} media_type={} file_id={} transfer={transfer} targets={}",
                j.id,
                domain(j.media),
                j.file_id,
                targets.len()
            );
            Ok(targets)
        }
        Err(error) => {
            tx.rollback().await?;
            Err(error)
        }
    }
}

/// Cross-device: the original is removed only after the commit, and only if unchanged.
async fn finish_source(db: &Database, j: &Journal) -> Result<()> {
    let identity = j.source.ok_or_else(internal)?;
    let path = j.path.clone();
    let outcome = blocking(move || fsops::remove_original(&path, &identity)).await?;
    let (phase, code) = match outcome {
        Ok(_) => ("complete", None),
        // Never remove a file that is no longer the one recorded; it is kept for inspection.
        Err(FsError::Changed | FsError::NotRegular) => {
            ("complete", Some("source_changed_retained"))
        }
        Err(error) => ("committed", Some(error.code())),
    };
    db.connect()
        .await?
        .execute(
            "UPDATE file_deletions SET phase=?,error_code=?,updated_at=? WHERE id=? AND phase='committed'",
            params![phase, code, now()?, j.id.to_string()],
        )
        .await?;
    Ok(())
}

const JOURNAL_COLUMNS: &str = "id,media_type,file_id,owner_id,path,mode,destination,source_dev,source_ino,source_size,source_mtime,source_mtime_ns,dest_dev,dest_ino,phase";
fn journal_row(r: &libsql::Row) -> Result<Journal> {
    let parse = |v: Option<String>| -> Result<Option<u64>> {
        v.map(|s| s.parse::<u64>().map_err(|_| internal()))
            .transpose()
    };
    let dev = parse(r.get(7)?)?;
    let ino = parse(r.get(8)?)?;
    let size: Option<i64> = r.get(9)?;
    let mtime: Option<i64> = r.get(10)?;
    let mtime_ns: Option<i64> = r.get(11)?;
    let source = match (dev, ino, size, mtime, mtime_ns) {
        (Some(dev), Some(ino), Some(size), Some(mtime), Some(mtime_ns)) => Some(Identity {
            dev,
            ino,
            size: u64::try_from(size).map_err(|_| internal())?,
            mtime,
            mtime_ns,
        }),
        (None, None, None, None, None) => None,
        _ => return Err(internal()),
    };
    let dest = match (parse(r.get(12)?)?, parse(r.get(13)?)?) {
        (Some(d), Some(i)) => Some((d, i)),
        (None, None) => None,
        _ => return Err(internal()),
    };
    Ok(Journal {
        id: Uuid::parse_str(&r.get::<String>(0)?).map_err(|_| internal())?,
        media: MediaDomain::parse(&r.get::<String>(1)?).map_err(|_| internal())?,
        file_id: r.get(2)?,
        owner_id: r.get(3)?,
        path: r.get(4)?,
        mode: r.get(5)?,
        destination: r.get(6)?,
        source,
        dest,
        phase: r.get(14)?,
    })
}
async fn read_journal(c: &Connection, id: Uuid) -> Result<Journal> {
    let row = c
        .query(
            &format!("SELECT {JOURNAL_COLUMNS} FROM file_deletions WHERE id=?"),
            [id.to_string()],
        )
        .await?
        .next()
        .await?
        .ok_or_else(internal)?;
    journal_row(&row)
}
async fn abort(db: &Database, id: Uuid, code: &str) -> Result<()> {
    db.connect()
        .await?
        .execute(
            "UPDATE file_deletions SET phase='aborted',error_code=?,updated_at=? WHERE id=? AND phase IN ('admitted','copied')",
            params![code, now()?, id.to_string()],
        )
        .await?;
    eprintln!("event=file_delete_aborted deletion_id={id} code={code}");
    Ok(())
}
async fn note(db: &Database, id: Uuid, code: &str) -> Result<()> {
    db.connect()
        .await?
        .execute(
            "UPDATE file_deletions SET error_code=?,updated_at=? WHERE id=? AND phase IN ('admitted','copied','committed')",
            params![code, now()?, id.to_string()],
        )
        .await?;
    Ok(())
}

/// Reconciles every unfinished deletion. The caller must hold the deletion guard (or be the
/// single worker at startup), so no unfinished row has a live owner. Individual rows that cannot
/// be resolved keep their phase and an error code; only storage failures propagate.
pub(crate) async fn recover_unfinished(db: &Database) -> Result<()> {
    let c = db.connect().await?;
    let mut rows = c
        .query(
            "SELECT id FROM file_deletions WHERE phase IN ('admitted','copied','committed') ORDER BY created_at,id LIMIT ?",
            [RECOVERY_BATCH],
        )
        .await?;
    let mut ids = Vec::new();
    while let Some(r) = rows.next().await? {
        ids.push(Uuid::parse_str(&r.get::<String>(0)?).map_err(|_| internal())?);
    }
    drop(rows);
    drop(c);
    for id in ids {
        if let Err(error) = reconcile(db, id, None).await {
            eprintln!(
                "event=file_delete_recovery_failed deletion_id={id} code={}",
                error.code()
            );
            if error.code() == "recycle_bin_storage_error" {
                return Err(error);
            }
        }
    }
    Ok(())
}
/// Startup entry point for the command worker: acquires the guard (free at startup).
/// Waits briefly for the permit (nothing else should hold it before the server accepts
/// requests); if it stays busy, the next deletion request performs the same recovery.
pub(crate) async fn startup(db: &Arc<Database>) -> Result<()> {
    const STARTUP_WAIT: std::time::Duration = std::time::Duration::from_secs(10);
    let deadline = tokio::time::Instant::now() + STARTUP_WAIT;
    loop {
        if let Ok(guard) = crate::import::acquire_deletion_guard(db, "startup-recovery") {
            // A stalled filesystem observation must not hold worker startup: the journal rows stay
            // unfinished and the next startup reconciles them.
            match tokio::time::timeout(STARTUP_WAIT, recover_unfinished(db)).await {
                Ok(result) => result?,
                Err(_) => {
                    eprintln!("event=file_delete_recovery_deferred code=timeout");
                    return Ok(());
                }
            }
            // Upgrade recycles share the permit; a failure here is retried by the import's own
            // resume, so it is reported but never blocks worker startup.
            if let Err(error) = crate::import::recover_upgrade_recycles(db, &guard).await {
                eprintln!(
                    "event=upgrade_recycle_recovery_failed code={}",
                    error.code()
                );
            }
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            eprintln!("event=file_delete_recovery_deferred code=import_busy");
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// Decides one unfinished row from the journal and the observed disk state:
/// roll forward when the recorded file is provably where the effect put it, abort when the source
/// is still the recorded file, and otherwise leave it for inspection without touching the DB.
async fn reconcile(db: &Database, id: Uuid, failure: Option<&str>) -> Result<()> {
    let j = read_journal(&db.connect().await?, id).await?;
    let interrupted = failure.unwrap_or("interrupted");
    match j.phase.as_str() {
        "committed" => return finish_source(db, &j).await,
        "copied" => {
            let destination = j.destination.clone().ok_or_else(internal)?;
            let (dev, ino) = j.dest.ok_or_else(internal)?;
            let observed = blocking(move || fsops::observe(&destination)).await?;
            return match observed {
                Ok(Some(found)) if found.same_inode(dev, ino) => {
                    commit(db, &j, "copy", "committed").await?;
                    let mut j = j;
                    j.phase = "committed".into();
                    finish_source(db, &j).await
                }
                Ok(_) => abort(db, id, "recycled_copy_missing").await,
                Err(error) => note(db, id, error.code()).await,
            };
        }
        "admitted" => {}
        _ => return Ok(()),
    }
    let path = j.path.clone();
    let source_now = blocking(move || fsops::observe(&path)).await?;
    let source_now = match source_now {
        Ok(v) => v,
        Err(error) => return note(db, id, error.code()).await,
    };
    match j.mode.as_str() {
        "absent" => match source_now {
            None => commit(db, &j, "none", "complete").await.map(drop),
            Some(_) => abort(db, id, "source_reappeared").await,
        },
        "permanent" => {
            let recorded = j.source.ok_or_else(internal)?;
            match source_now {
                None => commit(db, &j, "unlink", "complete").await.map(drop),
                Some(found) if found.same_inode(recorded.dev, recorded.ino) => {
                    abort(db, id, interrupted).await
                }
                Some(_) => abort(db, id, "source_changed").await,
            }
        }
        "recycle" => {
            let recorded = j.source.ok_or_else(internal)?;
            let destination = j.destination.clone().ok_or_else(internal)?;
            let probe = destination.clone();
            let dest_now = match blocking(move || fsops::observe(&probe)).await? {
                Ok(v) => v,
                Err(error) => return note(db, id, error.code()).await,
            };
            let ours = dest_now.is_some_and(|d| d.same_inode(recorded.dev, recorded.ino));
            match (source_now, ours) {
                (None, true) => commit(db, &j, "rename", "complete").await.map(drop),
                (Some(found), true) if found.same_inode(recorded.dev, recorded.ino) => {
                    // Interrupted no-replace link fallback: both names are the same inode.
                    let path = j.path.clone();
                    match blocking(move || {
                        fsops::remove_linked_original(&path, recorded.dev, recorded.ino)
                    })
                    .await?
                    {
                        Ok(_) => commit(db, &j, "link", "complete").await.map(drop),
                        Err(error) => note(db, id, error.code()).await,
                    }
                }
                (Some(found), false) if found.same_inode(recorded.dev, recorded.ino) => {
                    // Nothing was published as ours; remove only this deletion's partial file.
                    let id_text = id.to_string();
                    let partial = destination.clone();
                    if let Err(error) =
                        blocking(move || fsops::remove_partial(&partial, &id_text)).await?
                    {
                        return note(db, id, error.code()).await;
                    }
                    // An unrecorded published copy cannot be proven ours; it stays in the bin.
                    let code = if dest_now.is_some() && failure.is_none() {
                        "recycle_copy_retained"
                    } else {
                        interrupted
                    };
                    abort(db, id, code).await
                }
                (Some(_), _) => abort(db, id, "source_changed").await,
                (None, false) => {
                    // Neither name holds the recorded file: an external change. Keep the record
                    // for rescan/inspection instead of inventing a deletion outcome.
                    abort(db, id, "source_missing_unverified").await
                }
            }
        }
        _ => Err(internal()),
    }
}
