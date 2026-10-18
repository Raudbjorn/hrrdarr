//! Tracked receipt reservation, typed coverage and the settled processing handback.
//!
//! A tracked request resolves strictly by provider + domain + remote id. A known observed
//! receipt in the route's domain is captured on the parent (candidate, tracked authority, the
//! exact processing fence and any linked predecessor operation) inside the publication
//! transaction, or earlier when a linked predecessor is still finishing: the parent then waits
//! (phase `waiting_predecessor`) without stealing, starting or cancelling that operation. No
//! processing row is ever created for an absent receipt. Expected coverage (the receipt's
//! episodes, or its movie) is sealed with the inventory; credits are write-once proofs from the
//! predecessor's history or this scan's completed children. At terminal settlement the parent
//! hands the receipt back in the same transaction: full coverage -> `imported`, anything else
//! -> `blocked` with `downloaded_scan_incomplete`, preserving any hc021 marker.
use super::*;
use crate::commands::processing::{PathReasonChange, path_reasons};

/// Receipt-level evaluator refusals: the captured authority itself changed.
const RECEIPT_CODES: &[&str] = &[
    "release_authority_changed",
    "release_target_invalid",
    "release_target_missing",
    "invalid_receipt_authority",
];
const PREDECESSOR_POLL_SECONDS: i64 = 5;
const PREDECESSOR_IDLE_POLL_SECONDS: i64 = 30;

#[derive(Clone)]
pub(super) struct Receipt {
    pub(super) candidate: Uuid,
    provider: Uuid,
    provider_revision: i64,
    remote: String,
    predecessor: Option<String>,
    /// Already captured by an earlier generation of this parent.
    pub(super) captured: bool,
}
impl Receipt {
    pub(super) fn authority(&self, media: MediaDomain) -> ScanAuthority {
        ScanAuthority::Receipt {
            candidate_id: self.candidate,
            provider_id: self.provider,
            provider_revision: self.provider_revision,
            media_type: media,
            remote_id: self.remote.clone(),
        }
    }
}
pub(super) fn receipt_code(code: &str) -> bool {
    RECEIPT_CODES.contains(&code)
}

async fn predecessor_complete(c: &Connection, operation: &str) -> Result<bool> {
    Ok(c.query(
        "SELECT 1 FROM import_journal j JOIN import_history h ON h.operation_id=j.operation_id WHERE j.operation_id=? AND j.phase='complete'",
        [operation],
    )
    .await?
    .next()
    .await?
    .is_some())
}
/// Active predecessors are polled promptly; an idle unfinished one (for example awaiting its own
/// explicit resume) is polled less often. Neither consumes a scan attempt.
fn waiting(operation: &str) -> std::result::Result<Failure, Failure> {
    let active = crate::import::operation_active(operation)
        .map_err(|_| Failure::Retry("scan_storage_error"))?;
    Ok(Failure::Waiting(if active {
        PREDECESSOR_POLL_SECONDS
    } else {
        PREDECESSOR_IDLE_POLL_SECONDS
    }))
}

