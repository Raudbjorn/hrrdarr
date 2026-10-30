//! Read-only inventory. Call from a blocking worker; publication and imports revalidate it.
//! The deadline is checked between syscalls; it cannot interrupt a stalled filesystem syscall.
use super::*;
use crate::api::MediaDomain;
use rustix::fs::{Dir, FileType, StatExt};
use std::sync::atomic::{AtomicBool, Ordering};

mod durable;
mod siblings;

const MAX_ENTRIES: usize = 20_000;
const MAX_DEPTH: usize = 32;
const MAX_MANIFEST_BYTES: usize = 64 * 1024 * 1024;
const OBSERVATION_LIMIT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Entry {
    pub relative_path: String,
    pub kind: Kind,
    #[serde(deserialize_with = "durable::identity")]
    identity: Identity,
}

impl Entry {
    pub fn size(&self) -> u64 {
        self.identity.size
    }

    pub fn physical_key(&self) -> (u64, u64) {
        (self.identity.dev, self.identity.ino)
    }

    pub fn identity_json(&self) -> Result<String> {
        #[derive(Serialize)]
        struct Evidence<'a> {
            version: u8,
            #[serde(flatten)]
            identity: &'a Identity,
        }
        serde_json::to_string(&Evidence {
            version: 1,
            identity: &self.identity,
        })
        .map_err(|_| invalid())
    }
}

#[derive(Debug)]
pub(crate) struct Inventory {
    source_path: String,
    ancestors: Vec<Identity>,
    root: Entry,
    entries: Vec<Entry>,
}

fn limit() -> Error {
    Error::conflict(
        "scan_inventory_limit",
        "Source inventory exceeds its entry, depth, byte or observation limit",
    )
}

fn within_time(start: Instant) -> Result<()> {
    if start.elapsed() >= OBSERVATION_LIMIT {
        Err(limit())
    } else {
        Ok(())
    }
}

fn observe(parent: &File, name: &str, relative_path: String) -> Result<Entry> {
    let s = fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(io)?;
    let kind = match FileType::from_raw_mode(s.st_mode) {
        FileType::RegularFile => Kind::File,
        FileType::Directory => Kind::Directory,
        FileType::Symlink => Kind::Symlink,
        _ => Kind::Other,
    };
    Ok(Entry {
        relative_path,
        kind,
        identity: Identity {
            dev: s.st_dev,
            ino: s.st_ino,
            size: s.st_size.try_into().map_err(|_| invalid())?,
            mtime: s.mtime(),
            mtime_ns: s.st_mtime_nsec.try_into().map_err(|_| invalid())?,
            ctime: s.ctime(),
            ctime_ns: s.st_ctime_nsec.try_into().map_err(|_| invalid())?,
        },
    })
}

fn open_directory(parent: &File, name: &str) -> Result<File> {
    fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Permissions::empty(),
    )
    .map(File::from)
    .map_err(io)
}

fn parent_chain(
    path: &str,
    expected: Option<&[Identity]>,
    start: Instant,
) -> Result<(File, Vec<Identity>)> {
    within_time(start)?;
    let (parent, _) = parts(path)?;
    let mut current = directory(Path::new("/"))?;
    let mut identities = vec![Identity::read(&current)?];
    for component in parent.components() {
        within_time(start)?;
        match component {
            Component::RootDir => (),
            Component::Normal(name) => {
                current = open_directory(&current, name.to_str().ok_or_else(invalid)?)?;
                identities.push(Identity::read(&current)?);
            }
            _ => return Err(invalid()),
        }
    }
    within_time(start)?;
    if let Some(expected) = expected {
        check(
            expected.len() == identities.len()
                && expected.iter().zip(&identities).all(|(a, b)| a.same(b)),
        )?;
    }
    Ok((current, identities))
}

struct Walk {
    entries: Vec<Entry>,
    bytes: usize,
    start: Instant,
}

