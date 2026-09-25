//! Downloaded filenames must independently agree with their trusted receipt target.
use super::{Disposition, ReleaseDecision, ReleaseTarget, SearchError, decision, parser};
use crate::{api::MediaDomain, db::MediaTarget};
use libsql::Connection;

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
            let Some(r)=c.query("SELECT s.id,s.title,s.path,l.quality_profile_id,l.series_type,l.use_scene_numbering,e.season,e.number,e.runtime,e.episode_file_id FROM episodes e JOIN series s ON s.id=e.series_id LEFT JOIN library_settings l ON l.series_id=s.id WHERE e.id=?",[*id]).await?.next().await? else{return Ok(rejected("target_changed"))};
            let series_id = r.get::<i64>(0)?;
            let title = r.get::<String>(1)?;
            if parser::normalize(&parsed.title) != parser::normalize(&title) {
                return Ok(rejected("filename_target_mismatch"));
            }
            if r.get::<Option<String>>(4)?.as_deref() != Some("standard")
                || r.get::<Option<i64>>(5)? == Some(1)
            {
                return Ok(rejected("numbering_unsupported"));
            }
            let matches = match &parsed.numbering {
                Some(parser::Numbering::Episodes { season, episodes }) => {
                    *season == r.get::<i64>(6)? && episodes.as_slice() == [r.get::<i64>(7)?]
                }
                _ => false,
            };
            if !matches {
                return Ok(rejected("filename_target_mismatch"));
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
        c.execute_batch("INSERT INTO series(id,title,path)VALUES(1,'Harbor','/fictional/tv'); INSERT INTO seasons(series_id,number)VALUES(1,1);INSERT INTO episodes(id,series_id,season,number,title,runtime)VALUES(1,1,1,1,'Pilot',45);INSERT INTO movie_metadata(id,title,year,runtime)VALUES(1,'Harbor',2020,100);INSERT INTO movies(id,metadata_id,path)VALUES(1,1,'/fictional/movie');INSERT INTO quality_profiles VALUES(1,'tv','HD'),(2,'movies','HD');INSERT INTO quality_profile_items(profile_id,media_type,quality_id,position,allowed)VALUES(1,'tv',3,0,1),(2,'movies',3,0,1);INSERT INTO quality_profile_policies(profile_id,media_type,upgrade_allowed,cutoff_quality_id,min_format_score,cutoff_format_score,min_upgrade_format_score,language_id)VALUES(1,'tv',1,3,0,0,1,NULL),(2,'movies',1,3,0,0,1,-2);INSERT INTO library_settings(media_type,series_id,quality_profile_id,series_type,use_scene_numbering)VALUES('tv',1,1,'standard',0);INSERT INTO library_settings(media_type,movie_id,quality_profile_id,minimum_availability)VALUES('movies',1,2,'released');").await.unwrap();
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
}
