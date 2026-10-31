//! Restore: validate an archive completely, stage its database beside the live database and
//! leave a pending marker. Live state changes only at the next startup, before the database is
//! opened, and the replaced database (with its WAL companions) is retained as the original.
//!
//! On-disk protocol next to `<dir>/<db>` (all private, same filesystem):
//! - `.<db>.restore-staging-<uuid>/` work directory; removed on every failure.
//! - `.<db>.restore-pending/{database.db,restore.json}` published by one directory rename.
//! - `.<db>.restore-lock` is a persistent interprocess lock, never renamed or removed.
//! - `.<db>.restore-pending/original` names the retention directory once moving has started,
//!   so an interrupted apply resumes instead of creating a second original.
//! - `.<db>.restore-original-<stamp>-<rand>/` the replaced database plus `restore-applied.json`.
//! - `.<db>.restore-rejected-<stamp>-<rand>/` a pending restore that failed re-verification.
use super::{
    BackupFile, BackupManifest, DATABASE_NAME, ENTRY_NAMES, Error, MANIFEST_FORMAT, MANIFEST_NAME,
    PendingRestore, RestoreSource, archive, engine::hex,
};
use crate::db::{self, Database, SnapshotError, SnapshotOpen};
use axum::{body::Body, body::HttpBody, http::StatusCode};
use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub(crate) static RESTORE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
const MAX_MANIFEST_BYTES: u64 = 256 * 1024;
const MAX_MARKER_BYTES: u64 = 64 * 1024;
const CHUNK: usize = 64 * 1024;

pub(crate) enum Source {
    Backup { backup: BackupFile, path: PathBuf },
    Upload { body: Body },
}

fn err(status: StatusCode, code: &'static str, message: &'static str) -> Error {
    Error(status, code, message)
}
fn invalid_archive(message: &'static str) -> Error {
    err(
        StatusCode::UNPROCESSABLE_ENTITY,
        "backup_archive_invalid",
        message,
    )
}
fn checksum() -> Error {
    err(
        StatusCode::UNPROCESSABLE_ENTITY,
        "backup_checksum_mismatch",
        "The backup database does not match its manifest checksum",
    )
}
fn newer() -> Error {
    err(
        StatusCode::UNPROCESSABLE_ENTITY,
        "backup_schema_unsupported",
        "The backup was created by a newer schema than this version supports",
    )
}
fn history() -> Error {
    err(
        StatusCode::UNPROCESSABLE_ENTITY,
        "backup_history_mismatch",
        "The backup migration history does not match this version",
    )
}
fn integrity() -> Error {
    err(
        StatusCode::UNPROCESSABLE_ENTITY,
        "backup_integrity_failed",
        "The backup database failed its integrity check",
    )
}
pub(crate) fn too_large() -> Error {
    err(
        StatusCode::PAYLOAD_TOO_LARGE,
        "backup_too_large",
        "The backup archive exceeds the supported size",
    )
}
fn io_failure() -> Error {
    err(
        StatusCode::SERVICE_UNAVAILABLE,
        "restore_io_error",
        "The restore could not be staged; live state is unchanged",
    )
}
fn budget() -> Error {
    err(
        StatusCode::SERVICE_UNAVAILABLE,
        "backup_timeout",
        "Restore validation exceeded its time budget; live state is unchanged",
    )
}
fn already_pending() -> Error {
    err(
        StatusCode::CONFLICT,
        "restore_already_pending",
        "A restore is already pending; cancel it before staging another",
    )
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn open_untrusted(path: &Path) -> io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    }
    options.open(path)
}