/// `Ok(None)`: untracked, or an unknown remote id that falls back visibly to ordinary
/// identification. Known-but-unusable authority fails; it never degrades into a fallback.
pub(super) async fn resolve(
    c: &Connection,
    claim: &ScanClaim,
) -> std::result::Result<Option<Receipt>, Failure> {
    let Some((provider, remote)) = &claim.requested else {
        return Ok(None);
    };
    let key = claim.id.hyphenated().to_string();
    let captured = c
        .query(
            "SELECT candidate_id,predecessor_operation_id,json_extract(tracked_json,'$.provider_revision') FROM downloaded_scan_commands WHERE id=? AND candidate_id IS NOT NULL",
            [key.clone()],
        )
        .await?
        .next()
        .await?;
    if let Some(row) = captured {
        let receipt = Receipt {
            candidate: Uuid::parse_str(&row.get::<String>(0)?)
                .map_err(|_| Failure::Fail("scan_facts_invalid"))?,
            provider: Uuid::parse_str(provider).map_err(|_| Failure::Fail("scan_facts_invalid"))?,
            provider_revision: row.get(2)?,
            remote: remote.clone(),
            predecessor: row.get(1)?,
            captured: true,
        };
        drop(row);
        if let Some(operation) = &receipt.predecessor {
            if !predecessor_complete(c, operation).await? {
                return Err(waiting(operation)?);
            }
        }
        return Ok(Some(receipt));
    }
    let known = c
        .query(
            "SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id AND s.media_type=? WHERE p.id=?",
            params![domain(claim.media), provider.clone()],
        )
        .await?
        .next()
        .await?
        .is_some();
    if !known {
        return Err(Failure::Fail("scan_tracked_provider_unavailable"));
    }
    // An explicit successor continues exactly its predecessor's captured receipt.
    let lineage: Option<String> = match &claim.supersedes {
        None => None,
        Some(previous) => c
            .query(
                "SELECT candidate_id FROM downloaded_scan_commands WHERE id=? AND coverage_state='incomplete'",
                [previous.clone()],
            )
            .await?
            .next()
            .await?
            .map(|r| r.get::<Option<String>>(0))
            .transpose()?
            .flatten()
            .map(Some)
            .ok_or(Failure::Fail("scan_successor_receipt_changed"))?,
    };
    // (client, observed hash) is unique across domains, so this is the only possible receipt.
    let Some(row) = c
        .query(
            "SELECT r.id,r.media_type,r.status,r.client_revision,v.enabled,v.revision FROM rss_candidates r JOIN providers v ON v.id=r.client_id WHERE r.client_id=? AND r.observed_hash=?",
            params![provider.clone(), remote.clone()],
        )
        .await?
        .next()
        .await?
    else {
        // A successor never degrades into ordinary identification.
        if lineage.is_some() {
            return Err(Failure::Fail("scan_successor_receipt_changed"));
        }
        return Ok(None);
    };
    let candidate: String = row.get(0)?;
    if lineage
        .as_deref()
        .is_some_and(|expected| expected != candidate)
    {
        return Err(Failure::Fail("scan_successor_receipt_changed"));
    }
    if row.get::<String>(1)? != domain(claim.media) {
        // The same remote item is a receipt of the other domain: it cannot authorize this one.
        return Err(Failure::Fail("scan_tracked_receipt_domain"));
    }
    let provider_revision: i64 = row.get(3)?;
    if row.get::<String>(2)? != "observed"
        || row.get::<i64>(4)? != 1
        || row.get::<i64>(5)? != provider_revision
    {
        return Err(Failure::Fail("scan_tracked_receipt_unavailable"));
    }
    drop(row);
    if c.query(
        "WITH RECURSIVE ancestry(id,previous,depth) AS (SELECT id,supersedes_command_id,1 FROM downloaded_scan_commands WHERE id=?3 UNION ALL SELECT a.id,a.supersedes_command_id,x.depth+1 FROM downloaded_scan_commands a JOIN ancestry x ON a.id=x.previous WHERE x.depth<1024) SELECT 1 FROM downloaded_scan_commands WHERE candidate_id=?1 AND id!=?2 AND (status IN ('queued','running','retry_wait') OR id NOT IN (SELECT id FROM ancestry)) LIMIT 1",
        params![candidate.clone(), key, claim.supersedes.clone()],
    )
    .await?
    .next()
    .await?
    .is_some()
    {
        // Another scan holds (or settled) this receipt outside this request's explicit
        // lineage; only an explicit successor of the latest settled holder may continue it.
        return Err(Failure::Fail("scan_tracked_receipt_reserved"));
    }
    let processing = c
        .query(
            "SELECT status FROM download_processing WHERE candidate_id=?",
            [candidate.clone()],
        )
        .await?
        .next()
        .await?
        .map(|r| r.get::<String>(0))
        .transpose()?;
    if processing.as_deref() == Some("checking") {
        // The preflight owner is inspecting the client: never stolen.
        return Err(Failure::Retry("scan_tracked_receipt_busy"));
    }
    let predecessor = c
        .query(
            "SELECT operation_id FROM rss_candidate_imports WHERE candidate_id=?",
            [candidate.clone()],
        )
        .await?
        .next()
        .await?
        .map(|r| r.get::<String>(0))
        .transpose()?;
    let receipt = Receipt {
        candidate: Uuid::parse_str(&candidate).map_err(|_| Failure::Fail("scan_facts_invalid"))?,
        provider: Uuid::parse_str(provider).map_err(|_| Failure::Fail("scan_facts_invalid"))?,
        provider_revision,
        remote: remote.clone(),
        predecessor,
        captured: false,
    };
    if let Some(operation) = receipt.predecessor.clone() {
        if !predecessor_complete(c, &operation).await? {
            // Reserve now so no new singleton ownership can start, then wait for the original
            // owner to finish under its own authority.
            let tx = c
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .await?;
            let outcome = async {
                live(&tx, claim).await?;
                capture(&tx, claim, &receipt).await?;
                Ok::<_, Failure>(())
            }
            .await;
            match outcome {
                Ok(()) => tx.commit().await?,
                Err(failure) => {
                    tx.rollback().await?;
                    return Err(failure);
                }
            }
            eprintln!(
                "event=downloaded_scan_receipt_captured command_id={} media_type={} candidate_id={} waiting_for={operation}",
                claim.id,
                domain(claim.media),
                receipt.candidate
            );
            return Err(waiting(&operation)?);
        }
    }
    Ok(Some(receipt))
}

