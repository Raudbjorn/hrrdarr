//! Revision facts and policy comparison shared by release and owned-file decisions.
use super::{SearchContext, parser::ParsedRelease};
use crate::{media_files::FileRevision, revision_policy::Mode};
use chrono::{Days, Local, LocalResult, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Observation {
    pub parser_version: u8,
    pub value: FileRevision,
    pub explicit_marker: bool,
}
impl Observation {
    pub(crate) fn from_parsed(parsed: &ParsedRelease) -> Option<Self> {
        parsed.revision.clone().map(|value| Self {
            parser_version: 1,
            value,
            explicit_marker: parsed.revision_marker,
        })
    }
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        if self.parser_version != 1 {
            return Err("revision_evidence_version");
        }
        self.value
            .validate()
            .map_err(|_| "revision_evidence_invalid")
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Binding {
    pub version: u8,
    pub release: Option<Observation>,
    pub filename: Option<Observation>,
    pub effective: FileRevision,
}
impl Binding {
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        if self.version != 1 {
            return Err("revision_evidence_version");
        }
        self.effective
            .validate()
            .map_err(|_| "revision_evidence_invalid")?;
        for observed in [self.release.as_ref(), self.filename.as_ref()]
            .into_iter()
            .flatten()
        {
            observed.validate()?;
            if observed.value != self.effective {
                return Err("revision_evidence_conflict");
            }
        }
        if self.filename.as_ref().is_some_and(|v| !v.explicit_marker)
            || (self.release.is_none() && self.filename.is_none())
        {
            return Err("revision_evidence_invalid");
        }
        Ok(())
    }
}
pub(crate) fn reconcile(
    release: Option<&Observation>,
    filename: &ParsedRelease,
) -> Result<Binding, &'static str> {
    if let Some(release) = release {
        release.validate()?;
    }
    // An unmarked filename can have lost markers during a rename. Only a versioned
    // release observation establishes a marker-free baseline for automatic import.
    let file = Observation::from_parsed(filename).filter(|v| v.explicit_marker);
    if let Some(file) = &file {
        file.validate()?;
    }
    if let (Some(release), Some(file)) = (release, file.as_ref()) {
        if release.value != file.value {
            return Err("revision_evidence_conflict");
        }
    }
    let effective = file
        .as_ref()
        .or(release)
        .ok_or("revision_unknown")?
        .value
        .clone();
    Ok(Binding {
        version: 1,
        release: release.cloned(),
        filename: file,
        effective,
    })
}
pub(crate) fn order(a: &FileRevision, b: &FileRevision) -> Ordering {
    (a.real, a.version).cmp(&(b.real, b.version))
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct EvaluationClock {
    pub proper_cutoff_utc: i64,
}
fn single_midnight<T: TimeZone>(
    value: LocalResult<chrono::DateTime<T>>,
) -> Result<i64, &'static str> {
    match value {
        LocalResult::Single(value) => Ok(value.timestamp()),
        LocalResult::Ambiguous(..) => Err("proper_age_clock_ambiguous"),
        LocalResult::None => Err("proper_age_clock_nonexistent"),
    }
}
impl EvaluationClock {
    pub(crate) fn local(now_utc: i64) -> Result<Self, &'static str> {
        let today = Utc
            .timestamp_opt(now_utc, 0)
            .single()
            .ok_or("proper_age_clock_invalid")?
            .with_timezone(&Local)
            .date_naive();
        let midnight = today
            .checked_sub_days(Days::new(7))
            .and_then(|date| date.and_hms_opt(0, 0, 0))
            .ok_or("proper_age_clock_invalid")?;
        Ok(Self {
            proper_cutoff_utc: single_midnight(Local.from_local_datetime(&midnight))?,
        })
    }
}

