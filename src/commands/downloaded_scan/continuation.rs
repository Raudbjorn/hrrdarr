//! Downloaded-scan import continuation (cmd.010 phase B).
//!
//! A published parent (`status='running'`, `inventory_revision=1`) is re-claimed by the worker
//! as a continuation: every claim increments `run_generation` and advances `ready_order_at`
//! but never `attempts`. Each worker turn performs ONE bounded step, strictly sequentially:
//!
//! 1. an `importing` child: wait while its operation is active (poll >=5s), settle it
//!    `imported` once its journal is complete, relaunch a never-armed clean preview, or mark
//!    it `failed` (measured error or abandoned marker; never retried automatically);
//! 2. a `failed` child with an explicit resume request: start it under that generation;
//! 3. an explicitly resumed cleanup removal intent (see `cleanup.rs`);
//! 4. after `cancel_requested`: cancel untouched children and settle `cancelled` once nothing
//!    is in flight (stop-after-current);
//! 5. the next `ready` child: prepare its operation and start it;
//! 6. one move-mode source cleanup checkpoint, when applicable;
//! 7. otherwise settle the parent: `succeeded` (`success` iff a child of this parent imported,
//!    else `unsuccessful`), unless a failed child holds an unresolved operation, in which case
//!    the parent stays `running` (phase `recovery_required`) and is polled rarely. Settlement
//!    records typed receipt coverage and hands a captured receipt back atomically.
use super::*;
use crate::import::{ScanChildAdmission, ScanChildStart, StartOwned};

const ACTIVE_POLL_SECONDS: i64 = 5;
const RECOVERY_POLL_SECONDS: i64 = 300;
const MAX_GENERATION: i64 = 9007199254740991;

struct Pending {
    id: String,
    operation: Option<String>,
    resume_generation: i64,
}
struct Journal {
    phase: String,
    error: Option<String>,
    marker: Option<String>,
}
impl Journal {
    fn clean_preview(&self) -> bool {
        self.phase == "preview" && self.error.is_none() && self.marker.is_none()
    }
}
fn storage(error: crate::import::Error) -> Error {
    eprintln!("event=downloaded_scan_import_error code={}", error.code());
    Error(
        StatusCode::SERVICE_UNAVAILABLE,
        "downloaded_scan_import_unavailable",
    )
}
/// Child writes are fenced to this exact claim generation of a still-running parent.
const FENCE: &str = "EXISTS(SELECT 1 FROM downloaded_scan_commands p WHERE p.id=downloaded_scan_children.command_id AND p.status='running' AND p.inventory_revision=1 AND p.run_generation=?)";

async fn poll(c: &Connection, claim: &ScanClaim, delay: i64) -> Result<()> {
    c.execute(
        "UPDATE downloaded_scan_commands SET next_attempt_at=? WHERE id=? AND status='running' AND run_generation=?",
        params![now()? + delay, claim.id.hyphenated().to_string(), claim.run_generation],
    )
    .await?;
    Ok(())
}
async fn first(c: &Connection, key: &str, filter: &str) -> Result<Option<Pending>> {
    let row = c
        .query(
            &format!("SELECT id,operation_id,resume_generation FROM downloaded_scan_children WHERE command_id=? AND {filter} ORDER BY ordinal LIMIT 1"),
            [key],
        )
        .await?
        .next()
        .await?;
    row.map(|r| {
        Ok(Pending {
            id: r.get(0)?,
            operation: r.get(1)?,
            resume_generation: r.get(2)?,
        })
    })
    .transpose()
}
async fn journal(c: &Connection, operation: &str) -> Result<Journal> {
    let r = c
        .query(
            "SELECT phase,error_code,CASE WHEN length(CAST(path_failure_code AS BLOB))<=64 THEN path_failure_code ELSE 'invalid' END FROM import_journal WHERE operation_id=?",
            [operation],
        )
        .await?
        .next()
        .await?
        .ok_or(Error(StatusCode::INTERNAL_SERVER_ERROR, "downloaded_scan_journal_missing"))?;
    Ok(Journal {
        phase: r.get(0)?,
        error: r.get(1)?,
        marker: r.get(2)?,
    })
}
fn admission(claim: &ScanClaim, child: &str) -> Result<ScanChildAdmission> {
    Ok(ScanChildAdmission {
        command_id: claim.id,
        child_id: Uuid::parse_str(child).map_err(|_| bad())?,
        inventory_revision: 1,
        run_generation: claim.run_generation,
    })
}
fn log(claim: &ScanClaim, child: &str, event: &str, code: &str) {
    eprintln!(
        "event=downloaded_scan_{event} command_id={} media_type={} child_id={child} code={code} run_generation={}",
        claim.id,
        domain(claim.media),
        claim.run_generation
    );
}

