//! Move-mode source-directory cleanup checkpoints for a published parent (cmd.010 phase C).
//!
//! After every child settled and at least one imported child moved its file, a folder source
//! is cleaned in separately checkpointed worker turns:
//!
//! 1. `not_needed -> pending/observed`: the scanned folder must still be the inventory root;
//!    its parent identity is recorded. No effect.
//! 2. `observed -> remove_intent` (Health invalidated in the same transaction), then the
//!    anchored removal of now-empty recorded directories (including the private
//!    `.hrrdarr-move-{operation}` directories) on a blocking thread, then
//!    `remove_intent -> settled` as `complete` or `retained` with a static reason.
//!
//! A failure after the intent whose effect is unknown keeps the parent `running` with the
//! intent recorded; only `POST .../{id}/resume-cleanup` with the exact current
//! `cleanup_resume_generation` authorizes another attempt. Copy mode, a file source, or a
//! parent without an imported move never leaves `not_needed`.
use super::*;
use crate::import::scan_cleanup::{self, CleanupOutcome, CleanupPlan, DirIdentity, Uncertain};

const ACTIVE_POLL_SECONDS: i64 = 5;
const RECOVERY_POLL_SECONDS: i64 = 300;
const CLEANUP_BUDGET: Duration = Duration::from_secs(20);
const MAX_GENERATION: i64 = 9007199254740991;
const MAX_ROWS: i64 = 20_001;

struct State {
    media: MediaDomain,
    path: String,
    cancel: bool,
    cleanup_state: String,
    phase: Option<String>,
    resume_generation: i64,
    resume_requested: bool,
    applicable: bool,
    unresolved: bool,
    root: Option<DirIdentity>,
    parent: Option<DirIdentity>,
    removed: i64,
}
fn identity(dev: Option<i64>, ino: Option<i64>) -> Option<DirIdentity> {
    Some(DirIdentity {
        dev: u64::try_from(dev?).ok()?,
        ino: u64::try_from(ino?).ok()?,
    })
}
async fn state(c: &Connection, key: &str) -> Result<Option<State>> {
    let Some(r) = c
        .query(
            "SELECT p.media_type,p.source_path,p.cancel_requested,p.cleanup_state,json_extract(p.cleanup_json,'$.phase'),p.cleanup_resume_generation,p.cleanup_resume_requested,
             json_extract(p.inventory_json,'$.root.kind')='directory' AND EXISTS(SELECT 1 FROM downloaded_scan_children c WHERE c.command_id=p.id AND c.disposition='imported' AND c.resolved_mode='move'),
             EXISTS(SELECT 1 FROM downloaded_scan_children c JOIN import_journal j ON j.operation_id=c.operation_id WHERE c.command_id=p.id AND c.disposition IN ('failed','ready','importing') AND j.phase!='complete' AND NOT (j.phase='preview' AND j.error_code IS NULL AND j.path_failure_code IS NULL)),
             coalesce(json_extract(p.cleanup_json,'$.root_identity.dev'),json_extract(p.inventory_json,'$.root.identity.dev')),
             coalesce(json_extract(p.cleanup_json,'$.root_identity.ino'),json_extract(p.inventory_json,'$.root.identity.ino')),
             json_extract(p.cleanup_json,'$.parent_identity.dev'),json_extract(p.cleanup_json,'$.parent_identity.ino'),
             coalesce(json_extract(p.cleanup_json,'$.removed'),0)
             FROM downloaded_scan_commands p WHERE p.id=? AND p.status='running' AND p.inventory_revision=1",
            [key],
        )
        .await?
        .next()
        .await?
    else {
        return Ok(None);
    };
    Ok(Some(State {
        media: MediaDomain::parse(&r.get::<String>(0)?).map_err(|_| bad())?,
        path: r.get(1)?,
        cancel: r.get::<i64>(2)? == 1,
        cleanup_state: r.get(3)?,
        phase: r.get(4)?,
        resume_generation: r.get(5)?,
        resume_requested: r.get::<i64>(6)? == 1,
        applicable: r.get::<Option<i64>>(7)? == Some(1),
        unresolved: r.get::<i64>(8)? == 1,
        root: identity(r.get(9)?, r.get(10)?),
        parent: identity(r.get(11)?, r.get(12)?),
        removed: r.get(13)?,
    }))
}
async fn poll(c: &Connection, claim: &ScanClaim, delay: i64) -> Result<()> {
    c.execute(
        "UPDATE downloaded_scan_commands SET next_attempt_at=? WHERE id=? AND status='running' AND run_generation=?",
        params![now()? + delay, claim.id.hyphenated().to_string(), claim.run_generation],
    )
    .await?;
    Ok(())
}
fn log(claim: &ScanClaim, event: &str, code: &str) {
    eprintln!(
        "event=downloaded_scan_cleanup_{event} command_id={} media_type={} code={code} run_generation={}",
        claim.id,
        domain(claim.media),
        claim.run_generation
    );
}
async fn changed(c: &Connection, media: MediaDomain) -> Result<()> {
    signal(c, media, crate::health::PathDiagnosticChange::Progress).await
}
/// Cleanup evidence (an armed or uncertain removal intent) is read by Health; every
/// transition invalidates its generation in the caller's transaction. An uncertain failure
/// schedules a re-check like any failure; a settlement schedules one like a completion.
async fn signal(
    c: &Connection,
    media: MediaDomain,
    change: crate::health::PathDiagnosticChange,
) -> Result<()> {
    crate::health::path_diagnostics_changed(c, media, change)
        .await
        .map_err(|e| Error(e.0, e.1))
}