pub(crate) struct Facts<'a> {
    pub quality_id: i64,
    pub rank: usize,
    pub revision: Option<&'a FileRevision>,
    pub score: i64,
    pub group: Option<&'a str>,
    pub date_added: Option<i64>,
}
pub(crate) struct Context {
    pub mode: Mode,
    pub search: SearchContext,
    pub anime: bool,
    pub queued: bool,
    pub clock: EvaluationClock,
}
pub(crate) struct Profile {
    pub upgrade_allowed: bool,
    pub cutoff_rank: Option<usize>,
    pub cutoff_score: i64,
    pub minimum_increment: i64,
}
#[derive(Debug)]
pub(crate) struct Comparison {
    pub quality_order: Ordering,
    pub revision_order: Option<Ordering>,
    pub exact_revision_upgrade: bool,
    pub reasons: Vec<&'static str>,
}
impl Comparison {
    pub(crate) fn allowed(&self) -> bool {
        self.reasons.is_empty()
    }
    pub(crate) fn delay_bypass(&self, tv: bool, preferred: bool, mode: Mode) -> bool {
        preferred
            && self.allowed()
            && if tv {
                mode == Mode::PreferAndUpgrade
                    && self.quality_order == Ordering::Equal
                    && self.revision_order == Some(Ordering::Greater)
            } else {
                self.exact_revision_upgrade
            }
    }
}
pub(crate) fn compare(
    candidate: &Facts<'_>,
    current: &Facts<'_>,
    profile: &Profile,
    context: &Context,
) -> Comparison {
    let quality_order = candidate.rank.cmp(&current.rank);
    let revision_order = candidate
        .revision
        .zip(current.revision)
        .map(|(a, b)| order(a, b));
    let exact_revision_upgrade =
        candidate.quality_id == current.quality_id && revision_order == Some(Ordering::Greater);
    let mut result = Comparison {
        quality_order,
        revision_order,
        exact_revision_upgrade,
        reasons: Vec::new(),
    };
    if !profile.upgrade_allowed {
        result.reasons.push("upgrades_disabled");
    }
    match quality_order {
        Ordering::Less => result.reasons.push("not_quality_upgrade"),
        Ordering::Greater => {
            if profile.cutoff_rank.is_some_and(|rank| current.rank >= rank) {
                result.reasons.push("cutoff_met");
            }
        }
        Ordering::Equal => {
            let prefer = context.mode != Mode::DoNotPrefer;
            if prefer && revision_order.is_none() {
                result.reasons.push("revision_unknown");
            }
            if prefer && revision_order == Some(Ordering::Less) {
                result.reasons.push("not_revision_upgrade");
            }
            if !(prefer && exact_revision_upgrade) {
                if current.score >= profile.cutoff_score {
                    result.reasons.push("cutoff_met");
                } else if candidate.score <= current.score {
                    result.reasons.push("custom_format_not_upgrade");
                } else if candidate.score.saturating_sub(current.score) < profile.minimum_increment
                {
                    result.reasons.push("custom_format_upgrade_increment");
                }
            }
        }
    }
    if context.mode != Mode::DoNotPrefer && exact_revision_upgrade {
        if context.queued && context.mode == Mode::DoNotUpgrade {
            result.reasons.push("queued_revision_upgrade_disabled");
        }
        if context.search == SearchContext::Rss {
            if context.mode == Mode::DoNotUpgrade {
                result.reasons.push("revision_upgrade_disabled");
            } else {
                match current.date_added {
                    None => result.reasons.push("revision_age_unknown"),
                    Some(date) if date < context.clock.proper_cutoff_utc => {
                        result.reasons.push("revision_too_old")
                    }
                    _ => {}
                }
            }
        }
        let group = candidate
            .group
            .filter(|s| !s.trim().is_empty())
            .zip(current.group.filter(|s| !s.trim().is_empty()));
        if candidate.revision.is_some_and(|r| r.is_repack) {
            if context.mode == Mode::DoNotUpgrade {
                result.reasons.push("repack_upgrade_disabled");
            }
            if !group.is_some_and(|(a, b)| a.eq_ignore_ascii_case(b)) {
                result.reasons.push("repack_group_mismatch");
            }
        }
        if context.anime && !group.is_some_and(|(a, b)| a == b) {
            result.reasons.push("anime_revision_group_mismatch");
        }
    }
    result
}
/// A total key: unknown cannot compare equal to all known revisions and then use
/// CF ties, which would create a non-transitive ordering among three candidates.
pub(crate) fn preference_key(
    rank: usize,
    revision: Option<&FileRevision>,
    score: i64,
    mode: Mode,
) -> Result<(usize, i64, i64, i64), &'static str> {
    let (real, version) = if mode == Mode::DoNotPrefer {
        (0, 0)
    } else {
        let revision = revision.ok_or("revision_unknown")?;
        revision
            .validate()
            .map_err(|_| "revision_evidence_invalid")?;
        (revision.real, revision.version)
    };
    Ok((rank, real, version, score))
}

