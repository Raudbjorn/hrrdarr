//! hc.035 MountCheck: a mount backing an existing series/movie folder must not be read-only.
//! Linux reads the process's own `/proc/self/mountinfo`; other platforms degrade to an explicit
//! Notice. Item folders are resolved with read-only metadata calls; nothing is written or probed.
use super::{
    HealthIdentity, HealthIssue, HealthScope, HealthSeverity,
    library_roots::{
        LISTED_GROUPS, Limits, PATH_BYTES, SAMPLES, Stop, TITLE_BYTES, bounded_message, display,
        run_bounded,
    },
};
use crate::db::Database;
use libsql::{TransactionBehavior, params};
use std::{
    collections::{BTreeMap, HashMap},
    ffi::OsString,
    io,
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::Semaphore;

/// Bounds on the kernel mount table; a larger table is reported, never partially trusted.
pub(super) const MOUNTINFO_BYTES: usize = 4 << 20;
pub(super) const MOUNTINFO_LINES: usize = 65_536;
const FSTYPE_BYTES: usize = 32;
const TABLE_BUDGET: Duration = Duration::from_secs(1);
static MOUNT_PROBES: Semaphore = Semaphore::const_new(2);

/// Injectable source of `mountinfo`-format text (proc(5)); tests supply fixed samples.
pub(super) trait MountSource: Send + Sync {
    fn read(&self, max_bytes: usize) -> io::Result<Vec<u8>>;
}
pub(super) struct ProcSelfMountInfo;
impl MountSource for ProcSelfMountInfo {
    #[cfg(target_os = "linux")]
    fn read(&self, max_bytes: usize) -> io::Result<Vec<u8>> {
        use std::io::Read;
        let mut bytes = Vec::new();
        std::fs::File::open("/proc/self/mountinfo")?
            .take(max_bytes as u64 + 1)
            .read_to_end(&mut bytes)?;
        Ok(bytes)
    }
    #[cfg(not(target_os = "linux"))]
    fn read(&self, _: usize) -> io::Result<Vec<u8>> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Mount {
    pub mount_point: PathBuf,
    pub fstype: String,
    pub read_only: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TableError {
    Unsupported,
    Unreadable,
    TooLarge,
    Malformed,
}

/// proc(5) escapes space, tab, newline and backslash as three-digit octal (`\040`).
fn unescape(field: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(field.len());
    let mut index = 0;
    while index < field.len() {
        if field[index] == b'\\'
            && index + 3 < field.len()
            && field[index + 1..=index + 3]
                .iter()
                .all(|b| (b'0'..=b'7').contains(b))
        {
            let value = field[index + 1..=index + 3]
                .iter()
                .fold(0u16, |acc, b| acc * 8 + u16::from(b - b'0'));
            if let Ok(byte) = u8::try_from(value) {
                out.push(byte);
                index += 4;
                continue;
            }
        }
        out.push(field[index]);
        index += 1;
    }
    out
}

fn has_option(options: &[u8], name: &[u8]) -> bool {
    options.split(|b| *b == b',').any(|option| option == name)
}

/// Parses `/proc/<pid>/mountinfo`. Read-only is the per-mount `ro` flag (bind mounts can be
/// read-only over a writable superblock) or a read-only superblock.
pub(super) fn parse(bytes: &[u8]) -> Result<Vec<Mount>, TableError> {
    if bytes.len() > MOUNTINFO_BYTES {
        return Err(TableError::TooLarge);
    }
    let mut mounts = Vec::new();
    for line in bytes.split(|b| *b == b'\n').filter(|line| !line.is_empty()) {
        if mounts.len() == MOUNTINFO_LINES {
            return Err(TableError::TooLarge);
        }
        let fields: Vec<&[u8]> = line.split(|b| *b == b' ').collect();
        let separator = fields
            .iter()
            .skip(6)
            .position(|field| *field == b"-")
            .map(|index| index + 6)
            .ok_or(TableError::Malformed)?;
        if fields.len() < separator + 4 || !fields[4].starts_with(b"/") {
            return Err(TableError::Malformed);
        }
        let fstype = String::from_utf8_lossy(&unescape(fields[separator + 1])).into_owned();
        mounts.push(Mount {
            mount_point: PathBuf::from(OsString::from_vec(unescape(fields[4]))),
            fstype: display(&fstype, FSTYPE_BYTES),
            read_only: has_option(fields[5], b"ro") || has_option(fields[separator + 3], b"ro"),
        });
    }
    if mounts.is_empty() {
        return Err(TableError::Malformed);
    }
    Ok(mounts)
}

/// Mount point → index of the last (topmost) mount at that point.
pub(super) struct Table {
    mounts: Vec<Mount>,
    by_point: HashMap<PathBuf, usize>,
}
impl Table {
    pub(super) fn new(mounts: Vec<Mount>) -> Self {
        let by_point = mounts
            .iter()
            .enumerate()
            .map(|(index, mount)| (mount.mount_point.clone(), index))
            .collect();
        Self { mounts, by_point }
    }
    /// Longest mount-point prefix on component boundaries; equal points resolve to the topmost.
    pub(super) fn lookup(&self, path: &Path) -> Option<usize> {
        path.ancestors()
            .find_map(|ancestor| self.by_point.get(ancestor).copied())
    }
    pub(super) fn get(&self, index: usize) -> &Mount {
        &self.mounts[index]
    }
}

/// Resolves an existing item folder to its real location with read-only metadata calls. Parent
/// canonicalization is cached; only a symlinked item folder needs its own canonicalization.
fn resolve(path: &str, parents: &mut HashMap<PathBuf, Option<PathBuf>>) -> Option<PathBuf> {
    let path = Path::new(path);
    let meta = std::fs::symlink_metadata(path).ok()?;
    if meta.file_type().is_symlink() {
        let real = std::fs::canonicalize(path).ok()?;
        return std::fs::metadata(&real)
            .ok()
            .filter(|meta| meta.is_dir())
            .map(|_| real);
    }
    if !meta.is_dir() {
        return None;
    }
    let (parent, name) = (path.parent()?, path.file_name()?);
    parents
        .entry(parent.to_path_buf())
        .or_insert_with(|| std::fs::canonicalize(parent).ok())
        .as_ref()
        .map(|real| real.join(name))
}

#[derive(Default)]
struct Group {
    items: u64,
    samples: Vec<String>,
}

pub(super) async fn evaluate(
    db: &Database,
    identity: &HealthIdentity,
) -> Result<Option<HealthIssue>, &'static str> {
    evaluate_with(db, identity, Arc::new(ProcSelfMountInfo), &Limits::DEFAULT).await
}

fn storage<E>(_: E) -> &'static str {
    "storage_error"
}

pub(super) async fn evaluate_with(
    db: &Database,
    identity: &HealthIdentity,
    source: Arc<dyn MountSource>,
    limits: &Limits,
) -> Result<Option<HealthIssue>, &'static str> {
    let tv = match (identity.scope, identity.check_key.as_str()) {
        (HealthScope::Tv, "mount_read_only") => true,
        (HealthScope::Movies, "mount_read_only") => false,
        _ => return Err("check_failed"),
    };
    let noun = |count: u64| match (tv, count) {
        (true, _) => "series",
        (false, 1) => "movie",
        (false, _) => "movies",
    };
    let (count_sql, rows_sql) = if tv {
        (
            "SELECT count(*) FROM series",
            "SELECT substr(path,1,4097),substr(title,1,?) FROM series ORDER BY id LIMIT ?",
        )
    } else {
        (
            "SELECT count(*) FROM movies",
            "SELECT substr(m.path,1,4097),substr(mm.title,1,?) FROM movies m LEFT JOIN movie_metadata mm ON mm.id=m.metadata_id ORDER BY m.id LIMIT ?",
        )
    };
    let c = db.connect().await.map_err(storage)?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::ReadOnly)
        .await
        .map_err(storage)?;
    let total: i64 = tx
        .query(count_sql, ())
        .await
        .map_err(storage)?
        .next()
        .await
        .map_err(storage)?
        .ok_or("check_failed")?
        .get(0)
        .map_err(storage)?;
    let limit = i64::try_from(limits.items).map_err(|_| "check_failed")?;
    let mut rows = tx
        .query(rows_sql, params![TITLE_BYTES as i64 + 1, limit])
        .await
        .map_err(storage)?;
    let mut items: Vec<(String, String)> = Vec::new();
    let mut bytes = 0usize;
    while let Some(row) = rows.next().await.map_err(storage)? {
        let path: String = row.get(0).map_err(storage)?;
        let title: Option<String> = row.get(1).map_err(storage)?;
        let title = display(title.as_deref().unwrap_or("(untitled)"), TITLE_BYTES);
        bytes += path.len() + title.len();
        if bytes > limits.item_bytes {
            break;
        }
        // Invalid paths are the root-folder check's concern; they have no folder to resolve.
        if let Some(path) = crate::library::normalized_path(&path) {
            items.push((path, title));
        } else {
            items.push((String::new(), title));
        }
    }
    drop(rows);
    tx.commit().await.map_err(storage)?;
    drop(c);
    let total = u64::try_from(total).map_err(|_| "check_failed")?;
    let mut unevaluated = total.saturating_sub(items.len() as u64);
    if items.is_empty() {
        return Ok(notice(identity, tv, unevaluated, noun));
    }
    let table = {
        let job = tokio::task::spawn_blocking(move || {
            source
                .read(MOUNTINFO_BYTES)
                .map_err(|error| {
                    if error.kind() == io::ErrorKind::Unsupported {
                        TableError::Unsupported
                    } else {
                        TableError::Unreadable
                    }
                })
                .and_then(|bytes| parse(&bytes))
        });
        match tokio::time::timeout(TABLE_BUDGET, job).await {
            Ok(Ok(table)) => table,
            Ok(Err(_)) => return Err("check_failed"),
            Err(_) => Err(TableError::Unreadable),
        }
    };
    let table = match table {
        Ok(mounts) => Arc::new(Table::new(mounts)),
        Err(error) => return Ok(Some(unavailable(identity, tv, error))),
    };
    let paths: Vec<String> = items.iter().map(|(path, _)| path.clone()).collect();
    let lookup = table.clone();
    let mut parents = HashMap::new();
    let (found, stop) = run_bounded(&MOUNT_PROBES, paths, limits.budget, move |path: &String| {
        if path.is_empty() {
            return None;
        }
        resolve(path, &mut parents).and_then(|real| lookup.lookup(&real))
    })
    .await?;
    if stop != Stop::Complete {
        unevaluated += (items.len() - found.len()) as u64;
    }
    let mut groups: BTreeMap<usize, Group> = BTreeMap::new();
    let mut overflow_mounts = std::collections::HashSet::new();
    for (index, mount) in found.iter().enumerate() {
        let Some(mount) = *mount else { continue };
        if !table.get(mount).read_only {
            continue;
        }
        if !groups.contains_key(&mount) && groups.len() >= limits.groups {
            overflow_mounts.insert(mount);
            continue;
        }
        let group = groups.entry(mount).or_default();
        group.items += 1;
        if group.samples.len() < SAMPLES {
            group.samples.push(items[index].1.clone());
        }
    }
    if groups.is_empty() {
        return Ok(notice(identity, tv, unevaluated, noun));
    }
    let mut groups: Vec<(String, &Mount, Group)> = groups
        .into_iter()
        .map(|(index, group)| {
            let mount = table.get(index);
            (
                display(
                    &String::from_utf8_lossy(mount.mount_point.as_os_str().as_bytes()),
                    PATH_BYTES,
                ),
                mount,
                group,
            )
        })
        .collect();
    groups.sort_by(|a, b| a.0.cmp(&b.0));
    let listed: Vec<String> = groups
        .iter()
        .take(LISTED_GROUPS)
        .map(|(point, mount, group)| {
            format!(
                "{point} ({}; {} {}, e.g. {})",
                mount.fstype,
                group.items,
                noun(group.items),
                group.samples.join(", ")
            )
        })
        .collect();
    let more = groups.len().saturating_sub(LISTED_GROUPS) + overflow_mounts.len();
    let more = if more > 0 {
        format!(" | {more} more read-only mounts not listed")
    } else {
        String::new()
    };
    let bounded = if unevaluated > 0 {
        format!(
            " Evaluation was bounded: {unevaluated} {} were not evaluated.",
            noun(unevaluated)
        )
    } else {
        String::new()
    };
    let (prefix, plural) = if tv {
        (
            "Mount containing a series path is mounted read-only: ",
            "series",
        )
    } else {
        (
            "Mount containing a movie path is mounted read-only: ",
            "movies",
        )
    };
    Ok(Some(HealthIssue {
        identity: identity.clone(),
        severity: HealthSeverity::Error,
        reason: "mount_read_only".into(),
        message: bounded_message(format!(
            "{prefix}{}{more}. Hrrdarr cannot import, rename or delete media there; remount it read-write or move the affected {plural}.{bounded}",
            listed.join(" | ")
        )),
        wiki_url: wiki(tv).into(),
        compatibility_type: "MountCheck".into(),
    }))
}