/// Whether an explicit cleanup resume is waiting to be consumed (it runs before cancellation:
/// an effectful removal intent must settle even for a cancelled parent).
pub(super) async fn resume_pending(c: &Connection, key: &str) -> Result<bool> {
    Ok(c.query(
        "SELECT 1 FROM downloaded_scan_commands WHERE id=? AND status='running' AND cleanup_state='pending' AND cleanup_resume_requested=1",
        [key],
    )
    .await?
    .next()
    .await?
    .is_some())
}

/// At most one cleanup checkpoint. Returns true when this turn is consumed (a checkpoint, a
/// wait or a poll); false when the parent may settle.
pub(super) async fn advance(db: &Arc<Database>, c: &Connection, claim: &ScanClaim) -> Result<bool> {
    let key = claim.id.hyphenated().to_string();
    let Some(s) = state(c, &key).await? else {
        return Ok(true);
    };
    if s.unresolved {
        return Ok(false);
    }
    match (s.cleanup_state.as_str(), s.phase.as_deref()) {
        ("not_needed", _) if s.applicable && !s.cancel => {
            observe(c, claim, &key, s).await?;
            Ok(true)
        }
        ("not_needed", _) | ("complete", _) | ("retained", _) => Ok(false),
        ("pending", Some("observed")) => {
            execute(db, c, claim, &key, s, false).await?;
            Ok(true)
        }
        ("pending", Some("remove_intent")) if s.resume_requested => {
            execute(db, c, claim, &key, s, true).await?;
            Ok(true)
        }
        ("pending", Some("remove_intent")) => {
            // The removal's effect is unknown: never repeated without explicit authority.
            log(claim, "recovery_required", "remove_intent");
            poll(c, claim, RECOVERY_POLL_SECONDS).await?;
            Ok(true)
        }
        _ => Err(Error(StatusCode::CONFLICT, "scan_cleanup_state_invalid")),
    }
}

async fn observe(c: &Connection, claim: &ScanClaim, key: &str, s: State) -> Result<()> {
    let Some(root) = s.root else {
        return Err(Error(StatusCode::CONFLICT, "scan_cleanup_state_invalid"));
    };
    let path = s.path.clone();
    let measured = tokio::task::spawn_blocking(move || scan_cleanup::observe(&path, root))
        .await
        .map_err(|_| Error(StatusCode::SERVICE_UNAVAILABLE, "scan_cleanup_unavailable"))?;
    // An unusable root is a settled, retained outcome; nothing is removed. Its parent identity
    // could not be measured, so the recorded parent is the unmeasurable sentinel 0/0.
    let (cleanup_state, json) = match measured {
        Ok(parent) => (
            "pending",
            serde_json::json!({"version":1,"phase":"observed","root_identity":{"dev":root.dev,"ino":root.ino},"parent_identity":{"dev":parent.dev,"ino":parent.ino},"reason":null,"removed":0,"error":null}),
        ),
        Err(reason) => (
            "retained",
            serde_json::json!({"version":1,"phase":"settled","root_identity":{"dev":root.dev,"ino":root.ino},"parent_identity":{"dev":0,"ino":0},"reason":reason,"removed":0,"error":null}),
        ),
    };
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        let changed_rows = tx
            .execute(
                "UPDATE downloaded_scan_commands SET cleanup_state=?,cleanup_json=? WHERE id=? AND status='running' AND inventory_revision=1 AND run_generation=? AND cleanup_state='not_needed' AND cancel_requested=0",
                params![cleanup_state, json.to_string(), key, claim.run_generation],
            )
            .await?;
        if changed_rows == 1 && cleanup_state == "retained" {
            changed(&tx, s.media).await?;
        }
        Ok(changed_rows)
    }
    .await;
    if finish(tx, outcome).await? == 1 {
        log(claim, "observed", cleanup_state);
    }
    Ok(())
}