fn create_private(path: &Path) -> io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn private_dir(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

fn sync_dir(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

/// Manifest checks that need no database: format, identity, files and migration prefix.
fn check_manifest(manifest: &BackupManifest) -> Result<(), Error> {
    if manifest.format != MANIFEST_FORMAT
        || manifest.application != super::APPLICATION
        || manifest.backend != super::BACKEND
        || manifest.files.len() != 1
        || manifest.files[0].name != DATABASE_NAME
        || manifest.files[0].sha256.len() != 64
        || !manifest.files[0]
            .sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || manifest.secrets.key_included
    {
        return Err(invalid_archive(
            "The backup manifest is not a supported hrrdarr backup manifest",
        ));
    }
    let version = manifest.schema_version as usize;
    if version > db::max_schema_version() {
        return Err(newer());
    }
    let known = db::known_migration_histories();
    if version == 0
        || manifest.migrations.len() != version
        || !known.iter().any(|history| {
            history.len() >= version
                && manifest
                    .migrations
                    .iter()
                    .zip(history.iter())
                    .all(|(m, (v, name, sum))| {
                        m.version as usize == *v && m.name == *name && m.checksum == *sum
                    })
        })
    {
        return Err(history());
    }
    Ok(())
}

fn snapshot_matches_manifest(facts: &db::SnapshotFacts, manifest: &BackupManifest) -> bool {
    facts.schema_version == manifest.schema_version as usize
        && facts.migrations.len() == manifest.migrations.len()
        && facts
            .migrations
            .iter()
            .zip(&manifest.migrations)
            .all(|((v, name, sum), m)| {
                *v == m.version as usize && *name == m.name && *sum == m.checksum
            })
}

fn map_archive(error: archive::ArchiveError) -> Error {
    match error {
        archive::ArchiveError::Invalid(reason) => {
            eprintln!("event=backup_archive_rejected reason=\"{reason}\"");
            invalid_archive("The file is not a valid hrrdarr backup archive")
        }
        archive::ArchiveError::Io(e) => {
            eprintln!("event=backup_archive_io_error kind={:?}", e.kind());
            io_failure()
        }
    }
}

struct Opened {
    file: File,
    entries: Vec<archive::Entry>,
    manifest: BackupManifest,
}

fn open_archive(path: &Path, max_bytes: u64) -> Result<Opened, Error> {
    let mut file = open_untrusted(path).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            super::not_found()
        } else {
            invalid_archive("The backup archive could not be opened as a regular file")
        }
    })?;
    let meta = file.metadata().map_err(|_| io_failure())?;
    if !meta.is_file() {
        return Err(invalid_archive("The backup archive is not a regular file"));
    }
    if meta.len() > max_bytes.min(archive::MAX_ARCHIVE_BYTES) {
        return Err(too_large());
    }
    let entries = archive::entries(&mut file, meta.len(), &ENTRY_NAMES).map_err(map_archive)?;
    let manifest_entry = entries
        .iter()
        .find(|e| e.name == MANIFEST_NAME)
        .ok_or_else(|| invalid_archive("The backup manifest is missing"))?;
    if manifest_entry.size > MAX_MANIFEST_BYTES {
        return Err(invalid_archive("The backup manifest is too large"));
    }
    let mut bytes = Vec::with_capacity(manifest_entry.size as usize);
    archive::read_entry(&mut file, manifest_entry, &mut |c| {
        bytes.extend_from_slice(c);
        Ok(())
    })
    .map_err(map_archive)?;
    let manifest: BackupManifest = serde_json::from_slice(&bytes)
        .map_err(|_| invalid_archive("The backup manifest is not valid"))?;
    check_manifest(&manifest)?;
    let database = entries
        .iter()
        .find(|e| e.name == DATABASE_NAME)
        .ok_or_else(|| invalid_archive("The backup database is missing"))?;
    if database.size != manifest.files[0].size {
        return Err(checksum());
    }
    Ok(Opened {
        file,
        entries,
        manifest,
    })
}

/// Reads and checks only the manifest (used for listing details).
pub(crate) fn read_manifest(path: &Path) -> Result<BackupManifest, Error> {
    open_archive(path, archive::MAX_ARCHIVE_BYTES).map(|o| o.manifest)
}

/// Streams the database entry, verifying CRC, size and manifest SHA-256; `sink` receives bytes.
fn stream_database(
    opened: &mut Opened,
    deadline: Instant,
    sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
) -> Result<(), Error> {
    let entry = opened
        .entries
        .iter()
        .find(|e| e.name == DATABASE_NAME)
        .cloned()
        .ok_or_else(|| invalid_archive("The backup database is missing"))?;
    let mut sha = ring::digest::Context::new(&ring::digest::SHA256);
    let mut timed_out = false;
    let result = archive::read_entry(&mut opened.file, &entry, &mut |chunk| {
        if Instant::now() > deadline {
            timed_out = true;
            return Err(io::Error::new(io::ErrorKind::TimedOut, "budget"));
        }
        sha.update(chunk);
        sink(chunk)
    });
    if timed_out {
        return Err(budget());
    }
    result.map_err(map_archive)?;
    if hex(sha.finish().as_ref()) != opened.manifest.files[0].sha256 {
        return Err(checksum());
    }
    Ok(())
}

/// Full archive verification without extraction (used before a backup is published).
pub(crate) fn verify_archive(
    path: &Path,
    max_bytes: u64,
    deadline: Instant,
) -> Result<BackupManifest, Error> {
    let mut opened = open_archive(path, max_bytes)?;
    stream_database(&mut opened, deadline, &mut |_| Ok(()))?;
    Ok(opened.manifest)
}

/// Verifies the archive and extracts its database into `staging/database.db`.
fn extract(
    archive_path: &Path,
    staging: &Path,
    max_bytes: u64,
    deadline: Instant,
) -> Result<BackupManifest, Error> {
    let mut opened = open_archive(archive_path, max_bytes)?;
    let target = staging.join(DATABASE_NAME);
    let mut out =
        io::BufWriter::with_capacity(CHUNK, create_private(&target).map_err(|_| io_failure())?);
    stream_database(&mut opened, deadline, &mut |chunk| out.write_all(chunk))?;
    let file = out.into_inner().map_err(|_| io_failure())?;
    file.sync_all().map_err(|_| io_failure())?;
    Ok(opened.manifest)
}

