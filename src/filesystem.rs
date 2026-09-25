//! Read-only Linux filesystem lookup, classification and domain-scoped media enumeration.
use crate::{
    api::{ApiErrorEnvelope, MediaDomain},
    import::filesystem_directory,
    root_folders::{BUDGET, OBSERVERS},
};
use axum::{
    Json, Router,
    extract::{Query, rejection::QueryRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use rustix::fs::{self, AtFlags, FileType, Mode, OFlags, Stat};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    path::{Component, Path, PathBuf},
    time::Instant,
};

const MAX_PATH_BYTES: usize = 4096;
const MAX_ENTRIES: usize = 10_000;
const MAX_DEPTH: usize = 32;
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
#[derive(Debug)]
struct Error(StatusCode, &'static str);
type Result<T> = std::result::Result<T, Error>;
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (self.0, Json(ApiErrorEnvelope::new(self.1, self.1))).into_response()
    }
}
fn bad() -> Error {
    Error(StatusCode::BAD_REQUEST, "invalid_filesystem_query")
}
fn unavailable() -> Error {
    Error(StatusCode::SERVICE_UNAVAILABLE, "filesystem_unavailable")
}
fn timeout() -> Error {
    Error(StatusCode::GATEWAY_TIMEOUT, "filesystem_timeout")
}
fn limit(code: &'static str) -> Error {
    Error(StatusCode::PAYLOAD_TOO_LARGE, code)
}
fn io(error: impl Into<std::io::Error>) -> Error {
    let error = error.into();
    eprintln!(
        "{}",
        serde_json::json!({"level":"ERROR","component":"filesystem","condition":"observation_failed","os_code":error.raw_os_error(),"kind":format!("{:?}",error.kind())})
    );
    unavailable()
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct FilesystemLookup {
    #[ts(optional)]
    pub path: Option<String>,
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    pub include_files: bool,
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    pub allow_folders_without_trailing_slashes: bool,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct FilesystemPath {
    pub path: String,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct FilesystemMediaQuery {
    pub path: String,
    pub media_type: MediaDomain,
}
#[derive(Clone, Copy, Debug, Serialize, ts_rs::TS, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FilesystemKind {
    File,
    Folder,
}
#[derive(Serialize, ts_rs::TS)]
pub struct FilesystemType {
    pub r#type: FilesystemKind,
}
#[derive(Serialize, ts_rs::TS)]
pub struct FilesystemEntry {
    pub r#type: FilesystemKind,
    pub name: String,
    pub path: String,
    pub extension: Option<String>,
    pub size: Option<u64>,
    pub last_modified: Option<String>,
}
#[derive(Serialize, ts_rs::TS)]
pub struct FilesystemContents {
    pub path: String,
    pub parent: Option<String>,
    pub directories: Vec<FilesystemEntry>,
    pub files: Vec<FilesystemEntry>,
}
#[derive(Serialize, ts_rs::TS)]
pub struct FilesystemMediaFile {
    pub path: String,
    pub relative_path: String,
    pub name: String,
}
#[derive(Serialize, ts_rs::TS)]
pub struct FilesystemMediaFiles {
    pub path: String,
    pub media_type: MediaDomain,
    pub files: Vec<FilesystemMediaFile>,
}

pub fn router() -> Router {
    Router::new()
        .route("/api/v1/filesystem", get(contents))
        .route("/api/v1/filesystem/type", get(entity_type))
        .route("/api/v1/filesystem/media-files", get(media_files))
}
fn query<T>(value: std::result::Result<Query<T>, QueryRejection>) -> Result<T> {
    value.map(|q| q.0).map_err(|_| bad())
}
fn path(value: &str) -> Result<PathBuf> {
    if value.len() > MAX_PATH_BYTES
        || value.contains('\0')
        || !value.starts_with('/')
        || value.starts_with("//")
    {
        return Err(bad());
    }
    let raw = if value == "/" {
        value
    } else {
        value.strip_suffix('/').unwrap_or(value)
    };
    let p: PathBuf = Path::new(raw).components().collect();
    if p.to_str() != Some(raw)
        || p.components()
            .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
    {
        return Err(bad());
    }
    Ok(p)
}
fn lookup_path(raw: Option<&str>) -> Result<(PathBuf, bool)> {
    if raw.is_some_and(|p| p.len() > MAX_PATH_BYTES) {
        return Err(bad());
    }
    let raw = raw.filter(|p| !p.trim().is_empty()).unwrap_or("/");
    Ok((path(raw)?, raw.ends_with('/')))
}
fn output_path_limit(value: &str) -> Result<()> {
    if value.len() > MAX_PATH_BYTES {
        Err(limit("filesystem_path_limit"))
    } else {
        Ok(())
    }
}
fn directory_path(p: &Path) -> String {
    let text = p.to_str().expect("validated UTF8");
    if text == "/" {
        text.into()
    } else {
        format!("{text}/")
    }
}
fn parent(p: &Path) -> Option<String> {
    p.parent().map(directory_path)
}
async fn worker<T: Send + 'static>(
    action: impl FnOnce(Instant) -> Result<T> + Send + 'static,
) -> Result<T> {
    worker_budget(BUDGET, action).await
}
async fn worker_budget<T: Send + 'static>(
    budget: std::time::Duration,
    action: impl FnOnce(Instant) -> Result<T> + Send + 'static,
) -> Result<T> {
    let permit = OBSERVERS
        .try_acquire()
        .map_err(|_| Error(StatusCode::SERVICE_UNAVAILABLE, "filesystem_busy"))?;
    let deadline = Instant::now() + budget;
    // The actual blocking job owns the shared permit even if its HTTP waiter is cancelled.
    let job = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        if Instant::now() >= deadline {
            return Err(timeout());
        }
        let result = action(deadline);
        if Instant::now() >= deadline {
            return Err(timeout());
        }
        result
    });
    match tokio::time::timeout(budget, job).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err(Error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "filesystem_worker_failed",
        )),
        Err(_) => Err(timeout()),
    }
}
fn bounded<T: Serialize>(value: T) -> Result<Json<T>> {
    if serde_json::to_vec(&value).map_err(|_| unavailable())?.len() > MAX_RESPONSE_BYTES {
        return Err(limit("filesystem_response_limit"));
    }
    Ok(Json(value))
}
async fn contents(
    q: std::result::Result<Query<FilesystemLookup>, QueryRejection>,
) -> Result<Json<FilesystemContents>> {
    let q = query(q)?;
    let (p, trailing) = lookup_path(q.path.as_deref())?;
    bounded(
        worker(move |deadline| {
            lookup(
                p,
                trailing,
                q.include_files,
                q.allow_folders_without_trailing_slashes,
                deadline,
            )
        })
        .await?,
    )
}
async fn entity_type(
    q: std::result::Result<Query<FilesystemPath>, QueryRejection>,
) -> Result<Json<FilesystemType>> {
    let raw = query(q)?.path;
    let p = path(&raw)?;
    let trailing = raw.ends_with('/');
    bounded(
        worker(move |deadline| {
            if Instant::now() >= deadline {
                return Err(timeout());
            }
            classify(&p, trailing)
        })
        .await?,
    )
}
async fn media_files(
    q: std::result::Result<Query<FilesystemMediaQuery>, QueryRejection>,
) -> Result<Json<FilesystemMediaFiles>> {
    let q = query(q)?;
    let p = path(&q.path)?;
    bounded(worker(move |deadline| scan_media(p, q.media_type, Limits::new(deadline))).await?)
}
fn classify(p: &Path, trailing: bool) -> Result<FilesystemType> {
    if trailing {
        return Ok(FilesystemType {
            r#type: FilesystemKind::Folder,
        });
    }
    let kind = if let (Some(parent), Some(name)) = (p.parent(), p.file_name()) {
        match filesystem_directory(parent) {
            Ok(fd) => match fs::statat(&fd, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) => {
                    if FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile {
                        FilesystemKind::File
                    } else {
                        FilesystemKind::Folder
                    }
                }
                Err(rustix::io::Errno::NOENT | rustix::io::Errno::NOTDIR) => FilesystemKind::Folder,
                Err(e) => return Err(io(e)),
            },
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                FilesystemKind::Folder
            }
            Err(e) => return Err(io(e)),
        }
    } else {
        FilesystemKind::Folder
    };
    Ok(FilesystemType { r#type: kind })
}
struct Limits {
    deadline: Instant,
    entries: usize,
    bytes: usize,
    max_entries: usize,
    max_bytes: usize,
    max_depth: usize,
}
impl Limits {
    fn new(deadline: Instant) -> Self {
        Self {
            deadline,
            entries: 0,
            bytes: 0,
            max_entries: MAX_ENTRIES,
            max_bytes: MAX_RESPONSE_BYTES / 2,
            max_depth: MAX_DEPTH,
        }
    }
    fn visit(&mut self) -> Result<()> {
        if Instant::now() >= self.deadline {
            return Err(timeout());
        }
        self.entries += 1;
        if self.entries > self.max_entries {
            return Err(limit("filesystem_entry_limit"));
        }
        Ok(())
    }
    fn charge(&mut self, strings: &[&str]) -> Result<()> {
        self.bytes += strings.iter().map(|s| s.len() * 6).sum::<usize>() + 256;
        if self.bytes > self.max_bytes {
            return Err(limit("filesystem_response_limit"));
        }
        Ok(())
    }
}
fn name(entry: &fs::DirEntry) -> Result<&str> {
    entry
        .file_name()
        .to_str()
        .map_err(|_| Error(StatusCode::SERVICE_UNAVAILABLE, "filesystem_non_utf8_name"))
}
fn identity(stat: &Stat) -> (u64, u64) {
    (stat.st_dev, stat.st_ino)
}
fn verify_root(p: &Path, stat: &Stat) -> Result<()> {
    let fd = filesystem_directory(p).map_err(io)?;
    if identity(&fs::fstat(&fd).map_err(io)?) != identity(stat) {
        return Err(Error(StatusCode::CONFLICT, "filesystem_changed"));
    }
    Ok(())
}
fn special(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "boot"
            | "bootmgr"
            | "cache"
            | "msocache"
            | "recovery"
            | "$recycle.bin"
            | "recycler"
            | "system volume information"
            | "temporary internet files"
            | "windows"
            | ".fseventd"
            | ".spotlight"
            | ".trashes"
            | ".vol"
            | "cachedmessages"
            | "caches"
            | "trash"
            | ".@__thumb"
            | "@eadir"
            | "#recycle"
    )
}
fn extension(name: &str) -> String {
    name.rsplit_once('.')
        .filter(|(_, extension)| !extension.is_empty())
        .map(|(_, extension)| format!(".{extension}"))
        .unwrap_or_default()
}

