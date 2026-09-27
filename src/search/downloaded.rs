//! Downloaded filenames must independently agree with their trusted receipt target.
use super::{Disposition, ReleaseDecision, ReleaseTarget, SearchError, decision, parser};
use crate::{api::MediaDomain, db::MediaTarget};
use libsql::{Connection, params};

pub(crate) struct AcceptedFile {
    pub basename: String,
    pub root: String,
    pub quality_id: i64,
    pub revision_json: String,
    pub edition: Option<String>,
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

pub(crate) async fn evaluate(
    c: &Connection,
    target: &MediaTarget,
    filename: &str,
    size: u64,
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
    let parsed = match parser::parse(stem, tv) {
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
                Some(parser::Numbering::Daily { date }) if series_type == "daily" => {
                    match_daily(c, series_id, *id, r.get::<Option<String>>(10)?, date).await?
                }
                Some(parser::Numbering::Absolute { episode }) if series_type == "anime" => {
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
    let mut result = ReleaseDecision {
        target: Some(typed),
        disposition: Disposition::Accept,
        reasons: vec![],
        not_before: None,
        quality_id: None,
        parsed: Some(parsed.clone()),
    };
    if parsed.revision > 1 {
        result.deny("proper_upgrade_unsupported");
    }
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
            revision_json: "{\"version\":1,\"real\":0,\"is_repack\":false}".into(),
            edition: parsed.edition,
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
    let mut rows = c
        .query(
            "SELECT id,season FROM episodes WHERE series_id=? AND air_date=? LIMIT 1001",
            params![series_id, date],
        )
        .await?;
    let mut all = Vec::new();
    let mut count = 0;
    while let Some(row) = rows.next().await? {
        count += 1;
        if count > 1000 {
            return Err(SearchError("episode_match_limit"));
        }
        all.push((row.get::<i64>(0)?, row.get::<i64>(1)?));
    }
    if all.len() <= 1 {
        // Only the target itself (already confirmed above) has this date.
        return Ok(None);
    }
    let regular: Vec<i64> = all
        .into_iter()
        .filter(|(_, season)| *season > 0)
        .map(|(id, _)| id)
        .collect();
    if regular.len() == 1 {
        return Ok((regular[0] != episode_id).then_some("filename_target_mismatch"));
    }
    Ok(Some("ambiguous_air_date_match"))
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
    if scene {
        let hits = absolute_hits(c, series_id, "scene_absolute_episode_number", episode).await?;
        if hits.len() == 1 {
            return Ok((hits[0] != episode_id).then_some("filename_target_mismatch"));
        }
    }
    let hits = absolute_hits(c, series_id, "absolute_episode_number", episode).await?;
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
            let result = evaluate(&c, &target, name, 1073741824).await.unwrap();
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
            let accepted = evaluate(&c, &target, name, 1073741824)
                .await
                .unwrap()
                .accepted
                .unwrap();
            assert_eq!(accepted.quality_id, 3);
            assert!(!accepted.basename.contains('/'));
        }
        // Import uses the same policy guard: no audio measurement exists for Original/concrete.
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
            let result = evaluate(
                &c,
                &MediaTarget::Movie(1),
                "Harbor.2020.1080p.WEB-DL.mkv",
                1073741824,
            )
            .await
            .unwrap();
            assert_eq!(result.accepted.is_some(), language == -1);
            assert_eq!(
                result
                    .reasons
                    .contains(&"language_policy_unsupported".into()),
                language != -1
            );
            assert!(
                evaluate(
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
        let tiny = evaluate(
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
            let result = evaluate(&c, &target, name, 1073741824).await.unwrap();
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
        let accepted = evaluate(
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
        let ambiguous = evaluate(
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
        let unknown = evaluate(
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
        let mismatch = evaluate(
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
        let accepted = evaluate(
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
        let mismatch = evaluate(
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
        let ambiguous = evaluate(
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
        let unknown = evaluate(
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
        let accepted = evaluate(
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
        let accepted = evaluate(
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
        let accepted = evaluate(
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
            let result = evaluate(&c, &target, name, 1073741824).await.unwrap();
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
        let result = evaluate(
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
