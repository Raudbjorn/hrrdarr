//! Blocking, descriptor-relative rename primitives. Every traversed component and leaf is opened
//! without following symlinks (the same anchored walk the importer and recycle bin use). A move
//! is one `renameat2(RENAME_NOREPLACE)` inside a single filesystem, which is atomic: either the
//! old or the new name exists, never both and never neither. Cross-device moves are refused, not
//! emulated by copying.
use crate::recycle_bin::fsops::{FsError, open_dir, split};
use rustix::fs::{self, AtFlags, Dir, FileType, Mode, OFlags, RenameFlags};
use serde::{Deserialize, Serialize};
use std::fs::File;

/// Entries read when searching a directory for sidecars; larger directories are refused.
pub(crate) const MAX_DIR_ENTRIES: usize = 50_000;
/// Sidecars moved together with one media file.
pub(crate) const MAX_SIDECARS: usize = 64;
/// Extensions treated as sidecars of a media file (same stem followed by a dot).
const SIDECAR_EXTENSIONS: &[&str] = &[
    "srt", "ass", "ssa", "sub", "idx", "sup", "vtt", "nfo", "jpg", "jpeg", "png", "tbn",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MoveError {
    Missing,
    Exists,
    Changed,
    CrossDevice,
    Unsafe,
    Denied,
    NotRegular,
    TooLarge,
    Io,
}
impl MoveError {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Missing => "source_missing",
            Self::Exists => "destination_exists",
            Self::Changed => "source_changed",
            Self::CrossDevice => "cross_device",
            Self::Unsafe => "path_unsafe",
            Self::Denied => "permission_denied",
            Self::NotRegular => "file_not_regular",
            Self::TooLarge => "directory_too_large",
            Self::Io => "filesystem_error",
        }
    }
}
impl From<FsError> for MoveError {
    fn from(e: FsError) -> Self {
        match e {
            FsError::Missing => Self::Missing,
            FsError::Denied => Self::Denied,
            FsError::NotRegular => Self::NotRegular,
            FsError::Unsafe => Self::Unsafe,
            FsError::Changed => Self::Changed,
            FsError::Exists => Self::Exists,
            FsError::Limit => Self::TooLarge,
            FsError::Io => Self::Io,
        }
    }
}
fn errno(e: rustix::io::Errno) -> MoveError {
    match e {
        rustix::io::Errno::NOENT => MoveError::Missing,
        rustix::io::Errno::EXIST | rustix::io::Errno::NOTEMPTY => MoveError::Exists,
        rustix::io::Errno::XDEV => MoveError::CrossDevice,
        rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR => MoveError::Unsafe,
        rustix::io::Errno::ACCESS | rustix::io::Errno::PERM | rustix::io::Errno::ROFS => {
            MoveError::Denied
        }
        _ => MoveError::Io,
    }
}
type R<T> = std::result::Result<T, MoveError>;