async fn plan(c: &Connection, key: &str, s: &State, resumed: bool) -> Result<CleanupPlan> {
    let (Some(root), Some(parent)) = (s.root, s.parent) else {
        return Err(Error(StatusCode::CONFLICT, "scan_cleanup_state_invalid"));
    };
    let number = |v: String| v.parse::<u64>().map_err(|_| bad());
    let mut known_dirs = HashSet::new();
    let mut rows = c
        .query(
            "SELECT source_dev,source_ino FROM downloaded_scan_children WHERE command_id=? AND json_extract(decision_json,'$.entry_kind')='directory' AND source_dev IS NOT NULL LIMIT ?",
            params![key, MAX_ROWS],
        )
        .await?;
    while let Some(r) = rows.next().await? {
        known_dirs.insert((number(r.get(0)?)?, number(r.get(1)?)?));
    }
    drop(rows);
    let mut quarantine = HashMap::new();
    let mut rows = c
        .query(
            "SELECT c.operation_id,json_extract(j.stage_json,'$.quarantine.dev'),json_extract(j.stage_json,'$.quarantine.ino') FROM downloaded_scan_children c JOIN import_journal j ON j.operation_id=c.operation_id WHERE c.command_id=? AND c.disposition='imported' AND c.resolved_mode='move' AND json_type(j.stage_json,'$.quarantine') IS 'object' LIMIT ?",
            params![key, MAX_ROWS],
        )
        .await?;
    while let Some(r) = rows.next().await? {
        let operation: String = r.get(0)?;
        let (dev, ino): (i64, i64) = (r.get(1)?, r.get(2)?);
        quarantine.insert(
            format!(".hrrdarr-move-{operation}"),
            (
                u64::try_from(dev).map_err(|_| bad())?,
                u64::try_from(ino).map_err(|_| bad())?,
            ),
        );
    }
    drop(rows);
    let tv = s.media == MediaDomain::Tv;
    let mut unimported_media = HashSet::new();
    let mut rows = c
        .query(
            "SELECT relative_path,source_dev,source_ino,selected FROM downloaded_scan_children WHERE command_id=? AND json_extract(decision_json,'$.entry_kind')='file' AND disposition!='imported' AND source_dev IS NOT NULL LIMIT ?",
            params![key, MAX_ROWS],
        )
        .await?;
    while let Some(r) = rows.next().await? {
        let relative: String = r.get(0)?;
        let media = r.get::<i64>(3)? == 1
            || relative
                .rsplit_once('.')
                .is_some_and(|(_, ext)| scan_extension_supported(ext, tv));
        if media {
            unimported_media.insert((number(r.get(1)?)?, number(r.get(2)?)?));
        }
    }
    Ok(CleanupPlan {
        source_path: s.path.clone(),
        root,
        parent,
        known_dirs,
        quarantine,
        unimported_media,
        resumed,
        deadline: Instant::now() + CLEANUP_BUDGET,
    })
}

