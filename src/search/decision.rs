use super::*;
use crate::{
    providers::indexer::{Release, ReleaseIdentifiers},
    quality_profiles::{Cutoff, ProfileItem},
};
use libsql::{Connection, params};

struct Candidate {
    target: ReleaseTarget,
    profile: Option<i64>,
    runtime: Option<i64>,
    minimum: Option<String>,
    status: Option<String>,
    cinema: Option<String>,
    digital: Option<String>,
    physical: Option<String>,
    language: Option<i64>,
    monitored: bool,
    files: Vec<Option<i64>>,
    aired: bool,
}
fn id_agrees(actual: Option<i64>, supplied: Option<u32>) -> bool {
    supplied.is_none_or(|id| actual == Some(i64::from(id)))
}

/// Evaluates current library facts, never supplied numeric targets or client categories alone.
pub async fn evaluate(
    c: &Connection,
    media: MediaDomain,
    release: &Release,
    context: SearchContext,
    now: i64,
) -> Result<ReleaseDecision> {
    let tv = matches!(media, MediaDomain::Tv);
    let title = release.metadata.title.as_deref().unwrap_or("");
    let parsed = match parser::parse(title, tv) {
        Ok(v) => v,
        Err(code) => return Ok(ReleaseDecision::reject(code)),
    };
    let categories = &release.metadata.categories;
    if !categories.iter().any(|id| {
        if tv {
            (5000..6000).contains(id)
        } else {
            (2000..3000).contains(id)
        }
    }) || categories.iter().any(|id| {
        if tv {
            (2000..3000).contains(id)
        } else {
            (5000..6000).contains(id)
        }
    }) {
        return Ok(ReleaseDecision::reject("wrong_media_category"));
    }
    if !matches!(
        (&release.facts.identifiers, tv),
        (ReleaseIdentifiers::Tv { .. }, true) | (ReleaseIdentifiers::Movie { .. }, false)
    ) {
        return Ok(ReleaseDecision::reject("wrong_media_identifiers"));
    }
    // ponytail: serial scans stop at 10000 rows; index normalized identities when measured libraries need more.
    // ponytail: serial matching caps each scan at 10,000 rows; index normalized
    // identities and batch evaluation when larger-library throughput requires it.
    let mut candidates = Vec::new();
    if tv {
        let ReleaseIdentifiers::Tv {
            tvdb_id,
            tvmaze_id,
            tvrage_id,
            tmdb_id,
            imdb_id,
        } = &release.facts.identifiers
        else {
            unreachable!()
        };
        if tvmaze_id.is_some() || tvrage_id.is_some() || tmdb_id.is_some() || imdb_id.is_some() {
            return Ok(ReleaseDecision::reject("tv_identity_unsupported"));
        }
        let mut scanned = 0;
        let mut rows=c.query("SELECT s.id,s.title,s.tvdb_id,s.monitored,l.quality_profile_id,l.series_type,l.use_scene_numbering,s.original_language FROM series s LEFT JOIN library_settings l ON l.series_id=s.id ORDER BY s.id LIMIT 10001",()).await?;
        while let Some(r) = rows.next().await? {
            scanned += 1;
            if scanned > 10000 {
                return Err(SearchError("library_match_limit"));
            }
            let id = r.get::<i64>(0)?;
            if !id_agrees(r.get(2)?, *tvdb_id) {
                continue;
            }
            if tvdb_id.is_none()
                && parser::normalize(&r.get::<String>(1)?) != parser::normalize(&parsed.title)
            {
                continue;
            }
            let scene = r.get::<Option<i64>>(6)? == Some(1);
            let Some(series_type) = r.get::<Option<String>>(5)? else {
                return Ok(ReleaseDecision::reject("series_type_unconfigured"));
            };
            // A release and its eventual downloaded file must select the same unique episode.
            // Daily-on-anime and ordinary absolute numbering are admitted by source policy;
            // query-family construction remains independently driven by the configured type.
            let resolved = match parsed.numbering.as_ref().unwrap() {
                parser::Numbering::Daily { date } if series_type != "standard" => {
                    Some(downloaded::daily_candidates(c, id, date).await?)
                }
                parser::Numbering::Daily { .. } => continue,
                parser::Numbering::Absolute { episode } => {
                    Some(downloaded::absolute_candidates(c, id, scene, *episode).await?)
                }
                _ => None,
            };
            if resolved.as_ref().is_some_and(|ids| ids.len() != 1) {
                continue;
            }
            let mut eps=c.query("SELECT e.id,e.season,e.number,e.absolute_episode_number,e.air_date,e.monitored,e.runtime,e.episode_file_id,e.scene_season_number,e.scene_episode_number,e.scene_absolute_episode_number,s.monitored,e.air_date_utc FROM episodes e JOIN seasons s ON s.series_id=e.series_id AND s.number=e.season WHERE e.series_id=? ORDER BY e.id LIMIT 10001",[id]).await?;
            let mut ids = Vec::new();
            let mut matched_numbers = std::collections::BTreeSet::new();
            let mut aired = true;
            let mut files = Vec::new();
            let mut runtime = Some(0i64);
            let mut monitored = r.get::<i64>(3)? == 1;
            let mut scanned_episodes = 0;
            while let Some(e) = eps.next().await? {
                let episode_id = e.get::<i64>(0)?;
                scanned_episodes += 1;
                if scanned_episodes > 10000 {
                    return Err(SearchError("episode_match_limit"));
                }
                let season = if scene {
                    e.get::<Option<i64>>(8)?
                } else {
                    Some(e.get(1)?)
                };
                let number = if scene {
                    e.get::<Option<i64>>(9)?
                } else {
                    Some(e.get(2)?)
                };
                let matches = match parsed.numbering.as_ref().unwrap() {
                    parser::Numbering::Episodes {
                        season: s,
                        episodes,
                    } => season == Some(*s) && number.is_some_and(|n| episodes.contains(&n)),
                    parser::Numbering::Season { season: s } => season == Some(*s),
                    parser::Numbering::Daily { .. } | parser::Numbering::Absolute { .. } => {
                        resolved
                            .as_ref()
                            .is_some_and(|ids| ids.contains(&episode_id))
                    }
                };
                if matches {
                    ids.push(e.get(0)?);
                    if let Some(n) = number {
                        matched_numbers.insert(n);
                    }
                    aired &= e
                        .get::<Option<String>>(12)?
                        .as_deref()
                        .and_then(timestamp)
                        .is_some_and(|t| t <= now);
                    files.push(e.get(7)?);
                    monitored &= e.get::<i64>(5)? == 1 && e.get::<i64>(11)? == 1;
                    runtime = runtime
                        .zip(e.get::<Option<i64>>(6)?)
                        .and_then(|(a, b)| a.checked_add(b));
                }
            }
            if ids.len() > 10000 {
                return Err(SearchError("episode_match_limit"));
            }
            let complete = match parsed.numbering.as_ref().unwrap() {
                parser::Numbering::Episodes { episodes, .. } => {
                    episodes.len() == ids.len()
                        && episodes.iter().all(|n| matched_numbers.contains(n))
                }
                parser::Numbering::Daily { .. } | parser::Numbering::Absolute { .. } => {
                    ids.len() == 1
                }
                _ => !ids.is_empty(),
            };
            if !complete {
                continue;
            }
            candidates.push(Candidate {
                target: ReleaseTarget::Tv {
                    series_id: id,
                    episode_ids: ids,
                },
                profile: r.get(4)?,
                runtime,
                minimum: None,
                status: None,
                cinema: None,
                digital: None,
                physical: None,
                language: r.get(7)?,
                monitored,
                files,
                aired,
            });
        }
    } else {
        let ReleaseIdentifiers::Movie { tmdb_id, imdb_id } = &release.facts.identifiers else {
            unreachable!()
        };
        let mut rows=c.query("SELECT m.id,d.id,d.title,d.year,d.secondary_year,d.tmdb_id,d.imdb_id,m.monitored,l.quality_profile_id,l.minimum_availability,d.runtime,d.status,d.in_cinemas,d.digital_release,d.physical_release,d.original_language,f.id FROM movies m JOIN movie_metadata d ON d.id=m.metadata_id LEFT JOIN library_settings l ON l.movie_id=m.id LEFT JOIN movie_files f ON f.movie_id=m.id ORDER BY m.id LIMIT 10001",()).await?;
        let mut scanned = 0;
        while let Some(r) = rows.next().await? {
            scanned += 1;
            if scanned > 10000 {
                return Err(SearchError("library_match_limit"));
            }
            let stored_imdb = r.get::<Option<String>>(6)?;
            if !id_agrees(r.get(5)?, *tmdb_id)
                || imdb_id
                    .as_ref()
                    .is_some_and(|v| stored_imdb.as_ref() != Some(v))
            {
                continue;
            }
            if parsed.year != r.get::<Option<i64>>(3)? && parsed.year != r.get::<Option<i64>>(4)? {
                continue;
            }
            if tmdb_id.is_none() && imdb_id.is_none() {
                let mut names = vec![r.get::<String>(2)?];
                let mut aliases = c
                    .query(
                        "SELECT title FROM movie_alternative_titles WHERE metadata_id=? LIMIT 65",
                        [r.get::<i64>(1)?],
                    )
                    .await?;
                while let Some(a) = aliases.next().await? {
                    names.push(a.get(0)?)
                }
                if names.len() > 65 {
                    return Err(SearchError("movie_alias_limit"));
                }
                if !names
                    .iter()
                    .any(|s| parser::normalize(s) == parser::normalize(&parsed.title))
                {
                    continue;
                }
            }
            candidates.push(Candidate {
                target: ReleaseTarget::Movies {
                    movie_id: r.get(0)?,
                },
                profile: r.get(8)?,
                runtime: r.get(10)?,
                minimum: r.get(9)?,
                status: r.get(11)?,
                cinema: r.get(12)?,
                digital: r.get(13)?,
                physical: r.get(14)?,
                language: r.get(15)?,
                monitored: r.get::<i64>(7)? == 1,
                files: vec![r.get(16)?],
                aired: true,
            });
        }
    }
    if candidates.len() != 1 {
        return Ok(ReleaseDecision::reject(if candidates.is_empty() {
            "no_library_match"
        } else {
            "ambiguous_library_match"
        }));
    }
    let candidate = candidates.remove(0);
    let safe = |id: &i64| (1..=9_007_199_254_740_991).contains(id);
    if !match &candidate.target {
        ReleaseTarget::Tv {
            series_id,
            episode_ids,
        } => safe(series_id) && episode_ids.iter().all(safe),
        ReleaseTarget::Movies { movie_id } => safe(movie_id),
    } {
        return Err(SearchError("invalid_stored_target"));
    }
    let mut result = ReleaseDecision {
        target: Some(candidate.target.clone()),
        disposition: Disposition::Accept,
        reasons: Vec::new(),
        not_before: None,
        quality_id: None,
        parsed: Some(parsed.clone()),
        custom_formats: None,
    };
    if candidate.status.as_deref() == Some("deleted") {
        result.deny("movie_deleted")
    }
    if context == SearchContext::Rss && !candidate.aired {
        result.deny("episode_not_aired")
    }
    if context == SearchContext::Rss && !candidate.monitored {
        result.deny("not_monitored")
    }
    if release.facts.nuked == Some(true) {
        result.deny("nuked_release")
    }

    let mut evidence = crate::custom_formats::release(release, &parsed, tv);
    crate::custom_formats::populate_quality(c, &mut evidence, parsed.quality_name.as_deref(), tv)
        .await?;
    apply_quality(
        c,
        media,
        candidate.profile,
        &candidate.files,
        candidate.runtime,
        release.metadata.size_bytes,
        &parsed,
        &evidence,
        candidate.language,
        context,
        now,
        &mut result,
    )
    .await?;
    let policy=c.query("SELECT torrent_delay_minutes,usenet_delay_minutes,availability_delay_days FROM release_delay_policies WHERE media_type=?",[domain(media)]).await?.next().await?;
    if context == SearchContext::Rss {
        if let Some(p) = policy {
            if !tv && !available(&candidate, now, p.get::<i64>(2)?) {
                result.deny("movie_unavailable")
            }
            let delay = p.get::<i64>(if release.facts.torrent.is_some() {
                0
            } else {
                1
            })? * 60;
            if let Some(published) = timestamp(&release.metadata.published_at) {
                let until = published.saturating_add(delay);
                if until > now && result.disposition == Disposition::Accept {
                    result.disposition = Disposition::Delay;
                    result.not_before = Some(until);
                    result.reasons.push("configured_delay".into());
                }
            } else {
                result.deny("release_date_unknown")
            }
        } else {
            result.deny("release_policy_unconfigured")
        }
    }
    let (column, id) = match &candidate.target {
        ReleaseTarget::Tv { series_id, .. } => ("series_id", *series_id),
        ReleaseTarget::Movies { movie_id } => ("movie_id", *movie_id),
    };
    if c.query(
        &format!("SELECT 1 FROM blocklist_entries WHERE {column}=? AND source_title=? LIMIT 1"),
        params![id, title],
    )
    .await?
    .next()
    .await?
    .is_some()
    {
        result.deny("blocklisted_release")
    }
    Ok(result)
}
fn available(c: &Candidate, now: i64, delay_days: i64) -> bool {
    let reached = |value: &Option<String>| {
        value
            .as_deref()
            .and_then(timestamp)
            .is_some_and(|t| t.saturating_add(delay_days * 86400) <= now)
    };
    match c.minimum.as_deref() {
        Some("tba") => true,
        Some("announced") => {
            c.status
                .as_deref()
                .is_some_and(|s| matches!(s, "announced" | "in_cinemas" | "released"))
                || c.cinema.is_some()
                || c.digital.is_some()
                || c.physical.is_some()
        }
        Some("in_cinemas") => reached(&c.cinema) || reached(&c.digital) || reached(&c.physical),
        Some("released") => {
            reached(&c.digital)
                || reached(&c.physical)
                || (c.digital.is_none()
                    && c.physical.is_none()
                    && c.cinema
                        .as_deref()
                        .and_then(timestamp)
                        .is_some_and(|t| t.saturating_add((90 + delay_days) * 86400) <= now))
        }
        _ => false,
    }
}