impl Walk {
    fn push(&mut self, entry: Entry) -> Result<()> {
        within_time(self.start)?;
        if self.entries.len() >= MAX_ENTRIES {
            return Err(limit());
        }
        // Count the actual serialized inventory, including escaped path bytes.
        let bytes = serde_json::to_vec(&entry).map_err(|_| invalid())?.len() + 1;
        self.bytes = self.bytes.checked_add(bytes).ok_or_else(limit)?;
        if self.bytes > MAX_MANIFEST_BYTES {
            return Err(limit());
        }
        self.entries.push(entry);
        Ok(())
    }

    fn directory(&mut self, fd: &File, prefix: &str, depth: usize) -> Result<()> {
        within_time(self.start)?;
        let before = Identity::read(fd)?;
        for item in Dir::read_from(fd).map_err(io)? {
            within_time(self.start)?;
            let item = item.map_err(io)?;
            let name = item.file_name().to_str().map_err(|_| invalid())?;
            if matches!(name, "." | "..") {
                continue;
            }
            let relative = if prefix.is_empty() {
                name.to_owned()
            } else {
                format!("{prefix}/{name}")
            };
            if relative.len() > 4096 || depth >= MAX_DEPTH {
                return Err(limit());
            }
            let entry = observe(fd, name, relative.clone())?;
            let directory_identity =
                (entry.kind == Kind::Directory).then(|| entry.identity.clone());
            self.push(entry)?;
            if let Some(expected) = directory_identity {
                let nested = open_directory(fd, name)?;
                check(expected.unchanged(&Identity::read(&nested)?))?;
                self.directory(&nested, &relative, depth + 1)?;
                check(expected.unchanged(&observe(fd, name, relative)?.identity))?;
            }
        }
        check(before.unchanged(&Identity::read(fd)?))
    }
}

impl Inventory {
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn capture(source_path: &str) -> Result<Self> {
        (|| {
            validate_path(source_path)?;
            let start = Instant::now();
            let (_, basename) = parts(source_path)?;
            let (parent, ancestors) = parent_chain(source_path, None, start)?;
            let root = observe(&parent, basename, basename.to_owned())?;
            let envelope_bytes = serde_json::to_vec(&(source_path, &ancestors, &root))
                .map_err(|_| invalid())?
                .len();
            let mut walk = Walk {
                entries: Vec::new(),
                bytes: envelope_bytes + 2,
                start,
            };
            match root.kind {
                Kind::File => walk.push(root.clone())?,
                Kind::Directory => {
                    let fd = open_directory(&parent, basename)?;
                    check(root.identity.unchanged(&Identity::read(&fd)?))?;
                    walk.directory(&fd, "", 0)?;
                }
                _ => return Err(invalid()),
            }
            walk.entries
                .sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
            if root.kind == Kind::Directory
                && walk
                    .entries
                    .iter()
                    .any(|e| source_path.len() + 1 + e.relative_path.len() > 4096)
            {
                return Err(limit());
            }
            let inventory = Self {
                source_path: source_path.to_owned(),
                ancestors,
                root,
                entries: walk.entries,
            };
            inventory.verify_at(start)?;
            Ok(inventory)
        })()
        .map_err(|e: Error| e.at(PathRole::Source))
    }

    fn source_parent(&self, start: Instant) -> Result<File> {
        Ok(parent_chain(&self.source_path, Some(&self.ancestors), start)?.0)
    }

    fn entry_parent(&self, entry: &Entry, start: Instant) -> Result<(File, String)> {
        let parent = self.source_parent(start)?;
        let (_, root_name) = parts(&self.source_path)?;
        if self.root.kind == Kind::File {
            return Ok((parent, root_name.to_owned()));
        }
        let mut current = open_directory(&parent, root_name)?;
        check(self.root.identity.same(&Identity::read(&current)?))?;
        let mut components = entry.relative_path.split('/').peekable();
        let mut prefix = String::new();
        while let Some(name) = components.next() {
            within_time(start)?;
            if components.peek().is_none() {
                return Ok((current, name.to_owned()));
            }
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(name);
            let index = self
                .entries
                .binary_search_by(|e| e.relative_path.as_str().cmp(&prefix))
                .map_err(|_| invalid())?;
            let expected = &self.entries[index];
            check(expected.kind == Kind::Directory)?;
            current = open_directory(&current, name)?;
            check(expected.identity.same(&Identity::read(&current)?))?;
        }
        Err(invalid())
    }