/// Records (or consumes) removal authority under the global effect permit, removes, settles.
async fn execute(
    db: &Arc<Database>,
    c: &Connection,
    claim: &ScanClaim,
    key: &str,
    s: State,
    resumed: bool,
) -> Result<()> {
    let guard = match crate::import::acquire_cleanup_guard(db, key) {
        Ok(guard) => guard,
        Err(_) => return poll(c, claim, ACTIVE_POLL_SECONDS).await,
    };
    let plan = plan(c, key, &s, resumed).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let armed = async {
        let rows = if resumed {
            // Consume the exact explicit generation once; a second failure needs another one.
            tx.execute(
                "UPDATE downloaded_scan_commands SET cleanup_resume_requested=0,cleanup_json=json_set(cleanup_json,'$.error',NULL) WHERE id=? AND status='running' AND inventory_revision=1 AND run_generation=? AND cleanup_state='pending' AND json_extract(cleanup_json,'$.phase')='remove_intent' AND cleanup_resume_requested=1 AND cleanup_resume_generation=?",
                params![key, claim.run_generation, s.resume_generation],
            )
            .await?
        } else {
            tx.execute(
                "UPDATE downloaded_scan_commands SET cleanup_json=json_set(cleanup_json,'$.phase','remove_intent') WHERE id=? AND status='running' AND inventory_revision=1 AND run_generation=? AND cleanup_state='pending' AND json_extract(cleanup_json,'$.phase')='observed' AND cancel_requested=0",
                params![key, claim.run_generation],
            )
            .await?
        };
        if rows == 1 {
            changed(&tx, s.media).await?;
        }
        Ok(rows)
    }
    .await;
    if finish(tx, armed).await? != 1 {
        drop(guard);
        return Ok(());
    }
    log(
        claim,
        if resumed { "resumed" } else { "remove_intent" },
        "armed",
    );
    let result = tokio::task::spawn_blocking(move || scan_cleanup::remove(&plan))
        .await
        .unwrap_or(Err(Uncertain {
            code: "cleanup_io",
            removed: 0,
        }));
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let settled = async {
        let (sql, state, reason, removed, error) = match &result {
            Ok(CleanupOutcome::Complete { removed }) => {
                ("settle", "complete", None, *removed, None)
            }
            Ok(CleanupOutcome::Retained { reason, removed }) => {
                ("settle", "retained", Some(*reason), *removed, None)
            }
            Err(Uncertain { code, removed }) => ("pending", "pending", None, *removed, Some(*code)),
        };
        let total = s
            .removed
            .saturating_add(i64::try_from(removed).unwrap_or(MAX_GENERATION));
        let rows = if sql == "settle" {
            tx.execute(
                "UPDATE downloaded_scan_commands SET cleanup_state=?,cleanup_json=json_set(cleanup_json,'$.phase','settled','$.reason',?,'$.removed',?,'$.error',NULL) WHERE id=? AND status='running' AND run_generation=? AND cleanup_state='pending' AND json_extract(cleanup_json,'$.phase')='remove_intent' AND cleanup_resume_requested=0",
                params![state, reason, total, key, claim.run_generation],
            )
            .await?
        } else {
            tx.execute(
                "UPDATE downloaded_scan_commands SET cleanup_json=json_set(cleanup_json,'$.removed',?,'$.error',?) WHERE id=? AND status='running' AND run_generation=? AND cleanup_state='pending' AND json_extract(cleanup_json,'$.phase')='remove_intent'",
                params![total, error, key, claim.run_generation],
            )
            .await?
        };
        if rows == 1 {
            signal(
                &tx,
                s.media,
                if sql == "settle" {
                    crate::health::PathDiagnosticChange::Completion
                } else {
                    crate::health::PathDiagnosticChange::Failure
                },
            )
            .await?;
        }
        Ok(rows)
    }
    .await;
    let rows = finish(tx, settled).await;
    drop(guard);
    rows?;
    match result {
        Ok(CleanupOutcome::Complete { .. }) => log(claim, "settled", "complete"),
        Ok(CleanupOutcome::Retained { reason, .. }) => log(claim, "settled", reason),
        Err(Uncertain { code, .. }) => {
            log(claim, "uncertain", code);
            poll(c, claim, RECOVERY_POLL_SECONDS).await?;
        }
    }
    Ok(())
}

