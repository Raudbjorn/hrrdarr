//! Release admission rules. These never reinterpret an already observed import receipt.
use super::{ReleaseDecision, ReleaseTarget, Result, SearchError};
use crate::{api::MediaDomain, providers::indexer::Release, release_profile_terms as terms};
use libsql::Connection;

#[allow(clippy::too_many_arguments)]
pub(super) async fn apply(
    c: &Connection,
    media: MediaDomain,
    indexer: uuid::Uuid,
    release: &Release,
    target: &ReleaseTarget,
    air_dates: &[Option<chrono::DateTime<chrono::Utc>>],
    full_season: bool,
    now: i64,
    decision: &mut ReleaseDecision,
    operation: &mut terms::OperationEvidence,
) -> Result<()> {
    let owner = match target {
        ReleaseTarget::Tv { series_id, .. } => *series_id,
        ReleaseTarget::Movies { movie_id } => *movie_id,
    };
    let catalog = crate::release_profiles::applicable(c, media, owner, indexer)
        .await
        .map_err(|e| SearchError(e.code()))?;
    let definitions: Vec<_> = catalog.profiles.iter().map(|p| p.definition()).collect();
    if !definitions.is_empty() {
        let current = crate::release_profiles::terms(&definitions);
        let title = release.metadata.title.as_deref().unwrap_or("");
        if c.is_autocommit() {
            operation
                .evaluate_current(media, current.clone(), title)
                .await
                .map_err(|e| SearchError(e.code()))?;
        }
        // Operation-owned evidence survives global-cache eviction across a whole
        // publish/cohort batch. Applicability and temporal facts are still fresh.
        let matches = match operation.validated(media, &current, title) {
            Ok(value) => value.clone(),
            Err(terms::TermError::StateChanged) => {
                terms::cached_terms(media, &current, title).map_err(|e| SearchError(e.code()))?
            }
            Err(e) => return Err(SearchError(e.code())),
        };
        if matches.profiles.len() != definitions.len() {
            return Err(SearchError("release_profile_invariant"));
        }
        if definitions
            .iter()
            .zip(&matches.profiles)
            .any(|(p, m)| !p.required.is_empty() && m.required.is_empty())
        {
            decision.deny("release_required_term_missing");
        }
        if matches.profiles.iter().any(|m| !m.ignored.is_empty()) {
            decision.deny("release_ignored_term");
        }
    }
    if matches!(media, MediaDomain::Tv) {
        let policies: Vec<_> = definitions.iter().filter_map(|p| p.tv.as_ref()).collect();
        if let Some(grace) = policies
            .iter()
            .filter(|p| p.air_date_restriction)
            .map(|p| p.grace_days)
            .max()
        {
            let publication = instant(&release.metadata.published_at);
            let offset = i64::from(grace)
                .checked_mul(86_400)
                .ok_or(SearchError("release_profile_time_overflow"))?;
            let mut permitted = publication.is_some();
            for date in air_dates {
                match date {
                    Some(date) => {
                        let boundary = date
                            .checked_add_signed(chrono::Duration::seconds(offset))
                            .ok_or(SearchError("release_profile_time_overflow"))?;
                        permitted &= publication.is_some_and(|p| p >= boundary);
                    }
                    None => permitted = false,
                }
            }
            if !permitted {
                decision.deny("release_before_air_date");
            }
        }
        if full_season && (policies.is_empty() || policies.iter().any(|p| !p.allow_unaired_pack)) {
            let boundary = chrono::DateTime::from_timestamp(now, 0)
                .and_then(|now| now.checked_add_signed(chrono::Duration::days(1)))
                .ok_or(SearchError("release_profile_time_overflow"))?;
            if air_dates
                .iter()
                .any(|date| date.is_none_or(|date| date > boundary))
            {
                decision.deny("season_pack_not_aired");
            }
        }
    }
    Ok(())
}

// An episode UTC instant needs an explicit offset; date-only/naive values are
// unknown here. Preserve subseconds rather than rounding before admission.
pub(super) fn instant(value: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|v| v.with_timezone(&chrono::Utc))
}

// One worker dispatch is serial, but HTTP search/grab may overlap it. Bound the
// lifetime of retained batch evidence as well as matcher CPU admission: two
// operations * the matcher's 128 MiB ceiling, with no waiting admission queue.
pub(crate) struct Operation {
    evidence: terms::OperationEvidence,
    _permit: tokio::sync::OwnedSemaphorePermit,
}
impl std::ops::Deref for Operation {
    type Target = terms::OperationEvidence;
    fn deref(&self) -> &Self::Target {
        &self.evidence
    }
}
impl std::ops::DerefMut for Operation {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.evidence
    }
}
pub(crate) fn operation() -> Result<Operation> {
    static ADMISSION: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> =
        std::sync::OnceLock::new();
    let permit = ADMISSION
        .get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(2)))
        .clone()
        .try_acquire_owned()
        .map_err(|_| SearchError("release_term_busy"))?;
    Ok(Operation {
        evidence: terms::OperationEvidence::default(),
        _permit: permit,
    })
}
