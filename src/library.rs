//! Manual library records. Physical paths are declarations; no filesystem actions are performed.
use crate::db::Database;
use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, Path, Query, RawQuery, State,
        rejection::{JsonRejection, QueryRejection},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, put},
};
use libsql::{Connection, Value, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
const MAX_IDS: usize = 200;
const MAX_SEASONS: usize = 1000;
const MAX_RESPONSE: usize = 8 * 1024 * 1024;
#[derive(Debug)]
struct Error(StatusCode, &'static str, &'static str);
type Result<T> = std::result::Result<T, Error>;
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (
            self.0,
            Json(serde_json::json!({"error":{"code":self.1,"message":self.2}})),
        )
            .into_response()
    }
}
impl From<libsql::Error> for Error {
    fn from(_: libsql::Error) -> Self {
        Self(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database_error",
            "Library operation failed; no partial write committed",
        )
    }
}
fn bad(s: &'static str) -> Error {
    Error(StatusCode::BAD_REQUEST, "invalid_request", s)
}
fn conflict() -> Error {
    Error(
        StatusCode::CONFLICT,
        "library_conflict",
        "Catalog identity or library path conflicts with existing records",
    )
}
fn missing() -> Error {
    Error(
        StatusCode::NOT_FOUND,
        "library_not_found",
        "One or more library records or seasons do not exist",
    )
}
#[derive(Clone, Copy, PartialEq)]
enum Domain {
    Tv,
    Movies,
}
impl Domain {
    fn name(self) -> &'static str {
        if self == Self::Tv { "tv" } else { "movies" }
    }
    fn table(self) -> &'static str {
        if self == Self::Tv { "series" } else { "movies" }
    }
    fn target(self) -> &'static str {
        if self == Self::Tv {
            "series_id"
        } else {
            "movie_id"
        }
    }
}
#[derive(Clone)]
struct Context {
    db: Arc<Database>,
    domain: Domain,
}
#[derive(Clone, Debug, Default)]
pub enum Change<T> {
    #[default]
    Missing,
    Null,
    Value(T),
}
impl<T: Serialize> Serialize for Change<T> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Self::Value(v) => v.serialize(s),
            Self::Missing | Self::Null => s.serialize_none(),
        }
    }
}
impl<T> Change<T> {
    fn is_missing(&self) -> bool {
        matches!(self, Self::Missing)
    }
}
// Patch fields distinguish omission from explicit null without exposing an untyped DTO.
fn change<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> std::result::Result<Change<T>, D::Error> {
    Ok(Option::<T>::deserialize(d)?
        .map(Change::Value)
        .unwrap_or(Change::Null))
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SeriesType {
    Standard,
    Daily,
    Anime,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NewItems {
    All,
    None,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    Tba,
    Announced,
    InCinemas,
    Released,
}
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    #[serde(
        default,
        deserialize_with = "change",
        skip_serializing_if = "Change::is_missing"
    )]
    pub monitored: Change<bool>,
    #[serde(
        default,
        deserialize_with = "change",
        skip_serializing_if = "Change::is_missing"
    )]
    pub quality_profile_id: Change<i64>,
    #[serde(
        default,
        deserialize_with = "change",
        skip_serializing_if = "Change::is_missing"
    )]
    pub series_type: Change<SeriesType>,
    #[serde(
        default,
        deserialize_with = "change",
        skip_serializing_if = "Change::is_missing"
    )]
    pub season_folder: Change<bool>,
    #[serde(
        default,
        deserialize_with = "change",
        skip_serializing_if = "Change::is_missing"
    )]
    pub use_scene_numbering: Change<bool>,
    #[serde(
        default,
        deserialize_with = "change",
        skip_serializing_if = "Change::is_missing"
    )]
    pub monitor_new_items: Change<NewItems>,
    #[serde(
        default,
        deserialize_with = "change",
        skip_serializing_if = "Change::is_missing"
    )]
    pub minimum_availability: Change<Availability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seasons: Option<Vec<SeasonInput>>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SeasonInput {
    pub number: i64,
    pub monitored: bool,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Create {
    pub title: Option<String>,
    pub year: Option<i64>,
    pub tvdb_id: Option<i64>,
    pub tmdb_id: Option<i64>,
    pub imdb_id: Option<String>,
    pub metadata_id: Option<i64>,
    pub path: String,
    #[serde(default)]
    pub settings: Patch,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Bulk {
    items: Vec<Update>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Update {
    id: i64,
    patch: Patch,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Editor {
    ids: Vec<i64>,
    patch: Patch,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Page {
    limit: Option<u16>,
    offset: Option<u32>,
    ids: Option<String>,
    tvdb_id: Option<i64>,
    tmdb_id: Option<i64>,
}
#[derive(Debug, Serialize)]
pub struct Settings {
    pub quality_profile_id: Option<i64>,
    pub profile_name: Option<String>,
    pub series_type: Option<String>,
    pub season_folder: Option<bool>,
    pub use_scene_numbering: Option<bool>,
    pub monitor_new_items: Option<String>,
    pub minimum_availability: Option<String>,
    pub added: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct Statistics {
    pub total_episode_count: Option<i64>,
    pub episode_count: Option<i64>,
    pub episode_file_count: Option<i64>,
    pub file_count: i64,
    pub known_size_file_count: i64,
    pub size_on_disk: Option<i64>,
    pub season_count: Option<i64>,
}
impl Statistics {
    fn empty(d: Domain) -> Self {
        let tv = d == Domain::Tv;
        Self {
            total_episode_count: tv.then_some(0),
            episode_count: tv.then_some(0),
            episode_file_count: tv.then_some(0),
            file_count: 0,
            known_size_file_count: 0,
            size_on_disk: Some(0),
            season_count: tv.then_some(0),
        }
    }
}
#[derive(Debug, Serialize)]
pub struct Season {
    pub number: i64,
    pub monitored: bool,
    pub statistics: Statistics,
}
#[derive(Debug, Serialize)]
pub struct LibraryItem {
    pub id: i64,
    pub media_type: &'static str,
    pub metadata_id: Option<i64>,
    pub title: String,
    pub year: Option<i64>,
    pub tvdb_id: Option<i64>,
    pub tmdb_id: Option<i64>,
    pub imdb_id: Option<String>,
    pub path: String,
    pub poster: Option<String>,
    pub monitored: bool,
    pub settings: Settings,
    pub statistics: Statistics,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seasons: Option<Vec<Season>>,
    pub is_available: Option<bool>,
}
#[derive(Serialize)]
pub struct LibraryPage {
    pub items: Vec<LibraryItem>,
    pub total: i64,
    pub limit: u16,
    pub offset: u32,
}
#[derive(Serialize)]
pub struct LegacySeries {
    pub id: i64,
    pub title: String,
    pub year: Option<i64>,
    pub path: String,
    pub poster: Option<String>,
}

pub fn router(db: Arc<Database>) -> Router {
    let tv = Router::new()
        .route("/api/v1/tv/series", get(list).post(create))
        .route("/api/v1/tv/series/bulk", put(bulk))
        .route("/api/v1/tv/series/editor", put(editor))
        .route("/api/v1/tv/series/{id}", get(detail).put(update))
        .route("/api/v1/series", get(legacy))
        .with_state(Context {
            db: db.clone(),
            domain: Domain::Tv,
        });
    let movies = Router::new()
        .route("/api/v1/movies", get(list).post(create))
        .route("/api/v1/movies/bulk", put(bulk))
        .route("/api/v1/movies/editor", put(editor))
        .route("/api/v1/movies/{id}", get(detail).put(update))
        .with_state(Context {
            db,
            domain: Domain::Movies,
        });
    tv.merge(movies).layer(DefaultBodyLimit::max(256 * 1024))
}
fn no_query(query: RawQuery) -> Result<()> {
    if query.0.is_some_and(|q| !q.is_empty()) {
        return Err(bad(
            "This library route does not accept query parameters or effect flags",
        ));
    }
    Ok(())
}
fn body<T>(b: std::result::Result<Json<T>, JsonRejection>) -> Result<T> {
    b.map(|b| b.0).map_err(|e| {
        if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
            Error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "body_too_large",
                "Library request exceeds 256 KiB",
            )
        } else {
            bad("Expected documented library JSON fields and types")
        }
    })
}
fn positive(id: i64) -> Result<i64> {
    if id > 0 {
        Ok(id)
    } else {
        Err(bad("Ids must be positive"))
    }
}
fn ids(ids: Vec<i64>) -> Result<Vec<i64>> {
    if ids.is_empty()
        || ids.len() > MAX_IDS
        || ids.iter().any(|v| *v <= 0)
        || ids.iter().copied().collect::<BTreeSet<_>>().len() != ids.len()
    {
        return Err(bad("Expected 1 to 200 distinct positive ids"));
    }
    Ok(ids)
}
fn placeholders(n: usize) -> String {
    vec!["?"; n].join(",")
}
fn budget<T: Serialize>(value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value)
        .map_err(|_| bad("Invalid stored library data"))?
        .len();
    if bytes + 1024 > MAX_RESPONSE {
        return Err(Error(
            StatusCode::CONFLICT,
            "pagination_required",
            "Library response exceeds 8 MiB; use a smaller selection",
        ));
    }
    Ok(())
}
async fn list(
    State(ctx): State<Context>,
    q: std::result::Result<Query<Page>, QueryRejection>,
) -> Result<Json<LibraryPage>> {
    let q = q.map_err(|_| bad("Invalid library query"))?.0;
    let limit = q.limit.unwrap_or(100);
    let offset = q.offset.unwrap_or(0);
    if limit == 0 || limit > 500 {
        return Err(bad("Limit must be 1 to 500"));
    }
    if (ctx.domain == Domain::Tv && q.tmdb_id.is_some())
        || (ctx.domain == Domain::Movies && q.tvdb_id.is_some())
        || usize::from(q.ids.is_some())
            + usize::from(q.tvdb_id.is_some())
            + usize::from(q.tmdb_id.is_some())
            > 1
    {
        return Err(bad(
            "Choose at most one domain-appropriate library selector",
        ));
    }
    let (filter, values) = if let Some(raw) = q.ids {
        if raw.len() > 4200 || raw.split(',').count() > MAX_IDS {
            return Err(bad("Too many library ids"));
        }
        let selected = ids(raw
            .split(',')
            .map(|v| v.parse().map_err(|_| bad("Invalid library ids")))
            .collect::<Result<_>>()?)?;
        (
            format!("r.id IN ({})", placeholders(selected.len())),
            selected.into_iter().map(Value::Integer).collect(),
        )
    } else if let Some(id) = q.tvdb_id {
        ("r.tvdb_id=?".into(), vec![Value::Integer(positive(id)?)])
    } else if let Some(id) = q.tmdb_id {
        ("m.tmdb_id=?".into(), vec![Value::Integer(positive(id)?)])
    } else {
        ("1=1".into(), vec![])
    };
    let c = ctx.db.connect().await?;
    let tx = c.transaction().await?;
    let count_sql = format!(
        "SELECT count(*) FROM {} r {} WHERE {filter}",
        ctx.domain.table(),
        if ctx.domain == Domain::Movies {
            "JOIN movie_metadata m ON m.id=r.metadata_id"
        } else {
            ""
        }
    );
    let total = tx
        .query(&count_sql, values.clone())
        .await?
        .next()
        .await?
        .ok_or_else(missing)?
        .get(0)?;
    let items = fetch(
        &tx,
        ctx.domain,
        &filter,
        values,
        i64::from(limit),
        i64::from(offset),
        false,
    )
    .await?;
    tx.commit().await?;
    let result = LibraryPage {
        items,
        total,
        limit,
        offset,
    };
    budget(&result)?;
    Ok(Json(result))
}
async fn detail(
    query: RawQuery,
    State(ctx): State<Context>,
    Path(raw): Path<String>,
) -> Result<Json<LibraryItem>> {
    no_query(query)?;
    let id = positive(raw.parse().map_err(|_| bad("Invalid library id"))?)?;
    let c = ctx.db.connect().await?;
    let tx = c.transaction().await?;
    let item = fetch(
        &tx,
        ctx.domain,
        "r.id=?",
        vec![Value::Integer(id)],
        1,
        0,
        true,
    )
    .await?
    .pop()
    .ok_or_else(missing)?;
    tx.commit().await?;
    Ok(Json(item))
}
async fn legacy(query: RawQuery, State(ctx): State<Context>) -> Result<Json<Vec<LegacySeries>>> {
    no_query(query)?;
    let c = ctx.db.connect().await?;
    let mut rows = c
        .query(
            "SELECT id,title,year,path,poster FROM series ORDER BY title,id LIMIT 10001",
            (),
        )
        .await?;
    let mut items = vec![];
    let mut bytes = 1024;
    while let Some(r) = rows.next().await? {
        if items.len() == 10000 {
            return Err(Error(
                StatusCode::CONFLICT,
                "pagination_required",
                "Use the paged TV library route for more than 10000 series",
            ));
        }
        let item = LegacySeries {
            id: r.get(0)?,
            title: r.get(1)?,
            year: r.get(2)?,
            path: r.get(3)?,
            poster: r.get(4)?,
        };
        bytes += serde_json::to_vec(&item)
            .map_err(|_| bad("Invalid stored series"))?
            .len()
            + 1;
        if bytes > MAX_RESPONSE {
            return Err(Error(
                StatusCode::CONFLICT,
                "pagination_required",
                "Legacy series response exceeds 8 MiB; use the paged route",
            ));
        }
        items.push(item);
    }
    Ok(Json(items))
}

async fn fetch(
    c: &Connection,
    d: Domain,
    filter: &str,
    mut values: Vec<Value>,
    limit: i64,
    offset: i64,
    include_seasons: bool,
) -> Result<Vec<LibraryItem>> {
    let core = if d == Domain::Tv {
        "r.id,NULL,r.title,r.year,r.tvdb_id,NULL,NULL,r.path,r.poster,r.monitored"
    } else {
        "r.id,r.metadata_id,m.title,m.year,NULL,m.tmdb_id,m.imdb_id,r.path,NULL,r.monitored"
    };
    let sql = format!(
        "SELECT {core},s.quality_profile_id,p.name,s.series_type,s.season_folder,s.use_scene_numbering,s.monitor_new_items,s.minimum_availability,s.added FROM {} r {} LEFT JOIN library_settings s ON s.{}=r.id AND s.media_type='{}' LEFT JOIN quality_profiles p ON p.id=s.quality_profile_id AND p.media_type=s.media_type WHERE {filter} ORDER BY r.id LIMIT ? OFFSET ?",
        d.table(),
        if d == Domain::Movies {
            "JOIN movie_metadata m ON m.id=r.metadata_id"
        } else {
            ""
        },
        d.target(),
        d.name()
    );
    values.push(Value::Integer(limit));
    values.push(Value::Integer(offset));
    let mut rows = c.query(&sql, values).await?;
    let mut items = vec![];
    let mut bytes = 1024;
    while let Some(r) = rows.next().await? {
        let item = LibraryItem {
            id: r.get(0)?,
            media_type: d.name(),
            metadata_id: r.get(1)?,
            title: r.get(2)?,
            year: r.get(3)?,
            tvdb_id: r.get(4)?,
            tmdb_id: r.get(5)?,
            imdb_id: r.get(6)?,
            path: r.get(7)?,
            poster: r.get(8)?,
            monitored: r.get::<i64>(9)? != 0,
            settings: Settings {
                quality_profile_id: r.get(10)?,
                profile_name: r.get(11)?,
                series_type: r.get(12)?,
                season_folder: r.get::<Option<i64>>(13)?.map(|v| v != 0),
                use_scene_numbering: r.get::<Option<i64>>(14)?.map(|v| v != 0),
                monitor_new_items: r.get(15)?,
                minimum_availability: r.get(16)?,
                added: r.get(17)?,
            },
            statistics: Statistics::empty(d),
            seasons: if include_seasons && d == Domain::Tv {
                Some(vec![])
            } else {
                None
            },
            is_available: None,
        };
        bytes += serde_json::to_vec(&item)
            .map_err(|_| bad("Invalid stored library data"))?
            .len()
            + 1;
        if bytes > MAX_RESPONSE {
            return Err(Error(
                StatusCode::CONFLICT,
                "pagination_required",
                "Library response exceeds 8 MiB; use a smaller selection",
            ));
        }
        items.push(item);
    }
    drop(rows);
    if items.is_empty() {
        return Ok(items);
    }
    let selected: Vec<Value> = items.iter().map(|i| Value::Integer(i.id)).collect();
    let placeholders = placeholders(selected.len());
    let positions: BTreeMap<_, _> = items.iter().enumerate().map(|(i, r)| (r.id, i)).collect();
    if d == Domain::Tv {
        let mut rows=c.query(&format!("SELECT series_id,count(*),sum(episode_file_id IS NOT NULL),sum(episode_file_id IS NOT NULL OR (monitored=1 AND julianday(air_date_utc)<=julianday('now'))) FROM episodes WHERE series_id IN ({placeholders}) GROUP BY series_id"),selected.clone()).await?;
        while let Some(r) = rows.next().await? {
            let stats = &mut items[positions[&r.get::<i64>(0)?]].statistics;
            stats.total_episode_count = Some(r.get(1)?);
            stats.episode_file_count = Some(r.get(2)?);
            stats.episode_count = Some(r.get::<Option<i64>>(3)?.unwrap_or(0));
        }
        let mut rows=c.query(&format!("SELECT series_id,sum(number>0) FROM seasons WHERE series_id IN ({placeholders}) GROUP BY series_id"),selected.clone()).await?;
        while let Some(r) = rows.next().await? {
            items[positions[&r.get::<i64>(0)?]].statistics.season_count = Some(r.get(1)?);
        }
    }
    let file_table = if d == Domain::Tv {
        "episode_files"
    } else {
        "movie_files"
    };
    let target = if d == Domain::Tv {
        "episode_file_id"
    } else {
        "movie_file_id"
    };
    let mut rows=c.query(&format!("SELECT f.{owner},count(*),count(m.size),CASE WHEN count(*)=count(m.size) THEN sum(m.size) ELSE NULL END FROM {file_table} f LEFT JOIN file_metadata m ON m.{target}=f.id AND m.media_type=? WHERE f.{owner} IN ({placeholders}) GROUP BY f.{owner}",owner=d.target()),std::iter::once(Value::Text(d.name().into())).chain(selected.clone()).collect::<Vec<_>>()).await?;
    while let Some(r) = rows.next().await? {
        let stats = &mut items[positions[&r.get::<i64>(0)?]].statistics;
        stats.file_count = r.get(1)?;
        stats.known_size_file_count = r.get(2)?;
        stats.size_on_disk = r.get(3)?;
    }
    if include_seasons && d == Domain::Tv {
        let mut rows=c.query(&format!("SELECT s.series_id,s.number,s.monitored,count(e.id),count(e.episode_file_id),coalesce(sum(e.episode_file_id IS NOT NULL OR (e.monitored=1 AND julianday(e.air_date_utc)<=julianday('now'))),0) FROM seasons s LEFT JOIN episodes e ON e.series_id=s.series_id AND e.season=s.number WHERE s.series_id IN ({placeholders}) GROUP BY s.series_id,s.number ORDER BY s.series_id,s.number LIMIT 1001"),selected.clone()).await?;
        let mut count = 0;
        while let Some(r) = rows.next().await? {
            count += 1;
            if count > MAX_SEASONS {
                return Err(Error(
                    StatusCode::CONFLICT,
                    "pagination_required",
                    "Series detail exceeds 1000 seasons",
                ));
            }
            let mut statistics = Statistics::empty(d);
            statistics.total_episode_count = Some(r.get(3)?);
            statistics.episode_file_count = Some(r.get(4)?);
            statistics.episode_count = Some(r.get(5)?);
            statistics.season_count = None;
            items[positions[&r.get::<i64>(0)?]]
                .seasons
                .as_mut()
                .unwrap()
                .push(Season {
                    number: r.get(1)?,
                    monitored: r.get::<i64>(2)? != 0,
                    statistics,
                });
        }
        let mut rows=c.query(&format!("SELECT series_id,season,count(*),count(size),CASE WHEN count(*)=count(size) THEN sum(size) ELSE NULL END FROM (SELECT DISTINCT e.series_id,e.season,f.id,m.size FROM episodes e JOIN episode_files f ON f.id=e.episode_file_id AND f.series_id=e.series_id LEFT JOIN file_metadata m ON m.episode_file_id=f.id AND m.media_type='tv' WHERE e.series_id IN ({placeholders})) GROUP BY series_id,season"),selected).await?;
        while let Some(r) = rows.next().await? {
            let item = &mut items[positions[&r.get::<i64>(0)?]];
            let season_number = r.get::<i64>(1)?;
            if let Some(season) = item
                .seasons
                .as_mut()
                .unwrap()
                .iter_mut()
                .find(|s| s.number == season_number)
            {
                season.statistics.file_count = r.get(2)?;
                season.statistics.known_size_file_count = r.get(3)?;
                season.statistics.size_on_disk = r.get(4)?;
            }
        }
    }
    budget(&items)?;
    Ok(items)
}

fn season_ids(seasons: &[SeasonInput]) -> Result<()> {
    if seasons.len() > MAX_SEASONS
        || seasons.iter().any(|s| s.number < 0)
        || seasons
            .iter()
            .map(|s| s.number)
            .collect::<BTreeSet<_>>()
            .len()
            != seasons.len()
    {
        return Err(bad(
            "Expected at most 1000 distinct nonnegative season numbers",
        ));
    }
    Ok(())
}
fn changed<T>(value: &Change<T>) -> bool {
    !matches!(value, Change::Missing)
}
fn enum_value<T: Serialize>(value: &Change<T>) -> Option<Value> {
    match value {
        Change::Missing => None,
        Change::Null => Some(Value::Null),
        Change::Value(v) => Some(Value::Text(
            serde_json::to_value(v)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned(),
        )),
    }
}
fn boolean_value(value: &Change<bool>) -> Option<Value> {
    match value {
        Change::Missing => None,
        Change::Null => Some(Value::Null),
        Change::Value(v) => Some(Value::Integer(i64::from(*v))),
    }
}
fn patch_fields(d: Domain, p: &Patch) -> Result<Vec<(&'static str, Value)>> {
    if matches!(p.monitored, Change::Null) {
        return Err(bad("Monitoring cannot be null"));
    }
    if (d == Domain::Tv && changed(&p.minimum_availability))
        || (d == Domain::Movies
            && (changed(&p.series_type)
                || changed(&p.season_folder)
                || changed(&p.use_scene_numbering)
                || changed(&p.monitor_new_items)
                || p.seasons.is_some()))
    {
        return Err(bad("Library setting belongs to another domain"));
    }
    let mut fields = vec![];
    if let Some(v) = enum_value(&p.series_type) {
        fields.push(("series_type", v));
    }
    if let Some(v) = enum_value(&p.monitor_new_items) {
        fields.push(("monitor_new_items", v));
    }
    if let Some(v) = enum_value(&p.minimum_availability) {
        fields.push(("minimum_availability", v));
    }
    if let Some(v) = boolean_value(&p.season_folder) {
        fields.push(("season_folder", v));
    }
    if let Some(v) = boolean_value(&p.use_scene_numbering) {
        fields.push(("use_scene_numbering", v));
    }
    match p.quality_profile_id {
        Change::Missing => {}
        Change::Null => fields.push(("quality_profile_id", Value::Null)),
        Change::Value(id) => fields.push(("quality_profile_id", Value::Integer(positive(id)?))),
    }
    if let Some(seasons) = &p.seasons {
        season_ids(seasons)?;
    }
    Ok(fields)
}
async fn patch(c: &Connection, d: Domain, id: i64, p: &Patch, creating: bool) -> Result<()> {
    let fields = patch_fields(d, p)?;
    if let Change::Value(profile) = p.quality_profile_id {
        let mut rows = c
            .query(
                "SELECT id FROM quality_profiles WHERE id=? AND media_type=?",
                params![profile, d.name()],
            )
            .await?;
        if rows.next().await?.is_none() {
            return Err(bad("Quality profile does not exist in this media domain"));
        }
    }
    if !creating
        && fields.is_empty()
        && !changed(&p.monitored)
        && p.seasons.as_ref().is_none_or(Vec::is_empty)
    {
        return Err(bad("Update requires a supported setting"));
    }
    c.execute(
        &format!(
            "INSERT INTO library_settings(media_type,{}) VALUES(?,?) ON CONFLICT({}) DO NOTHING",
            d.target(),
            d.target()
        ),
        params![d.name(), id],
    )
    .await?;
    for (column, value) in fields {
        c.execute(
            &format!(
                "UPDATE library_settings SET {column}=? WHERE {}=?",
                d.target()
            ),
            vec![value, Value::Integer(id)],
        )
        .await?;
    }
    if let Change::Value(monitored) = p.monitored {
        c.execute(
            &format!("UPDATE {} SET monitored=? WHERE id=?", d.table()),
            params![i64::from(monitored), id],
        )
        .await?;
    }
    if let Some(seasons) = &p.seasons {
        for season in seasons {
            if creating {
                c.execute(
                    "INSERT INTO seasons(series_id,number,monitored)VALUES(?,?,?)",
                    params![id, season.number, i64::from(season.monitored)],
                )
                .await?;
            } else {
                let old = c
                    .query(
                        "SELECT monitored FROM seasons WHERE series_id=? AND number=?",
                        params![id, season.number],
                    )
                    .await?
                    .next()
                    .await?
                    .ok_or_else(missing)?
                    .get::<i64>(0)?;
                if old != i64::from(season.monitored) {
                    c.execute(
                        "UPDATE seasons SET monitored=? WHERE series_id=? AND number=?",
                        params![i64::from(season.monitored), id, season.number],
                    )
                    .await?;
                    c.execute(
                        "UPDATE episodes SET monitored=? WHERE series_id=? AND season=?",
                        params![i64::from(season.monitored), id, season.number],
                    )
                    .await?;
                }
            }
        }
    }
    Ok(())
}
async fn update(
    query: RawQuery,
    State(ctx): State<Context>,
    Path(raw): Path<String>,
    b: std::result::Result<Json<Patch>, JsonRejection>,
) -> Result<Json<LibraryItem>> {
    no_query(query)?;
    let id = positive(raw.parse().map_err(|_| bad("Invalid library id"))?)?;
    let mut result = persist(
        &ctx,
        vec![Update {
            id,
            patch: body(b)?,
        }],
    )
    .await?;
    Ok(Json(result.remove(0)))
}
async fn bulk(
    query: RawQuery,
    State(ctx): State<Context>,
    b: std::result::Result<Json<Bulk>, JsonRejection>,
) -> Result<Json<Vec<LibraryItem>>> {
    no_query(query)?;
    Ok(Json(persist(&ctx, body(b)?.items).await?))
}
async fn editor(
    query: RawQuery,
    State(ctx): State<Context>,
    b: std::result::Result<Json<Editor>, JsonRejection>,
) -> Result<Json<Vec<LibraryItem>>> {
    no_query(query)?;
    let req = body(b)?;
    let items = ids(req.ids)?
        .into_iter()
        .map(|id| Update {
            id,
            patch: req.patch.clone(),
        })
        .collect();
    Ok(Json(persist(&ctx, items).await?))
}
async fn persist(ctx: &Context, items: Vec<Update>) -> Result<Vec<LibraryItem>> {
    let selected = ids(items.iter().map(|i| i.id).collect())?;
    for i in &items {
        patch_fields(ctx.domain, &i.patch)?;
    }
    let c = ctx.db.connect().await?;
    let tx = c
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        for item in items {
            let mut rows = tx
                .query(
                    &format!("SELECT id FROM {} WHERE id=?", ctx.domain.table()),
                    [item.id],
                )
                .await?;
            if rows.next().await?.is_none() {
                return Err(missing());
            }
            drop(rows);
            patch(&tx, ctx.domain, item.id, &item.patch, false).await?;
        }
        let include = selected.len() == 1;
        fetch(
            &tx,
            ctx.domain,
            &format!("r.id IN ({})", placeholders(selected.len())),
            selected.into_iter().map(Value::Integer).collect(),
            MAX_IDS as i64,
            0,
            include,
        )
        .await
    }
    .await;
    match outcome {
        Ok(items) => {
            tx.commit().await?;
            Ok(items)
        }
        Err(e) => {
            tx.rollback().await?;
            Err(e)
        }
    }
}

fn path(raw: &str) -> Result<String> {
    if raw.len() > 4096
        || !raw.starts_with('/')
        || raw.contains('\\')
        || raw.chars().any(char::is_control)
        || raw.split('/').any(|s| matches!(s, "." | ".."))
    {
        return Err(bad(
            "Path must be an absolute POSIX directory without traversal or control characters",
        ));
    }
    let value = format!(
        "/{}",
        raw.split('/')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("/")
    );
    if value == "/" {
        return Err(bad("A library item needs a dedicated directory"));
    }
    Ok(value)
}
fn title(raw: Option<&str>) -> Result<&str> {
    raw.filter(|s| !s.trim().is_empty() && s.len() <= 1024 && !s.chars().any(char::is_control))
        .ok_or_else(|| bad("Title must contain 1 to 1024 bytes without control characters"))
}
async fn create(
    query: RawQuery,
    State(ctx): State<Context>,
    b: std::result::Result<Json<Create>, JsonRejection>,
) -> Result<(StatusCode, Json<LibraryItem>)> {
    no_query(query)?;
    let mut req = body(b)?;
    let d = ctx.domain;
    let path = path(&req.path)?;
    if req.year.is_some_and(|v| !(1..=9999).contains(&v)) {
        return Err(bad("Year must be 1 to 9999 or null"));
    }
    if req.imdb_id.as_ref().is_some_and(|s| {
        !s.starts_with("tt")
            || s.len() < 3
            || s.len() > 32
            || !s[2..].bytes().all(|b| b.is_ascii_digit())
    }) {
        return Err(bad("IMDb id must be tt followed by digits"));
    }
    if d == Domain::Tv
        && (req.tmdb_id.is_some() || req.imdb_id.is_some() || req.metadata_id.is_some())
        || d == Domain::Movies && req.tvdb_id.is_some()
    {
        return Err(bad("Catalog identity belongs to another domain"));
    }
    patch_fields(d, &req.settings)?;
    let c = ctx.db.connect().await?;
    let tx = c
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    let outcome = create_record(&tx, d, &mut req, &path).await;
    match outcome {
        Ok(item) => {
            tx.commit().await?;
            Ok((StatusCode::CREATED, Json(item)))
        }
        Err(e) => {
            tx.rollback().await?;
            Err(e)
        }
    }
}

async fn create_record(
    c: &Connection,
    d: Domain,
    req: &mut Create,
    path: &str,
) -> Result<LibraryItem> {
    let existing=c.query("SELECT 1 FROM (SELECT rtrim(path,'/') AS path FROM series UNION ALL SELECT rtrim(path,'/') AS path FROM movies) WHERE path=?1 OR substr(path,1,length(?1)+1)=?1||'/' OR substr(?1,1,length(path)+1)=path||'/' LIMIT 1",[path]).await?.next().await?.is_some();
    if existing {
        return Err(conflict());
    }
    let id = if d == Domain::Tv {
        let tvdb_id = positive(req.tvdb_id.ok_or_else(|| bad("TVDB id is required"))?)?;
        let title = title(req.title.as_deref())?;
        if c.query("SELECT id FROM series WHERE tvdb_id=?", [tvdb_id])
            .await?
            .next()
            .await?
            .is_some()
        {
            return Err(conflict());
        }
        c.execute(
            "INSERT INTO series(tvdb_id,title,year,path)VALUES(?,?,?,?)",
            params![tvdb_id, title, req.year, path],
        )
        .await?;
        c.last_insert_rowid()
    } else {
        let metadata = if let Some(metadata) = req.metadata_id {
            if req.title.is_some()
                || req.year.is_some()
                || req.tmdb_id.is_some()
                || req.imdb_id.is_some()
            {
                return Err(bad(
                    "Choose metadata_id or explicit catalog facts, not both",
                ));
            }
            positive(metadata)?;
            if c.query("SELECT id FROM movie_metadata WHERE id=?", [metadata])
                .await?
                .next()
                .await?
                .is_none()
            {
                return Err(missing());
            }
            metadata
        } else {
            let tmdb = positive(
                req.tmdb_id
                    .ok_or_else(|| bad("TMDB id or metadata_id is required"))?,
            )?;
            let title = title(req.title.as_deref())?;
            let existing = c
                .query(
                    "SELECT id,title,year,imdb_id FROM movie_metadata WHERE tmdb_id=?",
                    [tmdb],
                )
                .await?
                .next()
                .await?;
            if let Some(row) = existing {
                if row.get::<String>(1)? != title
                    || row.get::<Option<i64>>(2)? != req.year
                    || row.get::<Option<String>>(3)? != req.imdb_id
                {
                    return Err(conflict());
                }
                row.get(0)?
            } else {
                if let Some(imdb) = &req.imdb_id {
                    if c.query(
                        "SELECT id FROM movie_metadata WHERE imdb_id=?",
                        [imdb.clone()],
                    )
                    .await?
                    .next()
                    .await?
                    .is_some()
                    {
                        return Err(conflict());
                    }
                }
                c.execute(
                    "INSERT INTO movie_metadata(tmdb_id,imdb_id,title,year)VALUES(?,?,?,?)",
                    params![tmdb, req.imdb_id.clone(), title, req.year],
                )
                .await?;
                c.last_insert_rowid()
            }
        };
        if c.query("SELECT id FROM movies WHERE metadata_id=?", [metadata])
            .await?
            .next()
            .await?
            .is_some()
        {
            return Err(conflict());
        }
        c.execute(
            "INSERT INTO movies(metadata_id,path)VALUES(?,?)",
            params![metadata, path],
        )
        .await?;
        c.last_insert_rowid()
    };
    // Explicit native creation defaults apply only to newly created membership, never upgrades.
    if d == Domain::Tv {
        if matches!(req.settings.series_type, Change::Missing) {
            req.settings.series_type = Change::Value(SeriesType::Standard);
        }
        if matches!(req.settings.season_folder, Change::Missing) {
            req.settings.season_folder = Change::Value(true);
        }
        if matches!(req.settings.use_scene_numbering, Change::Missing) {
            req.settings.use_scene_numbering = Change::Value(false);
        }
        if matches!(req.settings.monitor_new_items, Change::Missing) {
            req.settings.monitor_new_items = Change::Value(NewItems::All);
        }
    } else if matches!(req.settings.minimum_availability, Change::Missing) {
        req.settings.minimum_availability = Change::Value(Availability::Released);
    }
    patch(c, d, id, &req.settings, true).await?;
    c.execute(
        &format!(
            "UPDATE library_settings SET added=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE {}=?",
            d.target()
        ),
        [id],
    )
    .await?;
    fetch(c, d, "r.id=?", vec![Value::Integer(id)], 1, 0, true)
        .await?
        .pop()
        .ok_or_else(missing)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn patch_serialization_preserves_omission_null_and_native_enum_values() {
        for value in [
            serde_json::json!({}),
            serde_json::json!({"quality_profile_id":null}),
            serde_json::json!({"series_type":"anime","monitored":false}),
        ] {
            let patch: Patch = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(patch).unwrap(), value);
        }
    }
}