/// Captures the exact receipt authority and processing fence. Must run while the parent is the
/// current unpublished claim; the 0054 capture triggers re-verify every field.
pub(super) async fn capture(
    tx: &Connection,
    claim: &ScanClaim,
    receipt: &Receipt,
) -> std::result::Result<(), Failure> {
    let candidate = receipt.candidate.hyphenated().to_string();
    let fence: String = tx
        .query(
            "SELECT coalesce((SELECT json_object('version',1,'policy_revision',policy_revision,'status',status,'resume_requested',resume_requested,'preflight_attempts',preflight_attempts,'total_preflight_attempts',total_preflight_attempts,'next_attempt_at',next_attempt_at,'created_at',created_at,'updated_at',updated_at,'error_code',error_code,'reasons_json',reasons_json,'scan_command_id',scan_command_id) FROM download_processing WHERE candidate_id=?1),json_object('version',1,'absent',json('true')))",
            [candidate.clone()],
        )
        .await?
        .next()
        .await?
        .ok_or(Failure::Retry("scan_storage_error"))?
        .get(0)?;
    let tracked = serde_json::json!({
        "version": 1,
        "provider_id": receipt.provider.hyphenated().to_string(),
        "provider_revision": receipt.provider_revision,
        "remote_id": receipt.remote,
    })
    .to_string();
    let changed = tx
        .execute(
            "UPDATE downloaded_scan_commands SET candidate_id=?,predecessor_operation_id=?,tracked_json=?,processing_fence_json=?,coverage_state='pending' WHERE id=? AND status='running' AND run_generation=? AND inventory_revision=0 AND cancel_requested=0 AND candidate_id IS NULL",
            params![
                candidate,
                receipt.predecessor.clone(),
                tracked,
                fence,
                claim.id.hyphenated().to_string(),
                claim.run_generation
            ],
        )
        .await?;
    if changed != 1 {
        return Err(Failure::Stale);
    }
    Ok(())
}

/// Seals the receipt's complete typed expected coverage with the inventory (revision 0).
pub(super) async fn expect_coverage(
    tx: &Connection,
    command: &str,
    media: MediaDomain,
    receipt: &Receipt,
) -> std::result::Result<(), Failure> {
    let candidate = receipt.candidate.hyphenated().to_string();
    let sql = match media {
        MediaDomain::Tv => {
            "INSERT INTO downloaded_scan_coverage(command_id,episode_id) SELECT ?1,episode_id FROM rss_candidate_episodes WHERE candidate_id=?2 ORDER BY episode_id"
        }
        MediaDomain::Movies => {
            "INSERT INTO downloaded_scan_coverage(command_id,movie_id) SELECT ?1,movie_id FROM rss_candidates WHERE id=?2 AND movie_id IS NOT NULL"
        }
    };
    if tx.execute(sql, params![command, candidate]).await? == 0 {
        return Err(Failure::Fail("scan_tracked_receipt_unavailable"));
    }
    Ok(())
}