    fn verify_at(&self, start: Instant) -> Result<()> {
        for entry in &self.entries {
            within_time(start)?;
            let (parent, name) = self.entry_parent(entry, start)?;
            if entry.kind == Kind::File {
                let file = child(&parent, &name, OFlags::RDONLY)?;
                check(entry.identity.unchanged(&Identity::read(&file)?))?;
            }
            let current = observe(&parent, &name, entry.relative_path.clone())?;
            check(entry.kind == current.kind && entry.identity.unchanged(&current.identity))?;
        }
        let parent = self.source_parent(start)?;
        let (_, basename) = parts(&self.source_path)?;
        let current = observe(&parent, basename, self.root.relative_path.clone())?;
        check(self.root.kind == current.kind && self.root.identity.unchanged(&current.identity))?;
        within_time(start)
    }

    pub fn verify_before_publish(&self) -> Result<()> {
        self.verify_at(Instant::now())
            .map_err(|e| e.at(PathRole::Source))
    }

    /// Holds the observed inode for probing; callers must check its identity again afterwards.
    pub fn open_file(&self, index: usize) -> Result<File> {
        (|| {
            let start = Instant::now();
            let entry = self.entries.get(index).ok_or_else(invalid)?;
            check(entry.kind == Kind::File)?;
            let (parent, name) = self.entry_parent(entry, start)?;
            let file = child(&parent, &name, OFlags::RDONLY)?;
            self.verify_open_file_at(index, &file, start)?;
            Ok(file)
        })()
        .map_err(|e: Error| e.at(PathRole::Source))
    }

    pub fn verify_open_file(&self, index: usize, file: &File) -> Result<()> {
        self.verify_open_file_at(index, file, Instant::now())
    }

    fn verify_open_file_at(&self, index: usize, file: &File, start: Instant) -> Result<()> {
        (|| {
            within_time(start)?;
            let entry = self.entries.get(index).ok_or_else(invalid)?;
            check(entry.kind == Kind::File && entry.identity.unchanged(&Identity::read(file)?))?;
            let (parent, name) = self.entry_parent(entry, start)?;
            let current = observe(&parent, &name, entry.relative_path.clone())?;
            check(current.kind == Kind::File && entry.identity.unchanged(&current.identity))?;
            within_time(start)
        })()
        .map_err(|e: Error| e.at(PathRole::Source))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::{
            ffi::OsStringExt,
            fs::{PermissionsExt, symlink},
        },
        path::PathBuf,
    };

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("hrrdarr-scan-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn path(&self, relative: &str) -> PathBuf {
            self.0.join(relative)
        }
        fn capture(&self, relative: &str) -> Result<Inventory> {
            Inventory::capture(self.path(relative).to_str().unwrap())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn complete_inventory_preserves_aliases_and_non_media_entries_without_following_them() {
        let f = Fixture::new();
        fs::create_dir(f.path("download")).unwrap();
        fs::create_dir(f.path("download/nested")).unwrap();
        fs::write(f.path("download/nested/tv.mkv"), b"tv bytes").unwrap();
        fs::hard_link(
            f.path("download/nested/tv.mkv"),
            f.path("download/alias.mkv"),
        )
        .unwrap();
        fs::write(f.path("outside.mkv"), b"private bytes").unwrap();
        symlink(f.path("outside.mkv"), f.path("download/link.mkv")).unwrap();
        symlink("missing", f.path("download/dangling")).unwrap();
        let socket = std::os::unix::net::UnixListener::bind(f.path("download/socket")).unwrap();
        let inventory = f.capture("download").unwrap();
        assert_eq!(
            inventory
                .entries()
                .iter()
                .map(|e| e.relative_path.as_str())
                .collect::<Vec<_>>(),
            [
                "alias.mkv",
                "dangling",
                "link.mkv",
                "nested",
                "nested/tv.mkv",
                "socket"
            ]
        );
        assert_eq!(
            inventory.entries[0].physical_key(),
            inventory.entries[4].physical_key()
        );
        assert_eq!(inventory.entries[1].kind, Kind::Symlink);
        assert_eq!(inventory.entries[5].kind, Kind::Other);
        for index in [1, 2, 3, 5] {
            assert!(inventory.open_file(index).is_err());
        }
        let mut file = inventory.open_file(4).unwrap();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"tv bytes");
        let evidence: serde_json::Value =
            serde_json::from_str(&inventory.entries[4].identity_json().unwrap()).unwrap();
        assert_eq!(evidence["version"], 1);
        assert_eq!(evidence["size"], 8);
        assert_eq!(inventory.entries[4].size(), 8);
        inventory.verify_open_file(4, &file).unwrap();
        inventory.verify_before_publish().unwrap();
        drop(socket);
    }

