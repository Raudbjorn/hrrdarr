//! Blocking, descriptor-relative recycle-bin primitives. Every traversed directory component and
//! every leaf is opened without following symlinks; nothing here removes a directory that is not
//! already empty, and nothing removes a path that has not been re-identified first.
use ring::digest::{Context, SHA256};
use rustix::fs::{self, AtFlags, Dir, FileType, Mode, OFlags, RenameFlags};
use std::{
    fs::File,
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    path::{Component, Path},
    time::{Duration, Instant, SystemTime},
};

/// Same bounds the import transfer uses for one media file.
pub(crate) const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024 * 1024;
pub(crate) const TRANSFER_LIMIT: Duration = Duration::from_secs(30 * 60);
/// Upstream appends `_2`, `_3`, ... until a name is free; bound the probe count.
const MAX_NAME_ATTEMPTS: u32 = 1000;
/// Root-folder failsafe: at most this many entries are read looking for a subdirectory.
const MAX_ROOT_ENTRIES: usize = 10_000;
const COPY_BUFFER_BYTES: usize = 1024 * 1024;
const PROBE_PREFIX: &str = ".hrrdarr-write-probe-";
pub(crate) const PARTIAL_PREFIX: &str = ".hrrdarr-recycle-";
pub(crate) const PARTIAL_SUFFIX: &str = ".partial";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FsError {
    Missing,
    Denied,
    NotRegular,
    Unsafe,
    Changed,
    Exists,
    Limit,
    Io,
}
impl FsError {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Missing => "path_missing",
            Self::Denied => "permission_denied",
            Self::NotRegular => "file_not_regular",
            Self::Unsafe => "path_unsafe",
            Self::Changed => "file_changed",
            Self::Exists => "recycle_destination_exists",
            Self::Limit => "transfer_limit",
            Self::Io => "filesystem_error",
        }
    }
}
fn errno(e: rustix::io::Errno) -> FsError {
    match e {
        rustix::io::Errno::NOENT => FsError::Missing,
        rustix::io::Errno::ACCESS | rustix::io::Errno::PERM | rustix::io::Errno::ROFS => {
            FsError::Denied
        }
        rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR => FsError::Unsafe,
        rustix::io::Errno::EXIST | rustix::io::Errno::NOTEMPTY => FsError::Exists,
        _ => FsError::Io,
    }
}
fn ioerr(e: std::io::Error) -> FsError {
    match e.raw_os_error() {
        Some(code) => errno(rustix::io::Errno::from_raw_os_error(code)),
        None => match e.kind() {
            std::io::ErrorKind::NotFound => FsError::Missing,
            std::io::ErrorKind::PermissionDenied => FsError::Denied,
            _ => FsError::Io,
        },
    }
}
pub(crate) type Result<T> = std::result::Result<T, FsError>;

/// Device/inode plus the content facts that change when bytes are rewritten in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Identity {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime: i64,
    pub mtime_ns: i64,
}
impl Identity {
    fn of(stat: &fs::Stat) -> Self {
        Self {
            dev: stat.st_dev as u64,
            ino: stat.st_ino as u64,
            size: stat.st_size as u64,
            mtime: stat.st_mtime as i64,
            mtime_ns: stat.st_mtime_nsec as i64,
        }
    }
    fn file(f: &File) -> Result<Self> {
        let m = f.metadata().map_err(ioerr)?;
        Ok(Self {
            dev: m.dev(),
            ino: m.ino(),
            size: m.len(),
            mtime: m.mtime(),
            mtime_ns: m.mtime_nsec(),
        })
    }
    pub(crate) fn same_inode(&self, dev: u64, ino: u64) -> bool {
        self.dev == dev && self.ino == ino
    }
    fn same_content(&self, other: &Self) -> bool {
        self.dev == other.dev
            && self.ino == other.ino
            && self.size == other.size
            && self.mtime == other.mtime
            && self.mtime_ns == other.mtime_ns
    }
}