/// Credits every still-uncredited expected member that the predecessor's history or one of this
/// parent's completed children proves, against the CURRENT association. Write-once; idempotent.
/// The WHERE clauses repeat the 0054 `scan_coverage_credit` proof so a stale association is
/// simply left uncredited instead of aborting the caller's transaction.
pub(super) async fn credit(tx: &Connection, command: &str) -> Result<()> {
    let row = tx
        .query(
            "SELECT media_type,candidate_id IS NOT NULL FROM downloaded_scan_commands WHERE id=? AND status='running' AND inventory_revision=1",
            [command],
        )
        .await?
        .next()
        .await?;
    let Some(row) = row else { return Ok(()) };
    let media: String = row.get(0)?;
    if row.get::<i64>(1)? != 1 {
        return Ok(());
    }
    drop(row);
    let mut rows = tx
        .query(
            "SELECT p.predecessor_operation_id FROM downloaded_scan_commands p JOIN import_journal j ON j.operation_id=p.predecessor_operation_id JOIN import_history h ON h.operation_id=j.operation_id WHERE p.id=?1 AND j.phase='complete' UNION SELECT c.operation_id FROM downloaded_scan_children c JOIN import_journal j ON j.operation_id=c.operation_id JOIN import_history h ON h.operation_id=j.operation_id WHERE c.command_id=?1 AND c.disposition='imported' AND j.phase='complete' UNION SELECT v.credited_operation_id FROM downloaded_scan_commands p JOIN downloaded_scan_coverage v ON v.command_id=p.supersedes_command_id JOIN import_journal j ON j.operation_id=v.credited_operation_id WHERE p.id=?1 AND v.credited_operation_id IS NOT NULL AND j.phase='complete' LIMIT 20002",
            [command],
        )
        .await?;
    let mut operations = Vec::new();
    while let Some(r) = rows.next().await? {
        operations.push(r.get::<String>(0)?);
    }
    drop(rows);
    if operations.len() > 20_001 {
        return Err(Error(StatusCode::CONFLICT, "scan_coverage_limit"));
    }
    // The authority clause mirrors the trigger's three sources: this receipt's linked
    // predecessor operation, this parent's imported child, or an explicit predecessor scan's
    // credit for the SAME member and the SAME (still current) file. A prior credit whose file
    // was since replaced is simply not carried.
    let sql = if media == "tv" {
        "UPDATE downloaded_scan_coverage SET credited_operation_id=?2,credited_file_id=(SELECT e.episode_file_id FROM episodes e WHERE e.id=downloaded_scan_coverage.episode_id) WHERE command_id=?1 AND credited_operation_id IS NULL AND episode_id IS NOT NULL AND EXISTS(SELECT 1 FROM import_history h JOIN episodes e ON e.id=downloaded_scan_coverage.episode_id JOIN episode_files ef ON ef.id=e.episode_file_id AND ef.series_id=e.series_id WHERE h.operation_id=?2 AND h.media_type='episode' AND ef.path=h.destination AND ((h.episode_id=e.id AND h.episode_file_id=ef.id) OR EXISTS(SELECT 1 FROM import_history_episodes hm WHERE hm.operation_id=h.operation_id AND hm.episode_id=e.id AND hm.episode_file_id=ef.id))) AND (EXISTS(SELECT 1 FROM downloaded_scan_commands p WHERE p.id=?1 AND p.predecessor_operation_id=?2) OR EXISTS(SELECT 1 FROM downloaded_scan_children c WHERE c.command_id=?1 AND c.operation_id=?2 AND c.disposition='imported') OR EXISTS(SELECT 1 FROM downloaded_scan_commands p JOIN downloaded_scan_coverage v ON v.command_id=p.supersedes_command_id WHERE p.id=?1 AND v.credited_operation_id=?2 AND v.episode_id=downloaded_scan_coverage.episode_id AND v.credited_file_id=(SELECT e.episode_file_id FROM episodes e WHERE e.id=downloaded_scan_coverage.episode_id)))"
    } else {
        "UPDATE downloaded_scan_coverage SET credited_operation_id=?2,credited_file_id=(SELECT h.movie_file_id FROM import_history h WHERE h.operation_id=?2) WHERE command_id=?1 AND credited_operation_id IS NULL AND movie_id IS NOT NULL AND EXISTS(SELECT 1 FROM import_history h JOIN movie_files mf ON mf.id=h.movie_file_id AND mf.movie_id=h.movie_id WHERE h.operation_id=?2 AND h.media_type='movie' AND h.movie_id=downloaded_scan_coverage.movie_id AND mf.path=h.destination) AND (EXISTS(SELECT 1 FROM downloaded_scan_commands p WHERE p.id=?1 AND p.predecessor_operation_id=?2) OR EXISTS(SELECT 1 FROM downloaded_scan_children c WHERE c.command_id=?1 AND c.operation_id=?2 AND c.disposition='imported') OR EXISTS(SELECT 1 FROM downloaded_scan_commands p JOIN downloaded_scan_coverage v ON v.command_id=p.supersedes_command_id WHERE p.id=?1 AND v.credited_operation_id=?2 AND v.movie_id=downloaded_scan_coverage.movie_id AND v.credited_file_id=(SELECT h.movie_file_id FROM import_history h WHERE h.operation_id=?2)))"
    };
    for operation in operations {
        tx.execute(sql, params![command, operation]).await?;
    }
    Ok(())
}

