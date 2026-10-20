//! Stored-file rename planning for TV (grouped episode files, season folders) and folder
//! renames for both domains. Facts are captured in one read-only transaction; custom-format
//! matching runs after it. The same computation feeds the read-only previews and rename
//! execution, so a command renames to exactly what a preview showed.
use super::movie_preview::Error;
use super::render::{
    self, EpisodeMemberNamingFacts, EpisodeNamingFacts, EpisodeSetNamingFacts, MovieNamingFacts,
    MultiEpisodeStyle, RenderConfig, RenderFacts, Template, TemplateField,
};
use crate::{
    api::MediaDomain,
    custom_formats::{self, Evidence, Format},
    db::Database,
    media_files::FileRevision,
    rename::fsops,
};
use axum::{
    Json, Router,
    extract::{RawQuery, State},
    http::StatusCode,
    routing::get,
};
use libsql::{Connection, TransactionBehavior};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};

const MAX_SAFE: i64 = 9_007_199_254_740_991;
pub(crate) const MAX_OWNERS: usize = 50;
pub(crate) const MAX_FILES: usize = 5000;
const MAX_QUERY_BYTES: usize = 4096;
const MAX_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const DEADLINE_SECONDS: u64 = 12;
type Result<T> = std::result::Result<T, Error>;

