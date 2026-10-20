//! hc.034 RootFolderCheck (TV series, movies) and MovieCollectionRootFolderCheck (movies scope).
//! Every library item's root folder must exist as a directory reachable by hrrdarr's anchored,
//! symlink-refusing walk. Observation only: never creates, writes into or enumerates folders.
//! Shared bounded-probe helpers are also used by the hc.035 mount detector.
use super::{HealthIdentity, HealthIssue, HealthScope, HealthSeverity};
use crate::db::Database;
use libsql::{TransactionBehavior, params};
use std::{
    collections::{BTreeMap, HashSet},
    io,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;

/// Work bounds for one evaluation. Exceeding any bound is reported, never silently healthy.
#[derive(Clone, Copy, Debug)]
pub(super) struct Limits {
    /// Library rows scanned per evaluation.
    pub items: usize,
    /// Bytes of item path/title text retained in memory (mount detector).
    pub item_bytes: usize,
    /// Distinct root folders probed, or distinct read-only mounts itemized.
    pub groups: usize,
    /// Wall-clock budget for filesystem probes, enforced off the async executor.
    pub budget: Duration,
}
impl Limits {
    pub(super) const DEFAULT: Self = Self {
        items: 100_000,
        item_bytes: 16 << 20,
        groups: 512,
        budget: Duration::from_secs(2),
    };
}
/// Distinct groups named in a message; the rest are counted.
pub(super) const LISTED_GROUPS: usize = 5;
/// Item titles named per group.
pub(super) const SAMPLES: usize = 3;
pub(super) const PATH_BYTES: usize = 256;
pub(super) const TITLE_BYTES: usize = 48;
/// Stored message bound (migration 0042).
const MESSAGE_BYTES: usize = 4096;
/// Covers scheduling jitter between the worker's own budget check and the async deadline.
const SLACK: Duration = Duration::from_millis(250);
/// Library rows longer than the validated 4096-byte path bound are invalid, never decoded whole.
const ROW_PATH_BYTES: i64 = 4097;
/// A probe stuck in the kernel (hung network mount) keeps its permit until it returns.
static ROOT_PROBES: Semaphore = Semaphore::const_new(2);

/// Control characters are replaced and long text is cut at a char boundary.
pub(super) fn display(text: &str, max: usize) -> String {
    let mut shown = String::new();
    for c in text.chars() {
        let c = if c.is_control() { '?' } else { c };
        if shown.len() + c.len_utf8() > max {
            shown.push('…');
            break;
        }
        shown.push(c);
    }
    shown
}

pub(super) fn bounded_message(mut message: String) -> String {
    if message.len() > MESSAGE_BYTES {
        let mut end = MESSAGE_BYTES - '…'.len_utf8();
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
        message.push('…');
    }
    message
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Stop {
    /// Every input was probed.
    Complete,
    /// The worker stopped between probes at the budget.
    Budget,
    /// The probe of the next unreported input did not return within the budget.
    Hung,
}

/// Runs `probe` serially on one blocking thread, returning the completed prefix of outputs.
/// A probe that never returns cannot block the health batch; its thread retains a permit.
pub(super) async fn run_bounded<I, T, F>(
    permits: &'static Semaphore,
    inputs: Vec<I>,
    budget: Duration,
    mut probe: F,
) -> Result<(Vec<T>, Stop), &'static str>
where
    I: Send + 'static,
    T: Send + 'static,
    F: FnMut(&I) -> T + Send + 'static,
{
    let total = inputs.len();
    let permit = permits.try_acquire().map_err(|_| "check_timeout")?;
    let (sender, receiver) = std::sync::mpsc::channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = cancelled.clone();
    let deadline = Instant::now() + budget;
    let job = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        for input in &inputs {
            if flag.load(Ordering::Relaxed) || Instant::now() >= deadline {
                return false;
            }
            if sender.send(probe(input)).is_err() {
                return false;
            }
        }
        true
    });
    let finished = match tokio::time::timeout(budget + SLACK, job).await {
        Ok(Ok(complete)) => Some(complete),
        Ok(Err(_)) => return Err("check_failed"),
        Err(_) => {
            cancelled.store(true, Ordering::Relaxed);
            None
        }
    };
    let done: Vec<T> = receiver.try_iter().take(total).collect();
    let stop = if done.len() == total {
        Stop::Complete
    } else if finished.is_some() {
        Stop::Budget
    } else {
        Stop::Hung
    };
    Ok((done, stop))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RootState {
    Present,
    Missing,
    NotDirectory,
    Symlink,
    PermissionDenied,
    /// Stale/disconnected network or FUSE mount, device gone, or I/O failure.
    Unavailable,
    Invalid,
    Unresponsive,
}
impl RootState {
    fn text(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Missing => "not found",
            Self::NotDirectory => "not a folder",
            Self::Symlink => "path traverses a symbolic link, which hrrdarr refuses",
            Self::PermissionDenied => "permission denied",
            Self::Unavailable => "mount unavailable, stale or disconnected",
            Self::Invalid => "invalid library path",
            Self::Unresponsive => "did not respond within the probe budget",
        }
    }
    fn reason(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Missing => "missing",
            Self::NotDirectory => "not_directory",
            Self::Symlink => "symlink",
            Self::PermissionDenied => "permission_denied",
            Self::Unavailable => "unavailable",
            Self::Invalid => "invalid",
            Self::Unresponsive => "unresponsive",
        }
    }
}