/// Expected members an explicit predecessor already proved against the CURRENT association
/// (the same file is still attached). A successor's inventory treats a file whose every member
/// is carried as already covered, so only explicitly authorized remaining work is selected.
pub(super) async fn carried_members(
    c: &Connection,
    claim: &ScanClaim,
) -> std::result::Result<HashSet<i64>, Failure> {
    let Some(previous) = &claim.supersedes else {
        return Ok(HashSet::new());
    };
    let sql = match claim.media {
        MediaDomain::Tv => {
            "SELECT v.episode_id FROM downloaded_scan_coverage v JOIN import_journal j ON j.operation_id=v.credited_operation_id JOIN episodes e ON e.id=v.episode_id WHERE v.command_id=? AND v.episode_id IS NOT NULL AND j.phase='complete' AND e.episode_file_id=v.credited_file_id LIMIT 20001"
        }
        MediaDomain::Movies => {
            "SELECT v.movie_id FROM downloaded_scan_coverage v JOIN import_journal j ON j.operation_id=v.credited_operation_id JOIN movie_files mf ON mf.id=v.credited_file_id AND mf.movie_id=v.movie_id WHERE v.command_id=? AND v.movie_id IS NOT NULL AND j.phase='complete' LIMIT 20001"
        }
    };
    let mut rows = c.query(sql, [previous.clone()]).await?;
    let mut members = HashSet::new();
    while let Some(r) = rows.next().await? {
        members.insert(r.get::<i64>(0)?);
    }
    if members.len() > 20_000 {
        return Err(Failure::Fail("scan_decision_limit"));
    }
    Ok(members)
}

#[derive(Clone, Copy)]
pub(super) enum Terminal {
    Succeeded,
    Cancelled,
}
const SETTLED_COVERAGE: &str = "coverage_state=CASE WHEN candidate_id IS NULL THEN 'untracked' WHEN EXISTS(SELECT 1 FROM downloaded_scan_coverage v WHERE v.command_id=downloaded_scan_commands.id) AND NOT EXISTS(SELECT 1 FROM downloaded_scan_coverage v WHERE v.command_id=downloaded_scan_commands.id AND v.credited_operation_id IS NULL) THEN 'complete' ELSE 'incomplete' END";

/// Settles a published parent and, in the same transaction, its immutable coverage proof and
/// the receipt handback. Returns whether this call settled it.
pub(super) async fn finalize(
    tx: &Connection,
    command: &str,
    terminal: Terminal,
    generation: Option<i64>,
    timestamp: i64,
) -> Result<bool> {
    credit(tx, command).await?;
    let sql = match terminal {
        Terminal::Succeeded => format!(
            "UPDATE downloaded_scan_commands SET status='succeeded',outcome=CASE WHEN EXISTS(SELECT 1 FROM downloaded_scan_children c WHERE c.command_id=downloaded_scan_commands.id AND c.disposition='imported') THEN 'success' ELSE 'unsuccessful' END,completed_at=?1,error_code=NULL,{SETTLED_COVERAGE} WHERE id=?2 AND status='running' AND inventory_revision=1 AND (?3 IS NULL OR run_generation=?3) AND NOT EXISTS(SELECT 1 FROM downloaded_scan_children c WHERE c.command_id=downloaded_scan_commands.id AND c.disposition IN ('ready','importing'))"
        ),
        Terminal::Cancelled => format!(
            "UPDATE downloaded_scan_commands SET status='cancelled',completed_at=?1,error_code=NULL,{SETTLED_COVERAGE} WHERE id=?2 AND status='running' AND cancel_requested=1 AND inventory_revision=1 AND (?3 IS NULL OR run_generation=?3)"
        ),
    };
    let changed = tx
        .execute(&sql, params![timestamp, command, generation])
        .await?;
    if changed != 1 {
        return Ok(false);
    }
    handback(tx, command, timestamp).await?;
    Ok(true)
}