/// Device and inode, stored as signed integers (bit casts) so they fit database columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Ident {
    pub dev: i64,
    pub ino: i64,
}
impl Ident {
    fn of(stat: &fs::Stat) -> Self {
        Self {
            dev: stat.st_dev as u64 as i64,
            ino: stat.st_ino as u64 as i64,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    File,
    Dir,
}
fn kind_ok(stat: &fs::Stat, kind: Kind) -> bool {
    let t = FileType::from_raw_mode(stat.st_mode);
    match kind {
        Kind::File => t == FileType::RegularFile,
        Kind::Dir => t == FileType::Directory,
    }
}
fn stat_in(dir: &File, name: &str) -> R<Option<fs::Stat>> {
    match fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(s) => Ok(Some(s)),
        Err(rustix::io::Errno::NOENT) => Ok(None),
        Err(e) => Err(errno(e)),
    }
}
fn parent_and_name(path: &str) -> R<(String, String)> {
    split(path).map_err(MoveError::from)
}
/// Identity of a regular file or directory at `path`; `None` when absent. A symlink is never
/// followed and never accepted.
pub(crate) fn observe(path: &str, kind: Kind) -> R<Option<Ident>> {
    let (parent, name) = parent_and_name(path)?;
    let dir = match open_dir(&parent) {
        Ok(d) => d,
        Err(FsError::Missing) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    match stat_in(&dir, &name)? {
        None => Ok(None),
        Some(s) if kind_ok(&s, kind) => Ok(Some(Ident::of(&s))),
        Some(_) => Err(MoveError::NotRegular),
    }
}
/// True when both paths are under `root` (exclusive) by whole components.
pub(crate) fn within(root: &str, path: &str) -> bool {
    let root = root.trim_end_matches('/');
    path.len() > root.len() + 1 && path.starts_with(root) && path.as_bytes()[root.len()] == b'/'
}
fn names(dir: &File) -> R<Vec<String>> {
    let mut out = vec![];
    let entries = Dir::read_from(dir).map_err(errno)?;
    for (seen, item) in entries.enumerate() {
        if seen >= MAX_DIR_ENTRIES {
            return Err(MoveError::TooLarge);
        }
        let item = item.map_err(errno)?;
        if let Ok(n) = item.file_name().to_str() {
            if !matches!(n, "." | "..") {
                out.push(n.to_owned());
            }
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Probe {
    /// Source present, destination free (missing directories would be created).
    Ready,
    /// Names differ only by case and no distinct destination entry exists.
    CaseOnly,
    Exists,
    SourceMissing,
    Unsafe,
}
/// Read-only feasibility check shared by previews and execution.
pub(crate) fn probe(root: &str, old: &str, new: &str, kind: Kind) -> Probe {
    if !within(root, old) || !within(root, new) {
        return Probe::Unsafe;
    }
    let inner = || -> R<Probe> {
        let (op, on) = parent_and_name(old)?;
        let (np, nn) = parent_and_name(new)?;
        let od = open_dir(&op)?;
        match stat_in(&od, &on)? {
            None => return Ok(Probe::SourceMissing),
            Some(s) if !kind_ok(&s, kind) => return Err(MoveError::NotRegular),
            Some(_) => {}
        }
        // The deepest existing ancestor of the destination parent must be reachable without links.
        let mut ancestor = np.clone();
        let nd = loop {
            match open_dir(&ancestor) {
                Ok(d) => break d,
                Err(FsError::Missing) => {
                    let cut = ancestor.rfind('/').ok_or(MoveError::Unsafe)?;
                    if cut == 0 {
                        return Err(MoveError::Unsafe);
                    }
                    ancestor.truncate(cut);
                    if !within(root, &format!("{ancestor}/x"))
                        && ancestor != root.trim_end_matches('/')
                    {
                        return Err(MoveError::Unsafe);
                    }
                }
                Err(e) => return Err(e.into()),
            }
        };
        if ancestor != np {
            return Ok(Probe::Ready);
        }
        if stat_in(&nd, &nn)?.is_none() {
            return Ok(Probe::Ready);
        }
        if op == np && on != nn && on.to_lowercase() == nn.to_lowercase() {
            let listing = names(&nd)?;
            if !listing.iter().any(|n| *n == nn) {
                return Ok(Probe::CaseOnly);
            }
        }
        Ok(Probe::Exists)
    };
    match inner() {
        Ok(p) => p,
        Err(MoveError::Missing) => Probe::SourceMissing,
        Err(_) => Probe::Unsafe,
    }
}
fn open_or_create_dir(parent: &File, name: &str) -> R<File> {
    match fs::mkdirat(parent, name, Mode::from_bits_truncate(0o755)) {
        Ok(()) | Err(rustix::io::Errno::EXIST) => {}
        Err(e) => return Err(errno(e)),
    }
    Ok(fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(errno)?
    .into())
}
/// Opens the destination parent, creating missing directories below `root` only.
fn destination_parent(root: &str, parent: &str) -> R<File> {
    if let Ok(d) = open_dir(parent) {
        return Ok(d);
    }
    let root = root.trim_end_matches('/');
    let tail = parent
        .strip_prefix(root)
        .and_then(|t| t.strip_prefix('/'))
        .ok_or(MoveError::Unsafe)?;
    let mut dir = open_dir(root)?;
    for part in tail.split('/') {
        dir = open_or_create_dir(&dir, part)?;
    }
    Ok(dir)
}
/// Moves `old` to `new` if `old` still has identity `expect`. Never replaces an existing entry
/// except the case-only same-entry rename on case-insensitive filesystems.
pub(crate) fn move_path(root: &str, old: &str, new: &str, kind: Kind, expect: Ident) -> R<()> {
    if !within(root, old) || !within(root, new) {
        return Err(MoveError::Unsafe);
    }
    let (op, on) = parent_and_name(old)?;
    let (np, nn) = parent_and_name(new)?;
    let od = open_dir(&op)?;
    let s = stat_in(&od, &on)?.ok_or(MoveError::Missing)?;
    if !kind_ok(&s, kind) {
        return Err(MoveError::NotRegular);
    }
    if Ident::of(&s) != expect {
        return Err(MoveError::Changed);
    }
    let nd = destination_parent(root, &np)?;
    let flags = if op == np && on != nn && on.to_lowercase() == nn.to_lowercase() {
        let listing = names(&nd)?;
        if listing.iter().any(|n| *n == nn) {
            return Err(MoveError::Exists);
        }
        RenameFlags::empty()
    } else {
        RenameFlags::NOREPLACE
    };
    if flags.is_empty() {
        fs::renameat(&od, on.as_str(), &nd, nn.as_str()).map_err(errno)?;
    } else {
        fs::renameat_with(&od, on.as_str(), &nd, nn.as_str(), flags).map_err(errno)?;
    }
    od.sync_all().map_err(|_| MoveError::Io)?;
    if op != np {
        nd.sync_all().map_err(|_| MoveError::Io)?;
    }
    Ok(())
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PairState {
    /// Old entry present with the recorded identity and new spelling absent.
    Source,
    /// New entry present with the recorded identity and old spelling absent.
    Moved,
    Anomaly,
}
pub(crate) fn pair_state(old: &str, new: &str, kind: Kind, expect: Ident) -> R<PairState> {
    let o = observe(old, kind)?;
    let n = observe(new, kind)?;
    Ok(match (o, n) {
        (Some(i), None) if i == expect => PairState::Source,
        (None, Some(i)) if i == expect => PairState::Moved,
        (Some(o), Some(n)) if o == expect && n == expect => case_only_pair_state(old, new)?,
        _ => PairState::Anomaly,
    })
}

fn case_only_pair_state(old: &str, new: &str) -> R<PairState> {
    let (op, on) = parent_and_name(old)?;
    let (np, nn) = parent_and_name(new)?;
    if op != np || on == nn || on.to_lowercase() != nn.to_lowercase() {
        return Ok(PairState::Anomaly);
    }
    // Case-insensitive lookup aliases both paths; two actual entries remain a conflict.
    let listing = names(&open_dir(&op)?)?;
    Ok(match (listing.contains(&on), listing.contains(&nn)) {
        (true, false) => PairState::Source,
        (false, true) => PairState::Moved,
        _ => PairState::Anomaly,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Sidecar {
    pub old: String,
    pub new: String,
    pub ident: Ident,
}
/// Files in the media file's directory named `<stem>.<suffix>` with an allow-listed final
/// extension, renamed to `<new_stem>.<suffix>`. Paths in `tracked` (other media files) are skipped.
pub(crate) fn discover_sidecars(
    old_media: &str,
    new_media: &str,
    tracked: &dyn Fn(&str) -> bool,
) -> R<Vec<Sidecar>> {
    let (op, on) = parent_and_name(old_media)?;
    let (np, nn) = parent_and_name(new_media)?;
    let (Some((old_stem, _)), Some((new_stem, _))) = (on.rsplit_once('.'), nn.rsplit_once('.'))
    else {
        return Ok(vec![]);
    };
    let dir = open_dir(&op)?;
    let prefix = format!("{old_stem}.");
    let mut out = vec![];
    let mut listing = names(&dir)?;
    listing.sort();
    for name in listing {
        if name == on || !name.starts_with(&prefix) {
            continue;
        }
        let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        if !SIDECAR_EXTENSIONS.contains(&ext.as_str()) {
            continue;
        }
        let old = format!("{op}/{name}");
        if tracked(&old) {
            continue;
        }
        let Some(stat) = stat_in(&dir, &name)? else {
            continue;
        };
        if !kind_ok(&stat, Kind::File) {
            continue;
        }
        if out.len() >= MAX_SIDECARS {
            return Err(MoveError::TooLarge);
        }
        out.push(Sidecar {
            old,
            new: format!("{np}/{new_stem}.{}", &name[prefix.len()..]),
            ident: Ident::of(&stat),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scratch() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("hrrdarr-fsops-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
    fn s(p: &std::path::Path, rel: &str) -> String {
        format!("{}/{rel}", p.display())
    }
    #[test]
    fn within_requires_whole_components() {
        assert!(within("/a/b", "/a/b/c"));
        assert!(within("/a/b/", "/a/b/c/d"));
        assert!(!within("/a/b", "/a/bc/d"));
        assert!(!within("/a/b", "/a/b"));
    }
    #[test]
    fn move_never_replaces_and_creates_missing_directories_below_root() {
        let d = scratch();
        std::fs::write(d.join("a.mkv"), b"a").unwrap();
        std::fs::write(d.join("b.mkv"), b"b").unwrap();
        let root = d.display().to_string();
        let a = observe(&s(&d, "a.mkv"), Kind::File).unwrap().unwrap();
        assert_eq!(
            probe(&root, &s(&d, "a.mkv"), &s(&d, "b.mkv"), Kind::File),
            Probe::Exists
        );
        assert_eq!(
            move_path(&root, &s(&d, "a.mkv"), &s(&d, "b.mkv"), Kind::File, a),
            Err(MoveError::Exists)
        );
        assert_eq!(std::fs::read(d.join("b.mkv")).unwrap(), b"b");
        assert_eq!(
            probe(&root, &s(&d, "a.mkv"), &s(&d, "new/deep/a.mkv"), Kind::File),
            Probe::Ready
        );
        move_path(
            &root,
            &s(&d, "a.mkv"),
            &s(&d, "new/deep/a.mkv"),
            Kind::File,
            a,
        )
        .unwrap();
        assert!(d.join("new/deep/a.mkv").exists() && !d.join("a.mkv").exists());
        assert_eq!(
            pair_state(&s(&d, "a.mkv"), &s(&d, "new/deep/a.mkv"), Kind::File, a),
            Ok(PairState::Moved)
        );
        // A changed source identity is refused.
        let b = observe(&s(&d, "b.mkv"), Kind::File).unwrap().unwrap();
        std::fs::remove_file(d.join("b.mkv")).unwrap();
        std::fs::write(d.join("b.mkv"), b"replaced").unwrap();
        let moved = move_path(&root, &s(&d, "b.mkv"), &s(&d, "c.mkv"), Kind::File, b);
        assert!(
            matches!(moved, Err(MoveError::Changed))
                || b != observe(&s(&d, "b.mkv"), Kind::File).unwrap().unwrap()
        );
        // Outside the root is unsafe.
        assert_eq!(
            probe(&root, &s(&d, "b.mkv"), "/tmp/elsewhere.mkv", Kind::File),
            Probe::Unsafe
        );
        std::fs::remove_dir_all(d).unwrap();
    }
    #[test]
    fn symlinked_components_are_refused() {
        let d = scratch();
        std::fs::create_dir(d.join("real")).unwrap();
        std::os::unix::fs::symlink(d.join("real"), d.join("link")).unwrap();
        std::fs::write(d.join("a.mkv"), b"a").unwrap();
        let root = d.display().to_string();
        let a = observe(&s(&d, "a.mkv"), Kind::File).unwrap().unwrap();
        assert_eq!(
            probe(&root, &s(&d, "a.mkv"), &s(&d, "link/a.mkv"), Kind::File),
            Probe::Unsafe
        );
        assert!(move_path(&root, &s(&d, "a.mkv"), &s(&d, "link/a.mkv"), Kind::File, a).is_err());
        assert!(d.join("a.mkv").exists() && !d.join("real/a.mkv").exists());
        // A symlink leaf is never a media file.
        std::os::unix::fs::symlink(d.join("a.mkv"), d.join("sym.mkv")).unwrap();
        assert_eq!(
            observe(&s(&d, "sym.mkv"), Kind::File),
            Err(MoveError::NotRegular)
        );
        std::fs::remove_dir_all(d).unwrap();
    }
    #[test]
    fn case_only_rename_and_sidecar_discovery() {
        let d = scratch();
        std::fs::write(d.join("show.mkv"), b"a").unwrap();
        std::fs::write(d.join("show.en.srt"), b"s").unwrap();
        std::fs::write(d.join("show2.srt"), b"x").unwrap();
        std::fs::write(d.join("show.mp4"), b"other media").unwrap();
        let root = d.display().to_string();
        let a = observe(&s(&d, "show.mkv"), Kind::File).unwrap().unwrap();
        assert_eq!(
            probe(&root, &s(&d, "show.mkv"), &s(&d, "Show.mkv"), Kind::File),
            Probe::Ready
        );
        let side = discover_sidecars(&s(&d, "show.mkv"), &s(&d, "Show.mkv"), &|_| false).unwrap();
        assert_eq!(side.len(), 1);
        assert!(side[0].new.ends_with("/Show.en.srt"));
        let tracked = discover_sidecars(&s(&d, "show.mkv"), &s(&d, "Show.mkv"), &|p| {
            p.ends_with("show.en.srt")
        })
        .unwrap();
        assert!(tracked.is_empty());
        assert_eq!(
            pair_state(&s(&d, "show.mkv"), &s(&d, "Show.mkv"), Kind::File, a),
            Ok(PairState::Source)
        );
        move_path(&root, &s(&d, "show.mkv"), &s(&d, "Show.mkv"), Kind::File, a).unwrap();
        assert_eq!(
            pair_state(&s(&d, "show.mkv"), &s(&d, "Show.mkv"), Kind::File, a),
            Ok(PairState::Moved)
        );
        let names: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert!(names.contains(&"Show.mkv".into()) && !names.contains(&"show.mkv".into()));
        std::fs::remove_dir_all(d).unwrap();
    }
    #[test]
    fn case_only_pair_uses_entry_spelling_and_rejects_hardlink_ambiguity() {
        let d = scratch();
        let old = s(&d, "show.mkv");
        let new = s(&d, "Show.mkv");
        std::fs::write(&old, b"media").unwrap();
        let ident = observe(&old, Kind::File).unwrap().unwrap();
        // Exercise the aliased-inode classifier without requiring a case-insensitive mount.
        assert_eq!(case_only_pair_state(&old, &new), Ok(PairState::Source));
        std::fs::rename(&old, &new).unwrap();
        assert_eq!(case_only_pair_state(&old, &new), Ok(PairState::Moved));
        std::fs::hard_link(&new, &old).unwrap();
        assert_eq!(
            pair_state(&old, &new, Kind::File, ident),
            Ok(PairState::Anomaly)
        );
        assert_eq!(case_only_pair_state(&old, &old), Ok(PairState::Anomaly));
        assert_eq!(
            case_only_pair_state(&old, &s(&d, "other.mkv")),
            Ok(PairState::Anomaly)
        );
        assert_eq!(
            case_only_pair_state(&old, &s(&d, "other/Show.mkv")),
            Ok(PairState::Anomaly)
        );
        std::fs::remove_file(&old).unwrap();
        std::fs::remove_file(&new).unwrap();
        assert_eq!(case_only_pair_state(&old, &new), Ok(PairState::Anomaly));
        std::fs::remove_dir_all(d).unwrap();
    }
    #[test]
    fn error_codes_are_closed_and_cross_device_is_distinct() {
        assert_eq!(errno(rustix::io::Errno::XDEV), MoveError::CrossDevice);
        assert_eq!(MoveError::CrossDevice.code(), "cross_device");
        assert_eq!(errno(rustix::io::Errno::EXIST), MoveError::Exists);
    }
}
