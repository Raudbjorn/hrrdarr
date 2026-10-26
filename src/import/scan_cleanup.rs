//! Anchored, bounded removal of directories a downloaded-folder scan emptied by moving files.
//!
//! Nothing here deletes a file or recurses into an unknown directory. Only directories whose
//! device/inode identity was recorded in the published inventory (or the engine's private
//! `.hrrdarr-move-{operation}` directory of an imported child, matched by name AND recorded
//! identity) are candidates, and each is removed with `rmdir` semantics (`AT_REMOVEDIR`), which
//! the kernel refuses for a non-empty directory. Everything else is retained and classified.
//! Call from a blocking thread; the deadline is checked between syscalls.
use rustix::fs::{self, AtFlags, Dir, FileType, Mode as Permissions, OFlags};
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    os::unix::fs::MetadataExt,
    path::Path,
    time::Instant,
};

const MAX_DEPTH: usize = 32;
const MAX_VISITED: usize = 20_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DirIdentity {
    pub dev: u64,
    pub ino: u64,
}

/// Everything the removal may rely on; all of it comes from durable scan evidence.
#[derive(Clone)]
pub(crate) struct CleanupPlan {
    pub source_path: String,
    pub root: DirIdentity,
    pub parent: DirIdentity,
    /// Directories recorded in the published inventory.
    pub known_dirs: HashSet<(u64, u64)>,
    /// Private move directories of imported children: name -> recorded identity.
    pub quarantine: HashMap<String, (u64, u64)>,
    /// Inventory files that are media the scan did not import (by device/inode).
    pub unimported_media: HashSet<(u64, u64)>,
    /// False on the first execution after `remove_intent`; true for an explicit resume, where an
    /// absent root under the same parent is the result of the earlier, uncertain removal.
    pub resumed: bool,
    pub deadline: Instant,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CleanupOutcome {
    /// The scanned folder and every emptied directory beneath it are gone.
    Complete { removed: u64 },
    /// Known leftovers remain; `reason` is one of the static retained codes.
    Retained { reason: &'static str, removed: u64 },
}

/// A failure after the intent was recorded whose effect is not known; it stays pending until an
/// explicit resume. `removed` counts directories this attempt did remove before failing.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Uncertain {
    pub code: &'static str,
    pub removed: u64,
}

/// Measures the scanned folder before any intent: the root must still be the inventory's
/// directory, and its parent identity is recorded for later continuity checks.
pub(crate) fn observe(
    source_path: &str,
    root: DirIdentity,
) -> std::result::Result<DirIdentity, &'static str> {
    let path = Path::new(source_path);
    let (parent_path, name) = split(path).ok_or("path_invalid")?;
    let parent = super::fs::directory_io(parent_path).map_err(classify_io)?;
    let parent_identity = identity(&parent).map_err(classify_io)?;
    let stat = fs::statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(classify_errno)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory
        || stat.st_dev != root.dev
        || stat.st_ino != root.ino
    {
        return Err("identity_changed");
    }
    Ok(parent_identity)
}

fn split(path: &Path) -> Option<(&Path, &str)> {
    Some((path.parent()?, path.file_name()?.to_str()?))
}

fn identity(f: &File) -> std::io::Result<DirIdentity> {
    let m = f.metadata()?;
    Ok(DirIdentity {
        dev: m.dev(),
        ino: m.ino(),
    })
}

fn classify_io(error: std::io::Error) -> &'static str {
    match error.kind() {
        std::io::ErrorKind::PermissionDenied => "path_denied",
        std::io::ErrorKind::NotFound => "identity_changed",
        std::io::ErrorKind::NotADirectory | std::io::ErrorKind::InvalidInput => "path_invalid",
        _ if error.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error()) => "path_invalid",
        _ => "cleanup_io",
    }
}

fn classify_errno(errno: rustix::io::Errno) -> &'static str {
    classify_io(errno.into())
}

