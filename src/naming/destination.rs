//! Resolves the automated owned-import destination: the naming-template render when a
//! format is configured and enabled for the target's domain/series-type, otherwise today's
//! byte-identical basename preservation. Renders the filename component only; the parent
//! directory stays `root` unchanged (no folder-format rendering, no directory creation --
//! see docs/naming-api.md).
use super::render::{
    self, EpisodeNamingFacts, MovieNamingFacts, RenderConfig, RenderFacts, TemplateField,
};
use crate::db::MediaTarget;
use libsql::{Connection, params};

#[derive(Debug)]
pub(crate) enum DestinationError {
    /// Storage failure; the caller applies its own module-local storage-error mapping.
    Db(libsql::Error),
    /// An invariant that should never break in practice (missing naming_settings row,
    /// corrupt stored enum, a target that vanished mid-lookup, or a non-UTF8 destination
    /// path) rather than a per-item content problem.
    Internal(&'static str),
    /// The configured template failed to parse or render against the current facts. The
    /// caller must surface this to the operator rather than falling back silently.
    Render(RenderFailure),
}
/// Only allowlisted field/phase identifiers may cross the process-log boundary.
#[derive(Debug)]
pub(crate) struct RenderFailure {
    pub field: &'static str,
    pub class: &'static str,
}
impl From<libsql::Error> for DestinationError {
    fn from(error: libsql::Error) -> Self {
        Self::Db(error)
    }
}
fn internal(reason: &'static str) -> DestinationError {
    eprintln!(
        "{}",
        serde_json::json!({"level":"ERROR","event":"naming_destination_internal_error","condition":reason})
    );
    DestinationError::Internal(reason)
}

async fn quality_title(
    c: &Connection,
    media: &str,
    quality_id: i64,
) -> Result<String, DestinationError> {
    Ok(c.query(
        "SELECT title FROM quality_definitions WHERE media_type=? AND quality_id=?",
        params![media, quality_id],
    )
    .await?
    .next()
    .await?
    .ok_or_else(|| internal("quality_definition_missing"))?
    .get(0)?)
}
fn preserved(root: &str, basename: &str) -> Result<String, DestinationError> {
    std::path::Path::new(root)
        .join(basename)
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| internal("non_utf8_destination"))
}
/// Renders one filename-format template and joins it with the downloaded file's own
/// extension, re-capping the rendered stem so `stem.ext` never exceeds 255 bytes (NAME_MAX)
/// even though the renderer's own 255-byte cap only accounts for the stem itself.
fn finish_render(
    field: TemplateField,
    template: &str,
    facts: RenderFacts<'_>,
    config: &RenderConfig<'_>,
    root: &str,
    basename: &str,
) -> Result<String, DestinationError> {
    let parsed = render::parse(field, template).map_err(|_| {
        DestinationError::Render(RenderFailure {
            field: field.name(),
            class: "invalid_template",
        })
    })?;
    let stem = render::render(&parsed, facts, config).map_err(|_| {
        DestinationError::Render(RenderFailure {
            field: field.name(),
            class: "render_failed",
        })
    })?;
    let extension = basename.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
    let budget = render::MAX_COMPONENT_BYTES.saturating_sub(extension.len() + 1);
    let stem = render::cap_bytes(&stem, budget);
    std::path::Path::new(root)
        .join(format!("{stem}.{extension}"))
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| internal("non_utf8_destination"))
}

// The same exact-input score cache guards the transaction without compiling regexes
// under its writer lock. Empty score weights deliberately make naming profile-independent.
async fn matching_names(
    c: &Connection,
    target: &MediaTarget,
    field: TemplateField,
    template: &str,
    evidence: &crate::custom_formats::Evidence,
) -> Result<Vec<String>, DestinationError> {
    let fail = || {
        DestinationError::Render(RenderFailure {
            field: field.name(),
            class: "custom_format_evaluation_failed",
        })
    };
    let parsed = render::parse(field, template).map_err(|_| {
        DestinationError::Render(RenderFailure {
            field: field.name(),
            class: "invalid_template",
        })
    })?;
    if !parsed.uses_custom_formats() {
        return Ok(Vec::new());
    }
    let media = match target {
        MediaTarget::Episode(_) => crate::api::MediaDomain::Tv,
        MediaTarget::Movie(_) => crate::api::MediaDomain::Movies,
    };
    let formats = crate::custom_formats::catalog(c, media)
        .await
        .map_err(|_| fail())?;
    let original = crate::search::downloaded::original_language(c, target)
        .await
        .map_err(|_| fail())?;
    let score = crate::custom_formats::score(
        &formats,
        media,
        evidence.clone(),
        original,
        Vec::new(),
        !c.is_autocommit(),
    )
    .await
    .map_err(|_| fail())?;
    let mut names = formats
        .into_iter()
        .filter(|f| f.definition.include_when_renaming && score.format_ids.contains(&f.id))
        .map(|f| f.definition.name)
        .collect::<Vec<_>>();
    names.sort();
    Ok(names)
}