/// Shared profile/rank/size/cutoff checks for releases and already downloaded files.
pub(super) async fn apply_quality(
    c: &Connection,
    media: MediaDomain,
    profile_id: Option<i64>,
    files: &[Option<i64>],
    runtime: Option<i64>,
    size_bytes: Option<u64>,
    parsed: &crate::search::parser::ParsedRelease,
    evidence: &crate::custom_formats::Evidence,
    original_language: Option<i64>,
    search_context: SearchContext,
    now: i64,
    result: &mut ReleaseDecision,
) -> Result<()> {
    let tv = matches!(media, MediaDomain::Tv);
    let quality = if let Some(name) = &parsed.quality_name {
        c.query("SELECT quality_id,min_size,max_size FROM quality_definitions WHERE media_type=? AND name=?",params![domain(media),name.as_str()]).await?.next().await?
    } else {
        None
    };
    if let Some(q) = quality {
        let quality_id = q.get::<i64>(0)?;
        result.quality_id = Some(quality_id);
        if let Some(profile_id) = profile_id {
            let profile = crate::quality_profiles::fetch(c, domain(media), profile_id)
                .await
                .map_err(|_| SearchError("release_profile_error"))?;
            let ranked = profile_ranks(&profile.items);
            if let Some((_, rank, allowed, min, max)) =
                ranked.iter().find(|(id, ..)| *id == quality_id)
            {
                if !allowed {
                    result.deny("quality_not_allowed")
                }
                let minimum = min.or(q.get::<Option<f64>>(1)?);
                let maximum = max.or(q.get::<Option<f64>>(2)?);
                if minimum.is_some() || maximum.is_some() {
                    if let Some((size, runtime)) = size_bytes.zip(runtime.filter(|v| *v > 0)) {
                        let per_minute = size as f64 / (1024.0 * 1024.0 * runtime as f64);
                        if minimum.is_some_and(|v| per_minute < v)
                            || maximum.is_some_and(|v| per_minute > v)
                        {
                            result.deny("size_outside_profile")
                        }
                    } else {
                        result.deny("size_or_runtime_unknown")
                    }
                }
                if let Some(policy) = profile.policy {
                    let formats = crate::custom_formats::catalog(c, media)
                        .await
                        .map_err(|_| SearchError("custom_format_catalog_invalid"))?;
                    let score = crate::custom_formats::score(
                        &formats,
                        media,
                        evidence.clone(),
                        original_language,
                        policy.format_items.clone(),
                        !c.is_autocommit(),
                    )
                    .await?;
                    if score.score < i64::from(policy.min_format_score) {
                        result.deny("custom_format_minimum_score");
                    }
                    result.custom_formats = Some(score.clone());
                    if !tv && policy.language_id != Some(-1) {
                        let wanted = if policy.language_id == Some(-2) {
                            original_language
                                .filter(|v| *v > 0)
                                .and_then(|v| i32::try_from(v).ok())
                        } else {
                            policy.language_id
                        };
                        if !wanted.is_some_and(|id| {
                            evidence
                                .languages
                                .as_ref()
                                .is_some_and(|langs| langs.contains(&id))
                        }) {
                            result.deny("language_not_wanted");
                        }
                    }
                    let cutoff = match policy.cutoff {
                        Cutoff::Quality { quality_id } => ranked
                            .iter()
                            .find(|(id, ..)| *id == quality_id)
                            .map(|(_, r, ..)| *r),
                        Cutoff::Group { position } => Some(position),
                    };
                    let mode = crate::revision_policy::read(c, media)
                        .await
                        .map_err(|_| SearchError("revision_policy_unavailable"))?
                        .mode;
                    let anime = if let Some(ReleaseTarget::Tv { series_id, .. }) = &result.target {
                        c.query(
                            "SELECT series_type='anime' FROM library_settings WHERE series_id=?",
                            [*series_id],
                        )
                        .await?
                        .next()
                        .await?
                        .map(|r| r.get::<Option<i64>>(0))
                        .transpose()?
                        .flatten()
                            == Some(1)
                    } else {
                        false
                    };
                    let clock = revision::EvaluationClock::local(now).map_err(SearchError)?;
                    let context = revision::Context {
                        mode,
                        search: search_context,
                        anime,
                        queued: false,
                        clock,
                    };
                    let comparison_policy = revision::Profile {
                        upgrade_allowed: policy.upgrade_allowed,
                        cutoff_rank: cutoff,
                        cutoff_score: i64::from(policy.cutoff_format_score),
                        minimum_increment: i64::from(policy.min_upgrade_format_score),
                    };
                    let unique_files: std::collections::BTreeSet<_> =
                        files.iter().flatten().copied().collect();
                    for file in unique_files {
                        let column = if tv {
                            "episode_file_id"
                        } else {
                            "movie_file_id"
                        };
                        let existing = c.query(&format!("SELECT quality_id,revision_json,release_group,date_added FROM file_metadata WHERE media_type=? AND {column}=?"),params![domain(media),file]).await?.next().await?;
                        let Some(row) = existing else {
                            result.deny("existing_quality_unknown");
                            continue;
                        };
                        let old_quality = row.get::<Option<i64>>(0)?;
                        let Some((old_id, old_rank, ..)) =
                            old_quality.and_then(|id| ranked.iter().find(|(q, ..)| *q == id))
                        else {
                            result.deny("existing_quality_unknown");
                            continue;
                        };
                        let old_revision = row
                            .get::<Option<String>>(1)?
                            .map(|s| {
                                serde_json::from_str::<crate::media_files::FileRevision>(&s)
                                    .map_err(|_| SearchError("existing_revision_invalid"))
                            })
                            .transpose()?;
                        if let Some(old) = &old_revision {
                            old.validate()
                                .map_err(|_| SearchError("existing_revision_invalid"))?;
                        }
                        let old_group: Option<String> = row.get(2)?;
                        let old_date = row.get::<Option<String>>(3)?.as_deref().and_then(timestamp);
                        let old_facts = crate::custom_formats::existing(c, media, file).await?;
                        let old_score = crate::custom_formats::score(
                            &formats,
                            media,
                            old_facts,
                            original_language,
                            policy.format_items.clone(),
                            !c.is_autocommit(),
                        )
                        .await?
                        .score;
                        let incoming = revision::Facts {
                            quality_id,
                            rank: *rank,
                            revision: parsed.revision.as_ref(),
                            score: score.score,
                            group: evidence.release_group.as_deref(),
                            date_added: None,
                        };
                        let current = revision::Facts {
                            quality_id: *old_id,
                            rank: *old_rank,
                            revision: old_revision.as_ref(),
                            score: old_score,
                            group: old_group.as_deref(),
                            date_added: old_date,
                        };
                        let comparison =
                            revision::compare(&incoming, &current, &comparison_policy, &context);
                        for reason in comparison.reasons {
                            result.deny(reason);
                        }
                    }
                } else {
                    result.deny("quality_policy_unconfigured")
                }
            } else {
                result.deny("quality_not_allowed")
            }
        } else {
            result.deny("quality_profile_unconfigured")
        }
    } else {
        result.deny("quality_unknown")
    }
    Ok(())
}