/// Read-only anchored walk: each component opened O_DIRECTORY|O_NOFOLLOW, nothing created.
pub(super) fn classify(path: &str) -> RootState {
    match crate::import::filesystem_directory(Path::new(path)) {
        Ok(_) => RootState::Present,
        // O_DIRECTORY|O_NOFOLLOW reports a symlinked component as ENOTDIR; lstat tells them apart.
        Err(error) => match classify_error(&error) {
            RootState::NotDirectory if traverses_symlink(path) => RootState::Symlink,
            state => state,
        },
    }
}

fn traverses_symlink(path: &str) -> bool {
    Path::new(path).ancestors().any(|ancestor| {
        std::fs::symlink_metadata(ancestor).is_ok_and(|meta| meta.file_type().is_symlink())
    })
}

pub(super) fn classify_error(error: &io::Error) -> RootState {
    use rustix::io::Errno;
    match Errno::from_io_error(error) {
        Some(Errno::NOENT) => RootState::Missing,
        Some(Errno::NOTDIR) => RootState::NotDirectory,
        Some(Errno::LOOP) => RootState::Symlink,
        Some(Errno::ACCESS | Errno::PERM) => RootState::PermissionDenied,
        Some(_) => RootState::Unavailable,
        None if error.kind() == io::ErrorKind::InvalidInput => RootState::Invalid,
        None => RootState::Unavailable,
    }
}

/// Upstream semantics: the longest configured root strictly containing the item, otherwise the
/// item's parent directory. Both inputs are normalized absolute paths.
pub(super) fn best_root<'a>(item: &'a str, roots: &HashSet<String>) -> &'a str {
    let mut parent = None;
    let mut end = item.len();
    while let Some(index) = item[..end].rfind('/') {
        let candidate = if index == 0 { "/" } else { &item[..index] };
        parent.get_or_insert(candidate);
        if roots.contains(candidate) {
            return candidate;
        }
        if index == 0 {
            break;
        }
        end = index;
    }
    parent.unwrap_or("/")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Series,
    Movies,
    Collections,
}
impl Kind {
    fn of(identity: &HealthIdentity) -> Result<Self, &'static str> {
        match (identity.scope, identity.check_key.as_str()) {
            (HealthScope::Tv, "root_folder") => Ok(Self::Series),
            (HealthScope::Movies, "root_folder") => Ok(Self::Movies),
            (HealthScope::Movies, "collection_root_folder") => Ok(Self::Collections),
            _ => Err("check_failed"),
        }
    }
    fn noun(self, count: u64) -> &'static str {
        match (self, count) {
            (Self::Series, _) => "series",
            (Self::Movies, 1) => "movie",
            (Self::Movies, _) => "movies",
            (Self::Collections, 1) => "collection",
            (Self::Collections, _) => "collections",
        }
    }
    fn reason_prefix(self) -> &'static str {
        match self {
            Self::Collections => "collection_root_folder",
            _ => "root_folder",
        }
    }
}

