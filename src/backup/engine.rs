//! Backup creation: private staging, consistent snapshot, manifest, archive verification,
//! no-clobber publication, then retention pruning of the same type only after success.
use super::{
    BackupFile, BackupManifest, BackupType, DATABASE_NAME, MANIFEST_FORMAT, MANIFEST_NAME,
    ManifestFile, ManifestMigration, ManifestSecrets, archive,
};
use crate::db::{self, Database, SnapshotOpen};
use std::{
    fs::{self, File},
    io::{self, BufWriter, Read},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Maximum materialized inventory; overflow is an error, never a partial listing.
pub(crate) const MAX_LISTED: usize = 100_000;
const MAX_STAGING_ENTRIES: usize = 1000;
const STAGING: &str = ".staging";
const STALE_STAGING_SECONDS: u64 = 24 * 60 * 60;
const NAME_PREFIX: &str = "hrrdarr_backup_";
const NAME_SUFFIX: &str = ".zip";
const STAMP: &str = "%Y%m%dT%H%M%SZ";
const CHUNK: usize = 64 * 1024;

/// Bounds for one backup. The command worker abandons an iteration after 45 seconds, so the
/// command budget stays below that; startup update backups may take longer.
#[derive(Clone, Copy, Debug)]
#[doc(hidden)]
pub struct BackupLimits {
    pub max_database_bytes: u64,
    pub max_archive_bytes: u64,
    pub budget: Duration,
}
impl BackupLimits {
    pub fn command() -> Self {
        Self {
            max_database_bytes: archive::MAX_ARCHIVE_BYTES - 1024 * 1024,
            max_archive_bytes: archive::MAX_ARCHIVE_BYTES,
            budget: Duration::from_secs(35),
        }
    }
    pub fn startup() -> Self {
        Self {
            budget: Duration::from_secs(600),
            ..Self::command()
        }
    }
}

/// A failed backup's stable code (stored on the command row). Nothing was published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackupFailure(pub &'static str);
impl std::fmt::Display for BackupFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for BackupFailure {}
const LOCAL_ONLY: BackupFailure = BackupFailure("backup_local_only");
const FOLDER: BackupFailure = BackupFailure("backup_folder_unavailable");
const UNSAFE: BackupFailure = BackupFailure("backup_folder_unsafe");
const TOO_LARGE: BackupFailure = BackupFailure("backup_too_large");
const TIMEOUT: BackupFailure = BackupFailure("backup_timeout");
const SNAPSHOT: BackupFailure = BackupFailure("snapshot_failed");
const INVALID: BackupFailure = BackupFailure("snapshot_invalid");
const ARCHIVE: BackupFailure = BackupFailure("archive_failed");
const PUBLISH: BackupFailure = BackupFailure("publish_failed");
const STORAGE: BackupFailure = BackupFailure("storage_error");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    pub backup: BackupFile,
    /// Older backups of the same type removed by retention after this one was published.
    pub pruned: u64,
}

pub(crate) fn backup_id(backup_type: BackupType, name: &str) -> String {
    db::sha256_hex(format!("{}/{name}", backup_type.dir()).as_bytes())[..32].to_owned()
}

/// Generated names only: `hrrdarr_backup_<UTC stamp>_<8 hex>.zip`.
pub(crate) fn parse_name(name: &str) -> Option<i64> {
    let core = name.strip_prefix(NAME_PREFIX)?.strip_suffix(NAME_SUFFIX)?;
    let (stamp, random) = core.split_once('_')?;
    if random.len() != 8
        || !random
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    let parsed = chrono::NaiveDateTime::parse_from_str(stamp, STAMP).ok()?;
    if parsed.format(STAMP).to_string() != stamp {
        return None;
    }
    Some(parsed.and_utc().timestamp())
}
fn new_name(created_at: i64) -> Option<String> {
    let stamp = chrono::DateTime::from_timestamp(created_at, 0)?
        .format(STAMP)
        .to_string();
    let random = uuid::Uuid::new_v4().simple().to_string();
    Some(format!(
        "{NAME_PREFIX}{stamp}_{}{NAME_SUFFIX}",
        &random[..8]
    ))
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn private_dir(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "not a real directory",
        )),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(path)
        }
        Err(e) => Err(e),
    }
}

