//! Native `config/naming` API: domain-scoped naming singletons plus a live preview
//! endpoint. Persists to the `naming_settings` table (migration 0029). Not wired
//! into any automated import path yet; see docs/naming-api.md.
pub mod render;
use crate::db::Database;
use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, Query, State,
        rejection::{JsonRejection, QueryRejection},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use libsql::{Connection, params};
use render::{RenderConfig, RenderFacts, TemplateField};
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, sync::Arc};
use uuid::Uuid;

#[derive(Debug)]
struct Error {
    status: StatusCode,
    code: &'static str,
    message: Cow<'static, str>,
}
type Result<T> = std::result::Result<T, Error>;
impl Error {
    fn bad(code: &'static str, message: impl Into<Cow<'static, str>>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code,
            message: message.into(),
        }
    }
    fn internal(code: &'static str, message: impl Into<Cow<'static, str>>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code,
            message: message.into(),
        }
    }
}
fn bad(message: &'static str) -> Error {
    Error::bad("invalid_naming_config", message)
}
fn conflict() -> Error {
    Error {
        status: StatusCode::CONFLICT,
        code: "naming_revision_conflict",
        message: Cow::Borrowed("Naming settings revision conflicts with the stored value"),
    }
}
fn corrupt() -> Error {
    Error::internal(
        "invalid_stored_naming_settings",
        "Stored naming settings are invalid",
    )
}
impl From<libsql::Error> for Error {
    fn from(error: libsql::Error) -> Self {
        eprintln!(
            "{}",
            serde_json::json!({"level":"ERROR","event":"naming_database_error","correlation_id":Uuid::new_v4(),"error_class":format!("{:?}",std::mem::discriminant(&error))})
        );
        Error::internal(
            "naming_database_error",
            "Naming settings operation failed; no partial write committed",
        )
    }
}
#[derive(Debug, Serialize, ts_rs::TS)]
struct NamingErrorDetail {
    code: &'static str,
    message: String,
}
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct NamingErrorEnvelope {
    error: NamingErrorDetail,
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(NamingErrorEnvelope {
                error: NamingErrorDetail {
                    code: self.code,
                    message: self.message.into_owned(),
                },
            }),
        )
            .into_response()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ColonReplacement {
    Delete,
    Dash,
    SpaceDash,
    SpaceDashSpace,
    Smart,
    Custom,
}
fn colon_from_db(value: &str) -> Result<ColonReplacement> {
    Ok(match value {
        "delete" => ColonReplacement::Delete,
        "dash" => ColonReplacement::Dash,
        "space_dash" => ColonReplacement::SpaceDash,
        "space_dash_space" => ColonReplacement::SpaceDashSpace,
        "smart" => ColonReplacement::Smart,
        "custom" => ColonReplacement::Custom,
        _ => return Err(corrupt()),
    })
}
fn colon_to_db(value: ColonReplacement) -> &'static str {
    match value {
        ColonReplacement::Delete => "delete",
        ColonReplacement::Dash => "dash",
        ColonReplacement::SpaceDash => "space_dash",
        ColonReplacement::SpaceDashSpace => "space_dash_space",
        ColonReplacement::Smart => "smart",
        ColonReplacement::Custom => "custom",
    }
}
fn colon_policy(value: ColonReplacement, custom: Option<&str>) -> render::ColonPolicy<'_> {
    match value {
        ColonReplacement::Delete => render::ColonPolicy::Delete,
        ColonReplacement::Dash => render::ColonPolicy::Dash,
        ColonReplacement::SpaceDash => render::ColonPolicy::SpaceDash,
        ColonReplacement::SpaceDashSpace => render::ColonPolicy::SpaceDashSpace,
        ColonReplacement::Smart => render::ColonPolicy::Smart,
        ColonReplacement::Custom => render::ColonPolicy::Custom(custom.unwrap_or("")),
    }
}
/// `custom_colon_replacement` present iff `colon_replacement=custom`; when present it
/// must itself be safe to splice into a path component (short, no separators/control
/// characters, and not a literal colon, which would defeat the point of replacing one).
fn validate_common(colon: ColonReplacement, custom: Option<&str>) -> Result<()> {
    match (colon, custom) {
        (ColonReplacement::Custom, None) => Err(bad(
            "custom_colon_replacement is required when colon_replacement is custom",
        )),
        (ColonReplacement::Custom, Some(text)) => {
            if text.is_empty()
                || text.len() > 16
                || text
                    .chars()
                    .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
            {
                return Err(bad(
                    "custom_colon_replacement must be 1 to 16 bytes without control characters, '/', '\\', or ':'",
                ));
            }
            Ok(())
        }
        (_, Some(_)) => Err(bad(
            "custom_colon_replacement is only valid when colon_replacement is custom",
        )),
        (_, None) => Ok(()),
    }
}
fn validate_multi_episode_style(value: Option<i64>) -> Result<()> {
    if value.is_some_and(|v| !(0..=5).contains(&v)) {
        return Err(bad("multi_episode_style must be 0 to 5"));
    }
    Ok(())
}
fn template_error(field: TemplateField, error: render::TemplateError) -> Error {
    use render::TemplateError as E;
    let name = field.name();
    match error {
        E::Empty => Error::bad(
            "invalid_naming_template",
            format!("{name} must not be an empty string; use null to unset"),
        ),
        E::TooLong => Error::bad(
            "invalid_naming_template",
            format!("{name} exceeds the maximum template length"),
        ),
        E::UnbalancedBraces => Error::bad(
            "invalid_naming_template",
            format!("{name} has unbalanced {{}} braces"),
        ),
        E::UnknownToken(token) => Error::bad(
            "unknown_naming_token",
            format!("{name} contains unknown token {token}"),
        ),
        E::WrongDomain(token) => Error::bad(
            "cross_domain_naming_token",
            format!("{name} contains token {token} from another media domain"),
        ),
        E::NotAllowedInField(token) => Error::bad(
            "naming_token_not_allowed_in_field",
            format!("{name} does not allow token {token}"),
        ),
        E::InvalidPadding(token) => Error::bad(
            "invalid_naming_template",
            format!("{name} has an invalid zero-padding count in {token}; use 1 to 4 zeros"),
        ),
        E::PathSeparatorInFilename => Error::bad(
            "invalid_naming_template",
            format!("{name} is a filename format and must not contain '/'"),
        ),
        E::EmptyPathComponent => Error::bad(
            "invalid_naming_template",
            format!("{name} produces an empty path component"),
        ),
        E::IllegalLiteralCharacter => Error::bad(
            "invalid_naming_template",
            format!("{name} literal text contains a control character"),
        ),
    }
}
fn validate_template(field: TemplateField, value: &Option<String>) -> Result<()> {
    if let Some(template) = value {
        render::parse(field, template).map_err(|e| template_error(field, e))?;
    }
    Ok(())
}
fn render_error(error: render::RenderError) -> Error {
    use render::RenderError as E;
    match error {
        E::DomainMismatch => Error::internal(
            "naming_render_domain_mismatch",
            "Naming renderer received facts for the wrong media domain",
        ),
        E::IllegalCharacter => Error::bad(
            "invalid_naming_examples",
            "Example rendering hit a character that replace_illegal_characters=false does not allow",
        ),
        E::EmptyComponent => Error::bad(
            "invalid_naming_examples",
            "Example rendering produced an empty path component",
        ),
    }
}
fn render_field(
    field: TemplateField,
    template: Option<&str>,
    facts: RenderFacts<'_>,
    config: &RenderConfig<'_>,
) -> Result<Option<String>> {
    let Some(template) = template else {
        return Ok(None);
    };
    let parsed = render::parse(field, template).map_err(|e| template_error(field, e))?;
    let rendered = render::render(&parsed, facts, config).map_err(render_error)?;
    Ok(Some(rendered))
}
/// A representative single episode, used only to render `/examples` previews.
fn example_episode_facts() -> render::EpisodeNamingFacts {
    render::EpisodeNamingFacts {
        series_title: "Halcyon Vale".into(),
        season: 3,
        episode: 7,
        episode_title: Some("The Long Dark".into()),
        quality_title: "WEBDL-1080p".into(),
        air_date: Some("2024-05-14".into()),
    }
}
/// A representative single movie, used only to render `/examples` previews.
fn example_movie_facts() -> render::MovieNamingFacts {
    render::MovieNamingFacts {
        movie_title: "The Wandering Harbor".into(),
        release_year: Some(2023),
        edition: Some("Director's Cut".into()),
        quality_title: "Bluray-1080p".into(),
    }
}

