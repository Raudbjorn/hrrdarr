//! Downloaded-folder scan producer seam (cmd.010 phase B).
//!
//! One published, selected scan child becomes exactly one physical import operation that
//! reuses the existing journal engine (`run`/`run_inner`): stage, verify, publish, one short
//! commit transaction for the file, every member association and the history membership,
//! then per-original retirement. The scan facts, the write-once child link and one
//! `scan_import_replacements` row per distinct original are the producer authority. Generic
//! execute and manual-command admission refuse these operations; only the scan command (an
//! initial start or an explicit resume generation) may launch one. No database transaction is
//! held across a filesystem effect.
use super::*;
use crate::{
    api::MediaDomain,
    search::{
        ReleaseTarget, SearchError,
        downloaded::{
            ScanAuthority, ScanDecisionProof, ScanFileDecision, ScanFileInput, ScanPolicyEligible,
            evaluate_scan_file, warm_scan_file,
        },
    },
};
use fs::{OldFile, Retirement};

const MAX_MEMBERS: usize = 20_000;
const MAX_REPLACEMENT_JSON_BYTES: usize = 131_072;
const MAX_PROOF_BYTES: usize = 16_384;

/// Queue identity and fences only; the importer reloads every fact from published storage.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ScanChildAdmission {
    pub command_id: Uuid,
    pub child_id: Uuid,
    pub inventory_revision: i64,
    pub run_generation: i64,
}
/// `resume_generation` is the child's current generation: 0 for the first execution, or the
/// generation an explicit resume request incremented. A stale value cannot start anything.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ScanChildStart {
    pub claim: ScanChildAdmission,
    pub resume_generation: i64,
}

struct Replacement {
    old: OldFile,
    state: String,
    retirement: Option<Retirement>,
}
struct Facts {
    child_id: String,
    quality_id: i64,
    revision_json: String,
    edition: Option<String>,
    evidence: crate::custom_formats::Evidence,
    members: Vec<(i64, Option<i64>)>,
    expected_old_movie: Option<i64>,
    replacements: Vec<Replacement>,
}

fn target_changed(message: &'static str) -> Error {
    Error::conflict("target_changed", message)
}
fn decision_changed() -> Error {
    Error::conflict(
        "scan_decision_changed",
        "Downloaded file no longer passes the published scan decision",
    )
}
fn search(error: SearchError) -> Error {
    match error.0 {
        "release_storage_error"
        | "scan_decision_timeout"
        | "scan_warm_requires_autocommit"
        | "scan_snapshot_required" => Error::internal(),
        _ => decision_changed(),
    }
}
fn domain(t: &MediaTarget) -> &'static str {
    match t {
        MediaTarget::Episode(_) => "tv",
        MediaTarget::Movie(_) => "movies",
    }
}