/// The configured folder may itself be a mount or symlink; everything below it is created as
/// private real directories.
fn ensure_folder(folder: &Path) -> io::Result<()> {
    if !folder.is_dir() {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(folder)?;
    }
    if !folder.is_dir() {
        return Err(io::Error::new(io::ErrorKind::NotADirectory, "folder"));
    }
    Ok(())
}

/// Settings-time check: the folder can be created and written. Leaves no probe file behind.
pub(crate) fn probe_writable(folder: &Path) -> io::Result<()> {
    ensure_folder(folder)?;
    let probe = folder.join(format!(".hrrdarr-probe-{}", uuid::Uuid::new_v4().simple()));
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)?;
    drop(file);
    fs::remove_file(probe)
}

fn sync_dir(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

/// Removes a private staging directory on every exit path.
struct Staging(PathBuf);
impl Drop for Staging {
    fn drop(&mut self) {
        if let Err(e) = fs::remove_dir_all(&self.0) {
            if e.kind() != io::ErrorKind::NotFound {
                eprintln!("event=backup_staging_cleanup_failed kind={:?}", e.kind());
            }
        }
    }
}

fn prepare(folder: &Path, backup_type: BackupType) -> io::Result<(PathBuf, Staging)> {
    ensure_folder(folder)?;
    let type_dir = folder.join(backup_type.dir());
    private_dir(&type_dir)?;
    let staging_root = folder.join(STAGING);
    private_dir(&staging_root)?;
    // Crash leftovers only: other instances sharing the folder use fresh unique directories.
    if let Ok(entries) = fs::read_dir(&staging_root) {
        for entry in entries.flatten().take(MAX_STAGING_ENTRIES) {
            let old = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age.as_secs() > STALE_STAGING_SECONDS);
            if old && entry.file_type().is_ok_and(|t| t.is_dir()) {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }
    let staging = staging_root.join(uuid::Uuid::new_v4().to_string());
    private_dir(&staging)?;
    Ok((type_dir, Staging(staging)))
}

struct Digest {
    size: u64,
    sha256: String,
    crc: u32,
}
fn digest(path: &Path, deadline: Instant) -> Result<Digest, BackupFailure> {
    let mut file = File::open(path).map_err(|_| ARCHIVE)?;
    let mut sha = ring::digest::Context::new(&ring::digest::SHA256);
    let mut crc = archive::Crc32::new();
    let mut size = 0u64;
    let mut buffer = vec![0u8; CHUNK];
    loop {
        if Instant::now() > deadline {
            return Err(TIMEOUT);
        }
        let n = file.read(&mut buffer).map_err(|_| ARCHIVE)?;
        if n == 0 {
            break;
        }
        sha.update(&buffer[..n]);
        crc.update(&buffer[..n]);
        size += n as u64;
    }
    Ok(Digest {
        size,
        sha256: hex(sha.finish().as_ref()),
        crc: crc.finish(),
    })
}
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn manifest(
    backup_type: BackupType,
    created_at: i64,
    facts: &db::SnapshotFacts,
    file: &Digest,
    encrypted_credentials: u64,
) -> BackupManifest {
    BackupManifest {
        format: MANIFEST_FORMAT,
        application: super::APPLICATION.into(),
        application_version: env!("CARGO_PKG_VERSION").into(),
        backup_type,
        created_at,
        backend: super::BACKEND.into(),
        sqlite_version: facts.sqlite_version.clone(),
        schema_version: facts.schema_version as u32,
        migrations: db::migration_history()
            .into_iter()
            .take(facts.schema_version)
            .map(|(version, name, checksum)| ManifestMigration {
                version: version as u32,
                name: name.into(),
                checksum,
            })
            .collect(),
        files: vec![ManifestFile {
            name: DATABASE_NAME.into(),
            size: file.size,
            sha256: file.sha256.clone(),
        }],
        includes: vec![
            "database: library, settings, history, commands and provider/notification configuration".into(),
            "credentials: AEAD ciphertext only, as stored".into(),
        ],
        excludes: vec![
            "credential key material (supply HRRDARR_PROVIDER_KEY to the restored instance)".into(),
            "media files, subtitles, sidecars and recycle bin contents".into(),
            "environment configuration (bind address, allowed hosts, trusted networks)".into(),
            "external service state (download clients, indexers, notification targets)".into(),
            "browser preferences".into(),
        ],
        secrets: ManifestSecrets {
            encrypted_credentials,
            key_included: false,
            key_source: super::KEY_SOURCE.into(),
        },
    }
}

/// Writes the archive into staging, then re-reads it through the restore validator.
fn write_archive(
    staging: &Path,
    manifest: &BackupManifest,
    database: &Path,
    digest: &Digest,
    limits: &BackupLimits,
    deadline: Instant,
) -> Result<(PathBuf, u64), BackupFailure> {
    let manifest_bytes = serde_json::to_vec_pretty(manifest).map_err(|_| ARCHIVE)?;
    let mut manifest_crc = archive::Crc32::new();
    manifest_crc.update(&manifest_bytes);
    let partial = staging.join("archive.partial");
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&partial).map_err(|_| ARCHIVE)?;
    let mut writer = archive::Writer::new(
        BufWriter::with_capacity(CHUNK, file),
        manifest.created_at,
        limits.max_archive_bytes,
    );
    let fail = |e: io::Error| {
        if e.kind() == io::ErrorKind::FileTooLarge {
            TOO_LARGE
        } else if e.kind() == io::ErrorKind::TimedOut {
            TIMEOUT
        } else {
            ARCHIVE
        }
    };
    writer
        .entry(
            MANIFEST_NAME,
            manifest_bytes.len() as u64,
            manifest_crc.finish(),
            &mut io::Cursor::new(&manifest_bytes),
            &mut |_| Ok(()),
        )
        .map_err(fail)?;
    let mut source = File::open(database).map_err(|_| ARCHIVE)?;
    let mut sha = ring::digest::Context::new(&ring::digest::SHA256);
    writer
        .entry(
            DATABASE_NAME,
            digest.size,
            digest.crc,
            &mut source,
            &mut |chunk| {
                if Instant::now() > deadline {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "budget"));
                }
                sha.update(chunk);
                Ok(())
            },
        )
        .map_err(fail)?;
    if hex(sha.finish().as_ref()) != digest.sha256 {
        return Err(ARCHIVE);
    }
    let (out, size) = writer.finish().map_err(fail)?;
    let file = out.into_inner().map_err(|_| ARCHIVE)?;
    file.sync_all().map_err(|_| ARCHIVE)?;
    drop(file);
    // Publish only what the restore path itself accepts (structure, CRCs, manifest, checksums).
    let verified = super::restore::verify_archive(&partial, limits.max_archive_bytes, deadline)
        .map_err(|e| {
            if e.1 == "backup_timeout" {
                TIMEOUT
            } else {
                ARCHIVE
            }
        })?;
    if &verified != manifest {
        return Err(ARCHIVE);
    }
    Ok((partial, size))
}