#[derive(Default)]
struct Group {
    items: u64,
    samples: Vec<String>,
}

struct Scan {
    groups: BTreeMap<String, Group>,
    invalid: Group,
    /// Items not evaluated: beyond the scan bound or under an unprobed root.
    unevaluated: u64,
}

fn storage<E>(_: E) -> &'static str {
    "storage_error"
}

async fn scan(db: &Database, kind: Kind, limits: &Limits) -> Result<Scan, &'static str> {
    let (count_sql, rows_sql) = match kind {
        Kind::Series => (
            "SELECT count(*) FROM series",
            "SELECT substr(path,1,?),substr(title,1,?) FROM series ORDER BY id LIMIT ?",
        ),
        Kind::Movies => (
            "SELECT count(*) FROM movies",
            "SELECT substr(m.path,1,?),substr(mm.title,1,?) FROM movies m LEFT JOIN movie_metadata mm ON mm.id=m.metadata_id ORDER BY m.id LIMIT ?",
        ),
        Kind::Collections => (
            "SELECT count(*) FROM movie_collections WHERE root_folder_id IS NOT NULL",
            "SELECT substr(r.path,1,?),substr(c.title,1,?) FROM movie_collections c JOIN root_folders r ON r.id=c.root_folder_id ORDER BY c.id LIMIT ?",
        ),
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
    let mut roots = HashSet::new();
    if kind != Kind::Collections {
        let media = if kind == Kind::Series { "tv" } else { "movies" };
        let mut rows = tx
            .query(
                "SELECT path FROM root_folders WHERE media_type=? ORDER BY id LIMIT 10001",
                [media],
            )
            .await
            .map_err(storage)?;
        while let Some(row) = rows.next().await.map_err(storage)? {
            if roots.len() == 10_000 {
                return Err("check_failed");
            }
            let path: String = row.get(0).map_err(storage)?;
            roots.insert(crate::library::normalized_path(&path).ok_or("check_failed")?);
        }
    }
    let limit = i64::try_from(limits.items).map_err(|_| "check_failed")?;
    let mut rows = tx
        .query(
            rows_sql,
            params![ROW_PATH_BYTES, TITLE_BYTES as i64 + 1, limit],
        )
        .await
        .map_err(storage)?;
    let mut scan = Scan {
        groups: BTreeMap::new(),
        invalid: Group::default(),
        unevaluated: 0,
    };
    let mut scanned: u64 = 0;
    while let Some(row) = rows.next().await.map_err(storage)? {
        scanned += 1;
        let path: String = row.get(0).map_err(storage)?;
        let title: Option<String> = row.get(1).map_err(storage)?;
        let title = display(title.as_deref().unwrap_or("(untitled)"), TITLE_BYTES);
        let group = match crate::library::normalized_path(&path) {
            None => &mut scan.invalid,
            Some(item) => {
                let root = if kind == Kind::Collections {
                    item.as_str()
                } else {
                    best_root(&item, &roots)
                };
                if !scan.groups.contains_key(root) && scan.groups.len() >= limits.groups {
                    scan.unevaluated += 1;
                    continue;
                }
                scan.groups.entry(root.to_owned()).or_default()
            }
        };
        group.items += 1;
        if group.samples.len() < SAMPLES {
            group.samples.push(title);
        }
    }
    drop(rows);
    tx.commit().await.map_err(storage)?;
    let total = u64::try_from(total).map_err(|_| "check_failed")?;
    scan.unevaluated += total.saturating_sub(scanned);
    Ok(scan)
}

