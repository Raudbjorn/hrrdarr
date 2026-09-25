//! Native episode reads and atomic monitoring. File presence describes DB association, not disk existence.
use crate::db::Database;
use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, Path, Query, State,
        rejection::{JsonRejection, QueryRejection},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, put},
};
use libsql::{Connection, Value, params};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, sync::Arc};
const MAX_IDS: usize = 200;
const MAX_PAGE: u16 = 500;
const MAX_LEGACY: i64 = 10_000;
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const METADATA_COLUMNS: &[&str] = &[
    "tvdb_id",
    "air_date",
    "air_date_utc",
    "last_search_time",
    "runtime",
    "finale_type",
    "overview",
    "absolute_episode_number",
    "scene_absolute_episode_number",
    "scene_episode_number",
    "scene_season_number",
    "unverified_scene_numbering",
    "images_json",
];
#[derive(Debug)]
struct Error(StatusCode, &'static str, &'static str);
type Result<T> = std::result::Result<T, Error>;
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (
            self.0,
            Json(crate::api::ApiErrorEnvelope::new(self.1, self.2)),
        )
            .into_response()
    }
}
impl From<libsql::Error> for Error {
    fn from(_: libsql::Error) -> Self {
        Self(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database_error",
            "Episode operation failed; no partial monitoring write committed",
        )
    }
}
fn bad(message: &'static str) -> Error {
    Error(StatusCode::BAD_REQUEST, "invalid_request", message)
}
fn missing() -> Error {
    Error(
        StatusCode::NOT_FOUND,
        "episode_not_found",
        "One or more episodes do not exist",
    )
}
fn id(raw: &str) -> Result<i64> {
    raw.parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| bad("Episode/series id must be a positive integer"))
}
fn query<T>(q: std::result::Result<Query<T>, QueryRejection>) -> Result<T> {
    q.map(|v| v.0)
        .map_err(|_| bad("Invalid episode query parameters"))
}
fn body<T>(b: std::result::Result<Json<T>, JsonRejection>) -> Result<T> {
    b.map(|v| v.0).map_err(|e| {
        if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
            Error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "body_too_large",
                "Episode request exceeds 16 KiB",
            )
        } else {
            bad("Expected JSON with documented fields")
        }
    })
}
#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(rename = "EpisodeSeriesProjection")]
pub struct SeriesProjection {
    pub id: i64,
    pub tvdb_id: Option<i64>,
    pub title: String,
    pub year: Option<i64>,
    pub path: String,
    pub poster: Option<String>,
    pub monitored: bool,
}
#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(rename = "EpisodeFileProjection")]
pub struct FileProjection {
    pub id: i64,
    pub series_id: i64,
    pub path: String,
}
#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(rename = "Episode")]
pub struct Episode {
    pub id: i64,
    pub series_id: i64,
    pub season: i64,
    pub number: i64,
    pub title: String,
    pub monitored: bool,
    pub episode_file_id: Option<i64>,
    pub has_file: bool,
    pub file_path: Option<String>,
    pub tvdb_id: Option<i64>,
    pub air_date: Option<String>,
    pub air_date_utc: Option<String>,
    pub last_search_time: Option<String>,
    pub runtime: Option<i64>,
    pub finale_type: Option<String>,
    pub overview: Option<String>,
    pub absolute_episode_number: Option<i64>,
    pub scene_absolute_episode_number: Option<i64>,
    pub scene_episode_number: Option<i64>,
    pub scene_season_number: Option<i64>,
    pub unverified_scene_numbering: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub series: Option<SeriesProjection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub episode_file: Option<FileProjection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub images: Option<Option<Vec<EpisodeCover>>>,
}
#[derive(Default, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "EpisodeIncludes")]
pub struct Includes {
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    include_series: bool,
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    include_episode_file: bool,
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    include_images: bool,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "EpisodeQuery", optional_fields)]
pub struct ListQuery {
    series_id: Option<i64>,
    season: Option<i64>,
    episode_ids: Option<String>,
    episode_file_id: Option<i64>,
    #[serde(default)]
    #[ts(as = "Option<u32>", optional)]
    offset: u32,
    #[serde(default = "page_size")]
    #[ts(as = "Option<u16>", optional)]
    limit: u16,
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    include_series: bool,
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    include_episode_file: bool,
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    include_images: bool,
}
fn page_size() -> u16 {
    100
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "EpisodeMonitor")]
pub struct Monitor {
    monitored: bool,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "EpisodeMonitorMany")]