/// No-clobber publication: a hard link fails if the name exists; filesystems without hard links
/// fall back to rename after an existence check (single backup worker per folder owner).
fn publish(partial: &Path, type_dir: &Path, created_at: i64) -> Result<String, BackupFailure> {
    for _ in 0..3 {
        let name = new_name(created_at).ok_or(PUBLISH)?;
        let target = type_dir.join(&name);
        match fs::hard_link(partial, &target) {
            Ok(()) => {
                let _ = fs::remove_file(partial);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(_) => {
                if fs::symlink_metadata(&target).is_ok() {
                    continue;
                }
                fs::rename(partial, &target).map_err(|_| PUBLISH)?;
            }
        }
        sync_dir(type_dir).map_err(|_| PUBLISH)?;
        return Ok(name);
    }
    Err(PUBLISH)
}

fn inventory_deadline(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "inventory deadline",
        ));
    }
    Ok(())
}

/// Visits generated regular files without buffering directory contents; false stops the scan.
fn scan(
    folder: &Path,
    filter: Option<BackupType>,
    deadline: Instant,
    mut visit: impl FnMut(BackupFile) -> io::Result<bool>,
) -> io::Result<()> {
    for backup_type in BackupType::ALL {
        inventory_deadline(deadline)?;
        if filter.is_some_and(|f| f != backup_type) {
            continue;
        }
        let dir = folder.join(backup_type.dir());
        match fs::symlink_metadata(&dir) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => continue,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        }
        for entry in fs::read_dir(&dir)? {
            inventory_deadline(deadline)?;
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(created_at) = parse_name(&name) else {
                continue;
            };
            if !entry.file_type()?.is_file() {
                continue;
            }
            let backup = BackupFile {
                id: backup_id(backup_type, &name),
                size_bytes: entry.metadata()?.len(),
                name,
                backup_type,
                created_at,
            };
            inventory_deadline(deadline)?;
            if !visit(backup)? {
                return Ok(());
            }
        }
    }
    inventory_deadline(deadline)
}