pub(super) async fn step(db: &Arc<Database>, claim: ScanClaim) -> Result<()> {
    let c = connection(db).await?;
    let key = claim.id.hyphenated().to_string();
    let Some(p) = read_parent(&c, claim.id).await? else {
        return Ok(());
    };
    if !matches!(p.status, CommandStatus::Running)
        || p.revision != 1
        || p.run_generation != claim.run_generation
    {
        return Ok(());
    }
    if let Some(child) = first(&c, &key, "disposition='importing'").await? {
        return importing(db, &c, &claim, child).await;
    }
    if let Some(child) = first(&c, &key, "disposition='failed' AND resume_requested=1").await? {
        return start(db, &c, &claim, child).await;
    }
    if super::cleanup::resume_pending(&c, &key).await? {
        super::cleanup::advance(db, &c, &claim).await?;
        return Ok(());
    }
    if p.cancel_requested {
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        let settled = settle_cancelled(&tx, &key, now()?, Some(claim.run_generation)).await;
        let settled = finish(tx, settled).await?;
        if settled {
            log(&claim, "-", "cancelled", "stop_after_current");
            return Ok(());
        }
        return poll(&c, &claim, RECOVERY_POLL_SECONDS).await;
    }
    if let Some(child) = first(&c, &key, "disposition='ready'").await? {
        return ready(db, &c, &claim, child).await;
    }
    if super::cleanup::advance(db, &c, &claim).await? {
        return Ok(());
    }
    settle(&c, &claim).await
}

/// The single in-flight child. A detached engine task keeps running across worker restarts;
/// it is observed, never re-executed, and its completion is settled exactly once.
async fn importing(
    db: &Arc<Database>,
    c: &Connection,
    claim: &ScanClaim,
    child: Pending,
) -> Result<()> {
    let operation = child.operation.clone().ok_or(Error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "downloaded_scan_link_missing",
    ))?;
    if crate::import::operation_active(&operation).map_err(storage)? {
        return poll(c, claim, ACTIVE_POLL_SECONDS).await;
    }
    let j = journal(c, &operation).await?;
    if j.phase == "complete" {
        // The child settles and its typed history credits receipt coverage together.
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        let outcome = async {
            let rows = tx
                .execute(
                    &format!("UPDATE downloaded_scan_children SET disposition='imported',error_code=NULL WHERE id=? AND disposition='importing' AND operation_id=? AND {FENCE}"),
                    params![child.id.clone(), operation, claim.run_generation],
                )
                .await?;
            if rows == 1 {
                super::receipt::credit(&tx, &claim.id.hyphenated().to_string()).await?;
            }
            Ok(())
        }
        .await;
        finish(tx, outcome).await?;
        log(claim, &child.id, "child_imported", "complete");
        return Ok(());
    }
    if j.clean_preview() {
        // The start transaction committed but the engine never armed (for example a dropped
        // worker turn). Launching the same untouched preview is the original authority.
        return start(db, c, claim, child).await;
    }
    // A measured failure or an abandoned marker: never retried without an explicit resume.
    let code = j.error.unwrap_or_else(|| "import_interrupted".into());
    c.execute(
        &format!("UPDATE downloaded_scan_children SET disposition='failed',error_code=? WHERE id=? AND disposition='importing' AND operation_id=? AND {FENCE}"),
        params![code.clone(), child.id.clone(), operation, claim.run_generation],
    )
    .await?;
    log(claim, &child.id, "child_failed", &code);
    Ok(())
}

