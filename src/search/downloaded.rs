//! Downloaded filenames must independently agree with their trusted receipt target.
use super::{Disposition, ReleaseDecision, ReleaseTarget, SearchError, decision, parser};
use crate::{api::MediaDomain, db::MediaTarget};
use libsql::{Connection, params};

pub(crate) struct AcceptedFile {
    pub basename: String,
    pub root: String,
    pub quality_id: i64,
    pub revision_json: String,
    pub revision_binding: super::revision::Binding,
    pub edition: Option<String>,
    pub evidence: crate::custom_formats::Evidence,
}
pub(crate) struct DownloadedDecision {
    pub accepted: Option<AcceptedFile>,
    pub reasons: Vec<String>,
}
fn rejected(reason: &str) -> DownloadedDecision {
    DownloadedDecision {
        accepted: None,
        reasons: vec![reason.into()],
    }
}

// This retains the existing native scene-policy switch; upstream sceneSource/origin mapping
// is a separate unfinished contract. Local rescans deliberately do not call this resolver.
pub(super) async fn absolute_candidates(
    c: &Connection,
    series_id: i64,
    scene: bool,
    episode: i64,
) -> super::Result<Vec<i64>> {
    if scene {
        let hits = absolute_hits(c, series_id, "scene_absolute_episode_number", episode).await?;
        if hits.len() == 1 {
            return Ok(hits);
        }
    }
    absolute_hits(c, series_id, "absolute_episode_number", episode).await
}

#[cfg(test)]
pub(crate) async fn evaluate_fresh_release(
    c: &Connection,
    target: &MediaTarget,
    filename: &str,
    size: u64,
) -> super::Result<DownloadedDecision> {
    // These numbering/quality cases model a newly observed release whose title
    // matches the test file stem. Legacy absent receipt evidence is tested separately.
    let stem = filename
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(filename)
        .rsplit_once('.')
        .map_or(filename, |(s, _)| s);
    let tv = matches!(target, MediaTarget::Episode(_));
    let receipt = parser::parse(stem, tv)
        .ok()
        .map(|parsed| crate::custom_formats::parsed(stem, &parsed, tv, Some(size)));
    evaluate_with_evidence(c, target, filename, size, receipt).await
}
#[cfg(test)]
pub(crate) async fn evaluate_with_evidence(
    c: &Connection,
    target: &MediaTarget,
    filename: &str,
    size: u64,
    receipt: Option<crate::custom_formats::Evidence>,
) -> super::Result<DownloadedDecision> {
    evaluate_bound(
        c,
        target,
        filename,
        size,
        receipt,
        None,
        super::SearchContext::Rss,
    )
    .await
}
pub(crate) async fn evaluate_owned(
    c: &Connection,
    target: &MediaTarget,
    filename: &str,
    size: u64,
    receipt: Option<crate::custom_formats::Evidence>,
    frozen: &crate::media_files::FileRevision,
    context: super::SearchContext,
) -> super::Result<DownloadedDecision> {
    evaluate_bound(c, target, filename, size, receipt, Some(frozen), context).await
}
async fn evaluate_bound(
    c: &Connection,
    target: &MediaTarget,
    filename: &str,
    size: u64,
    receipt: Option<crate::custom_formats::Evidence>,
    frozen: Option<&crate::media_files::FileRevision>,
    context: super::SearchContext,
) -> super::Result<DownloadedDecision> {
    if filename.is_empty() || filename.len() > 4096 || filename.chars().any(char::is_control) {
        return Ok(rejected("invalid_filename"));
    }
    let basename = filename.rsplit(['/', '\\']).next().unwrap_or("");
    let Some((stem, extension)) = basename.rsplit_once('.') else {
        return Ok(rejected("unsupported_file_type"));
    };
    if !matches!(
        extension.to_ascii_lowercase().as_str(),
        "mkv" | "mp4" | "avi" | "m4v" | "ts" | "m2ts"
    ) {
        return Ok(rejected("unsupported_file_type"));
    }
    if stem
        .split(|c: char| !c.is_alphanumeric())
        .any(|v| v.eq_ignore_ascii_case("sample"))
    {
        return Ok(rejected("sample_file"));
    }
    let tv = matches!(target, MediaTarget::Episode(_));
    let mut parsed = match parser::parse(stem, tv) {
        Ok(parsed) => parsed,
        Err(code) => return Ok(rejected(code)),
    };
    let (typed, root, profile, runtime, old_file) = match target {
        MediaTarget::Episode(id) => {
            let Some(r)=c.query("SELECT s.id,s.title,s.path,l.quality_profile_id,l.series_type,l.use_scene_numbering,e.season,e.number,e.runtime,e.episode_file_id,e.air_date,e.absolute_episode_number,e.scene_absolute_episode_number FROM episodes e JOIN series s ON s.id=e.series_id LEFT JOIN library_settings l ON l.series_id=s.id WHERE e.id=?",[*id]).await?.next().await? else{return Ok(rejected("target_changed"))};
            let series_id = r.get::<i64>(0)?;
            let title = r.get::<String>(1)?;
            if parser::normalize(&parsed.title) != parser::normalize(&title) {
                return Ok(rejected("filename_target_mismatch"));
            }
            let Some(series_type) = r.get::<Option<String>>(4)? else {
                return Ok(rejected("series_type_unconfigured"));
            };
            let scene = r.get::<Option<i64>>(5)? == Some(1);
            let target_season = r.get::<i64>(6)?;
            let target_number = r.get::<i64>(7)?;
            let mismatch = match &parsed.numbering {
                Some(parser::Numbering::Episodes { season, episodes }) => {
                    // This arm only reads season/number (not
                    // scene_season_number/scene_episode_number), so a
                    // scene-numbered series -- of any series_type -- is a
                    // separate, pre-existing gap left unfixed here.
                    if scene {
                        Some("numbering_unsupported")
                    } else {
                        (*season != target_season || episodes.as_slice() != [target_number])
                            .then_some("filename_target_mismatch")
                    }
                }
                Some(parser::Numbering::Daily { date }) if series_type != "standard" => {
                    match_daily(c, series_id, *id, r.get::<Option<String>>(10)?, date).await?
                }
                Some(parser::Numbering::Absolute { episode }) => {
                    match_absolute(
                        c,
                        series_id,
                        *id,
                        scene,
                        r.get::<Option<i64>>(12)?,
                        r.get::<Option<i64>>(11)?,
                        *episode,
                    )
                    .await?
                }
                _ => Some("filename_target_mismatch"),
            };
            if let Some(reason) = mismatch {
                return Ok(rejected(reason));
            }
            (
                ReleaseTarget::Tv {
                    series_id,
                    episode_ids: vec![*id],
                },
                r.get::<String>(2)?,
                r.get::<Option<i64>>(3)?,
                r.get::<Option<i64>>(8)?,
                r.get::<Option<i64>>(9)?,
            )
        }
        MediaTarget::Movie(id) => {
            let Some(r)=c.query("SELECT d.id,d.title,d.year,d.secondary_year,m.path,l.quality_profile_id,d.runtime,f.id FROM movies m JOIN movie_metadata d ON d.id=m.metadata_id LEFT JOIN library_settings l ON l.movie_id=m.id LEFT JOIN movie_files f ON f.movie_id=m.id WHERE m.id=?",[*id]).await?.next().await? else{return Ok(rejected("target_changed"))};
            let metadata_id = r.get::<i64>(0)?;
            let title = r.get::<String>(1)?;
            let year = r.get::<Option<i64>>(2)?;
            let secondary = r.get::<Option<i64>>(3)?;
            let root = r.get::<String>(4)?;
            let profile = r.get::<Option<i64>>(5)?;
            let runtime = r.get::<Option<i64>>(6)?;
            let old = r.get::<Option<i64>>(7)?;
            if parsed.year.is_none() || (parsed.year != year && parsed.year != secondary) {
                return Ok(rejected("filename_year_mismatch"));
            }
            let mut matches = parser::normalize(&parsed.title) == parser::normalize(&title);
            if !matches {
                let mut rows = c
                    .query(
                        "SELECT title FROM movie_alternative_titles WHERE metadata_id=? LIMIT 1001",
                        [metadata_id],
                    )
                    .await?;
                let mut count = 0;
                while let Some(r) = rows.next().await? {
                    count += 1;
                    if count > 1000 {
                        return Err(SearchError("library_match_limit"));
                    }
                    matches |=
                        parser::normalize(&r.get::<String>(0)?) == parser::normalize(&parsed.title);
                }
            }
            if !matches {
                return Ok(rejected("filename_target_mismatch"));
            }
            (
                ReleaseTarget::Movies { movie_id: *id },
                root,
                profile,
                runtime,
                old,
            )
        }
    };
    if size == 0 {
        return Ok(rejected("empty_file"));
    }
    let release_observation = receipt.as_ref().and_then(|e| e.revision.as_ref());
    let revision_binding = if let Some(frozen) = frozen {
        frozen
            .validate()
            .map_err(|_| SearchError("revision_evidence_invalid"))?;
        let filename =
            super::revision::Observation::from_parsed(&parsed).filter(|v| v.explicit_marker);
        if filename.as_ref().is_some_and(|v| v.value != *frozen)
            || release_observation.is_some_and(|v| v.value != *frozen)
        {
            return Ok(rejected("revision_evidence_conflict"));
        }
        super::revision::Binding {
            version: 1,
            release: release_observation.cloned(),
            filename,
            effective: frozen.clone(),
        }
    } else {
        match super::revision::reconcile(release_observation, &parsed) {
            Ok(binding) => binding,
            Err(reason) => return Ok(rejected(reason)),
        }
    };
    parsed.revision = Some(revision_binding.effective.clone());
    let mut result = ReleaseDecision {
        target: Some(typed),
        disposition: Disposition::Accept,
        reasons: vec![],
        not_before: None,
        quality_id: None,
        parsed: Some(parsed.clone()),
        custom_formats: None,
    };

    let mut evidence = crate::custom_formats::parsed(stem, &parsed, tv, Some(size));
    if let Some(receipt) = receipt {
        crate::custom_formats::validate_evidence(
            if tv {
                MediaDomain::Tv
            } else {
                MediaDomain::Movies
            },
            &receipt,
        )
        .map_err(SearchError)?;
        evidence.title = receipt.title;
        evidence.indexer_flags = receipt.indexer_flags;
        if evidence.languages.is_none() {
            evidence.languages = receipt.languages;
        }
        if evidence.release_group.is_none() {
            evidence.release_group = receipt.release_group;
        }
    }
    crate::custom_formats::populate_quality(c, &mut evidence, parsed.quality_name.as_deref(), tv)
        .await?;
    evidence.filename = Some(basename.to_owned());
    evidence.revision = revision_binding
        .release
        .clone()
        .or_else(|| revision_binding.filename.clone());
    let original = original_language(c, target).await?;
    decision::apply_quality(
        c,
        if tv {
            MediaDomain::Tv
        } else {
            MediaDomain::Movies
        },
        profile,
        &[old_file],
        runtime,
        Some(size),
        &parsed,
        &evidence,
        original,
        context,
        chrono::Utc::now().timestamp(),
        &mut result,
    )
    .await?;
    if !matches!(result.disposition, Disposition::Accept) {
        return Ok(DownloadedDecision {
            accepted: None,
            reasons: result.reasons,
        });
    }
    Ok(DownloadedDecision {
        accepted: Some(AcceptedFile {
            basename: basename.into(),
            root,
            quality_id: result.quality_id.ok_or(SearchError("quality_unknown"))?,
            revision_json: serde_json::to_string(&revision_binding.effective)
                .map_err(|_| SearchError("revision_evidence_invalid"))?,
            revision_binding,
            edition: parsed.edition,
            evidence,
        }),
        reasons: vec![],
    })
}

/// Daily (air-date) matching: an exact string match against the episode's air
/// date, no tolerance window (Sonarr's EpisodeRepository.FindOneByAirDate).
/// When more than one episode in the series shares that date, specials
/// (season 0) are excluded first; the match is only resolved if that leaves
/// exactly one regular episode -- which must be this target, or the file
/// belongs to that other regular episode instead -- otherwise it stays
/// ambiguous rather than guessing.
async fn match_daily(
    c: &Connection,
    series_id: i64,
    episode_id: i64,
    target_air_date: Option<String>,
    date: &str,
) -> super::Result<Option<&'static str>> {
    let Some(air_date) = target_air_date else {
        return Ok(Some("episode_air_date_unknown"));
    };
    if air_date != date {
        return Ok(Some("filename_target_mismatch"));
    }
    let candidates = daily_candidates(c, series_id, date).await?;
    if candidates.len() == 1 {
        return Ok((candidates[0] != episode_id).then_some("filename_target_mismatch"));
    }
    Ok(Some("ambiguous_air_date_match"))
}

// Shared by release selection and downloaded-target validation. Resolve identity before
// accumulating monitoring/runtime/file facts, so excluded specials cannot affect policy.
pub(super) async fn daily_candidates(
    c: &Connection,
    series_id: i64,
    date: &str,
) -> super::Result<Vec<i64>> {
    let mut rows = c
        .query(
            "SELECT id,season FROM episodes WHERE series_id=? AND air_date=? LIMIT 1001",
            params![series_id, date],
        )
        .await?;
    let mut all = Vec::new();
    while let Some(row) = rows.next().await? {
        if all.len() == 1000 {
            return Err(SearchError("episode_match_limit"));
        }
        all.push((row.get::<i64>(0)?, row.get::<i64>(1)?));
    }
    if all.len() > 1 {
        all.retain(|(_, season)| *season > 0);
    }
    Ok(all.into_iter().map(|(id, _)| id).collect())
}

/// Counts episodes in a series whose `column` equals `number`, bounded the
/// same way the rest of this module bounds unindexed scans.
async fn absolute_hits(
    c: &Connection,
    series_id: i64,
    column: &str,
    number: i64,
) -> super::Result<Vec<i64>> {
    let mut rows = c
        .query(
            &format!("SELECT id FROM episodes WHERE series_id=? AND {column}=? LIMIT 1001"),
            params![series_id, number],
        )
        .await?;
    let mut ids = Vec::new();
    let mut count = 0;
    while let Some(row) = rows.next().await? {
        count += 1;
        if count > 1000 {
            return Err(SearchError("episode_match_limit"));
        }
        ids.push(row.get::<i64>(0)?);
    }
    Ok(ids)
}