fn stored_bool(value: i64) -> Result<bool> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(corrupt()),
    }
}
struct Row {
    revision: i64,
    rename_enabled: bool,
    replace_illegal_characters: bool,
    colon_replacement: String,
    custom_colon_replacement: Option<String>,
    standard_episode_format: Option<String>,
    daily_episode_format: Option<String>,
    anime_episode_format: Option<String>,
    series_folder_format: Option<String>,
    season_folder_format: Option<String>,
    specials_folder_format: Option<String>,
    multi_episode_style: Option<i64>,
    standard_movie_format: Option<String>,
    movie_folder_format: Option<String>,
}
async fn fetch(conn: &Connection, domain: &'static str) -> Result<Row> {
    let row = conn
        .query(
            "SELECT revision,rename_enabled,replace_illegal_characters,colon_replacement,custom_colon_replacement,standard_episode_format,daily_episode_format,anime_episode_format,series_folder_format,season_folder_format,specials_folder_format,multi_episode_style,standard_movie_format,movie_folder_format FROM naming_settings WHERE domain=?",
            [domain],
        )
        .await?
        .next()
        .await?
        .ok_or_else(corrupt)?;
    Ok(Row {
        revision: row.get(0)?,
        rename_enabled: stored_bool(row.get(1)?)?,
        replace_illegal_characters: stored_bool(row.get(2)?)?,
        colon_replacement: row.get(3)?,
        custom_colon_replacement: row.get(4)?,
        standard_episode_format: row.get(5)?,
        daily_episode_format: row.get(6)?,
        anime_episode_format: row.get(7)?,
        series_folder_format: row.get(8)?,
        season_folder_format: row.get(9)?,
        specials_folder_format: row.get(10)?,
        multi_episode_style: row.get(11)?,
        standard_movie_format: row.get(12)?,
        movie_folder_format: row.get(13)?,
    })
}
async fn finish<T>(tx: libsql::Transaction, outcome: Result<T>) -> Result<T> {
    match outcome {
        Ok(value) => {
            tx.commit().await?;
            Ok(value)
        }
        Err(error) => {
            tx.rollback().await?;
            Err(error)
        }
    }
}
fn revision_in_range(revision: i64) -> Result<()> {
    if !(1..9007199254740991).contains(&revision) {
        return Err(bad("revision out of range"));
    }
    Ok(())
}
/// A plain `Option<T>` field silently defaults to `None` when its key is entirely
/// absent from the JSON body — fine for the examples query, wrong for a full-
/// replacement PUT. Attaching a custom deserializer (even one this trivial)
/// suppresses that default, so the key must be present; its value may still be
/// `null`. Matches `providers::required_nullable`.
fn required_nullable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> std::result::Result<Option<T>, D::Error> {
    Option::<T>::deserialize(deserializer)
}