/// The native preference boundary is shared by retained search and pending RSS.
/// Protocol preference follows quality/revision/CF; existing deterministic ties remain.
pub(crate) type ReleasePreference = ((usize, i64, i64, i64), bool, u32, i64);
pub(crate) async fn release_preference(
    c: &libsql::Connection,
    media: crate::api::MediaDomain,
    release: &crate::providers::indexer::Release,
    decision: &super::ReleaseDecision,
) -> super::Result<ReleasePreference> {
    let target = decision
        .target
        .as_ref()
        .ok_or(super::SearchError("invalid_stored_target"))?;
    let owned = match target {
        super::ReleaseTarget::Tv { episode_ids, .. } => crate::db::MediaTarget::Episode(
            *episode_ids
                .first()
                .ok_or(super::SearchError("invalid_stored_target"))?,
        ),
        super::ReleaseTarget::Movies { movie_id } => crate::db::MediaTarget::Movie(*movie_id),
    };
    let ranks = super::target_ranks(c, &owned).await?;
    let rank = ranks
        .iter()
        .find(|(id, _)| Some(*id) == decision.quality_id)
        .map(|(_, rank)| *rank)
        .ok_or(super::SearchError("invalid_stored_quality"))?;
    let mode = crate::revision_policy::read(c, media)
        .await
        .map_err(|error| super::SearchError(error.code()))?
        .mode;
    let quality = preference_key(
        rank,
        decision
            .parsed
            .as_ref()
            .and_then(|parsed| parsed.revision.as_ref()),
        decision
            .custom_formats
            .as_ref()
            .map_or(0, |score| score.score),
        mode,
    )
    .map_err(super::SearchError)?;
    let delay = super::delay::select(c, media, target).await?;
    Ok((
        quality,
        super::delay::preferred(&delay, release),
        release.metadata.seeders.unwrap_or(0),
        super::timestamp(&release.metadata.published_at).unwrap_or(0),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn revision(version: i64, real: i64, is_repack: bool) -> FileRevision {
        FileRevision {
            version,
            real,
            is_repack,
        }
    }
    #[test]
    fn evidence_reconciliation_preserves_unknown_and_conflicts() {
        let plain = super::super::parser::parse("Harbor.2020.1080p.WEB-DL", false).unwrap();
        let proper = super::super::parser::parse("Harbor.2020.1080p.WEB-DL.PROPER", false).unwrap();
        let repack = super::super::parser::parse("Harbor.2020.1080p.WEB-DL.REPACK", false).unwrap();
        assert_eq!(reconcile(None, &plain).unwrap_err(), "revision_unknown");
        let observation = Observation::from_parsed(&proper).unwrap();
        let inherited = reconcile(Some(&observation), &plain).unwrap();
        assert_eq!(inherited.effective, revision(2, 0, false));
        assert!(inherited.filename.is_none());
        assert_eq!(
            reconcile(Some(&observation), &repack).unwrap_err(),
            "revision_evidence_conflict"
        );
        assert_eq!(
            reconcile(None, &repack).unwrap().effective,
            revision(2, 0, true)
        );
        let legacy = serde_json::json!({"version":1,"real":0,"is_repack":false,"extra":1});
        assert!(serde_json::from_value::<FileRevision>(legacy).is_err());
        assert!(revision(0, 0, false).validate().is_err());
        assert!(
            revision(1, i64::from(i32::MAX) + 1, false)
                .validate()
                .is_err()
        );
    }
    #[test]
    fn comparison_distinguishes_quality_identity_policy_and_groups() {
        let old = revision(1, 0, false);
        let proper = revision(2, 0, false);
        let repack = revision(2, 0, true);
        let current = Facts {
            quality_id: 1,
            rank: 1,
            revision: Some(&old),
            score: 100,
            group: Some("Group"),
            date_added: Some(100),
        };
        let mut candidate = Facts {
            quality_id: 1,
            rank: 1,
            revision: Some(&proper),
            score: 0,
            group: Some("group"),
            date_added: None,
        };
        let mut context = Context {
            mode: Mode::PreferAndUpgrade,
            search: SearchContext::Rss,
            anime: false,
            queued: false,
            clock: EvaluationClock {
                proper_cutoff_utc: 100,
            },
        };
        let mut policy = Profile {
            upgrade_allowed: true,
            cutoff_rank: Some(1),
            cutoff_score: 0,
            minimum_increment: 10,
        };
        let comparison = compare(&candidate, &current, &policy, &context);
        assert!(comparison.allowed()); // Boundary equality accepts; revision path precedes CF cutoff.
        assert!(comparison.delay_bypass(false, true, context.mode));
        candidate.quality_id = 2;
        assert!(!compare(&candidate, &current, &policy, &context).exact_revision_upgrade);
        assert!(!compare(&candidate, &current, &policy, &context).allowed());
        candidate.quality_id = 1;
        context.clock.proper_cutoff_utc = 101;
        assert!(
            compare(&candidate, &current, &policy, &context)
                .reasons
                .contains(&"revision_too_old")
        );
        context.search = SearchContext::UserSearch;
        assert!(compare(&candidate, &current, &policy, &context).allowed());
        context.mode = Mode::DoNotUpgrade;
        assert!(compare(&candidate, &current, &policy, &context).allowed()); // Explicit search bypasses RSS proper guard only.
        candidate.revision = Some(&repack);
        assert!(
            compare(&candidate, &current, &policy, &context)
                .reasons
                .contains(&"repack_upgrade_disabled")
        );
        context.mode = Mode::PreferAndUpgrade;
        assert!(compare(&candidate, &current, &policy, &context).allowed()); // Repack group comparison ignores case.
        context.anime = true;
        assert!(
            compare(&candidate, &current, &policy, &context)
                .reasons
                .contains(&"anime_revision_group_mismatch")
        );
        context.mode = Mode::DoNotPrefer;
        assert_eq!(
            compare(&candidate, &current, &policy, &context).reasons,
            vec!["cutoff_met"]
        );
        policy.upgrade_allowed = false;
        assert!(
            compare(&candidate, &current, &policy, &context)
                .reasons
                .contains(&"upgrades_disabled")
        );
    }
    #[test]
    fn movie_delay_can_follow_cf_upgrade_without_revision_preference() {
        let old = revision(1, 0, false);
        let new = revision(2, 0, false);
        let current = Facts {
            quality_id: 1,
            rank: 1,
            revision: Some(&old),
            score: 0,
            group: None,
            date_added: None,
        };
        let candidate = Facts {
            quality_id: 1,
            rank: 1,
            revision: Some(&new),
            score: 10,
            group: None,
            date_added: None,
        };
        let mut policy = Profile {
            upgrade_allowed: true,
            cutoff_rank: Some(1),
            cutoff_score: 100,
            minimum_increment: 1,
        };
        let context = Context {
            mode: Mode::DoNotPrefer,
            search: SearchContext::Rss,
            anime: false,
            queued: false,
            clock: EvaluationClock {
                proper_cutoff_utc: 0,
            },
        };
        let comparison = compare(&candidate, &current, &policy, &context);
        assert!(comparison.allowed());
        assert!(comparison.delay_bypass(false, true, context.mode));
        assert!(!comparison.delay_bypass(true, true, context.mode));
        assert!(!comparison.delay_bypass(false, false, context.mode));
        policy.upgrade_allowed = false;
        assert!(
            !compare(&candidate, &current, &policy, &context).delay_bypass(
                false,
                true,
                context.mode
            )
        );
    }
    #[test]
    fn unknown_and_total_preference_are_explicit() {
        let low = revision(1, 0, false);
        let high = revision(2, 0, false);
        let real = revision(1, 1, false);
        let mode = Mode::PreferAndUpgrade;
        assert!(
            preference_key(1, Some(&high), 0, mode).unwrap()
                > preference_key(1, Some(&low), 100, mode).unwrap()
        );
        assert!(
            preference_key(1, Some(&real), 0, mode).unwrap()
                > preference_key(1, Some(&high), 100, mode).unwrap()
        );
        assert_eq!(
            preference_key(1, None, 50, mode).unwrap_err(),
            "revision_unknown"
        );
        assert_eq!(
            preference_key(1, None, 50, Mode::DoNotPrefer).unwrap(),
            (1, 0, 0, 50)
        );
        assert_eq!(order(&high, &revision(2, 0, true)), Ordering::Equal);
        let current = Facts {
            quality_id: 1,
            rank: 1,
            revision: None,
            score: 0,
            group: None,
            date_added: None,
        };
        let mut candidate = Facts {
            quality_id: 1,
            rank: 1,
            revision: Some(&high),
            score: 10,
            group: None,
            date_added: None,
        };
        let policy = Profile {
            upgrade_allowed: true,
            cutoff_rank: Some(3),
            cutoff_score: 100,
            minimum_increment: 1,
        };
        let context = Context {
            mode,
            search: SearchContext::UserSearch,
            anime: false,
            queued: false,
            clock: EvaluationClock {
                proper_cutoff_utc: 0,
            },
        };
        assert_eq!(
            compare(&candidate, &current, &policy, &context).reasons,
            vec!["revision_unknown"]
        );
        candidate.rank = 2;
        assert!(compare(&candidate, &current, &policy, &context).allowed());
    }
    #[test]
    fn clock_child() {
        let Ok(case) = std::env::var("HRRDARR_REVISION_CLOCK_CASE") else {
            return;
        };
        let (now, cutoff) = match case.as_str() {
            "spring" => ("2026-03-12T12:00:00Z", "2026-03-05T05:00:00Z"),
            "autumn" => ("2026-11-05T12:00:00Z", "2026-10-29T04:00:00Z"),
            _ => panic!("unexpected isolated clock case"),
        };
        let timestamp = |s| chrono::DateTime::parse_from_rfc3339(s).unwrap().timestamp();
        let clock = EvaluationClock::local(timestamp(now)).unwrap();
        assert_eq!(clock.proper_cutoff_utc, timestamp(cutoff));
    }
    #[test]
    fn clock_in_isolated_timezone() {
        for case in ["spring", "autumn"] {
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "search::revision::tests::clock_child",
                    "--nocapture",
                ])
                .env("TZ", "America/New_York")
                .env("HRRDARR_REVISION_CLOCK_CASE", case)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stdout)
            );
        }
    }
    #[test]
    fn midnight_resolution_never_guesses() {
        let instant = Utc.timestamp_opt(100, 0).unwrap();
        assert_eq!(single_midnight(LocalResult::Single(instant)).unwrap(), 100);
        assert_eq!(
            single_midnight(LocalResult::Ambiguous(instant, instant)).unwrap_err(),
            "proper_age_clock_ambiguous"
        );
        assert_eq!(
            single_midnight::<Utc>(LocalResult::None).unwrap_err(),
            "proper_age_clock_nonexistent"
        );
    }
}
