//! Linux descriptor-relative transfers. No path component may be a symlink.
use super::{Error, Mode, Result};
use ring::digest::{Context, SHA256};
use rustix::fs::{self, AtFlags, Mode as Permissions, OFlags, RenameFlags};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::MetadataExt,
    path::{Component, Path},
    time::{Duration, Instant},
};

const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024 * 1024;
const TRANSFER_LIMIT: Duration = Duration::from_secs(30 * 60);
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Identity {
    dev: u64,
    ino: u64,
    size: u64,
    mtime: i64,
    mtime_ns: i64,
    ctime: i64,
    ctime_ns: i64,
}
impl Identity {
    fn read(f: &File) -> Result<Self> {
        let m = f.metadata().map_err(io)?;
        Ok(Self {
            dev: m.dev(),
            ino: m.ino(),
            size: m.len(),
            mtime: m.mtime(),
            mtime_ns: m.mtime_nsec(),
            ctime: m.ctime(),
            ctime_ns: m.ctime_nsec(),
        })
    }
    fn same(&self, other: &Self) -> bool {
        self.dev == other.dev && self.ino == other.ino
    }
    fn content(&self, other: &Self) -> bool {
        self.same(other)
            && self.size == other.size
            && self.mtime == other.mtime
            && self.mtime_ns == other.mtime_ns
    }
    fn unchanged(&self, other: &Self) -> bool {
        self.content(other) && self.ctime == other.ctime && self.ctime_ns == other.ctime_ns
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Plan {
    pub owner_id: i64,
    pub root: String,
    pub source: String,
    pub destination: String,
    pub mode: Mode,
    root_identity: Identity,
    source_parent: Identity,
    destination_parent: Identity,
    source_identity: Identity,
    stage_name: String,
    quarantine_name: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Stage {
    directory: Identity,
    file: Identity,
    pub sha256: Option<String>,
    quarantine: Option<Identity>,
    source_retired: bool,
}
impl Stage {
    pub fn size(&self) -> u64 {
        self.file.size
    }
}
fn io(error: impl Into<std::io::Error>) -> Error {
    let error = error.into();
    let mut result = Error::conflict(
        "filesystem_error",
        "File operation failed; retained files and journal require retry or inspection",
    );
    result.diagnostic = Some(format!(
        "kind={:?}, os_code={:?}",
        error.kind(),
        error.raw_os_error()
    ));
    result
}
fn changed() -> Error {
    Error::conflict(
        "identity_changed",
        "A path or file changed since preview; retained or quarantined files require inspection",
    )
}
fn unowned() -> Error {
    Error::conflict(
        "unowned_artifact",
        "An unrecorded staging artifact exists; it is retained for inspection",
    )
}
fn check(ok: bool) -> Result<()> {
    if ok { Ok(()) } else { Err(changed()) }
}
fn parts(path: &str) -> Result<(&Path, &str)> {
    let p = Path::new(path);
    Ok((
        p.parent().ok_or_else(changed)?,
        p.file_name().and_then(|s| s.to_str()).ok_or_else(changed)?,
    ))
}
pub fn validate_path(path: &str) -> Result<()> {
    let normalized: std::path::PathBuf = Path::new(path).components().collect();
    if normalized.to_str() != Some(path)
        || path.len() > 4096
        || path.contains('\0')
        || !Path::new(path).is_absolute()
        || Path::new(path)
            .components()
            .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
    {
        return Err(Error::bad(
            "Paths must be absolute, normalized, UTF-8 paths of at most 4096 bytes",
        ));
    }
    Ok(())
}
pub(super) fn directory(path: &Path) -> Result<File> {
    directory_io(path).map_err(io)
}
pub(super) fn directory_io(path: &Path) -> std::io::Result<File> {
    let mut fd: File = fs::open(
        "/",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Permissions::empty(),
    )?
    .into();
    for c in path.components() {
        match c {
            Component::RootDir => {}
            Component::Normal(name) => {
                fd = fs::openat(
                    &fd,
                    name,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Permissions::empty(),
                )?
                .into()
            }
            _ => return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput)),
        }
    }
    Ok(fd)
}
fn child(dir: &File, name: &str, flags: OFlags) -> Result<File> {
    let f: File = fs::openat(
        dir,
        name,
        flags | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Permissions::from_bits_truncate(0o600),
    )
    .map_err(io)?
    .into();
    check(f.metadata().map_err(io)?.is_file())?;
    Ok(f)
}
fn optional(dir: &File, name: &str) -> Result<Option<File>> {
    match fs::openat(
        dir,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Permissions::empty(),
    ) {
        Ok(fd) => {
            let f: File = fd.into();
            check(f.metadata().map_err(io)?.is_file())?;
            Ok(Some(f))
        }
        Err(rustix::io::Errno::NOENT) => Ok(None),
        Err(e) => Err(io(e)),
    }
}
fn sync(dir: &File) -> Result<()> {
    dir.sync_all().map_err(io)
}
impl Plan {
    pub fn preview(
        owner_id: i64,
        root: String,
        source: String,
        destination: String,
        mode: Mode,
        id: &str,
    ) -> Result<Self> {
        for path in [&root, &source, &destination] {
            validate_path(path)?;
        }
        check(
            Path::new(&destination).starts_with(&root)
                && destination != root
                && source != destination,
        )?;
        let root_fd = directory(Path::new(&root))?;
        let (sp, sn) = parts(&source)?;
        let (dp, dn) = parts(&destination)?;
        let source_parent = directory(sp)?;
        let destination_parent = directory(dp)?;
        check(optional(&destination_parent, dn)?.is_none())?;
        let source_fd = child(&source_parent, sn, OFlags::RDONLY)?;
        let source_identity = Identity::read(&source_fd)?;
        check(source_identity.size > 0 && source_identity.size <= MAX_FILE_BYTES)?;
        if mode == Mode::Hardlink {
            check(source_identity.dev == Identity::read(&destination_parent)?.dev)?;
        }
        Ok(Self {
            owner_id,
            root,
            source,
            destination,
            mode,
            root_identity: Identity::read(&root_fd)?,
            source_parent: Identity::read(&source_parent)?,
            destination_parent: Identity::read(&destination_parent)?,
            source_identity,
            stage_name: format!(".hrrdarr-import-{id}"),
            quarantine_name: format!(".hrrdarr-move-{id}"),
        })
    }
    fn parents(&self) -> Result<(File, File)> {
        check(
            self.root_identity
                .same(&Identity::read(&directory(Path::new(&self.root))?)?),
        )?;
        let s = directory(parts(&self.source)?.0)?;
        let d = directory(parts(&self.destination)?.0)?;
        check(
            self.source_parent.same(&Identity::read(&s)?)
                && self.destination_parent.same(&Identity::read(&d)?),
        )?;
        Ok((s, d))
    }
    fn stage_dir(&self, stage: &Stage, d: &File) -> Result<File> {
        let f: File = fs::openat(
            d,
            self.stage_name.as_str(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Permissions::empty(),
        )
        .map_err(io)?
        .into();
        check(stage.directory.same(&Identity::read(&f)?))?;
        Ok(f)
    }
    pub fn create_stage(&self) -> Result<Stage> {
        let (s, d) = self.parents()?;
        let source = child(&s, parts(&self.source)?.1, OFlags::RDONLY)?;
        check(self.source_identity.unchanged(&Identity::read(&source)?))?;
        fs::mkdirat(
            &d,
            self.stage_name.as_str(),
            Permissions::from_bits_truncate(0o700),
        )
        .map_err(|e| {
            if e == rustix::io::Errno::EXIST {
                unowned()
            } else {
                io(e)
            }
        })?;
        sync(&d)?;
        let dir: File = fs::openat(
            &d,
            self.stage_name.as_str(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Permissions::empty(),
        )
        .map_err(io)?
        .into();
        if self.mode == Mode::Hardlink {
            fs::linkat(&s, parts(&self.source)?.1, &dir, "media", AtFlags::empty()).map_err(io)?;
        }
        let file = if self.mode == Mode::Hardlink {
            child(&dir, "media", OFlags::RDONLY)?
        } else {
            child(&dir, "media", OFlags::RDWR | OFlags::CREATE | OFlags::EXCL)?
        };
        if self.mode == Mode::Hardlink {
            check(self.source_identity.content(&Identity::read(&file)?))?;
        }
        sync(&dir)?;
        Ok(Stage {
            directory: Identity::read(&dir)?,
            file: Identity::read(&file)?,
            sha256: None,
            quarantine: None,
            source_retired: false,
        })
    }
    pub fn transfer(&self, mut stage: Stage) -> Result<Stage> {
        let (s, d) = self.parents()?;
        let dir = self.stage_dir(&stage, &d)?;
        let mut source = child(&s, parts(&self.source)?.1, OFlags::RDONLY)?;
        let before = Identity::read(&source)?;
        check(self.source_identity.content(&before))?;
        let mut file = child(
            &dir,
            "media",
            if self.mode == Mode::Hardlink {
                OFlags::RDONLY
            } else {
                OFlags::RDWR
            },
        )?;
        check(stage.file.same(&Identity::read(&file)?))?;
        let digest = if self.mode == Mode::Hardlink {
            hash(&mut file)?
        } else {
            file.set_len(0).map_err(io)?;
            stream(&mut source, Some(&mut file))?
        };
        check(before.unchanged(&Identity::read(&source)?))?;
        file.sync_all().map_err(io)?;
        sync(&dir)?;
        stage.file = Identity::read(&file)?;
        check(stage.file.size == self.source_identity.size)?;
        stage.sha256 = Some(digest);
        Ok(stage)
    }
    pub fn publish(&self, stage: &Stage) -> Result<()> {
        let (_, d) = self.parents()?;
        let dir = self.stage_dir(stage, &d)?;
        verify(&mut child(&dir, "media", OFlags::RDONLY)?, stage)?;
        match fs::linkat(
            &dir,
            "media",
            &d,
            parts(&self.destination)?.1,
            AtFlags::empty(),
        ) {
            Ok(()) => {}
            Err(rustix::io::Errno::EXIST) => {}
            Err(e) => return Err(io(e)),
        }
        verify(
            &mut child(&d, parts(&self.destination)?.1, OFlags::RDONLY)?,
            stage,
        )?;
        sync(&d)
    }
    pub fn verify_destination(&self, stage: &Stage) -> Result<()> {
        let (_, d) = self.parents()?;
        verify(
            &mut child(&d, parts(&self.destination)?.1, OFlags::RDONLY)?,
            stage,
        )
    }
    pub fn prepare_cleanup(&self, mut stage: Stage) -> Result<Stage> {
        if self.mode != Mode::Move || stage.quarantine.is_some() {
            return Ok(stage);
        }
        let (s, _) = self.parents()?;
        fs::mkdirat(
            &s,
            self.quarantine_name.as_str(),
            Permissions::from_bits_truncate(0o700),
        )
        .map_err(|e| {
            if e == rustix::io::Errno::EXIST {
                unowned()
            } else {
                io(e)
            }
        })?;
        sync(&s)?;
        let dir: File = fs::openat(
            &s,
            self.quarantine_name.as_str(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Permissions::empty(),
        )
        .map_err(io)?
        .into();
        stage.quarantine = Some(Identity::read(&dir)?);
        Ok(stage)
    }
    fn quarantine_dir(&self, stage: &Stage, s: &File) -> Result<File> {
        let dir: File = fs::openat(
            s,
            self.quarantine_name.as_str(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Permissions::empty(),
        )
        .map_err(io)?
        .into();
        check(
            stage
                .quarantine
                .as_ref()
                .is_some_and(|i| Identity::read(&dir).is_ok_and(|v| i.same(&v))),
        )?;
        Ok(dir)
    }
    pub fn retire_source(&self, mut stage: Stage) -> Result<Stage> {
        if self.mode != Mode::Move || stage.source_retired {
            return Ok(stage);
        }
        self.verify_destination(&stage)?;
        let (s, _) = self.parents()?;
        let dir = self.quarantine_dir(&stage, &s)?;
        if optional(&dir, "source")?.is_none() {
            let mut original = child(&s, parts(&self.source)?.1, OFlags::RDONLY)?;
            check(self.source_identity.content(&Identity::read(&original)?))?;
            check(hash(&mut original)? == *stage.sha256.as_ref().ok_or_else(changed)?)?;
            fs::renameat_with(
                &s,
                parts(&self.source)?.1,
                &dir,
                "source",
                RenameFlags::NOREPLACE,
            )
            .map_err(io)?;
            sync(&s)?;
            sync(&dir)?;
        }
        let mut retired = child(&dir, "source", OFlags::RDONLY)?;
        check(self.source_identity.content(&Identity::read(&retired)?))?;
        check(hash(&mut retired)? == *stage.sha256.as_ref().ok_or_else(changed)?)?;
        stage.source_retired = true;
        Ok(stage)
    }
    pub fn cleanup(&self, stage: &Stage) -> Result<()> {
        self.verify_destination(stage)?;
        let (s, d) = self.parents()?;
        if self.mode == Mode::Move {
            check(stage.source_retired)?;
            let dir = self.quarantine_dir(stage, &s)?;
            if let Some(mut file) = optional(&dir, "source")? {
                check(self.source_identity.content(&Identity::read(&file)?))?;
                check(hash(&mut file)? == *stage.sha256.as_ref().ok_or_else(changed)?)?;
                fs::unlinkat(&dir, "source", AtFlags::empty()).map_err(io)?;
                sync(&dir)?;
            }
        }
        // Keep the empty private directories as durable ownership receipts. Later housekeeping
        // may retire them after operation retention expires; never guess ownership after restart.
        let dir = self.stage_dir(stage, &d)?;
        if let Some(file) = optional(&dir, "media")? {
            check(stage.file.same(&Identity::read(&file)?))?;
            fs::unlinkat(&dir, "media", AtFlags::empty()).map_err(io)?;
            sync(&dir)?;
        }
        Ok(())
    }
}
fn verify(file: &mut File, stage: &Stage) -> Result<()> {
    check(stage.file.content(&Identity::read(file)?))?;
    check(Some(hash(file)?) == stage.sha256)
}
fn hash(file: &mut File) -> Result<String> {
    let before = Identity::read(file)?;
    file.seek(SeekFrom::Start(0)).map_err(io)?;
    let digest = stream(file, None)?;
    check(before.unchanged(&Identity::read(file)?))?;
    Ok(digest)
}
fn stream(source: &mut File, mut destination: Option<&mut File>) -> Result<String> {
    let started = Instant::now();
    let mut digest = Context::new(&SHA256);
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut total = 0u64;
    loop {
        if started.elapsed() > TRANSFER_LIMIT {
            return Err(Error::conflict(
                "transfer_timeout",
                "Transfer time budget exceeded; source is retained",
            ));
        }
        let n = source.read(&mut buffer).map_err(io)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        check(total <= MAX_FILE_BYTES)?;
        digest.update(&buffer[..n]);
        if let Some(d) = destination.as_mut() {
            d.write_all(&buffer[..n]).map_err(io)?;
        }
    }
    Ok(digest
        .finish()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