struct Paths {
    dir: PathBuf,
    base: String,
}
impl Paths {
    fn new(db_path: &Path) -> io::Result<Self> {
        let absolute = std::path::absolute(db_path)?;
        let dir = absolute
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "database path"))?
            .to_path_buf();
        let base = absolute
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "database name"))?
            .to_owned();
        Ok(Self { dir, base })
    }
    fn pending(&self) -> PathBuf {
        self.dir.join(format!(".{}.restore-pending", self.base))
    }
    fn lock(&self) -> io::Result<File> {
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
        }
        let file = options.open(self.dir.join(format!(".{}.restore-lock", self.base)))?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::other("restore lock is not a regular file"));
        }
        file.try_lock().map_err(io::Error::other)?;
        // Keep this inode permanently: unlinking a released lock would let concurrent callers
        // lock different inodes at the same path. The live database inode changes during apply.
        Ok(file)
    }
    fn unique(&self, kind: &str) -> PathBuf {
        let stamp = chrono::DateTime::from_timestamp(now(), 0)
            .map(|t| t.format("%Y%m%dT%H%M%SZ").to_string())
            .unwrap_or_default();
        let random = uuid::Uuid::new_v4().simple().to_string();
        self.dir.join(format!(
            ".{}.restore-{kind}-{stamp}-{}",
            self.base,
            &random[..8]
        ))
    }
    fn live(&self, suffix: &str) -> PathBuf {
        self.dir.join(format!("{}{suffix}", self.base))
    }
}

/// Removes a staging directory on every failure path.
struct Staging(Option<PathBuf>);
impl Drop for Staging {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            if let Err(e) = fs::remove_dir_all(&path) {
                if e.kind() != io::ErrorKind::NotFound {
                    eprintln!("event=restore_staging_cleanup_failed kind={:?}", e.kind());
                }
            }
        }
    }
}

async fn receive(
    mut body: Body,
    target: PathBuf,
    limit: u64,
    deadline: Instant,
) -> Result<(), Error> {
    let file = tokio::task::spawn_blocking(move || create_private(&target))
        .await
        .map_err(|_| io_failure())?
        .map_err(|_| io_failure())?;
    let mut file = tokio::fs::File::from_std(file);
    let mut received = 0u64;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let frame = tokio::time::timeout(
            remaining,
            std::future::poll_fn(|cx| std::pin::Pin::new(&mut body).poll_frame(cx)),
        )
        .await
        .map_err(|_| budget())?;
        let Some(frame) = frame else { break };
        let frame = frame.map_err(|_| {
            err(
                StatusCode::BAD_REQUEST,
                "backup_upload_incomplete",
                "The upload was interrupted; live state is unchanged",
            )
        })?;
        let Ok(data) = frame.into_data() else {
            continue;
        };
        received += data.len() as u64;
        if received > limit {
            return Err(too_large());
        }
        tokio::io::AsyncWriteExt::write_all(&mut file, &data)
            .await
            .map_err(|_| io_failure())?;
    }
    if received == 0 {
        return Err(invalid_archive("The upload is empty"));
    }
    tokio::io::AsyncWriteExt::flush(&mut file)
        .await
        .map_err(|_| io_failure())?;
    file.sync_all().await.map_err(|_| io_failure())?;
    Ok(())
}

