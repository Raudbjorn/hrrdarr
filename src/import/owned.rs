//! Receipt-bound imports. The sidecar is authority for replacement and factual metadata.
use super::*;
use fs::{OldFile, Retirement};

pub(crate) struct OwnedImport {
    pub candidate_id: String,
    pub target: MediaTarget,
    pub source: String,
    pub destination: String,
    pub mode: Mode,
    pub expected_size: u64,
    pub quality_id: i64,
    pub revision_json: String,
    pub edition: Option<String>,
    pub policy_revision: i64,
    pub mapping_id: i64,
    pub mapping_revision: i64,
    pub host: String,
}
pub(super) struct Facts {
    pub quality_id: i64,
    pub revision_json: String,
    pub edition: Option<String>,
    pub old: Option<OldFile>,
    pub retirement_state: String,
    pub retirement: Option<Retirement>,
    pub evidence: Option<crate::custom_formats::Evidence>,
    pub expected_size: u64,
}
pub(super) async fn facts(c: &Connection, operation: &str) -> Result<Option<Facts>> {
    let Some(r)=c.query("SELECT quality_id,revision_json,edition,old_file_json,retirement_state,retirement_json,provenance_json,(SELECT media_type FROM operations WHERE id=operation_id) FROM rss_candidate_imports WHERE operation_id=?",[operation]).await?.next().await? else {return Ok(None)};
    let provenance: serde_json::Value =
        serde_json::from_str(&r.get::<String>(6)?).map_err(|_| Error::internal())?;
    let expected_size = provenance
        .get("expected_size")
        .and_then(serde_json::Value::as_u64)
        .filter(|size| *size > 0)
        .ok_or_else(Error::internal)?;
    let evidence: Option<crate::custom_formats::Evidence> = provenance
        .get("comparison_facts")
        .cloned()
        .map(|v| serde_json::from_value(v).map_err(|_| Error::internal()))
        .transpose()?;
    if let Some(evidence) = &evidence {
        crate::custom_formats::validate_evidence(
            if r.get::<String>(7)? == "episode" {
                crate::api::MediaDomain::Tv
            } else {
                crate::api::MediaDomain::Movies
            },
            evidence,
        )
        .map_err(|_| Error::internal())?;
    }
    Ok(Some(Facts {
        evidence,
        expected_size,
        quality_id: r.get(0)?,
        revision_json: r.get(1)?,
        edition: r.get(2)?,
        old: r
            .get::<Option<String>>(3)?
            .map(|v| serde_json::from_str(&v).map_err(|_| Error::internal()))
            .transpose()?,
        retirement_state: r.get(4)?,
        retirement: r
            .get::<Option<String>>(5)?
            .map(|v| serde_json::from_str(&v).map_err(|_| Error::internal()))
            .transpose()?,
    }))
}
pub(super) async fn ownership(
    c: &Connection,
    t: &MediaTarget,
) -> Result<(i64, String, Option<i64>, Option<i64>)> {
    let sql = match t {
        MediaTarget::Episode(_) => {
            "SELECT s.id,s.path,e.season,e.episode_file_id FROM episodes e JOIN series s ON s.id=e.series_id WHERE e.id=?"
        }
        MediaTarget::Movie(_) => {
            "SELECT m.id,m.path,NULL,f.id FROM movies m LEFT JOIN movie_files f ON f.movie_id=m.id WHERE m.id=?"
        }
    };
    let r = c
        .query(sql, [id(t)])
        .await?
        .next()
        .await?
        .ok_or_else(Error::missing)?;
    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
}
async fn old_record(
    c: &Connection,
    t: &MediaTarget,
    fid: i64,
) -> Result<(String, serde_json::Value)> {
    let (table, column, edition) = match t {
        MediaTarget::Episode(_) => ("episode_files", "episode_file_id", "NULL"),
        MediaTarget::Movie(_) => ("movie_files", "movie_file_id", "f.edition"),
    };
    let r=c.query(&format!("SELECT f.path,json_object('edition',{edition},'quality_id',m.quality_id,'revision_json',m.revision_json,'languages_json',m.languages_json,'media_info_json',m.media_info_json,'size',m.size,'date_added',m.date_added,'season_number',m.season_number,'original_file_path',m.original_file_path,'original_release_title',m.original_release_title,'release_group',m.release_group,'indexer_flags',m.indexer_flags,'release_type',m.release_type) FROM {table} f LEFT JOIN file_metadata m ON m.{column}=f.id WHERE f.id=?"),[fid]).await?.next().await?.ok_or_else(Error::missing)?;
    let path: String = r.get(0)?;
    let claims:i64=c.query("SELECT count(*) FROM (SELECT path FROM episode_files WHERE path=?1 UNION ALL SELECT path FROM movie_files WHERE path=?1)",[path.clone()]).await?.next().await?.ok_or_else(Error::internal)?.get(0)?;
    if claims != 1 {
        return Err(Error::conflict(
            "old_file_shared_path",
            "Original path is claimed by another file; automatic replacement is unsafe",
        ));
    }
    Ok((
        path,
        serde_json::from_str(&r.get::<String>(1)?).map_err(|_| Error::internal())?,
    ))
}
pub(super) async fn before(
    c: &Connection,
    operation: &str,
    t: &MediaTarget,
) -> Result<(i64, String, Option<i64>)> {
    let Some(f) = facts(c, operation).await? else {
        return owner(c, t).await;
    };
    // Legacy journals lack comparison facts, not authorization to bypass current policy.
    // Their immutable provenance still supplies size; missing provider facts stay unknown.
    let source: String = c
        .query("SELECT source FROM operations WHERE id=?", [operation])
        .await?
        .next()
        .await?
        .ok_or_else(Error::internal)?
        .get(0)?;
    let decision = crate::search::downloaded::evaluate_with_evidence(
        c,
        t,
        &source,
        f.expected_size,
        f.evidence.clone(),
    )
    .await
    .map_err(|_| Error::conflict("preflight_changed", "Import policy evaluation failed"))?;
    if decision.accepted.is_none() {
        return Err(Error::conflict(
            "preflight_changed",
            "Import no longer satisfies current policy",
        ));
    }
    let current = ownership(c, t).await?;
    if current.3 != f.old.as_ref().map(|o| o.file_id) {
        return Err(Error::conflict(
            "target_changed",
            "Expected file association changed",
        ));
    }
    if let Some(old) = f.old {
        let (path, metadata) = old_record(c, t, old.file_id).await?;
        let mut old_metadata = old.metadata.clone();
        if let Some(fields) = old_metadata.as_object_mut() {
            fields
                .entry("original_release_title")
                .or_insert(serde_json::Value::Null);
        }
        if path != old.path || metadata != old_metadata {
            return Err(Error::conflict(
                "target_changed",
                "Original file facts changed",
            ));
        }
    }
    Ok((current.0, current.1, current.2))
}
async fn linked(c: &Connection, candidate: &str) -> Result<Option<String>> {
    Ok(c.query(
        "SELECT operation_id FROM rss_candidate_imports WHERE candidate_id=?",
        [candidate],
    )
    .await?
    .next()
    .await?
    .map(|r| r.get(0))
    .transpose()?)
}
pub(crate) async fn prepare_owned(db: Arc<Database>, input: OwnedImport) -> Result<Operation> {
    local(&db)?;
    let c = db.connect().await?;
    if let Some(op) = linked(&c, &input.candidate_id).await? {
        return status(db, &op).await;
    }
    if !matches!(input.mode, Mode::Copy | Mode::Hardlink) {
        return Err(Error::bad("Owned downloads require copy or hardlink"));
    }
    let revision: crate::media_files::FileRevision = serde_json::from_str(&input.revision_json)
        .map_err(|_| Error::bad("Invalid factual revision"))?;
    if revision.version < 1
        || revision.real < 0
        || input.edition.as_ref().is_some_and(|v| v.len() > 1024)
    {
        return Err(Error::bad("Invalid file facts"));
    }
    let own = ownership(&c, &input.target).await?;
    source_unmanaged(&c, &input.source).await?;
    let old = match own.3 {
        Some(fid) => Some((fid, old_record(&c, &input.target, fid).await?)),
        None => None,
    };
    let same_path = old
        .as_ref()
        .is_some_and(|(_, (path, _))| path == &input.destination);
    destination_available(
        &c,
        &input.target,
        &input.destination,
        old.as_ref().map(|v| v.0),
        same_path,
    )
    .await?;
    let operation = Uuid::new_v4().to_string();
    let op = operation.clone();
    let root = own.1.clone();
    let source = input.source.clone();
    let destination = input.destination.clone();
    let mode = input.mode;
    let expected = input.expected_size;
    let (plan, old) = blocking(move || {
        let old = old
            .map(|(fid, (path, metadata))| OldFile::capture(fid, path, metadata, &root, &op))
            .transpose()?;
        let plan = Plan::preview_replacement(
            own.0,
            root.clone(),
            source,
            destination,
            mode,
            &op,
            if same_path { old.as_ref() } else { None },
        )?;
        if plan.source_size() != expected {
            return Err(Error::conflict(
                "source_changed",
                "Downloaded file size changed",
            ));
        }
        if let Some(old) = &old {
            old.recovery_path()?;
        }
        Ok((plan, old))
    })
    .await?;
    // Compile/match outside the writer lock. The transaction repeats the exact
    // facts/configuration lookup and requires its already evaluated fingerprint.
    crate::search::downloaded::evaluate_receipt(
        &c,
        &input.target,
        &input.source,
        input.expected_size,
        &input.candidate_id,
    )
    .await
    .map_err(|_| {
        Error::conflict(
            "preflight_changed",
            "Downloaded file decision could not be validated",
        )
    })?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    if let Some(op) = linked(&tx, &input.candidate_id).await? {
        tx.rollback().await?;
        return status(db, &op).await;
    }
    let current = ownership(&tx, &input.target).await?;
    if current.0 != plan.owner_id
        || current.1 != plan.root
        || current.3 != old.as_ref().map(|o| o.file_id)
    {
        return Err(Error::conflict(
            "target_changed",
            "Target changed during preflight",
        ));
    }
    if let Some(old) = &old {
        let record = old_record(&tx, &input.target, old.file_id).await?;
        if record.0 != old.path || record.1 != old.metadata {
            return Err(Error::conflict(
                "target_changed",
                "Original metadata changed",
            ));
        }
    }
    let decision = crate::search::downloaded::evaluate_receipt(
        &tx,
        &input.target,
        &input.source,
        input.expected_size,
        &input.candidate_id,
    )
    .await
    .map_err(|_| {
        Error::conflict(
            "preflight_changed",
            "Downloaded file decision could not be validated",
        )
    })?;
    let accepted = decision.accepted.ok_or_else(|| {
        Error::conflict(
            "preflight_changed",
            "Downloaded file no longer passes current import policy",
        )
    })?;
    // Re-derive the SAME destination fresh, inside this transaction, rather than trusting
    // `plan.destination` still matches `accepted.basename` -- a configured naming template
    // means destination is no longer always `root.join(basename)`. A facts change that makes
    // rendering newly fail (or newly succeed differently) surfaces as the same conflict as any
    // other preflight fact drift, not a panic or a silent mismatch.
    let expected_destination = match crate::naming::destination::resolve_owned_destination(
        &tx,
        &input.target,
        &accepted.root,
        &accepted.basename,
        accepted.quality_id,
        accepted.edition.as_deref(),
    )
    .await
    {
        Ok(destination) => Some(destination),
        Err(crate::naming::destination::DestinationError::Render(_)) => None,
        Err(crate::naming::destination::DestinationError::Db(e)) => return Err(Error::from(e)),
        Err(crate::naming::destination::DestinationError::Internal(reason)) => {
            eprintln!("event=naming_destination_unavailable condition={reason}");
            return Err(Error::internal());
        }
    };
    if accepted.root != plan.root
        || accepted.quality_id != input.quality_id
        || accepted.revision_json != input.revision_json
        || accepted.edition != input.edition
        || expected_destination.as_deref() != Some(plan.destination.as_str())
    {
        return Err(Error::conflict(
            "preflight_changed",
            "Downloaded file policy facts changed",
        ));
    }
    destination_available(
        &tx,
        &input.target,
        &plan.destination,
        old.as_ref().map(|v| v.file_id),
        same_path,
    )
    .await?;
    source_unmanaged(&tx, &plan.source).await?;
    let domain = match input.target {
        MediaTarget::Episode(_) => "tv",
        MediaTarget::Movie(_) => "movies",
    };
    let guard=tx.query("SELECT 1 FROM rss_candidates r JOIN download_processing d ON d.candidate_id=r.id JOIN download_processing_policies p ON p.provider_id=r.client_id AND p.media_type=r.media_type JOIN providers v ON v.id=r.client_id JOIN provider_scopes s ON s.provider_id=v.id AND s.media_type=r.media_type JOIN remote_path_mappings m ON m.id=? AND m.media_type=r.media_type WHERE r.id=? AND r.status='observed' AND r.media_type=? AND d.status='checking' AND d.policy_revision=? AND p.revision=d.policy_revision AND p.provider_revision=r.client_revision AND p.enabled=1 AND p.mode=? AND v.revision=r.client_revision AND v.enabled=1 AND m.revision=? AND m.host=?",params![input.mapping_id,input.candidate_id.clone(),domain,input.policy_revision,input.mode.name(),input.mapping_revision,input.host.clone()]).await?.next().await?;
    if guard.is_none() {
        return Err(Error::conflict(
            "preflight_changed",
            "Download processing policy, provider or mapping changed",
        ));
    }
    let (media, episode, movie) = target(&input.target);
    tx.execute("INSERT INTO operations(id,media_type,episode_id,movie_id,source,mode,destination,status,message)VALUES(?,?,?,?,?,?,?,'preview','Owned completed download import')",params![operation.clone(),media,episode,movie,plan.source.clone(),plan.mode.name(),plan.destination.clone()]).await?;
    tx.execute(
        "INSERT INTO import_journal(operation_id,plan_json,phase)VALUES(?,?,'preview')",
        params![operation.clone(), json(&plan)?],
    )
    .await?;
    let provenance = serde_json::json!({"policy_revision":input.policy_revision,"mapping_id":input.mapping_id,"mapping_revision":input.mapping_revision,"host":input.host,"expected_size":input.expected_size,"comparison_facts":accepted.evidence});
    if json(&provenance)?.len() > 16384 {
        return Err(Error::bad("Import factual provenance exceeds limit"));
    }
    let old_id = old.as_ref().map(|v| v.file_id);
    tx.execute("INSERT INTO rss_candidate_imports(candidate_id,operation_id,quality_id,revision_json,edition,provenance_json,old_file_json,old_episode_file_id,old_movie_file_id)VALUES(?,?,?,?,?,?,?,?,?)",params![input.candidate_id.clone(),operation.clone(),input.quality_id,input.revision_json,input.edition,json(&provenance)?,old.as_ref().map(json).transpose()?,if episode.is_some(){old_id}else{None},if movie.is_some(){old_id}else{None}]).await?;
    tx.execute("UPDATE download_processing SET status='importing',error_code=NULL,updated_at=unixepoch() WHERE candidate_id=? AND status='checking'",[input.candidate_id]).await?;
    tx.commit().await?;
    status(db, &operation).await
}