/// Absolute (anime) matching, mirroring Sonarr's ParsingService.GetAnimeEpisodes:
/// when the series uses scene numbering, a scene_absolute_episode_number match
/// is used only if it resolves to exactly one episode in the series; an empty
/// or ambiguous scene-column result is discarded (not rejected) and matching
/// retries against the plain absolute_episode_number column, which is also
/// used directly when the series does not use scene numbering. A second
/// episode sharing that final column's value is an unresolved ambiguity
/// (Sonarr's Find(seriesId, absoluteEpisodeNumber) uses `.SingleOrDefault()`,
/// which rejects rather than picks a duplicate).
async fn match_absolute(
    c: &Connection,
    series_id: i64,
    episode_id: i64,
    scene: bool,
    target_scene_absolute: Option<i64>,
    target_absolute: Option<i64>,
    episode: i64,
) -> super::Result<Option<&'static str>> {
    let hits = absolute_candidates(c, series_id, scene, episode).await?;
    match hits.len() {
        0 => {
            if target_absolute.is_none() && (!scene || target_scene_absolute.is_none()) {
                Ok(Some("episode_absolute_number_unknown"))
            } else {
                Ok(Some("filename_target_mismatch"))
            }
        }
        1 => Ok((hits[0] != episode_id).then_some("filename_target_mismatch")),
        _ => Ok(Some("ambiguous_absolute_match")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[tokio::test]
    async fn downloaded_files_require_exact_target_quality_and_size() {
        let scratch = Scratch(
            std::env::temp_dir().join(format!("hrrdarr-downloaded-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir(&scratch.0).unwrap();
        let db = crate::db::Database::open_local(scratch.0.join("db"))
            .await
            .unwrap();
        let c = db.connect().await.unwrap();
        // Any (-1) keeps this fixture language-unrestricted; Original (-2) requires audio matching.
        c.execute_batch("INSERT INTO series(id,title,path)VALUES(1,'Harbor','/fictional/tv'); INSERT INTO seasons(series_id,number)VALUES(1,1);INSERT INTO episodes(id,series_id,season,number,title,runtime)VALUES(1,1,1,1,'Pilot',45);INSERT INTO movie_metadata(id,title,year,runtime)VALUES(1,'Harbor',2020,100);INSERT INTO movies(id,metadata_id,path)VALUES(1,1,'/fictional/movie');INSERT INTO quality_profiles VALUES(1,'tv','HD'),(2,'movies','HD');INSERT INTO quality_profile_items(profile_id,media_type,quality_id,position,allowed)VALUES(1,'tv',3,0,1),(2,'movies',3,0,1);INSERT INTO quality_profile_policies(profile_id,media_type,upgrade_allowed,cutoff_quality_id,min_format_score,cutoff_format_score,min_upgrade_format_score,language_id)VALUES(1,'tv',1,3,0,0,1,NULL),(2,'movies',1,3,0,0,1,-1);INSERT INTO library_settings(media_type,series_id,quality_profile_id,series_type,use_scene_numbering)VALUES('tv',1,1,'standard',0);INSERT INTO library_settings(media_type,movie_id,quality_profile_id,minimum_availability)VALUES('movies',1,2,'released');").await.unwrap();
        for (target, name, reason) in [
            (
                MediaTarget::Episode(1),
                "Harbor.S01E02.1080p.WEB-DL.mkv",
                "filename_target_mismatch",
            ),
            (
                MediaTarget::Movie(1),
                "Harbor.2021.1080p.WEB-DL.mkv",
                "filename_year_mismatch",
            ),
            (
                MediaTarget::Episode(1),
                "Harbor.S01E01.sample.1080p.WEB-DL.mkv",
                "sample_file",
            ),
            (
                MediaTarget::Movie(1),
                "Harbor.2020.sample.1080p.WEB-DL.mkv",
                "sample_file",
            ),
            (
                MediaTarget::Episode(1),
                "Harbor.S01E01.1080p.Bluray.mkv",
                "quality_not_allowed",
            ),
            (
                MediaTarget::Movie(1),
                "Harbor.2020.1080p.Bluray.mkv",
                "quality_not_allowed",
            ),
        ] {
            let result = evaluate_fresh_release(&c, &target, name, 1073741824)
                .await
                .unwrap();
            assert!(result.accepted.is_none(), "{name}");
            assert!(
                result.reasons.iter().any(|r| r == reason),
                "{name}: {:?}",
                result.reasons
            );
        }
        for (target, name) in [
            (
                MediaTarget::Episode(1),
                "folder/Harbor.S01E01.1080p.WEB-DL.mkv",
            ),
            (MediaTarget::Movie(1), "folder/Harbor.2020.1080p.WEB-DL.mkv"),
        ] {
            let accepted = evaluate_fresh_release(&c, &target, name, 1073741824)
                .await
                .unwrap()
                .accepted
                .unwrap();
            assert_eq!(accepted.quality_id, 3);
            assert!(!accepted.basename.contains('/'));
        }
        // No filename/receipt audio evidence exists here, so restricted policies still reject.
        c.execute("UPDATE movie_metadata SET original_language=1", ())
            .await
            .unwrap();
        for language in [-2, 0, 1, 57, -1] {
            c.execute(
                "UPDATE quality_profile_policies SET language_id=? WHERE media_type='movies'",
                [language],
            )
            .await
            .unwrap();
            let result = evaluate_fresh_release(
                &c,
                &MediaTarget::Movie(1),
                "Harbor.2020.1080p.WEB-DL.mkv",
                1073741824,
            )
            .await
            .unwrap();
            assert_eq!(result.accepted.is_some(), language == -1);
            assert_eq!(
                result.reasons.contains(&"language_not_wanted".into()),
                language != -1
            );
            assert!(
                evaluate_fresh_release(
                    &c,
                    &MediaTarget::Episode(1),
                    "Harbor.S01E01.1080p.WEB-DL.mkv",
                    1073741824
                )
                .await
                .unwrap()
                .accepted
                .is_some()
            );
        }
        let tiny = evaluate_fresh_release(
            &c,
            &MediaTarget::Episode(1),
            "Harbor.S01E01.1080p.WEB-DL.mkv",
            1,
        )
        .await
        .unwrap();
        assert!(tiny.reasons.iter().any(|r| r == "size_outside_profile"));
        c.execute_batch("INSERT INTO episode_files(id,series_id,path)VALUES(1,1,'/fictional/tv/old.mkv');UPDATE episodes SET episode_file_id=1 WHERE id=1;INSERT INTO file_metadata(media_type,episode_file_id,quality_id)VALUES('tv',1,3);INSERT INTO movie_files(id,movie_id,path)VALUES(1,1,'/fictional/movie/old.mkv');INSERT INTO file_metadata(media_type,movie_file_id,quality_id)VALUES('movies',1,3);").await.unwrap();
        for (target, name) in [
            (MediaTarget::Episode(1), "Harbor.S01E01.1080p.WEB-DL.mkv"),
            (MediaTarget::Movie(1), "Harbor.2020.1080p.WEB-DL.mkv"),
        ] {
            let result = evaluate_fresh_release(&c, &target, name, 1073741824)
                .await
                .unwrap();
            assert!(
                result.accepted.is_none(),
                "existing cutoff file cannot be replaced by same quality"
            );
        }
    }

    #[tokio::test]
    async fn daily_and_absolute_numbering_match_real_episode_data() {
        let scratch = Scratch(std::env::temp_dir().join(format!(
            "hrrdarr-downloaded-numbering-{}",
            uuid::Uuid::new_v4()
        )));
        std::fs::create_dir(&scratch.0).unwrap();
        let db = crate::db::Database::open_local(scratch.0.join("db"))
            .await
            .unwrap();
        let c = db.connect().await.unwrap();
        c.execute_batch(
            "INSERT INTO quality_profiles VALUES(9,'tv','HD');\
             INSERT INTO quality_profile_items(profile_id,media_type,quality_id,position,allowed)VALUES(9,'tv',3,0,1);\
             INSERT INTO quality_profile_policies(profile_id,media_type,upgrade_allowed,cutoff_quality_id,min_format_score,cutoff_format_score,min_upgrade_format_score,language_id)VALUES(9,'tv',1,3,0,0,1,NULL);\
             INSERT INTO series(id,title,path)VALUES(10,'Nightly','/fictional/tv-daily');\
             INSERT INTO seasons(series_id,number)VALUES(10,0),(10,1);\
             INSERT INTO library_settings(media_type,series_id,quality_profile_id,series_type,use_scene_numbering)VALUES('tv',10,9,'daily',0);\
             INSERT INTO episodes(id,series_id,season,number,title,runtime,air_date)VALUES\
                (101,10,1,1,'Regular airing alone',45,'2024-05-14'),\
                (102,10,1,2,'First of a same-day pair',45,'2024-05-15'),\
                (103,10,1,3,'Second of a same-day pair',45,'2024-05-15'),\
                (104,10,1,4,'Undated episode',45,NULL),\
                (105,10,0,1,'Special sharing the regular episode date',45,'2024-05-14'),\
                (106,10,0,2,'Special that collides with a unique regular episode',45,'2024-05-16'),\
                (107,10,1,5,'The regular episode that actually owns that date',45,'2024-05-16');\
             INSERT INTO series(id,title,path)VALUES(20,'Aurora','/fictional/tv-anime');\
             INSERT INTO seasons(series_id,number)VALUES(20,1);\
             INSERT INTO library_settings(media_type,series_id,quality_profile_id,series_type,use_scene_numbering)VALUES('tv',20,9,'anime',0);\
             INSERT INTO episodes(id,series_id,season,number,title,runtime,absolute_episode_number)VALUES\
                (201,20,1,1,'Unique absolute number',45,23),\
                (202,20,1,2,'First of a duplicate pair',45,24),\
                (203,20,1,3,'Second of a duplicate pair',45,24),\
                (204,20,1,4,'Missing absolute number',45,NULL);\
             INSERT INTO series(id,title,path)VALUES(21,'Borealis','/fictional/tv-anime-scene');\
             INSERT INTO seasons(series_id,number)VALUES(21,1);\
             INSERT INTO library_settings(media_type,series_id,quality_profile_id,series_type,use_scene_numbering)VALUES('tv',21,9,'anime',1);\
             INSERT INTO episodes(id,series_id,season,number,title,runtime,absolute_episode_number,scene_absolute_episode_number)VALUES\
                (301,21,1,1,'Scene-numbered episode',45,999,50),\
                (302,21,1,2,'No scene mapping, falls back to the plain column',45,51,NULL),\
                (303,21,1,3,'Shares a scene number but is unique on the plain column',45,60,60),\
                (304,21,1,4,'Collides on the scene number only',45,70,60);",
        )
        .await
        .unwrap();

        // Daily: an exact air-date match succeeds even though a season-0 special
        // shares the same date (upstream excludes specials when disambiguating).
        let accepted = evaluate_fresh_release(
            &c,
            &MediaTarget::Episode(101),
            "Nightly.2024.05.14.1080p.WEB-DL.mkv",
            1073741824,
        )
        .await
        .unwrap()
        .accepted
        .unwrap();
        assert_eq!(accepted.quality_id, 3);

        // Daily: two regular (non-special) episodes share the same air date, so
        // matching either of them by date alone is ambiguous.
        let ambiguous = evaluate_fresh_release(
            &c,
            &MediaTarget::Episode(102),
            "Nightly.2024.05.15.1080p.WEB-DL.mkv",
            1073741824,
        )
        .await
        .unwrap();
        assert!(ambiguous.accepted.is_none());
        assert!(
            ambiguous
                .reasons
                .iter()
                .any(|r| r == "ambiguous_air_date_match"),
            "{:?}",
            ambiguous.reasons
        );

        // Daily: the target episode has no recorded air date, so it can never be
        // verified against a parsed date -- this is a distinct, honest reason
        // from a plain mismatch.
        let unknown = evaluate_fresh_release(
            &c,
            &MediaTarget::Episode(104),
            "Nightly.2024.01.01.1080p.WEB-DL.mkv",
            1073741824,
        )
        .await
        .unwrap();
        assert!(unknown.accepted.is_none());
        assert!(
            unknown
                .reasons
                .iter()
                .any(|r| r == "episode_air_date_unknown"),
            "{:?}",
            unknown.reasons
        );

        // Daily: the target is a special that shares its date with exactly one
        // regular episode. Upstream resolves that collision to the regular
        // episode, so this file belongs to episode 107, not this special --
        // a mismatch, not an ambiguity.
        let mismatch = evaluate_fresh_release(
            &c,
            &MediaTarget::Episode(106),
            "Nightly.2024.05.16.1080p.WEB-DL.mkv",
            1073741824,
        )
        .await
        .unwrap();
        assert!(mismatch.accepted.is_none());
        assert!(
            mismatch
                .reasons
                .iter()
                .any(|r| r == "filename_target_mismatch"),
            "{:?}",
            mismatch.reasons
        );

        // Absolute (anime, no scene numbering): a unique absolute number matches.
        let accepted = evaluate_fresh_release(
            &c,
            &MediaTarget::Episode(201),
            "Aurora - 023 1080p WEB-DL.mkv",
            1073741824,
        )
        .await
        .unwrap()
        .accepted
        .unwrap();
        assert_eq!(accepted.quality_id, 3);

        // Absolute: a parsed number that matches no episode's absolute number is a
        // plain mismatch, not an ambiguous or missing-data condition.
        let mismatch = evaluate_fresh_release(
            &c,
            &MediaTarget::Episode(201),
            "Aurora - 999 1080p WEB-DL.mkv",
            1073741824,
        )
        .await
        .unwrap();
        assert!(mismatch.accepted.is_none());
        assert!(
            mismatch
                .reasons
                .iter()
                .any(|r| r == "filename_target_mismatch"),
            "{:?}",
            mismatch.reasons
        );

        // Absolute: two episodes share the same absolute number, so picking
        // either one for that number is ambiguous.
        let ambiguous = evaluate_fresh_release(
            &c,
            &MediaTarget::Episode(202),
            "Aurora - 024 1080p WEB-DL.mkv",
            1073741824,
        )
        .await
        .unwrap();
        assert!(ambiguous.accepted.is_none());
        assert!(
            ambiguous
                .reasons
                .iter()
                .any(|r| r == "ambiguous_absolute_match"),
            "{:?}",
            ambiguous.reasons
        );

        // Absolute: the target episode has no absolute number recorded.
        let unknown = evaluate_fresh_release(
            &c,
            &MediaTarget::Episode(204),
            "Aurora - 025 1080p WEB-DL.mkv",
            1073741824,
        )
        .await
        .unwrap();
        assert!(unknown.accepted.is_none());
        assert!(
            unknown
                .reasons
                .iter()
                .any(|r| r == "episode_absolute_number_unknown"),
            "{:?}",
            unknown.reasons
        );

        // Absolute (anime, scene numbering enabled): matches against
        // scene_absolute_episode_number, not the plain absolute_episode_number
        // (999) recorded on the same row.
        let accepted = evaluate_fresh_release(
            &c,
            &MediaTarget::Episode(301),
            "Borealis - 050 1080p WEB-DL.mkv",
            1073741824,
        )
        .await
        .unwrap()
        .accepted
        .unwrap();
        assert_eq!(accepted.quality_id, 3);

        // Absolute (anime, scene numbering enabled, but this episode has no
        // scene mapping): the scene column has zero hits for 51, so matching
        // falls back to the plain absolute_episode_number column, mirroring
        // Sonarr's GetAnimeEpisodes falling back to FindEpisode(series,
        // absoluteEpisodeNumber) when the scene lookup finds nothing.
        let accepted = evaluate_fresh_release(
            &c,
            &MediaTarget::Episode(302),
            "Borealis - 051 1080p WEB-DL.mkv",
            1073741824,
        )
        .await
        .unwrap()
        .accepted
        .unwrap();
        assert_eq!(accepted.quality_id, 3);

        // Absolute (anime, scene numbering enabled): two episodes (303, 304)
        // share scene_absolute_episode_number=60, so that scene-column match
        // is ambiguous and discarded (not rejected outright); matching retries
        // against absolute_episode_number=60, which only episode 303 has, so
        // the match still resolves.
        let accepted = evaluate_fresh_release(
            &c,
            &MediaTarget::Episode(303),
            "Borealis - 060 1080p WEB-DL.mkv",
            1073741824,
        )
        .await
        .unwrap()
        .accepted
        .unwrap();
        assert_eq!(accepted.quality_id, 3);
        // Series type controls query construction, but source admission only excludes daily
        // numbering on Standard. Daily-on-anime and ordinary absolute on other known types
        // must reach the same identity/quality/size checks rather than fail at a type gate.
        for kind in ["daily", "anime", "standard"] {
            c.execute(
                "UPDATE library_settings SET series_type=? WHERE series_id=10",
                [kind],
            )
            .await
            .unwrap();
            let result = evaluate_fresh_release(
                &c,
                &MediaTarget::Episode(101),
                "Nightly.2024.05.14.1080p.WEB-DL.mkv",
                1073741824,
            )
            .await
            .unwrap();
            assert_eq!(
                result.accepted.is_some(),
                kind != "standard",
                "{kind}: {:?}",
                result.reasons
            );
            assert!(
                evaluate_fresh_release(
                    &c,
                    &MediaTarget::Episode(105),
                    "Nightly.2024.05.14.1080p.WEB-DL.mkv",
                    1073741824
                )
                .await
                .unwrap()
                .accepted
                .is_none()
            );
            assert!(
                evaluate_fresh_release(
                    &c,
                    &MediaTarget::Episode(102),
                    "Nightly.2024.05.15.1080p.WEB-DL.mkv",
                    1073741824
                )
                .await
                .unwrap()
                .accepted
                .is_none()
            );
            c.execute(
                "UPDATE library_settings SET series_type=? WHERE series_id=20",
                [kind],
            )
            .await
            .unwrap();
            assert!(
                evaluate_fresh_release(
                    &c,
                    &MediaTarget::Episode(201),
                    "Aurora - 023 1080p WEB-DL.mkv",
                    1073741824
                )
                .await
                .unwrap()
                .accepted
                .is_some()
            );
            let ambiguous = evaluate_fresh_release(
                &c,
                &MediaTarget::Episode(202),
                "Aurora - 024 1080p WEB-DL.mkv",
                1073741824,
            )
            .await
            .unwrap();
            assert_eq!(ambiguous.reasons, vec!["ambiguous_absolute_match"]);
        }
        c.execute(
            "UPDATE episodes SET absolute_episode_number=NULL WHERE id=201",
            (),
        )
        .await
        .unwrap();
        let changed = evaluate_fresh_release(
            &c,
            &MediaTarget::Episode(201),
            "Aurora - 023 1080p WEB-DL.mkv",
            1073741824,
        )
        .await
        .unwrap();
        assert_eq!(changed.reasons, vec!["episode_absolute_number_unknown"]);
    }

    #[tokio::test]
    async fn scene_numbered_series_reject_standard_style_filenames_regardless_of_series_type() {
        let scratch = Scratch(
            std::env::temp_dir().join(format!("hrrdarr-downloaded-scene-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir(&scratch.0).unwrap();
        let db = crate::db::Database::open_local(scratch.0.join("db"))
            .await
            .unwrap();
        let c = db.connect().await.unwrap();
        c.execute_batch(
            "INSERT INTO series(id,title,path)VALUES(1,'Harbor','/fictional/tv');\
             INSERT INTO seasons(series_id,number)VALUES(1,1);\
             INSERT INTO episodes(id,series_id,season,number,title,runtime)VALUES(1,1,1,1,'Pilot',45);\
             INSERT INTO library_settings(media_type,series_id,series_type,use_scene_numbering)VALUES('tv',1,'standard',1);\
             INSERT INTO series(id,title,path)VALUES(2,'Comet','/fictional/tv2');\
             INSERT INTO seasons(series_id,number)VALUES(2,1);\
             INSERT INTO episodes(id,series_id,season,number,title,runtime,absolute_episode_number)VALUES(2,2,1,1,'Premiere',45,1);\
             INSERT INTO library_settings(media_type,series_id,series_type,use_scene_numbering)VALUES('tv',2,'anime',1);",
        )
        .await
        .unwrap();
        for (target, name) in [
            (MediaTarget::Episode(1), "Harbor.S01E01.1080p.WEB-DL.mkv"),
            // A scene-numbered anime series given a standard-style (SxxExx)
            // filename hits the same unimplemented Episodes arm -- this is
            // not gated by series_type, only by use_scene_numbering.
            (MediaTarget::Episode(2), "Comet.S01E01.1080p.WEB-DL.mkv"),
        ] {
            let result = evaluate_fresh_release(&c, &target, name, 1073741824)
                .await
                .unwrap();
            assert!(result.accepted.is_none(), "{name}");
            assert!(
                result.reasons.iter().any(|r| r == "numbering_unsupported"),
                "{name}: {:?}",
                result.reasons
            );
        }
    }

    #[tokio::test]
    async fn unconfigured_series_type_is_reported_distinctly() {
        let scratch = Scratch(std::env::temp_dir().join(format!(
            "hrrdarr-downloaded-unconfigured-{}",
            uuid::Uuid::new_v4()
        )));
        std::fs::create_dir(&scratch.0).unwrap();
        let db = crate::db::Database::open_local(scratch.0.join("db"))
            .await
            .unwrap();
        let c = db.connect().await.unwrap();
        c.execute_batch(
            "INSERT INTO series(id,title,path)VALUES(1,'Harbor','/fictional/tv');\
             INSERT INTO seasons(series_id,number)VALUES(1,1);\
             INSERT INTO episodes(id,series_id,season,number,title,runtime)VALUES(1,1,1,1,'Pilot',45);",
        )
        .await
        .unwrap();
        let result = evaluate_fresh_release(
            &c,
            &MediaTarget::Episode(1),
            "Harbor.S01E01.1080p.WEB-DL.mkv",
            1073741824,
        )
        .await
        .unwrap();
        assert!(result.accepted.is_none());
        assert!(
            result
                .reasons
                .iter()
                .any(|r| r == "series_type_unconfigured"),
            "{:?}",
            result.reasons
        );
    }
}

pub(crate) async fn original_language(
    c: &Connection,
    target: &MediaTarget,
) -> super::Result<Option<i64>> {
    let sql = match target {
        MediaTarget::Episode(_) => {
            "SELECT s.original_language FROM series s JOIN episodes e ON e.series_id=s.id WHERE e.id=?"
        }
        MediaTarget::Movie(_) => {
            "SELECT d.original_language FROM movie_metadata d JOIN movies m ON m.metadata_id=d.id WHERE m.id=?"
        }
    };
    let id = match target {
        MediaTarget::Episode(id) | MediaTarget::Movie(id) => *id,
    };
    Ok(c.query(sql, [id])
        .await?
        .next()
        .await?
        .map(|r| r.get(0))
        .transpose()?
        .flatten())
}

pub(crate) async fn evaluate_receipt(
    c: &Connection,
    target: &MediaTarget,
    filename: &str,
    size: u64,
    candidate: &str,
) -> super::Result<DownloadedDecision> {
    let row = c
        .query(
            "SELECT comparison_facts_json FROM rss_candidates WHERE id=?",
            [candidate],
        )
        .await?
        .next()
        .await?
        .ok_or(SearchError("release_target_missing"))?;
    let evidence = row
        .get::<Option<String>>(0)?
        .map(|s| serde_json::from_str(&s).map_err(|_| SearchError("custom_format_facts_invalid")))
        .transpose()?;
    let id = uuid::Uuid::parse_str(candidate).map_err(|_| SearchError("release_target_missing"))?;
    let context = crate::commands::search::authority(c, id)
        .await
        .map_err(|_| SearchError("release_authority_changed"))?;
    evaluate_bound(c, target, filename, size, evidence, None, context).await
}

// Explicit downloaded-scan decisions. These values authorize no filesystem effects.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
const SCAN_FACT_BYTES: usize = 64 * 1024 * 1024;
const SCAN_PROOF_BYTES: usize = 16 * 1024;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "media_type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ScanOwner {
    Tv { series_id: i64 },
    Movies { movie_id: i64 },
}
impl ScanOwner {
    fn media(self) -> MediaDomain {
        match self {
            Self::Tv { .. } => MediaDomain::Tv,
            Self::Movies { .. } => MediaDomain::Movies,
        }
    }
    fn id(self) -> i64 {
        match self {
            Self::Tv { series_id } => series_id,
            Self::Movies { movie_id } => movie_id,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ScanAuthority {
    Local {
        owner: ScanOwner,
    },
    Receipt {
        candidate_id: uuid::Uuid,
        provider_id: uuid::Uuid,
        provider_revision: i64,
        media_type: MediaDomain,
        remote_id: String,
    },
}
#[derive(Clone, Copy)]
pub(crate) struct ScanFileInput<'a> {
    pub basename: &'a str,
    pub relative_path: &'a str,
    pub size: u64,
    /// Exact serialized descriptor identity captured by the scanner/importer. This
    /// evaluator checks structure/equality; the caller must actually verify the FD.
    pub source_identity_json: &'a str,
    pub folder_name: Option<&'a str>,
    /// Actual current client name, bound by caller to the matching provider/remote
    /// observation. This is evidence only and can never supply receipt authority.
    pub download_name: Option<&'a str>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ScanPrerequisite {
    QualityObservation,
    MediaSample,
    AudioObservation,
    SiblingMultipart,
    UnpackingState,
    DestinationSpace,
    NamingPolicy,
    ImportHistoryCoverage,
}
#[derive(Debug)]
pub(crate) enum ScanFileDecision {
    Rejected { reasons: Vec<String> },
    NeedsObservation { requirements: Vec<ScanPrerequisite> },
    PolicyEligible(ScanPolicyEligible),
}
#[derive(Debug)]
pub(crate) struct ScanPolicyEligible {
    pub proof: ScanDecisionProof,
    pub target: ReleaseTarget,
    pub members: Vec<ScanMember>,
    pub root: String,
    pub parsed: parser::ParsedFile,
    pub evidence: crate::custom_formats::Evidence,
    pub revision: super::revision::ScanRevisionEvidence,
    pub quality: decision::ImportQualityAssessment,
    pub import_policy: crate::import_policy::Policy,
    pub runtime_minutes: Option<i64>,
    pub preference: ScanPreference,
    /// Eligibility is ONLY identity/quality policy. Consumers must resolve each
    /// prerequisite using measured scanner facts before creating an import journal.
    pub requirements: Vec<ScanPrerequisite>,
}
impl ScanPolicyEligible {
    /// New downloaded files only. An existing-library rescan has different sample exemptions.
    /// This resolves media policy, not the remaining prerequisites or filesystem authority.
    pub fn media_rejections(
        &self,
        observation: &crate::import::probe::Observation,
    ) -> Vec<&'static str> {
        use crate::import::probe::Observation;
        let tv = matches!(self.target, ReleaseTarget::Tv { .. });
        let extension = self
            .proof
            .basename
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase());
        let sample_exempt = matches!(extension.as_deref(), Some("flv" | "strm"))
            || (!tv && matches!(extension.as_deref(), Some("iso" | "img" | "m2ts")))
            || (tv && self.members.first().is_some_and(|m| m.season == 0));
        let mut reasons = Vec::new();
        if !sample_exempt {
            let duration = match observation {
                Observation::Measured(media) => media.runtime_ticks,
                Observation::Unavailable(_) => None,
            };
            const TICKS_PER_SECOND: i64 = 10_000_000;
            let minimum_seconds = match self.runtime_minutes.unwrap_or(0) {
                ..=3 => 15,
                4..=10 => 90,
                11..=30 => 300,
                _ => 600,
            };
            match duration {
                None => reasons.push("sample_indeterminate"),
                Some(ticks) if ticks < minimum_seconds * TICKS_PER_SECOND => {
                    reasons.push("sample_file")
                }
                Some(_) => (),
            }
        }
        if matches!(observation, Observation::Measured(media) if media.audio_stream_count == 0) {
            reasons.push("no_audio_tracks");
        }
        reasons
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ScanMember {
    pub episode_id: i64,
    pub season: i64,
    pub number: i64,
    pub title: String,
    pub runtime: Option<i64>,
    pub old_file_id: Option<i64>,
    pub air_date: Option<String>,
    pub absolute: Option<i64>,
    pub scene_season: Option<i64>,
    pub scene_episode: Option<i64>,
    pub scene_absolute: Option<i64>,
    pub unverified_scene: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ScanDecisionProof {
    pub version: u8,
    pub owner: ScanOwner,
    pub basename: String,
    pub relative_path: String,
    pub size: u64,
    pub source_identity_json: String,
    pub folder_name: Option<String>,
    pub download_name: Option<String>,
    pub authority_fingerprint: String,
    pub owner_fingerprint: String,
    pub membership_fingerprint: String,
    pub parsed_fingerprint: String,
    pub policy_fingerprint: String,
    pub evidence_fingerprint: String,
    pub revision: super::revision::ScanRevisionEvidence,
}
/// Higher policy/size first; normalized and then raw path break ties ascending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScanPreference {
    pub policy: (usize, i64, i64, i64),
    pub size: u64,
    pub normalized_path: String,
    pub relative_path: String,
}
impl Ord for ScanPreference {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.policy
            .cmp(&other.policy)
            .then(self.size.cmp(&other.size))
            .then_with(|| other.normalized_path.cmp(&self.normalized_path))
            .then_with(|| other.relative_path.cmp(&self.relative_path))
    }
}
impl PartialOrd for ScanPreference {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
fn scan_rejected(code: &str) -> ScanFileDecision {
    ScanFileDecision::Rejected {
        reasons: vec![code.into()],
    }
}
fn scan_text(row: &libsql::Row, index: i32, limit: usize) -> super::Result<Option<String>> {
    row.get::<Option<Vec<u8>>>(index)?
        .map(|bytes| {
            if bytes.len() > limit {
                return Err(SearchError("scan_facts_limit"));
            }
            let text = String::from_utf8(bytes).map_err(|_| SearchError("scan_facts_invalid"))?;
            if text.chars().any(char::is_control) {
                return Err(SearchError("scan_facts_invalid"));
            }
            Ok(text)
        })
        .transpose()
}
fn scan_json_text(row: &libsql::Row, index: i32, limit: usize) -> super::Result<Option<String>> {
    row.get::<Option<Vec<u8>>>(index)?
        .map(|bytes| {
            if bytes.len() > limit {
                return Err(SearchError("scan_facts_limit"));
            }
            String::from_utf8(bytes).map_err(|_| SearchError("scan_facts_invalid"))
        })
        .transpose()
}
fn scan_id(value: i64) -> super::Result<i64> {
    if (1..=9_007_199_254_740_991).contains(&value) {
        Ok(value)
    } else {
        Err(SearchError("invalid_stored_target"))
    }
}
fn source_identity(raw: &str, size: u64) -> super::Result<String> {
    if raw.len() > 4096 {
        return Err(SearchError("source_identity_invalid"));
    }
    let value: serde_json::Value =
        serde_json::from_str(raw).map_err(|_| SearchError("source_identity_invalid"))?;
    let object = value
        .as_object()
        .ok_or(SearchError("source_identity_invalid"))?;
    if object.len() != 8
        || object.get("version").and_then(|v| v.as_u64()) != Some(1)
        || !["dev", "ino", "size"]
            .iter()
            .all(|k| object.get(*k).and_then(|v| v.as_u64()).is_some())
        || !["mtime", "ctime"]
            .iter()
            .all(|k| object.get(*k).and_then(|v| v.as_i64()).is_some())
        || !["mtime_ns", "ctime_ns"].iter().all(|k| {
            object
                .get(*k)
                .and_then(|v| v.as_i64())
                .is_some_and(|v| (0..1_000_000_000).contains(&v))
        })
        || object.get("size").and_then(|v| v.as_u64()) != Some(size)
    {
        return Err(SearchError("source_identity_invalid"));
    }
    serde_json::to_string(&value).map_err(|_| SearchError("source_identity_invalid"))
}
#[derive(Debug, Serialize)]
struct ScanOwnerFacts {
    owner: ScanOwner,
    title: String,
    year: Option<i64>,
    secondary_year: Option<i64>,
    root: String,
    profile: Option<i64>,
    series_type: Option<String>,
    scene: bool,
    original_language: Option<i64>,
    runtime: Option<i64>,
    old_file: Option<i64>,
    alternatives: Vec<String>,
}
async fn scan_owner_facts(c: &Connection, owner: ScanOwner) -> super::Result<ScanOwnerFacts> {
    scan_id(owner.id())?;
    let mut facts = match owner {
        ScanOwner::Tv { series_id } => {
            let r=c.query("SELECT substr(CAST(s.title AS BLOB),1,4097),substr(CAST(s.path AS BLOB),1,4097),l.quality_profile_id,l.series_type,l.use_scene_numbering,s.original_language,s.runtime FROM series s LEFT JOIN library_settings l ON l.series_id=s.id WHERE s.id=?",[series_id]).await?.next().await?.ok_or(SearchError("target_changed"))?;
            ScanOwnerFacts {
                owner,
                title: scan_text(&r, 0, 4096)?.ok_or(SearchError("target_changed"))?,
                root: scan_text(&r, 1, 4096)?.ok_or(SearchError("target_changed"))?,
                profile: r.get(2)?,
                series_type: r.get(3)?,
                scene: r.get::<Option<i64>>(4)? == Some(1),
                original_language: r.get(5)?,
                year: None,
                secondary_year: None,
                runtime: r.get(6)?,
                old_file: None,
                alternatives: Vec::new(),
            }
        }
        ScanOwner::Movies { movie_id } => {
            let r=c.query("SELECT d.id,substr(CAST(d.title AS BLOB),1,4097),d.year,d.secondary_year,substr(CAST(m.path AS BLOB),1,4097),l.quality_profile_id,d.original_language,d.runtime,f.id FROM movies m JOIN movie_metadata d ON d.id=m.metadata_id LEFT JOIN library_settings l ON l.movie_id=m.id LEFT JOIN movie_files f ON f.movie_id=m.id WHERE m.id=?",[movie_id]).await?.next().await?.ok_or(SearchError("target_changed"))?;
            let mut alternatives = Vec::new();
            let mut rows=c.query("SELECT substr(CAST(title AS BLOB),1,4097) FROM movie_alternative_titles WHERE metadata_id=? ORDER BY title LIMIT 1001",[r.get::<i64>(0)?]).await?;
            while let Some(row) = rows.next().await? {
                if alternatives.len() == 1000 {
                    return Err(SearchError("library_match_limit"));
                }
                alternatives
                    .push(scan_text(&row, 0, 4096)?.ok_or(SearchError("scan_facts_invalid"))?);
            }
            ScanOwnerFacts {
                owner,
                title: scan_text(&r, 1, 4096)?.ok_or(SearchError("target_changed"))?,
                year: r.get(2)?,
                secondary_year: r.get(3)?,
                root: scan_text(&r, 4, 4096)?.ok_or(SearchError("target_changed"))?,
                profile: r.get(5)?,
                original_language: r.get(6)?,
                runtime: r.get(7)?,
                old_file: r.get(8)?,
                series_type: None,
                scene: false,
                alternatives,
            }
        }
    };
    facts.alternatives.sort();
    if !std::path::Path::new(&facts.root).is_absolute() {
        return Err(SearchError("owner_root_invalid"));
    }
    Ok(facts)
}
fn identity_agrees(facts: &ScanOwnerFacts, title: &str, year: Option<i64>) -> bool {
    let title = parser::normalize(title);
    !title.is_empty()
        && (parser::normalize(&facts.title) == title
            || facts
                .alternatives
                .iter()
                .any(|s| parser::normalize(s) == title))
        && (facts.owner.media() == MediaDomain::Tv
            || year.is_none_or(|y| Some(y) == facts.year || Some(y) == facts.secondary_year))
}
fn folder_identity(raw: &str) -> &str {
    // Identification only; the raw folder remains in proof and UnpackingState must
    // independently reject a still-active working folder before effects.
    raw.strip_prefix("_UNPACK_")
        .or_else(|| raw.strip_prefix("_FAILED_"))
        .unwrap_or(raw)
}
/// Resolve one unique existing owner from actual filename/folder identity. SQL and
/// accumulated title bytes are bounded; same-title collisions are never first-row wins.
pub(crate) async fn resolve_scan_owner(
    c: &Connection,
    media: MediaDomain,
    raw: &str,
) -> super::Result<ScanOwner> {
    if raw.is_empty() || raw.len() > 1024 || raw.chars().any(char::is_control) {
        return Err(SearchError("invalid_owner_identification"));
    }
    let raw = folder_identity(raw);
    let parsed = parser::parse_downloaded(raw, media == MediaDomain::Tv).ok();
    let title = parsed.as_ref().map_or(raw, |p| p.parsed.title.as_str());
    let year = parsed.as_ref().and_then(|p| p.parsed.year);
    let wanted = parser::normalize(title);
    if wanted.is_empty() {
        return Err(SearchError("missing_owner_identification"));
    }
    let sql = if media == MediaDomain::Tv {
        "SELECT id,substr(CAST(title AS BLOB),1,4097),NULL,NULL FROM series ORDER BY id LIMIT 20001"
    } else {
        "SELECT m.id,substr(CAST(d.title AS BLOB),1,4097),d.year,d.secondary_year FROM movies m JOIN movie_metadata d ON d.id=m.metadata_id UNION ALL SELECT m.id,substr(CAST(a.title AS BLOB),1,4097),d.year,d.secondary_year FROM movies m JOIN movie_metadata d ON d.id=m.metadata_id JOIN movie_alternative_titles a ON a.metadata_id=d.id LIMIT 20001"
    };
    let mut rows = c.query(sql, ()).await?;
    let mut count = 0;
    let mut bytes = 0usize;
    let mut matches = BTreeSet::new();
    while let Some(r) = rows.next().await? {
        count += 1;
        if count > 20_000 {
            return Err(SearchError("library_match_limit"));
        }
        let title = scan_text(&r, 1, 4096)?.ok_or(SearchError("scan_facts_invalid"))?;
        bytes += title.len();
        if bytes > SCAN_FACT_BYTES {
            return Err(SearchError("scan_facts_limit"));
        }
        let primary_year = r.get::<Option<i64>>(2)?;
        let secondary_year = r.get::<Option<i64>>(3)?;
        if parser::normalize(&title) == wanted
            && (media == MediaDomain::Tv
                || year.is_none_or(|y| primary_year == Some(y) || secondary_year == Some(y)))
        {
            matches.insert(scan_id(r.get(0)?)?);
        }
    }
    if matches.len() != 1 {
        return Err(SearchError(if matches.is_empty() {
            "no_library_match"
        } else {
            "ambiguous_library_match"
        }));
    }
    let id = *matches.first().unwrap();
    Ok(if media == MediaDomain::Tv {
        ScanOwner::Tv { series_id: id }
    } else {
        ScanOwner::Movies { movie_id: id }
    })
}
struct ScanReceipt {
    owner: ScanOwner,
    title: String,
    evidence: Option<crate::custom_formats::Evidence>,
    episodes: BTreeSet<i64>,
    fingerprint: String,
}
async fn scan_receipt(c: &Connection, authority: &ScanAuthority) -> super::Result<ScanReceipt> {
    let ScanAuthority::Receipt {
        candidate_id,
        provider_id,
        provider_revision,
        media_type,
        remote_id,
    } = authority
    else {
        return Err(SearchError("invalid_receipt_authority"));
    };
    if *provider_revision <= 0
        || !matches!(remote_id.len(), 40 | 64)
        || !remote_id
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return Err(SearchError("invalid_receipt_authority"));
    }
    let r=c.query("SELECT r.media_type,r.series_id,r.movie_id,r.client_id,r.client_revision,r.observed_hash,r.status,substr(CAST(r.title AS BLOB),1,1025),substr(CAST(r.comparison_facts_json AS BLOB),1,16385),substr(CAST(r.submission_identity_json AS BLOB),1,4097),r.created_at,r.updated_at,r.search_result_id,p.revision,p.enabled,p.implementation FROM rss_candidates r JOIN providers p ON p.id=r.client_id JOIN provider_scopes s ON s.provider_id=p.id AND s.media_type=r.media_type WHERE r.id=?",[candidate_id.to_string()]).await?.next().await?.ok_or(SearchError("release_authority_changed"))?;
    if r.get::<String>(0)? != super::domain(*media_type)
        || r.get::<String>(3)? != provider_id.to_string()
        || r.get::<i64>(4)? != *provider_revision
        || r.get::<String>(5)? != *remote_id
        || r.get::<String>(6)? != "observed"
        || r.get::<i64>(13)? != *provider_revision
        || r.get::<i64>(14)? != 1
        || r.get::<String>(15)? != "qbittorrent"
    {
        return Err(SearchError("release_authority_changed"));
    }
    let owner = match media_type {
        MediaDomain::Tv => ScanOwner::Tv {
            series_id: scan_id(r.get(1)?)?,
        },
        MediaDomain::Movies => ScanOwner::Movies {
            movie_id: scan_id(r.get(2)?)?,
        },
    };
    let title = scan_text(&r, 7, 1024)?.ok_or(SearchError("release_authority_changed"))?;
    let evidence_raw = scan_json_text(&r, 8, 16384)?;
    let evidence: Option<crate::custom_formats::Evidence> = evidence_raw
        .as_deref()
        .map(|s| serde_json::from_str(s).map_err(|_| SearchError("custom_format_facts_invalid")))
        .transpose()?;
    if let Some(evidence) = &evidence {
        crate::custom_formats::validate_evidence(*media_type, evidence).map_err(SearchError)?;
    }
    let identity_raw =
        scan_json_text(&r, 9, 4096)?.ok_or(SearchError("release_authority_changed"))?;
    let identity: crate::providers::qbittorrent::SubmissionIdentity =
        serde_json::from_str(&identity_raw)
            .map_err(|_| SearchError("release_authority_changed"))?;
    let identity_value: serde_json::Value = serde_json::from_str(&identity_raw)
        .map_err(|_| SearchError("release_authority_changed"))?;
    if identity_value.get("version").and_then(|v| v.as_u64()) != Some(1)
        || !identity.hashes().iter().any(|hash| hash == remote_id)
        || identity.hashes().iter().any(|hash| {
            !matches!(hash.len(), 40 | 64)
                || !hash
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        })
        || identity.hashes().is_empty()
        || identity.hashes().len() > 2
        || ["settings_fingerprint", "payload_sha256"]
            .iter()
            .any(|key| {
                !identity_value
                    .get(*key)
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| {
                        s.len() == 64
                            && s.bytes()
                                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                    })
            })
    {
        return Err(SearchError("release_authority_changed"));
    }
    let mut episodes = BTreeSet::new();
    if let ScanOwner::Tv { series_id } = owner {
        let mut rows=c.query("SELECT e.episode_id,e.series_id FROM rss_candidate_episodes e WHERE e.candidate_id=? ORDER BY e.episode_id LIMIT 20001",[candidate_id.to_string()]).await?;
        while let Some(row) = rows.next().await? {
            if episodes.len() == parser::MAX_DOWNLOADED_MEMBERS || row.get::<i64>(1)? != series_id {
                return Err(SearchError("release_target_invalid"));
            }
            episodes.insert(scan_id(row.get(0)?)?);
        }
        if episodes.is_empty() {
            return Err(SearchError("release_target_missing"));
        }
    }
    if !match (owner, identity.target()) {
        (ScanOwner::Tv { .. }, MediaTarget::Episode(id)) => episodes.contains(id),
        (ScanOwner::Movies { movie_id }, MediaTarget::Movie(id)) => movie_id == *id,
        _ => false,
    } {
        return Err(SearchError("release_target_invalid"));
    }
    if c.query(
        "SELECT candidate_id FROM rss_hash_claims WHERE client_id=? AND hash=?",
        params![provider_id.to_string(), remote_id.clone()],
    )
    .await?
    .next()
    .await?
    .map(|r| r.get::<String>(0))
    .transpose()?
    .as_deref()
        != Some(candidate_id.to_string().as_str())
    {
        return Err(SearchError("release_authority_changed"));
    }
    let origin = crate::commands::search::authority(c, *candidate_id)
        .await
        .map_err(|_| SearchError("release_authority_changed"))?;
    let origin = match origin {
        super::SearchContext::Rss => "rss",
        super::SearchContext::UserSearch => "user_search",
    };
    let fingerprint = decision::facts_digest(&(
        authority,
        &title,
        &evidence,
        &identity_value,
        &episodes,
        r.get::<i64>(10)?,
        r.get::<i64>(11)?,
        r.get::<Option<String>>(12)?,
        origin,
    ))?;
    Ok(ScanReceipt {
        owner,
        title,
        evidence,
        episodes,
        fingerprint,
    })
}

async fn scan_catalog(c: &Connection, series_id: i64) -> super::Result<Vec<ScanMember>> {
    let mut rows=c.query("SELECT id,season,number,substr(CAST(title AS BLOB),1,4097),runtime,episode_file_id,substr(CAST(air_date AS BLOB),1,65),absolute_episode_number,scene_season_number,scene_episode_number,scene_absolute_episode_number,unverified_scene_numbering FROM episodes WHERE series_id=? ORDER BY id LIMIT 20001",[series_id]).await?;
    let mut catalog = Vec::new();
    let mut bytes = 0usize;
    while let Some(row) = rows.next().await? {
        if catalog.len() == parser::MAX_DOWNLOADED_MEMBERS {
            return Err(SearchError("episode_match_limit"));
        }
        let member = ScanMember {
            episode_id: scan_id(row.get(0)?)?,
            season: row.get(1)?,
            number: row.get(2)?,
            title: scan_text(&row, 3, 4096)?.unwrap_or_default(),
            runtime: row.get(4)?,
            old_file_id: row.get(5)?,
            air_date: scan_text(&row, 6, 64)?,
            absolute: row.get(7)?,
            scene_season: row.get(8)?,
            scene_episode: row.get(9)?,
            scene_absolute: row.get(10)?,
            unverified_scene: row.get::<Option<i64>>(11)? == Some(1),
        };
        bytes = bytes.saturating_add(
            member.title.len() + member.air_date.as_ref().map_or(0, String::len) + 128,
        );
        if bytes > SCAN_FACT_BYTES {
            return Err(SearchError("scan_facts_limit"));
        }
        catalog.push(member);
    }
    Ok(catalog)
}
/// Index once for the complete file: scene mappings may map one scene coordinate
/// onto multiple ordinary episodes, but no missing member is silently dropped.
fn scan_members(
    catalog: &[ScanMember],
    numbering: &parser::DownloadedNumbering,
    scene: bool,
    series_type: &str,
    folder: bool,
) -> super::Result<Vec<ScanMember>> {
    let mut ordinary: BTreeMap<(i64, i64), Vec<usize>> = BTreeMap::new();
    let mut scene_numbers: BTreeMap<(i64, i64), Vec<usize>> = BTreeMap::new();
    let mut absolute: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
    let mut scene_absolute: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
    let mut dates: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, m) in catalog.iter().enumerate() {
        ordinary.entry((m.season, m.number)).or_default().push(i);
        if let (Some(s), Some(e)) = (m.scene_season, m.scene_episode) {
            scene_numbers.entry((s, e)).or_default().push(i);
        }
        if let Some(n) = m.absolute {
            absolute.entry(n).or_default().push(i);
        }
        if let Some(n) = m.scene_absolute {
            scene_absolute.entry(n).or_default().push(i);
        }
        if let Some(date) = m.air_date.as_deref() {
            dates.entry(date).or_default().push(i);
        }
    }
    let mut selected = BTreeSet::new();
    let mut add = |hits: &[usize], scene_mapping: bool, multiple: bool| -> super::Result<()> {
        if hits.is_empty() {
            return Err(SearchError("episode_match_missing"));
        }
        if !multiple && hits.len() != 1 {
            return Err(SearchError("episode_match_ambiguous"));
        }
        for i in hits {
            if scene_mapping && catalog[*i].unverified_scene {
                return Err(SearchError("scene_mapping_unverified"));
            }
            selected.insert(*i);
        }
        Ok(())
    };
    use parser::DownloadedNumbering as N;
    match numbering {
        N::Episodes { season, episodes } => {
            for number in episodes {
                if let Some(hits) = scene
                    .then(|| scene_numbers.get(&(*season, *number)))
                    .flatten()
                {
                    add(hits, true, true)?;
                } else {
                    add(
                        ordinary.get(&(*season, *number)).map_or(&[], Vec::as_slice),
                        false,
                        false,
                    )?;
                }
            }
        }
        N::Absolute { episodes } => {
            for number in episodes {
                if let Some(hits) = scene
                    .then(|| scene_absolute.get(number))
                    .flatten()
                    .filter(|hits| hits.len() == 1)
                {
                    add(hits, true, false)?;
                } else {
                    add(
                        absolute.get(number).map_or(&[], Vec::as_slice),
                        false,
                        false,
                    )?;
                }
            }
        }
        N::Daily { dates: wanted } => {
            if series_type == "standard" {
                return Err(SearchError("numbering_unsupported"));
            }
            for date in wanted {
                let hits = dates
                    .get(date.as_str())
                    .ok_or(SearchError("episode_match_missing"))?;
                let regular: Vec<_> = hits
                    .iter()
                    .copied()
                    .filter(|i| catalog[*i].season != 0)
                    .collect();
                add(if hits.len() > 1 { &regular } else { hits }, false, false)?;
            }
        }
        N::Season { season } if folder => {
            let hits: Vec<_> = catalog
                .iter()
                .enumerate()
                .filter(|(_, m)| m.season == *season)
                .map(|(i, _)| i)
                .collect();
            add(&hits, false, true)?;
        }
        N::Season { .. } => return Err(SearchError("file_numbering_incomplete")),
    }
    if selected.is_empty() {
        return Err(SearchError("episode_match_missing"));
    }
    let result: Vec<_> = selected.into_iter().map(|i| catalog[i].clone()).collect();
    if result.iter().any(|m| m.season != result[0].season) {
        return Err(SearchError("cross_season_file"));
    }
    Ok(result)
}
fn scan_runtime(members: &[ScanMember], series_runtime: Option<i64>) -> super::Result<Option<i64>> {
    let mut total = 0i64;
    for m in members {
        let runtime = m.runtime.filter(|v| *v > 0).or(series_runtime).unwrap_or(0);
        if runtime < 0 {
            return Err(SearchError("runtime_invalid"));
        }
        total = total
            .checked_add(runtime)
            .ok_or(SearchError("runtime_overflow"))?;
    }
    // This is expected-runtime policy, not an observed metadata value; fallback is once per file.
    Ok(Some(if total == 0 { 45 } else { total }))
}
fn scan_extension_supported(extension: &str, tv: bool) -> bool {
    match extension.to_ascii_lowercase().as_str() {
        "webm" | "m4v" | "3gp" | "nsv" | "ty" | "strm" | "rm" | "rmvb" | "m3u" | "ifo" | "mov"
        | "qt" | "divx" | "xvid" | "bivx" | "nrg" | "pva" | "wmv" | "asf" | "asx" | "ogm"
        | "ogv" | "m2v" | "avi" | "bin" | "dat" | "dvr-ms" | "mpg" | "mpeg" | "mp4" | "avc"
        | "vp3" | "svq3" | "nuv" | "viv" | "dv" | "fli" | "flv" | "wpl" | "img" | "iso" | "vob"
        | "mkv" | "ts" | "wtv" | "m2ts" => true,
        "mk3d" => !tv,
        _ => false,
    }
}
fn scan_path_excluded(file: ScanFileInput<'_>, tv: bool) -> bool {
    let basename = file.basename.to_ascii_lowercase();
    if basename.starts_with("._")
        || basename.starts_with(".unmanic")
        || matches!(basename.as_str(), ".ds_store" | "thumbs.db")
    {
        return true;
    }
    if let Some((stem, _)) = basename.rsplit_once('.') {
        if let Some((_, suffix)) = stem.rsplit_once('-') {
            if matches!(
                suffix,
                "trailer"
                    | "other"
                    | "behindthescenes"
                    | "deleted"
                    | "featurette"
                    | "interview"
                    | "scene"
                    | "short"
            ) {
                return true;
            }
        }
    }
    let Some((parents, _)) = file.relative_path.rsplit_once('/') else {
        return false;
    };
    parents.split('/').any(|part| {
        let part = part.to_ascii_lowercase();
        part.starts_with('.')
            || matches!(
                part.as_str(),
                "@eadir"
                    | "plex versions"
                    | "extras"
                    | "extrafanart"
                    | "behind the scenes"
                    | "deleted scenes"
                    | "featurettes"
                    | "interviews"
                    | "other"
                    | "scenes"
                    | "samples"
                    | "shorts"
                    | "trailers"
            )
            || (!tv && part == "sample")
            || (tv
                && matches!(
                    part.as_str(),
                    "backdrops" | "theme-music" | "theme_music" | "theme.music" | "theme music"
                ))
    })
}
fn scan_file_valid(file: ScanFileInput<'_>) -> super::Result<String> {
    if file.basename.is_empty()
        || file.basename.len() > 255
        || file.basename.contains(['/', '\\'])
        || file.basename.chars().any(char::is_control)
        || file.relative_path.is_empty()
        || file.relative_path.len() > 4096
        || file.relative_path.contains('\\')
        || file.relative_path.chars().any(char::is_control)
        || file
            .relative_path
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
        || file.relative_path.rsplit('/').next() != Some(file.basename)
        || file.size == 0
        || file.size > 256 * 1024 * 1024 * 1024
    {
        return Err(SearchError("invalid_scan_file"));
    }
    if file.folder_name.is_some_and(|s| {
        s.is_empty() || s.len() > 1024 || s.contains(['/', '\\']) || s.chars().any(char::is_control)
    }) {
        return Err(SearchError("invalid_scan_folder"));
    }
    if file
        .download_name
        .is_some_and(|s| s.is_empty() || s.len() > 1024 || s.chars().any(char::is_control))
    {
        return Err(SearchError("invalid_download_name"));
    }
    source_identity(file.source_identity_json, file.size)
}
/// Opaque per-file evaluated facts avoid the process-wide cache's 2048-entry
/// limit. This is not authority: the transaction rereads and compares every fact.
pub(crate) struct ScanWarmFile {
    decision: ScanFileDecision,
    rejected_quality: Option<decision::ImportQualityAssessment>,
}
impl ScanWarmFile {
    pub fn decision(&self) -> &ScanFileDecision {
        &self.decision
    }
}
pub(crate) async fn warm_scan_file(
    c: &Connection,
    authority: &ScanAuthority,
    file: ScanFileInput<'_>,
) -> super::Result<ScanWarmFile> {
    if !c.is_autocommit() {
        return Err(SearchError("scan_warm_requires_autocommit"));
    }
    let mut rejected_quality = None;
    let decision = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        evaluate_scan_inner(c, authority, file, None, &mut rejected_quality),
    )
    .await
    .map_err(|_| SearchError("scan_decision_timeout"))??;
    Ok(ScanWarmFile {
        decision,
        rejected_quality,
    })
}
/// Caller owns a read/write snapshot and must independently reverify the captured
/// descriptor identity. Expected proof equality is required before journal/link CAS.
pub(crate) async fn evaluate_scan_file(
    c: &Connection,
    authority: &ScanAuthority,
    file: ScanFileInput<'_>,
    expected: Option<&ScanDecisionProof>,
    warmed: &ScanWarmFile,
) -> super::Result<ScanFileDecision> {
    if c.is_autocommit() {
        return Err(SearchError("scan_snapshot_required"));
    }
    if let Some(proof) = expected {
        if proof.version != 1
            || serde_json::to_vec(proof)
                .map_err(|_| SearchError("scan_proof_invalid"))?
                .len()
                > SCAN_PROOF_BYTES
        {
            return Err(SearchError("scan_proof_invalid"));
        }
    }
    let mut rejected_quality = None;
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        evaluate_scan_inner(
            c,
            authority,
            file,
            match &warmed.decision {
                ScanFileDecision::PolicyEligible(p) => Some(&p.quality),
                _ => warmed.rejected_quality.as_ref(),
            },
            &mut rejected_quality,
        ),
    )
    .await
    .map_err(|_| SearchError("scan_decision_timeout"))??;
    if let Some(expected) = expected {
        if !matches!(&result,ScanFileDecision::PolicyEligible(current) if &current.proof==expected)
        {
            return Err(SearchError("scan_preflight_changed"));
        }
    }
    Ok(result)
}
async fn evaluate_scan_inner(
    c: &Connection,
    authority: &ScanAuthority,
    file: ScanFileInput<'_>,
    warmed: Option<&decision::ImportQualityAssessment>,
    rejected_quality: &mut Option<decision::ImportQualityAssessment>,
) -> super::Result<ScanFileDecision> {
    let identity = scan_file_valid(file)?;
    // Validate known receipt before considering local parse prerequisites. A missing
    // or contradictory tracked authority can never turn into an untracked scan.
    let receipt = match authority {
        ScanAuthority::Receipt { .. } => Some(scan_receipt(c, authority).await?),
        ScanAuthority::Local { .. } => None,
    };
    let owner = receipt
        .as_ref()
        .map(|r| r.owner)
        .unwrap_or_else(|| match authority {
            ScanAuthority::Local { owner } => *owner,
            _ => unreachable!(),
        });
    if matches!(authority, ScanAuthority::Local { .. }) && file.download_name.is_some() {
        return Err(SearchError("download_name_requires_receipt"));
    }
    let facts = scan_owner_facts(c, owner).await?;
    let tv = owner.media() == MediaDomain::Tv;
    let Some((stem, extension)) = file.basename.rsplit_once('.') else {
        return Ok(scan_rejected("unsupported_file_type"));
    };
    if !scan_extension_supported(extension, tv) {
        return Ok(scan_rejected("unsupported_file_type"));
    }
    if scan_path_excluded(file, tv) {
        return Ok(scan_rejected("excluded_media_path"));
    }
    // "Sample" can be a title; measured runtime and excluded directory segments decide samples.
    let mut parsed = match parser::parse_downloaded(stem, tv) {
        Ok(p) => p,
        Err(code) => return Ok(scan_rejected(code)),
    };
    let folder = file
        .folder_name
        .map(folder_identity)
        .map(|s| parser::parse_downloaded(s, tv))
        .transpose()
        .map_err(SearchError)?;
    let download = file
        .download_name
        .map(|s| parser::parse_downloaded(s, tv))
        .transpose()
        .map_err(SearchError)?;
    let receipt_parsed = receipt
        .as_ref()
        .map(|r| parser::parse_downloaded(&r.title, tv))
        .transpose()
        .map_err(SearchError)?;
    let mut identified = false;
    for observed in std::iter::once(&parsed)
        .chain(folder.iter())
        .chain(download.iter())
        .chain(receipt_parsed.iter())
    {
        if observed.parsed.title.is_empty() {
            continue;
        }
        if !identity_agrees(&facts, &observed.parsed.title, observed.parsed.year) {
            return Ok(scan_rejected("filename_target_mismatch"));
        }
        identified = true;
    }
    if !identified {
        return Ok(scan_rejected("missing_owner_identification"));
    }
    if let ScanAuthority::Local { .. } = authority {
        // Explicit owner is not permission to override a collision in catalog identity.
        let observed = if !parsed.parsed.title.is_empty() {
            stem
        } else {
            file.folder_name
                .ok_or(SearchError("missing_owner_identification"))?
        };
        if resolve_scan_owner(c, owner.media(), observed).await? != owner {
            return Ok(scan_rejected("filename_target_mismatch"));
        }
    }
    let (members, target, runtime, files) = match owner {
        ScanOwner::Tv { series_id } => {
            let catalog = scan_catalog(c, series_id).await?;
            let series_type = facts
                .series_type
                .as_deref()
                .ok_or(SearchError("series_type_unconfigured"))?;
            let numbering = parsed
                .numbering
                .as_ref()
                .or_else(|| folder.as_ref().and_then(|f| f.numbering.as_ref()))
                .ok_or(SearchError("file_numbering_incomplete"))?;
            let members = scan_members(&catalog, numbering, facts.scene, series_type, false)?;
            if let Some(numbering) = folder.as_ref().and_then(|f| f.numbering.as_ref()) {
                let allowed: BTreeSet<_> =
                    scan_members(&catalog, numbering, facts.scene, series_type, true)?
                        .into_iter()
                        .map(|m| m.episode_id)
                        .collect();
                if members.iter().any(|m| !allowed.contains(&m.episode_id)) {
                    return Ok(scan_rejected("folder_target_mismatch"));
                }
            }
            if receipt
                .as_ref()
                .is_some_and(|r| members.iter().any(|m| !r.episodes.contains(&m.episode_id)))
            {
                return Ok(scan_rejected("release_target_mismatch"));
            }
            let runtime = scan_runtime(&members, facts.runtime)?;
            let files = members.iter().map(|m| m.old_file_id).collect::<Vec<_>>();
            let ids = members.iter().map(|m| m.episode_id).collect();
            (
                members,
                ReleaseTarget::Tv {
                    series_id,
                    episode_ids: ids,
                },
                runtime,
                files,
            )
        }
        ScanOwner::Movies { movie_id } => (
            Vec::new(),
            ReleaseTarget::Movies { movie_id },
            facts.runtime,
            vec![facts.old_file],
        ),
    };
    let revision = super::revision::scan_revision(
        &parsed.parsed,
        folder.as_ref().map(|f| &f.parsed),
        download.as_ref().map(|f| &f.parsed),
        receipt
            .as_ref()
            .and_then(|r| r.evidence.as_ref())
            .and_then(|e| e.revision.as_ref()),
    )
    .map_err(SearchError)?;
    // Preserve every observation in proof; choose technical facts only from an
    // actual parsed observation when the filename did not supply them.
    if parsed.parsed.quality_name.is_none() {
        parsed.parsed.quality_name = folder
            .as_ref()
            .and_then(|p| p.parsed.quality_name.clone())
            .or_else(|| {
                download
                    .as_ref()
                    .and_then(|p| p.parsed.quality_name.clone())
            })
            .or_else(|| {
                receipt_parsed
                    .as_ref()
                    .and_then(|p| p.parsed.quality_name.clone())
            });
    }
    parsed.parsed.revision = Some(revision.effective.clone());
    let mut evidence = crate::custom_formats::parsed(stem, &parsed.parsed, tv, Some(file.size));
    evidence.filename = Some(file.basename.into());
    // CF revision evidence must refer to an actual retained observation, not an
    // effective value rewritten onto the filename's different marker provenance.
    evidence.revision = [
        Some(&revision.filename),
        revision.folder.as_ref(),
        revision.download.as_ref(),
        revision.receipt.as_ref(),
    ]
    .into_iter()
    .flatten()
    .find(|observed| observed.value == revision.effective)
    .cloned();
    evidence.release_type = tv.then_some(if members.len() > 1 { 2 } else { 1 });
    if let Some(prior) = receipt.as_ref().and_then(|r| r.evidence.as_ref()) {
        evidence.title = prior.title.clone();
        evidence.indexer_flags = prior.indexer_flags;
        if evidence.languages.is_none() {
            evidence.languages = prior.languages.clone();
        }
        if evidence.release_group.is_none() {
            evidence.release_group = prior.release_group.clone();
        }
    }
    crate::custom_formats::populate_quality(
        c,
        &mut evidence,
        parsed.parsed.quality_name.as_deref(),
        tv,
    )
    .await?;
    let quality = match decision::assess_downloaded_quality(
        c,
        owner.media(),
        facts.profile,
        &files,
        &parsed.parsed,
        &evidence,
        facts.original_language,
        warmed,
    )
    .await
    {
        Ok(q) => q,
        Err(SearchError("quality_observation_required")) => {
            return Ok(ScanFileDecision::NeedsObservation {
                requirements: vec![ScanPrerequisite::QualityObservation],
            });
        }
        Err(e) => return Err(e),
    };
    if !quality.reasons.is_empty() {
        let reasons = quality.reasons.clone();
        *rejected_quality = Some(quality);
        return Ok(ScanFileDecision::Rejected { reasons });
    }
    let import_policy = crate::import_policy::read(c, owner.media())
        .await
        .map_err(|_| SearchError("import_policy_unavailable"))?;
    let proof = ScanDecisionProof {
        version: 1,
        owner,
        basename: file.basename.into(),
        relative_path: file.relative_path.into(),
        size: file.size,
        source_identity_json: identity,
        folder_name: file.folder_name.map(str::to_owned),
        download_name: file.download_name.map(str::to_owned),
        authority_fingerprint: receipt
            .as_ref()
            .map(|r| r.fingerprint.clone())
            .unwrap_or(decision::facts_digest(authority)?),
        owner_fingerprint: decision::facts_digest(&facts)?,
        membership_fingerprint: decision::facts_digest(&members)?,
        parsed_fingerprint: decision::facts_digest(&(
            &parsed,
            &folder,
            &download,
            &receipt_parsed,
        ))?,
        // Revalidation must observe changed import settings before any filesystem effect.
        policy_fingerprint: decision::facts_digest(&(&quality, &import_policy))?,
        evidence_fingerprint: decision::facts_digest(&evidence)?,
        revision: revision.clone(),
    };
    if serde_json::to_vec(&proof)
        .map_err(|_| SearchError("scan_proof_invalid"))?
        .len()
        > SCAN_PROOF_BYTES
    {
        return Err(SearchError("scan_proof_limit"));
    }
    let preference = ScanPreference {
        policy: super::revision::preference_key(
            quality.rank,
            Some(&revision.effective),
            quality.score,
            quality.mode,
        )
        .map_err(SearchError)?,
        size: file.size,
        normalized_path: file.relative_path.to_lowercase(),
        relative_path: file.relative_path.into(),
    };
    Ok(ScanFileDecision::PolicyEligible(ScanPolicyEligible {
        proof,
        target,
        members,
        root: facts.root,
        parsed,
        evidence,
        revision,
        quality,
        import_policy,
        runtime_minutes: runtime,
        preference,
        requirements: vec![
            ScanPrerequisite::MediaSample,
            ScanPrerequisite::AudioObservation,
            ScanPrerequisite::SiblingMultipart,
            ScanPrerequisite::UnpackingState,
            ScanPrerequisite::DestinationSpace,
            ScanPrerequisite::NamingPolicy,
            ScanPrerequisite::ImportHistoryCoverage,
        ],
    }))
}
/// Deterministic whole-file greedy selection; a shared physical file is never
/// partially selected. The scanner separately rejects hardlink identity aliases.
pub(crate) fn select_scan_files(files: &[ScanPolicyEligible]) -> super::Result<Vec<usize>> {
    if files.len() > parser::MAX_DOWNLOADED_MEMBERS {
        return Err(SearchError("scan_file_limit"));
    }
    let mut order: Vec<_> = (0..files.len()).collect();
    let mut paths = BTreeSet::new();
    for file in files {
        if !paths.insert((
            &file.proof.relative_path,
            file.proof.owner.id(),
            file.proof.owner.media() as u8,
        )) {
            return Err(SearchError("duplicate_scan_file"));
        }
    }
    order.sort_by(|a, b| files[*b].preference.cmp(&files[*a].preference));
    let mut claimed = BTreeSet::new();
    let mut selected = Vec::new();
    for i in order {
        let keys: Vec<_> = match &files[i].target {
            ReleaseTarget::Tv {
                series_id,
                episode_ids,
            } => episode_ids.iter().map(|id| (0, *series_id, *id)).collect(),
            ReleaseTarget::Movies { movie_id } => vec![(1, *movie_id, 0)],
        };
        if keys.iter().all(|key| !claimed.contains(key)) {
            claimed.extend(keys);
            selected.push(i);
        }
    }
    Ok(selected)
}

#[cfg(test)]
mod scan_tests {
    use super::*;
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    async fn fixture() -> (Scratch, crate::db::Database, Connection) {
        let s = Scratch(
            std::env::temp_dir().join(format!("hrrdarr-scan-decision-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir(&s.0).unwrap();
        let db = crate::db::Database::open_local(s.0.join("db"))
            .await
            .unwrap();
        let c = db.connect().await.unwrap();
        // Import comparisons intentionally do not inherit release wanted/cutoff,
        // availability, upgrade_allowed, minimum CF or movie-language vetoes.
        c.execute_batch("INSERT INTO series(id,title,path)VALUES(1,'Harbor','/fictional/tv');INSERT INTO seasons(series_id,number)VALUES(1,0),(1,1),(1,2);INSERT INTO episodes(id,series_id,season,number,title,runtime,absolute_episode_number,air_date)VALUES(1,1,1,1,'Pilot',45,47,'2026-09-24'),(2,1,1,2,'Next',45,48,'2026-09-25'),(3,1,1,3,'End',45,49,'2026-09-26');INSERT INTO movie_metadata(id,title,year,runtime,original_language)VALUES(1,'Harbor',2020,100,1);INSERT INTO movies(id,metadata_id,path)VALUES(1,1,'/fictional/movie');INSERT INTO quality_profiles VALUES(1,'tv','HD'),(2,'movies','HD');INSERT INTO quality_profile_items(profile_id,media_type,quality_id,position,allowed)VALUES(1,'tv',3,0,0),(1,'tv',7,1,1),(2,'movies',3,0,0),(2,'movies',7,1,1);INSERT INTO quality_profile_policies(profile_id,media_type,upgrade_allowed,cutoff_quality_id,min_format_score,cutoff_format_score,min_upgrade_format_score,language_id)VALUES(1,'tv',0,7,100,100,100,NULL),(2,'movies',0,7,100,100,100,2);INSERT INTO library_settings(media_type,series_id,quality_profile_id,series_type,use_scene_numbering)VALUES('tv',1,1,'standard',0);INSERT INTO library_settings(media_type,movie_id,quality_profile_id,minimum_availability)VALUES('movies',1,2,'released');").await.unwrap();
        (s, db, c)
    }
    const IDENTITY: &str = r#"{"version":1,"dev":1,"ino":2,"size":1073741824,"mtime":1,"mtime_ns":0,"ctime":1,"ctime_ns":0}"#;
    fn input(name: &str) -> ScanFileInput<'_> {
        ScanFileInput {
            basename: name,
            relative_path: name,
            size: 1073741824,
            source_identity_json: IDENTITY,
            folder_name: None,
            download_name: None,
        }
    }
    async fn assess(
        c: &Connection,
        authority: &ScanAuthority,
        file: ScanFileInput<'_>,
    ) -> ScanFileDecision {
        let warm = warm_scan_file(c, authority, file).await.unwrap();
        c.execute_batch("BEGIN").await.unwrap();
        let result = evaluate_scan_file(c, authority, file, None, &warm)
            .await
            .unwrap();
        c.execute_batch("ROLLBACK").await.unwrap();
        result
    }
    fn eligible(result: ScanFileDecision) -> ScanPolicyEligible {
        match result {
            ScanFileDecision::PolicyEligible(p) => p,
            other => panic!("expected policy eligibility, got {other:?}"),
        }
    }
    #[tokio::test]
    async fn scan_observation_consumes_real_source_policy_and_retains_metadata() {
        use crate::import::{scan, scan_fs::Inventory};
        use std::{
            sync::atomic::AtomicBool,
            time::{Duration, Instant},
        };
        let (scratch, _db, c) = fixture().await;
        let directory = scratch.0.join("downloads");
        std::fs::create_dir(&directory).unwrap();
        // One second of PCM audio, identified from bytes rather than the suffix.
        // FLV is intentionally sample-exempt in both pinned import contracts.
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&16036u32.to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&8000u32.to_le_bytes());
        wav.extend_from_slice(&16000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&16000u32.to_le_bytes());
        wav.resize(16044, 0);
        let cancel = AtomicBool::new(false);
        for (domain, authority, name) in [
            (
                MediaDomain::Tv,
                ScanAuthority::Local {
                    owner: ScanOwner::Tv { series_id: 1 },
                },
                "Harbor.S01E01.1080p.WEB-DL.flv",
            ),
            (
                MediaDomain::Movies,
                ScanAuthority::Local {
                    owner: ScanOwner::Movies { movie_id: 1 },
                },
                "Harbor.2020.1080p.WEB-DL.flv",
            ),
        ] {
            let source = directory.join(name);
            std::fs::write(&source, &wav).unwrap();
            let inventory = Inventory::capture(source.to_str().unwrap()).unwrap();
            let identity = inventory.entries()[0].identity_json().unwrap();
            let input = ScanFileInput {
                basename: name,
                relative_path: name,
                size: wav.len() as u64,
                source_identity_json: &identity,
                folder_name: None,
                download_name: None,
            };
            let p = eligible(assess(&c, &authority, input).await);
            let observed = scan::observe(
                &inventory,
                0,
                &p,
                Instant::now() + Duration::from_secs(20),
                &cancel,
            )
            .unwrap();
            assert!(observed.reasons.is_empty(), "{:?}", observed.reasons);
            assert!(
                matches!(
                    observed.media,
                    scan::MediaObservation::Measured {
                        audio_stream_count: 1,
                        ..
                    }
                ),
                "real installed ffprobe required for this consumer test"
            );
            assert_eq!(observed.space, scan::SpaceObservation::Unavailable); // fictional owner parent
            let command = uuid::Uuid::new_v4();
            let child = uuid::Uuid::new_v4();
            c.execute("INSERT INTO downloaded_scan_commands(id,media_type,source_path,requested_mode,ready_order_at,next_attempt_at,created_at)VALUES(?1,?2,?3,'copy',100,100,100)",params![command.to_string(),crate::import_policy::domain(domain),source.to_str().unwrap()]).await.unwrap();
            c.execute("UPDATE downloaded_scan_commands SET status='running',attempts=1,run_generation=1,started_at=100 WHERE id=?",[command.to_string()]).await.unwrap();
            let decision =
                serde_json::json!({"version":1,"observation":observed.media}).to_string();
            c.execute_batch("BEGIN IMMEDIATE").await.unwrap();
            c.execute("INSERT INTO downloaded_scan_children(id,command_id,ordinal,relative_path,source_path,decision_json,disposition)VALUES(?1,?2,0,?3,?4,?5,'rejected')",params![child.to_string(),command.to_string(),name,source.to_str().unwrap(),decision]).await.unwrap();
            observed.persist_media(&c, child, command).await.unwrap();
            c.execute_batch("COMMIT").await.unwrap();
            let raw = scan::retained_media(&c, child, command, &observed.media)
                .await
                .unwrap()
                .unwrap();
            assert!(raw.contains("runtime_ticks"));
            assert!(
                scan::retained_media(&c, child, uuid::Uuid::new_v4(), &observed.media)
                    .await
                    .is_err()
            );
            let mut altered = observed.media.clone();
            if let scan::MediaObservation::Measured {
                audio_stream_count, ..
            } = &mut altered
            {
                *audio_stream_count = 0;
            }
            assert!(
                scan::retained_media(&c, child, command, &altered)
                    .await
                    .is_err()
            );
            let old = crate::import_policy::read(&c, domain).await.unwrap();
            c.execute_batch("BEGIN IMMEDIATE").await.unwrap();
            crate::import_policy::set(
                &c,
                domain,
                &crate::import_policy::Update {
                    revision: old.revision,
                    skip_free_space_check_when_importing: true,
                    minimum_free_space_mib: 100,
                    download_client_working_folders: "downloads".into(),
                },
                true,
            )
            .await
            .unwrap();
            c.execute_batch("COMMIT").await.unwrap();
            let changed = eligible(assess(&c, &authority, input).await);
            let veto = scan::observe(
                &inventory,
                0,
                &changed,
                Instant::now() + Duration::from_secs(20),
                &cancel,
            )
            .unwrap();
            assert_eq!(veto.space, scan::SpaceObservation::Skipped);
            assert_eq!(veto.reasons, vec!["working_folder"]);
            assert_eq!(std::fs::read(&source).unwrap(), wav);
            std::fs::rename(&source, source.with_extension("moved")).unwrap();
            assert_eq!(
                scan::retained_media(&c, child, command, &observed.media)
                    .await
                    .unwrap(),
                Some(raw)
            );
            assert!(
                scan::observe(
                    &inventory,
                    0,
                    &changed,
                    Instant::now() + Duration::from_secs(20),
                    &cancel
                )
                .is_err()
            );
        }
    }
    #[tokio::test]
    async fn scan_complete_members_runtime_and_both_domain_import_equality() {
        let (_s, _db, c) = fixture().await;
        c.execute_batch("INSERT INTO episode_files(id,series_id,path)VALUES(1,1,'/fictional/tv/old.mkv'),(2,1,'/fictional/tv/other.mkv');UPDATE episodes SET episode_file_id=1 WHERE id IN(1,2);UPDATE episodes SET episode_file_id=2 WHERE id=3;INSERT INTO file_metadata(media_type,episode_file_id,quality_id,revision_json)VALUES('tv',1,3,'{\"version\":1,\"real\":0,\"is_repack\":false}'),('tv',2,3,'{\"version\":1,\"real\":0,\"is_repack\":false}');INSERT INTO movie_files(id,movie_id,path)VALUES(1,1,'/fictional/movie/old.mkv');INSERT INTO file_metadata(media_type,movie_file_id,quality_id,revision_json)VALUES('movies',1,3,'{\"version\":1,\"real\":0,\"is_repack\":false}');").await.unwrap();
        let tv = ScanAuthority::Local {
            owner: ScanOwner::Tv { series_id: 1 },
        };
        let movie = ScanAuthority::Local {
            owner: ScanOwner::Movies { movie_id: 1 },
        };
        let p = eligible(assess(&c, &tv, input("Harbor.S01E01-E03.1080p.WEB-DL.mkv")).await);
        assert_eq!(
            p.members.iter().map(|m| m.episode_id).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(p.runtime_minutes, Some(135));
        assert_eq!(p.quality.originals.len(), 2);
        assert_eq!(p.revision.effective.version, 1);
        assert!(!p.requirements.is_empty());
        let m = eligible(assess(&c, &movie, input("Harbor.2020.1080p.WEB-DL.mkv")).await);
        assert_eq!(m.runtime_minutes, Some(100));
        assert_eq!(m.quality.originals.len(), 1);
        // Every distinct original participates: a better third-member file rejects
        // the entire multi-member source, even though the first two compare equal.
        c.execute_batch(
            "UPDATE file_metadata SET quality_id=7 WHERE media_type='tv' AND episode_file_id=2",
        )
        .await
        .unwrap();
        assert!(
            matches!(assess(&c,&tv,input("Harbor.S01E01-E03.1080p.WEB-DL.mkv")).await,ScanFileDecision::Rejected{reasons} if reasons==["not_quality_upgrade"])
        );
        assert!(matches!(
            assess(&c, &movie, input("Harbor.2021.1080p.WEB-DL.mkv")).await,
            ScanFileDecision::Rejected { .. }
        ));
    }
    #[tokio::test]
    async fn scan_format_breadth_and_sample_titles_retain_measured_policy() {
        let (_s, _db, c) = fixture().await;
        c.execute_batch("UPDATE series SET title='Sample' WHERE id=1;UPDATE movie_metadata SET title='Sample' WHERE id=1").await.unwrap();
        let tv = ScanAuthority::Local {
            owner: ScanOwner::Tv { series_id: 1 },
        };
        let movie = ScanAuthority::Local {
            owner: ScanOwner::Movies { movie_id: 1 },
        };
        for (authority, stem) in [(&tv, "Sample.S01E01"), (&movie, "Sample.2020")] {
            for extension in ["WEBM", "mov", "flv", "strm", "iso", "m3u", "dvr-ms"] {
                let name = format!("{stem}.1080p.WEB-DL.{extension}");
                let result = eligible(assess(&c, authority, input(&name)).await);
                // Format recognition and a legitimate title never bypass the measured checks.
                assert!(result.requirements.contains(&ScanPrerequisite::MediaSample));
                assert!(
                    result
                        .requirements
                        .contains(&ScanPrerequisite::AudioObservation)
                );
            }
            assert!(
                matches!(assess(&c, authority, input(&format!("{stem}.1080p.WEB-DL.exe"))).await,
                ScanFileDecision::Rejected { reasons } if reasons == ["unsupported_file_type"])
            );
        }
        eligible(assess(&c, &movie, input("Sample.2020.1080p.WEB-DL.mk3d")).await);
        assert!(
            matches!(assess(&c, &tv, input("Sample.S01E01.1080p.WEB-DL.mk3d")).await,
            ScanFileDecision::Rejected { reasons } if reasons == ["unsupported_file_type"])
        );
    }
    #[tokio::test]
    async fn scan_import_settings_are_domain_scoped_and_invalidate_retained_proofs() {
        use crate::import_policy::{self, Update};
        let (_s, _db, c) = fixture().await;
        for (media, other, owner, name) in [
            (
                MediaDomain::Tv,
                MediaDomain::Movies,
                ScanOwner::Tv { series_id: 1 },
                "Harbor.S01E01.1080p.WEB-DL.mkv",
            ),
            (
                MediaDomain::Movies,
                MediaDomain::Tv,
                ScanOwner::Movies { movie_id: 1 },
                "Harbor.2020.1080p.WEB-DL.mkv",
            ),
        ] {
            let authority = ScanAuthority::Local { owner };
            let file = input(name);
            let before = eligible(assess(&c, &authority, file).await);
            let warm = warm_scan_file(&c, &authority, file).await.unwrap();
            let other_policy = import_policy::read(&c, other).await.unwrap();
            let tx = c
                .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
                .await
                .unwrap();
            import_policy::set(
                &tx,
                other,
                &Update {
                    revision: other_policy.revision,
                    skip_free_space_check_when_importing: !other_policy
                        .skip_free_space_check_when_importing,
                    minimum_free_space_mib: other_policy.minimum_free_space_mib,
                    download_client_working_folders: other_policy.download_client_working_folders,
                },
                true,
            )
            .await
            .unwrap();
            tx.commit().await.unwrap();
            c.execute_batch("BEGIN").await.unwrap();
            eligible(
                evaluate_scan_file(&c, &authority, file, Some(&before.proof), &warm)
                    .await
                    .unwrap(),
            );
            c.execute_batch("ROLLBACK").await.unwrap();
            // Every setting and an explicit same-value revision invalidate old approval.
            for changed in 0..4 {
                let before = eligible(assess(&c, &authority, file).await);
                let warm = warm_scan_file(&c, &authority, file).await.unwrap();
                let current = import_policy::read(&c, media).await.unwrap();
                assert_eq!(before.import_policy, current);
                let mut update = Update {
                    revision: current.revision,
                    skip_free_space_check_when_importing: current
                        .skip_free_space_check_when_importing,
                    minimum_free_space_mib: current.minimum_free_space_mib,
                    download_client_working_folders: current.download_client_working_folders,
                };
                match changed {
                    0 => {
                        update.skip_free_space_check_when_importing =
                            !update.skip_free_space_check_when_importing
                    }
                    1 => update.minimum_free_space_mib += 1,
                    2 => update.download_client_working_folders = " busy|_FAILED_".into(),
                    _ => (),
                }
                let tx = c
                    .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
                    .await
                    .unwrap();
                let saved = import_policy::set(&tx, media, &update, true).await.unwrap();
                tx.commit().await.unwrap();
                c.execute_batch("BEGIN").await.unwrap();
                assert_eq!(
                    evaluate_scan_file(&c, &authority, file, Some(&before.proof), &warm)
                        .await
                        .unwrap_err()
                        .0,
                    "scan_preflight_changed"
                );
                c.execute_batch("ROLLBACK").await.unwrap();
                let current = eligible(assess(&c, &authority, file).await);
                assert_eq!(current.import_policy, saved);
                assert!(
                    current
                        .requirements
                        .contains(&ScanPrerequisite::DestinationSpace)
                );
                assert!(
                    current
                        .requirements
                        .contains(&ScanPrerequisite::UnpackingState)
                );
            }
        }
    }
    #[tokio::test]
    async fn scan_runtime_uses_series_fallback_and_invalidates_proof() {
        let (_s, _db, c) = fixture().await;
        let authority = ScanAuthority::Local {
            owner: ScanOwner::Tv { series_id: 1 },
        };
        let file = input("Harbor.S01E01-E03.1080p.WEB-DL.mkv");
        c.execute_batch("UPDATE series SET runtime=20 WHERE id=1;UPDATE episodes SET runtime=CASE id WHEN 1 THEN 10 WHEN 2 THEN 0 ELSE NULL END").await.unwrap();
        let selected = eligible(assess(&c, &authority, file).await);
        assert_eq!(selected.runtime_minutes, Some(50));
        let warm = warm_scan_file(&c, &authority, file).await.unwrap();
        c.execute_batch("UPDATE series SET runtime=30 WHERE id=1;BEGIN")
            .await
            .unwrap();
        assert_eq!(
            evaluate_scan_file(&c, &authority, file, Some(&selected.proof), &warm)
                .await
                .unwrap_err()
                .0,
            "scan_preflight_changed"
        );
        c.execute_batch("ROLLBACK").await.unwrap();
        assert_eq!(
            eligible(assess(&c, &authority, file).await).runtime_minutes,
            Some(70)
        );
        c.execute_batch("UPDATE series SET runtime=NULL WHERE id=1")
            .await
            .unwrap();
        assert_eq!(
            eligible(assess(&c, &authority, file).await).runtime_minutes,
            Some(10)
        );
        c.execute_batch("UPDATE episodes SET runtime=0")
            .await
            .unwrap();
        // Upstream's zero-total fallback is45 once for this file, not once per unknown episode.
        assert_eq!(
            eligible(assess(&c, &authority, file).await).runtime_minutes,
            Some(45)
        );
        c.execute_batch("UPDATE series SET runtime=0 WHERE id=1;UPDATE episodes SET runtime=NULL")
            .await
            .unwrap();
        assert_eq!(
            eligible(assess(&c, &authority, file).await).runtime_minutes,
            Some(45)
        );
        let mut members = selected.members;
        members[0].runtime = Some(i64::MAX);
        members[1].runtime = Some(1);
        assert_eq!(
            scan_runtime(&members, None).unwrap_err().0,
            "runtime_overflow"
        );
    }
    #[tokio::test]
    async fn scan_measured_sample_and_audio_boundaries_keep_domain_exemptions() {
        use crate::import::probe::Observation;
        let observed = |ticks: Option<i64>, audio: bool| {
            let duration =
                ticks.map(|ticks| format!("{}.{:07}", ticks / 10_000_000, ticks % 10_000_000));
            let streams = if audio {
                serde_json::json!([{"codec_type":"audio"}])
            } else {
                serde_json::json!([])
            };
            Observation::Measured(
                crate::media_files::probe::parse(
                    &serde_json::to_vec(
                        &serde_json::json!({"format":{"duration":duration},"streams":streams}),
                    )
                    .unwrap(),
                )
                .unwrap(),
            )
        };
        let (_s, _db, c) = fixture().await;
        for (authority, name) in [
            (
                ScanAuthority::Local {
                    owner: ScanOwner::Tv { series_id: 1 },
                },
                "Harbor.S01E01.1080p.WEB-DL.mkv",
            ),
            (
                ScanAuthority::Local {
                    owner: ScanOwner::Movies { movie_id: 1 },
                },
                "Harbor.2020.1080p.WEB-DL.mkv",
            ),
        ] {
            let mut decision = eligible(assess(&c, &authority, input(name)).await);
            for (expected, seconds) in [
                (0, 15),
                (3, 15),
                (4, 90),
                (10, 90),
                (11, 300),
                (30, 300),
                (31, 600),
                (100, 600),
            ] {
                decision.runtime_minutes = Some(expected);
                assert!(
                    decision
                        .media_rejections(&observed(Some(seconds * 10_000_000), true))
                        .is_empty()
                );
                assert_eq!(
                    decision.media_rejections(&observed(Some(seconds * 10_000_000 - 1), true)),
                    ["sample_file"]
                );
            }
            assert_eq!(
                decision.media_rejections(&observed(Some(0), false)),
                ["sample_file", "no_audio_tracks"]
            );
            assert_eq!(
                decision.media_rejections(&observed(None, true)),
                ["sample_indeterminate"]
            );
            assert_eq!(
                decision.media_rejections(&Observation::Unavailable("probe_timeout")),
                ["sample_indeterminate"]
            );
            for extension in ["FLV", "strm", "ISO", "img", "m2ts"] {
                decision.proof.basename = name.replace("mkv", extension);
                let exempt = extension.eq_ignore_ascii_case("flv")
                    || extension == "strm"
                    || matches!(
                        authority,
                        ScanAuthority::Local {
                            owner: ScanOwner::Movies { .. }
                        }
                    );
                let reasons = decision.media_rejections(&Observation::Unavailable("probe_failed"));
                assert_eq!(reasons.is_empty(), exempt);
                let reasons = decision.media_rejections(&observed(Some(0), false));
                assert!(reasons.contains(&"no_audio_tracks"));
                assert_eq!(!reasons.contains(&"sample_file"), exempt);
            }
            if matches!(
                authority,
                ScanAuthority::Local {
                    owner: ScanOwner::Tv { .. }
                }
            ) {
                decision.proof.basename = name.into();
                decision.members[0].season = 0;
                assert!(
                    decision
                        .media_rejections(&Observation::Unavailable("probe_failed"))
                        .is_empty()
                );
                assert_eq!(
                    decision.media_rejections(&observed(Some(0), false)),
                    ["no_audio_tracks"]
                );
            }
        }
    }
    #[tokio::test]
    async fn scan_excludes_relative_extras_without_rejecting_owner_titles() {
        let (_s, _db, c) = fixture().await;
        for (authority, name, tv) in [
            (
                ScanAuthority::Local {
                    owner: ScanOwner::Tv { series_id: 1 },
                },
                "Harbor.S01E01.1080p.WEB-DL.mkv",
                true,
            ),
            (
                ScanAuthority::Local {
                    owner: ScanOwner::Movies { movie_id: 1 },
                },
                "Harbor.2020.1080p.WEB-DL.mkv",
                false,
            ),
        ] {
            for parent in [
                "nested/SaMpLeS",
                "Extras",
                "nested/.cache",
                "@eaDir",
                "Plex Versions",
            ] {
                let relative = format!("{parent}/{name}");
                let file = ScanFileInput {
                    relative_path: &relative,
                    ..input(name)
                };
                assert!(matches!(assess(&c, &authority, file).await,
                    ScanFileDecision::Rejected { reasons } if reasons == ["excluded_media_path"]));
            }
            for parent in ["sample", "backdrops", "theme_music", "My Samples"] {
                let relative = format!("{parent}/{name}");
                let file = ScanFileInput {
                    relative_path: &relative,
                    ..input(name)
                };
                let rejected = (parent == "sample" && !tv)
                    || (tv && matches!(parent, "backdrops" | "theme_music"));
                let result = assess(&c, &authority, file).await;
                if rejected {
                    assert!(
                        matches!(result, ScanFileDecision::Rejected { reasons } if reasons == ["excluded_media_path"])
                    );
                } else {
                    eligible(result);
                }
            }
            let trailer = name.replace(".mkv", "-trailer.mkv");
            assert!(matches!(assess(&c, &authority, input(&trailer)).await,
                ScanFileDecision::Rejected { reasons } if reasons == ["excluded_media_path"]));
        }
    }
    #[tokio::test]
    async fn scan_daily_absolute_scene_complete_and_ambiguous_members() {
        let (_s, _db, c) = fixture().await;
        let tv = ScanAuthority::Local {
            owner: ScanOwner::Tv { series_id: 1 },
        };
        c.execute_batch("UPDATE library_settings SET series_type='daily' WHERE series_id=1;INSERT INTO episodes(id,series_id,season,number,title,air_date)VALUES(4,1,0,1,'Special','2026-09-24')").await.unwrap();
        let p = eligible(
            assess(
                &c,
                &tv,
                input("Harbor.2026.09.24.2026.09.25.1080p.WEB-DL.mkv"),
            )
            .await,
        );
        assert_eq!(p.members.len(), 2);
        c.execute_batch("UPDATE library_settings SET series_type='anime',use_scene_numbering=1 WHERE series_id=1;UPDATE episodes SET scene_season_number=3,scene_episode_number=7 WHERE id IN(1,2);UPDATE episodes SET scene_season_number=3,scene_episode_number=8 WHERE id=3").await.unwrap();
        let p = eligible(assess(&c, &tv, input("Harbor.S03E07E08.1080p.WEB-DL.mkv")).await);
        assert_eq!(p.members.len(), 3);
        let p = eligible(assess(&c, &tv, input("Harbor - 047-049v0 1080p WEB-DL.mkv")).await);
        assert_eq!(p.members.len(), 3);
        assert_eq!(p.revision.effective.version, 0);
        c.execute_batch("UPDATE episodes SET unverified_scene_numbering=1 WHERE id=2")
            .await
            .unwrap();
        assert_eq!(
            warm_scan_file(&c, &tv, input("Harbor.S03E07.1080p.WEB-DL.mkv"))
                .await
                .err()
                .unwrap()
                .0,
            "scene_mapping_unverified"
        );
        c.execute_batch("UPDATE episodes SET absolute_episode_number=47 WHERE id=2")
            .await
            .unwrap();
        assert_eq!(
            warm_scan_file(&c, &tv, input("Harbor - 047 1080p WEB-DL.mkv"))
                .await
                .err()
                .unwrap()
                .0,
            "episode_match_ambiguous"
        );
        assert_eq!(
            warm_scan_file(&c, &tv, input("Harbor.S01E01-E05.1080p.WEB-DL.mkv"))
                .await
                .err()
                .unwrap()
                .0,
            "episode_match_missing"
        );
    }
    #[tokio::test]
    async fn scan_proof_rejects_changed_identity_membership_and_policy() {
        let (_s, _db, c) = fixture().await;
        let tv = ScanAuthority::Local {
            owner: ScanOwner::Tv { series_id: 1 },
        };
        let file = input("Harbor.S01E01.1080p.WEB-DL.mkv");
        let warm = warm_scan_file(&c, &tv, file).await.unwrap();
        let proof = match warm.decision() {
            ScanFileDecision::PolicyEligible(p) => p.proof.clone(),
            other => panic!("{other:?}"),
        };
        c.execute_batch("BEGIN").await.unwrap();
        assert!(
            evaluate_scan_file(&c, &tv, file, Some(&proof), &warm)
                .await
                .is_ok()
        );
        let changed = IDENTITY.replace("\"ino\":2", "\"ino\":3");
        assert_eq!(
            evaluate_scan_file(
                &c,
                &tv,
                ScanFileInput {
                    source_identity_json: &changed,
                    ..file
                },
                Some(&proof),
                &warm
            )
            .await
            .unwrap_err()
            .0,
            "scan_preflight_changed"
        );
        c.execute_batch("ROLLBACK;UPDATE episodes SET runtime=46 WHERE id=1;BEGIN")
            .await
            .unwrap();
        assert_eq!(
            evaluate_scan_file(&c, &tv, file, Some(&proof), &warm)
                .await
                .unwrap_err()
                .0,
            "scan_preflight_changed"
        );
        c.execute_batch("ROLLBACK;UPDATE quality_profile_policies SET min_format_score=99 WHERE profile_id=1;BEGIN").await.unwrap();
        assert_eq!(
            evaluate_scan_file(&c, &tv, file, None, &warm)
                .await
                .unwrap_err()
                .0,
            "scan_preflight_changed"
        );
        c.execute_batch("ROLLBACK").await.unwrap();
        assert_eq!(
            evaluate_scan_file(&c, &tv, file, None, &warm)
                .await
                .unwrap_err()
                .0,
            "scan_snapshot_required"
        );
        let observation = assess(&c, &tv, input("Harbor.S01E01.mkv")).await;
        assert!(
            matches!(observation,ScanFileDecision::NeedsObservation{requirements} if requirements==[ScanPrerequisite::QualityObservation])
        );
        assert!(
            scan_file_valid(ScanFileInput {
                relative_path: "../a.mkv",
                ..file
            })
            .is_err()
        );
        assert!(source_identity("{}", 1).is_err());
    }
    #[tokio::test]
    async fn scan_selection_is_whole_file_and_deterministic() {
        let (_s, _db, c) = fixture().await;
        let tv = ScanAuthority::Local {
            owner: ScanOwner::Tv { series_id: 1 },
        };
        let a = eligible(assess(&c, &tv, input("Harbor.S01E01E02.1080p.WEB-DL.mkv")).await);
        let b = eligible(assess(&c, &tv, input("Harbor.S01E02E03.1080p.WEB-DL.mkv")).await);
        let files = vec![b, a];
        assert_eq!(select_scan_files(&files).unwrap(), vec![1]);
    }
    async fn receipt(
        c: &Connection,
        media: MediaDomain,
        explicit_revision: Option<i64>,
    ) -> ScanAuthority {
        let indexer = uuid::Uuid::new_v4();
        let client = uuid::Uuid::new_v4();
        let command = uuid::Uuid::new_v4();
        let candidate = uuid::Uuid::new_v4();
        let domain = super::super::domain(media);
        for (id, implementation) in [(indexer, "torznab"), (client, "qbittorrent")] {
            c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint)VALUES(?,?,?,1,1,1,1,'http://fixture.invalid')",params![id.to_string(),implementation,implementation]).await.unwrap();
        }
        c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year)VALUES(?,'torznab',?,?,'[]',?,?)",params![indexer.to_string(),domain,if media==MediaDomain::Tv{"[5000]"}else{"[2000]"},if media==MediaDomain::Tv{Some(0)}else{None},if media==MediaDomain::Movies{Some(0)}else{None}]).await.unwrap();
        c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags)VALUES(?,'qbittorrent',?,?,0,0,'started','default',0,0,0)",params![client.to_string(),domain,domain]).await.unwrap();
        c.execute("INSERT INTO rss_commands(id,name,media_type,indexer_id,indexer_revision,client_id,client_revision,next_attempt_at,created_at)VALUES(?,'rss_sync',?,?,1,?,1,100,100)",params![command.to_string(),domain,indexer.to_string(),client.to_string()]).await.unwrap();
        let title = if media == MediaDomain::Tv {
            "Harbor.S01E01.1080p.WEB-DL"
        } else {
            "Harbor.2020.1080p.WEB-DL"
        };
        let evidence = explicit_revision.map(|version| {
            let p =
                parser::parse(&format!("{title}.v{version}"), media == MediaDomain::Tv).unwrap();
            serde_json::to_string(&crate::custom_formats::parsed(
                title,
                &p,
                media == MediaDomain::Tv,
                Some(1073741824),
            ))
            .unwrap()
        });
        c.execute("INSERT INTO rss_candidates(id,command_id,media_type,indexer_id,indexer_revision,client_id,client_revision,fingerprint,title,private_payload,series_id,status,decision_reasons_json,created_at,updated_at)SELECT ?,id,media_type,indexer_id,indexer_revision,client_id,client_revision,?,?,?,?,'pending','[]',100,100 FROM rss_commands WHERE id=?",params![candidate.to_string(),"a".repeat(64),title,vec![1u8;29],if media==MediaDomain::Tv{Some(1)}else{None},command.to_string()]).await.unwrap();
        if media == MediaDomain::Tv {
            c.execute(
                "INSERT INTO rss_candidate_episodes VALUES(?,1,1)",
                [candidate.to_string()],
            )
            .await
            .unwrap();
        }
        let hash = "a".repeat(40);
        let identity=serde_json::json!({"version":1,"target":{"media_type":if media==MediaDomain::Tv{"episode"}else{"movie"},"id":1},"hashes":[hash],"settings_fingerprint":"c".repeat(64),"payload_sha256":"d".repeat(64)}).to_string();
        c.execute("UPDATE rss_candidates SET status='prepared',submission_identity_json=?,movie_id=? WHERE id=?",params![identity,if media==MediaDomain::Movies{Some(1)}else{None},candidate.to_string()]).await.unwrap();
        c.execute(
            "INSERT INTO rss_hash_claims VALUES(?,?,?)",
            params![client.to_string(), hash.clone(), candidate.to_string()],
        )
        .await
        .unwrap();
        c.execute(
            // Capture evidence at the schema's real dispatch boundary, never initial INSERT.
            "UPDATE rss_candidates SET status='submitting',private_payload=NULL,comparison_facts_json=? WHERE id=?",
            params![evidence,candidate.to_string()],
        )
        .await
        .unwrap();
        c.execute(
            "UPDATE rss_candidates SET status='observed',observed_hash=? WHERE id=?",
            params![hash.clone(), candidate.to_string()],
        )
        .await
        .unwrap();
        ScanAuthority::Receipt {
            candidate_id: candidate,
            provider_id: client,
            provider_revision: 1,
            media_type: media,
            remote_id: hash,
        }
    }
    #[tokio::test]
    async fn scan_real_receipt_missing_revision_is_local_evidence_not_untracked_authority() {
        let (_s, _db, c) = fixture().await;
        for (media, name) in [
            (MediaDomain::Tv, "Harbor.S01E01.1080p.WEB-DL.mkv"),
            (MediaDomain::Movies, "Harbor.2020.1080p.WEB-DL.mkv"),
        ] {
            let authority = receipt(&c, media, None).await;
            let p = eligible(assess(&c, &authority, input(name)).await);
            assert!(p.revision.receipt.is_none());
            assert_eq!(p.revision.effective.version, 1);
            let ScanAuthority::Receipt {
                candidate_id,
                provider_id,
                provider_revision,
                media_type,
                remote_id,
            } = authority
            else {
                unreachable!()
            };
            let stale = ScanAuthority::Receipt {
                candidate_id,
                provider_id,
                provider_revision: provider_revision + 1,
                media_type,
                remote_id,
            };
            assert_eq!(
                warm_scan_file(&c, &stale, input(name))
                    .await
                    .err()
                    .unwrap()
                    .0,
                "release_authority_changed"
            );
        }
        // A second receipt is a separate scenario; active receipts retain target ownership.
        let (_revision_scratch, _revision_db, c) = fixture().await;
        let authority = receipt(&c, MediaDomain::Tv, Some(2)).await;
        let p = eligible(assess(&c, &authority, input("Harbor.S01E01v0.1080p.WEB-DL.mkv")).await);
        assert_eq!(p.revision.filename.value.version, 0);
        assert_eq!(p.revision.receipt.as_ref().unwrap().value.version, 2);
        assert_eq!(p.revision.effective.version, 2);
        let client_name = "Harbor.S01E01.REAL.v0.1080p.WEB-DL";
        let with_client = ScanFileInput {
            download_name: Some(client_name),
            ..input("Harbor.S01E01.1080p.WEB-DL.mkv")
        };
        let p = eligible(assess(&c, &authority, with_client).await);
        assert_eq!(
            (p.revision.effective.real, p.revision.effective.version),
            (1, 0)
        );
        assert_eq!(p.proof.download_name.as_deref(), Some(client_name));
        assert!(p.revision.download.is_some());
        let local = ScanAuthority::Local {
            owner: ScanOwner::Tv { series_id: 1 },
        };
        assert_eq!(
            warm_scan_file(&c, &local, with_client)
                .await
                .err()
                .unwrap()
                .0,
            "download_name_requires_receipt"
        );
        // Missing known receipt must fail even when an explicit local owner could match.
        let missing = ScanAuthority::Receipt {
            candidate_id: uuid::Uuid::new_v4(),
            provider_id: uuid::Uuid::new_v4(),
            provider_revision: 1,
            media_type: MediaDomain::Tv,
            remote_id: "a".repeat(40),
        };
        assert_eq!(
            warm_scan_file(&c, &missing, input("Harbor.S01E01.mkv"))
                .await
                .err()
                .unwrap()
                .0,
            "release_authority_changed"
        );
    }
    #[tokio::test]
    async fn scan_more_than_global_cache_capacity_revalidates_all_originals() {
        let (_s, _db, c) = fixture().await;
        let specs=serde_json::json!([{"name":"English","negate":false,"required":true,"condition":{"kind":"language","value":1,"except_language":false}}]).to_string();
        // A nonempty unique catalog forces real scoring and prevents another parallel
        // fixture from restoring the first entry into the process-wide cache.
        c.execute("INSERT INTO custom_formats(id,media_type,name,include_when_renaming,specifications_json)VALUES(1,'tv',?,0,?)",params![format!("Capacity {}",uuid::Uuid::new_v4()),specs]).await.unwrap();
        c.execute_batch("BEGIN;
          WITH RECURSIVE n(i) AS(VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<2049) INSERT INTO episode_files(id,series_id,path)SELECT i,1,'/fictional/tv/old-'||i||'.mkv' FROM n;
          WITH RECURSIVE n(i) AS(VALUES(4) UNION ALL SELECT i+1 FROM n WHERE i<2049) INSERT INTO episodes(id,series_id,season,number,title,runtime,episode_file_id)SELECT i,1,1,i,'Episode '||i,45,i FROM n;
          UPDATE episodes SET episode_file_id=id WHERE id<=3;
          INSERT INTO file_metadata(media_type,episode_file_id,quality_id,revision_json)SELECT 'tv',id,3,'{\"version\":1,\"real\":0,\"is_repack\":false}' FROM episode_files;
          COMMIT;").await.unwrap();
        let authority = ScanAuthority::Local {
            owner: ScanOwner::Tv { series_id: 1 },
        };
        let file = input("Harbor.S01E01-E2049.1080p.WEB-DL.mkv");
        let warm = warm_scan_file(&c, &authority, file).await.unwrap();
        let prepared = match warm.decision() {
            ScanFileDecision::PolicyEligible(p) => p,
            other => panic!("{other:?}"),
        };
        assert_eq!(prepared.members.len(), 2049);
        assert_eq!(prepared.quality.originals.len(), 2049);
        let formats = crate::custom_formats::catalog(&c, MediaDomain::Tv)
            .await
            .unwrap();
        // The original cache-only approach is demonstrably insufficient here: the
        // first actual old-file score has been evicted by 2048 later distinct scores.
        assert_eq!(
            crate::custom_formats::score(
                &formats,
                MediaDomain::Tv,
                prepared.quality.originals[0].evidence.clone(),
                None,
                vec![],
                true
            )
            .await
            .unwrap_err()
            .0,
            "custom_format_state_changed"
        );
        c.execute_batch("BEGIN IMMEDIATE").await.unwrap();
        let current = eligible(
            evaluate_scan_file(&c, &authority, file, Some(&prepared.proof), &warm)
                .await
                .unwrap(),
        );
        assert_eq!(current.quality.originals.len(), 2049);
        assert_eq!(current.runtime_minutes, Some(2049 * 45));
        c.execute_batch("ROLLBACK").await.unwrap();
    }
}