/// Complete sorted inventory or an explicit resource/deadline error.
pub(crate) fn list(
    folder: &Path,
    filter: Option<BackupType>,
    deadline: Instant,
) -> io::Result<Vec<BackupFile>> {
    let mut out = Vec::new();
    scan(folder, filter, deadline, |backup| {
        if out.len() == MAX_LISTED {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "inventory limit",
            ));
        }
        out.push(backup);
        Ok(true)
    })?;
    out.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.name.cmp(&a.name))
    });
    inventory_deadline(deadline)?;
    Ok(out)
}

pub(crate) fn find(
    folder: &Path,
    id: &str,
    deadline: Instant,
) -> io::Result<Option<(BackupFile, PathBuf)>> {
    let mut found = None;
    scan(folder, None, deadline, |backup| {
        if backup.id != id {
            return Ok(true);
        }
        let path = folder.join(backup.backup_type.dir()).join(&backup.name);
        found = Some((backup, path));
        Ok(false)
    })?;
    Ok(found)
}

pub(crate) fn remove(path: &Path) -> io::Result<()> {
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a file"));
    }
    fs::remove_file(path)?;
    if let Some(parent) = path.parent() {
        sync_dir(parent)?;
    }
    Ok(())
}

/// Removes backups of one type older than the retention window, never `keep`.
fn prune(
    folder: &Path,
    backup_type: BackupType,
    retention_days: i64,
    keep: &str,
    now: i64,
    deadline: Instant,
) -> u64 {
    let cutoff = now - retention_days * 86_400;
    let Ok(all) = list(folder, Some(backup_type), deadline) else {
        eprintln!("event=backup_prune_failed code=list");
        return 0;
    };
    let mut removed = 0;
    for backup in all {
        if inventory_deadline(deadline).is_err() {
            eprintln!("event=backup_prune_failed code=timeout");
            break;
        }
        if backup.name == keep || backup.created_at >= cutoff {
            continue;
        }
        match remove(&folder.join(backup_type.dir()).join(&backup.name)) {
            Ok(()) => removed += 1,
            Err(e) => eprintln!("event=backup_prune_failed kind={:?}", e.kind()),
        }
    }
    removed
}

/// Runs `work` against libSQL on a blocking thread so a long snapshot never stalls the executor.
async fn off_executor<T: Send + 'static>(
    work: impl FnOnce(tokio::runtime::Handle) -> T + Send + 'static,
) -> Result<T, BackupFailure> {
    let handle = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || work(handle))
        .await
        .map_err(|_| SNAPSHOT)
}