pub(super) async fn retire(
    c: &Connection,
    operation: &str,
    target: &MediaTarget,
    plan: &Plan,
    stage: &Stage,
    lease: &Arc<Permit>,
) -> Result<()> {
    let Some(mut facts) = facts(c, operation).await? else {
        return Ok(());
    };
    let Some(old) = facts.old.take() else {
        return Ok(());
    };
    if facts.retirement_state != "pending" {
        return Ok(());
    }
    committed_owner(c, operation, target, plan).await?;
    if matches!(target, MediaTarget::Episode(_))
        && c.query(
            "SELECT 1 FROM episodes WHERE episode_file_id=? LIMIT 1",
            [old.file_id],
        )
        .await?
        .next()
        .await?
        .is_some()
    {
        c.execute("UPDATE rss_candidate_imports SET retirement_state='shared_retained' WHERE operation_id=? AND retirement_state='pending'",[operation]).await?;
        return Ok(());
    }
    if facts.retirement.is_none() {
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
        c.execute("UPDATE rss_candidate_imports SET retirement_json=? WHERE operation_id=? AND retirement_state='pending'",params![json(&checkpoint)?,operation]).await?;
        facts.retirement = Some(checkpoint);
    }
    committed_owner(c, operation, target, plan).await?;
    let p = plan.clone();
    let s = stage.clone();
    let original = old.clone();
    let checkpoint = facts.retirement.ok_or_else(Error::internal)?;
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
    committed_owner(&tx, operation, target, plan).await?;
    if matches!(target, MediaTarget::Episode(_)) {
        if tx
            .query(
                "SELECT 1 FROM episodes WHERE episode_file_id=? LIMIT 1",
                [old.file_id],
            )
            .await?
            .next()
            .await?
            .is_some()
        {
            return Err(Error::conflict(
                "target_changed",
                "Old file acquired another association; retained recovery artifact requires inspection",
            ));
        }
        if old.path != plan.destination
            && tx
                .execute(
                    "UPDATE episode_files SET path=? WHERE id=? AND path=?",
                    params![old.recovery_path()?, old.file_id, old.path],
                )
                .await?
                != 1
        {
            return Err(Error::conflict(
                "target_changed",
                "Old file path changed during retirement",
            ));
        }
    }
    tx.execute("UPDATE rss_candidate_imports SET retirement_state='quarantined' WHERE operation_id=? AND retirement_state='pending'",[operation]).await?;
    tx.commit().await?;
    Ok(())
}