/// Starts a linked operation under the child's current generation (0, or an explicit resume).
async fn start(
    db: &Arc<Database>,
    c: &Connection,
    claim: &ScanClaim,
    child: Pending,
) -> Result<()> {
    let request = ScanChildStart {
        claim: admission(claim, &child.id)?,
        resume_generation: child.resume_generation,
    };
    match crate::import::start_scan(db.clone(), request).await {
        Ok(StartOwned::Complete) => Ok(()),
        Ok(StartOwned::Started | StartOwned::Active | StartOwned::Busy) => {
            poll(c, claim, ACTIVE_POLL_SECONDS).await
        }
        Err(e) if e.transient() => poll(c, claim, ACTIVE_POLL_SECONDS).await,
        Err(e) if e.code() == "scan_claim_stale" => Ok(()),
        Err(e) if e.code() == "scan_cancelled" => {
            // A resume requested before the cancel cannot start a never-armed preview; consume
            // it so the cancel branch can settle the child instead of refusing every turn.
            c.execute(
                &format!("UPDATE downloaded_scan_children SET resume_requested=0 WHERE id=? AND disposition='failed' AND resume_requested=1 AND {FENCE}"),
                params![child.id.clone(), claim.run_generation],
            )
            .await?;
            Ok(())
        }
        Err(e) => {
            let code = e.code();
            log(claim, &child.id, "child_start_refused", code);
            // A refused explicit resume is consumed; the measured failure stays visible.
            c.execute(
                &format!("UPDATE downloaded_scan_children SET resume_requested=0,error_code=? WHERE id=? AND disposition='failed' AND resume_requested=1 AND {FENCE}"),
                params![code, child.id.clone(), claim.run_generation],
            )
            .await?;
            if let Some(operation) = &child.operation {
                // A refused first start leaves a resumable measured failure, not a clean preview.
                c.execute(
                    "UPDATE import_journal SET error_code=? WHERE operation_id=? AND phase='preview' AND error_code IS NULL",
                    params![code, operation.clone()],
                )
                .await?;
                c.execute(
                    &format!("UPDATE downloaded_scan_children SET disposition='failed',error_code=? WHERE id=? AND disposition IN ('ready','importing') AND {FENCE}"),
                    params![code, child.id.clone(), claim.run_generation],
                )
                .await?;
            }
            Ok(())
        }
    }
}

async fn ready(
    db: &Arc<Database>,
    c: &Connection,
    claim: &ScanClaim,
    child: Pending,
) -> Result<()> {
    if child.operation.is_none() {
        match crate::import::prepare_scan(db.clone(), admission(claim, &child.id)?).await {
            Ok(_) => {}
            Err(e) if matches!(e.code(), "scan_claim_stale" | "scan_cancelled") => return Ok(()),
            Err(e) if e.transient() => return poll(c, claim, ACTIVE_POLL_SECONDS).await,
            Err(e) => {
                // A measured pre-journal refusal: no operation exists, so nothing is uncertain.
                // A measured path cause (`hc021_*_prepare`) is durable Health evidence: it is
                // recorded and the Health generation invalidated in one transaction. Ordinary
                // decision/naming refusals are not path evidence and leave Health untouched.
                let measured = e.prepare_failure_code();
                let code = measured.unwrap_or(e.code());
                let tx = c
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .await?;
                let outcome = async {
                    let rows = tx
                        .execute(
                            &format!("UPDATE downloaded_scan_children SET disposition='failed',error_code=? WHERE id=? AND disposition='ready' AND operation_id IS NULL AND {FENCE}"),
                            params![code, child.id.clone(), claim.run_generation],
                        )
                        .await?;
                    if rows == 1 && measured.is_some() {
                        crate::health::path_diagnostics_changed(
                            &tx,
                            claim.media,
                            crate::health::PathDiagnosticChange::Failure,
                        )
                        .await
                        .map_err(|e| Error(e.0, e.1))?;
                    }
                    Ok(())
                }
                .await;
                finish(tx, outcome).await?;
                log(claim, &child.id, "child_prepare_refused", code);
                return Ok(());
            }
        }
    }
    // Build the value and drop the row before starting: the database uses a rollback journal,
    // so a live read statement on this connection would block the start transaction's commit.
    let linked = {
        let row = c
            .query(
                "SELECT operation_id,resume_generation FROM downloaded_scan_children WHERE id=? AND operation_id IS NOT NULL",
                [child.id.clone()],
            )
            .await?
            .next()
            .await?;
        match row {
            Some(row) => Pending {
                id: child.id.clone(),
                operation: row.get(0)?,
                resume_generation: row.get(1)?,
            },
            None => return Ok(()),
        }
    };
    start(db, c, claim, linked).await
}