/// Splits a stored absolute path into its directory components and leaf, rejecting `.`/`..`,
/// empty or non-UTF-8 components. Repeated separators are tolerated as the OS does.
pub(crate) fn split(path: &str) -> Result<(String, String)> {
    let p = Path::new(path);
    // Path::components silently drops interior `.`; inspect the raw separators instead.
    if !p.is_absolute()
        || path.len() > 4096
        || path.contains('\0')
        || path.split('/').any(|s| matches!(s, "." | ".."))
    {
        return Err(FsError::Unsafe);
    }
    if p.components()
        .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
    {
        return Err(FsError::Unsafe);
    }
    let name = p
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or(FsError::Unsafe)?;
    let parent = p.parent().and_then(|p| p.to_str()).ok_or(FsError::Unsafe)?;
    Ok((parent.to_owned(), name.to_owned()))
}

pub(crate) fn open_dir(path: &str) -> Result<File> {
    crate::import::filesystem_directory(Path::new(path)).map_err(ioerr)
}
fn open_child_dir(dir: &File, name: &str) -> std::result::Result<File, rustix::io::Errno> {
    Ok(fs::openat(
        dir,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?
    .into())
}
fn stat_child(dir: &File, name: &str) -> std::result::Result<fs::Stat, rustix::io::Errno> {
    fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW)
}

