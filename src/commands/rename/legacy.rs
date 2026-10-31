//! Explicit, opt-in recovery of rename journals that older binaries settled as
//! `failed` / `rollback_incomplete` while the media still sat at its new path (or partly moved)
//! and the database still named the old path. Current code keeps such an item `planned` so
//! startup recovery finishes it; the historical rows were excluded from that recovery.
//!
//! Nothing here touches media. The assessment reads the filesystem (read-only, descriptor-relative
//! walks), and the admitted transition is one database transaction that either reopens the row
//! as `planned` (the worker's existing journal recovery then rolls it forward or retains it) or,
//! when the original layout is verifiably intact, settles it as `rolled_back`.
use super::*;

/// Each assessed item costs several filesystem probes, so inventory pages and selections are small.
const DEFAULT_LISTED: u16 = 25;
const MAX_LISTED: u16 = 25;
const MAX_SELECTED: usize = 25;
/// Request deadlines cannot cancel a running filesystem probe. Keep its slot until the
/// blocking closure exits, so repeated timed-out requests cannot fill the blocking pool.
const MAX_ASSESSMENTS: usize = 4;
static ASSESSMENTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(MAX_ASSESSMENTS);

async fn blocking_assessment<T: Send + 'static>(
    probe: impl FnOnce() -> T + Send + 'static,
) -> Result<T> {
    let permit = ASSESSMENTS
        .acquire()
        .await
        .map_err(|_| Error(StatusCode::INTERNAL_SERVER_ERROR, "command_storage_error"))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        probe()
    })
    .await
    .map_err(|_| Error(StatusCode::INTERNAL_SERVER_ERROR, "command_storage_error"))
}
/// Exactly the class older binaries wrote; every other failed item is unrelated evidence.
const LEGACY: &str = "i.state='failed' AND i.error_code='rollback_incomplete'";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum LegacyLayout {
    /// Every media file and sidecar is at its recorded source.
    AtSource,
    /// Every media file and sidecar is at its planned destination.
    Moved,
    /// Some pairs moved and some did not.
    Partial,
    /// A pair has an unrelated entry, a changed identity or an ambiguous alias.
    Conflict,
    /// The filesystem could not be read safely (see `layout_error`).
    Unreadable,
    /// The journal could not be parsed, so no layout was observed.
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum LegacyAction {
    /// Reopen as `planned`; the worker moves the remaining pairs and commits the path.
    RollForward,
    /// Layout verified as the original: settle as `rolled_back`, no media change.
    SettleRestored,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum LegacyRefusal {
    NotLegacy,
    JournalInvalid,
    OwnerMissing,
    DatabasePathChanged,
    OwnerBusy,
    UnsettledJournal,
    CounterInconsistent,
    LayoutConflict,
    LayoutUnreadable,
}
#[derive(Serialize, ts_rs::TS)]
pub struct LegacySidecar {
    pub old_path: String,
    pub new_path: String,
    pub device: i64,
    pub inode: i64,
}
#[derive(Serialize, ts_rs::TS)]
pub struct LegacyRenameItem {
    pub item_id: i64,
    pub command_id: Uuid,
    /// `owner_id` is a series ID for `tv` and a movie ID for `movies`; never interchangeable.
    pub media_type: MediaDomain,
    pub owner_id: i64,
    pub kind: RenameEventKind,
    pub file_id: Option<i64>,
    pub old_path: String,
    pub new_path: String,
    pub device: Option<i64>,
    pub inode: Option<i64>,
    pub sidecars: Vec<LegacySidecar>,
    pub layout: LegacyLayout,
    pub layout_error: Option<String>,
    /// The owning command's current `items_failed` (the old binary counted this attempt).
    pub command_items_failed: i64,
    /// Present only when the item may be admitted.
    pub action: Option<LegacyAction>,
    pub refusal: Option<LegacyRefusal>,
    /// Change this transition makes to the command's `items_failed`.
    pub items_failed_delta: i64,
    /// Set only by an executed (non-dry-run) request that committed the transition.
    pub reopened: bool,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct LegacyRecoveryInput {
    /// Required, no default: a dry run lists the effects, `false` is the explicit opt-in.
    pub dry_run: bool,
    pub item_ids: Vec<i64>,
}
#[derive(Serialize, ts_rs::TS)]
pub struct LegacyRecoveryReport {
    pub dry_run: bool,
    pub items: Vec<LegacyRenameItem>,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct LegacyQuery {
    #[ts(optional)]
    pub media_type: Option<MediaDomain>,
    #[serde(default = "default_listed")]
    #[ts(as = "Option<u16>", optional)]
    pub limit: u16,
    #[serde(default)]
    #[ts(as = "Option<u32>", optional)]
    pub offset: u32,
}
fn default_listed() -> u16 {
    DEFAULT_LISTED
}

/// A row exactly as stored, plus the owning command's failure counter.
struct Stored {
    journal: Result<Journal>,
    command_items_failed: i64,
    legacy: bool,
    raw_old: String,
    raw_new: String,
    raw_identity: Option<Ident>,
}
const SELECT: &str = "SELECT i.id,i.command_id,i.kind,i.series_id,i.movie_id,i.file_id,i.old_path,i.new_path,i.identity_json,i.sidecars_json,c.items_failed,CASE WHEN i.state='failed' AND i.error_code='rollback_incomplete' THEN 1 ELSE 0 END FROM rename_items i JOIN rename_commands c ON c.id=i.command_id";
fn stored(r: &libsql::Row) -> Result<Stored> {
    Ok(Stored {
        journal: parse_journal(r),
        command_items_failed: r.get(10)?,
        legacy: r.get::<i64>(11)? == 1,
        raw_old: r.get(6)?,
        raw_new: r.get(7)?,
        raw_identity: serde_json::from_str(&r.get::<String>(8)?).ok(),
    })
}
fn owner(r: &libsql::Row) -> Result<(MediaDomain, i64)> {
    match (r.get::<Option<i64>>(3)?, r.get::<Option<i64>>(4)?) {
        (Some(series), None) => Ok((MediaDomain::Tv, series)),
        (None, Some(movie)) => Ok((MediaDomain::Movies, movie)),
        _ => Err(bad()),
    }
}

/// Read-only observation of every media/sidecar pair.
fn observe(j: &Journal) -> (LegacyLayout, Option<&'static str>) {
    let (mut source, mut moved) = (0_usize, 0_usize);
    let pairs = j.pairs();
    let total = pairs.len();
    for (i, (old, new, ident)) in pairs.into_iter().enumerate() {
        let kind = if i == 0 { j.kind } else { Kind::File };
        match fsops::pair_state(&old, &new, kind, ident) {
            Ok(PairState::Source) => source += 1,
            Ok(PairState::Moved) => moved += 1,
            Ok(PairState::Anomaly) => return (LegacyLayout::Conflict, None),
            Err(e) => return (LegacyLayout::Unreadable, Some(e.code())),
        }
    }
    let layout = if source == total {
        LegacyLayout::AtSource
    } else if moved == total {
        LegacyLayout::Moved
    } else {
        LegacyLayout::Partial
    };
    (layout, None)
}

/// Database-side admission facts, re-evaluated inside the writing transaction.
async fn database_gate(c: &Connection, j: &mut Journal) -> Result<Option<LegacyRefusal>> {
    if !bind_root(c, j).await? {
        return Ok(Some(LegacyRefusal::OwnerMissing));
    }
    let tv = j.series_id.is_some();
    let (owner_table, owner_id) = if tv {
        ("series", j.series_id.unwrap_or(0))
    } else {
        ("movies", j.movie_id.unwrap_or(0))
    };
    let recorded: Option<String> = if j.kind == Kind::Dir {
        c.query(
            &format!("SELECT path FROM {owner_table} WHERE id=?"),
            [owner_id],
        )
        .await?
        .next()
        .await?
        .map(|r| r.get(0))
        .transpose()?
    } else {
        let (table, column) = if tv {
            ("episode_files", "series_id")
        } else {
            ("movie_files", "movie_id")
        };
        c.query(
            &format!("SELECT path FROM {table} WHERE id=? AND {column}=?"),
            params![j.file_id.unwrap_or(0), owner_id],
        )
        .await?
        .next()
        .await?
        .map(|r| r.get(0))
        .transpose()?
    };
    if recorded.as_deref() != Some(j.old.as_str()) {
        return Ok(Some(LegacyRefusal::DatabasePathChanged));
    }
    let column = if tv { "series_id" } else { "movie_id" };
    if c.query(&format!("SELECT 1 FROM rename_command_targets t JOIN rename_commands r ON r.id=t.command_id WHERE r.status IN ('queued','running','retry_wait') AND t.{column}=?"), [owner_id]).await?.next().await?.is_some() {
        return Ok(Some(LegacyRefusal::OwnerBusy));
    }
    if c.query(
        // A planned folder journal overlaps every file of its owner; files overlap only themselves.
        &format!("SELECT 1 FROM rename_items WHERE state='planned' AND id<>? AND {column}=? AND (kind='folder' OR ?='folder' OR file_id=?) LIMIT 1"),
        params![
            j.id,
            owner_id,
            if j.kind == Kind::Dir { "folder" } else { "file" },
            j.file_id
        ],
    )
    .await?
    .next()
    .await?
    .is_some()
    {
        return Ok(Some(LegacyRefusal::UnsettledJournal));
    }
    Ok(None)
}

async fn assess(c: &Connection, row: &libsql::Row) -> Result<(LegacyRenameItem, Option<Journal>)> {
    let s = stored(row)?;
    let (media_type, owner_id) = owner(row)?;
    let mut item = LegacyRenameItem {
        item_id: row.get(0)?,
        command_id: Uuid::parse_str(&row.get::<String>(1)?).map_err(|_| bad())?,
        media_type,
        owner_id,
        kind: if row.get::<String>(2)? == "folder" {
            RenameEventKind::Folder
        } else {
            RenameEventKind::File
        },
        file_id: row.get(5)?,
        old_path: s.raw_old,
        new_path: s.raw_new,
        device: s.raw_identity.map(|i| i.dev),
        inode: s.raw_identity.map(|i| i.ino),
        sidecars: vec![],
        layout: LegacyLayout::Unknown,
        layout_error: None,
        command_items_failed: s.command_items_failed,
        action: None,
        refusal: None,
        items_failed_delta: 0,
        reopened: false,
    };
    if !s.legacy {
        item.refusal = Some(LegacyRefusal::NotLegacy);
        return Ok((item, None));
    }
    let Ok(mut journal) = s.journal else {
        item.refusal = Some(LegacyRefusal::JournalInvalid);
        return Ok((item, None));
    };
    item.sidecars = journal
        .sidecars
        .iter()
        .map(|x| LegacySidecar {
            old_path: x.old.clone(),
            new_path: x.new.clone(),
            device: x.ident.dev,
            inode: x.ident.ino,
        })
        .collect();
    if let Some(refusal) = database_gate(c, &mut journal).await? {
        item.refusal = Some(refusal);
        return Ok((item, Some(journal)));
    }
    let journal = Arc::new(journal);
    let observed = journal.clone();
    let (layout, error) = blocking_assessment(move || observe(&observed)).await?;
    let journal = Arc::into_inner(journal).ok_or_else(bad)?;
    item.layout = layout;
    item.layout_error = error.map(str::to_owned);
    match layout {
        LegacyLayout::Conflict => item.refusal = Some(LegacyRefusal::LayoutConflict),
        LegacyLayout::Unreadable => item.refusal = Some(LegacyRefusal::LayoutUnreadable),
        LegacyLayout::AtSource => item.action = Some(LegacyAction::SettleRestored),
        LegacyLayout::Moved | LegacyLayout::Partial => {
            if item.command_items_failed < 1 {
                item.refusal = Some(LegacyRefusal::CounterInconsistent);
            } else {
                item.action = Some(LegacyAction::RollForward);
                item.items_failed_delta = -1;
            }
        }
        LegacyLayout::Unknown => item.refusal = Some(LegacyRefusal::JournalInvalid),
    }
    Ok((item, Some(journal)))
}

/// Applies one admitted item atomically. The row can only leave the legacy class here, so a
/// rerun finds nothing to change and cannot decrement the counter twice.
async fn apply(
    db: &Database,
    item: &mut LegacyRenameItem,
    mut journal: Journal,
    action: LegacyAction,
) -> Result<()> {
    let c = connection(db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome: Result<Option<LegacyRefusal>> = async {
        if let Some(refusal) = database_gate(&tx, &mut journal).await? {
            return Ok(Some(refusal));
        }
        let moved = match action {
            LegacyAction::RollForward => {
                "UPDATE rename_items SET state='planned' WHERE id=? AND state='failed' AND error_code='rollback_incomplete'"
            }
            LegacyAction::SettleRestored => {
                "UPDATE rename_items SET state='rolled_back',error_code='legacy_rollback_verified' WHERE id=? AND state='failed' AND error_code='rollback_incomplete'"
            }
        };
        if tx.execute(moved, [journal.id]).await? != 1 {
            return Ok(Some(LegacyRefusal::NotLegacy));
        }
        if action == LegacyAction::RollForward
            && tx
                .execute(
                    "UPDATE rename_commands SET items_failed=items_failed-1 WHERE id=? AND items_failed>0",
                    [journal.command_id.clone()],
                )
                .await?
                != 1
        {
            return Ok(Some(LegacyRefusal::CounterInconsistent));
        }
        Ok(None)
    }
    .await;
    match outcome {
        Ok(None) => {
            tx.commit().await?;
            item.reopened = true;
            eprintln!(
                "event=rename_legacy_recovery item_id={} command_id={} action={}",
                item.item_id,
                item.command_id,
                match action {
                    LegacyAction::RollForward => "reopened_planned",
                    LegacyAction::SettleRestored => "settled_rolled_back",
                }
            );
            Ok(())
        }
        Ok(Some(refusal)) => {
            tx.rollback().await?;
            item.refusal = Some(refusal);
            item.action = None;
            item.items_failed_delta = 0;
            Ok(())
        }
        Err(e) => {
            let _ = tx.rollback().await;
            Err(e)
        }
    }
}

pub(super) async fn inventory(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<LegacyQuery>, QueryRejection>,
) -> Result<Json<ApiPage<LegacyRenameItem>>> {
    let q = q.map_err(|_| bad())?.0;
    if !(1..=MAX_LISTED).contains(&q.limit) {
        return Err(bad());
    }
    let media = q.media_type.map(domain);
    let c = connection(&db).await?;
    let scope = "(?1 IS NULL OR (?1='tv' AND i.series_id IS NOT NULL) OR (?1='movies' AND i.movie_id IS NOT NULL))";
    let total: i64 = c
        .query(
            &format!("SELECT count(*) FROM rename_items i WHERE {LEGACY} AND {scope}"),
            [media],
        )
        .await?
        .next()
        .await?
        .ok_or_else(bad)?
        .get(0)?;
    let mut rows = c
        .query(
            &format!("{SELECT} WHERE {LEGACY} AND {scope} ORDER BY i.id LIMIT ?2 OFFSET ?3"),
            params![media, i64::from(q.limit), i64::from(q.offset)],
        )
        .await?;
    let mut items = vec![];
    while let Some(r) = rows.next().await? {
        items.push(assess(&c, &r).await?.0);
    }
    bounded(ApiPage {
        items,
        total,
        limit: q.limit,
        offset: q.offset,
    })
}

pub(super) async fn recover(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
    input: std::result::Result<Json<LegacyRecoveryInput>, JsonRejection>,
) -> Result<Json<LegacyRecoveryReport>> {
    q.map_err(|_| bad())?;
    let input = input.map_err(|_| bad())?.0;
    if !distinct(&input.item_ids, MAX_SELECTED) {
        return Err(bad());
    }
    // Keep owned imports/deletions from replacing a verified source between assessment
    // and the transaction that retires or reopens its recovery evidence.
    let _guard = crate::import::acquire_deletion_guard(&db, "legacy-recovery")
        .map_err(|_| Error(StatusCode::CONFLICT, "import_busy"))?;
    let c = connection(&db).await?;
    let mut items = vec![];
    for id in &input.item_ids {
        let row = c
            .query(&format!("{SELECT} WHERE i.id=?"), [*id])
            .await?
            .next()
            .await?
            .ok_or(Error(StatusCode::NOT_FOUND, "rename_item_not_found"))?;
        items.push(assess(&c, &row).await?);
    }
    let mut report = vec![];
    for (mut item, journal) in items {
        if let (false, Some(action), Some(journal)) = (input.dry_run, item.action, journal) {
            apply(&db, &mut item, journal, action).await?;
        }
        report.push(item);
    }
    bounded(LegacyRecoveryReport {
        dry_run: input.dry_run,
        items: report,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn cancelled_assessments_keep_their_slots_until_blocking_work_exits() {
        let mut releases = vec![];
        for _ in 0..MAX_ASSESSMENTS {
            let (release, blocked) = std::sync::mpsc::channel();
            let (started, running) = tokio::sync::oneshot::channel();
            let request = tokio::spawn(blocking_assessment(move || {
                started.send(()).unwrap();
                blocked.recv_timeout(Duration::from_secs(10)).unwrap();
            }));
            tokio::time::timeout(Duration::from_secs(2), running)
                .await
                .unwrap()
                .unwrap();
            // Dropping a timed-out request cancels the await, but its filesystem closure
            // is already running. Its capacity must not become available to a new request.
            request.abort();
            assert!(request.await.unwrap_err().is_cancelled());
            releases.push(release);
        }
        assert_eq!(ASSESSMENTS.available_permits(), 0);
        let (started, mut running) = tokio::sync::oneshot::channel();
        let next = tokio::spawn(blocking_assessment(move || started.send(()).unwrap()));
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut running)
                .await
                .is_err()
        );
        releases.pop().unwrap().send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), running)
            .await
            .unwrap()
            .unwrap();
        next.await.unwrap().unwrap();
        for release in releases {
            release.send(()).unwrap();
        }
        // Also prove all detached closures eventually return their slots.
        let all = tokio::time::timeout(
            Duration::from_secs(2),
            ASSESSMENTS.acquire_many(MAX_ASSESSMENTS as u32),
        )
        .await
        .unwrap()
        .unwrap();
        drop(all);
    }

    #[tokio::test]
    async fn recovery_refuses_an_active_file_effect_and_releases_its_guard_on_error() {
        let path = std::env::temp_dir().join(format!("hrrdarr-legacy-guard-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        let db = Arc::new(Database::open_local(path.join("db")).await.unwrap());
        let run = || {
            recover(
                State(db.clone()),
                Ok(Query(Empty {})),
                Ok(Json(LegacyRecoveryInput {
                    dry_run: false,
                    item_ids: vec![1],
                })),
            )
        };
        let guard = crate::import::acquire_deletion_guard(&db, "owned-replacement").unwrap();
        let error = run().await.err().expect("replacement excludes recovery");
        assert_eq!((error.0, error.1), (StatusCode::CONFLICT, "import_busy"));
        drop(guard);
        // With the permit free, admission reaches lookup. An error must release its guard.
        let error = run().await.err().expect("fixture has no legacy item");
        assert_eq!(
            (error.0, error.1),
            (StatusCode::NOT_FOUND, "rename_item_not_found")
        );
        let guard = crate::import::acquire_deletion_guard(&db, "next-replacement").unwrap();
        drop(guard);
        drop(db);
        std::fs::remove_dir_all(path).unwrap();
    }
}