fn open_dir(parent: &File, name: &str) -> std::result::Result<File, rustix::io::Errno> {
    fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Permissions::empty(),
    )
    .map(File::from)
}

const ARCHIVE_EXTENSIONS: &[&str] = &["rar", "zip", "7z", "tar", "gz", "bz2", "xz", "zst"];
fn archive(name: &str) -> bool {
    let Some((_, ext)) = name.rsplit_once('.') else {
        return false;
    };
    let ext = ext.to_ascii_lowercase();
    ARCHIVE_EXTENSIONS.contains(&ext.as_str())
        || (ext.len() == 3 && ext.starts_with('r') && ext[1..].bytes().all(|b| b.is_ascii_digit()))
}

struct Walk<'a> {
    plan: &'a CleanupPlan,
    visited: usize,
    removed: u64,
    denied: bool,
    unimported: bool,
    archives: bool,
    unknown: bool,
}

impl Walk<'_> {
    fn over_budget(&mut self) -> bool {
        self.visited += 1;
        self.visited > MAX_VISITED || Instant::now() >= self.plan.deadline
    }

    /// Post-order: returns true when `dir` is empty after pruning its removable directories.
    fn prune(&mut self, dir: &File, depth: usize) -> std::result::Result<bool, Uncertain> {
        let mut names = Vec::new();
        let entries = Dir::read_from(dir).map_err(|e| self.failed(e))?;
        for item in entries {
            let item = item.map_err(|e| self.failed(e))?;
            let Ok(name) = item.file_name().to_str() else {
                // A non-UTF-8 name was never part of the inventory: it is retained.
                self.unknown = true;
                return Ok(false);
            };
            if matches!(name, "." | "..") {
                continue;
            }
            if self.over_budget() {
                // Bounded: what was not visited is kept, so this directory is not empty.
                self.unknown = true;
                return Ok(false);
            }
            names.push(name.to_owned());
        }
        let mut empty = true;
        for name in names {
            let stat = match fs::statat(dir, name.as_str(), AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) => stat,
                Err(rustix::io::Errno::NOENT) => continue,
                Err(rustix::io::Errno::ACCESS | rustix::io::Errno::PERM) => {
                    self.denied = true;
                    empty = false;
                    continue;
                }
                Err(e) => return Err(self.failed(e)),
            };
            let key = (stat.st_dev, stat.st_ino);
            if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
                empty = false;
                if self.plan.unimported_media.contains(&key) {
                    self.unimported = true;
                } else if archive(&name) {
                    self.archives = true;
                } else {
                    self.unknown = true;
                }
                continue;
            }
            let known = self.plan.known_dirs.contains(&key)
                || self.plan.quarantine.get(&name) == Some(&key);
            if !known || depth + 1 >= MAX_DEPTH {
                // Never descend into a directory the scan did not record.
                self.unknown = true;
                empty = false;
                continue;
            }
            let nested = match open_dir(dir, &name) {
                Ok(nested) => nested,
                Err(rustix::io::Errno::NOENT) => continue,
                Err(rustix::io::Errno::ACCESS | rustix::io::Errno::PERM) => {
                    self.denied = true;
                    empty = false;
                    continue;
                }
                Err(rustix::io::Errno::NOTDIR | rustix::io::Errno::LOOP) => {
                    self.unknown = true;
                    empty = false;
                    continue;
                }
                Err(e) => return Err(self.failed(e)),
            };
            let opened = identity(&nested).map_err(|e| self.failed(e))?;
            if (opened.dev, opened.ino) != key {
                // Replaced between stat and open: not ours.
                self.unknown = true;
                empty = false;
                continue;
            }
            if !self.prune(&nested, depth + 1)? {
                empty = false;
                continue;
            }
            drop(nested);
            match fs::unlinkat(dir, name.as_str(), AtFlags::REMOVEDIR) {
                Ok(()) => self.removed += 1,
                Err(rustix::io::Errno::NOENT) => {}
                // Something appeared after the walk; rmdir refused it, so it is retained.
                Err(rustix::io::Errno::NOTEMPTY | rustix::io::Errno::EXIST) => {
                    self.unknown = true;
                    empty = false;
                }
                Err(rustix::io::Errno::ACCESS | rustix::io::Errno::PERM) => {
                    self.denied = true;
                    empty = false;
                }
                Err(e) => return Err(self.failed(e)),
            }
        }
        Ok(empty)
    }

    fn failed(&self, error: impl Into<std::io::Error>) -> Uncertain {
        let error: std::io::Error = error.into();
        eprintln!(
            "event=downloaded_scan_cleanup_io kind={:?} os_code={:?}",
            error.kind(),
            error.raw_os_error()
        );
        Uncertain {
            code: "cleanup_io",
            removed: self.removed,
        }
    }

    fn retained(&self) -> &'static str {
        if self.denied {
            "path_denied"
        } else if self.unimported {
            "unimported_media"
        } else if self.archives {
            "archive_leftovers"
        } else {
            "unknown_leftovers"
        }
    }
}