/// `rename_enabled=0`, or a `NULL` format for the target's domain/series-type, both fall
/// back to `root.join(basename)` -- today's exact behavior. Never invents a default template.
pub(crate) async fn resolve_owned_destination(
    c: &Connection,
    target: &MediaTarget,
    root: &str,
    basename: &str,
    quality_id: i64,
    revision: &crate::media_files::FileRevision,
    edition: Option<&str>,
    evidence: &crate::custom_formats::Evidence,
) -> Result<String, DestinationError> {
    let domain = match target {
        MediaTarget::Episode(_) => "tv",
        MediaTarget::Movie(_) => "movies",
    };
    let settings = c
        .query(
            "SELECT rename_enabled,replace_illegal_characters,colon_replacement,custom_colon_replacement,standard_episode_format,daily_episode_format,anime_episode_format,standard_movie_format FROM naming_settings WHERE domain=?",
            [domain],
        )
        .await?
        .next()
        .await?
        .ok_or_else(|| internal("naming_settings_missing"))?;
    if settings.get::<i64>(0)? == 0 {
        return preserved(root, basename);
    }
    let replace_illegal_characters = settings.get::<i64>(1)? == 1;
    let colon_replacement: String = settings.get(2)?;
    let custom_colon_replacement: Option<String> = settings.get(3)?;
    let colon = super::colon_from_db(&colon_replacement)
        .map_err(|_| internal("invalid_stored_naming_settings"))?;
    let config = RenderConfig {
        replace_illegal_characters,
        colon: super::colon_policy(colon, custom_colon_replacement.as_deref()),
    };
    match target {
        MediaTarget::Episode(id) => {
            let row = c
                .query(
                    "SELECT s.title,e.season,e.number,e.title,e.air_date,l.series_type,e.absolute_episode_number FROM episodes e JOIN series s ON s.id=e.series_id LEFT JOIN library_settings l ON l.series_id=s.id WHERE e.id=?",
                    [*id],
                )
                .await?
                .next()
                .await?
                .ok_or_else(|| internal("target_missing"))?;
            let series_type: Option<String> = row.get(5)?;
            let (field, template) = match series_type.as_deref() {
                Some("daily") => (
                    TemplateField::DailyEpisode,
                    settings.get::<Option<String>>(5)?,
                ),
                Some("anime") => (
                    TemplateField::AnimeEpisode,
                    settings.get::<Option<String>>(6)?,
                ),
                _ => (
                    TemplateField::StandardEpisode,
                    settings.get::<Option<String>>(4)?,
                ),
            };
            let Some(template) = template else {
                return preserved(root, basename);
            };
            let custom_formats = matching_names(c, target, field, &template, evidence).await?;
            let facts = EpisodeNamingFacts {
                custom_formats,
                series_title: row.get(0)?,
                anime: series_type.as_deref() == Some("anime"),
                season: row.get(1)?,
                episode: row.get(2)?,
                episode_title: Some(row.get(3)?),
                quality_title: quality_title(c, "tv", quality_id).await?,
                revision: Some(revision.clone()),
                air_date: row.get(4)?,
            };
            if field == TemplateField::AnimeEpisode && template.contains("{absolute:") {
                validated_basename(root, basename)?;
                let parsed = render::parse(field, &template)
                    .map_err(|_| scan_failure(field, "invalid_template"))?;
                let set = render::EpisodeSetNamingFacts::from_single(&facts, row.get(6)?);
                let stem = render::render_episode_set(&parsed, &set, None, &config)
                    .map_err(|e| set_render_failure(field, e))?;
                let filename = match basename.rsplit_once('.') {
                    Some((_, ext)) if !ext.is_empty() => format!("{stem}.{ext}"),
                    _ => stem,
                };
                validated_basename(root, &filename)?;
                return preserved(root, &filename);
            }
            finish_render(
                field,
                &template,
                RenderFacts::Episode(&facts),
                &config,
                root,
                basename,
            )
        }
        MediaTarget::Movie(id) => {
            let Some(template) = settings.get::<Option<String>>(7)? else {
                return preserved(root, basename);
            };
            let row = c
                .query(
                    "SELECT d.title,d.year FROM movies m JOIN movie_metadata d ON d.id=m.metadata_id WHERE m.id=?",
                    [*id],
                )
                .await?
                .next()
                .await?
                .ok_or_else(|| internal("target_missing"))?;
            let custom_formats =
                matching_names(c, target, TemplateField::StandardMovie, &template, evidence)
                    .await?;
            let facts = MovieNamingFacts {
                custom_formats,
                movie_title: row.get(0)?,
                release_year: row.get(1)?,
                edition: edition.map(str::to_owned),
                quality_title: quality_title(c, "movies", quality_id).await?,
                revision: Some(revision.clone()),
            };
            finish_render(
                TemplateField::StandardMovie,
                &template,
                RenderFacts::Movie(&facts),
                &config,
                root,
                basename,
            )
        }
    }
}