#[derive(Debug, Serialize, ts_rs::TS)]
pub struct TvNamingConfig {
    pub revision: i64,
    pub rename_enabled: bool,
    pub replace_illegal_characters: bool,
    pub colon_replacement: ColonReplacement,
    pub custom_colon_replacement: Option<String>,
    pub standard_episode_format: Option<String>,
    pub daily_episode_format: Option<String>,
    pub anime_episode_format: Option<String>,
    pub series_folder_format: Option<String>,
    pub season_folder_format: Option<String>,
    pub specials_folder_format: Option<String>,
    pub multi_episode_style: Option<i64>,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct TvNamingUpdate {
    pub revision: i64,
    pub rename_enabled: bool,
    pub replace_illegal_characters: bool,
    pub colon_replacement: ColonReplacement,
    #[serde(deserialize_with = "required_nullable")]
    pub custom_colon_replacement: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub standard_episode_format: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub daily_episode_format: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub anime_episode_format: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub series_folder_format: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub season_folder_format: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub specials_folder_format: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub multi_episode_style: Option<i64>,
}
fn to_tv_resource(row: Row) -> Result<TvNamingConfig> {
    Ok(TvNamingConfig {
        revision: row.revision,
        rename_enabled: row.rename_enabled,
        replace_illegal_characters: row.replace_illegal_characters,
        colon_replacement: colon_from_db(&row.colon_replacement)?,
        custom_colon_replacement: row.custom_colon_replacement,
        standard_episode_format: row.standard_episode_format,
        daily_episode_format: row.daily_episode_format,
        anime_episode_format: row.anime_episode_format,
        series_folder_format: row.series_folder_format,
        season_folder_format: row.season_folder_format,
        specials_folder_format: row.specials_folder_format,
        multi_episode_style: row.multi_episode_style,
    })
}
fn validate_tv(input: &TvNamingUpdate) -> Result<()> {
    revision_in_range(input.revision)?;
    validate_common(
        input.colon_replacement,
        input.custom_colon_replacement.as_deref(),
    )?;
    validate_multi_episode_style(input.multi_episode_style)?;
    validate_template(
        TemplateField::StandardEpisode,
        &input.standard_episode_format,
    )?;
    validate_template(TemplateField::DailyEpisode, &input.daily_episode_format)?;
    validate_template(TemplateField::AnimeEpisode, &input.anime_episode_format)?;
    validate_template(TemplateField::SeriesFolder, &input.series_folder_format)?;
    validate_template(TemplateField::SeasonFolder, &input.season_folder_format)?;
    validate_template(TemplateField::SpecialsFolder, &input.specials_folder_format)?;
    Ok(())
}

#[derive(Debug, Serialize, ts_rs::TS)]
pub struct MovieNamingConfig {
    pub revision: i64,
    pub rename_enabled: bool,
    pub replace_illegal_characters: bool,
    pub colon_replacement: ColonReplacement,
    pub custom_colon_replacement: Option<String>,
    pub standard_movie_format: Option<String>,
    pub movie_folder_format: Option<String>,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct MovieNamingUpdate {
    pub revision: i64,
    pub rename_enabled: bool,
    pub replace_illegal_characters: bool,
    pub colon_replacement: ColonReplacement,
    #[serde(deserialize_with = "required_nullable")]
    pub custom_colon_replacement: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub standard_movie_format: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub movie_folder_format: Option<String>,
}
fn to_movie_resource(row: Row) -> Result<MovieNamingConfig> {
    Ok(MovieNamingConfig {
        revision: row.revision,
        rename_enabled: row.rename_enabled,
        replace_illegal_characters: row.replace_illegal_characters,
        colon_replacement: colon_from_db(&row.colon_replacement)?,
        custom_colon_replacement: row.custom_colon_replacement,
        standard_movie_format: row.standard_movie_format,
        movie_folder_format: row.movie_folder_format,
    })
}
fn validate_movies(input: &MovieNamingUpdate) -> Result<()> {
    revision_in_range(input.revision)?;
    validate_common(
        input.colon_replacement,
        input.custom_colon_replacement.as_deref(),
    )?;
    validate_template(TemplateField::StandardMovie, &input.standard_movie_format)?;
    validate_template(TemplateField::MovieFolder, &input.movie_folder_format)?;
    Ok(())
}

#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields)]
pub struct TvNamingExamplesQuery {
    pub rename_enabled: Option<bool>,
    pub replace_illegal_characters: Option<bool>,
    pub colon_replacement: Option<ColonReplacement>,
    pub custom_colon_replacement: Option<String>,
    pub standard_episode_format: Option<String>,
    pub daily_episode_format: Option<String>,
    pub anime_episode_format: Option<String>,
    pub series_folder_format: Option<String>,
    pub season_folder_format: Option<String>,
    pub specials_folder_format: Option<String>,
}
#[derive(Serialize, ts_rs::TS)]
pub struct TvNamingExamples {
    pub standard_episode_format: Option<String>,
    pub daily_episode_format: Option<String>,
    pub anime_episode_format: Option<String>,
    pub series_folder_format: Option<String>,
    pub season_folder_format: Option<String>,
    pub specials_folder_format: Option<String>,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields)]
pub struct MovieNamingExamplesQuery {
    pub rename_enabled: Option<bool>,
    pub replace_illegal_characters: Option<bool>,
    pub colon_replacement: Option<ColonReplacement>,
    pub custom_colon_replacement: Option<String>,
    pub standard_movie_format: Option<String>,
    pub movie_folder_format: Option<String>,
}
#[derive(Serialize, ts_rs::TS)]
pub struct MovieNamingExamples {
    pub standard_movie_format: Option<String>,
    pub movie_folder_format: Option<String>,
}

#[derive(Clone)]
struct Context {
    db: Arc<Database>,
}
pub fn router(db: Arc<Database>) -> Router {
    let tv = Router::new()
        .route("/api/v1/tv/config/naming", get(get_tv).put(put_tv))
        .route("/api/v1/tv/config/naming/examples", get(examples_tv))
        .with_state(Context { db: db.clone() });
    let movies = Router::new()
        .route(
            "/api/v1/movies/config/naming",
            get(get_movies).put(put_movies),
        )
        .route(
            "/api/v1/movies/config/naming/examples",
            get(examples_movies),
        )
        .with_state(Context { db });
    tv.merge(movies).layer(DefaultBodyLimit::max(32 * 1024))
}

async fn get_tv(State(ctx): State<Context>) -> Result<Json<TvNamingConfig>> {
    let conn = ctx.db.connect().await?;
    Ok(Json(to_tv_resource(fetch(&conn, "tv").await?)?))
}
async fn put_tv(
    State(ctx): State<Context>,
    input: std::result::Result<Json<TvNamingUpdate>, JsonRejection>,
) -> Result<Json<TvNamingConfig>> {
    let input = input
        .map_err(|_| bad("Expected documented TV naming JSON fields and types"))?
        .0;
    validate_tv(&input)?;
    let conn = ctx.db.connect().await?;
    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        let changed = tx
            .execute(
                "UPDATE naming_settings SET rename_enabled=?,replace_illegal_characters=?,colon_replacement=?,custom_colon_replacement=?,standard_episode_format=?,daily_episode_format=?,anime_episode_format=?,series_folder_format=?,season_folder_format=?,specials_folder_format=?,multi_episode_style=?,revision=revision+1 WHERE domain='tv' AND revision=?",
                params![
                    i64::from(input.rename_enabled),
                    i64::from(input.replace_illegal_characters),
                    colon_to_db(input.colon_replacement),
                    input.custom_colon_replacement.clone(),
                    input.standard_episode_format.clone(),
                    input.daily_episode_format.clone(),
                    input.anime_episode_format.clone(),
                    input.series_folder_format.clone(),
                    input.season_folder_format.clone(),
                    input.specials_folder_format.clone(),
                    input.multi_episode_style,
                    input.revision
                ],
            )
            .await?;
        if changed != 1 {
            return Err(conflict());
        }
        to_tv_resource(fetch(&tx, "tv").await?)
    }
    .await;
    finish(tx, outcome).await.map(Json)
}
async fn examples_tv(
    State(ctx): State<Context>,
    query: std::result::Result<Query<TvNamingExamplesQuery>, QueryRejection>,
) -> Result<Json<TvNamingExamples>> {
    let query = query
        .map_err(|_| bad("Invalid TV naming examples query"))?
        .0;
    let conn = ctx.db.connect().await?;
    let row = fetch(&conn, "tv").await?;
    let replace_illegal_characters = query
        .replace_illegal_characters
        .unwrap_or(row.replace_illegal_characters);
    let colon = query
        .colon_replacement
        .unwrap_or(colon_from_db(&row.colon_replacement)?);
    // The stored custom text only carries over when the *effective* (possibly
    // overridden) mode is still Custom; otherwise a query that previews a
    // different mode would spuriously fail validate_common's "only valid when
    // custom" check against a stored value the caller never asked to see.
    let custom = if colon == ColonReplacement::Custom {
        query
            .custom_colon_replacement
            .clone()
            .or_else(|| row.custom_colon_replacement.clone())
    } else {
        query.custom_colon_replacement.clone()
    };
    validate_common(colon, custom.as_deref())?;
    let config = RenderConfig {
        replace_illegal_characters,
        colon: colon_policy(colon, custom.as_deref()),
    };
    let facts = example_episode_facts();
    let facts = RenderFacts::Episode(&facts);
    Ok(Json(TvNamingExamples {
        standard_episode_format: render_field(
            TemplateField::StandardEpisode,
            query
                .standard_episode_format
                .as_deref()
                .or(row.standard_episode_format.as_deref()),
            facts,
            &config,
        )?,
        daily_episode_format: render_field(
            TemplateField::DailyEpisode,
            query
                .daily_episode_format
                .as_deref()
                .or(row.daily_episode_format.as_deref()),
            facts,
            &config,
        )?,
        anime_episode_format: render_field(
            TemplateField::AnimeEpisode,
            query
                .anime_episode_format
                .as_deref()
                .or(row.anime_episode_format.as_deref()),
            facts,
            &config,
        )?,
        series_folder_format: render_field(
            TemplateField::SeriesFolder,
            query
                .series_folder_format
                .as_deref()
                .or(row.series_folder_format.as_deref()),
            facts,
            &config,
        )?,
        season_folder_format: render_field(
            TemplateField::SeasonFolder,
            query
                .season_folder_format
                .as_deref()
                .or(row.season_folder_format.as_deref()),
            facts,
            &config,
        )?,
        specials_folder_format: render_field(
            TemplateField::SpecialsFolder,
            query
                .specials_folder_format
                .as_deref()
                .or(row.specials_folder_format.as_deref()),
            facts,
            &config,
        )?,
    }))
}