async fn encrypted_credentials(path: PathBuf) -> u64 {
    let count = async move {
        let copy = libsql::Builder::new_local(&path)
            .flags(libsql::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .build()
            .await?;
        let c = copy.connect()?;
        let mut total = 0u64;
        for table in ["providers", "notifications"] {
            let exists: i64 = c
                .query(
                    "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name=?1",
                    [table],
                )
                .await?
                .next()
                .await?
                .ok_or(libsql::Error::QueryReturnedNoRows)?
                .get(0)?;
            if exists == 1 {
                let n: i64 = c
                    .query(
                        &format!("SELECT count(*) FROM {table} WHERE credentials IS NOT NULL"),
                        (),
                    )
                    .await?
                    .next()
                    .await?
                    .ok_or(libsql::Error::QueryReturnedNoRows)?
                    .get(0)?;
                total += n.max(0) as u64;
            }
        }
        Ok::<u64, libsql::Error>(total)
    };
    count.await.unwrap_or(0)
}

/// Settings and the resolved folder, with the media-overlap guard.
async fn destination(db: &Database) -> Result<(PathBuf, i64), BackupFailure> {
    let path = db.local_path().ok_or(LOCAL_ONLY)?.to_path_buf();
    let c = db.connect().await.map_err(|_| STORAGE)?;
    let stored = super::read_settings(&c).await.map_err(|_| STORAGE)?;
    let folder = super::resolve_folder(&path, stored.folder.as_deref()).map_err(|_| FOLDER)?;
    let text = folder.to_str().ok_or(UNSAFE)?.to_owned();
    if super::overlaps_media(&c, &text)
        .await
        .map_err(|_| STORAGE)?
    {
        return Err(UNSAFE);
    }
    Ok((folder, stored.retention_days))
}

enum Source {
    Live,
    /// A closed pre-migration snapshot file, copied (never modified) into staging.
    Copy(PathBuf),
}

async fn run(
    db: &Database,
    backup_type: BackupType,
    source: Source,
    limits: BackupLimits,
) -> Result<Published, BackupFailure> {
    let started = Instant::now();
    let deadline = started + limits.budget;
    let created_at = now();
    let (folder, retention_days) = destination(db).await?;
    let prepare_folder = folder.clone();
    let (type_dir, staging) =
        tokio::task::spawn_blocking(move || prepare(&prepare_folder, backup_type))
            .await
            .map_err(|_| FOLDER)?
            .map_err(|_| FOLDER)?;
    let snapshot = staging.0.join(DATABASE_NAME);
    match source {
        Source::Live => {
            let conn = db.connect().await.map_err(|_| STORAGE)?;
            let pages = |sql: &'static str| {
                let conn = conn.clone();
                async move {
                    conn.query(sql, ())
                        .await?
                        .next()
                        .await?
                        .ok_or(libsql::Error::QueryReturnedNoRows)?
                        .get::<i64>(0)
                }
            };
            let bytes = pages("PRAGMA page_count").await.map_err(|_| SNAPSHOT)?
                * pages("PRAGMA page_size").await.map_err(|_| SNAPSHOT)?;
            if bytes.max(0) as u64 > limits.max_database_bytes {
                return Err(TOO_LARGE);
            }
            // A stuck or oversized VACUUM is interrupted at the deadline instead of outliving
            // the worker iteration.
            let interrupter = conn.clone();
            let remaining = deadline.saturating_duration_since(Instant::now());
            let timer = tokio::spawn(async move {
                tokio::time::sleep(remaining).await;
                let _ = interrupter.interrupt();
            });
            let target = snapshot.clone();
            let result = off_executor(move |handle| {
                handle.block_on(async move { db::vacuum_into(&conn, &target).await })
            })
            .await;
            timer.abort();
            if Instant::now() > deadline {
                return Err(TIMEOUT);
            }
            result?.map_err(|_| SNAPSHOT)?;
        }
        Source::Copy(path) => {
            let target = snapshot.clone();
            tokio::task::spawn_blocking(move || -> io::Result<()> {
                let mut options = fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                let mut out = options.open(&target)?;
                io::copy(&mut File::open(&path)?, &mut out)?;
                out.sync_all()
            })
            .await
            .map_err(|_| SNAPSHOT)?
            .map_err(|_| SNAPSHOT)?;
        }
    }
    let inspect = snapshot.clone();
    let facts = off_executor(move |handle| {
        handle.block_on(db::inspect_snapshot(&inspect, SnapshotOpen::SelfContained))
    })
    .await?
    .map_err(|_| INVALID)?;
    let count_path = snapshot.clone();
    let encrypted =
        off_executor(move |handle| handle.block_on(encrypted_credentials(count_path))).await?;
    let work_snapshot = snapshot.clone();
    let staging_dir = staging.0.clone();
    let publish_folder = folder.clone();
    let (published, pruned) = tokio::task::spawn_blocking(move || {
        let _staging = staging;
        // The snapshot is closed and self-contained (no -wal/-journal) from here on.
        let digest = digest(&work_snapshot, deadline)?;
        if digest.size > limits.max_database_bytes {
            return Err(TOO_LARGE);
        }
        let manifest = manifest(backup_type, created_at, &facts, &digest, encrypted);
        let (partial, size) = write_archive(
            &staging_dir,
            &manifest,
            &work_snapshot,
            &digest,
            &limits,
            deadline,
        )?;
        if Instant::now() > deadline {
            return Err(TIMEOUT);
        }
        let name = publish(&partial, &type_dir, created_at)?;
        let backup = BackupFile {
            id: backup_id(backup_type, &name),
            name,
            backup_type,
            size_bytes: size,
            created_at,
        };
        // Retention runs only after a successful publish, and never for manual backups.
        let pruned = if backup_type == BackupType::Manual {
            0
        } else {
            prune(
                &publish_folder,
                backup_type,
                retention_days,
                &backup.name,
                now(),
                deadline,
            )
        };
        Ok((backup, pruned))
    })
    .await
    .map_err(|_| ARCHIVE)??;
    println!(
        "event=backup_published backup_id={} backup_type={} size_bytes={} pruned={} elapsed_ms={}",
        published.id,
        backup_type.dir(),
        published.size_bytes,
        pruned,
        started.elapsed().as_millis()
    );
    Ok(Published {
        backup: published,
        pruned,
    })
}

