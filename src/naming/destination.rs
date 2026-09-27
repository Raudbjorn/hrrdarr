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
                    "SELECT s.title,e.season,e.number,e.title,e.air_date,l.series_type FROM episodes e JOIN series s ON s.id=e.series_id LEFT JOIN library_settings l ON l.series_id=s.id WHERE e.id=?",
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
                season: row.get(1)?,
                episode: row.get(2)?,
                episode_title: Some(row.get(3)?),
                quality_title: quality_title(c, "tv", quality_id).await?,
                air_date: row.get(4)?,
            };
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

    #[test]
    fn short_stem_is_untouched_and_joined_with_the_downloaded_files_own_extension() {
        let facts = EpisodeNamingFacts {
            series_title: "Halcyon Vale".into(),
            season: 3,
            episode: 7,
            episode_title: Some("The Long Dark".into()),
            quality_title: "WEBDL-1080p".into(),
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
            season: 1,
            episode: 1,
            episode_title: None,
            quality_title: "WEBDL-1080p".into(),
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
            season: 1,
            episode: 1,
            episode_title: None,
            quality_title: "WEBDL-1080p".into(),
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
