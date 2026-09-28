//! Effective profile decisions; legacy minute-only settings never invent preference.
use super::{Disposition, ReleaseDecision, ReleaseTarget, Result, SearchContext, SearchError};
use crate::{
    api::MediaDomain,
    delay_profiles::{EffectiveDelay, Protocol},
    providers::indexer::Release,
};
use libsql::Connection;

pub(crate) fn protocol(release: &Release) -> Protocol {
    if release.facts.torrent.is_some() {
        Protocol::Torrent
    } else {
        Protocol::Usenet
    }
}
pub(crate) async fn select(
    c: &Connection,
    media: MediaDomain,
    target: &ReleaseTarget,
) -> Result<EffectiveDelay> {
    let owner = match target {
        ReleaseTarget::Tv { series_id, .. } => *series_id,
        ReleaseTarget::Movies { movie_id } => *movie_id,
    };
    crate::delay_profiles::select(c, media, owner)
        .await
        .map_err(|error| SearchError(error.code()))
}
pub(crate) fn preferred(policy: &EffectiveDelay, release: &Release) -> bool {
    matches!(policy, EffectiveDelay::Profile {settings, ..} if settings.preferred_protocol == protocol(release))
}
pub(super) fn apply(
    policy: &EffectiveDelay,
    release: &Release,
    context: SearchContext,
    now: i64,
    oldest_pending_publication: Option<i64>,
    quality: super::decision::QualityAssessment,
    decision: &mut ReleaseDecision,
) {
    let torrent = protocol(release) == Protocol::Torrent;
    let (minutes, bypass, full) = match policy {
        EffectiveDelay::Unconfigured { .. } => {
            if context == SearchContext::Rss {
                decision.deny("release_policy_unconfigured");
            }
            return;
        }
        EffectiveDelay::LegacyAgeOnly {
            torrent_delay_minutes,
            usenet_delay_minutes,
            ..
        } => (
            if torrent {
                *torrent_delay_minutes
            } else {
                *usenet_delay_minutes
            },
            false,
            false,
        ),
        EffectiveDelay::Profile { settings, .. } => {
            if !(if torrent {
                settings.enable_torrent
            } else {
                settings.enable_usenet
            }) {
                decision.deny("protocol_disabled");
            }
            let bypass = preferred(policy, release)
                && (quality.revision_bypass
                    || (settings.bypass_if_highest_quality && quality.highest_allowed)
                    || (settings.bypass_if_above_custom_format_score
                        && decision.custom_formats.as_ref().is_some_and(|score| {
                            score.score >= i64::from(settings.minimum_custom_format_score)
                        })));
            (
                if torrent {
                    settings.torrent_delay_minutes
                } else {
                    settings.usenet_delay_minutes
                },
                bypass,
                true,
            )
        }
    };
    // User search skips waiting, not protocol admission. Every other veto survives bypass.
    if context != SearchContext::Rss || decision.disposition == Disposition::Reject {
        return;
    }
    if full && (minutes == 0 || bypass) {
        return;
    }
    let seconds = i64::from(minutes) * 60;
    // Pending age comes from publication, independently of current eligibility. Its
    // strict boundary differs from the candidate's own inclusive age boundary.
    if full
        && oldest_pending_publication
            .is_some_and(|published| published.saturating_add(seconds) < now)
    {
        return;
    }
    let Some(published) = super::timestamp(&release.metadata.published_at) else {
        decision.deny("release_date_unknown");
        return;
    };
    let own_deadline = published.saturating_add(seconds);
    if own_deadline > now {
        let cohort_deadline = if full {
            oldest_pending_publication.map(|old| old.saturating_add(seconds).saturating_add(1))
        } else {
            None
        };
        decision.disposition = Disposition::Delay;
        decision.not_before =
            Some(cohort_deadline.map_or(own_deadline, |old| old.min(own_deadline)));
        decision.reasons.push("configured_delay".into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn release(published: i64, torrent: bool) -> Release {
        let date = chrono::DateTime::from_timestamp(published, 0)
            .unwrap()
            .to_rfc2822();
        crate::providers::indexer::parse_page(&format!(r#"<rss xmlns:torznab="http://torznab.com/schemas/2015/feed"><channel><item><title>Harbor.S01E01.1080p.WEB-DL</title><pubDate>{date}</pubDate><link>https://fixture.invalid/release</link><torznab:attr name="category" value="5030"/></item></channel></rss>"#),0,100,torrent,MediaDomain::Tv).unwrap().items.remove(0)
    }
    fn policy() -> crate::delay_profiles::FullSettings {
        crate::delay_profiles::FullSettings {
            torrent_delay_minutes: 10,
            usenet_delay_minutes: 10,
            enable_torrent: true,
            enable_usenet: true,
            preferred_protocol: Protocol::Torrent,
            bypass_if_highest_quality: false,
            bypass_if_above_custom_format_score: false,
            minimum_custom_format_score: 10,
        }
    }
    fn decision(score: i64) -> ReleaseDecision {
        ReleaseDecision {
            target: None,
            disposition: Disposition::Accept,
            reasons: vec![],
            not_before: None,
            quality_id: Some(3),
            parsed: None,
            custom_formats: Some(crate::custom_formats::Score {
                score,
                format_ids: vec![],
            }),
        }
    }
    #[test]
    fn exact_delay_boundaries_preserve_legacy_and_all_vetoes() {
        let now = 10000;
        let full = EffectiveDelay::Profile {
            revision: 1,
            id: 1,
            settings: policy(),
        };
        for (own_age, oldest_age, expected, deadline) in [
            (600, None, Disposition::Accept, None),
            (599, None, Disposition::Delay, Some(now + 1)),
            (0, Some(600), Disposition::Delay, Some(now + 1)),
            (0, Some(601), Disposition::Accept, None),
        ] {
            let mut result = decision(0);
            apply(
                &full,
                &release(now - own_age, true),
                SearchContext::Rss,
                now,
                oldest_age.map(|age| now - age),
                Default::default(),
                &mut result,
            );
            assert_eq!(result.disposition, expected);
            assert_eq!(result.not_before, deadline);
        }
        let legacy = EffectiveDelay::LegacyAgeOnly {
            revision: 1,
            torrent_delay_minutes: 10,
            usenet_delay_minutes: 10,
        };
        let mut result = decision(100);
        apply(
            &legacy,
            &release(now, true),
            SearchContext::Rss,
            now,
            Some(now - 601),
            super::super::decision::QualityAssessment {
                highest_allowed: true,
                revision_bypass: true,
            },
            &mut result,
        );
        assert_eq!(result.disposition, Disposition::Delay);
        assert_eq!(result.not_before, Some(now + 600));
        for torrent in [true, false] {
            for bypass in ["revision", "highest", "score"] {
                let mut settings = policy();
                settings.bypass_if_highest_quality = bypass == "highest";
                settings.bypass_if_above_custom_format_score = bypass == "score";
                let policy = EffectiveDelay::Profile {
                    revision: 1,
                    id: 1,
                    settings,
                };
                let quality = super::super::decision::QualityAssessment {
                    highest_allowed: true,
                    revision_bypass: bypass == "revision",
                };
                let mut result = decision(10);
                apply(
                    &policy,
                    &release(now, torrent),
                    SearchContext::Rss,
                    now,
                    None,
                    quality,
                    &mut result,
                );
                assert_eq!(
                    result.disposition,
                    if torrent {
                        Disposition::Accept
                    } else {
                        Disposition::Delay
                    },
                    "{bypass}"
                );
                let mut rejected = decision(10);
                rejected.deny("nuked_release");
                apply(
                    &policy,
                    &release(now, torrent),
                    SearchContext::Rss,
                    now,
                    None,
                    quality,
                    &mut rejected,
                );
                assert_eq!(rejected.disposition, Disposition::Reject);
            }
        }
        let mut settings = policy();
        settings.bypass_if_above_custom_format_score = true;
        let mut below = decision(9);
        apply(
            &EffectiveDelay::Profile {
                revision: 1,
                id: 1,
                settings: settings.clone(),
            },
            &release(now, true),
            SearchContext::Rss,
            now,
            None,
            Default::default(),
            &mut below,
        );
        assert_eq!(below.disposition, Disposition::Delay);
        settings.enable_torrent = false;
        let mut disabled = decision(100);
        apply(
            &EffectiveDelay::Profile {
                revision: 1,
                id: 1,
                settings,
            },
            &release(now, true),
            SearchContext::UserSearch,
            now,
            None,
            Default::default(),
            &mut disabled,
        );
        assert_eq!(disabled.reasons, vec!["protocol_disabled"]);
        assert_eq!(disabled.disposition, Disposition::Reject);
    }
    #[test]
    fn policy_storage_and_invariant_errors_have_distinct_http_statuses() {
        use axum::response::IntoResponse;
        for code in [
            "delay_profile_storage_error",
            "revision_policy_storage_error",
        ] {
            assert_eq!(
                SearchError(code).into_response().status(),
                axum::http::StatusCode::SERVICE_UNAVAILABLE
            );
        }
        for code in [
            "delay_profile_invariant",
            "revision_policy_missing",
            "revision_policy_invalid",
        ] {
            assert_eq!(
                SearchError(code).into_response().status(),
                axum::http::StatusCode::INTERNAL_SERVER_ERROR
            );
        }
    }
}