    #[test]
    fn publication_detects_additions_and_source_changes_and_probe_detects_path_replacement() {
        let f = Fixture::new();
        fs::create_dir(f.path("download")).unwrap();
        fs::write(f.path("download/movie.mkv"), b"movie").unwrap();
        let original = f.capture("download").unwrap();
        fs::write(f.path("download/extra.txt"), b"retained").unwrap();
        assert_eq!(
            original.verify_before_publish().unwrap_err().code(),
            "identity_changed"
        );
        let inventory = f.capture("download/movie.mkv").unwrap();
        let held = inventory.open_file(0).unwrap();
        fs::rename(f.path("download/movie.mkv"), f.path("download/old.mkv")).unwrap();
        fs::write(f.path("download/movie.mkv"), b"other").unwrap();
        assert!(inventory.verify_open_file(0, &held).is_err());
        assert!(inventory.open_file(0).is_err());
        let current = f.capture("download/movie.mkv").unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(f.path("download/movie.mkv"))
            .unwrap()
            .set_len(10)
            .unwrap();
        assert!(current.verify_before_publish().is_err());
    }

    #[test]
    fn replaced_ancestors_fail_even_when_original_source_parent_is_moved_back() {
        let f = Fixture::new();
        fs::create_dir_all(f.path("ancestor/download")).unwrap();
        fs::write(f.path("ancestor/download/movie.mkv"), b"movie").unwrap();
        let inventory = f.capture("ancestor/download").unwrap();
        fs::rename(f.path("ancestor"), f.path("old-ancestor")).unwrap();
        fs::create_dir(f.path("ancestor")).unwrap();
        fs::rename(f.path("old-ancestor/download"), f.path("ancestor/download")).unwrap();
        assert!(inventory.verify_before_publish().is_err());
        assert!(inventory.open_file(0).is_err());
    }

    #[test]
    fn empty_and_zero_byte_sources_are_observed_but_invalid_paths_and_non_utf8_fail() {
        let f = Fixture::new();
        fs::create_dir(f.path("empty")).unwrap();
        assert!(f.capture("empty").unwrap().entries().is_empty());
        fs::write(f.path("zero.mkv"), []).unwrap();
        assert_eq!(f.capture("zero.mkv").unwrap().entries()[0].size(), 0);
        symlink(f.path("empty"), f.path("link")).unwrap();
        assert!(f.capture("link").is_err());
        assert!(f.capture("link/file").is_err());
        assert!(f.capture("empty/../zero.mkv").is_err());
        assert!(Inventory::capture("relative.mkv").is_err());
        assert!(Inventory::capture("/").is_err());
        let bad_name = std::ffi::OsString::from_vec(vec![0xff]);
        fs::write(f.path("empty").join(bad_name), b"bad name").unwrap();
        assert!(f.capture("empty").is_err());
    }

