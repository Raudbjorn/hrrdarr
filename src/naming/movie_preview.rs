//! Stored-file previews, deliberately separate from import destination selection.
//! Reads a coherent snapshot, then evaluates current custom formats outside the transaction.
use super::render::{self, MovieNamingFacts, RenderConfig, RenderFacts, Template, TemplateField};
use crate::{
    api::{ApiErrorEnvelope, MediaDomain},
    custom_formats::{self, Evidence, Format},
    db::Database,
    media_files::FileRevision,
};
use axum::{
    Json, Router,
    extract::{RawQuery, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use libsql::{Connection, Row, TransactionBehavior};
use serde::Serialize;
use std::{collections::BTreeSet, sync::Arc, time::Duration};
const MAX_SAFE: i64 = 9_007_199_254_740_991;
const MAX_IDS: usize = 200;
const MAX_QUERY_BYTES: usize = 4096;
const MAX_SNAPSHOT_BYTES: usize = 32 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_PATH_BYTES: usize = 4096;
const DEADLINE_SECONDS: u64 = 8;
#[derive(Debug)]
struct Error(StatusCode, &'static str, &'static str);
type Result<T> = std::result::Result<T, Error>;
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (self.0, Json(ApiErrorEnvelope::new(self.1, self.2))).into_response()
    }
}
fn bad() -> Error {
    Error(
        StatusCode::BAD_REQUEST,
        "invalid_request",
        "Provide one bounded list of distinct positive movie IDs",
    )
}
fn storage() -> Error {
    Error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "database_error",
        "Movie filename preview could not read stored data",
    )
}
fn config_error() -> Error {
    Error(
        StatusCode::UNPROCESSABLE_ENTITY,
        "naming_configuration_invalid",
        "The enabled movie naming configuration is invalid or unconfigured",
    )
}
fn unavailable() -> Error {
    Error(
        StatusCode::SERVICE_UNAVAILABLE,
        "rename_preview_unavailable",
        "Movie filename preview exceeded its resource limit or is temporarily busy",
    )
}
impl From<libsql::Error> for Error {
    fn from(error: libsql::Error) -> Self {
        // SQLite primary result codes also cover extended BUSY/LOCKED variants.
        const SQLITE_BUSY: i32 = 5;
        const SQLITE_LOCKED: i32 = 6;
        let code = match error {
            libsql::Error::SqliteFailure(code, _)
            | libsql::Error::RemoteSqliteFailure(code, _, _) => Some(code & 0xff),
            _ => None,
        };
        if matches!(code, Some(SQLITE_BUSY | SQLITE_LOCKED)) {
            unavailable()
        } else {
            storage()
        }
    }
}
#[derive(Debug, Clone, Copy, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum MovieRenamePreviewStatus {
    Change,
    Unavailable,
}
#[derive(Debug, Clone, Copy, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum MovieRenamePreviewReason {
    InvalidPath,
    MissingExtension,
    InvalidFileFacts,
    NamingRenderFailed,
}
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct MovieRenamePreviewItem {
    pub movie_id: i64,
    pub movie_file_id: i64,
    pub existing_path: Option<String>,
    pub new_path: Option<String>,
    pub status: MovieRenamePreviewStatus,
    pub reasons: Vec<MovieRenamePreviewReason>,
}
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct MovieRenamePreview {
    pub naming_revision: i64,
    pub rename_enabled: bool,
    pub standard_movie_format: Option<String>,
    pub movie_ids: Vec<i64>,
    pub files_considered: usize,
    pub unchanged_count: usize,
    pub unavailable_count: usize,
    pub items: Vec<MovieRenamePreviewItem>,
}
pub fn router(db: Arc<Database>) -> Router {
    Router::new()
        .route("/api/v1/movies/rename-preview", get(preview))
        .with_state(db)
}
fn ids(raw: Option<String>) -> Result<Vec<i64>> {
    let raw = raw.ok_or_else(bad)?;
    if raw.len() > MAX_QUERY_BYTES || raw.split('&').count() != 1 {
        return Err(bad());
    }
    let fields = url::form_urlencoded::parse(raw.as_bytes()).collect::<Vec<_>>();
    if fields.len() != 1 || fields[0].0 != "movie_ids" {
        return Err(bad());
    }
    let mut ids = BTreeSet::new();
    for text in fields[0].1.split(',') {
        if text.is_empty() {
            return Err(bad());
        }
        let mut id = 0_i64;
        for b in text.bytes() {
            if !b.is_ascii_digit() {
                return Err(bad());
            }
            id = id
                .checked_mul(10)
                .and_then(|n| n.checked_add(i64::from(b - b'0')))
                .filter(|n| *n <= MAX_SAFE)
                .ok_or_else(bad)?;
        }
        if id == 0 || !ids.insert(id) || ids.len() > MAX_IDS {
            return Err(bad());
        }
    }
    Ok(ids.into_iter().collect())
}
struct Settings {
    revision: i64,
    enabled: bool,
    format: Option<String>,
    template: Option<Template>,
    replace: bool,
    colon: super::ColonReplacement,
    custom: Option<String>,
}
impl Settings {
    fn cf(&self) -> bool {
        self.template
            .as_ref()
            .is_some_and(Template::uses_custom_formats)
    }
    fn quality(&self) -> bool {
        self.template.as_ref().is_some_and(Template::uses_quality)
    }
    fn revision(&self) -> bool {
        self.template.as_ref().is_some_and(Template::uses_revision)
    }
    fn render_config(&self) -> RenderConfig<'_> {
        RenderConfig {
            replace_illegal_characters: self.replace,
            colon: super::colon_policy(self.colon, self.custom.as_deref()),
        }
    }
}
async fn settings(c: &Connection) -> Result<Settings> {
    let row=c.query("SELECT revision,rename_enabled,standard_movie_format,replace_illegal_characters,colon_replacement,custom_colon_replacement FROM naming_settings WHERE domain='movies'",()).await?.next().await?.ok_or_else(storage)?;
    let revision = row.get::<i64>(0)?;
    if !(0..=MAX_SAFE).contains(&revision) {
        return Err(storage());
    }
    let enabled = match row.get::<i64>(1)? {
        0 => false,
        1 => true,
        _ => return Err(storage()),
    };
    let format = row.get::<Option<String>>(2)?;
    if !enabled {
        return Ok(Settings {
            revision,
            enabled,
            format,
            template: None,
            replace: true,
            colon: super::ColonReplacement::Smart,
            custom: None,
        });
    }
    let template = render::parse(
        TemplateField::StandardMovie,
        format.as_deref().ok_or_else(config_error)?,
    )
    .map_err(|_| config_error())?;
    let replace = match row.get::<i64>(3)? {
        0 => false,
        1 => true,
        _ => return Err(config_error()),
    };
    let colon = super::colon_from_db(&row.get::<String>(4)?).map_err(|_| config_error())?;
    let custom = row.get::<Option<String>>(5)?;
    super::validate_common(colon, custom.as_deref()).map_err(|_| config_error())?;
    Ok(Settings {
        revision,
        enabled,
        format,
        template: Some(template),
        replace,
        colon,
        custom,
    })
}
struct File {
    movie: i64,
    id: i64,
    root: String,
    path: String,
    original: Option<String>,
    facts: MovieNamingFacts,
    evidence: Option<Evidence>,
    language: Option<i64>,
    invalid: bool,
}
struct Snapshot {
    settings: Settings,
    files: Vec<File>,
    formats: Vec<Format>,
}
fn valid_text(s: &str) -> bool {
    s.len() <= 4096 && !s.chars().any(char::is_control)
}
fn safe_id(id: i64) -> bool {
    (1..=MAX_SAFE).contains(&id)
}
async fn capture(c: &Connection, ids: &[i64]) -> Result<Snapshot> {
    let settings = settings(c).await?;
    let formats = if settings.cf() {
        custom_formats::catalog(c, MediaDomain::Movies)
            .await
            .map_err(|_| storage())?
    } else {
        vec![]
    };
    let mut budget = serde_json::to_vec(&formats).map_err(|_| storage())?.len()
        + settings.format.as_ref().map_or(0, String::len);
    if budget > MAX_SNAPSHOT_BYTES {
        return Err(unavailable());
    }
    let mut files = vec![];
    for &movie in ids {
        let mut rows=c.query("SELECT m.id,m.path,d.title,d.year,d.original_language,f.id,f.path,f.edition,fm.quality_id,fm.revision_json,fm.original_release_title,q.title FROM movies m JOIN movie_metadata d ON d.id=m.metadata_id LEFT JOIN movie_files f ON f.movie_id=m.id LEFT JOIN file_metadata fm ON fm.movie_file_id=f.id AND fm.media_type='movies' LEFT JOIN quality_definitions q ON q.media_type='movies' AND q.quality_id=fm.quality_id WHERE m.id=? ORDER BY f.id",[movie]).await?;
        let mut found = false;
        while let Some(row) = rows.next().await? {
            found = true;
            let Some(id) = row.get::<Option<i64>>(5)? else {
                continue;
            };
            if !safe_id(id) {
                return Err(storage());
            }
            let mut file = read_file(&row, movie, id, &settings)?;
            if settings.cf() {
                match custom_formats::existing(c, MediaDomain::Movies, id).await {
                    Ok(evidence) => {
                        if custom_formats::validate_evidence(MediaDomain::Movies, &evidence)
                            .is_err()
                        {
                            file.invalid = true;
                        }
                        file.evidence = Some(evidence);
                    }
                    Err(e) if e.0 == "release_storage_error" => return Err(storage()),
                    Err(_) => file.invalid = true,
                }
            }
            budget = budget
                .checked_add(
                    file.root.len()
                        + file.path.len()
                        + file.original.as_ref().map_or(0, String::len)
                        + file.facts.movie_title.len()
                        + file.facts.edition.as_ref().map_or(0, String::len)
                        + file.facts.quality_title.len()
                        + serde_json::to_vec(&file.evidence)
                            .map_err(|_| storage())?
                            .len(),
                )
                .ok_or_else(unavailable)?;
            if budget > MAX_SNAPSHOT_BYTES || files.len() >= MAX_IDS {
                return Err(unavailable());
            }
            files.push(file);
        }
        if !found {
            return Err(Error(
                StatusCode::NOT_FOUND,
                "movie_not_found",
                "One or more selected movies were not found",
            ));
        }
    }
    Ok(Snapshot {
        settings,
        files,
        formats,
    })
}
fn read_file(row: &Row, movie: i64, id: i64, settings: &Settings) -> Result<File> {
    let root = row.get::<String>(1)?;
    let path = row.get::<String>(6)?;
    let original = if !settings.enabled {
        row.get::<Option<String>>(10)?
    } else {
        None
    };
    let title = if settings
        .template
        .as_ref()
        .is_some_and(Template::uses_movie_title)
    {
        row.get::<String>(2)?
    } else {
        String::new()
    };
    let year = if settings.template.as_ref().is_some_and(Template::uses_year) {
        row.get::<Option<i64>>(3)?
    } else {
        None
    };
    let edition = if settings
        .template
        .as_ref()
        .is_some_and(Template::uses_edition)
    {
        row.get::<Option<String>>(7)?
    } else {
        None
    };
    let mut invalid = settings.enabled
        && (!valid_text(&title)
            || edition.as_ref().is_some_and(|v| !valid_text(v))
            || year.is_some_and(|v| !(1..=9999).contains(&v)));
    let mut quality_title = String::new();
    let mut revision = None;
    let mut has_quality = false;
    if settings.quality() {
        let quality = row.get::<Option<i64>>(8)?;
        has_quality = quality.is_some();
        let name = row.get::<Option<String>>(11)?;
        if quality.is_some() && name.is_none() {
            invalid = true;
        }
        quality_title = name.unwrap_or_default();
        invalid |= !valid_text(&quality_title);
    }
    if settings.revision() && has_quality {
        if let Some(raw) = row.get::<Option<String>>(9)? {
            if raw.len() > MAX_SNAPSHOT_BYTES {
                return Err(unavailable());
            }
            match serde_json::from_str::<FileRevision>(&raw) {
                Ok(value) if value.validate().is_ok() => revision = Some(value),
                _ => invalid = true,
            }
        }
    }
    let language = if settings.cf() {
        row.get::<Option<i64>>(4)?
    } else {
        None
    };
    if language.is_some_and(|v| !(0..=57).contains(&v)) {
        invalid = true;
    }
    Ok(File {
        movie,
        id,
        root,
        path,
        original,
        facts: MovieNamingFacts {
            movie_title: title,
            release_year: year,
            edition,
            quality_title,
            revision,
            custom_formats: vec![],
        },
        evidence: None,
        language,
        invalid,
    })
}
fn relative(root: &str, path: &str) -> Option<String> {
    fn absolute(value: &str) -> bool {
        value.starts_with('/')
            && value.len() <= MAX_PATH_BYTES
            && !value.contains('\\')
            && !value.chars().any(char::is_control)
            && value[1..]
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != ".." && part.len() <= 255)
    }
    if !root.starts_with('/')
        || root.len() > MAX_PATH_BYTES
        || (root != "/" && root.bytes().all(|b| b == b'/'))
    {
        return None;
    }
    let root = root.trim_end_matches('/');
    let root = if root.is_empty() { "/" } else { root };
    if (root != "/" && !absolute(root)) || !absolute(path) {
        return None;
    }
    let relative = if root == "/" {
        path.strip_prefix('/')
    } else {
        path.strip_prefix(root).and_then(|s| s.strip_prefix('/'))
    }?;
    if relative.is_empty() {
        return None;
    }
    Some(relative.to_owned())
}
fn failed(
    file: &File,
    existing: Option<String>,
    reason: MovieRenamePreviewReason,
) -> MovieRenamePreviewItem {
    MovieRenamePreviewItem {
        movie_id: file.movie,
        movie_file_id: file.id,
        existing_path: existing,
        new_path: None,
        status: MovieRenamePreviewStatus::Unavailable,
        reasons: vec![reason],
    }
}
fn filename(
    file: &File,
    existing: &str,
    settings: &Settings,
) -> std::result::Result<String, MovieRenamePreviewReason> {
    let basename = existing
        .rsplit('/')
        .next()
        .ok_or(MovieRenamePreviewReason::InvalidPath)?;
    let (current_stem, extension) = basename
        .rsplit_once('.')
        .filter(|(_, e)| !e.is_empty())
        .ok_or(MovieRenamePreviewReason::MissingExtension)?;
    if extension.len() >= 254
        || extension.chars().any(|c| {
            c.is_control() || matches!(c, '/' | '\\' | '<' | '>' | '"' | '|' | '?' | '*' | ':')
        })
    {
        return Err(MovieRenamePreviewReason::NamingRenderFailed);
    }
    let stem = if let Some(template) = &settings.template {
        render::render(
            template,
            RenderFacts::Movie(&file.facts),
            &settings.render_config(),
        )
    } else {
        let stem = file
            .original
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(current_stem);
        render::preview_original_stem(stem)
    }
    .map_err(|_| MovieRenamePreviewReason::NamingRenderFailed)?;
    let stem = render::cap_bytes(&stem, 255 - extension.len() - 1).trim_end_matches([' ', '.']);
    if stem.is_empty() {
        return Err(MovieRenamePreviewReason::NamingRenderFailed);
    }
    Ok(format!("{stem}.{extension}"))
}
async fn preview(
    State(db): State<Arc<Database>>,
    RawQuery(raw): RawQuery,
) -> Result<Json<MovieRenamePreview>> {
    let ids = ids(raw)?;
    tokio::time::timeout(Duration::from_secs(DEADLINE_SECONDS), async {
        let c = db.connect().await?;
        let tx = c
            .transaction_with_behavior(TransactionBehavior::ReadOnly)
            .await?;
        let captured = capture(&tx, &ids).await;
        let mut captured = match captured {
            Ok(s) => {
                tx.commit().await?;
                s
            }
            Err(e) => {
                tx.rollback().await?;
                return Err(e);
            }
        };
        let mut items = vec![];
        let mut unchanged = 0;
        let mut unavailable_count = 0;
        for file in &mut captured.files {
            let existing = relative(&file.root, &file.path);
            let Some(existing) = existing else {
                unavailable_count += 1;
                items.push(failed(file, None, MovieRenamePreviewReason::InvalidPath));
                continue;
            };
            if file.invalid {
                unavailable_count += 1;
                items.push(failed(
                    file,
                    Some(existing),
                    MovieRenamePreviewReason::InvalidFileFacts,
                ));
                continue;
            }
            if captured.settings.cf() {
                let evidence = file.evidence.clone().ok_or_else(storage)?;
                let score = custom_formats::score(
                    &captured.formats,
                    MediaDomain::Movies,
                    evidence,
                    file.language,
                    vec![],
                    false,
                )
                .await
                .map_err(|_| unavailable())?;
                file.facts.custom_formats = captured
                    .formats
                    .iter()
                    .filter(|f| {
                        f.definition.include_when_renaming && score.format_ids.contains(&f.id)
                    })
                    .map(|f| f.definition.name.clone())
                    .collect();
            }
            match filename(file, &existing, &captured.settings) {
                Ok(new_path) if new_path == existing => unchanged += 1,
                Ok(new_path) => items.push(MovieRenamePreviewItem {
                    movie_id: file.movie,
                    movie_file_id: file.id,
                    existing_path: Some(existing),
                    new_path: Some(new_path),
                    status: MovieRenamePreviewStatus::Change,
                    reasons: vec![],
                }),
                Err(reason) => {
                    unavailable_count += 1;
                    items.push(failed(file, Some(existing), reason));
                }
            }
        }
        let result = MovieRenamePreview {
            naming_revision: captured.settings.revision,
            rename_enabled: captured.settings.enabled,
            standard_movie_format: captured.settings.format,
            movie_ids: ids,
            files_considered: captured.files.len(),
            unchanged_count: unchanged,
            unavailable_count,
            items,
        };
        if serde_json::to_vec(&result).map_err(|_| storage())?.len() > MAX_RESPONSE_BYTES {
            return Err(unavailable());
        }
        Ok(Json(result))
    })
    .await
    .map_err(|_| unavailable())?
}
