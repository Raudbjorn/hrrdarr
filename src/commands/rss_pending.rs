//! Bounded cross-command pending cohorts. External-action ownership is never reassigned.
use super::*;

pub(super) struct Pending {
    pub work: CandidateWork,
    pub release: indexer::Release,
}
fn overlaps(a: &ReleaseTarget, b: &ReleaseTarget) -> bool {
    match (a, b) {
        (ReleaseTarget::Movies { movie_id: a }, ReleaseTarget::Movies { movie_id: b }) => a == b,
        (
            ReleaseTarget::Tv {
                series_id: a,
                episode_ids: ae,
            },
            ReleaseTarget::Tv {
                series_id: b,
                episode_ids: be,
            },
        ) => a == b && ae.iter().any(|id| be.contains(id)),
        _ => false,
    }
}
pub(super) fn decode(client: &RefreshClient, work: &CandidateWork) -> Result<indexer::Release> {
    let mut bytes = client
        .open_release(
            &payload_context(work.public.id, work.public.source),
            work.payload.as_deref().ok_or_else(bad)?,
        )
        .map_err(|error| Error(StatusCode::SERVICE_UNAVAILABLE, error.code))?;
    let decoded = indexer::decode_private(&bytes);
    bytes.fill(0);
    decoded.map_err(|_| bad())
}
// Preparation has already selected an owner. Continue that exact receipt before
// processing older pending siblings that still supply its publication-age evidence.
async fn prepared_owner(c: &Connection, target: &ReleaseTarget) -> Result<Option<CandidateWork>> {
    let (column, owner, media) = match target {
        ReleaseTarget::Tv { series_id, .. } => ("series_id", *series_id, "tv"),
        ReleaseTarget::Movies { movie_id } => ("movie_id", *movie_id, "movies"),
    };
    let mut rows = c.query(&format!("SELECT id FROM rss_candidates WHERE media_type=? AND {column}=? AND status='prepared' ORDER BY created_at,id"), params![media,owner]).await?;
    let mut ids = Vec::new();
    while let Some(row) = rows.next().await? {
        if ids.len() >= 1024 {
            return Err(Error(StatusCode::TOO_MANY_REQUESTS, "candidate_limit"));
        }
        ids.push(Uuid::parse_str(&row.get::<String>(0)?).map_err(|_| bad())?);
    }
    drop(rows);
    for id in ids {
        let work = match candidate(c, id).await {
            Ok(work) => work,
            Err(error) => {
                settle_due_error(c, id, error).await?;
                continue;
            }
        };
        if work
            .public
            .target
            .as_ref()
            .is_some_and(|other| overlaps(target, other))
        {
            // Preserve origin, including explicit UserSearch. process_candidate reads
            // that receipt's own authority and current policy/source/identity again.
            return Ok(Some(work));
        }
    }
    Ok(None)
}
pub(super) async fn cohort(
    c: &Connection,
    client: &RefreshClient,
    target: &ReleaseTarget,
) -> Result<Vec<Pending>> {
    let (column, owner, media) = match target {
        ReleaseTarget::Tv { series_id, .. } => ("series_id", *series_id, "tv"),
        ReleaseTarget::Movies { movie_id } => ("movie_id", *movie_id, "movies"),
    };
    // No LIMIT: the admission cap bounds all candidates, and truncating the set
    // could change either its oldest publication or its highest-ranked release.
    let mut rows = c.query(&format!("SELECT id FROM rss_candidates WHERE media_type=? AND {column}=? AND status='pending' ORDER BY id"), params![media,owner]).await?;
    let mut ids = Vec::new();
    while let Some(row) = rows.next().await? {
        if ids.len() >= 1024 {
            return Err(Error(StatusCode::TOO_MANY_REQUESTS, "candidate_limit"));
        }
        ids.push(Uuid::parse_str(&row.get::<String>(0)?).map_err(|_| bad())?);
    }
    drop(rows);
    let mut output = Vec::new();
    for id in ids {
        let work = match candidate(c, id).await {
            Ok(work) => work,
            Err(error) => {
                if transient_local_error(error.1) {
                    return Err(error);
                }
                eprintln!(
                    "event=rss_pending_invalid candidate_id={id} code={}",
                    error.1
                );
                if !c.is_autocommit() {
                    settle_due_error(c, id, error).await?;
                }
                continue;
            }
        };
        if !work
            .public
            .target
            .as_ref()
            .is_some_and(|other| overlaps(target, other))
        {
            continue;
        }
        // Explicit search has already selected its offer; it is not an RSS delay candidate.
        if !matches!(
            work.public.origin,
            super::super::search::CandidateOrigin::Rss
        ) {
            continue;
        }
        match decode(client, &work) {
            Ok(release) => output.push(Pending { work, release }),
            Err(error) => {
                // Do not discard an unrelated valid receipt because a sibling cannot
                // be decoded. Settle that exact row only under the current writer.
                eprintln!(
                    "event=rss_pending_unreadable candidate_id={id} code={}",
                    error.1
                );
                if !c.is_autocommit() {
                    settle_due_error(c, id, error).await?;
                }
            }
        }
    }
    Ok(output)
}
pub(super) fn oldest(items: &[Pending]) -> Option<i64> {
    items
        .iter()
        .filter_map(|item| crate::search::timestamp(&item.release.metadata.published_at))
        .min()
}
pub(super) async fn evaluate(
    c: &Connection,
    client: &RefreshClient,
    media: MediaDomain,
    indexer: Uuid,
    release: &indexer::Release,
    context: SearchContext,
    timestamp: i64,
    operation: &mut crate::release_profile_terms::OperationEvidence,
) -> Result<crate::search::ReleaseDecision> {
    let decision = crate::search::evaluate_with_evidence(
        c, media, indexer, release, context, timestamp, operation,
    )
    .await
    .map_err(|error| Error(StatusCode::INTERNAL_SERVER_ERROR, error.0))?;
    let Some(target) = decision.target.as_ref() else {
        return Ok(decision);
    };
    if context != SearchContext::Rss
        || !matches!(
            crate::search::delay::select(c, media, target)
                .await
                .map_err(|error| Error(StatusCode::INTERNAL_SERVER_ERROR, error.0))?,
            crate::delay_profiles::EffectiveDelay::Profile { .. }
        )
    {
        return Ok(decision);
    }
    let items = cohort(c, client, target).await?;
    crate::search::evaluate_with_pending(
        c,
        media,
        indexer,
        release,
        context,
        timestamp,
        oldest(&items),
        operation,
    )
    .await
    .map_err(|error| Error(StatusCode::INTERNAL_SERVER_ERROR, error.0))
}
pub(super) async fn best(
    c: &Connection,
    items: &[Pending],
    timestamp: i64,
    operation: &mut crate::release_profile_terms::OperationEvidence,
) -> Result<Option<Uuid>> {
    if c.is_autocommit() {
        return Err(bad());
    }
    let oldest = oldest(items);
    let mut best: Option<(crate::search::revision::ReleasePreference, Uuid)> = None;
    for item in items {
        let p = &item.work.public;
        if !singleton(&p.target) || !valid_target(c, p.source).await? {
            continue;
        }
        let decision = match crate::search::evaluate_with_pending(
            c,
            p.source.media_type,
            p.source.indexer_id,
            &item.release,
            SearchContext::Rss,
            timestamp,
            oldest,
            operation,
        )
        .await
        {
            Ok(decision) => decision,
            Err(error) => {
                // Attribute permanent validation failures to the sibling that caused
                // them. Transient catalog/storage failures abort this transaction.
                settle_due_error(c, p.id, Error(StatusCode::INTERNAL_SERVER_ERROR, error.0))
                    .await?;
                continue;
            }
        };
        if decision.disposition != Disposition::Accept || decision.target != p.target {
            continue;
        }
        let rank = match crate::search::revision::release_preference(
            c,
            p.source.media_type,
            &item.release,
            &decision,
        )
        .await
        {
            Ok(rank) => rank,
            Err(error) => {
                settle_due_error(c, p.id, Error(StatusCode::INTERNAL_SERVER_ERROR, error.0))
                    .await?;
                continue;
            }
        };
        if best
            .as_ref()
            .is_none_or(|(prior, id)| rank > *prior || (rank == *prior && p.id < *id))
        {
            best = Some((rank, p.id));
        }
    }
    Ok(best.map(|(_, id)| id))
}
pub(super) async fn select_work(
    db: &Database,
    client: &RefreshClient,
    work: CandidateWork,
    operation: &mut crate::release_profile_terms::OperationEvidence,
) -> Result<CandidateWork> {
    if work.public.status != "pending"
        || !matches!(
            work.public.origin,
            super::super::search::CandidateOrigin::Rss
        )
    {
        return Ok(work);
    }
    let Some(target) = work.public.target.as_ref() else {
        return Ok(work);
    };
    let c = connection(db).await?;
    // Compile only outside the writer; the same complete cohort is read afresh below.
    for item in cohort(&c, client, target).await? {
        if let Err(error) = crate::search::evaluate_with_evidence(
            &c,
            item.work.public.source.media_type,
            item.work.public.source.indexer_id,
            &item.release,
            SearchContext::Rss,
            now()?,
            operation,
        )
        .await
        {
            if transient_local_error(error.0) {
                return Err(Error(StatusCode::INTERNAL_SERVER_ERROR, error.0));
            }
            // Prewarm is not a writer: defer permanent per-item settlement until
            // the current-policy evaluation inside the Immediate transaction.
            eprintln!(
                "event=rss_pending_prewarm_invalid candidate_id={} code={}",
                item.work.public.id, error.0
            );
        }
    }
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        let current = candidate(&tx, work.public.id).await?;
        if current.public.status != "pending" || current.public.target.as_ref() != Some(target) {
            return Ok(current);
        }
        if let Some(owner) = prepared_owner(&tx, target).await? {
            return Ok(owner);
        }
        let items = cohort(&tx, client, target).await?;
        let winner = best(&tx, &items, now()?, operation)
            .await?
            .unwrap_or(work.public.id);
        candidate(&tx, winner).await
    }
    .await;
    finish(tx, outcome).await
}