    #[test]
    fn unreadable_directory_is_failure_not_empty() {
        let f = Fixture::new();
        fs::create_dir_all(f.path("download/private")).unwrap();
        fs::write(f.path("download/private/movie.mkv"), b"movie").unwrap();
        fs::set_permissions(f.path("download/private"), fs::Permissions::from_mode(0)).unwrap();
        let actual_denial = fs::read_dir(f.path("download/private")).is_err();
        let result = f.capture("download");
        fs::set_permissions(
            f.path("download/private"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        if actual_denial {
            assert_eq!(
                result
                    .unwrap_err()
                    .path_kind
                    .map(|k| matches!(k, PathKind::Denied)),
                Some(true)
            );
        } else {
            // Privileged test runners can read mode000; that does not establish denied-access behavior.
            assert_eq!(result.unwrap().entries().len(), 2);
        }
    }

    #[test]
    fn traversal_depth_entry_bytes_and_elapsed_limits_are_closed_boundaries() {
        let f = Fixture::new();
        let mut path = f.path("download");
        fs::create_dir(&path).unwrap();
        for _ in 0..MAX_DEPTH {
            path.push("d");
            fs::create_dir(&path).unwrap();
        }
        assert_eq!(f.capture("download").unwrap().entries().len(), MAX_DEPTH);
        fs::write(path.join("overflow.mkv"), b"too deep").unwrap();
        assert_eq!(
            f.capture("download").unwrap_err().code(),
            "scan_inventory_limit"
        );
        fs::write(f.path("one.mkv"), b"one").unwrap();
        let entry = f.capture("one.mkv").unwrap().entries.remove(0);
        let mut walk = Walk {
            entries: Vec::new(),
            bytes: 2,
            start: Instant::now(),
        };
        for _ in 0..MAX_ENTRIES {
            walk.push(entry.clone()).unwrap();
        }
        assert_eq!(
            walk.push(entry.clone()).unwrap_err().code(),
            "scan_inventory_limit"
        );
        let encoded = serde_json::to_vec(&entry).unwrap().len() + 1;
        let mut boundary = Walk {
            entries: Vec::new(),
            bytes: MAX_MANIFEST_BYTES - encoded,
            start: Instant::now(),
        };
        boundary.push(entry.clone()).unwrap();
        assert_eq!(boundary.bytes, MAX_MANIFEST_BYTES);
        assert_eq!(
            boundary.push(entry.clone()).unwrap_err().code(),
            "scan_inventory_limit"
        );
        let mut expired = Walk {
            entries: Vec::new(),
            bytes: 2,
            start: Instant::now() - OBSERVATION_LIMIT,
        };
        assert_eq!(
            expired.push(entry).unwrap_err().code(),
            "scan_inventory_limit"
        );
    }

    #[test]
    fn expired_observation_stops_ancestor_replay_and_file_revalidation() {
        let f = Fixture::new();
        fs::create_dir_all(f.path("download/nested")).unwrap();
        fs::write(f.path("download/nested/movie.mkv"), b"movie").unwrap();
        let inventory = f.capture("download").unwrap();
        let file = inventory.open_file(1).unwrap();
        let expired = Instant::now() - OBSERVATION_LIMIT;
        assert_eq!(
            parent_chain("/missing/ancestor/file", None, expired)
                .unwrap_err()
                .code(),
            "scan_inventory_limit"
        );
        assert_eq!(
            inventory
                .entry_parent(&inventory.entries[1], expired)
                .unwrap_err()
                .code(),
            "scan_inventory_limit"
        );
        assert_eq!(
            inventory
                .verify_open_file_at(1, &file, expired)
                .unwrap_err()
                .code(),
            "scan_inventory_limit"
        );
        assert_eq!(
            inventory.verify_at(expired).unwrap_err().code(),
            "scan_inventory_limit"
        );
    }

    fn saved(inventory: &Inventory) -> (String, Vec<String>) {
        (
            inventory.envelope_json(MediaDomain::Tv).unwrap(),
            inventory
                .entries()
                .iter()
                .map(|e| e.durable_json().unwrap())
                .collect(),
        )
    }
    fn restore_at(f: &Fixture, path: &str, evidence: &(String, Vec<String>)) -> Result<Inventory> {
        let rows: Vec<&[u8]> = evidence.1.iter().map(|s| s.as_bytes()).collect();
        Inventory::restore(
            MediaDomain::Tv,
            f.path(path).to_str().unwrap(),
            evidence.0.as_bytes(),
            &rows,
        )
    }

    #[test]
    fn durable_roundtrip_retains_full_historical_evidence_for_files_and_folders() {
        let f = Fixture::new();
        fs::create_dir_all(f.path("download/nested")).unwrap();
        fs::write(f.path("download/nested/movie.mkv"), b"movie").unwrap();
        symlink("missing", f.path("download/alias")).unwrap();
        for path in ["download", "download/nested/movie.mkv"] {
            let original = f.capture(path).unwrap();
            let evidence = saved(&original);
            let restored = restore_at(&f, path, &evidence).unwrap();
            assert_eq!(saved(&restored), evidence);
            restored.verify_before_publish().unwrap();
            let index = restored
                .entries
                .iter()
                .position(|e| e.kind == Kind::File)
                .unwrap();
            let mut bytes = Vec::new();
            restored
                .open_file(index)
                .unwrap()
                .read_to_end(&mut bytes)
                .unwrap();
            assert_eq!(bytes, b"movie");
            assert_eq!(
                restored.source_path(index).unwrap(),
                f.path("download/nested/movie.mkv").to_str().unwrap()
            );
        }
    }

    #[test]
    fn restart_after_each_completed_move_does_not_adopt_missing_or_replaced_sources() {
        let f = Fixture::new();
        fs::create_dir_all(f.path("download/nested")).unwrap();
        fs::write(f.path("download/nested/a.mkv"), b"first").unwrap();
        fs::write(f.path("download/nested/b.mkv"), b"second").unwrap();
        let original = f.capture("download").unwrap();
        let evidence = saved(&original);
        original.open_file(1).unwrap();
        fs::rename(f.path("download/nested/a.mkv"), f.path("imported-a.mkv")).unwrap();
        let restored = restore_at(&f, "download", &evidence).unwrap();
        assert!(restored.open_file(1).is_err());
        restored.open_file(2).unwrap();
        // A legitimate preceding move changes parent timestamps, not its ownership identity.
        assert!(restored.verify_before_publish().is_err());
        fs::rename(f.path("download/nested/b.mkv"), f.path("imported-b.mkv")).unwrap();
        let complete = restore_at(&f, "download", &evidence).unwrap();
        assert!(complete.open_file(2).is_err());
        assert_eq!(saved(&complete), evidence);
        // A file-command root can itself already be moved; restore remains read-only.
        let file_evidence = saved(&f.capture("imported-a.mkv").unwrap());
        fs::rename(f.path("imported-a.mkv"), f.path("settled-a.mkv")).unwrap();
        let moved_file = restore_at(&f, "imported-a.mkv", &file_evidence).unwrap();
        assert!(moved_file.open_file(0).is_err());
        fs::write(f.path("download/nested/b.mkv"), b"second").unwrap();
        assert!(complete.open_file(2).is_err());
    }

    #[test]
    fn restart_rejects_replaced_outer_ancestors_and_targeted_nested_directories() {
        let f = Fixture::new();
        fs::create_dir_all(f.path("ancestor/download/nested")).unwrap();
        fs::write(f.path("ancestor/download/nested/movie.mkv"), b"movie").unwrap();
        let evidence = saved(&f.capture("ancestor/download").unwrap());
        fs::rename(f.path("ancestor"), f.path("old-ancestor")).unwrap();
        fs::create_dir(f.path("ancestor")).unwrap();
        fs::rename(f.path("old-ancestor/download"), f.path("ancestor/download")).unwrap();
        assert_eq!(
            restore_at(&f, "ancestor/download", &evidence)
                .unwrap_err()
                .code(),
            "identity_changed"
        );
        let evidence = saved(&f.capture("ancestor/download").unwrap());
        fs::rename(f.path("ancestor/download/nested"), f.path("old-nested")).unwrap();
        fs::create_dir(f.path("ancestor/download/nested")).unwrap();
        fs::rename(
            f.path("old-nested/movie.mkv"),
            f.path("ancestor/download/nested/movie.mkv"),
        )
        .unwrap();
        let restored = restore_at(&f, "ancestor/download", &evidence).unwrap();
        assert_eq!(
            restored.open_file(1).unwrap_err().code(),
            "identity_changed"
        );
    }

    #[test]
    fn durable_input_rejects_wrong_scope_shapes_bounds_and_incomplete_structure() {
        let f = Fixture::new();
        fs::create_dir_all(f.path("download/nested")).unwrap();
        fs::write(f.path("download/nested/movie.mkv"), b"movie").unwrap();
        let evidence = saved(&f.capture("download").unwrap());
        let rows: Vec<&[u8]> = evidence.1.iter().map(|s| s.as_bytes()).collect();
        let path = f.path("download");
        let path = path.to_str().unwrap();
        assert!(
            Inventory::restore(MediaDomain::Movies, path, evidence.0.as_bytes(), &rows).is_err()
        );
        assert!(
            Inventory::restore(MediaDomain::Tv, "/other", evidence.0.as_bytes(), &rows).is_err()
        );
        assert!(Inventory::restore(MediaDomain::Tv, path, &[b' '; 16 * 1024 + 1], &rows).is_err());
        assert!(
            Inventory::restore(
                MediaDomain::Tv,
                path,
                evidence.0.as_bytes(),
                &vec![rows[0]; MAX_ENTRIES + 1]
            )
            .is_err()
        );
        let oversized = vec![b' '; 32 * 1024 + 1];
        assert!(
            Inventory::restore(MediaDomain::Tv, path, evidence.0.as_bytes(), &[&oversized])
                .is_err()
        );
        let large = vec![b' '; 32 * 1024];
        assert!(
            Inventory::restore(
                MediaDomain::Tv,
                path,
                evidence.0.as_bytes(),
                &vec![large.as_slice(); 2048]
            )
            .is_err()
        );
        for (pointer, value) in [
            ("/version", serde_json::json!(2)),
            ("/ancestor_count", serde_json::json!(0)),
            ("/ancestor_digest", serde_json::json!("bad")),
            ("/entry_count", serde_json::json!(1)),
            ("/root/kind", serde_json::json!("symlink")),
            ("/root/relative_path", serde_json::json!("other")),
            ("/root/identity/mtime_ns", serde_json::json!(1_000_000_000)),
            ("/root/identity/ctime_ns", serde_json::json!(-1)),
            ("/root/identity/size", serde_json::json!(-1)),
            ("/root/identity/ino", serde_json::json!(1.5)),
        ] {
            let mut envelope: serde_json::Value = serde_json::from_str(&evidence.0).unwrap();
            *envelope.pointer_mut(pointer).unwrap() = value;
            let altered = (envelope.to_string(), evidence.1.clone());
            assert!(restore_at(&f, "download", &altered).is_err(), "{pointer}");
        }
        for relative in [
            "../movie.mkv",
            "nested//movie.mkv",
            "/movie.mkv",
            "nested/./movie.mkv",
            "missing/movie.mkv",
        ] {
            let mut altered = evidence.clone();
            let mut row: serde_json::Value = serde_json::from_str(&altered.1[1]).unwrap();
            row["relative_path"] = relative.into();
            altered.1[1] = row.to_string();
            assert!(restore_at(&f, "download", &altered).is_err(), "{relative}");
        }
        let mut altered = evidence.clone();
        altered.1.swap(0, 1);
        assert!(restore_at(&f, "download", &altered).is_err());
        altered.1[1] = altered.1[0].clone();
        assert!(restore_at(&f, "download", &altered).is_err());
        let file = saved(&f.capture("download/nested/movie.mkv").unwrap());
        let mut altered = file.clone();
        let mut row: serde_json::Value = serde_json::from_str(&altered.1[0]).unwrap();
        row["identity"]["unexpected"] = true.into();
        altered.1[0] = row.to_string();
        assert!(restore_at(&f, "download/nested/movie.mkv", &altered).is_err());
        let mut altered = file;
        let mut row: serde_json::Value = serde_json::from_str(&altered.1[0]).unwrap();
        row["identity"]["size"] = 100.into();
        altered.1[0] = row.to_string();
        assert!(restore_at(&f, "download/nested/movie.mkv", &altered).is_err());
    }

    #[test]
    fn file_input_siblings_are_complete_read_only_and_change_cancel_deadline_checked() {
        let f = Fixture::new();
        fs::create_dir_all(f.path("download/nested")).unwrap();
        fs::write(f.path("download/movie.part1.mkv"), b"movie").unwrap();
        fs::write(f.path("download/movie.part2.bin"), b"all extensions").unwrap();
        fs::write(
            f.path("download/nested/not-immediate.mkv"),
            b"outside sibling scope",
        )
        .unwrap();
        symlink("movie.part1.mkv", f.path("download/link")).unwrap();
        let inventory = f.capture("download/movie.part1.mkv").unwrap();
        let cancel = AtomicBool::new(false);
        let deadline = Instant::now() + OBSERVATION_LIMIT;
        let siblings = inventory.observe_siblings(0, deadline, &cancel).unwrap();
        assert_eq!(
            siblings
                .entries()
                .iter()
                .map(|e| (e.relative_path.as_str(), e.kind))
                .collect::<Vec<_>>(),
            [
                ("link", Kind::Symlink),
                ("movie.part1.mkv", Kind::File),
                ("movie.part2.bin", Kind::File),
                ("nested", Kind::Directory)
            ]
        );
        assert_eq!(inventory.entries().len(), 1);
        let folder = f.capture("download").unwrap();
        let selected = folder
            .entries()
            .iter()
            .position(|e| e.relative_path == "movie.part1.mkv")
            .unwrap();
        let folder_siblings = folder
            .observe_siblings(selected, deadline, &cancel)
            .unwrap();
        assert_eq!(
            folder_siblings
                .entries()
                .iter()
                .map(|e| &e.relative_path)
                .collect::<Vec<_>>(),
            siblings
                .entries()
                .iter()
                .map(|e| &e.relative_path)
                .collect::<Vec<_>>()
        );
        inventory
            .verify_siblings(0, &siblings, deadline, &cancel)
            .unwrap();
        fs::write(f.path("download/new.txt"), b"new").unwrap();
        assert!(
            inventory
                .verify_siblings(0, &siblings, deadline, &cancel)
                .is_err()
        );
        let updated = inventory.observe_siblings(0, deadline, &cancel).unwrap();
        assert_eq!(updated.entries().len(), 5);
        cancel.store(true, Ordering::Release);
        assert_eq!(
            inventory
                .observe_siblings(0, deadline, &cancel)
                .unwrap_err()
                .code(),
            "scan_cancelled"
        );
        assert!(
            inventory
                .verify_siblings(0, &updated, deadline, &cancel)
                .is_err()
        );
        cancel.store(false, Ordering::Release);
        assert!(
            inventory
                .observe_siblings(0, Instant::now(), &cancel)
                .is_err()
        );
        fs::rename(f.path("download/movie.part1.mkv"), f.path("original.mkv")).unwrap();
        fs::write(f.path("download/movie.part1.mkv"), b"movie").unwrap();
        assert!(inventory.observe_siblings(0, deadline, &cancel).is_err());
    }
}