fn profile_ranks(items: &[ProfileItem]) -> Vec<(i64, usize, bool, Option<f64>, Option<f64>)> {
    let mut ranked = Vec::new();
    for (rank, item) in items.iter().enumerate() {
        match item {
            ProfileItem::Quality(l) => {
                ranked.push((l.quality_id, rank, l.allowed, l.min_size, l.max_size))
            }
            ProfileItem::Group { allowed, items, .. } => {
                for l in items {
                    ranked.push((
                        l.quality_id,
                        rank,
                        *allowed && l.allowed,
                        l.min_size,
                        l.max_size,
                    ));
                }
            }
        }
    }
    ranked
}
pub(crate) async fn target_ranks(
    c: &Connection,
    target: &crate::db::MediaTarget,
) -> Result<Vec<(i64, usize)>> {
    let (media, column, id) = match target {
        crate::db::MediaTarget::Episode(id) => (
            MediaDomain::Tv,
            "series_id",
            c.query("SELECT series_id FROM episodes WHERE id=?", [*id])
                .await?
                .next()
                .await?
                .ok_or(SearchError("release_target_missing"))?
                .get::<i64>(0)?,
        ),
        crate::db::MediaTarget::Movie(id) => (MediaDomain::Movies, "movie_id", *id),
    };
    let profile = c
        .query(
            &format!("SELECT quality_profile_id FROM library_settings WHERE {column}=?"),
            [id],
        )
        .await?
        .next()
        .await?
        .ok_or(SearchError("release_profile_error"))?
        .get::<Option<i64>>(0)?
        .ok_or(SearchError("release_profile_error"))?;
    let profile = crate::quality_profiles::fetch(c, domain(media), profile)
        .await
        .map_err(|_| SearchError("release_profile_error"))?;
    Ok(profile_ranks(&profile.items)
        .into_iter()
        .filter(|(_, _, allowed, ..)| *allowed)
        .map(|(id, rank, ..)| (id, rank))
        .collect())
}