pub struct MonitorMany {
    episode_ids: Vec<i64>,
    monitored: bool,
}
fn validate_ids(ids: &[i64]) -> Result<()> {
    if ids.is_empty()
        || ids.len() > MAX_IDS
        || ids.iter().any(|id| *id <= 0)
        || ids.iter().collect::<BTreeSet<_>>().len() != ids.len()
    {
        return Err(bad("Provide 1..200 distinct positive episode ids"));
    }
    Ok(())
}
fn ids_filter(ids: &[i64]) -> (String, Vec<Value>) {
    (
        format!("e.id IN ({})", vec!["?"; ids.len()].join(",")),
        ids.iter().map(|id| Value::Integer(*id)).collect(),
    )
}
pub fn router(db: Arc<Database>) -> Router {
    Router::new()
        .route("/api/v1/episodes", get(list))
        .route("/api/v1/episodes/monitor", put(monitor_many))
        .route("/api/v1/episodes/{id}", get(detail).put(monitor_one))
        .route("/api/v1/series/{id}/episodes", get(legacy))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .with_state(db)
}
const SELECT: &str = "SELECT e.id,e.series_id,e.season,e.number,e.title,e.monitored,e.episode_file_id,f.path,e.tvdb_id,e.air_date,e.air_date_utc,e.last_search_time,e.runtime,e.finale_type,e.overview,e.absolute_episode_number,e.scene_absolute_episode_number,e.scene_episode_number,e.scene_season_number,e.unverified_scene_numbering,e.images_json,s.tvdb_id,s.title,s.year,s.path,s.poster,s.monitored FROM episodes e JOIN series s ON s.id=e.series_id LEFT JOIN episode_files f ON f.id=e.episode_file_id AND f.series_id=e.series_id";
async fn fetch(
    conn: &Connection,
    filter: &str,
    mut values: Vec<Value>,
    limit: i64,
    offset: i64,
    includes: &Includes,
) -> Result<Vec<Episode>> {
    values.push(Value::Integer(limit));
    values.push(Value::Integer(offset));
    let mut select = SELECT.to_owned();
    if !includes.include_images {
        select = select.replace("e.images_json", "NULL");
    }
    if !includes.include_series {
        select = select.replace(
            "s.tvdb_id,s.title,s.year,s.path,s.poster,s.monitored",
            "NULL,NULL,NULL,NULL,NULL,NULL",
        );
    }
    let mut rows=conn.query(&format!("{select} WHERE {filter} ORDER BY e.series_id,e.season,e.number,e.id LIMIT ? OFFSET ?"),values).await?;
    let mut items = Vec::new();
    let mut response_bytes = 1024usize;
    while let Some(row) = rows.next().await? {
        response_bytes += 512;
        for column in 0..row.column_count() {
            if let Value::Text(text) = row.get_value(column)? {
                response_bytes = response_bytes.saturating_add(text.len().saturating_mul(6));
            }
        }
        if response_bytes > MAX_RESPONSE_BYTES {
            return Err(Error(
                StatusCode::CONFLICT,
                "pagination_required",
                "Episode response exceeds byte budget; request a smaller page or fewer ids",
            ));
        }

        let series_id = row.get(1)?;
        let file_id = row.get::<Option<i64>>(6)?;
        let path = row.get::<Option<String>>(7)?;
        let images = if includes.include_images {
            Some(match row.get::<Option<String>>(20)? {
                None => None,
                Some(value) => Some(serde_json::from_str(&value).map_err(|_| {
                    Error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "metadata_invalid",
                        "Stored episode images are invalid",
                    )
                })?),
            })
        } else {
            None
        };
        items.push(Episode {
            id: row.get(0)?,
            series_id,
            season: row.get(2)?,
            number: row.get(3)?,
            title: row.get(4)?,
            monitored: row.get::<i64>(5)? != 0,
            episode_file_id: file_id,
            has_file: file_id.is_some(),
            file_path: path.clone(),
            tvdb_id: row.get(8)?,
            air_date: row.get(9)?,
            air_date_utc: row.get(10)?,
            last_search_time: row.get(11)?,
            runtime: row.get(12)?,
            finale_type: row.get(13)?,
            overview: row.get(14)?,
            absolute_episode_number: row.get(15)?,
            scene_absolute_episode_number: row.get(16)?,
            scene_episode_number: row.get(17)?,
            scene_season_number: row.get(18)?,
            unverified_scene_numbering: row.get::<Option<i64>>(19)?.map(|v| v != 0),
            images,
            series: if includes.include_series {
                Some(SeriesProjection {
                    id: series_id,
                    tvdb_id: row.get(21)?,
                    title: row.get(22)?,
                    year: row.get(23)?,
                    path: row.get(24)?,
                    poster: row.get(25)?,
                    monitored: row.get::<i64>(26)? != 0,
                })
            } else {
                None
            },
            episode_file: if includes.include_episode_file {
                file_id.zip(path).map(|(id, path)| FileProjection {
                    id,
                    series_id,
                    path,
                })
            } else {
                None
            },
        });
    }
    Ok(items)
}
async fn list(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<ListQuery>, QueryRejection>,
) -> Result<Json<crate::api::ApiPage<Episode>>> {
    let q = query(q)?;
    if q.limit == 0 || q.limit > MAX_PAGE {
        return Err(bad("Page limit must be 1..500"));
    }
    if usize::from(q.series_id.is_some())
        + usize::from(q.episode_ids.is_some())
        + usize::from(q.episode_file_id.is_some())
        != 1
    {
        return Err(bad(
            "Choose exactly one selector: series_id, episode_ids or episode_file_id",
        ));
    }
    if q.season.is_some() && q.series_id.is_none() {
        return Err(bad("season requires series_id"));
    }
    let (filter, values) = if let Some(series) = q.series_id {
        if series <= 0 || q.season.is_some_and(|n| n < 0) {
            return Err(bad("series_id must be positive and season nonnegative"));
        }
        if let Some(season) = q.season {
            (
                "e.series_id=? AND e.season=?".into(),
                vec![Value::Integer(series), Value::Integer(season)],
            )
        } else {
            ("e.series_id=?".into(), vec![Value::Integer(series)])
        }
    } else if let Some(file) = q.episode_file_id {
        if file <= 0 {
            return Err(bad("episode_file_id must be positive"));
        }
        ("e.episode_file_id=?".into(), vec![Value::Integer(file)])
    } else {
        let raw = q.episode_ids.as_deref().unwrap_or("");
        if raw.len() > 4200 {
            return Err(bad("Episode id list too large"));
        }
        let ids = raw.split(',').map(id).collect::<Result<Vec<_>>>()?;
        validate_ids(&ids)?;
        ids_filter(&ids)
    };
    let conn = db.connect().await?;
    let tx = conn.transaction().await?;
    let total = tx
        .query(
            &format!("SELECT count(*) FROM episodes e WHERE {filter}"),
            values.clone(),
        )
        .await?
        .next()
        .await?
        .ok_or_else(missing)?
        .get::<i64>(0)?;
    let includes = Includes {
        include_series: q.include_series,
        include_episode_file: q.include_episode_file,
        include_images: q.include_images,
    };
    let items = fetch(
        &tx,
        &filter,
        values,
        i64::from(q.limit),
        i64::from(q.offset),
        &includes,
    )
    .await?;
    tx.rollback().await?;
    Ok(Json(crate::api::ApiPage {
        items,
        total,
        offset: q.offset,
        limit: q.limit,
    }))
}
async fn detail(State(db): State<Arc<Database>>, Path(raw): Path<String>) -> Result<Json<Episode>> {
    let id = id(&raw)?;
    let conn = db.connect().await?;
    Ok(Json(
        fetch(
            &conn,
            "e.id=?",
            vec![Value::Integer(id)],
            1,
            0,
            &Includes {
                include_series: true,
                include_episode_file: true,
                include_images: true,
            },
        )
        .await?
        .pop()
        .ok_or_else(missing)?,
    ))
}
#[derive(Serialize, ts_rs::TS)]
#[ts(rename = "LegacyEpisode")]
pub struct LegacyEpisode {
    id: i64,
    series_id: i64,
    season: i64,
    number: i64,
    title: String,
    file_path: Option<String>,
}
async fn legacy(
    State(db): State<Arc<Database>>,
    Path(raw): Path<String>,
) -> Result<Json<Vec<LegacyEpisode>>> {
    let conn = db.connect().await?;
    let mut rows=conn.query("SELECT e.id,e.series_id,e.season,e.number,e.title,f.path FROM episodes e LEFT JOIN episode_files f ON f.id=e.episode_file_id AND f.series_id=e.series_id WHERE e.series_id=?1 ORDER BY e.season,e.number,e.id LIMIT ?2",params![id(&raw)?,MAX_LEGACY+1]).await?;
    let mut items = Vec::new();
    let mut bytes = 0usize;
    while let Some(row) = rows.next().await? {
        let title: String = row.get(4)?;
        let path: Option<String> = row.get(5)?;
        bytes = bytes.saturating_add(
            (title.len() + path.as_ref().map_or(0, String::len)).saturating_mul(6) + 128,
        );
        if items.len() >= MAX_LEGACY as usize || bytes > MAX_RESPONSE_BYTES {
            return Err(Error(
                StatusCode::CONFLICT,
                "pagination_required",
                "Series exceeds legacy response limit; use the paged episodes endpoint",
            ));
        }
        items.push(LegacyEpisode {
            id: row.get(0)?,
            series_id: row.get(1)?,
            season: row.get(2)?,
            number: row.get(3)?,
            title,
            file_path: path,
        });
    }
    Ok(Json(items))
}
async fn set_monitoring(
    db: &Database,
    ids: Vec<i64>,
    monitored: bool,
    includes: Includes,
) -> Result<Vec<Episode>> {
    validate_ids(&ids)?;
    let (filter, values) = ids_filter(&ids);
    let conn = db.connect().await?;
    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    let result: Result<Vec<Episode>> = async {
        let existing = fetch(
            &tx,
            &filter,
            values.clone(),
            MAX_IDS as i64,
            0,
            &Includes::default(),
        )
        .await?;
        if existing.len() != ids.len() {
            return Err(missing());
        }
        for id in &ids {
            tx.execute(
                "UPDATE episodes SET monitored=?1 WHERE id=?2",
                params![i64::from(monitored), *id],
            )
            .await?;
        }
        fetch(&tx, &filter, values, MAX_IDS as i64, 0, &includes).await
    }
    .await;
    match result {
        Ok(items) => {
            tx.commit().await?;
            Ok(items)
        }
        Err(error) => {
            tx.rollback().await?;
            Err(error)
        }
    }
}
async fn monitor_one(
    State(db): State<Arc<Database>>,
    Path(raw): Path<String>,
    b: std::result::Result<Json<Monitor>, JsonRejection>,
) -> Result<Json<Episode>> {
    Ok(Json(
        set_monitoring(
            &db,
            vec![id(&raw)?],
            body(b)?.monitored,
            Includes::default(),
        )
        .await?
        .pop()
        .ok_or_else(missing)?,
    ))
}
async fn monitor_many(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<Includes>, QueryRejection>,
    b: std::result::Result<Json<MonitorMany>, JsonRejection>,
) -> Result<Json<Vec<Episode>>> {
    let b = body(b)?;
    Ok(Json(
        set_monitoring(&db, b.episode_ids, b.monitored, query(q)?).await?,
    ))
}