async fn settle(c: &Connection, claim: &ScanClaim) -> Result<()> {
    let key = claim.id.hyphenated().to_string();
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        let unresolved = tx
            .query(
                "SELECT 1 FROM downloaded_scan_children c JOIN import_journal j ON j.operation_id=c.operation_id WHERE c.command_id=? AND c.disposition='failed' AND j.phase!='complete' LIMIT 1",
                [key.clone()],
            )
            .await?
            .next()
            .await?
            .is_some();
        if unresolved {
            return Ok(None);
        }
        let settled = super::receipt::finalize(
            &tx,
            &key,
            super::receipt::Terminal::Succeeded,
            Some(claim.run_generation),
            now()?,
        )
        .await?;
        Ok(Some(u64::from(settled)))
    }
    .await;
    match finish(tx, outcome).await? {
        None => {
            log(claim, "-", "recovery_required", "unresolved_child");
            poll(c, claim, RECOVERY_POLL_SECONDS).await
        }
        Some(changed) => {
            if changed == 1 {
                log(claim, "-", "settled", "succeeded");
            }
            Ok(())
        }
    }
}

/// Cancels every untouched child and settles the parent `cancelled` when nothing remains in
/// flight. Returns false while an importing or unresolved child still holds an operation.
pub(super) async fn settle_cancelled(
    tx: &Connection,
    key: &str,
    timestamp: i64,
    generation: Option<i64>,
) -> Result<bool> {
    tx.execute(
        "UPDATE downloaded_scan_children SET disposition='cancelled' WHERE command_id=? AND disposition='ready' AND (operation_id IS NULL OR EXISTS(SELECT 1 FROM import_journal j WHERE j.operation_id=downloaded_scan_children.operation_id AND j.phase='preview' AND j.error_code IS NULL AND j.path_failure_code IS NULL))",
        [key],
    )
    .await?;
    // A failed child whose journal never left preview never armed: arm moves preview to
    // staging and records the unresolved marker in one transaction. Its measured child
    // error_code remains; only the never-armed journal error is cleared so it can be cancelled.
    let mut rows = tx
        .query(
            "SELECT c.id,c.operation_id FROM downloaded_scan_children c JOIN import_journal j ON j.operation_id=c.operation_id WHERE c.command_id=? AND c.disposition='failed' AND j.phase='preview' AND j.path_failure_code IS NULL LIMIT 20001",
            [key],
        )
        .await?;
    let mut untouched = Vec::new();
    while let Some(r) = rows.next().await? {
        untouched.push((r.get::<String>(0)?, r.get::<String>(1)?));
    }
    drop(rows);
    for (child, operation) in untouched {
        if crate::import::operation_active(&operation).map_err(storage)? {
            continue;
        }
        tx.execute(
            "UPDATE import_journal SET error_code=NULL WHERE operation_id=? AND phase='preview' AND path_failure_code IS NULL",
            [operation],
        )
        .await?;
        tx.execute(
            "UPDATE downloaded_scan_children SET disposition='cancelled',resume_requested=0 WHERE id=? AND disposition='failed'",
            [child],
        )
        .await?;
    }
    let blocked = tx
        .query(
            "SELECT 1 FROM downloaded_scan_children c LEFT JOIN import_journal j ON j.operation_id=c.operation_id WHERE c.command_id=? AND (c.disposition IN ('ready','importing') OR (c.operation_id IS NOT NULL AND j.phase!='complete' AND c.disposition!='cancelled')) LIMIT 1",
            [key],
        )
        .await?
        .next()
        .await?
        .is_some();
    if blocked {
        return Ok(false);
    }
    // A cleanup that never recorded removal intent is retained as cancelled; an armed
    // removal intent keeps the parent running until it is explicitly resumed and settled.
    if !super::cleanup::settle_for_cancel(tx, key).await? {
        return Ok(false);
    }
    super::receipt::finalize(
        tx,
        key,
        super::receipt::Terminal::Cancelled,
        generation,
        timestamp,
    )
    .await?;
    Ok(true)
}

// ---------------------------------------------------------------------------------------
// Explicit child resume
// ---------------------------------------------------------------------------------------