/// Hands a captured receipt back at settlement. An absent processing row stays absent; a
/// receipt whose linked predecessor still owns processing is left to that owner.
async fn handback(tx: &Connection, command: &str, timestamp: i64) -> Result<()> {
    let row = tx
        .query(
            "SELECT p.candidate_id,p.coverage_state,p.media_type,json_type(p.processing_fence_json,'$.absent') IS 'true',EXISTS(SELECT 1 FROM rss_candidate_imports i WHERE i.candidate_id=p.candidate_id),EXISTS(SELECT 1 FROM download_processing d WHERE d.candidate_id=p.candidate_id),CASE WHEN json_type(p.processing_fence_json,'$.scan_command_id') IS 'text' THEN json_extract(p.processing_fence_json,'$.scan_command_id') END FROM downloaded_scan_commands p WHERE p.id=? AND p.candidate_id IS NOT NULL",
            [command],
        )
        .await?
        .next()
        .await?;
    let Some(row) = row else { return Ok(()) };
    let candidate: String = row.get(0)?;
    let complete = row.get::<String>(1)? == "complete";
    let media = MediaDomain::parse(&row.get::<String>(2)?).map_err(|_| bad())?;
    let absent = row.get::<i64>(3)? == 1;
    let linked = row.get::<i64>(4)? == 1;
    let exists = row.get::<i64>(5)? == 1;
    // An explicit successor captured the previous lineage handback as its fence; it must still
    // be exactly that row (the 0054 handback trigger repeats the full fence comparison).
    let fenced: Option<String> = row.get(6)?;
    drop(row);
    if absent || linked {
        if absent && exists {
            // The insert trigger forbids this; never paper over it.
            return Err(Error(StatusCode::CONFLICT, "scan_receipt_fence_changed"));
        }
        eprintln!(
            "event=downloaded_scan_receipt_released command_id={command} candidate_id={candidate} processing={} complete={complete}",
            if absent {
                "absent"
            } else {
                "predecessor_owner"
            }
        );
        return Ok(());
    }
    let previous: String = tx
        .query(
            "SELECT reasons_json FROM download_processing WHERE candidate_id=? AND scan_command_id IS ?",
            params![candidate.clone(), fenced.clone()],
        )
        .await?
        .next()
        .await?
        .ok_or(Error(StatusCode::CONFLICT, "scan_receipt_fence_changed"))?
        .get(0)?;
    let previous: Vec<String> = serde_json::from_str(&previous).map_err(|_| bad())?;
    let (status, error, reasons, change) = if complete {
        (
            "imported",
            None,
            path_reasons(&previous, &[], PathReasonChange::Complete)?,
            crate::health::PathDiagnosticChange::Completion,
        )
    } else {
        (
            "blocked",
            Some("import_failed"),
            path_reasons(
                &previous,
                &["downloaded_scan_incomplete".into()],
                PathReasonChange::Preserve,
            )?,
            crate::health::PathDiagnosticChange::Failure,
        )
    };
    let reasons = serde_json::to_string(&reasons).map_err(|_| bad())?;
    let changed = tx
        .execute(
            "UPDATE download_processing SET scan_command_id=?,status=?,error_code=?,reasons_json=?,updated_at=max(updated_at,?) WHERE candidate_id=? AND scan_command_id IS ?",
            params![command, status, error, reasons, timestamp, candidate.clone(), fenced],
        )
        .await?;
    if changed != 1 {
        return Err(Error(StatusCode::CONFLICT, "scan_receipt_fence_changed"));
    }
    crate::health::path_diagnostics_changed(tx, media, change)
        .await
        .map_err(|e| Error(e.0, e.1))?;
    eprintln!(
        "event=downloaded_scan_receipt_handback command_id={command} candidate_id={candidate} status={status}"
    );
    Ok(())
}