pub(super) async fn destination_available(
    c: &Connection,
    target: &MediaTarget,
    path: &str,
    old: Option<i64>,
    same_path: bool,
) -> Result<()> {
    if !same_path {
        return destination_free(c, path).await;
    }
    let fid = old.ok_or_else(Error::internal)?;
    let (old_path, _) = old_record(c, target, fid).await?;
    if old_path != path {
        return Err(Error::conflict(
            "target_changed",
            "Original file path changed",
        ));
    }
    if let MediaTarget::Episode(episode) = target {
        let count: i64 = c
            .query(
                "SELECT count(*) FROM episodes WHERE episode_file_id=? AND id!=?",
                params![fid, *episode],
            )
            .await?
            .next()
            .await?
            .ok_or_else(Error::internal)?
            .get(0)?;
        if count != 0 {
            return Err(Error::conflict(
                "shared_file_conflict",
                "Same-path replacement would change another episode's file",
            ));
        }
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
        .and_then(|v| v.old)
        .filter(|old| old.path == plan.destination))
}
pub(super) async fn available(
    c: &Connection,
    operation: &str,
    target: &MediaTarget,
    plan: &Plan,
) -> Result<()> {
    let old = placement_old(c, operation, plan).await?;
    destination_available(
        c,
        target,
        &plan.destination,
        old.as_ref().map(|v| v.file_id),
        old.is_some(),
    )
    .await
}
pub(super) async fn install(
    c: &Connection,
    operation: &str,
    target: &MediaTarget,
    plan: &Plan,
    stage: &Stage,
    lease: &Arc<Permit>,
) -> Result<()> {
    let Some(old) = placement_old(c, operation, plan).await? else {
        return Err(Error::internal());
    };
    before(c, operation, target).await?;
    available(c, operation, target, plan).await?;
    let state = c
        .query(
            "SELECT state FROM same_path_replacements WHERE operation_id=?",
            [operation],
        )
        .await?
        .next()
        .await?
        .map(|r| r.get::<String>(0))
        .transpose()?;
    if state.as_deref() == Some("restore_intent") {
        let p = plan.clone();
        let s = stage.clone();
        let o = old.clone();
        leased(lease, move || p.restore_exchange(&s, &o)).await?;
        c.execute(
            "UPDATE same_path_replacements SET state='restored' WHERE operation_id=?",
            [operation],
        )
        .await?;
    }
    if state.is_none() {
        c.execute(
            "INSERT INTO same_path_replacements(operation_id)VALUES(?)",
            [operation],
        )
        .await?;
    }
    c.execute("UPDATE same_path_replacements SET state='exchange_intent' WHERE operation_id=? AND state='restored'",[operation]).await?;
    let p = plan.clone();
    let s = stage.clone();
    let installed = state.as_deref() == Some("installed");
    leased(lease, move || {
        if installed {
            p.verify_destination(&s)?;
        }
        p.exchange(&s, &old)
    })
    .await?;
    c.execute(
        "UPDATE same_path_replacements SET state='installed' WHERE operation_id=?",
        [operation],
    )
    .await?;
    Ok(())
}
/// Fresh durable read is mandatory: commit errors do not establish rollback.
pub(super) async fn restore_uncommitted(
    db: &Database,
    operation: &str,
    lease: &Arc<Permit>,
) -> Result<()> {
    let c = db.connect().await?;
    let record = load(&c, operation).await?;
    if matches!(record.phase.as_str(), "committed" | "complete")
        || c.query(
            "SELECT 1 FROM import_history WHERE operation_id=?",
            [operation],
        )
        .await?
        .next()
        .await?
        .is_some()
    {
        return Ok(());
    }
    let Some(plan) = record.plan else {
        return Ok(());
    };
    let Some(stage) = record.stage else {
        return Ok(());
    };
    let Some(old) = placement_old(&c, operation, &plan).await? else {
        return Ok(());
    };
    let Some(row) = c
        .query(
            "SELECT state FROM same_path_replacements WHERE operation_id=?",
            [operation],
        )
        .await?
        .next()
        .await?
    else {
        return Ok(());
    };
    let state: String = row.get(0)?;
    drop(row);
    if state == "restored" {
        return Ok(());
    }
    c.execute("UPDATE same_path_replacements SET state='restore_intent' WHERE operation_id=? AND state IN ('exchange_intent','installed')",[operation]).await?;
    leased(lease, move || plan.restore_exchange(&stage, &old)).await?;
    c.execute(
        "UPDATE same_path_replacements SET state='restored' WHERE operation_id=?",
        [operation],
    )
    .await?;
    Ok(())
}