/// Validates a candidate fully and publishes it as the pending restore. Live state is unchanged.
pub(crate) async fn stage(
    db: &Database,
    source: Source,
    limits: super::ApiLimits,
) -> Result<PendingRestore, Error> {
    let db_path = db.local_path().ok_or_else(super::local_only)?.to_path_buf();
    let paths = Paths::new(&db_path).map_err(|_| io_failure())?;
    let deadline = Instant::now() + limits.restore_budget;
    let _guard = RESTORE_LOCK.lock().await;
    let _process_lock = paths.lock().map_err(|_| io_failure())?;
    let pending = paths.pending();
    if fs::symlink_metadata(&pending).is_ok() {
        return Err(already_pending());
    }
    let staging_path = paths.dir.join(format!(
        ".{}.restore-staging-{}",
        paths.base,
        uuid::Uuid::new_v4()
    ));
    let create = staging_path.clone();
    tokio::task::spawn_blocking(move || private_dir(&create))
        .await
        .map_err(|_| io_failure())?
        .map_err(|_| io_failure())?;
    let staging = Staging(Some(staging_path.clone()));
    let (archive_path, source_kind, backup) = match source {
        Source::Backup { backup, path } => (path, RestoreSource::Backup, Some(backup)),
        Source::Upload { body } => {
            let upload = staging_path.join("upload.zip");
            receive(body, upload.clone(), limits.max_upload_bytes, deadline).await?;
            (upload, RestoreSource::Upload, None)
        }
    };
    let work = staging_path.clone();
    let max = limits.max_upload_bytes;
    let manifest = tokio::task::spawn_blocking(move || {
        let manifest = extract(&archive_path, &work, max, deadline)?;
        if archive_path.starts_with(&work) {
            let _ = fs::remove_file(&archive_path);
        }
        Ok::<_, Error>(manifest)
    })
    .await
    .map_err(|_| io_failure())??;
    // Read-only: the verified candidate bytes are never modified before the restart applies them.
    let candidate = staging_path.join(DATABASE_NAME);
    let handle = tokio::runtime::Handle::current();
    let inspect = candidate.clone();
    let facts = tokio::task::spawn_blocking(move || {
        handle.block_on(db::inspect_snapshot(&inspect, SnapshotOpen::ReadOnly))
    })
    .await
    .map_err(|_| io_failure())?
    .map_err(|e| match e {
        SnapshotError::Newer(_) => newer(),
        SnapshotError::History => history(),
        SnapshotError::Integrity => integrity(),
        SnapshotError::Unreadable => invalid_archive("The backup database cannot be opened"),
    })?;
    if !snapshot_matches_manifest(&facts, &manifest) {
        return Err(history());
    }
    if Instant::now() > deadline {
        return Err(budget());
    }
    let record = PendingRestore {
        source: source_kind,
        backup_id: backup.as_ref().map(|b| b.id.clone()),
        backup_name: backup.as_ref().map(|b| b.name.clone()),
        backup_type: manifest.backup_type,
        backup_created_at: manifest.created_at,
        schema_version: manifest.schema_version,
        application_version: manifest.application_version.clone(),
        staged_at: now(),
        database_sha256: manifest.files[0].sha256.clone(),
        database_bytes: manifest.files[0].size,
        restart_required: true,
    };
    let marker = serde_json::to_vec_pretty(&record).map_err(|_| io_failure())?;
    let publish_from = staging_path.clone();
    let publish_to = pending.clone();
    let dir = paths.dir.clone();
    tokio::task::spawn_blocking(move || -> Result<(), Error> {
        // Cancellation of the HTTP future must not release ownership or clean up the staging
        // directory while this blocking publication is still running.
        let _process_lock = _process_lock;
        let mut staging = staging;
        // Any (even stray) companion files would be applied with the candidate; none may exist.
        for suffix in ["-wal", "-shm", "-journal"] {
            if fs::symlink_metadata(publish_from.join(format!("{DATABASE_NAME}{suffix}"))).is_ok() {
                return Err(invalid_archive("The backup database is not self-contained"));
            }
        }
        let mut file =
            create_private(&publish_from.join("restore.json")).map_err(|_| io_failure())?;
        file.write_all(&marker).map_err(|_| io_failure())?;
        file.sync_all().map_err(|_| io_failure())?;
        sync_dir(&publish_from).map_err(|_| io_failure())?;
        if fs::symlink_metadata(&publish_to).is_ok() {
            return Err(already_pending());
        }
        fs::rename(&publish_from, &publish_to).map_err(|_| already_pending())?;
        staging.0 = None;
        sync_dir(&dir).map_err(|_| io_failure())
    })
    .await
    .map_err(|_| io_failure())??;
    println!(
        "event=backup_restore_staged source={} schema_version={} backup_type={}",
        match record.source {
            RestoreSource::Backup => "backup",
            RestoreSource::Upload => "upload",
        },
        record.schema_version,
        record.backup_type.dir()
    );
    Ok(record)
}

fn read_marker(path: &Path) -> io::Result<PendingRestore> {
    let mut bytes = Vec::new();
    open_untrusted(path)?
        .take(MAX_MARKER_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_MARKER_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "marker size"));
    }
    serde_json::from_slice(&bytes).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "marker"))
}

pub(crate) fn read_pending(db_path: &Path) -> Result<Option<PendingRestore>, Error> {
    let paths = Paths::new(db_path).map_err(|_| io_failure())?;
    let _process_lock = paths.lock().map_err(|_| io_failure())?;
    match read_marker(&paths.pending().join("restore.json")) {
        Ok(record) => Ok(Some(record)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(err(
            StatusCode::SERVICE_UNAVAILABLE,
            "restore_marker_invalid",
            "The pending restore marker is unreadable",
        )),
    }
}

/// Explicit cancellation removes the staged candidate; live state is untouched.
pub(crate) fn cancel_pending(db_path: &Path) -> Result<bool, Error> {
    let paths = Paths::new(db_path).map_err(|_| io_failure())?;
    let _process_lock = paths.lock().map_err(|_| io_failure())?;
    let pending = paths.pending();
    match fs::symlink_metadata(&pending) {
        Ok(meta) if meta.is_dir() => {
            if fs::symlink_metadata(pending.join("original")).is_ok() {
                // An apply has started; only startup may finish it.
                return Err(err(
                    StatusCode::CONFLICT,
                    "restore_in_progress",
                    "A restore apply is in progress and will finish at the next startup",
                ));
            }
            fs::remove_dir_all(&pending).map_err(|_| io_failure())?;
            sync_dir(&paths.dir).map_err(|_| io_failure())?;
            Ok(true)
        }
        Ok(_) => Err(io_failure()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(io_failure()),
    }
}

/// Startup outcome of a pending restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreOutcome {
    None,
    /// The staged database is live; the replaced database is retained in `original`.
    Applied {
        original: PathBuf,
    },
    /// Re-verification failed before anything moved; the candidate is quarantined and the live
    /// database is unchanged.
    Rejected {
        quarantined: PathBuf,
        reason: &'static str,
    },
}

fn file_sha256(path: &Path) -> io::Result<(u64, String)> {
    let mut file = open_untrusted(path)?;
    let mut sha = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = vec![0u8; CHUNK];
    let mut size = 0u64;
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        sha.update(&buffer[..n]);
        size += n as u64;
    }
    Ok((size, hex(sha.finish().as_ref())))
}