async fn get_movies(State(ctx): State<Context>) -> Result<Json<MovieNamingConfig>> {
    let conn = ctx.db.connect().await?;
    Ok(Json(to_movie_resource(fetch(&conn, "movies").await?)?))
}
async fn put_movies(
    State(ctx): State<Context>,
    input: std::result::Result<Json<MovieNamingUpdate>, JsonRejection>,
) -> Result<Json<MovieNamingConfig>> {
    let input = input
        .map_err(|_| bad("Expected documented movie naming JSON fields and types"))?
        .0;
    validate_movies(&input)?;
    let conn = ctx.db.connect().await?;
    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        let changed = tx
            .execute(
                "UPDATE naming_settings SET rename_enabled=?,replace_illegal_characters=?,colon_replacement=?,custom_colon_replacement=?,standard_movie_format=?,movie_folder_format=?,revision=revision+1 WHERE domain='movies' AND revision=?",
                params![
                    i64::from(input.rename_enabled),
                    i64::from(input.replace_illegal_characters),
                    colon_to_db(input.colon_replacement),
                    input.custom_colon_replacement.clone(),
                    input.standard_movie_format.clone(),
                    input.movie_folder_format.clone(),
                    input.revision
                ],
            )
            .await?;
        if changed != 1 {
            return Err(conflict());
        }
        to_movie_resource(fetch(&tx, "movies").await?)
    }
    .await;
    finish(tx, outcome).await.map(Json)
}
async fn examples_movies(
    State(ctx): State<Context>,
    query: std::result::Result<Query<MovieNamingExamplesQuery>, QueryRejection>,
) -> Result<Json<MovieNamingExamples>> {
    let query = query
        .map_err(|_| bad("Invalid movie naming examples query"))?
        .0;
    let conn = ctx.db.connect().await?;
    let row = fetch(&conn, "movies").await?;
    let replace_illegal_characters = query
        .replace_illegal_characters
        .unwrap_or(row.replace_illegal_characters);
    let colon = query
        .colon_replacement
        .unwrap_or(colon_from_db(&row.colon_replacement)?);
    let custom = if colon == ColonReplacement::Custom {
        query
            .custom_colon_replacement
            .clone()
            .or_else(|| row.custom_colon_replacement.clone())
    } else {
        query.custom_colon_replacement.clone()
    };
    validate_common(colon, custom.as_deref())?;
    let config = RenderConfig {
        replace_illegal_characters,
        colon: colon_policy(colon, custom.as_deref()),
    };
    let facts = example_movie_facts();
    let facts = RenderFacts::Movie(&facts);
    Ok(Json(MovieNamingExamples {
        standard_movie_format: render_field(
            TemplateField::StandardMovie,
            query
                .standard_movie_format
                .as_deref()
                .or(row.standard_movie_format.as_deref()),
            facts,
            &config,
        )?,
        movie_folder_format: render_field(
            TemplateField::MovieFolder,
            query
                .movie_folder_format
                .as_deref()
                .or(row.movie_folder_format.as_deref()),
            facts,
            &config,
        )?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use axum::http::Uri;
    use std::path::PathBuf;

    struct Sandbox(PathBuf);
    impl Sandbox {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("hrrdarr-naming-api-{}", Uuid::new_v4()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Sandbox {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    async fn open() -> (Sandbox, Context) {
        let sandbox = Sandbox::new();
        let db = Arc::new(Database::open_local(sandbox.0.join("db")).await.unwrap());
        (sandbox, Context { db })
    }
    fn tv_update(revision: i64) -> TvNamingUpdate {
        TvNamingUpdate {
            revision,
            rename_enabled: true,
            replace_illegal_characters: true,
            colon_replacement: ColonReplacement::Dash,
            custom_colon_replacement: None,
            standard_episode_format: Some("{Series Title}".into()),
            daily_episode_format: None,
            anime_episode_format: None,
            series_folder_format: None,
            season_folder_format: None,
            specials_folder_format: None,
            multi_episode_style: None,
        }
    }
    fn movie_update(revision: i64) -> MovieNamingUpdate {
        MovieNamingUpdate {
            revision,
            rename_enabled: true,
            replace_illegal_characters: true,
            colon_replacement: ColonReplacement::Dash,
            custom_colon_replacement: None,
            standard_movie_format: Some("{Movie Title}".into()),
            movie_folder_format: None,
        }
    }
    #[tokio::test]
    async fn tv_put_round_trips_bumps_revision_and_rejects_stale_revision() {
        let (_sandbox, ctx) = open().await;
        let updated = put_tv(State(ctx.clone()), Ok(Json(tv_update(1))))
            .await
            .unwrap()
            .0;
        assert_eq!(updated.revision, 2);
        assert!(updated.rename_enabled);
        assert_eq!(
            updated.standard_episode_format.as_deref(),
            Some("{Series Title}")
        );
        let fetched = get_tv(State(ctx.clone())).await.unwrap().0;
        assert_eq!(fetched.revision, 2);
        let stale = put_tv(State(ctx), Ok(Json(tv_update(1))))
            .await
            .unwrap_err();
        assert_eq!(stale.status, StatusCode::CONFLICT);
        assert_eq!(stale.code, "naming_revision_conflict");
    }
    #[tokio::test]
    async fn movie_put_round_trips_bumps_revision_and_rejects_stale_revision() {
        let (_sandbox, ctx) = open().await;
        let updated = put_movies(State(ctx.clone()), Ok(Json(movie_update(1))))
            .await
            .unwrap()
            .0;
        assert_eq!(updated.revision, 2);
        let fetched = get_movies(State(ctx.clone())).await.unwrap().0;
        assert_eq!(fetched.revision, 2);
        let stale = put_movies(State(ctx), Ok(Json(movie_update(1))))
            .await
            .unwrap_err();
        assert_eq!(stale.status, StatusCode::CONFLICT);
        assert_eq!(stale.code, "naming_revision_conflict");
    }
    #[tokio::test]
    async fn examples_can_preview_a_different_colon_mode_than_the_stored_custom_one() {
        let (_sandbox, ctx) = open().await;
        let mut custom = tv_update(1);
        custom.colon_replacement = ColonReplacement::Custom;
        custom.custom_colon_replacement = Some("~".into());
        let _ = put_tv(State(ctx.clone()), Ok(Json(custom))).await.unwrap();
        // Overriding away from `custom` without supplying new custom text must not
        // inherit the stored custom text and fail validation against the override.
        let query = TvNamingExamplesQuery {
            rename_enabled: None,
            replace_illegal_characters: None,
            colon_replacement: Some(ColonReplacement::Smart),
            custom_colon_replacement: None,
            standard_episode_format: None,
            daily_episode_format: None,
            anime_episode_format: None,
            series_folder_format: None,
            season_folder_format: None,
            specials_folder_format: None,
        };
        let result = examples_tv(State(ctx), Ok(Query(query))).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }
    #[test]
    fn examples_query_parses_enum_and_bool_and_rejects_wrong_domain_field() {
        let uri: Uri = "/x?colon_replacement=space_dash&rename_enabled=true"
            .parse()
            .unwrap();
        let parsed = Query::<TvNamingExamplesQuery>::try_from_uri(&uri)
            .unwrap()
            .0;
        assert_eq!(parsed.colon_replacement, Some(ColonReplacement::SpaceDash));
        assert_eq!(parsed.rename_enabled, Some(true));
        let uri: Uri = "/x?standard_movie_format=X".parse().unwrap();
        assert!(Query::<TvNamingExamplesQuery>::try_from_uri(&uri).is_err());
    }
    #[test]
    fn put_json_requires_every_format_key_present_even_when_null() {
        let base = serde_json::json!({
            "revision": 1, "rename_enabled": false, "replace_illegal_characters": true,
            "colon_replacement": "smart", "custom_colon_replacement": null,
            "standard_episode_format": null, "daily_episode_format": null,
            "anime_episode_format": null, "series_folder_format": null,
            "season_folder_format": null, "specials_folder_format": null,
            "multi_episode_style": null,
        });
        assert!(serde_json::from_value::<TvNamingUpdate>(base.clone()).is_ok());
        for key in [
            "standard_episode_format",
            "multi_episode_style",
            "custom_colon_replacement",
        ] {
            let mut missing = base.clone();
            missing.as_object_mut().unwrap().remove(key);
            assert!(
                serde_json::from_value::<TvNamingUpdate>(missing).is_err(),
                "omitting {key} must be rejected, not silently treated as null"
            );
        }
    }
    #[tokio::test]
    async fn fresh_install_matches_seeded_migration_defaults() {
        let (_sandbox, ctx) = open().await;
        let conn = ctx.db.connect().await.unwrap();
        let tv = to_tv_resource(fetch(&conn, "tv").await.unwrap()).unwrap();
        assert!(!tv.rename_enabled);
        assert!(tv.replace_illegal_characters);
        assert_eq!(tv.colon_replacement, ColonReplacement::Smart);
        assert_eq!(tv.revision, 1);
        assert!(tv.standard_episode_format.is_none());
        assert!(tv.multi_episode_style.is_none());
        let movies = to_movie_resource(fetch(&conn, "movies").await.unwrap()).unwrap();
        assert!(!movies.rename_enabled);
        assert!(movies.standard_movie_format.is_none());
        assert!(movies.movie_folder_format.is_none());
    }
    #[test]
    fn colon_replacement_round_trips_through_db_strings() {
        for value in [
            ColonReplacement::Delete,
            ColonReplacement::Dash,
            ColonReplacement::SpaceDash,
            ColonReplacement::SpaceDashSpace,
            ColonReplacement::Smart,
            ColonReplacement::Custom,
        ] {
            assert_eq!(colon_from_db(colon_to_db(value)).unwrap(), value);
        }
        assert!(colon_from_db("unsupported").is_err());
    }
    #[test]
    fn custom_colon_replacement_presence_and_content_are_validated() {
        assert!(validate_common(ColonReplacement::Custom, None).is_err());
        assert!(validate_common(ColonReplacement::Smart, Some("-")).is_err());
        assert!(validate_common(ColonReplacement::Custom, Some("")).is_err());
        assert!(validate_common(ColonReplacement::Custom, Some(":")).is_err());
        assert!(validate_common(ColonReplacement::Custom, Some("/")).is_err());
        assert!(validate_common(ColonReplacement::Custom, Some(&"x".repeat(17))).is_err());
        assert!(validate_common(ColonReplacement::Custom, Some("~")).is_ok());
        assert!(validate_common(ColonReplacement::Smart, None).is_ok());
    }
    #[test]
    fn multi_episode_style_bounds() {
        assert!(validate_multi_episode_style(None).is_ok());
        assert!(validate_multi_episode_style(Some(0)).is_ok());
        assert!(validate_multi_episode_style(Some(5)).is_ok());
        assert!(validate_multi_episode_style(Some(6)).is_err());
        assert!(validate_multi_episode_style(Some(-1)).is_err());
    }
    #[test]
    fn cross_domain_and_unknown_tokens_are_rejected_with_distinct_codes() {
        let unknown = template_error(
            TemplateField::StandardEpisode,
            render::TemplateError::UnknownToken("{Foo}".into()),
        );
        assert_eq!(unknown.code, "unknown_naming_token");
        assert!(unknown.message.contains("{Foo}"));
        assert!(unknown.message.contains("standard_episode_format"));
        let cross = template_error(
            TemplateField::StandardEpisode,
            render::TemplateError::WrongDomain("{Movie Title}".into()),
        );
        assert_eq!(cross.code, "cross_domain_naming_token");
        assert!(cross.message.contains("{Movie Title}"));
    }
}