#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ScanChildResumeInput {
    // The child's linked operation as last read; resume never names a different operation.
    pub operation_id: Uuid,
    pub inventory_revision: u8,
    // The child's resume_generation as last read. Success authorizes exactly generation+1.
    pub expected_resume_generation: i64,
}
pub(super) async fn resume_tv(
    state: State<Arc<Database>>,
    path: Path<(String, String)>,
    q: Q<Empty>,
    input: J<ScanChildResumeInput>,
) -> Result<Json<ScanChild>> {
    resume(state, MediaDomain::Tv, path, q, input).await
}
pub(super) async fn resume_movies(
    state: State<Arc<Database>>,
    path: Path<(String, String)>,
    q: Q<Empty>,
    input: J<ScanChildResumeInput>,
) -> Result<Json<ScanChild>> {
    resume(state, MediaDomain::Movies, path, q, input).await
}
fn resume_conflict(code: &'static str) -> Error {
    Error(StatusCode::CONFLICT, code)
}
/// Increments the child's monotone resume generation and requests one more execution of the
/// SAME operation. Holding the process execution permit through commit means no import can
/// arm while the durable evidence is read. A lost response is reconciled by GET or by
/// repeating the identical body: an already-authorized, not yet consumed generation replays
/// without increment. A replay mutates nothing, so it is answered even while an import holds
/// the permit; every authorizing path requires the permit (`409 import_busy` otherwise).
async fn resume(
    State(db): State<Arc<Database>>,
    media: MediaDomain,
    Path((id, child_id)): Path<(String, String)>,
    q: Q<Empty>,
    input: J<ScanChildResumeInput>,
) -> Result<Json<ScanChild>> {
    q.map_err(|_| bad())?;
    let input = input.map_err(|_| bad())?.0;
    if !(0..MAX_GENERATION).contains(&input.expected_resume_generation) {
        return Err(bad());
    }
    let id = parse_id(&id)?;
    let child_id = parse_id(&child_id)?;
    let c = connection(&db).await?;
    let guard = crate::import::acquire_resume_guard(&db).ok();
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        let p = scoped_parent(&tx, id, media).await?;
        let key = child_id.hyphenated().to_string();
        let row = tx
            .query(
                "SELECT c.operation_id,c.disposition,c.resume_generation,c.resume_requested,j.phase,j.error_code IS NOT NULL OR j.path_failure_code IS NOT NULL FROM downloaded_scan_children c LEFT JOIN import_journal j ON j.operation_id=c.operation_id WHERE c.id=? AND c.command_id=?",
                params![key.clone(), id.hyphenated().to_string()],
            )
            .await?
            .next()
            .await?
            .ok_or_else(not_found)?;
        let operation: Option<String> = row.get(0)?;
        let disposition: String = row.get(1)?;
        let generation: i64 = row.get(2)?;
        let requested = row.get::<i64>(3)? == 1;
        let phase: Option<String> = row.get(4)?;
        let unresolved = row.get::<Option<i64>>(5)? == Some(1);
        drop(row);
        if !matches!(p.status, CommandStatus::Running)
            || p.revision != i64::from(input.inventory_revision)
            || operation.as_deref() != Some(input.operation_id.hyphenated().to_string().as_str())
        {
            return Err(resume_conflict("downloaded_scan_resume_conflict"));
        }
        if requested && generation == input.expected_resume_generation + 1 {
            // Exact replay of an already-authorized generation: no second authorization.
            return read_child(&tx, id, &key).await;
        }
        if guard.is_none() {
            return Err(resume_conflict("import_busy"));
        }
        if requested || generation != input.expected_resume_generation {
            return Err(resume_conflict("downloaded_scan_resume_generation"));
        }
        if disposition != "failed"
            || phase.as_deref().is_none_or(|phase| phase == "complete")
            || !unresolved
        {
            return Err(resume_conflict("downloaded_scan_resume_not_required"));
        }
        // Cancellation only permits finishing an already effectful child.
        if p.cancel_requested && phase.as_deref() == Some("preview") {
            return Err(resume_conflict("downloaded_scan_cancelled"));
        }
        let changed = tx
            .execute(
                "UPDATE downloaded_scan_children SET resume_generation=resume_generation+1,resume_requested=1 WHERE id=? AND disposition='failed' AND resume_generation=? AND resume_requested=0",
                params![key.clone(), generation],
            )
            .await?;
        if changed != 1 {
            return Err(resume_conflict("downloaded_scan_resume_generation"));
        }
        tx.execute(
            "UPDATE downloaded_scan_commands SET next_attempt_at=? WHERE id=? AND status='running'",
            params![now()?, id.hyphenated().to_string()],
        )
        .await?;
        read_child(&tx, id, &key).await
    }
    .await;
    let result = finish(tx, outcome).await;
    drop(guard);
    result
}
async fn read_child(c: &Connection, id: Uuid, child_id: &str) -> Result<Json<ScanChild>> {
    let row = c
        .query(
            &format!("SELECT {CHILD_COLUMNS} FROM {CHILD_FROM} WHERE c.command_id=? AND c.id=?"),
            params![id.hyphenated().to_string(), child_id],
        )
        .await?
        .next()
        .await?
        .ok_or_else(not_found)?;
    bounded(super::child(row)?)
}