fn err(status: StatusCode, code: &'static str, message: &'static str) -> Error {
    Error(status, code, message)
}
fn bad() -> Error {
    err(
        StatusCode::BAD_REQUEST,
        "invalid_request",
        "Provide a bounded selection of distinct positive IDs",
    )
}
fn storage() -> Error {
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "database_error",
        "Rename preview could not read stored data",
    )
}
fn config_error() -> Error {
    err(
        StatusCode::UNPROCESSABLE_ENTITY,
        "naming_configuration_invalid",
        "The enabled naming configuration is invalid",
    )
}
fn busy() -> Error {
    err(
        StatusCode::SERVICE_UNAVAILABLE,
        "rename_preview_unavailable",
        "Rename preview exceeded its resource limit or is temporarily busy",
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum RenameStatus {
    Change,
    /// A name cannot be produced from stored facts and configuration.
    Unavailable,
    /// A name was produced but the move cannot be made now (collision, missing source, links).
    Blocked,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum RenameReason {
    InvalidPath,
    OutsideOwnerFolder,
    MissingExtension,
    InvalidFileFacts,
    NamingRenderFailed,
    MultiEpisodeStyleRequired,
    NoLinkedEpisodes,
    InvalidEpisodeSet,
    FolderFormatUnavailable,
    DestinationInUse,
    DuplicateDestination,
    DestinationExists,
    SourceMissing,
    PathUnsafe,
}
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct TvRenamePreviewItem {
    pub series_id: i64,
    pub episode_file_id: i64,
    pub season_number: Option<i64>,
    pub episode_ids: Vec<i64>,
    pub episode_numbers: Vec<i64>,
    pub existing_path: Option<String>,
    pub new_path: Option<String>,
    pub status: RenameStatus,
    pub reasons: Vec<RenameReason>,
}
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct TvRenamePreview {
    pub naming_revision: i64,
    pub rename_enabled: bool,
    pub series_ids: Vec<i64>,
    pub season: Option<i64>,
    pub files_considered: usize,
    pub unchanged_count: usize,
    pub unavailable_count: usize,
    pub blocked_count: usize,
    pub items: Vec<TvRenamePreviewItem>,
}
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct FolderRenamePreviewItem {
    pub series_id: Option<i64>,
    pub movie_id: Option<i64>,
    pub existing_path: String,
    pub new_path: Option<String>,
    pub status: RenameStatus,
    pub reasons: Vec<RenameReason>,
}
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct FolderRenamePreview {
    pub media_type: MediaDomain,
    pub naming_revision: i64,
    pub folder_format: Option<String>,
    pub ids: Vec<i64>,
    pub unchanged_count: usize,
    pub items: Vec<FolderRenamePreviewItem>,
}

/// One planned file rename with absolute paths, for execution.
#[derive(Debug, Clone)]
pub(crate) struct PlannedFile {
    pub series_id: i64,
    pub file_id: i64,
    pub season: Option<i64>,
    pub episode_ids: Vec<i64>,
    pub episode_numbers: Vec<i64>,
    pub root: String,
    pub old_abs: String,
    pub new_abs: Option<String>,
    pub unchanged: bool,
    pub status: Option<RenameStatus>,
    pub reasons: Vec<RenameReason>,
}
pub(crate) struct TvComputed {
    pub revision: i64,
    pub enabled: bool,
    pub files: Vec<PlannedFile>,
}
#[derive(Debug, Clone)]
pub(crate) struct PlannedFolder {
    pub series_id: Option<i64>,
    pub movie_id: Option<i64>,
    pub old_abs: String,
    pub new_abs: Option<String>,
    pub unchanged: bool,
    pub status: Option<RenameStatus>,
    pub reasons: Vec<RenameReason>,
}
pub(crate) struct FolderComputed {
    pub revision: i64,
    pub format: Option<String>,
    pub folders: Vec<PlannedFolder>,
}

fn valid_text(s: &str) -> bool {
    s.len() <= 4096 && !s.chars().any(char::is_control)
}
fn abs_ok(v: &str) -> bool {
    v.starts_with('/')
        && v.len() <= 4096
        && !v.contains('\\')
        && !v.chars().any(char::is_control)
        && v[1..]
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != ".." && p.len() <= 255)
}
fn relative(root: &str, path: &str) -> Option<String> {
    let root = root.trim_end_matches('/');
    if root.is_empty() || !abs_ok(root) || !abs_ok(path) {
        return None;
    }
    path.strip_prefix(root)?
        .strip_prefix('/')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

struct TvSettings {
    revision: i64,
    enabled: bool,
    replace: bool,
    colon: super::ColonReplacement,
    custom: Option<String>,
    standard: Option<Template>,
    daily: Option<Template>,
    anime: Option<Template>,
    series_folder: Option<Template>,
    season_folder: Option<Template>,
    specials_folder: Option<Template>,
    series_folder_text: Option<String>,
    style: Option<MultiEpisodeStyle>,
}
impl TvSettings {
    fn config(&self) -> RenderConfig<'_> {
        RenderConfig {
            replace_illegal_characters: self.replace,
            colon: super::colon_policy(self.colon, self.custom.as_deref()),
        }
    }
    fn file_templates(&self) -> impl Iterator<Item = &Template> {
        [&self.standard, &self.daily, &self.anime]
            .into_iter()
            .flatten()
    }
}
fn parse_opt(field: TemplateField, text: Option<String>) -> Result<Option<Template>> {
    text.map(|t| render::parse(field, &t).map_err(|_| config_error()))
        .transpose()
}
async fn tv_settings(c: &Connection) -> Result<TvSettings> {
    let row=c.query("SELECT revision,rename_enabled,replace_illegal_characters,colon_replacement,custom_colon_replacement,standard_episode_format,daily_episode_format,anime_episode_format,series_folder_format,season_folder_format,specials_folder_format,multi_episode_style FROM naming_settings WHERE domain='tv'",()).await?.next().await?.ok_or_else(storage)?;
    let revision = row.get::<i64>(0)?;
    if !(1..=MAX_SAFE).contains(&revision) {
        return Err(storage());
    }
    let colon = super::colon_from_db(&row.get::<String>(3)?).map_err(|_| config_error())?;
    let custom = row.get::<Option<String>>(4)?;
    super::validate_common(colon, custom.as_deref()).map_err(|_| config_error())?;
    let enabled = row.get::<i64>(1)? == 1;
    let series_folder_text = row.get::<Option<String>>(8)?;
    let style = row
        .get::<Option<i64>>(11)?
        .map(MultiEpisodeStyle::try_from)
        .transpose()
        .map_err(|_| config_error())?;
    Ok(TvSettings {
        revision,
        enabled,
        replace: row.get::<i64>(2)? == 1,
        colon,
        custom,
        standard: if enabled {
            parse_opt(TemplateField::StandardEpisode, row.get(5)?)?
        } else {
            None
        },
        daily: if enabled {
            parse_opt(TemplateField::DailyEpisode, row.get(6)?)?
        } else {
            None
        },
        anime: if enabled {
            parse_opt(TemplateField::AnimeEpisode, row.get(7)?)?
        } else {
            None
        },
        series_folder: parse_opt(TemplateField::SeriesFolder, series_folder_text.clone())?,
        season_folder: parse_opt(TemplateField::SeasonFolder, row.get(9)?)?,
        specials_folder: parse_opt(TemplateField::SpecialsFolder, row.get(10)?)?,
        series_folder_text,
        style,
    })
}

struct Member {
    id: i64,
    season: i64,
    number: i64,
    title: String,
    air: Option<String>,
    absolute: Option<i64>,
}
struct TvFile {
    id: i64,
    path: String,
    original: Option<String>,
    quality_title: String,
    revision: Option<FileRevision>,
    members: Vec<Member>,
    evidence: Option<Evidence>,
    cf_names: Vec<String>,
    invalid: bool,
}
struct Series {
    id: i64,
    title: String,
    path: String,
    kind: Option<String>,
    season_folder: Option<bool>,
    language: Option<i64>,
    files: Vec<TvFile>,
    tracked: BTreeMap<String, i64>,
}
struct Snapshot {
    settings: TvSettings,
    series: Vec<Series>,
    formats: Vec<Format>,
}
fn not_found() -> Error {
    err(
        StatusCode::NOT_FOUND,
        "series_not_found",
        "One or more selected series were not found",
    )
}
async fn capture_tv(c: &Connection, ids: &[i64], season: Option<i64>) -> Result<Snapshot> {
    let settings = tv_settings(c).await?;
    let uses_cf = settings.file_templates().any(Template::uses_custom_formats);
    let uses_quality = settings.file_templates().any(Template::uses_quality);
    let uses_revision = settings.file_templates().any(Template::uses_revision);
    let formats = if uses_cf {
        custom_formats::catalog(c, MediaDomain::Tv)
            .await
            .map_err(|_| storage())?
    } else {
        vec![]
    };
    let mut budget = serde_json::to_vec(&formats).map_err(|_| storage())?.len();
    let mut total_files = 0usize;
    let mut out = vec![];
    for &id in ids {
        let row=c.query("SELECT s.id,s.title,s.path,l.series_type,l.season_folder,s.original_language FROM series s LEFT JOIN library_settings l ON l.series_id=s.id WHERE s.id=?",[id]).await?.next().await?.ok_or_else(not_found)?;
        let mut series = Series {
            id,
            title: row.get(1)?,
            path: row.get(2)?,
            kind: row.get(3)?,
            season_folder: row.get::<Option<i64>>(4)?.map(|v| v != 0),
            language: row.get(5)?,
            files: vec![],
            tracked: BTreeMap::new(),
        };
        let mut rows=c.query("SELECT f.id,f.path,fm.original_release_title,fm.quality_id,fm.revision_json,q.title FROM episode_files f LEFT JOIN file_metadata fm ON fm.episode_file_id=f.id AND fm.media_type='tv' LEFT JOIN quality_definitions q ON q.media_type='tv' AND q.quality_id=fm.quality_id WHERE f.series_id=? ORDER BY f.id",[id]).await?;
        let mut by_id: BTreeMap<i64, usize> = BTreeMap::new();
        while let Some(r) = rows.next().await? {
            total_files += 1;
            if total_files > MAX_FILES {
                return Err(busy());
            }
            let mut invalid = false;
            let mut quality_title = String::new();
            let mut revision = None;
            if uses_quality {
                let name = r.get::<Option<String>>(5)?;
                if r.get::<Option<i64>>(3)?.is_some() && name.is_none() {
                    invalid = true;
                }
                quality_title = name.unwrap_or_default();
                invalid |= !valid_text(&quality_title);
            }
            if uses_revision && r.get::<Option<i64>>(3)?.is_some() {
                if let Some(raw) = r.get::<Option<String>>(4)? {
                    match serde_json::from_str::<FileRevision>(&raw) {
                        Ok(v) if raw.len() < 4096 && v.validate().is_ok() => revision = Some(v),
                        _ => invalid = true,
                    }
                }
            }
            let path = r.get::<String>(1)?;
            let original = if settings.enabled {
                None
            } else {
                r.get::<Option<String>>(2)?
            };
            budget += path.len() + quality_title.len() + original.as_ref().map_or(0, String::len);
            by_id.insert(r.get(0)?, series.files.len());
            series.files.push(TvFile {
                id: r.get(0)?,
                path,
                original,
                quality_title,
                revision,
                members: vec![],
                evidence: None,
                cf_names: vec![],
                invalid,
            });
        }
        drop(rows);
        let mut rows=c.query("SELECT e.episode_file_id,e.id,e.season,e.number,e.title,e.air_date,e.absolute_episode_number FROM episodes e WHERE e.series_id=? AND e.episode_file_id IS NOT NULL ORDER BY e.season,e.number,e.id",[id]).await?;
        while let Some(r) = rows.next().await? {
            let Some(&index) = by_id.get(&r.get::<i64>(0)?) else {
                continue;
            };
            let member = Member {
                id: r.get(1)?,
                season: r.get(2)?,
                number: r.get(3)?,
                title: r.get(4)?,
                air: r.get(5)?,
                absolute: r.get(6)?,
            };
            budget += member.title.len() + 64;
            if budget > MAX_SNAPSHOT_BYTES
                || series.files[index].members.len() >= render::MAX_EPISODE_MEMBERS
            {
                return Err(busy());
            }
            series.files[index].members.push(member);
        }
        drop(rows);
        series.tracked = series
            .files
            .iter()
            .map(|f| (f.path.clone(), f.id))
            .collect();
        if let Some(season) = season {
            series
                .files
                .retain(|f| f.members.iter().any(|m| m.season == season));
        }
        if uses_cf {
            for f in &mut series.files {
                match custom_formats::existing(c, MediaDomain::Tv, f.id).await {
                    Ok(e) => {
                        if custom_formats::validate_evidence(MediaDomain::Tv, &e).is_err() {
                            f.invalid = true;
                        }
                        f.evidence = Some(e);
                    }
                    Err(e) if e.0 == "release_storage_error" => return Err(storage()),
                    Err(_) => f.invalid = true,
                }
            }
        }
        out.push(series);
    }
    Ok(Snapshot {
        settings,
        series: out,
        formats,
    })
}

struct Fail(RenameStatus, RenameReason);
fn unavailable(reason: RenameReason) -> Fail {
    Fail(RenameStatus::Unavailable, reason)
}
fn ext_ok(extension: &str) -> bool {
    !extension.is_empty()
        && extension.len() < 254
        && !extension.chars().any(|c| {
            c.is_control() || matches!(c, '/' | '\\' | '<' | '>' | '"' | '|' | '?' | '*' | ':')
        })
}
fn render_error(e: render::RenderError) -> RenameReason {
    match e {
        render::RenderError::NeedsPolicy => RenameReason::MultiEpisodeStyleRequired,
        render::RenderError::InvalidEpisodeSet => RenameReason::InvalidEpisodeSet,
        _ => RenameReason::NamingRenderFailed,
    }
}
/// The folder a file belongs in, relative to the series folder ("" is the series folder itself).
fn season_dir(
    settings: &TvSettings,
    series: &Series,
    season: i64,
    current: &str,
) -> std::result::Result<String, Fail> {
    match series.season_folder {
        None => Ok(current.to_owned()),
        Some(false) => Ok(String::new()),
        Some(true) => {
            let template = if season == 0 {
                settings.specials_folder.as_ref()
            } else {
                settings.season_folder.as_ref()
            };
            let Some(template) = template else {
                return Ok(current.to_owned());
            };
            let facts = EpisodeNamingFacts {
                series_title: series.title.clone(),
                anime: false,
                season,
                episode: 0,
                episode_title: None,
                quality_title: String::new(),
                revision: None,
                custom_formats: vec![],
                air_date: None,
            };
            let name = render::render(template, RenderFacts::Episode(&facts), &settings.config())
                .map_err(|_| unavailable(RenameReason::FolderFormatUnavailable))?;
            if name.contains('/') {
                return Err(unavailable(RenameReason::FolderFormatUnavailable));
            }
            Ok(name)
        }
    }
}
fn plan_file(
    settings: &TvSettings,
    series: &Series,
    file: &TvFile,
) -> std::result::Result<(Option<i64>, String), Fail> {
    let Some(rel) = relative(&series.path, &file.path) else {
        return Err(unavailable(if abs_ok(&file.path) && abs_ok(&series.path) {
            RenameReason::OutsideOwnerFolder
        } else {
            RenameReason::InvalidPath
        }));
    };
    if file.invalid {
        return Err(unavailable(RenameReason::InvalidFileFacts));
    }
    if file.members.is_empty() {
        return Err(unavailable(RenameReason::NoLinkedEpisodes));
    }
    let (dir, base) = rel.rsplit_once('/').unwrap_or(("", rel.as_str()));
    let (current_stem, extension) = base
        .rsplit_once('.')
        .filter(|(_, e)| !e.is_empty())
        .ok_or_else(|| unavailable(RenameReason::MissingExtension))?;
    if !ext_ok(extension) {
        return Err(unavailable(RenameReason::NamingRenderFailed));
    }
    let first = &file.members[0];
    let stem = if settings.enabled {
        let template = match series.kind.as_deref() {
            Some("daily") => &settings.daily,
            Some("anime") => &settings.anime,
            _ => &settings.standard,
        }
        .as_ref()
        .ok_or_else(|| unavailable(RenameReason::NamingRenderFailed))?;
        let facts = EpisodeSetNamingFacts {
            series_title: series.title.clone(),
            anime: series.kind.as_deref() == Some("anime"),
            members: file
                .members
                .iter()
                .map(|m| EpisodeMemberNamingFacts {
                    episode_id: m.id,
                    season: m.season,
                    episode: m.number,
                    title: Some(m.title.clone()),
                    air_date: m.air.clone(),
                    absolute_episode_number: m.absolute,
                })
                .collect(),
            quality_title: file.quality_title.clone(),
            revision: file.revision.clone(),
            custom_formats: file.cf_names.clone(),
        };
        render::render_episode_set(template, &facts, settings.style, &settings.config())
            .map_err(|e| unavailable(render_error(e)))?
    } else {
        let stem = file
            .original
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(current_stem);
        render::preview_original_stem(stem)
            .map_err(|_| unavailable(RenameReason::NamingRenderFailed))?
    };
    let stem = render::cap_bytes(&stem, 255 - extension.len() - 1).trim_end_matches([' ', '.']);
    if stem.is_empty() {
        return Err(unavailable(RenameReason::NamingRenderFailed));
    }
    let season = first.season;
    let new_dir = season_dir(settings, series, season, dir)?;
    let name = format!("{stem}.{extension}");
    Ok((
        Some(season),
        if new_dir.is_empty() {
            name
        } else {
            format!("{new_dir}/{name}")
        },
    ))
}

fn file_state(
    series: &Series,
    file: &TvFile,
    result: std::result::Result<(Option<i64>, String), Fail>,
) -> PlannedFile {
    let numbers: Vec<i64> = file.members.iter().map(|m| m.number).collect();
    let ids: Vec<i64> = file.members.iter().map(|m| m.id).collect();
    let mut p = PlannedFile {
        series_id: series.id,
        file_id: file.id,
        season: file.members.first().map(|m| m.season),
        episode_ids: ids,
        episode_numbers: numbers,
        root: series.path.clone(),
        old_abs: file.path.clone(),
        new_abs: None,
        unchanged: false,
        status: None,
        reasons: vec![],
    };
    match result {
        Ok((_, rel)) => {
            let abs = format!("{}/{rel}", series.path.trim_end_matches('/'));
            if abs == file.path {
                p.unchanged = true;
            } else {
                p.new_abs = Some(abs);
                p.status = Some(RenameStatus::Change);
            }
        }
        Err(Fail(status, reason)) => {
            p.status = Some(status);
            p.reasons = vec![reason];
        }
    }
    p
}
fn block(p: &mut PlannedFile, reason: RenameReason) {
    p.status = Some(RenameStatus::Blocked);
    if !p.reasons.contains(&reason) {
        p.reasons.push(reason);
    }
}
/// Plans files of the selected series (optionally one season, optionally an explicit file set).
/// With `check_filesystem`, a read-only descriptor-relative probe marks missing sources, existing
/// destinations and unsafe links as blocked.
pub(crate) async fn compute_tv(
    db: &Database,
    ids: Vec<i64>,
    season: Option<i64>,
    only: Option<&BTreeSet<i64>>,
    check_filesystem: bool,
) -> Result<TvComputed> {
    tokio::time::timeout(Duration::from_secs(DEADLINE_SECONDS), async {
        let c = db.connect().await?;
        let tx = c
            .transaction_with_behavior(TransactionBehavior::ReadOnly)
            .await?;
        let captured = capture_tv(&tx, &ids, season).await;
        let mut snap = match captured {
            Ok(s) => {
                tx.commit().await?;
                s
            }
            Err(e) => {
                tx.rollback().await?;
                return Err(e);
            }
        };
        let mut files = vec![];
        let mut seen: BTreeMap<String, usize> = BTreeMap::new();
        for series in &mut snap.series {
            if let Some(only) = only {
                series.files.retain(|f| only.contains(&f.id));
            }
            let mut indexes = vec![];
            for i in 0..series.files.len() {
                if snap
                    .settings
                    .file_templates()
                    .any(Template::uses_custom_formats)
                {
                    if let Some(e) = series.files[i].evidence.clone() {
                        if !series.files[i].invalid {
                            let score = custom_formats::score(
                                &snap.formats,
                                MediaDomain::Tv,
                                e,
                                series.language,
                                vec![],
                                false,
                            )
                            .await
                            .map_err(|_| busy())?;
                            series.files[i].cf_names = snap
                                .formats
                                .iter()
                                .filter(|f| {
                                    f.definition.include_when_renaming
                                        && score.format_ids.contains(&f.id)
                                })
                                .map(|f| f.definition.name.clone())
                                .collect();
                        }
                    }
                }
                let result = plan_file(&snap.settings, series, &series.files[i]);
                let planned = file_state(series, &series.files[i], result);
                indexes.push(files.len());
                if let Some(new) = &planned.new_abs {
                    seen.entry(new.clone()).or_default();
                    *seen.get_mut(new).unwrap() += 1;
                }
                files.push(planned);
            }
            for &i in &indexes {
                let Some(new) = files[i].new_abs.clone() else {
                    continue;
                };
                if seen.get(&new).copied().unwrap_or(0) > 1 {
                    block(&mut files[i], RenameReason::DuplicateDestination);
                }
                if series
                    .tracked
                    .get(&new)
                    .is_some_and(|other| *other != files[i].file_id)
                {
                    block(&mut files[i], RenameReason::DestinationInUse);
                }
            }
        }
        if check_filesystem {
            let work: Vec<(usize, String, String, String)> = files
                .iter()
                .enumerate()
                .filter(|(_, f)| f.status == Some(RenameStatus::Change))
                .map(|(i, f)| {
                    (
                        i,
                        f.root.clone(),
                        f.old_abs.clone(),
                        f.new_abs.clone().unwrap(),
                    )
                })
                .collect();
            let probes = tokio::task::spawn_blocking(move || {
                work.into_iter()
                    .map(|(i, root, old, new)| {
                        (i, fsops::probe(&root, &old, &new, fsops::Kind::File))
                    })
                    .collect::<Vec<_>>()
            })
            .await
            .map_err(|_| busy())?;
            for (i, probe) in probes {
                match probe {
                    fsops::Probe::Ready | fsops::Probe::CaseOnly => {}
                    fsops::Probe::Exists => block(&mut files[i], RenameReason::DestinationExists),
                    fsops::Probe::SourceMissing => {
                        block(&mut files[i], RenameReason::SourceMissing)
                    }
                    fsops::Probe::Unsafe => block(&mut files[i], RenameReason::PathUnsafe),
                }
            }
        }
        Ok(TvComputed {
            revision: snap.settings.revision,
            enabled: snap.settings.enabled,
            files,
        })
    })
    .await
    .map_err(|_| busy())?
}

// ---- folders -------------------------------------------------------------------------------

async fn render_folder(c: &Connection, domain: MediaDomain, ids: &[i64]) -> Result<FolderComputed> {
    let (revision, format, replace, colon_text, custom) = {
        let d = if domain == MediaDomain::Tv {
            "tv"
        } else {
            "movies"
        };
        let col = if domain == MediaDomain::Tv {
            "series_folder_format"
        } else {
            "movie_folder_format"
        };
        let row=c.query(&format!("SELECT revision,{col},replace_illegal_characters,colon_replacement,custom_colon_replacement FROM naming_settings WHERE domain=?"),[d]).await?.next().await?.ok_or_else(storage)?;
        (
            row.get::<i64>(0)?,
            row.get::<Option<String>>(1)?,
            row.get::<i64>(2)? == 1,
            row.get::<String>(3)?,
            row.get::<Option<String>>(4)?,
        )
    };
    let colon = super::colon_from_db(&colon_text).map_err(|_| config_error())?;
    super::validate_common(colon, custom.as_deref()).map_err(|_| config_error())?;
    let template = format
        .as_deref()
        .map(|f| {
            render::parse(
                if domain == MediaDomain::Tv {
                    TemplateField::SeriesFolder
                } else {
                    TemplateField::MovieFolder
                },
                f,
            )
            .map_err(|_| config_error())
        })
        .transpose()?;
    let config = RenderConfig {
        replace_illegal_characters: replace,
        colon: super::colon_policy(colon, custom.as_deref()),
    };
    let mut folders = vec![];
    for &id in ids {
        let (old, rendered) = if domain == MediaDomain::Tv {
            let row = c
                .query("SELECT title,path FROM series WHERE id=?", [id])
                .await?
                .next()
                .await?
                .ok_or_else(not_found)?;
            let (title, path) = (row.get::<String>(0)?, row.get::<String>(1)?);
            let r = template.as_ref().map(|t| {
                let facts = EpisodeNamingFacts {
                    series_title: title.clone(),
                    anime: false,
                    season: 0,
                    episode: 0,
                    episode_title: None,
                    quality_title: String::new(),
                    revision: None,
                    custom_formats: vec![],
                    air_date: None,
                };
                render::render(t, RenderFacts::Episode(&facts), &config)
            });
            (path, r)
        } else {
            let row=c.query("SELECT d.title,d.year,m.path,(SELECT f.edition FROM movie_files f WHERE f.movie_id=m.id) FROM movies m JOIN movie_metadata d ON d.id=m.metadata_id WHERE m.id=?",[id]).await?.next().await?.ok_or(err(StatusCode::NOT_FOUND,"movie_not_found","One or more selected movies were not found"))?;
            let r = template.as_ref().map(|t| {
                let facts = MovieNamingFacts {
                    movie_title: row.get::<String>(0).unwrap_or_default(),
                    release_year: row.get::<Option<i64>>(1).unwrap_or(None),
                    edition: row.get::<Option<String>>(3).unwrap_or(None),
                    quality_title: String::new(),
                    revision: None,
                    custom_formats: vec![],
                };
                render::render(t, RenderFacts::Movie(&facts), &config)
            });
            (row.get::<String>(2)?, r)
        };
        let mut f = PlannedFolder {
            series_id: (domain == MediaDomain::Tv).then_some(id),
            movie_id: (domain == MediaDomain::Movies).then_some(id),
            old_abs: old.clone(),
            new_abs: None,
            unchanged: false,
            status: None,
            reasons: vec![],
        };
        let fail = |f: &mut PlannedFolder, reason| {
            f.status = Some(RenameStatus::Unavailable);
            f.reasons = vec![reason];
        };
        let trimmed = old.trim_end_matches('/');
        match rendered {
            None => fail(&mut f, RenameReason::FolderFormatUnavailable),
            Some(Err(_)) => fail(&mut f, RenameReason::NamingRenderFailed),
            Some(Ok(name)) if name.contains('/') => {
                fail(&mut f, RenameReason::FolderFormatUnavailable)
            }
            Some(Ok(name)) => match trimmed.rsplit_once('/') {
                Some((parent, current)) if !parent.is_empty() && abs_ok(trimmed) => {
                    if current == name {
                        f.unchanged = true;
                    } else {
                        f.new_abs = Some(format!("{parent}/{name}"));
                        f.status = Some(RenameStatus::Change);
                    }
                }
                _ => fail(&mut f, RenameReason::InvalidPath),
            },
        }
        folders.push(f);
    }
    // Collisions among selected folders and against every other tracked folder.
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for f in &folders {
        if let Some(n) = &f.new_abs {
            *seen.entry(n.clone()).or_default() += 1;
        }
    }
    let table = if domain == MediaDomain::Tv {
        "series"
    } else {
        "movies"
    };
    for f in &mut folders {
        let Some(n) = f.new_abs.clone() else { continue };
        if seen[&n] > 1 {
            f.status = Some(RenameStatus::Blocked);
            f.reasons.push(RenameReason::DuplicateDestination);
        }
        let used = c
            .query(
                &format!("SELECT 1 FROM {table} WHERE path=? AND id!=?"),
                libsql::params![n.clone(), f.series_id.or(f.movie_id).unwrap_or(0)],
            )
            .await?
            .next()
            .await?
            .is_some();
        if used {
            f.status = Some(RenameStatus::Blocked);
            f.reasons.push(RenameReason::DestinationInUse);
        }
    }
    Ok(FolderComputed {
        revision,
        format,
        folders,
    })
}
pub(crate) async fn compute_folders(
    db: &Database,
    domain: MediaDomain,
    ids: Vec<i64>,
    check_filesystem: bool,
) -> Result<FolderComputed> {
    tokio::time::timeout(Duration::from_secs(DEADLINE_SECONDS), async {
        let c = db.connect().await?;
        let tx = c
            .transaction_with_behavior(TransactionBehavior::ReadOnly)
            .await?;
        let result = render_folder(&tx, domain, &ids).await;
        let mut done = match result {
            Ok(v) => {
                tx.commit().await?;
                v
            }
            Err(e) => {
                tx.rollback().await?;
                return Err(e);
            }
        };
        if check_filesystem {
            let work: Vec<(usize, String, String)> = done
                .folders
                .iter()
                .enumerate()
                .filter(|(_, f)| f.status == Some(RenameStatus::Change))
                .map(|(i, f)| (i, f.old_abs.clone(), f.new_abs.clone().unwrap()))
                .collect();
            let probes = tokio::task::spawn_blocking(move || {
                work.into_iter()
                    .map(|(i, old, new)| {
                        let root = old
                            .trim_end_matches('/')
                            .rsplit_once('/')
                            .map_or("/", |p| p.0)
                            .to_owned();
                        (i, fsops::probe(&root, &old, &new, fsops::Kind::Dir))
                    })
                    .collect::<Vec<_>>()
            })
            .await
            .map_err(|_| busy())?;
            for (i, probe) in probes {
                let reason = match probe {
                    fsops::Probe::Ready | fsops::Probe::CaseOnly => continue,
                    fsops::Probe::Exists => RenameReason::DestinationExists,
                    fsops::Probe::SourceMissing => RenameReason::SourceMissing,
                    fsops::Probe::Unsafe => RenameReason::PathUnsafe,
                };
                done.folders[i].status = Some(RenameStatus::Blocked);
                done.folders[i].reasons.push(reason);
            }
        }
        Ok(done)
    })
    .await
    .map_err(|_| busy())?
}

// ---- routes --------------------------------------------------------------------------------

fn parse_ids(text: &str) -> Result<Vec<i64>> {
    let mut ids = BTreeSet::new();
    for part in text.split(',') {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) || part.len() > 16 {
            return Err(bad());
        }
        let id: i64 = part.parse().map_err(|_| bad())?;
        if !(1..=MAX_SAFE).contains(&id) || !ids.insert(id) || ids.len() > MAX_OWNERS {
            return Err(bad());
        }
    }
    Ok(ids.into_iter().collect())
}
fn fields(raw: Option<String>) -> Result<Vec<(String, String)>> {
    let raw = raw.ok_or_else(bad)?;
    if raw.len() > MAX_QUERY_BYTES {
        return Err(bad());
    }
    let fields: Vec<_> = url::form_urlencoded::parse(raw.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let keys: BTreeSet<_> = fields.iter().map(|f| f.0.clone()).collect();
    if keys.len() != fields.len() {
        return Err(bad());
    }
    Ok(fields)
}
fn size_check<T: Serialize>(v: &T) -> Result<()> {
    if serde_json::to_vec(v).map_err(|_| storage())?.len() > MAX_RESPONSE_BYTES {
        return Err(busy());
    }
    Ok(())
}
async fn tv_preview(
    State(db): State<Arc<Database>>,
    RawQuery(raw): RawQuery,
) -> Result<Json<TvRenamePreview>> {
    let fields = fields(raw)?;
    let get = |k: &str| fields.iter().find(|f| f.0 == k).map(|f| f.1.as_str());
    if fields
        .iter()
        .any(|f| !matches!(f.0.as_str(), "series_id" | "season" | "series_ids"))
    {
        return Err(bad());
    }
    let (ids, season) = match (get("series_id"), get("series_ids"), get("season")) {
        (Some(one), None, season) => {
            let ids = parse_ids(one)?;
            if ids.len() != 1 {
                return Err(bad());
            }
            let season = season
                .map(|s| {
                    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) || s.len() > 10 {
                        return Err(bad());
                    }
                    s.parse::<i64>().map_err(|_| bad())
                })
                .transpose()?;
            (ids, season)
        }
        (None, Some(many), None) => (parse_ids(many)?, None),
        _ => return Err(bad()),
    };
    let computed = compute_tv(&db, ids.clone(), season, None, true).await?;
    let mut items = vec![];
    let (mut unchanged, mut unavailable_count, mut blocked_count) = (0, 0, 0);
    let considered = computed.files.len();
    for f in computed.files {
        if f.unchanged {
            unchanged += 1;
            continue;
        }
        let root = f.root.trim_end_matches('/').to_owned();
        let status = f.status.unwrap_or(RenameStatus::Change);
        match status {
            RenameStatus::Unavailable => unavailable_count += 1,
            RenameStatus::Blocked => blocked_count += 1,
            RenameStatus::Change => {}
        }
        items.push(TvRenamePreviewItem {
            series_id: f.series_id,
            episode_file_id: f.file_id,
            season_number: f.season,
            episode_ids: f.episode_ids,
            episode_numbers: f.episode_numbers,
            existing_path: relative(&root, &f.old_abs),
            new_path: f.new_abs.as_deref().and_then(|n| relative(&root, n)),
            status,
            reasons: f.reasons,
        });
    }
    items.sort_by(|a, b| {
        (
            a.series_id,
            std::cmp::Reverse(a.season_number),
            &a.episode_numbers,
            a.episode_file_id,
        )
            .cmp(&(
                b.series_id,
                std::cmp::Reverse(b.season_number),
                &b.episode_numbers,
                b.episode_file_id,
            ))
    });
    let result = TvRenamePreview {
        naming_revision: computed.revision,
        rename_enabled: computed.enabled,
        series_ids: ids,
        season,
        files_considered: considered,
        unchanged_count: unchanged,
        unavailable_count,
        blocked_count,
        items,
    };
    size_check(&result)?;
    Ok(Json(result))
}
async fn folder_preview(
    db: Arc<Database>,
    domain: MediaDomain,
    raw: Option<String>,
) -> Result<Json<FolderRenamePreview>> {
    let key = if domain == MediaDomain::Tv {
        "series_ids"
    } else {
        "movie_ids"
    };
    let fields = fields(raw)?;
    if fields.len() != 1 || fields[0].0 != key {
        return Err(bad());
    }
    let ids = parse_ids(&fields[0].1)?;
    let computed = compute_folders(&db, domain, ids.clone(), true).await?;
    let mut unchanged = 0;
    let mut items = vec![];
    for f in computed.folders {
        if f.unchanged {
            unchanged += 1;
            continue;
        }
        items.push(FolderRenamePreviewItem {
            series_id: f.series_id,
            movie_id: f.movie_id,
            existing_path: f.old_abs,
            new_path: f.new_abs,
            status: f.status.unwrap_or(RenameStatus::Change),
            reasons: f.reasons,
        });
    }
    let result = FolderRenamePreview {
        media_type: domain,
        naming_revision: computed.revision,
        folder_format: computed.format,
        ids,
        unchanged_count: unchanged,
        items,
    };
    size_check(&result)?;
    Ok(Json(result))
}
pub fn router(db: Arc<Database>) -> Router {
    Router::new()
        .route("/api/v1/tv/rename-preview", get(tv_preview))
        .route(
            "/api/v1/tv/rename-folder-preview",
            get(|State(db): State<Arc<Database>>, RawQuery(raw): RawQuery| {
                folder_preview(db, MediaDomain::Tv, raw)
            }),
        )
        .route(
            "/api/v1/movies/rename-folder-preview",
            get(|State(db): State<Arc<Database>>, RawQuery(raw): RawQuery| {
                folder_preview(db, MediaDomain::Movies, raw)
            }),
        )
        .with_state(db)
}