fn lookup(
    p: PathBuf,
    trailing: bool,
    include_files: bool,
    allow: bool,
    deadline: Instant,
) -> Result<FilesystemContents> {
    if Instant::now() >= deadline {
        return Err(timeout());
    }
    let selected = if trailing || p == Path::new("/") {
        p
    } else if allow {
        match filesystem_directory(&p) {
            Ok(_) => p,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                p.parent().ok_or_else(bad)?.to_owned()
            }
            Err(e) => return Err(io(e)),
        }
    } else {
        p.parent().ok_or_else(bad)?.to_owned()
    };
    let fd = filesystem_directory(&selected).map_err(io)?;
    let root_stat = fs::fstat(&fd).map_err(io)?;
    let mut budget = Limits::new(deadline);
    let mut directories = Vec::new();
    let mut files = Vec::new();
    for entry in fs::Dir::read_from(&fd).map_err(io)? {
        let entry = entry.map_err(io)?;
        let n = name(&entry)?;
        if n == "." || n == ".." {
            continue;
        }
        budget.visit()?;
        let stat = fs::statat(&fd, entry.file_name(), AtFlags::SYMLINK_NOFOLLOW).map_err(io)?;
        let kind = match FileType::from_raw_mode(stat.st_mode) {
            FileType::Directory if !special(n) => FilesystemKind::Folder,
            FileType::RegularFile if include_files => FilesystemKind::File,
            _ => continue,
        };
        let full = selected.join(n);
        let full = if kind == FilesystemKind::Folder {
            directory_path(&full)
        } else {
            full.to_str().ok_or_else(bad)?.into()
        };
        output_path_limit(&full)?;
        budget.charge(&[n, &full])?;
        let size = if kind == FilesystemKind::File {
            Some(u64::try_from(stat.st_size).map_err(|_| unavailable())?)
                .filter(|v| *v <= 9007199254740991)
        } else {
            None
        };
        let modified = chrono::DateTime::from_timestamp(
            stat.st_mtime as i64,
            u32::try_from(stat.st_mtime_nsec).map_err(|_| unavailable())?,
        )
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true));
        let item = FilesystemEntry {
            r#type: kind,
            name: n.into(),
            path: full,
            extension: if kind == FilesystemKind::File {
                Some(extension(n))
            } else {
                None
            },
            size,
            last_modified: modified,
        };
        if kind == FilesystemKind::Folder {
            directories.push(item)
        } else {
            files.push(item)
        }
    }
    directories.sort_by(|a, b| a.name.cmp(&b.name));
    files.sort_by(|a, b| a.name.cmp(&b.name));
    verify_root(&selected, &root_stat)?;
    Ok(FilesystemContents {
        path: directory_path(&selected),
        parent: parent(&selected),
        directories,
        files,
    })
}
fn recognized(name: &str, domain: MediaDomain) -> bool {
    let ext = extension(name).to_ascii_lowercase();
    matches!(
        ext.as_str(),
        ".3gp"
            | ".asf"
            | ".asx"
            | ".avc"
            | ".avi"
            | ".bin"
            | ".bivx"
            | ".dat"
            | ".divx"
            | ".dv"
            | ".dvr-ms"
            | ".fli"
            | ".flv"
            | ".ifo"
            | ".img"
            | ".iso"
            | ".m2ts"
            | ".m2v"
            | ".m3u"
            | ".m4v"
            | ".mkv"
            | ".mov"
            | ".mp4"
            | ".mpeg"
            | ".mpg"
            | ".nrg"
            | ".nsv"
            | ".nuv"
            | ".ogm"
            | ".ogv"
            | ".pva"
            | ".qt"
            | ".rm"
            | ".rmvb"
            | ".strm"
            | ".svq3"
            | ".ts"
            | ".ty"
            | ".viv"
            | ".vob"
            | ".vp3"
            | ".webm"
            | ".wmv"
            | ".wpl"
            | ".wtv"
            | ".xvid"
    ) || (domain == MediaDomain::Movies && ext == ".mk3d")
}
fn scan_media(p: PathBuf, domain: MediaDomain, mut budget: Limits) -> Result<FilesystemMediaFiles> {
    if Instant::now() >= budget.deadline {
        return Err(timeout());
    }
    let fd = filesystem_directory(&p).map_err(io)?;
    let root_stat = fs::fstat(&fd).map_err(io)?;
    let mut result = Vec::new();
    let mut ancestors = vec![identity(&root_stat)];
    walk(
        &fd,
        &p,
        Path::new(""),
        domain,
        &mut budget,
        &mut ancestors,
        &mut result,
    )?;
    verify_root(&p, &root_stat)?;
    result.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    Ok(FilesystemMediaFiles {
        path: p.to_str().ok_or_else(bad)?.into(),
        media_type: domain,
        files: result,
    })
}
fn walk(
    fd: &File,
    base: &Path,
    relative: &Path,
    domain: MediaDomain,
    budget: &mut Limits,
    ancestors: &mut Vec<(u64, u64)>,
    result: &mut Vec<FilesystemMediaFile>,
) -> Result<()> {
    for entry in fs::Dir::read_from(fd).map_err(io)? {
        let entry = entry.map_err(io)?;
        let n = name(&entry)?;
        if n == "." || n == ".." {
            continue;
        }
        budget.visit()?;
        let stat = fs::statat(fd, entry.file_name(), AtFlags::SYMLINK_NOFOLLOW).map_err(io)?;
        let rel = relative.join(n);
        match FileType::from_raw_mode(stat.st_mode) {
            FileType::Directory => {
                if ancestors.len() > budget.max_depth {
                    return Err(limit("filesystem_depth_limit"));
                }
                let child: File = fs::openat(
                    fd,
                    entry.file_name(),
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(io)?
                .into();
                let actual = fs::fstat(&child).map_err(io)?;
                if identity(&actual) != identity(&stat) || ancestors.contains(&identity(&actual)) {
                    return Err(Error(StatusCode::CONFLICT, "filesystem_changed"));
                }
                ancestors.push(identity(&actual));
                walk(&child, base, &rel, domain, budget, ancestors, result)?;
                ancestors.pop();
            }
            FileType::RegularFile if recognized(n, domain) => {
                let full = base.join(&rel);
                let full = full.to_str().ok_or_else(bad)?;
                let rel = rel.to_str().ok_or_else(bad)?;
                output_path_limit(full)?;
                budget.charge(&[full, rel, n])?;
                result.push(FilesystemMediaFile {
                    path: full.into(),
                    relative_path: rel.into(),
                    name: n.into(),
                });
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "hrrdarr-filesystem-bounds-{}",
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    fn error<T>(value: Result<T>, code: &str) {
        match value {
            Err(e) => assert_eq!(e.1, code),
            Ok(_) => panic!("expected {code}"),
        }
    }
    #[test]
    fn selection_and_scan_bounds_are_explicit_errors() {
        // Default root selection is checked without enumerating the host root.
        assert_eq!(lookup_path(None).unwrap(), (PathBuf::from("/"), true));
        assert_eq!(
            lookup_path(Some(" \t ")).unwrap(),
            (PathBuf::from("/"), true)
        );
        assert_eq!(parent(Path::new("/")), None);
        error(
            lookup_path(Some(&" ".repeat(MAX_PATH_BYTES + 1))),
            "invalid_filesystem_query",
        );
        error(path("//"), "invalid_filesystem_query");
        assert!(recognized(".MKV", MediaDomain::Tv));
        error(
            path(&format!("/{}", "a".repeat(MAX_PATH_BYTES))),
            "invalid_filesystem_query",
        );
        error(
            output_path_limit(&"a".repeat(MAX_PATH_BYTES + 1)),
            "filesystem_path_limit",
        );
        error(
            bounded("a".repeat(MAX_RESPONSE_BYTES)),
            "filesystem_response_limit",
        );
        let s = Scratch::new();
        std::fs::create_dir(s.0.join("nested")).unwrap();
        std::fs::write(s.0.join("nested/file.MP4"), b"video").unwrap();
        std::fs::write(s.0.join("ignored.txt"), b"text").unwrap();
        let mut limits = Limits::new(Instant::now() + BUDGET);
        limits.max_entries = 1;
        error(
            scan_media(s.0.clone(), MediaDomain::Tv, limits),
            "filesystem_entry_limit",
        );
        let mut limits = Limits::new(Instant::now() + BUDGET);
        limits.max_depth = 0;
        error(
            scan_media(s.0.clone(), MediaDomain::Tv, limits),
            "filesystem_depth_limit",
        );
        let mut limits = Limits::new(Instant::now() + BUDGET);
        limits.max_bytes = 1;
        error(
            scan_media(s.0.clone(), MediaDomain::Movies, limits),
            "filesystem_response_limit",
        );
        error(
            scan_media(s.0.clone(), MediaDomain::Tv, Limits::new(Instant::now())),
            "filesystem_timeout",
        );
        error(
            lookup(s.0.clone(), true, true, false, Instant::now()),
            "filesystem_timeout",
        );
        // Changed root identity is detected after an otherwise successful observation.
        let before = fs::fstat(filesystem_directory(&s.0).unwrap()).unwrap();
        let moved = s.0.with_extension("moved");
        std::fs::rename(&s.0, &moved).unwrap();
        std::fs::create_dir(&s.0).unwrap();
        error(verify_root(&s.0, &before), "filesystem_changed");
        std::fs::remove_dir_all(moved).unwrap();
    }
    #[tokio::test]
    async fn cancelled_and_timed_out_workers_hold_shared_slots_until_they_finish() {
        let _test_guard = crate::root_folders::OBSERVER_TEST_LOCK.lock().await;
        let other = OBSERVERS.try_acquire().unwrap();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let waiter = tokio::spawn(worker(move |_| {
            ready_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(())
        }));
        ready_rx.await.unwrap();
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        error(worker(|_| Ok(())).await, "filesystem_busy");
        release_tx.send(()).unwrap();
        let returned = tokio::time::timeout(Duration::from_secs(1), OBSERVERS.acquire())
            .await
            .unwrap()
            .unwrap();
        drop(returned);
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let waiter = tokio::spawn(worker_budget(Duration::from_millis(100), move |_| {
            ready_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(())
        }));
        ready_rx.await.unwrap();
        error(waiter.await.unwrap(), "filesystem_timeout");
        error(worker(|_| Ok(())).await, "filesystem_busy");
        release_tx.send(()).unwrap();
        let returned = tokio::time::timeout(Duration::from_secs(1), OBSERVERS.acquire())
            .await
            .unwrap()
            .unwrap();
        drop(returned);
        drop(other);
    }
}