fn wiki(tv: bool) -> &'static str {
    if tv {
        "https://wiki.servarr.com/sonarr/system#series-mount-ro"
    } else {
        "https://wiki.servarr.com/radarr/system#movies-mount-ro"
    }
}

fn notice(
    identity: &HealthIdentity,
    tv: bool,
    unevaluated: u64,
    noun: impl Fn(u64) -> &'static str,
) -> Option<HealthIssue> {
    (unevaluated > 0).then(|| HealthIssue {
        identity: identity.clone(),
        severity: HealthSeverity::Notice,
        reason: "mount_check_truncated".into(),
        message: format!(
            "No read-only mount was found in the evaluated part of the library. Evaluation was bounded: {unevaluated} {} were not evaluated.",
            noun(unevaluated)
        ),
        wiki_url: wiki(tv).into(),
        compatibility_type: "MountCheck".into(),
    })
}

fn unavailable(identity: &HealthIdentity, tv: bool, error: TableError) -> HealthIssue {
    let detail = match error {
        TableError::Unsupported => "is not supported on this platform",
        TableError::Unreadable => "is unavailable: the process mount table could not be read",
        TableError::TooLarge => "is unavailable: the process mount table exceeds its bounds",
        TableError::Malformed => "is unavailable: the process mount table could not be parsed",
    };
    HealthIssue {
        identity: identity.clone(),
        severity: HealthSeverity::Notice,
        reason: "mount_table_unavailable".into(),
        message: format!(
            "Read-only mount detection {detail}. Library folders were not checked for read-only mounts."
        ),
        wiki_url: wiki(tv).into(),
        compatibility_type: "MountCheck".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real-format lines (proc(5)); IDs and devices are arbitrary.
    const SAMPLE: &str = "\
22 1 259:2 / / rw,relatime shared:1 - ext4 /dev/nvme0n1p2 rw
36 22 0:32 / /mnt/media rw,noatime shared:20 - nfs4 server:/export rw,vers=4.2
37 36 0:33 / /mnt/media/tv ro,noatime shared:21 - nfs4 server:/tv rw,vers=4.2
38 22 259:3 /srv/library /mnt/bind ro,relatime shared:22 - ext4 /dev/nvme0n1p3 rw
39 22 0:34 / /mnt/My\\040Movies rw,relatime shared:23 master:7 - cifs //nas/movies ro,vers=3.1.1
40 22 0:35 / /mnt/stack rw,relatime - tmpfs tmpfs rw
41 22 0:36 / /mnt/stack ro,relatime - tmpfs tmpfs rw
";

    #[test]
    fn parses_escapes_bind_mounts_and_read_only_flags() {
        let mounts = parse(SAMPLE.as_bytes()).unwrap();
        assert_eq!(mounts.len(), 7);
        assert_eq!(mounts[0].mount_point, PathBuf::from("/"));
        assert!(!mounts[0].read_only);
        // Per-mount ro over a writable superblock (read-only bind mount).
        assert_eq!(mounts[3].mount_point, PathBuf::from("/mnt/bind"));
        assert!(mounts[3].read_only);
        // `\040` is an escaped space; superblock ro alone is read-only.
        assert_eq!(mounts[4].mount_point, PathBuf::from("/mnt/My Movies"));
        assert_eq!(mounts[4].fstype, "cifs");
        assert!(mounts[4].read_only);
        assert_eq!(unescape(b"a\\011b\\012c\\134d\\0"), b"a\tb\nc\\d\\0");
    }

    #[test]
    fn lookup_chooses_longest_component_prefix_and_topmost_stack() {
        let table = Table::new(parse(SAMPLE.as_bytes()).unwrap());
        let point = |path: &str| {
            table
                .get(table.lookup(Path::new(path)).unwrap())
                .mount_point
                .clone()
        };
        assert_eq!(point("/mnt/media/tv/Show"), PathBuf::from("/mnt/media/tv"));
        assert_eq!(point("/mnt/media/tv"), PathBuf::from("/mnt/media/tv"));
        assert_eq!(point("/mnt/media/tv2/Show"), PathBuf::from("/mnt/media"));
        assert_eq!(
            point("/mnt/My Movies/Film"),
            PathBuf::from("/mnt/My Movies")
        );
        assert_eq!(point("/home/x"), PathBuf::from("/"));
        let stacked = table.lookup(Path::new("/mnt/stack/a")).unwrap();
        assert_eq!(stacked, 6);
        assert!(table.get(stacked).read_only);
    }

    #[test]
    fn malformed_and_oversized_tables_are_rejected() {
        assert_eq!(parse(b""), Err(TableError::Malformed));
        assert_eq!(parse(b"1 2 3:4 / /x rw\n"), Err(TableError::Malformed));
        assert_eq!(
            parse(b"1 2 3:4 / relative rw - ext4 /dev/x rw\n"),
            Err(TableError::Malformed)
        );
        let big = vec![b'x'; MOUNTINFO_BYTES + 1];
        assert_eq!(parse(&big), Err(TableError::TooLarge));
        let many = "1 0 0:1 / / rw - tmpfs t rw\n".repeat(MOUNTINFO_LINES + 1);
        assert_eq!(parse(many.as_bytes()), Err(TableError::TooLarge));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn proc_self_mountinfo_parses_on_linux() {
        let bytes = ProcSelfMountInfo.read(MOUNTINFO_BYTES).unwrap();
        let table = Table::new(parse(&bytes).unwrap());
        assert!(table.lookup(Path::new("/")).is_some());
    }
}