pub(super) async fn is_scan(c: &Connection, operation: &str) -> Result<bool> {
    Ok(c.query(
        "SELECT 1 FROM scan_import_facts WHERE operation_id=?",
        [operation],
    )
    .await?
    .next()
    .await?
    .is_some())
}
async fn members(c: &Connection, child: &str) -> Result<Vec<(i64, Option<i64>)>> {
    let mut rows = c
        .query(
            "SELECT episode_id,expected_old_episode_file_id FROM downloaded_scan_child_episodes WHERE child_id=? ORDER BY episode_id LIMIT ?",
            params![child, (MAX_MEMBERS + 1) as i64],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(r) = rows.next().await? {
        out.push((r.get(0)?, r.get(1)?));
    }
    if out.len() > MAX_MEMBERS {
        return Err(Error::internal());
    }
    Ok(out)
}
async fn facts(c: &Connection, operation: &str) -> Result<Facts> {
    let r = c
        .query(
            "SELECT f.child_id,f.quality_id,f.revision_json,f.edition,f.comparison_facts_json,c.expected_old_movie_file_id,p.media_type FROM scan_import_facts f JOIN downloaded_scan_children c ON c.id=f.child_id JOIN downloaded_scan_commands p ON p.id=f.command_id WHERE f.operation_id=?",
            [operation],
        )
        .await?
        .next()
        .await?
        .ok_or_else(Error::internal)?;
    let revision_json: String = r.get(2)?;
    let revision: crate::media_files::FileRevision =
        serde_json::from_str(&revision_json).map_err(|_| Error::internal())?;
    revision.validate().map_err(|_| Error::internal())?;
    let evidence: crate::custom_formats::Evidence =
        serde_json::from_str(&r.get::<String>(4)?).map_err(|_| Error::internal())?;
    let media = MediaDomain::parse(&r.get::<String>(6)?).map_err(|_| Error::internal())?;
    crate::custom_formats::validate_evidence(media, &evidence).map_err(|_| Error::internal())?;
    let child_id: String = r.get(0)?;
    let quality_id = r.get(1)?;
    let edition = r.get(3)?;
    let expected_old_movie = r.get(5)?;
    drop(r);
    let members = members(c, &child_id).await?;
    let mut rows = c
        .query(
            "SELECT CASE WHEN typeof(old_file_json)='text' AND length(CAST(old_file_json AS BLOB))<=? THEN old_file_json END,retirement_state,retirement_json FROM scan_import_replacements WHERE operation_id=? ORDER BY old_file_id LIMIT ?",
            params![MAX_REPLACEMENT_JSON_BYTES as i64, operation, (MAX_MEMBERS + 1) as i64],
        )
        .await?;
    let mut replacements = Vec::new();
    while let Some(r) = rows.next().await? {
        let old = r.get::<Option<String>>(0)?.ok_or_else(Error::internal)?;
        replacements.push(Replacement {
            old: serde_json::from_str(&old).map_err(|_| Error::internal())?,
            state: r.get(1)?,
            retirement: r
                .get::<Option<String>>(2)?
                .map(|v| serde_json::from_str(&v).map_err(|_| Error::internal()))
                .transpose()?,
        });
    }
    if replacements.len() > MAX_MEMBERS {
        return Err(Error::internal());
    }
    Ok(Facts {
        child_id,
        quality_id,
        revision_json,
        edition,
        evidence,
        members,
        expected_old_movie,
        replacements,
    })
}

/// Structural recheck used before every effect and inside commit: owner/root, every member's
/// frozen association and every captured original. Policy was re-evaluated at prepare; the
/// 0054 triggers reserve the owner's path, settings and member identities meanwhile.
pub(super) async fn before(
    c: &Connection,
    operation: &str,
    t: &MediaTarget,
) -> Result<(i64, String, Option<i64>)> {
    let f = facts(c, operation).await?;
    let current = owned::ownership(c, t).await?;
    match t {
        MediaTarget::Episode(_) => {
            for (episode, expected) in &f.members {
                let row = c
                    .query(
                        "SELECT series_id,episode_file_id FROM episodes WHERE id=?",
                        [*episode],
                    )
                    .await?
                    .next()
                    .await?
                    .ok_or_else(|| target_changed("Scan episode membership changed"))?;
                if row.get::<i64>(0)? != current.0 || row.get::<Option<i64>>(1)? != *expected {
                    return Err(target_changed("Expected episode association changed"));
                }
            }
        }
        MediaTarget::Movie(_) => {
            if current.3 != f.expected_old_movie {
                return Err(target_changed("Expected movie file changed"));
            }
        }
    }
    for r in &f.replacements {
        let (path, metadata) = owned::old_record(c, t, r.old.file_id).await?;
        if path != r.old.path || metadata != r.old.metadata {
            return Err(target_changed("Original file facts changed"));
        }
    }
    Ok((current.0, current.1, current.2))
}
async fn outside_references(c: &Connection, child: &str, file: i64) -> Result<i64> {
    Ok(c.query(
        "SELECT count(*) FROM episodes e WHERE e.episode_file_id=? AND NOT EXISTS(SELECT 1 FROM downloaded_scan_child_episodes m WHERE m.child_id=? AND m.episode_id=e.id)",
        params![file, child],
    )
    .await?
    .next()
    .await?
    .ok_or_else(Error::internal)?
    .get(0)?)
}
pub(super) async fn available(
    c: &Connection,
    operation: &str,
    t: &MediaTarget,
    plan: &Plan,
) -> Result<()> {
    let f = facts(c, operation).await?;
    let Some(old) = f
        .replacements
        .iter()
        .find(|r| r.old.path == plan.destination)
    else {
        return destination_free(c, &plan.destination).await;
    };
    let (path, _) = owned::old_record(c, t, old.old.file_id).await?;
    if path != plan.destination {
        return Err(target_changed("Original file path changed"));
    }
    if matches!(t, MediaTarget::Episode(_))
        && outside_references(c, &f.child_id, old.old.file_id).await? != 0
    {
        return Err(Error::conflict(
            "shared_file_conflict",
            "Same-path replacement would change an episode outside this file",
        ));
    }
    Ok(())
}
pub(super) async fn placement_old(
    c: &Connection,
    operation: &str,
    plan: &Plan,
) -> Result<Option<OldFile>> {
    Ok(facts(c, operation)
        .await?
        .replacements
        .into_iter()
        .map(|r| r.old)
        .find(|old| old.path == plan.destination))
}
/// Every member must still point at the committed physical file and carry its history row.
pub(super) async fn committed_members(c: &Connection, operation: &str) -> Result<()> {
    if !is_scan(c, operation).await? {
        return Ok(());
    }
    let mismatched: i64 = c
        .query(
            "SELECT count(*) FROM scan_import_facts f JOIN downloaded_scan_child_episodes m ON m.child_id=f.child_id JOIN import_history h ON h.operation_id=f.operation_id LEFT JOIN episodes e ON e.id=m.episode_id LEFT JOIN import_history_episodes x ON x.operation_id=h.operation_id AND x.episode_id=m.episode_id WHERE f.operation_id=? AND (e.episode_file_id IS NOT h.episode_file_id OR x.episode_file_id IS NOT h.episode_file_id)",
            [operation],
        )
        .await?
        .next()
        .await?
        .ok_or_else(Error::internal)?
        .get(0)?;
    if mismatched != 0 {
        return Err(target_changed(
            "Committed scan membership changed; source cleanup is paused",
        ));
    }
    Ok(())
}

/// One short transaction: one physical file, every member association by compare-and-swap,
/// one physical history row plus relational membership, then `committed`. Any failed
/// association or history write rolls everything back.
pub(super) async fn commit(
    c: &Connection,
    opid: &str,
    t: &MediaTarget,
    plan: &Plan,
    stage: &Stage,
) -> Result<()> {
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        let (owner_id, root, season) = before(&tx, opid, t).await?;
        if owner_id != plan.owner_id || root != plan.root {
            return Err(target_changed("Library ownership changed before commit"));
        }
        available(&tx, opid, t, plan).await?;
        let f = facts(&tx, opid).await?;
        let size = i64::try_from(stage.size()).map_err(|_| Error::internal())?;
        let sha = stage.sha256.clone().ok_or_else(Error::internal)?;
        let same = f
            .replacements
            .iter()
            .find(|r| r.old.path == plan.destination)
            .map(|r| r.old.clone());
        let e = &f.evidence;
        let languages = e.languages.as_ref().map(json).transpose()?;
        match t {
            MediaTarget::Episode(anchor) => {
                if let Some(old) = &same {
                    // The exchanged original serves only members (checked by `available`).
                    let expected = f
                        .members
                        .iter()
                        .filter(|(_, o)| *o == Some(old.file_id))
                        .count() as u64;
                    let detached = tx
                        .execute(
                            "UPDATE episodes SET episode_file_id=NULL WHERE episode_file_id=?1 AND id IN (SELECT episode_id FROM downloaded_scan_child_episodes WHERE child_id=?2)",
                            params![old.file_id, f.child_id.clone()],
                        )
                        .await?;
                    if detached != expected
                        || tx
                            .execute(
                                "UPDATE episode_files SET path=? WHERE id=? AND path=?",
                                params![old.recovery_path()?, old.file_id, old.path.clone()],
                            )
                            .await?
                            != 1
                    {
                        return Err(target_changed("Exchanged original changed"));
                    }
                }
                tx.execute(
                    "INSERT INTO episode_files(series_id,path)VALUES(?,?)",
                    params![owner_id, plan.destination.clone()],
                )
                .await?;
                let fid = tx.last_insert_rowid();
                tx.execute(
                    "INSERT INTO file_metadata(media_type,episode_file_id,size,date_added,season_number,quality_id,revision_json,original_release_title,languages_json,release_group,indexer_flags,release_type)VALUES('tv',?,?,strftime('%Y-%m-%dT%H:%M:%SZ','now'),?,?,?,?,?,?,?,?)",
                    params![fid, size, season, f.quality_id, f.revision_json.clone(), e.title.clone(), languages, e.release_group.clone(), e.indexer_flags, e.release_type],
                )
                .await?;
                for (episode, expected) in &f.members {
                    let expected = if same.as_ref().is_some_and(|o| Some(o.file_id) == *expected) {
                        None
                    } else {
                        *expected
                    };
                    if tx
                        .execute(
                            "UPDATE episodes SET episode_file_id=? WHERE id=? AND episode_file_id IS ?",
                            params![fid, *episode, expected],
                        )
                        .await?
                        != 1
                    {
                        return Err(target_changed("Episode association changed"));
                    }
                }
                tx.execute(
                    "INSERT INTO import_history(operation_id,media_type,episode_id,movie_id,episode_file_id,movie_file_id,source,destination,size,sha256)VALUES(?,'episode',?,NULL,?,NULL,?,?,?,?)",
                    params![opid, *anchor, fid, plan.source.clone(), plan.destination.clone(), size, sha.clone()],
                )
                .await?;
                for (episode, _) in &f.members {
                    tx.execute(
                        "INSERT INTO import_history_episodes(operation_id,episode_id,episode_file_id)VALUES(?,?,?)",
                        params![opid, *episode, fid],
                    )
                    .await?;
                }
            }
            MediaTarget::Movie(movie) => {
                let fid = if let Some(old) = f.expected_old_movie {
                    if tx
                        .execute(
                            "UPDATE movie_files SET path=?,edition=? WHERE id=? AND movie_id=?",
                            params![plan.destination.clone(), f.edition.clone(), old, *movie],
                        )
                        .await?
                        != 1
                    {
                        return Err(target_changed("Movie file changed"));
                    }
                    tx.execute("DELETE FROM file_metadata WHERE movie_file_id=?", [old])
                        .await?;
                    old
                } else {
                    tx.execute(
                        "INSERT INTO movie_files(movie_id,path,edition)VALUES(?,?,?)",
                        params![*movie, plan.destination.clone(), f.edition.clone()],
                    )
                    .await?;
                    tx.last_insert_rowid()
                };
                tx.execute(
                    "INSERT INTO file_metadata(media_type,movie_file_id,size,date_added,original_file_path,quality_id,revision_json,original_release_title,languages_json,release_group,indexer_flags,release_type)VALUES('movies',?,?,strftime('%Y-%m-%dT%H:%M:%SZ','now'),?,?,?,?,?,?,?,?)",
                    params![fid, size, plan.source.clone(), f.quality_id, f.revision_json.clone(), e.title.clone(), languages, e.release_group.clone(), e.indexer_flags, e.release_type],
                )
                .await?;
                tx.execute(
                    "INSERT INTO import_history(operation_id,media_type,episode_id,movie_id,episode_file_id,movie_file_id,source,destination,size,sha256)VALUES(?,'movie',NULL,?,NULL,?,?,?,?,?)",
                    params![opid, *movie, fid, plan.source.clone(), plan.destination.clone(), size, sha.clone()],
                )
                .await?;
            }
        }
        tx.execute(
            "UPDATE import_journal SET phase='committed',error_code=NULL,updated_at=CURRENT_TIMESTAMP WHERE operation_id=?",
            [opid],
        )
        .await?;
        tx.execute(
            "UPDATE operations SET status='committed' WHERE id=?",
            [opid],
        )
        .await?;
        diagnostic_changed(&tx, opid, crate::health::PathDiagnosticChange::Progress).await
    }
    .await;
    match outcome {
        Ok(()) => {
            tx.commit().await?;
            Ok(())
        }
        Err(e) => {
            tx.rollback().await?;
            Err(e)
        }
    }
}