/// Upstream failsafe: refuse deletion when the library root is missing or has no subdirectory
/// (an unmounted share often looks like an empty directory).
pub(crate) fn root_available(root: &str) -> Result<bool> {
    let dir = open_dir(root)?;
    let entries = Dir::read_from(&dir).map_err(errno)?;
    for (seen, item) in entries.enumerate() {
        if seen >= MAX_ROOT_ENTRIES {
            break;
        }
        let item = item.map_err(errno)?;
        let Ok(name) = item.file_name().to_str() else {
            continue;
        };
        if matches!(name, "." | "..") {
            continue;
        }
        if let Ok(stat) = stat_child(&dir, name) {
            if FileType::from_raw_mode(stat.st_mode) == FileType::Directory {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

pub(crate) struct Present {
    pub dir: File,
    pub name: String,
    pub identity: Identity,
}
pub(crate) enum Source {
    Absent,
    Present(Present),
}
/// Inspects the stored file path. A missing parent or leaf is `Absent` (upstream removes the
/// record anyway); a symlink or special file is never treated as the media file.
pub(crate) fn inspect(path: &str) -> Result<Source> {
    let (parent, name) = split(path)?;
    let dir = match open_dir(&parent) {
        Ok(dir) => dir,
        Err(FsError::Missing) => return Ok(Source::Absent),
        Err(e) => return Err(e),
    };
    match stat_child(&dir, &name) {
        Err(rustix::io::Errno::NOENT) => Ok(Source::Absent),
        Err(e) => Err(errno(e)),
        Ok(stat) if FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile => {
            Ok(Source::Present(Present {
                dir,
                name,
                identity: Identity::of(&stat),
            }))
        }
        Ok(_) => Err(FsError::NotRegular),
    }
}
/// Current identity at the recorded leaf, or None when absent.
pub(crate) fn observe(path: &str) -> Result<Option<Identity>> {
    match inspect(path)? {
        Source::Absent => Ok(None),
        Source::Present(p) => Ok(Some(p.identity)),
    }
}

pub(crate) struct Destination {
    pub dir: File,
    pub name: String,
    pub path: String,
}
/// Creates `bin/<subfolder...>` without following symlinks and selects a free leaf name using
/// the upstream `name_N.ext` convention. The bin itself must already exist.
pub(crate) fn prepare_destination(
    bin: &str,
    subfolder: &[String],
    leaf: &str,
) -> Result<Destination> {
    let mut dir = open_dir(bin)?;
    let mut path = bin.to_owned();
    for component in subfolder {
        if component.is_empty()
            || matches!(component.as_str(), "." | "..")
            || component.contains('/')
        {
            return Err(FsError::Unsafe);
        }
        match fs::mkdirat(&dir, component.as_str(), Mode::from_bits_truncate(0o755)) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
            Err(e) => return Err(errno(e)),
        }
        dir = open_child_dir(&dir, component).map_err(errno)?;
        path.push('/');
        path.push_str(component);
    }
    let (stem, extension) = match leaf.rfind('.') {
        Some(index) if index > 0 => (&leaf[..index], &leaf[index..]),
        _ => (leaf, ""),
    };
    for attempt in 1..=MAX_NAME_ATTEMPTS {
        let name = if attempt == 1 {
            leaf.to_owned()
        } else {
            format!("{stem}_{attempt}{extension}")
        };
        if name.len() > 255 {
            return Err(FsError::Unsafe);
        }
        match stat_child(&dir, &name) {
            Err(rustix::io::Errno::NOENT) => {
                let full = format!("{path}/{name}");
                if full.len() > 4096 {
                    return Err(FsError::Unsafe);
                }
                return Ok(Destination {
                    dir,
                    name,
                    path: full,
                });
            }
            Ok(_) => continue,
            Err(e) => return Err(errno(e)),
        }
    }
    Err(FsError::Exists)
}

fn verify_source(source: &Present) -> Result<File> {
    let file: File = fs::openat(
        &source.dir,
        source.name.as_str(),
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(errno)?
    .into();
    let identity = Identity::file(&file)?;
    if !file.metadata().map_err(ioerr)?.is_file() {
        return Err(FsError::NotRegular);
    }
    if !identity.same_content(&source.identity) {
        return Err(FsError::Changed);
    }
    Ok(file)
}
fn touch(file: &File) {
    // Retention ages from deletion time, as upstream does; failures are tolerated there too.
    if let Err(error) = file.set_modified(SystemTime::now()) {
        eprintln!(
            "event=recycle_mtime_failed kind={:?} os_code={:?}",
            error.kind(),
            error.raw_os_error()
        );
    }
}
fn touch_child(dir: &File, name: &str) {
    if let Ok(fd) = fs::openat(
        dir,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    ) {
        touch(&File::from(fd));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Moved {
    Rename,
    Link,
    /// Destination identity of the verified, published copy; the source is still present.
    Copy {
        dev: u64,
        ino: u64,
    },
}
/// Moves the verified source into the bin. Same-filesystem moves use one no-replace rename (or a
/// no-replace link followed by unlinking the source when the filesystem lacks RENAME_NOREPLACE).
/// Across filesystems the bytes are copied to a private partial file, fsynced, re-read and hashed
/// against the source hash, then published without replacement; the source is left in place for
/// removal after the database commit.
pub(crate) fn move_into_bin(
    source: &Present,
    destination: &Destination,
    deletion_id: &str,
) -> Result<Moved> {
    let file = verify_source(source)?;
    drop(file);
    match fs::renameat_with(
        &source.dir,
        source.name.as_str(),
        &destination.dir,
        destination.name.as_str(),
        RenameFlags::NOREPLACE,
    ) {
        Ok(()) => {
            // The rename happened; a later sync failure must not be reported as "not moved".
            if let Err(error) = sync_pair(&source.dir, &destination.dir) {
                eprintln!("event=recycle_sync_failed code={}", error.code());
            }
            touch_child(&destination.dir, &destination.name);
            return Ok(Moved::Rename);
        }
        Err(rustix::io::Errno::XDEV) => {}
        Err(rustix::io::Errno::INVAL | rustix::io::Errno::NOSYS) => {
            fs::linkat(
                &source.dir,
                source.name.as_str(),
                &destination.dir,
                destination.name.as_str(),
                AtFlags::empty(),
            )
            .map_err(|e| match e {
                rustix::io::Errno::XDEV => FsError::Io,
                other => errno(other),
            })?;
            // Both names now refer to one inode; recovery recognises this state by inode.
            destination.dir.sync_all().map_err(ioerr)?;
            remove_verified(&source.dir, &source.name, &source.identity)?;
            if let Err(error) = source.dir.sync_all() {
                eprintln!("event=recycle_sync_failed code={}", ioerr(error).code());
            }
            touch_child(&destination.dir, &destination.name);
            return Ok(Moved::Link);
        }
        Err(e) => return Err(errno(e)),
    }
    copy_into_bin(source, destination, deletion_id)
}
fn sync_pair(a: &File, b: &File) -> Result<()> {
    a.sync_all().map_err(ioerr)?;
    b.sync_all().map_err(ioerr)
}
pub(crate) fn partial_name(deletion_id: &str) -> String {
    format!("{PARTIAL_PREFIX}{deletion_id}{PARTIAL_SUFFIX}")
}
fn copy_into_bin(source: &Present, destination: &Destination, deletion_id: &str) -> Result<Moved> {
    let deadline = Instant::now() + TRANSFER_LIMIT;
    if source.identity.size > MAX_FILE_BYTES {
        return Err(FsError::Limit);
    }
    let mut input = verify_source(source)?;
    let partial = partial_name(deletion_id);
    let mut output: File = fs::openat(
        &destination.dir,
        partial.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_bits_truncate(0o600),
    )
    .map_err(errno)?
    .into();
    let result = (|| {
        let mut buffer = vec![0u8; COPY_BUFFER_BYTES];
        let mut hash = Context::new(&SHA256);
        let mut copied: u64 = 0;
        loop {
            if Instant::now() >= deadline {
                return Err(FsError::Limit);
            }
            let read = input.read(&mut buffer).map_err(ioerr)?;
            if read == 0 {
                break;
            }
            copied += read as u64;
            if copied > source.identity.size || copied > MAX_FILE_BYTES {
                return Err(FsError::Changed);
            }
            hash.update(&buffer[..read]);
            output.write_all(&buffer[..read]).map_err(ioerr)?;
        }
        if copied != source.identity.size {
            return Err(FsError::Changed);
        }
        let mode = input.metadata().map_err(ioerr)?.mode() & 0o777;
        fs::fchmod(&output, Mode::from_bits_truncate(mode)).map_err(errno)?;
        output.sync_all().map_err(ioerr)?;
        // The source must not have changed during the copy.
        if !Identity::file(&input)?.same_content(&source.identity) {
            return Err(FsError::Changed);
        }
        // Re-read what reached the bin's filesystem rather than trusting the write path.
        let mut check: File = fs::openat(
            &destination.dir,
            partial.as_str(),
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(errno)?
        .into();
        let mut verify = Context::new(&SHA256);
        let mut seen: u64 = 0;
        loop {
            if Instant::now() >= deadline {
                return Err(FsError::Limit);
            }
            let read = check.read(&mut buffer).map_err(ioerr)?;
            if read == 0 {
                break;
            }
            seen += read as u64;
            verify.update(&buffer[..read]);
        }
        if seen != copied || verify.finish().as_ref() != hash.finish().as_ref() {
            return Err(FsError::Changed);
        }
        touch(&output);
        let published = Identity::file(&output)?;
        match fs::renameat_with(
            &destination.dir,
            partial.as_str(),
            &destination.dir,
            destination.name.as_str(),
            RenameFlags::NOREPLACE,
        ) {
            Ok(()) => {}
            Err(rustix::io::Errno::INVAL | rustix::io::Errno::NOSYS) => {
                fs::linkat(
                    &destination.dir,
                    partial.as_str(),
                    &destination.dir,
                    destination.name.as_str(),
                    AtFlags::empty(),
                )
                .map_err(errno)?;
                fs::unlinkat(&destination.dir, partial.as_str(), AtFlags::empty())
                    .map_err(errno)?;
            }
            Err(e) => return Err(errno(e)),
        }
        destination.dir.sync_all().map_err(ioerr)?;
        Ok(Moved::Copy {
            dev: published.dev,
            ino: published.ino,
        })
    })();
    if result.is_err() {
        // The private partial name is unique to this deletion and was created exclusively here.
        let _ = fs::unlinkat(&destination.dir, partial.as_str(), AtFlags::empty());
    }
    result
}

/// Removes `dir/name` only when it is still the identified regular file.
pub(crate) fn remove_verified(dir: &File, name: &str, identity: &Identity) -> Result<()> {
    let stat = stat_child(dir, name).map_err(errno)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
        || !Identity::of(&stat).same_content(identity)
    {
        return Err(FsError::Changed);
    }
    fs::unlinkat(dir, name, AtFlags::empty()).map_err(errno)
}
pub(crate) fn unlink_source(source: &Present) -> Result<()> {
    remove_verified(&source.dir, &source.name, &source.identity)?;
    source.dir.sync_all().map_err(ioerr)
}
/// Removes the original after a committed cross-device copy, if it is still the same file.
pub(crate) fn remove_original(path: &str, identity: &Identity) -> Result<bool> {
    match inspect(path)? {
        Source::Absent => Ok(false),
        Source::Present(present) => {
            if !present.identity.same_content(identity) {
                return Err(FsError::Changed);
            }
            unlink_source(&present)?;
            Ok(true)
        }
    }
}
/// Removes the source name of a linked (same-inode) recycle whose unlink was interrupted.
pub(crate) fn remove_linked_original(path: &str, dev: u64, ino: u64) -> Result<bool> {
    match inspect(path)? {
        Source::Absent => Ok(false),
        Source::Present(present) => {
            if !present.identity.same_inode(dev, ino) {
                return Err(FsError::Changed);
            }
            fs::unlinkat(&present.dir, present.name.as_str(), AtFlags::empty()).map_err(errno)?;
            if let Err(error) = present.dir.sync_all() {
                eprintln!("event=recycle_sync_failed code={}", ioerr(error).code());
            }
            Ok(true)
        }
    }
}
/// Removes this deletion's own private partial file, if one was left by an interrupted copy.
pub(crate) fn remove_partial(destination: &str, deletion_id: &str) -> Result<()> {
    let (parent, _) = split(destination)?;
    let dir = match open_dir(&parent) {
        Ok(dir) => dir,
        Err(FsError::Missing) => return Ok(()),
        Err(e) => return Err(e),
    };
    let name = partial_name(deletion_id);
    match stat_child(&dir, &name) {
        Err(rustix::io::Errno::NOENT) => Ok(()),
        Err(e) => Err(errno(e)),
        Ok(stat) if FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile => {
            fs::unlinkat(&dir, name.as_str(), AtFlags::empty()).map_err(errno)
        }
        Ok(_) => Err(FsError::NotRegular),
    }
}

/// hc.036 probe: the configured bin exists as a real directory and a private file can be
/// created and removed in it.
pub(crate) fn writable(bin: &str) -> Result<()> {
    let dir = open_dir(bin)?;
    let name = format!("{PROBE_PREFIX}{}", uuid::Uuid::new_v4());
    let probe: File = fs::openat(
        &dir,
        name.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_bits_truncate(0o600),
    )
    .map_err(errno)?
    .into();
    drop(probe);
    fs::unlinkat(&dir, name.as_str(), AtFlags::empty()).map_err(errno)
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CleanupReport {
    pub files_removed: u64,
    pub folders_removed: u64,
    pub skipped: u64,
    pub failed: u64,
    pub bounded: bool,
}
pub(crate) struct CleanupLimits {
    pub max_depth: usize,
    pub max_entries: usize,
    pub max_removals: u64,
    pub deadline: Instant,
}
/// Removes regular files whose modification time is at least `days` old anywhere inside `bin`,
/// then empty subdirectories. Symlinks, special files and other mounts are skipped and never
/// followed; the bin directory itself is never removed. Bounded by depth, entries, removals and
/// wall time; whatever is not visited is kept.
pub(crate) fn cleanup(
    bin: &str,
    days: i64,
    now: SystemTime,
    limits: &CleanupLimits,
) -> Result<CleanupReport> {
    let dir = open_dir(bin)?;
    let root = Identity::file(&dir)?;
    let age = Duration::from_secs(u64::try_from(days).map_err(|_| FsError::Io)? * 86_400);
    let cutoff = now.checked_sub(age).ok_or(FsError::Io)?;
    let cutoff = cutoff
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| (d.as_secs() as i64, d.subsec_nanos() as i64))
        .unwrap_or((0, 0));
    let mut walk = Cleanup {
        root_dev: root.dev,
        cutoff,
        limits,
        visited: 0,
        report: CleanupReport::default(),
    };
    walk.visit(&dir, 0)?;
    Ok(walk.report)
}
struct Cleanup<'a> {
    root_dev: u64,
    cutoff: (i64, i64),
    limits: &'a CleanupLimits,
    visited: usize,
    report: CleanupReport,
}
impl Cleanup<'_> {
    fn stop(&mut self) -> bool {
        if self.visited >= self.limits.max_entries
            || self.report.files_removed + self.report.folders_removed >= self.limits.max_removals
            || Instant::now() >= self.limits.deadline
        {
            self.report.bounded = true;
            return true;
        }
        false
    }
    /// Returns true when `dir` is empty after processing.
    fn visit(&mut self, dir: &File, depth: usize) -> Result<bool> {
        let mut names = Vec::new();
        let entries = Dir::read_from(dir).map_err(errno)?;
        let mut empty = true;
        for item in entries {
            let item = item.map_err(errno)?;
            let Ok(name) = item.file_name().to_str() else {
                // Non-UTF-8 names are kept untouched.
                self.report.skipped += 1;
                empty = false;
                continue;
            };
            if matches!(name, "." | "..") {
                continue;
            }
            self.visited += 1;
            if self.stop() {
                return Ok(false);
            }
            names.push(name.to_owned());
        }
        for name in names {
            if self.stop() {
                return Ok(false);
            }
            let stat = match stat_child(dir, &name) {
                Ok(stat) => stat,
                Err(rustix::io::Errno::NOENT) => continue,
                Err(_) => {
                    self.report.failed += 1;
                    empty = false;
                    continue;
                }
            };
            if stat.st_dev as u64 != self.root_dev {
                // A different mount inside the bin is never entered or removed.
                self.report.skipped += 1;
                empty = false;
                continue;
            }
            match FileType::from_raw_mode(stat.st_mode) {
                FileType::RegularFile => {
                    let mtime = (stat.st_mtime as i64, stat.st_mtime_nsec as i64);
                    if mtime > self.cutoff || name.starts_with(PARTIAL_PREFIX) {
                        // Not yet expired, or an in-flight transfer's private partial file.
                        empty = false;
                        continue;
                    }
                    match remove_verified(dir, &name, &Identity::of(&stat)) {
                        Ok(()) => self.report.files_removed += 1,
                        Err(FsError::Missing) => {}
                        Err(_) => {
                            // Radarr continues past per-file permission failures; so do we.
                            self.report.failed += 1;
                            empty = false;
                        }
                    }
                }
                FileType::Directory => {
                    if depth + 1 >= self.limits.max_depth {
                        self.report.skipped += 1;
                        empty = false;
                        continue;
                    }
                    let nested = match open_child_dir(dir, &name) {
                        Ok(nested) => nested,
                        Err(rustix::io::Errno::NOENT) => continue,
                        Err(_) => {
                            self.report.failed += 1;
                            empty = false;
                            continue;
                        }
                    };
                    let opened = Identity::file(&nested)?;
                    if !opened.same_inode(stat.st_dev as u64, stat.st_ino as u64) {
                        self.report.skipped += 1;
                        empty = false;
                        continue;
                    }
                    if !self.visit(&nested, depth + 1)? {
                        empty = false;
                        continue;
                    }
                    drop(nested);
                    match fs::unlinkat(dir, name.as_str(), AtFlags::REMOVEDIR) {
                        Ok(()) => self.report.folders_removed += 1,
                        Err(rustix::io::Errno::NOENT) => {}
                        Err(_) => {
                            // Something appeared meanwhile or removal was refused: kept.
                            empty = false;
                        }
                    }
                }
                _ => {
                    // Symlinks, sockets, devices and FIFOs are kept and never followed.
                    self.report.skipped += 1;
                    empty = false;
                }
            }
        }
        Ok(empty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn scratch() -> Scratch {
        let path =
            std::env::temp_dir().join(format!("hrrdarr-recycle-fs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Scratch(path)
    }
    fn age(path: &Path, days: u64) {
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(SystemTime::now() - Duration::from_secs(days * 86_400 + 60))
            .unwrap();
    }
    fn limits() -> CleanupLimits {
        CleanupLimits {
            max_depth: 32,
            max_entries: 10_000,
            max_removals: 10_000,
            deadline: Instant::now() + Duration::from_secs(30),
        }
    }
    #[test]
    fn split_rejects_traversal_and_relative_paths() {
        assert_eq!(
            split("/a/b/c.mkv").unwrap(),
            ("/a/b".to_owned(), "c.mkv".to_owned())
        );
        for bad in ["relative/c.mkv", "/a/../c.mkv", "/a/./c.mkv", "/", "/a\0b"] {
            assert!(split(bad).is_err(), "{bad}");
        }
    }
    #[test]
    fn destination_names_follow_suffix_convention_and_never_replace() {
        let s = scratch();
        let bin = s.0.join("bin");
        std::fs::create_dir(&bin).unwrap();
        let bin = bin.to_str().unwrap();
        let sub = vec!["Show".to_owned(), "Season 1".to_owned()];
        let first = prepare_destination(bin, &sub, "ep.mkv").unwrap();
        assert_eq!(first.path, format!("{bin}/Show/Season 1/ep.mkv"));
        std::fs::write(&first.path, b"occupied").unwrap();
        let second = prepare_destination(bin, &sub, "ep.mkv").unwrap();
        assert_eq!(second.path, format!("{bin}/Show/Season 1/ep_2.mkv"));
        std::fs::write(format!("{bin}/Show/noext"), b"x").unwrap();
        let bare = prepare_destination(bin, &["Show".to_owned()], "noext").unwrap();
        assert_eq!(bare.path, format!("{bin}/Show/noext_2"));
        assert!(prepare_destination(bin, &["..".to_owned()], "x").is_err());
        // A symlinked subfolder inside the bin is never traversed.
        std::os::unix::fs::symlink(&s.0, format!("{bin}/Linked")).unwrap();
        assert_eq!(
            prepare_destination(bin, &["Linked".to_owned()], "x").err(),
            Some(FsError::Unsafe)
        );
        assert_eq!(std::fs::read(&first.path).unwrap(), b"occupied");
    }
    #[test]
    fn inspect_refuses_symlink_leaf_and_reports_absent_files() {
        let s = scratch();
        let real = s.0.join("real.mkv");
        std::fs::write(&real, b"bytes").unwrap();
        let link = s.0.join("link.mkv");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(matches!(
            inspect(real.to_str().unwrap()).unwrap(),
            Source::Present(_)
        ));
        assert!(matches!(
            inspect(link.to_str().unwrap()).err(),
            Some(FsError::NotRegular)
        ));
        assert!(matches!(
            inspect(s.0.join("missing/x.mkv").to_str().unwrap()).unwrap(),
            Source::Absent
        ));
    }
    #[test]
    fn same_device_move_preserves_bytes_and_refuses_changed_sources() {
        let s = scratch();
        let library = s.0.join("library");
        let bin = s.0.join("bin");
        std::fs::create_dir(&library).unwrap();
        std::fs::create_dir(&bin).unwrap();
        let file = library.join("movie.mkv");
        std::fs::write(&file, b"movie bytes").unwrap();
        let Source::Present(source) = inspect(file.to_str().unwrap()).unwrap() else {
            panic!("present")
        };
        let destination =
            prepare_destination(bin.to_str().unwrap(), &["library".to_owned()], "movie.mkv")
                .unwrap();
        std::fs::write(&file, b"rewritten bytes").unwrap();
        assert_eq!(
            move_into_bin(&source, &destination, &uuid::Uuid::new_v4().to_string()).err(),
            Some(FsError::Changed)
        );
        assert!(file.exists());
        let Source::Present(source) = inspect(file.to_str().unwrap()).unwrap() else {
            panic!("present")
        };
        let moved =
            move_into_bin(&source, &destination, &uuid::Uuid::new_v4().to_string()).unwrap();
        assert_eq!(moved, Moved::Rename);
        assert!(!file.exists());
        assert_eq!(
            std::fs::read(&destination.path).unwrap(),
            b"rewritten bytes"
        );
    }
    #[test]
    fn cleanup_removes_only_expired_regular_files_inside_the_bin() {
        let s = scratch();
        let bin = s.0.join("bin");
        let outside = s.0.join("outside");
        std::fs::create_dir_all(bin.join("Show/Season 1")).unwrap();
        std::fs::create_dir_all(bin.join("Fresh")).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let old = bin.join("Show/Season 1/old.mkv");
        let fresh = bin.join("Fresh/new.mkv");
        let outside_old = outside.join("old.mkv");
        for path in [&old, &fresh, &outside_old] {
            std::fs::write(path, b"bytes").unwrap();
        }
        age(&old, 10);
        age(&outside_old, 10);
        // A symlink to an expired file outside the bin, and a symlinked directory.
        std::os::unix::fs::symlink(&outside_old, bin.join("link.mkv")).unwrap();
        std::os::unix::fs::symlink(&outside, bin.join("linked-dir")).unwrap();
        let partial = bin.join(format!("{PARTIAL_PREFIX}x{PARTIAL_SUFFIX}"));
        std::fs::write(&partial, b"in flight").unwrap();
        age(&partial, 10);
        let report = cleanup(bin.to_str().unwrap(), 7, SystemTime::now(), &limits()).unwrap();
        assert_eq!(report.files_removed, 1);
        assert_eq!(
            report.folders_removed, 2,
            "Show/Season 1 and Show become empty"
        );
        assert_eq!(report.skipped, 2, "two symlinks kept");
        assert!(!old.exists());
        assert!(fresh.exists());
        assert!(outside_old.exists());
        assert!(partial.exists());
        assert!(bin.join("link.mkv").symlink_metadata().is_ok());
        assert!(bin.exists());
        // The bin itself is never removed, even when empty.
        std::fs::remove_dir_all(bin.join("Fresh")).unwrap();
        std::fs::remove_file(bin.join("link.mkv")).unwrap();
        std::fs::remove_file(bin.join("linked-dir")).unwrap();
        std::fs::remove_file(&partial).unwrap();
        cleanup(bin.to_str().unwrap(), 7, SystemTime::now(), &limits()).unwrap();
        assert!(bin.is_dir());
        // A symlinked bin is refused outright.
        let alias = s.0.join("alias");
        std::os::unix::fs::symlink(&bin, &alias).unwrap();
        assert!(cleanup(alias.to_str().unwrap(), 7, SystemTime::now(), &limits()).is_err());
    }
    #[test]
    fn cleanup_is_bounded_and_keeps_unvisited_entries() {
        let s = scratch();
        for i in 0..5 {
            let path = s.0.join(format!("{i}.mkv"));
            std::fs::write(&path, b"x").unwrap();
            age(&path, 30);
        }
        let mut bounded = limits();
        bounded.max_removals = 2;
        let report = cleanup(s.0.to_str().unwrap(), 1, SystemTime::now(), &bounded).unwrap();
        assert!(report.bounded);
        assert_eq!(report.files_removed, 2);
        assert_eq!(std::fs::read_dir(&s.0).unwrap().count(), 3);
    }
    #[test]
    fn writability_probe_leaves_no_artifact() {
        let s = scratch();
        writable(s.0.to_str().unwrap()).unwrap();
        assert_eq!(std::fs::read_dir(&s.0).unwrap().count(), 0);
        assert_eq!(
            writable(s.0.join("missing").to_str().unwrap()).err(),
            Some(FsError::Missing)
        );
    }
}