pub(super) async fn evaluate(
    db: &Database,
    identity: &HealthIdentity,
) -> Result<Option<HealthIssue>, &'static str> {
    evaluate_with(db, identity, &Limits::DEFAULT).await
}

pub(super) async fn evaluate_with(
    db: &Database,
    identity: &HealthIdentity,
    limits: &Limits,
) -> Result<Option<HealthIssue>, &'static str> {
    let kind = Kind::of(identity)?;
    let mut scan = scan(db, kind, limits).await?;
    let roots: Vec<String> = scan.groups.keys().cloned().collect();
    let (states, stop) = if roots.is_empty() {
        (Vec::new(), Stop::Complete)
    } else {
        run_bounded(
            &ROOT_PROBES,
            roots.clone(),
            limits.budget,
            |root: &String| classify(root),
        )
        .await?
    };
    let mut problems = Vec::new();
    for (index, root) in roots.iter().enumerate() {
        let state = match states.get(index) {
            Some(state) => *state,
            None if index == states.len() && stop == Stop::Hung => RootState::Unresponsive,
            None => {
                scan.unevaluated += scan.groups[root].items;
                continue;
            }
        };
        if state != RootState::Present {
            let group = scan.groups.remove(root).ok_or("check_failed")?;
            problems.push((display(root, PATH_BYTES), state, group));
        }
    }
    if scan.invalid.items > 0 {
        problems.push((
            "(invalid library paths)".into(),
            RootState::Invalid,
            std::mem::take(&mut scan.invalid),
        ));
    }
    Ok(issue(identity, kind, problems, scan.unevaluated))
}