async fn referenced(c: &Connection, file: i64) -> Result<bool> {
    Ok(c.query(
        "SELECT 1 FROM episodes WHERE episode_file_id=? LIMIT 1",
        [file],
    )
    .await?
    .next()
    .await?
    .is_some())
}
/// Quarantined originals with every member episode that each one served (one entry per
/// distinct original, so a multi-episode original is listed, and recycled, once).
pub(super) async fn quarantined(
    c: &Connection,
    operation: &str,
    target: &MediaTarget,
) -> Result<Vec<super::recycle::Retired>> {
    let f = facts(c, operation).await?;
    let mut out = Vec::new();
    for r in f.replacements {
        if r.state != "quarantined" {
            continue;
        }
        let targets = match target {
            MediaTarget::Episode(_) => f
                .members
                .iter()
                .filter(|(_, old)| *old == Some(r.old.file_id))
                .map(|(episode, _)| MediaTarget::Episode(*episode))
                .collect::<Vec<_>>(),
            MediaTarget::Movie(movie) => vec![MediaTarget::Movie(*movie)],
        };
        if targets.is_empty() {
            return Err(Error::internal());
        }
        out.push(super::recycle::Retired {
            old: r.old,
            targets,
        });
    }
    Ok(out)
}

/// Each distinct original is checkpointed independently: shared originals stay in place,
/// others move into a private per-original recovery directory. Bytes are never deleted.
pub(super) async fn retire(
    c: &Connection,
    operation: &str,
    target: &MediaTarget,
    plan: &Plan,
    stage: &Stage,
    lease: &Arc<Permit>,
) -> Result<()> {
    let media = domain(target);
    let tv = matches!(target, MediaTarget::Episode(_));
    for r in facts(c, operation).await?.replacements {
        if r.state != "pending" {
            continue;
        }
        committed_owner(c, operation, target, plan).await?;
        let old = r.old;
        if tv && referenced(c, old.file_id).await? {
            if c.execute(
                "UPDATE scan_import_replacements SET retirement_state='shared_retained' WHERE operation_id=? AND media_type=? AND old_file_id=? AND retirement_state='pending'",
                params![operation, media, old.file_id],
            )
            .await?
                != 1
            {
                return Err(Error::internal());
            }
            continue;
        }
        let checkpoint = match r.retirement {
            Some(checkpoint) => checkpoint,
            None => {
                let original = old.clone();
                let p = plan.clone();
                let s = stage.clone();
                let checkpoint = leased(lease, move || {
                    if original.path == p.destination {
                        original.prepare_exchange_retirement(&p, &s)
                    } else {
                        original.prepare_retirement()
                    }
                })
                .await?;
                c.execute(
                    "UPDATE scan_import_replacements SET retirement_json=? WHERE operation_id=? AND media_type=? AND old_file_id=? AND retirement_state='pending'",
                    params![json(&checkpoint)?, operation, media, old.file_id],
                )
                .await?;
                checkpoint
            }
        };
        committed_owner(c, operation, target, plan).await?;
        let p = plan.clone();
        let s = stage.clone();
        let original = old.clone();
        leased(lease, move || {
            p.verify_destination(&s)?;
            if original.path == p.destination {
                original.retire_exchanged(&p, &s, &checkpoint)
            } else {
                original.retire(&checkpoint)
            }
        })
        .await?;
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        let outcome = async {
            committed_owner(&tx, operation, target, plan).await?;
            if tv {
                if referenced(&tx, old.file_id).await? {
                    return Err(target_changed(
                        "Old file acquired another association; retained recovery artifact requires inspection",
                    ));
                }
                if old.path != plan.destination
                    && tx
                        .execute(
                            "UPDATE episode_files SET path=? WHERE id=? AND path=?",
                            params![old.recovery_path()?, old.file_id, old.path.clone()],
                        )
                        .await?
                        != 1
                {
                    return Err(target_changed("Old file path changed during retirement"));
                }
            }
            if tx
                .execute(
                    "UPDATE scan_import_replacements SET retirement_state='quarantined' WHERE operation_id=? AND media_type=? AND old_file_id=? AND retirement_state='pending'",
                    params![operation, media, old.file_id],
                )
                .await?
                != 1
            {
                return Err(Error::internal());
            }
            Ok(())
        }
        .await;
        match outcome {
            Ok(()) => tx.commit().await?,
            Err(e) => {
                tx.rollback().await?;
                return Err(e);
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Producer admission: prepare (operation + journal + facts + write-once link) and start.
// ---------------------------------------------------------------------------------------

struct Child {
    status: String,
    revision: i64,
    run_generation: i64,
    cancel: bool,
    selected: bool,
    disposition: String,
    operation: Option<String>,
    source: String,
    destination: Option<String>,
    mode: Option<String>,
    series_id: Option<i64>,
    movie_id: Option<i64>,
    expected_old_movie: Option<i64>,
    quality_id: Option<i64>,
    revision_json: Option<String>,
    edition: Option<String>,
    identity: Option<String>,
    proof: Option<String>,
    resume_generation: i64,
    resume_requested: bool,
    /// The parent's captured tracked receipt authority, when the scan reserved one.
    receipt: Option<ScanAuthority>,
}
async fn child(c: &Connection, claim: &ScanChildAdmission) -> Result<Child> {
    let r = c
        .query(
            "SELECT p.status,p.inventory_revision,p.run_generation,p.cancel_requested,c.selected,c.disposition,c.operation_id,c.source_path,c.destination,c.resolved_mode,c.series_id,c.movie_id,c.expected_old_movie_file_id,c.quality_id,c.revision_json,c.edition,c.source_identity_json,CASE WHEN length(CAST(json_extract(c.decision_json,'$.proof') AS BLOB))<=? THEN json_extract(c.decision_json,'$.proof') END,c.resume_generation,c.resume_requested,p.candidate_id,p.media_type,json_extract(p.tracked_json,'$.provider_id'),json_extract(p.tracked_json,'$.provider_revision'),json_extract(p.tracked_json,'$.remote_id') FROM downloaded_scan_children c JOIN downloaded_scan_commands p ON p.id=c.command_id WHERE c.id=? AND c.command_id=?",
            params![
                MAX_PROOF_BYTES as i64,
                claim.child_id.hyphenated().to_string(),
                claim.command_id.hyphenated().to_string()
            ],
        )
        .await?
        .next()
        .await?
        .ok_or_else(Error::missing)?;
    let child = Child {
        status: r.get(0)?,
        revision: r.get(1)?,
        run_generation: r.get(2)?,
        cancel: r.get::<i64>(3)? == 1,
        selected: r.get::<i64>(4)? == 1,
        disposition: r.get(5)?,
        operation: r.get(6)?,
        source: r.get(7)?,
        destination: r.get(8)?,
        mode: r.get(9)?,
        series_id: r.get(10)?,
        movie_id: r.get(11)?,
        expected_old_movie: r.get(12)?,
        quality_id: r.get(13)?,
        revision_json: r.get(14)?,
        edition: r.get(15)?,
        identity: r.get(16)?,
        proof: r.get(17)?,
        resume_generation: r.get(18)?,
        resume_requested: r.get::<i64>(19)? == 1,
        receipt: match r.get::<Option<String>>(20)? {
            None => None,
            Some(candidate) => {
                let uuid = |v: String| Uuid::parse_str(&v).map_err(|_| Error::internal());
                Some(ScanAuthority::Receipt {
                    candidate_id: uuid(candidate)?,
                    media_type: MediaDomain::parse(&r.get::<String>(21)?)
                        .map_err(|_| Error::internal())?,
                    provider_id: uuid(r.get::<String>(22)?)?,
                    provider_revision: r.get(23)?,
                    remote_id: r.get(24)?,
                })
            }
        },
    };
    // The parent claim is the only scheduling authority; a superseded generation does nothing.
    if child.status != "running"
        || child.revision != claim.inventory_revision
        || child.run_generation != claim.run_generation
    {
        return Err(Error::conflict(
            "scan_claim_stale",
            "The scan claim changed; this worker turn is superseded",
        ));
    }
    Ok(child)
}
fn cancelled() -> Error {
    Error::conflict(
        "scan_cancelled",
        "The scan was cancelled; no further child may start",
    )
}
async fn destination_for(
    c: &Connection,
    e: &ScanPolicyEligible,
    basename: &str,
) -> Result<Option<String>> {
    use crate::naming::destination::{
        DestinationError, resolve_episode_set_destination, resolve_owned_destination,
    };
    let resolved = match &e.target {
        ReleaseTarget::Tv { .. } => {
            let ids: Vec<i64> = e.members.iter().map(|m| m.episode_id).collect();
            resolve_episode_set_destination(
                c,
                &ids,
                &e.root,
                basename,
                e.quality.quality_id,
                &e.revision.effective,
                &e.evidence,
            )
            .await
            .map(|d| d.destination)
        }
        ReleaseTarget::Movies { movie_id } => {
            resolve_owned_destination(
                c,
                &MediaTarget::Movie(*movie_id),
                &e.root,
                basename,
                e.quality.quality_id,
                &e.revision.effective,
                e.evidence.edition.as_deref(),
                &e.evidence,
            )
            .await
        }
    };
    match resolved {
        Ok(destination) => Ok(Some(destination)),
        Err(DestinationError::Render(_)) => Ok(None),
        Err(DestinationError::Db(error)) => Err(Error::from(error)),
        Err(DestinationError::Unavailable(code)) => Err(Error::unavailable(code)),
        Err(DestinationError::Internal(reason)) => {
            eprintln!("event=scan_destination_unavailable condition={reason}");
            Err(Error::internal())
        }
    }
}
fn input(proof: &ScanDecisionProof) -> ScanFileInput<'_> {
    ScanFileInput {
        basename: &proof.basename,
        relative_path: &proof.relative_path,
        size: proof.size,
        source_identity_json: &proof.source_identity_json,
        folder_name: proof.folder_name.as_deref(),
        download_name: proof.download_name.as_deref(),
    }
}
/// Selected facts are immutable after publication; reject anything malformed as storage.
struct Selection {
    target: MediaTarget,
    destination: String,
    mode: Mode,
    quality_id: i64,
    revision_json: String,
    identity: serde_json::Value,
    proof: ScanDecisionProof,
    members: Vec<(i64, Option<i64>)>,
}
async fn selection(c: &Connection, child_id: &str, child: &Child) -> Result<Selection> {
    let members = members(c, child_id).await?;
    let target = match (child.series_id, child.movie_id) {
        (Some(_), None) => MediaTarget::Episode(members.first().ok_or_else(Error::internal)?.0),
        (None, Some(movie)) if members.is_empty() => MediaTarget::Movie(movie),
        _ => return Err(Error::internal()),
    };
    let proof: ScanDecisionProof =
        serde_json::from_str(child.proof.as_deref().ok_or_else(Error::internal)?)
            .map_err(|_| Error::internal())?;
    Ok(Selection {
        target,
        destination: child.destination.clone().ok_or_else(Error::internal)?,
        mode: match child.mode.as_deref() {
            Some("copy") => Mode::Copy,
            Some("move") => Mode::Move,
            _ => return Err(Error::internal()),
        },
        quality_id: child.quality_id.ok_or_else(Error::internal)?,
        revision_json: child.revision_json.clone().ok_or_else(Error::internal)?,
        identity: serde_json::from_str(child.identity.as_deref().ok_or_else(Error::internal)?)
            .map_err(|_| Error::internal())?,
        proof,
        members,
    })
}
/// Original files this physical file replaces: distinct frozen TV originals, or the movie's.
fn originals(child: &Child, s: &Selection) -> Vec<i64> {
    match s.target {
        MediaTarget::Episode(_) => {
            let set: std::collections::BTreeSet<i64> =
                s.members.iter().filter_map(|(_, old)| *old).collect();
            set.into_iter().collect()
        }
        MediaTarget::Movie(_) => child.expected_old_movie.into_iter().collect(),
    }
}
async fn associations_unchanged(c: &Connection, child: &Child, s: &Selection) -> Result<()> {
    match s.target {
        MediaTarget::Episode(_) => {
            for (episode, expected) in &s.members {
                let current: Option<i64> = c
                    .query(
                        "SELECT episode_file_id FROM episodes WHERE id=?",
                        [*episode],
                    )
                    .await?
                    .next()
                    .await?
                    .ok_or_else(|| target_changed("Scan episode was removed"))?
                    .get(0)?;
                if current != *expected {
                    return Err(target_changed("Expected episode association changed"));
                }
            }
        }
        MediaTarget::Movie(_) => {
            if owned::ownership(c, &s.target).await?.3 != child.expected_old_movie {
                return Err(target_changed("Expected movie file changed"));
            }
        }
    }
    Ok(())
}

/// Loads published authority, prepares filesystem facts outside any write transaction, then
/// repeats every fence and decision inside one IMMEDIATE transaction that writes the
/// operation, preview journal, scan facts, per-original replacement rows and the write-once
/// child link. An existing link returns that same operation.
pub(crate) async fn prepare_scan(
    db: Arc<Database>,
    claim: ScanChildAdmission,
) -> Result<Operation> {
    local(&db)?;
    let child_id = claim.child_id.hyphenated().to_string();
    let c = db.connect().await?;
    let current = child(&c, &claim).await?;
    if let Some(operation) = &current.operation {
        return status_in(&c, operation).await;
    }
    if current.cancel {
        return Err(cancelled());
    }
    if !current.selected || current.disposition != "ready" {
        return Err(Error::conflict(
            "scan_child_not_ready",
            "Only a ready selected scan child can be prepared",
        ));
    }
    let s = selection(&c, &child_id, &current).await?;
    // A tracked parent decided every child under its captured receipt; the same authority
    // must reproduce the published proof (its fingerprint binds the receipt).
    let authority = current.receipt.clone().unwrap_or(ScanAuthority::Local {
        owner: s.proof.owner,
    });
    let warm = warm_scan_file(&c, &authority, input(&s.proof))
        .await
        .map_err(search)?;
    if !matches!(warm.decision(), ScanFileDecision::PolicyEligible(_)) {
        return Err(decision_changed());
    }
    let own = owned::ownership(&c, &s.target).await?;
    if Some(own.0) != current.series_id.or(current.movie_id) {
        return Err(target_changed("Scan owner changed"));
    }
    source_unmanaged(&c, &current.source).await?;
    associations_unchanged(&c, &current, &s).await?;
    let mut captured = Vec::new();
    for fid in originals(&current, &s) {
        captured.push((fid, owned::old_record(&c, &s.target, fid).await?));
    }
    let same = captured
        .iter()
        .position(|(_, (path, _))| *path == s.destination);
    if let Some(index) = same {
        if matches!(s.target, MediaTarget::Episode(_))
            && outside_references(&c, &child_id, captured[index].0).await? != 0
        {
            return Err(Error::conflict(
                "shared_file_conflict",
                "Same-path replacement would change an episode outside this file",
            ));
        }
    } else {
        destination_free(&c, &s.destination).await?;
    }
    let operation = Uuid::new_v4().to_string();
    let op = operation.clone();
    let root = own.1.clone();
    let source = current.source.clone();
    let destination = s.destination.clone();
    let mode = s.mode;
    let identity = s.identity.clone();
    let (plan, olds) = blocking(move || {
        let olds = captured
            .into_iter()
            .map(|(fid, (path, metadata))| OldFile::capture_scan(fid, path, metadata, &root, &op))
            .collect::<Result<Vec<_>>>()?;
        let plan = Plan::preview_replacement(
            own.0,
            root.clone(),
            source,
            destination,
            mode,
            &op,
            same.map(|index| &olds[index]),
        )?;
        if !plan.source_matches(&identity) {
            let mut error = Error::conflict(
                "source_changed",
                "Downloaded file changed since the scan inventory",
            );
            error.path_kind = Some(PathKind::Changed);
            return Err(error.at(PathRole::Source));
        }
        for old in &olds {
            old.recovery_path()?;
        }
        Ok((plan, olds))
    })
    .await?;
    let mut old_json = Vec::with_capacity(olds.len());
    for old in &olds {
        let text = json(old)?;
        if text.len() > MAX_REPLACEMENT_JSON_BYTES {
            return Err(Error::bad("Original file evidence exceeds limit"));
        }
        old_json.push((old.file_id, text));
    }
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        let now = child(&tx, &claim).await?;
        if let Some(existing) = now.operation {
            return Ok(existing);
        }
        if now.cancel {
            return Err(cancelled());
        }
        if !now.selected || now.disposition != "ready" {
            return Err(Error::conflict(
                "scan_child_not_ready",
                "Only a ready selected scan child can be prepared",
            ));
        }
        let decision = evaluate_scan_file(&tx, &authority, input(&s.proof), Some(&s.proof), &warm)
            .await
            .map_err(search)?;
        let ScanFileDecision::PolicyEligible(eligible) = decision else {
            return Err(decision_changed());
        };
        let revision = json(&eligible.revision.effective)?;
        let edition = match s.target {
            MediaTarget::Episode(_) => None,
            MediaTarget::Movie(_) => eligible.evidence.edition.clone(),
        };
        if eligible.quality.quality_id != s.quality_id
            || revision != s.revision_json
            || edition != now.edition
            || eligible.root != plan.root
            || eligible.members.iter().map(|m| m.episode_id).collect::<Vec<_>>()
                != s.members.iter().map(|m| m.0).collect::<Vec<_>>()
        {
            return Err(decision_changed());
        }
        if destination_for(&tx, &eligible, &s.proof.basename).await?.as_deref()
            != Some(s.destination.as_str())
        {
            return Err(Error::conflict(
                "scan_naming_changed",
                "Library naming changed since the scan inventory was published",
            ));
        }
        let own = owned::ownership(&tx, &s.target).await?;
        if own.0 != plan.owner_id || own.1 != plan.root {
            return Err(target_changed("Library path or ownership changed"));
        }
        associations_unchanged(&tx, &now, &s).await?;
        for old in &olds {
            let (path, metadata) = owned::old_record(&tx, &s.target, old.file_id).await?;
            if path != old.path || metadata != old.metadata {
                return Err(target_changed("Original metadata changed"));
            }
        }
        if same.is_none() {
            destination_free(&tx, &plan.destination).await?;
        } else if matches!(s.target, MediaTarget::Episode(_))
            && outside_references(&tx, &child_id, olds[same.unwrap_or(0)].file_id).await? != 0
        {
            return Err(Error::conflict(
                "shared_file_conflict",
                "Same-path replacement would change an episode outside this file",
            ));
        }
        source_unmanaged(&tx, &plan.source).await?;
        let (media, episode, movie) = target(&s.target);
        tx.execute(
            "INSERT INTO operations(id,media_type,episode_id,movie_id,source,mode,destination,status,message)VALUES(?,?,?,?,?,?,?,'preview','Downloaded scan import')",
            params![operation.clone(), media, episode, movie, plan.source.clone(), plan.mode.name(), plan.destination.clone()],
        )
        .await?;
        tx.execute(
            "INSERT INTO import_journal(operation_id,plan_json,phase)VALUES(?,?,'preview')",
            params![operation.clone(), json(&plan)?],
        )
        .await?;
        let comparison = json(&eligible.evidence)?;
        if comparison.len() > MAX_PROOF_BYTES {
            return Err(Error::bad("Import comparison facts exceed limit"));
        }
        tx.execute(
            "INSERT INTO scan_import_facts(operation_id,child_id,command_id,inventory_revision,run_generation,quality_id,revision_json,edition,expected_size,comparison_facts_json)VALUES(?,?,?,?,?,?,?,?,?,?)",
            params![
                operation.clone(),
                child_id.clone(),
                claim.command_id.hyphenated().to_string(),
                claim.inventory_revision,
                claim.run_generation,
                s.quality_id,
                s.revision_json.clone(),
                now.edition.clone(),
                i64::try_from(plan.source_size()).map_err(|_| Error::internal())?,
                comparison
            ],
        )
        .await?;
        for (fid, text) in &old_json {
            tx.execute(
                "INSERT INTO scan_import_replacements(operation_id,media_type,old_file_id,old_episode_file_id,old_movie_file_id,old_file_json)VALUES(?,?,?,?,?,?)",
                params![
                    operation.clone(),
                    domain(&s.target),
                    *fid,
                    episode.map(|_| *fid),
                    movie.map(|_| *fid),
                    text.clone()
                ],
            )
            .await?;
        }
        if tx
            .execute(
                "UPDATE downloaded_scan_children SET operation_id=? WHERE id=? AND operation_id IS NULL AND disposition='ready'",
                params![operation.clone(), child_id.clone()],
            )
            .await?
            != 1
        {
            return Err(Error::internal());
        }
        crate::health::path_diagnostics_changed(
            &tx,
            media_domain(&s.target),
            crate::health::PathDiagnosticChange::Progress,
        )
        .await
        .map_err(|_| Error::internal())?;
        Ok(operation.clone())
    }
    .await;
    let linked = match outcome {
        Ok(linked) => {
            tx.commit().await?;
            linked
        }
        Err(e) => {
            tx.rollback().await?;
            return Err(e);
        }
    };
    status(db, &linked).await
}
fn media_domain(t: &MediaTarget) -> MediaDomain {
    super::media(t)
}

/// Reads the child's linked operation (never an arbitrary ID), takes the process execution
/// permit, repeats the claim/journal checks and consumes the exact start authority in one
/// transaction, then launches the existing engine with that same permit.
pub(crate) async fn start_scan(db: Arc<Database>, start: ScanChildStart) -> Result<StartOwned> {
    local(&db)?;
    let c = db.connect().await?;
    let first = child(&c, &start.claim).await?;
    let operation = first.operation.clone().ok_or_else(|| {
        Error::conflict(
            "scan_child_unlinked",
            "The scan child has no prepared operation",
        )
    })?;
    if first.resume_generation != start.resume_generation {
        return Err(Error::conflict(
            "scan_resume_generation",
            "The child resume generation changed",
        ));
    }
    let complete = load(&c, &operation).await?.phase == "complete";
    let permit = if complete {
        None
    } else {
        let Some(permit) = acquire(&db, &operation) else {
            return Ok(if operation_active(&operation)? {
                StartOwned::Active
            } else {
                StartOwned::Busy
            });
        };
        Some(permit)
    };
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        let now = child(&tx, &start.claim).await?;
        if now.operation.as_deref() != Some(operation.as_str())
            || now.resume_generation != start.resume_generation
        {
            return Err(Error::conflict(
                "scan_resume_generation",
                "The child resume generation changed",
            ));
        }
        let record = load(&tx, &operation).await?;
        let clean =
            record.phase == "preview" && record.error.is_none() && record.path_failure.is_none();
        let key = start.claim.child_id.hyphenated().to_string();
        if now.resume_requested {
            if now.disposition != "failed" {
                return Err(Error::conflict(
                    "scan_recovery_required",
                    "Only a failed child consumes an explicit resume",
                ));
            }
            // Cancellation permits finishing an already effectful child, never a fresh start.
            if record.phase == "preview" && now.cancel {
                return Err(cancelled());
            }
            tx.execute(
                "UPDATE downloaded_scan_children SET disposition='importing',resume_requested=0,error_code=NULL WHERE id=? AND disposition='failed' AND resume_requested=1 AND resume_generation=?",
                params![key, start.resume_generation],
            )
            .await?;
            tx.execute(
                "UPDATE import_journal SET error_code=NULL WHERE operation_id=? AND phase!='complete'",
                [operation.clone()],
            )
            .await?;
        } else if clean && matches!(now.disposition.as_str(), "ready" | "importing") {
            if now.cancel {
                return Err(cancelled());
            }
            tx.execute(
                "UPDATE downloaded_scan_children SET disposition='importing',error_code=NULL WHERE id=? AND disposition IN ('ready','importing')",
                [key],
            )
            .await?;
        } else if record.phase == "complete" && now.disposition == "importing" {
            // Nothing to consume: the worker settles a completed journal exactly once.
        } else {
            return Err(Error::conflict(
                "scan_recovery_required",
                "A failed or interrupted scan child requires an explicit resume",
            ));
        }
        Ok(record.phase == "complete")
    }
    .await;
    let complete = match outcome {
        Ok(complete) => {
            tx.commit().await?;
            complete
        }
        Err(e) => {
            tx.rollback().await?;
            return Err(e);
        }
    };
    match permit {
        Some(permit) if !complete => {
            // The detached task owns the lease; a worker deadline cannot release it early.
            drop(launch(db, operation, permit, false));
            Ok(StartOwned::Started)
        }
        _ => Ok(StartOwned::Complete),
    }
}