/// Destination and policy snapshot for immutable scan inventory. The producer must
/// compare the captured naming revision during prepare, and reserve collisions for
/// the entire published manifest; this read-only resolver does not reserve paths.
#[derive(Debug)]
pub(crate) struct EpisodeSetDestination {
    pub destination: String,
    pub naming_revision: i64,
}
fn scan_failure(field: TemplateField, class: &'static str) -> DestinationError {
    DestinationError::Render(RenderFailure {
        field: field.name(),
        class,
    })
}
fn set_render_failure(field: TemplateField, error: render::RenderError) -> DestinationError {
    scan_failure(
        field,
        match error {
            render::RenderError::NeedsPolicy => "multi_episode_style_required",
            render::RenderError::MissingMembershipToken => "membership_token_required",
            render::RenderError::MissingAbsoluteNumber => "absolute_number_required",
            render::RenderError::MissingAirDate => "air_date_required",
            render::RenderError::ComponentTooLong => "filename_too_long",
            render::RenderError::IllegalCharacter => "illegal_character",
            render::RenderError::InvalidEpisodeSet => "invalid_episode_set",
            _ => "render_failed",
        },
    )
}
fn validated_basename(root: &str, basename: &str) -> Result<(), DestinationError> {
    if !std::path::Path::new(root).is_absolute()
        || basename.is_empty()
        || basename.len() > render::MAX_COMPONENT_BYTES
        || matches!(basename, "." | "..")
        || basename
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\'))
    {
        return Err(scan_failure(
            TemplateField::StandardEpisode,
            "invalid_source_basename",
        ));
    }
    Ok(())
}
/// Complete membership, loaded in the caller's read/write transaction. The bounded
/// set must belong to one series/season. Duplicate IDs are harmless, conflicting
/// coordinates or missing rows fail. No filesystem writes or collision guesses.
pub(crate) async fn resolve_episode_set_destination(
    c: &Connection,
    episode_ids: &[i64],
    root: &str,
    basename: &str,
    quality_id: i64,
    revision: &crate::media_files::FileRevision,
    evidence: &crate::custom_formats::Evidence,
) -> Result<EpisodeSetDestination, DestinationError> {
    use render::{EpisodeMemberNamingFacts, EpisodeSetNamingFacts, MultiEpisodeStyle};
    let fail = |class| scan_failure(TemplateField::StandardEpisode, class);
    if c.is_autocommit() {
        return Err(internal("episode_set_requires_snapshot"));
    }
    validated_basename(root, basename)?;
    if episode_ids.is_empty()
        || episode_ids.len() > render::MAX_EPISODE_MEMBERS
        || episode_ids.iter().any(|id| *id <= 0)
    {
        return Err(fail("invalid_episode_set"));
    }
    let ids = episode_ids
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    let encoded = serde_json::to_string(&ids).map_err(|_| internal("episode_ids_encoding"))?;
    let mut rows=c.query("SELECT e.id,e.series_id,e.season,e.number,substr(e.title,1,4097),substr(e.air_date,1,33),e.absolute_episode_number,substr(s.title,1,4097),l.series_type FROM episodes e JOIN series s ON s.id=e.series_id LEFT JOIN library_settings l ON l.series_id=s.id WHERE e.id IN (SELECT value FROM json_each(?)) ORDER BY e.season,e.number,e.id LIMIT 20001",[encoded]).await?;
    const MAX_SET_FACT_BYTES: usize = 64 * 1024 * 1024;
    let mut fact_bytes = 0usize;
    let mut members = Vec::with_capacity(ids.len());
    let mut owner = None;
    let mut series_title = None;
    let mut series_type = None;
    while let Some(row) = rows.next().await? {
        let current: i64 = row.get(1)?;
        if owner.is_some_and(|id| id != current) {
            return Err(fail("mixed_series"));
        }
        owner = Some(current);
        series_title = Some(row.get::<String>(7)?);
        series_type = row.get::<Option<String>>(8)?;
        let member = EpisodeMemberNamingFacts {
            episode_id: row.get(0)?,
            season: row.get(2)?,
            episode: row.get(3)?,
            title: row.get(4)?,
            air_date: row.get(5)?,
            absolute_episode_number: row.get(6)?,
        };
        fact_bytes = fact_bytes
            .saturating_add(std::mem::size_of::<EpisodeMemberNamingFacts>())
            .saturating_add(member.title.as_ref().map_or(0, String::len))
            .saturating_add(member.air_date.as_ref().map_or(0, String::len));
        if fact_bytes > MAX_SET_FACT_BYTES
            || member.title.as_ref().is_some_and(|s| s.len() > 4096)
            || member.air_date.as_ref().is_some_and(|s| s.len() > 32)
            || series_title.as_ref().is_some_and(|s| s.len() > 4096)
        {
            return Err(fail("naming_facts_too_large"));
        }
        members.push(member);
    }
    if members.len() != ids.len() {
        return Err(fail("target_missing"));
    }
    if members
        .windows(2)
        .any(|w| w[0].season != w[1].season || w[0].episode == w[1].episode)
    {
        return Err(fail("invalid_episode_set"));
    }
    let settings=c.query("SELECT revision,rename_enabled,replace_illegal_characters,colon_replacement,custom_colon_replacement,standard_episode_format,daily_episode_format,anime_episode_format,multi_episode_style FROM naming_settings WHERE domain='tv'",()).await?.next().await?.ok_or_else(||internal("naming_settings_missing"))?;
    let naming_revision = settings.get(0)?;
    let (field, column) = match series_type.as_deref() {
        Some("daily") => (TemplateField::DailyEpisode, 6),
        Some("anime") => (TemplateField::AnimeEpisode, 7),
        _ => (TemplateField::StandardEpisode, 5),
    };
    let template: Option<String> = settings.get(column)?;
    if settings.get::<i64>(1)? == 0 || template.is_none() {
        return Ok(EpisodeSetDestination {
            destination: preserved(root, basename)?,
            naming_revision,
        });
    }
    let template = template.unwrap();
    let style = settings
        .get::<Option<i64>>(8)?
        .map(MultiEpisodeStyle::try_from)
        .transpose()
        .map_err(|_| internal("invalid_stored_naming_style"))?;
    let colon: String = settings.get(3)?;
    let custom: Option<String> = settings.get(4)?;
    let config = RenderConfig {
        replace_illegal_characters: settings.get::<i64>(2)? == 1,
        colon: super::colon_policy(
            super::colon_from_db(&colon).map_err(|_| internal("invalid_stored_naming_settings"))?,
            custom.as_deref(),
        ),
    };
    // Original-language/custom-format facts are series-scoped; only after proving
    // the common owner may an ID be used for that lookup. It never selects naming membership.
    let custom_formats = matching_names(
        c,
        &MediaTarget::Episode(members[0].episode_id),
        field,
        &template,
        evidence,
    )
    .await?;
    let facts = EpisodeSetNamingFacts {
        series_title: series_title.ok_or_else(|| fail("target_missing"))?,
        anime: series_type.as_deref() == Some("anime"),
        members,
        quality_title: quality_title(c, "tv", quality_id).await?,
        revision: Some(revision.clone()),
        custom_formats,
    };
    let parsed =
        render::parse(field, &template).map_err(|_| scan_failure(field, "invalid_template"))?;
    let stem = render::render_episode_set(&parsed, &facts, style, &config)
        .map_err(|e| set_render_failure(field, e))?;
    let filename = match basename.rsplit_once('.') {
        Some((_, ext)) if !ext.is_empty() => format!("{stem}.{ext}"),
        _ => stem,
    };
    validated_basename(root, &filename).map_err(|_| scan_failure(field, "filename_too_long"))?;
    Ok(EpisodeSetDestination {
        destination: preserved(root, &filename)?,
        naming_revision,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use render::ColonPolicy;

    fn config() -> RenderConfig<'static> {
        RenderConfig {
            replace_illegal_characters: true,
            colon: ColonPolicy::Dash,
        }
    }

    #[tokio::test]
    async fn complete_set_destination_reads_real_members_and_preserves_unconfigured_basename() {
        struct Scratch(std::path::PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let scratch = Scratch(
            std::env::temp_dir().join(format!("hrrdarr-set-naming-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir(&scratch.0).unwrap();
        let db = crate::db::Database::open_local(scratch.0.join("db"))
            .await
            .unwrap();
        let c = db.connect().await.unwrap();
        c.execute("INSERT INTO series(id,title,path) VALUES(1,'Halcyon Vale','/library/Show'),(2,'Other','/library/Other')",()).await.unwrap();
        c.execute(
            "INSERT INTO seasons(series_id,number) VALUES(1,3),(2,3)",
            (),
        )
        .await
        .unwrap();
        c.execute("INSERT INTO episodes(id,series_id,season,number,title,absolute_episode_number) VALUES(7,1,3,7,'Part One',47),(8,1,3,8,'Part Two',48),(10,1,3,10,'Part Four',50),(20,2,3,7,'Other',7)",()).await.unwrap();
        let evidence = crate::custom_formats::Evidence {
            version: 1,
            revision: None,
            title: "release".into(),
            filename: None,
            languages: None,
            release_group: None,
            indexer_flags: None,
            release_type: None,
            size: None,
            year: None,
            edition: None,
            source: 0,
            resolution: 0,
            modifier: 0,
        };
        let revision = crate::media_files::FileRevision {
            version: 1,
            real: 0,
            is_repack: false,
        };
        assert!(
            resolve_episode_set_destination(
                &c,
                &[7, 8],
                "/library/Show",
                "complete.mkv",
                0,
                &revision,
                &evidence
            )
            .await
            .is_err()
        );
        let tx = c.transaction().await.unwrap();
        let original = resolve_episode_set_destination(
            &tx,
            &[8, 7, 7],
            "/library/Show",
            "complete.mkv",
            0,
            &revision,
            &evidence,
        )
        .await
        .unwrap();
        assert_eq!(original.destination, "/library/Show/complete.mkv");
        assert_eq!(original.naming_revision, 1);
        assert!(
            resolve_episode_set_destination(
                &tx,
                &[7, 20],
                "/library/Show",
                "complete.mkv",
                0,
                &revision,
                &evidence
            )
            .await
            .is_err()
        );
        assert!(
            resolve_episode_set_destination(
                &tx,
                &[7, 99],
                "/library/Show",
                "complete.mkv",
                0,
                &revision,
                &evidence
            )
            .await
            .is_err()
        );
        assert!(
            resolve_episode_set_destination(
                &tx,
                &[7, 8],
                "/library/Show",
                "../escape.mkv",
                0,
                &revision,
                &evidence
            )
            .await
            .is_err()
        );
        tx.rollback().await.unwrap();
        c.execute("UPDATE naming_settings SET rename_enabled=1,standard_episode_format='S{season:00}E{episode:00}',revision=revision+1 WHERE domain='tv'",()).await.unwrap();
        let tx = c.transaction().await.unwrap();
        let error = resolve_episode_set_destination(
            &tx,
            &[7, 8],
            "/library/Show",
            "complete.mkv",
            0,
            &revision,
            &evidence,
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            DestinationError::Render(RenderFailure {
                class: "multi_episode_style_required",
                ..
            })
        ));
        tx.rollback().await.unwrap();
        c.execute("UPDATE naming_settings SET multi_episode_style=5,revision=revision+1 WHERE domain='tv'",()).await.unwrap();
        let tx = c.transaction().await.unwrap();
        let named = resolve_episode_set_destination(
            &tx,
            &[10, 8, 7],
            "/library/Show",
            "complete.mkv",
            0,
            &revision,
            &evidence,
        )
        .await
        .unwrap();
        assert_eq!(named.destination, "/library/Show/S03E07-E08+E10.mkv");
        assert_eq!(named.naming_revision, 3);
        tx.rollback().await.unwrap();
        c.execute("UPDATE naming_settings SET standard_episode_format=NULL,revision=revision+1 WHERE domain='tv'",()).await.unwrap();
        let tx = c.transaction().await.unwrap();
        let original = resolve_episode_set_destination(
            &tx,
            &[7, 8],
            "/library/Show",
            "complete.mkv",
            0,
            &revision,
            &evidence,
        )
        .await
        .unwrap();
        assert_eq!(original.destination, "/library/Show/complete.mkv");
        assert_eq!(original.naming_revision, 4);
        tx.rollback().await.unwrap();
    }
    #[test]
    fn short_stem_is_untouched_and_joined_with_the_downloaded_files_own_extension() {
        let facts = EpisodeNamingFacts {
            series_title: "Halcyon Vale".into(),
            anime: false,
            season: 3,
            episode: 7,
            episode_title: Some("The Long Dark".into()),
            quality_title: "WEBDL-1080p".into(),
            revision: None,
            custom_formats: vec![],
            air_date: None,
        };
        let destination = finish_render(
            TemplateField::StandardEpisode,
            "{Series Title} - S{season:00}E{episode:00}",
            RenderFacts::Episode(&facts),
            &config(),
            "/library/Show",
            "release.name.mkv",
        )
        .unwrap();
        assert_eq!(destination, "/library/Show/Halcyon Vale - S03E07.mkv");
    }

    #[test]
    fn a_very_long_rendered_stem_is_capped_to_leave_room_for_the_extension() {
        let facts = EpisodeNamingFacts {
            series_title: "x".repeat(500),
            anime: false,
            season: 1,
            episode: 1,
            episode_title: None,
            quality_title: "WEBDL-1080p".into(),
            revision: None,
            custom_formats: vec![],
            air_date: None,
        };
        let destination = finish_render(
            TemplateField::StandardEpisode,
            "{Series Title}",
            RenderFacts::Episode(&facts),
            &config(),
            "/library/Show",
            "source.mkv",
        )
        .unwrap();
        let filename = std::path::Path::new(&destination)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap();
        // 255 (NAME_MAX) total, minus ".mkv" (4 bytes), minus the joining dot (1 byte).
        assert_eq!(filename.len(), 255);
        assert!(filename.ends_with(".mkv"));
        let mut facts = facts;
        facts.custom_formats = vec!["é".repeat(100), "ü".repeat(100)];
        let destination = finish_render(
            TemplateField::StandardEpisode,
            "{Custom Formats}",
            RenderFacts::Episode(&facts),
            &config(),
            "/library/Show",
            "source.mkv",
        )
        .unwrap();
        let filename = std::path::Path::new(&destination)
            .file_name()
            .unwrap()
            .to_str()
            .unwrap();
        assert!(filename.len() <= render::MAX_COMPONENT_BYTES);
        assert!(filename.ends_with(".mkv"));
    }

    #[test]
    fn a_template_that_renders_empty_is_a_render_error_not_a_silent_fallback() {
        let facts = EpisodeNamingFacts {
            series_title: "Halcyon Vale".into(),
            anime: false,
            season: 1,
            episode: 1,
            episode_title: None,
            quality_title: "WEBDL-1080p".into(),
            revision: None,
            custom_formats: vec![],
            air_date: None,
        };
        let err = finish_render(
            TemplateField::StandardEpisode,
            "{Episode Title}",
            RenderFacts::Episode(&facts),
            &config(),
            "/library/Show",
            "release.mkv",
        )
        .unwrap_err();
        assert!(
            matches!(
                err,
                DestinationError::Render(RenderFailure {
                    field: "standard_episode_format",
                    class: "render_failed"
                })
            ),
            "{err:?}"
        );
    }
}