fn issue(
    identity: &HealthIdentity,
    kind: Kind,
    problems: Vec<(String, RootState, Group)>,
    unevaluated: u64,
) -> Option<HealthIssue> {
    let (wiki, compatibility) = match kind {
        Kind::Series => (
            "https://wiki.servarr.com/sonarr/system#missing-root-folder",
            "RootFolderCheck",
        ),
        Kind::Movies => (
            "https://wiki.servarr.com/radarr/system#missing-root-folder",
            "RootFolderCheck",
        ),
        Kind::Collections => (
            "https://wiki.servarr.com/radarr/system#movie-collection-missing-root-folder",
            "MovieCollectionRootFolderCheck",
        ),
    };
    let bounded = if unevaluated > 0 {
        format!(
            " Evaluation was bounded: {unevaluated} {} were not evaluated.",
            kind.noun(unevaluated)
        )
    } else {
        String::new()
    };
    let (severity, reason, message) = if problems.is_empty() {
        if unevaluated == 0 {
            return None;
        }
        (
            HealthSeverity::Notice,
            format!("{}_check_truncated", kind.reason_prefix()),
            format!(
                "No missing root folder was found in the evaluated part of the library.{bounded}"
            ),
        )
    } else {
        let entries: Vec<String> = problems
            .iter()
            .take(LISTED_GROUPS)
            .map(|(path, state, group)| {
                format!(
                    "{path} ({}; {} {}, e.g. {})",
                    state.text(),
                    group.items,
                    kind.noun(group.items),
                    group.samples.join(", ")
                )
            })
            .collect();
        let more = problems.len().saturating_sub(LISTED_GROUPS);
        let more = if more > 0 {
            format!(" | {more} more root folders not listed")
        } else {
            String::new()
        };
        let prefix = match (kind, problems.len()) {
            (Kind::Collections, 1) => "Missing root folder for movie collection: ",
            (Kind::Collections, _) => "Multiple root folders are missing for movie collections: ",
            (_, 1) => "Missing root folder: ",
            _ => "Multiple root folders are missing: ",
        };
        let remedy = match kind {
            Kind::Collections => {
                "Restore the folder or its mount, or choose another root folder for the affected collections"
            }
            Kind::Series => {
                "Restore the folder or its mount, correct its permissions, or move the affected series"
            }
            Kind::Movies => {
                "Restore the folder or its mount, correct its permissions, or move the affected movies"
            }
        };
        let reason = match problems.as_slice() {
            [(_, state, _)] => format!("{}_{}", kind.reason_prefix(), state.reason()),
            _ => format!("{}_multiple_missing", kind.reason_prefix()),
        };
        (
            HealthSeverity::Error,
            reason,
            format!(
                "{prefix}{}{more}. {remedy}; hrrdarr never creates library root folders.{bounded}",
                entries.join(" | ")
            ),
        )
    };
    Some(HealthIssue {
        identity: identity.clone(),
        severity,
        reason,
        message: bounded_message(message),
        wiki_url: wiki.into(),
        compatibility_type: compatibility.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn best_root_prefers_longest_configured_strict_parent() {
        let roots: HashSet<String> = ["/media", "/media/tv", "/media/tv-old"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(best_root("/media/tv/Show", &roots), "/media/tv");
        assert_eq!(best_root("/media/tv/Show/Extras", &roots), "/media/tv");
        // Component boundary: /media/tv does not contain /media/tv2.
        assert_eq!(best_root("/media/tv2/Show", &roots), "/media");
        // The item at a root is not its own root; upstream falls back to the strict parent.
        assert_eq!(best_root("/media/tv", &roots), "/media");
        assert_eq!(best_root("/other/Show", &roots), "/other");
        assert_eq!(best_root("/Show", &roots), "/");
    }

    #[test]
    fn errors_classify_distinctly() {
        use rustix::io::Errno;
        let from = |errno: Errno| io::Error::from_raw_os_error(errno.raw_os_error());
        assert_eq!(classify_error(&from(Errno::NOENT)), RootState::Missing);
        assert_eq!(
            classify_error(&from(Errno::NOTDIR)),
            RootState::NotDirectory
        );
        assert_eq!(classify_error(&from(Errno::LOOP)), RootState::Symlink);
        assert_eq!(
            classify_error(&from(Errno::ACCESS)),
            RootState::PermissionDenied
        );
        assert_eq!(
            classify_error(&from(Errno::PERM)),
            RootState::PermissionDenied
        );
        for errno in [Errno::NOTCONN, Errno::STALE, Errno::IO, Errno::NODEV] {
            assert_eq!(classify_error(&from(errno)), RootState::Unavailable);
        }
        assert_eq!(
            classify_error(&io::Error::from(io::ErrorKind::InvalidInput)),
            RootState::Invalid
        );
    }

    #[test]
    fn display_and_message_are_bounded() {
        assert_eq!(display("a\nb", 8), "a?b");
        assert_eq!(display("abcdef", 3), "abc…");
        let long = bounded_message("é".repeat(5000));
        assert!(long.len() <= MESSAGE_BYTES);
        assert!(long.ends_with('…'));
    }

    #[tokio::test]
    async fn bounded_runner_reports_budget_and_hung_probes() {
        static PERMITS: Semaphore = Semaphore::const_new(1);
        let (done, stop) = run_bounded(&PERMITS, vec![1, 2, 3], Duration::from_secs(5), |v| v * 2)
            .await
            .unwrap();
        assert_eq!((done, stop), (vec![2, 4, 6], Stop::Complete));
        // The second probe exceeds the budget: the first output is retained, the rest are not.
        let (done, stop) = run_bounded(
            &PERMITS,
            vec![0u64, 2_000, 0],
            Duration::from_millis(100),
            |v| {
                std::thread::sleep(Duration::from_millis(*v));
                *v
            },
        )
        .await
        .unwrap();
        assert_eq!(done, vec![0]);
        assert_eq!(stop, Stop::Hung);
        // The hung thread still holds the only permit: a new run is explicitly unevaluated.
        assert_eq!(
            run_bounded(&PERMITS, vec![1], Duration::from_millis(10), |v| *v)
                .await
                .unwrap_err(),
            "check_timeout"
        );
    }
}