/// Settles a not-yet-effectful (`observed`) cleanup of a cancelled parent as retained.
/// Returns false while a removal intent still needs an explicit resume.
pub(super) async fn settle_for_cancel(tx: &Connection, key: &str) -> Result<bool> {
    let row = tx
        .query(
            "SELECT media_type,cleanup_state,json_extract(cleanup_json,'$.phase') FROM downloaded_scan_commands WHERE id=?",
            [key],
        )
        .await?
        .next()
        .await?
        .ok_or_else(not_found)?;
    let media = MediaDomain::parse(&row.get::<String>(0)?).map_err(|_| bad())?;
    let state: String = row.get(1)?;
    let phase: Option<String> = row.get(2)?;
    drop(row);
    match (state.as_str(), phase.as_deref()) {
        ("pending", Some("observed")) => {
            tx.execute(
                "UPDATE downloaded_scan_commands SET cleanup_state='retained',cleanup_json=json_set(cleanup_json,'$.phase','settled','$.reason','cancelled') WHERE id=? AND cleanup_state='pending' AND json_extract(cleanup_json,'$.phase')='observed' AND cleanup_resume_requested=0",
                [key],
            )
            .await?;
            changed(tx, media).await?;
            Ok(true)
        }
        ("pending", _) => Ok(false),
        _ => Ok(true),
    }
}

// ---------------------------------------------------------------------------------------
// Explicit cleanup resume
// ---------------------------------------------------------------------------------------

#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ScanCleanupResumeInput {
    pub inventory_revision: u8,
    // The parent's cleanup.resume_generation as last read. Success authorizes exactly +1.
    pub expected_cleanup_resume_generation: i64,
}
pub(super) async fn resume_tv(
    state: axum::extract::State<Arc<Database>>,
    path: Path<String>,
    q: Q<Empty>,
    input: J<ScanCleanupResumeInput>,
) -> Result<Json<DownloadedScanCommand>> {
    resume(state, MediaDomain::Tv, path, q, input).await
}
pub(super) async fn resume_movies(
    state: axum::extract::State<Arc<Database>>,
    path: Path<String>,
    q: Q<Empty>,
    input: J<ScanCleanupResumeInput>,
) -> Result<Json<DownloadedScanCommand>> {
    resume(state, MediaDomain::Movies, path, q, input).await
}
fn conflict(code: &'static str) -> Error {
    Error(StatusCode::CONFLICT, code)
}
/// Authorizes exactly one more attempt of an uncertain removal intent. Holding the global
/// effect permit through commit means no import or cleanup runs while the evidence is read.
/// An exact replay of the authorized, not yet consumed generation returns without change.
async fn resume(
    axum::extract::State(db): axum::extract::State<Arc<Database>>,
    media: MediaDomain,
    Path(id): Path<String>,
    q: Q<Empty>,
    input: J<ScanCleanupResumeInput>,
) -> Result<Json<DownloadedScanCommand>> {
    q.map_err(|_| bad())?;
    let input = input.map_err(|_| bad())?.0;
    if !(0..MAX_GENERATION).contains(&input.expected_cleanup_resume_generation) {
        return Err(bad());
    }
    let id = parse_id(&id)?;
    let c = connection(&db).await?;
    let guard = crate::import::acquire_resume_guard(&db).ok();
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        let p = scoped_parent(&tx, id, media).await?;
        if !matches!(p.status, CommandStatus::Running)
            || p.revision != i64::from(input.inventory_revision)
        {
            return Err(conflict("downloaded_scan_cleanup_resume_conflict"));
        }
        let generation = p.cleanup_resume_generation;
        if p.cleanup_resume_requested && generation == input.expected_cleanup_resume_generation + 1
        {
            return bounded(render(&tx, p).await?);
        }
        if guard.is_none() {
            return Err(conflict("import_busy"));
        }
        if p.cleanup_resume_requested || generation != input.expected_cleanup_resume_generation {
            return Err(conflict("downloaded_scan_cleanup_resume_generation"));
        }
        if p.cleanup_state != "pending" || p.cleanup_phase.as_deref() != Some("remove_intent") {
            return Err(conflict("downloaded_scan_cleanup_resume_not_required"));
        }
        let key = id.hyphenated().to_string();
        let changed_rows = tx
            .execute(
                "UPDATE downloaded_scan_commands SET cleanup_resume_generation=cleanup_resume_generation+1,cleanup_resume_requested=1,next_attempt_at=? WHERE id=? AND status='running' AND cleanup_state='pending' AND cleanup_resume_requested=0 AND cleanup_resume_generation=?",
                params![now()?, key, generation],
            )
            .await?;
        if changed_rows != 1 {
            return Err(conflict("downloaded_scan_cleanup_resume_generation"));
        }
        let p = scoped_parent(&tx, id, media).await?;
        bounded(render(&tx, p).await?)
    }
    .await;
    let result = finish(tx, outcome).await;
    drop(guard);
    result
}