/// Removes the emptied directories, then the scanned folder itself if it became empty.
/// Deterministic refusals are settled outcomes; only unexplained I/O stays uncertain.
pub(crate) fn remove(plan: &CleanupPlan) -> std::result::Result<CleanupOutcome, Uncertain> {
    let retained = |reason| Ok(CleanupOutcome::Retained { reason, removed: 0 });
    let path = Path::new(&plan.source_path);
    let Some((parent_path, name)) = split(path) else {
        return retained("path_invalid");
    };
    let parent = match super::fs::directory_io(parent_path) {
        Ok(parent) => parent,
        Err(e) => {
            return match classify_io(e) {
                "cleanup_io" => Err(Uncertain {
                    code: "cleanup_io",
                    removed: 0,
                }),
                reason => retained(reason),
            };
        }
    };
    match identity(&parent) {
        Ok(found) if found == plan.parent => {}
        Ok(_) => return retained("identity_changed"),
        Err(_) => {
            return Err(Uncertain {
                code: "cleanup_io",
                removed: 0,
            });
        }
    }
    let stat = match fs::statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        // Same parent, no replacement, and our own retained intent: the earlier attempt
        // removed it. On a first attempt an absent root was removed by someone else.
        Err(rustix::io::Errno::NOENT) if plan.resumed => {
            return Ok(CleanupOutcome::Complete { removed: 0 });
        }
        Err(rustix::io::Errno::NOENT) => return retained("identity_changed"),
        Err(e) => {
            return match classify_errno(e) {
                "cleanup_io" => Err(Uncertain {
                    code: "cleanup_io",
                    removed: 0,
                }),
                reason => retained(reason),
            };
        }
    };
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory
        || (stat.st_dev, stat.st_ino) != (plan.root.dev, plan.root.ino)
    {
        return retained("identity_changed");
    }
    let root = match open_dir(&parent, name) {
        Ok(root) => root,
        Err(e) => {
            return match classify_errno(e) {
                "cleanup_io" => Err(Uncertain {
                    code: "cleanup_io",
                    removed: 0,
                }),
                reason => retained(reason),
            };
        }
    };
    match identity(&root) {
        Ok(found) if found == plan.root => {}
        _ => return retained("identity_changed"),
    }
    let mut walk = Walk {
        plan,
        visited: 0,
        removed: 0,
        denied: false,
        unimported: false,
        archives: false,
        unknown: false,
    };
    let empty = walk.prune(&root, 0)?;
    drop(root);
    if !empty {
        return Ok(CleanupOutcome::Retained {
            reason: walk.retained(),
            removed: walk.removed,
        });
    }
    match fs::unlinkat(&parent, name, AtFlags::REMOVEDIR) {
        Ok(()) => Ok(CleanupOutcome::Complete {
            removed: walk.removed + 1,
        }),
        Err(rustix::io::Errno::NOTEMPTY | rustix::io::Errno::EXIST) => {
            Ok(CleanupOutcome::Retained {
                reason: "unknown_leftovers",
                removed: walk.removed,
            })
        }
        Err(rustix::io::Errno::ACCESS | rustix::io::Errno::PERM) => Ok(CleanupOutcome::Retained {
            reason: "path_denied",
            removed: walk.removed,
        }),
        Err(e) => Err(walk.failed(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn key(path: &Path) -> (u64, u64) {
        let m = std::fs::symlink_metadata(path).unwrap();
        (m.dev(), m.ino())
    }
    fn plan(root: &Path, known: &[&Path], resumed: bool) -> CleanupPlan {
        let (dev, ino) = key(root);
        let parent = observe(root.to_str().unwrap(), DirIdentity { dev, ino }).unwrap();
        CleanupPlan {
            source_path: root.to_str().unwrap().into(),
            root: DirIdentity { dev, ino },
            parent,
            known_dirs: known.iter().map(|p| key(p)).collect(),
            quarantine: HashMap::new(),
            unimported_media: HashSet::new(),
            resumed,
            deadline: Instant::now() + Duration::from_secs(10),
        }
    }

    #[test]
    fn removes_only_recorded_empty_directories_and_keeps_files_and_unknown_dirs() {
        let base = std::env::temp_dir().join(format!("hrrdarr-cleanup-{}", uuid::Uuid::new_v4()));
        let _scratch = Scratch(base.clone());
        let root = base.join("Release");
        let empty = root.join("Subs");
        let keep = root.join("Sample");
        let unknown = root.join("Later");
        for d in [&empty, &keep, &unknown] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(keep.join("sample.mkv"), b"x").unwrap();
        let p = plan(&root, &[&empty, &keep], false);
        // `Later` was never recorded: never entered, never removed.
        assert_eq!(
            remove(&p),
            Ok(CleanupOutcome::Retained {
                reason: "unknown_leftovers",
                removed: 1
            })
        );
        assert!(!empty.exists());
        assert!(keep.join("sample.mkv").exists());
        assert!(unknown.exists());
        // Without the unknown directory the sample file alone still keeps its directory.
        std::fs::remove_dir(&unknown).unwrap();
        assert_eq!(
            remove(&plan(&root, &[&keep], false)),
            Ok(CleanupOutcome::Retained {
                reason: "unknown_leftovers",
                removed: 0
            })
        );
        std::fs::remove_file(keep.join("sample.mkv")).unwrap();
        let p = plan(&root, &[&keep], false);
        assert_eq!(remove(&p), Ok(CleanupOutcome::Complete { removed: 2 }));
        assert!(!root.exists());
        // An absent root is complete only for an explicit resume of retained intent.
        assert_eq!(
            remove(&CleanupPlan { resumed: true, ..p }),
            Ok(CleanupOutcome::Complete { removed: 0 })
        );
    }

    #[test]
    fn replaced_root_or_parent_is_identity_changed_and_archives_are_classified() {
        let base = std::env::temp_dir().join(format!("hrrdarr-cleanup-{}", uuid::Uuid::new_v4()));
        let _scratch = Scratch(base.clone());
        let root = base.join("Release");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("release.r00"), b"x").unwrap();
        let p = plan(&root, &[], false);
        assert_eq!(
            remove(&p),
            Ok(CleanupOutcome::Retained {
                reason: "archive_leftovers",
                removed: 0
            })
        );
        std::fs::remove_dir_all(&root).unwrap();
        std::fs::create_dir(&root).unwrap();
        for resumed in [false, true] {
            assert_eq!(
                remove(&CleanupPlan {
                    resumed,
                    ..p.clone()
                }),
                Ok(CleanupOutcome::Retained {
                    reason: "identity_changed",
                    removed: 0
                })
            );
        }
        assert!(root.exists(), "a replacement is never removed");
    }
}