/// Source calendar dates and instants are distinct. Timestamp columns represent UTC;
/// naive source SQLite timestamps in those columns are explicitly interpreted as UTC.
pub(crate) fn normalize_date(raw: &str) -> Option<String> {
    let date = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok()?;
    let canonical = date.format("%Y-%m-%d").to_string();
    (canonical == raw).then_some(canonical)
}
pub(crate) fn normalize_utc(raw: &str) -> Option<String> {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(raw) {
        return Some(
            dt.with_timezone(&chrono::Utc)
                .to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true),
        );
    }
    for format in ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S%.f"] {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(raw, format) {
            return Some(
                dt.and_utc()
                    .to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true),
            );
        }
    }
    None
}

#[derive(Debug, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
pub enum EpisodeCoverType {
    Unknown,
    Poster,
    Banner,
    Fanart,
    Screenshot,
    Headshot,
    Clearlogo,
}
#[derive(Debug, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct EpisodeCover {
    pub cover_type: EpisodeCoverType,
    #[serde(
        default,
        deserialize_with = "crate::library::change",
        skip_serializing_if = "crate::library::Change::is_missing"
    )]
    #[ts(as="Option<String>",optional=nullable)]
    pub url: crate::library::Change<String>,
    #[serde(
        default,
        deserialize_with = "crate::library::change",
        skip_serializing_if = "crate::library::Change::is_missing"
    )]
    #[ts(as="Option<String>",optional=nullable)]
    pub remote_url: crate::library::Change<String>,
}