/// Creates a manual or scheduled backup of the live database.
#[doc(hidden)]
pub async fn create_backup(
    db: &Database,
    backup_type: BackupType,
    limits: BackupLimits,
) -> Result<Published, BackupFailure> {
    if backup_type == BackupType::Update {
        return Err(SNAPSHOT);
    }
    let result = run(db, backup_type, Source::Live, limits).await;
    if let Err(failure) = result {
        eprintln!(
            "event=backup_failed backup_type={} code={}",
            backup_type.dir(),
            failure.0
        );
    }
    result
}

/// Publishes the pre-migration snapshot taken by the migration runner as an `update` backup.
/// The runner's own recovery directory is left untouched.
pub async fn publish_update(
    db: &Database,
    migration_dir: &Path,
) -> Result<Published, BackupFailure> {
    let source = migration_dir.join(DATABASE_NAME);
    let result = run(
        db,
        BackupType::Update,
        Source::Copy(source),
        BackupLimits::startup(),
    )
    .await;
    if let Err(failure) = result {
        eprintln!("event=backup_failed backup_type=update code={}", failure.0);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_names_round_trip_and_foreign_names_are_ignored() {
        let name = new_name(1_760_000_000).unwrap();
        assert_eq!(parse_name(&name), Some(1_760_000_000));
        for bad in [
            "hrrdarr_backup_20250101T000000Z_zzzzzzzz.zip",
            "hrrdarr_backup_20251301T000000Z_01234567.zip",
            "hrrdarr_backup_2025011T000000Z_01234567.zip",
            "../hrrdarr_backup_20250101T000000Z_01234567.zip",
            "hrrdarr_backup_20250101T000000Z_01234567.zip.partial",
            "notes.txt",
        ] {
            assert_eq!(parse_name(bad), None, "{bad}");
        }
        assert_ne!(
            backup_id(BackupType::Manual, &name),
            backup_id(BackupType::Scheduled, &name)
        );
    }
    #[test]
    fn expired_inventory_never_reports_a_missing_backup_or_empty_list() {
        let absent = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        assert_eq!(
            list(&absent, None, Instant::now()).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert_eq!(
            find(&absent, &"0".repeat(32), Instant::now())
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
    }
}