fn quarantine(paths: &Paths, reason: &'static str) -> Result<RestoreOutcome, crate::db::Error> {
    let target = paths.unique("rejected");
    fs::rename(paths.pending(), &target)?;
    sync_dir(&paths.dir)?;
    Ok(RestoreOutcome::Rejected {
        quarantined: target,
        reason,
    })
}

fn apply(db_path: &Path) -> Result<RestoreOutcome, crate::db::Error> {
    let paths = Paths::new(db_path)?;
    // Lock before even looking for pending state. Another startup may temporarily have no
    // live database, or may have just consumed pending state but not yet released ownership.
    // This guard stays in the blocking task even if its async caller times out.
    let _process_lock = paths
        .lock()
        .map_err(|e| format!("restore already owned or cannot be locked: {e}"))?;
    let pending = paths.pending();
    match fs::symlink_metadata(&pending) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(RestoreOutcome::None),
        Err(e) => return Err(e.into()),
        Ok(meta) if !meta.is_dir() => {
            return Err("pending restore path is not a directory; restore not applied".into());
        }
        Ok(_) => {}
    }
    let resumed = match fs::symlink_metadata(pending.join("original")) {
        Ok(meta) if meta.is_file() => {
            let mut name = String::new();
            open_untrusted(&pending.join("original"))?
                .take(MAX_MARKER_BYTES + 1)
                .read_to_string(&mut name)?;
            if name.len() as u64 > MAX_MARKER_BYTES {
                return Err("pending restore original marker is too large".into());
            }
            let name = name.trim();
            if name.contains(['/', '\\'])
                || !name.starts_with(&format!(".{}.restore-original-", paths.base))
            {
                return Err("pending restore names an invalid original directory".into());
            }
            Some(name.to_owned())
        }
        Ok(_) => return Err("pending restore original marker is not a regular file".into()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let record = match read_marker(&pending.join("restore.json")) {
        Ok(record) => record,
        Err(_) if resumed.is_none() => return quarantine(&paths, "marker_invalid"),
        Err(_) => return Err(
            "pending restore marker unreadable mid-apply; restore the retained original manually"
                .into(),
        ),
    };
    // Refuse to move a database another process owns.
    let live = paths.live("");
    let _lock = match fs::OpenOptions::new().read(true).write(true).open(&live) {
        Ok(file) => {
            file.try_lock()
                .map_err(|e| format!("database already owned or cannot be locked: {e}"))?;
            Some(file)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let staged = pending.join(DATABASE_NAME);
    let original = if fs::symlink_metadata(&staged).is_ok() {
        let (size, sha) = file_sha256(&staged)?;
        if size != record.database_bytes || sha != record.database_sha256 {
            if resumed.is_none() {
                return quarantine(&paths, "staged_checksum_mismatch");
            }
            return Err(
                "staged restore changed mid-apply; restore the retained original manually".into(),
            );
        }
        let original = match &resumed {
            Some(name) => paths.dir.join(name),
            None => {
                // No original marker means no live files have moved. A crash may have left an
                // empty/truncated unpublished marker; discard only a regular leaf, never follow
                // a symlink or remove an unexpected directory.
                let partial = pending.join("original.partial");
                match fs::symlink_metadata(&partial) {
                    Ok(meta) if meta.is_file() => fs::remove_file(&partial)?,
                    Ok(_) => {
                        return Err("pending restore partial marker is not a regular file".into());
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
                let original = paths.unique("original");
                let name = original
                    .file_name()
                    .and_then(|n| n.to_str())
                    .ok_or("original directory name")?
                    .to_owned();
                let mut file = create_private(&partial)?;
                file.write_all(name.as_bytes())?;
                file.sync_all()?;
                fs::rename(partial, pending.join("original"))?;
                sync_dir(&pending)?;
                original
            }
        };
        // Publish the marker before creating its directory, so repeated interrupted marker
        // writes cannot accumulate unused originals. Resume also handles a published marker
        // whose directory was not created yet, and refuses symlink/non-directory targets.
        match private_dir(&original) {
            Ok(()) => sync_dir(&paths.dir)?,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                if !fs::symlink_metadata(&original)?.is_dir() {
                    return Err("pending restore original path is not a directory".into());
                }
            }
            Err(e) => return Err(e.into()),
        }
        // Retain the live database with every journal companion, then install the candidate.
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let from = paths.live(suffix);
            if fs::symlink_metadata(&from).is_ok() {
                fs::rename(&from, original.join(format!("{DATABASE_NAME}{suffix}")))?;
            }
        }
        sync_dir(&original)?;
        sync_dir(&paths.dir)?;
        fs::rename(&staged, &live)?;
        sync_dir(&paths.dir)?;
        original
    } else {
        // Installed before an interruption: the live file must be exactly the candidate.
        let Some(name) = resumed.as_deref().map(str::trim) else {
            // Nothing has moved yet: set the incomplete candidate aside and keep the live database.
            return quarantine(&paths, "staged_database_missing");
        };
        let (size, sha) = file_sha256(&live).map_err(
            |_| "restored database missing mid-apply; recover from the retained original",
        )?;
        if size != record.database_bytes || sha != record.database_sha256 {
            return Err("live database does not match the staged restore; recover from the retained original".into());
        }
        paths.dir.join(name)
    };
    let applied = serde_json::to_vec_pretty(&record)?;
    let mut file = match create_private(&original.join("restore-applied.json")) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(original.join("restore-applied.json"))?,
        Err(e) => return Err(e.into()),
    };
    file.write_all(&applied)?;
    file.sync_all()?;
    sync_dir(&original)?;
    fs::remove_dir_all(&pending)?;
    sync_dir(&paths.dir)?;
    Ok(RestoreOutcome::Applied { original })
}

/// Called at startup before the database is opened: applies a pending restore, if any.
pub async fn apply_pending_restore(db_path: &Path) -> Result<RestoreOutcome, crate::db::Error> {
    let path = db_path.to_path_buf();
    let outcome = tokio::time::timeout(
        Duration::from_secs(600),
        tokio::task::spawn_blocking(move || apply(&path)),
    )
    .await
    .map_err(|_| "pending restore did not finish within its startup budget")??;
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    struct RecoveryFixture(PathBuf);

    impl RecoveryFixture {
        fn new() -> Self {
            let dir =
                std::env::temp_dir().join(format!("hrrdarr-recovery-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&dir).unwrap();
            Self(dir)
        }
        fn paths(&self) -> Paths {
            Paths::new(&self.0.join("custom.db")).unwrap()
        }
        fn stage(&self) {
            let paths = self.paths();
            fs::create_dir(paths.pending()).unwrap();
            // These are file-protocol tests: archive/database validation precedes publication.
            let candidate = b"validated candidate";
            fs::write(paths.pending().join(DATABASE_NAME), candidate).unwrap();
            let record = PendingRestore {
                source: RestoreSource::Upload,
                backup_id: None,
                backup_name: None,
                backup_type: super::super::BackupType::Manual,
                backup_created_at: 0,
                schema_version: 1,
                application_version: "test".into(),
                staged_at: 0,
                database_sha256: hex(
                    ring::digest::digest(&ring::digest::SHA256, candidate).as_ref()
                ),
                database_bytes: candidate.len() as u64,
                restart_required: true,
            };
            fs::write(
                paths.pending().join("restore.json"),
                serde_json::to_vec(&record).unwrap(),
            )
            .unwrap();
            for suffix in ["", "-wal", "-shm", "-journal"] {
                fs::write(paths.live(suffix), format!("original{suffix}")).unwrap();
            }
        }
        fn apply(&self) -> Result<RestoreOutcome, crate::db::Error> {
            apply(&self.paths().live(""))
        }
        fn assert_applied(&self, original: &Path) {
            assert_eq!(
                fs::read(self.paths().live("")).unwrap(),
                b"validated candidate"
            );
            for suffix in ["", "-wal", "-shm", "-journal"] {
                assert_eq!(
                    fs::read(original.join(format!("{DATABASE_NAME}{suffix}"))).unwrap(),
                    format!("original{suffix}").as_bytes()
                );
                if !suffix.is_empty() {
                    assert!(!self.paths().live(suffix).exists());
                }
            }
            assert!(!self.paths().pending().exists());
            assert!(original.join("restore-applied.json").is_file());
            assert_eq!(self.apply().unwrap(), RestoreOutcome::None);
        }
    }
    impl Drop for RecoveryFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn unpublished_original_markers_resume_without_extra_retention_directories() {
        for interrupted in [
            "",
            ".custom.db.restore-orig",
            ".custom.db.restore-original-complete",
        ] {
            let fixture = RecoveryFixture::new();
            fixture.stage();
            fs::write(
                fixture.paths().pending().join("original.partial"),
                interrupted,
            )
            .unwrap();
            let RestoreOutcome::Applied { original } = fixture.apply().unwrap() else {
                panic!("not applied")
            };
            fixture.assert_applied(&original);
            assert_eq!(
                fs::read_dir(&fixture.0)
                    .unwrap()
                    .filter(|entry| entry
                        .as_ref()
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".custom.db.restore-original-"))
                    .count(),
                1
            );
        }
    }

    #[test]
    fn published_original_marker_resumes_before_directory_creation() {
        let fixture = RecoveryFixture::new();
        fixture.stage();
        let name = ".custom.db.restore-original-recorded";
        fs::write(fixture.paths().pending().join("original"), name).unwrap();
        let original = fixture.0.join(name);
        assert!(!original.exists());
        assert_eq!(
            fixture.apply().unwrap(),
            RestoreOutcome::Applied {
                original: original.clone()
            }
        );
        fixture.assert_applied(&original);
    }

    #[cfg(unix)]
    #[test]
    fn unexpected_original_marker_types_preserve_live_and_candidate() {
        for name in ["original.partial", "original"] {
            for symlink in [false, true] {
                let fixture = RecoveryFixture::new();
                fixture.stage();
                let marker = fixture.paths().pending().join(name);
                let target = fixture.0.join("unrelated");
                fs::write(&target, "keep me").unwrap();
                if symlink {
                    std::os::unix::fs::symlink(&target, &marker).unwrap();
                } else {
                    fs::create_dir(&marker).unwrap();
                }
                assert!(fixture.apply().is_err());
                assert_eq!(fs::read(fixture.paths().live("")).unwrap(), b"original");
                assert_eq!(
                    fs::read(fixture.paths().pending().join(DATABASE_NAME)).unwrap(),
                    b"validated candidate"
                );
                assert_eq!(fs::read(target).unwrap(), b"keep me");
                assert!(fs::symlink_metadata(marker).is_ok());
            }
        }
    }

    // Invoked as a separate test process so the regression proves OS-level ownership.
    #[test]
    fn restore_lock_child_process() {
        let Some(dir) = std::env::var_os("HRRDARR_TEST_RESTORE_LOCK_DIR") else {
            return;
        };
        let dir = PathBuf::from(dir);
        let paths = Paths::new(&dir.join("custom.db")).unwrap();
        let _lock = paths.lock().unwrap();
        fs::write(dir.join("ready"), []).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !dir.join("release").exists() {
            assert!(
                Instant::now() < deadline,
                "parent did not release test lock"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn concurrent_startup_is_refused_even_without_live_database_or_pending_marker() {
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let fixture = RecoveryFixture::new();
        let mut child = Child(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "backup::restore::tests::restore_lock_child_process",
                    "--nocapture",
                ])
                .env("HRRDARR_TEST_RESTORE_LOCK_DIR", &fixture.0)
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while !fixture.0.join("ready").exists() {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "lock child exited early"
            );
            assert!(Instant::now() < deadline, "lock child did not become ready");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            fixture
                .apply()
                .unwrap_err()
                .to_string()
                .contains("restore already owned"),
            "pending inspection is protected too"
        );
        assert!(read_pending(&fixture.paths().live("")).is_err());
        assert!(cancel_pending(&fixture.paths().live("")).is_err());
        fixture.stage();
        let original = fixture.0.join(".custom.db.restore-original-retained");
        fs::create_dir(&original).unwrap();
        fs::write(
            fixture.paths().pending().join("original"),
            original.file_name().unwrap().as_encoded_bytes(),
        )
        .unwrap();
        fs::rename(fixture.paths().live(""), original.join(DATABASE_NAME)).unwrap();
        assert!(
            fixture
                .apply()
                .unwrap_err()
                .to_string()
                .contains("restore already owned")
        );
        assert!(!fixture.paths().live("").exists());
        assert_eq!(fs::read(original.join(DATABASE_NAME)).unwrap(), b"original");
        assert!(fixture.paths().pending().join(DATABASE_NAME).is_file());
        fs::write(fixture.0.join("release"), []).unwrap();
        assert!(child.0.wait().unwrap().success());
        assert_eq!(
            fixture.apply().unwrap(),
            RestoreOutcome::Applied {
                original: original.clone()
            }
        );
        fixture.assert_applied(&original);
    }

    #[tokio::test]
    async fn streamed_uploads_stop_at_the_bound_without_a_declared_length() {
        let dir = std::env::temp_dir().join(format!("hrrdarr-restore-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let over = receive(Body::from(vec![1u8; 4097]), dir.join("a"), 4096, deadline)
            .await
            .unwrap_err();
        assert_eq!(over.code(), "backup_too_large");
        let empty = receive(Body::empty(), dir.join("b"), 4096, deadline)
            .await
            .unwrap_err();
        assert_eq!(empty.code(), "backup_archive_invalid");
        receive(Body::from(vec![1u8; 4096]), dir.join("c"), 4096, deadline)
            .await
            .unwrap();
        assert_eq!(std::fs::metadata(dir.join("c")).unwrap().len(), 4096);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn manifests_must_match_this_builds_migration_prefix() {
        let history = db::known_migration_histories().remove(0);
        let mut manifest = BackupManifest {
            format: MANIFEST_FORMAT,
            application: super::super::APPLICATION.into(),
            application_version: "0".into(),
            backup_type: super::super::BackupType::Manual,
            created_at: 0,
            backend: super::super::BACKEND.into(),
            sqlite_version: "3".into(),
            schema_version: 2,
            migrations: history
                .iter()
                .take(2)
                .map(|(v, n, c)| super::super::ManifestMigration {
                    version: *v as u32,
                    name: (*n).into(),
                    checksum: c.clone(),
                })
                .collect(),
            files: vec![super::super::ManifestFile {
                name: DATABASE_NAME.into(),
                size: 1,
                sha256: "a".repeat(64),
            }],
            includes: vec![],
            excludes: vec![],
            secrets: super::super::ManifestSecrets {
                encrypted_credentials: 0,
                key_included: false,
                key_source: String::new(),
            },
        };
        // An older schema prefix is restorable; migrations then run at the next startup.
        assert!(check_manifest(&manifest).is_ok());
        manifest.migrations[1].name = "renamed".into();
        assert_eq!(
            check_manifest(&manifest).unwrap_err().code(),
            "backup_history_mismatch"
        );
        manifest.migrations.truncate(1);
        assert_eq!(
            check_manifest(&manifest).unwrap_err().code(),
            "backup_history_mismatch"
        );
        manifest.schema_version = (db::max_schema_version() + 1) as u32;
        assert_eq!(
            check_manifest(&manifest).unwrap_err().code(),
            "backup_schema_unsupported"
        );
    }
    #[tokio::test]
    async fn backup_restore_preserves_each_published_history_and_rejects_crossed_manifests() {
        for history in db::test_migration_histories() {
            let dir = std::env::temp_dir()
                .join(format!("hrrdarr-history-backup-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&dir).unwrap();
            let path = dir.join("database.db");
            let (raw, c) = db::published_prefix_tests::seed_lineage(&path, history, history.len())
                .await
                .unwrap();
            c.execute(
                "INSERT INTO series(id,title,path) VALUES(1,'Before backup','/tv/kept')",
                (),
            )
            .await
            .unwrap();
            drop(c);
            drop(raw);
            let database = db::Database::open_local(&path).await.unwrap();
            let published = super::super::create_backup(
                &database,
                super::super::BackupType::Manual,
                super::super::BackupLimits::startup(),
            )
            .await
            .unwrap();
            let archive_path = dir.join("Backups/manual").join(&published.backup.name);
            let manifest = read_manifest(&archive_path).unwrap();
            assert_eq!(manifest.schema_version as usize, history.len());
            assert!(check_manifest(&manifest).is_ok());
            let snapshot = dir.join("identity.db");
            let c = database.connect().await.unwrap();
            db::vacuum_into(&c, &snapshot).await.unwrap();
            let facts = db::inspect_snapshot(&snapshot, SnapshotOpen::ReadOnly)
                .await
                .unwrap();
            assert!(snapshot_matches_manifest(&facts, &manifest));
            if history.len() == 83 {
                let mut crossed = manifest.clone();
                let other = db::known_migration_histories()
                    .into_iter()
                    .find(|h| h.len() == 83 && h[78].1 != manifest.migrations[78].name)
                    .unwrap();
                crossed.migrations = other
                    .into_iter()
                    .map(|(v, n, c)| super::super::ManifestMigration {
                        version: v as u32,
                        name: n.into(),
                        checksum: c,
                    })
                    .collect();
                assert!(
                    check_manifest(&crossed).is_ok(),
                    "both exact published histories are supported"
                );
                assert!(
                    !snapshot_matches_manifest(&facts, &crossed),
                    "same version does not establish the same history"
                );
            }
            c.execute("UPDATE series SET title='After backup' WHERE id=1", ())
                .await
                .unwrap();
            stage(
                &database,
                Source::Backup {
                    backup: published.backup,
                    path: archive_path,
                },
                super::super::ApiLimits::default(),
            )
            .await
            .unwrap();
            drop(c);
            drop(database);
            assert!(matches!(
                apply_pending_restore(&path).await.unwrap(),
                RestoreOutcome::Applied { .. }
            ));
            let restored = db::Database::open_local(&path).await.unwrap();
            let c = restored.connect().await.unwrap();
            assert_eq!(
                c.query("SELECT title FROM series WHERE id=1", ())
                    .await
                    .unwrap()
                    .next()
                    .await
                    .unwrap()
                    .unwrap()
                    .get::<String>(0)
                    .unwrap(),
                "Before backup"
            );
            drop(c);
            drop(restored);
            fs::remove_dir_all(dir).unwrap();
        }
    }
}
