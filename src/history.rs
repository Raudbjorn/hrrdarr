//! Native import receipts and provenance-tagged source events; neither proves completed cleanup.
use crate::{
    api::{ApiErrorEnvelope, ApiPage, MediaDomain},
    db::{Database, MediaTarget},
};
use axum::{
    Json, Router,
    extract::{Query, State, rejection::QueryRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use chrono::{DateTime, Datelike, NaiveDateTime, Utc};
use libsql::{TransactionBehavior, Value};
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};
use uuid::Uuid;
const MAX_INTEGER: i64 = 9007199254740991;
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
#[derive(Debug)]
struct Error(StatusCode, &'static str);
type Result<T> = std::result::Result<T, Error>;
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (self.0, Json(ApiErrorEnvelope::new(self.1, self.1))).into_response()
    }
}
impl From<libsql::Error> for Error {
    fn from(error: libsql::Error) -> Self {
        eprintln!(
            "event=history_storage_error kind={:?}",
            std::mem::discriminant(&error)
        );
        storage()
    }
}
fn bad() -> Error {
    Error(StatusCode::BAD_REQUEST, "invalid_history_query")
}
fn storage() -> Error {
    Error(StatusCode::INTERNAL_SERVER_ERROR, "history_storage_error")
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields)]
pub struct HistoryQuery {
    pub media_type: Option<MediaDomain>,
    pub episode_id: Option<i64>,
    pub movie_id: Option<i64>,
    pub series_id: Option<i64>,
    pub season: Option<i64>,
    pub from: Option<String>,
    pub to: Option<String>,
    #[serde(default = "page_size")]
    #[ts(as = "Option<u16>", optional)]
    pub limit: u16,
    #[serde(default)]
    #[ts(as = "Option<u32>", optional)]
    pub offset: u32,
}
fn page_size() -> u16 {
    50
}
#[derive(Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum HistoryEventType {
    FileImported,
}
#[derive(Serialize, ts_rs::TS)]
pub struct HistoricalFile {
    pub media_type: MediaDomain,
    pub id: i64,
}
#[derive(Serialize, ts_rs::TS)]
pub struct NativeHistoryEvent {
    pub id: Uuid,
    pub event_type: HistoryEventType,
    pub target: MediaTarget,
    pub file: HistoricalFile,
    pub source: String,
    pub destination: String,
    pub size_bytes: i64,
    pub sha256: String,
    pub imported_at: String,
}
#[derive(Serialize, ts_rs::TS)]
#[serde(tag = "origin", rename_all = "snake_case")]
pub enum HistoryEvent {
    NativeImport(NativeHistoryEvent),
    SourceSnapshot(SourceHistoryEvent),
}
#[derive(Serialize, ts_rs::TS)]
pub struct SourceHistoryIdentity {
    pub application: crate::snapshots::Application,
    pub fingerprint: String,
    pub source_id: i64,
}
#[derive(Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum SourceHistoryEventType {
    Grabbed,
    SeriesFolderImported,
    DownloadFolderImported,
    DownloadFailed,
    FileDeleted,
    FileRenamed,
    DownloadIgnored,
    MovieFolderImported,
}
#[derive(Serialize, ts_rs::TS)]
pub struct SourceHistoryEvent {
    pub id: SourceHistoryIdentity,
    pub event_type: SourceHistoryEventType,
    pub source_event_type: i64,
    pub target: MediaTarget,
    pub occurred_at: String,
    pub source_title: Option<String>,
    pub download_id: Option<String>,
    pub quality: Option<crate::media_files::FileQuality>,
    pub languages: Option<Vec<i64>>,
}
/// Storage and query bounds share one canonical representation. Trailing fractional zeroes
/// would otherwise make equal instants compare differently in SQLite's text ordering.
pub(crate) fn canonical_timestamp(value: &DateTime<Utc>) -> Option<String> {
    if !(0..=9999).contains(&value.year()) || value.timestamp_subsec_nanos() >= 1_000_000_000 {
        return None;
    }
    let text = value.format("%Y-%m-%d %H:%M:%S%.f").to_string();
    Some(if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        text
    })
}
fn date(raw: &str) -> Result<DateTime<Utc>> {
    if raw.len() > 40
        || raw.split_once('.').is_some_and(|(_, fraction)| {
            fraction.bytes().take_while(u8::is_ascii_digit).count() > 9
        })
    {
        return Err(bad());
    }
    let value = DateTime::parse_from_rfc3339(raw)
        .map_err(|_| bad())?
        .with_timezone(&Utc);
    if canonical_timestamp(&value).is_none() {
        return Err(bad());
    }
    Ok(value)
}
fn filter(q: &HistoryQuery) -> Result<(String, Vec<Value>)> {
    if !(1..=100).contains(&q.limit) || q.offset > 10000 {
        return Err(bad());
    }
    if [q.episode_id, q.movie_id, q.series_id]
        .into_iter()
        .flatten()
        .any(|v| !(1..=MAX_INTEGER).contains(&v))
        || q.season.is_some_and(|v| !(0..=MAX_INTEGER).contains(&v))
    {
        return Err(bad());
    }
    let tv = q.episode_id.is_some() || q.series_id.is_some() || q.season.is_some();
    if (tv && (q.movie_id.is_some() || q.media_type == Some(MediaDomain::Movies)))
        || (q.movie_id.is_some() && q.media_type == Some(MediaDomain::Tv))
        || (q.season.is_some() && q.series_id.is_none())
    {
        return Err(bad());
    }
    let from = q.from.as_deref().map(date).transpose()?;
    let to = q.to.as_deref().map(date).transpose()?;
    if from.zip(to).is_some_and(|(from, to)| from >= to) {
        return Err(bad());
    }
    let mut conditions = Vec::new();
    let mut values = Vec::new();
    if let Some(media) = q.media_type {
        conditions.push("h.media_type=?".to_string());
        values.push(Value::Text(
            if media == MediaDomain::Tv {
                "episode"
            } else {
                "movie"
            }
            .into(),
        ));
    }
    for (column, value) in [("h.episode_id", q.episode_id), ("h.movie_id", q.movie_id)] {
        if let Some(value) = value {
            conditions.push(format!("{column}=?"));
            values.push(Value::Integer(value));
        }
    }
    if let Some(series) = q.series_id {
        let mut sql =
            "EXISTS(SELECT 1 FROM episodes e WHERE e.id=h.episode_id AND e.series_id=?".to_string();
        values.push(Value::Integer(series));
        if let Some(season) = q.season {
            sql.push_str(" AND e.season=?");
            values.push(Value::Integer(season));
        }
        sql.push(')');
        conditions.push(sql);
    }
    for (comparison, value) in [(">=", from), ("<", to)] {
        if let Some(value) = value {
            conditions.push(format!("h.imported_at{comparison}?"));
            values.push(Value::Text(canonical_timestamp(&value).ok_or_else(bad)?));
        }
    }
    Ok((
        if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        },
        values,
    ))
}
fn positive(value: i64) -> Result<i64> {
    if (1..=MAX_INTEGER).contains(&value) {
        Ok(value)
    } else {
        Err(storage())
    }
}
fn native_event(row: libsql::Row) -> Result<NativeHistoryEvent> {
    let id = Uuid::parse_str(&row.get::<String>(0)?).map_err(|_| storage())?;
    let media = row.get::<String>(1)?;
    let (target, file) = match media.as_str() {
        "episode" => (
            MediaTarget::Episode(positive(row.get(2)?)?),
            HistoricalFile {
                media_type: MediaDomain::Tv,
                id: positive(row.get(4)?)?,
            },
        ),
        "movie" => (
            MediaTarget::Movie(positive(row.get(3)?)?),
            HistoricalFile {
                media_type: MediaDomain::Movies,
                id: positive(row.get(5)?)?,
            },
        ),
        _ => return Err(storage()),
    };
    let size_bytes = row.get::<i64>(8)?;
    if !(0..=MAX_INTEGER).contains(&size_bytes) {
        return Err(storage());
    }
    let raw = row.get::<String>(10)?;
    let timestamp =
        NaiveDateTime::parse_from_str(&raw, "%Y-%m-%d %H:%M:%S").map_err(|_| storage())?;
    if !(0..=9999).contains(&timestamp.year())
        || timestamp.format("%Y-%m-%d %H:%M:%S").to_string() != raw
    {
        return Err(storage());
    }
    Ok(NativeHistoryEvent {
        id,
        event_type: HistoryEventType::FileImported,
        target,
        file,
        source: row.get(6)?,
        destination: row.get(7)?,
        size_bytes,
        sha256: row.get(9)?,
        imported_at: timestamp
            .and_utc()
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    })
}

fn source_event(row: libsql::Row) -> Result<SourceHistoryEvent> {
    let application = match row.get::<String>(12)?.as_str() {
        "sonarr" => crate::snapshots::Application::Sonarr,
        "radarr" => crate::snapshots::Application::Radarr,
        _ => return Err(storage()),
    };
    let target = match row.get::<String>(1)?.as_str() {
        "episode" => MediaTarget::Episode(positive(row.get(2)?)?),
        "movie" => MediaTarget::Movie(positive(row.get(3)?)?),
        _ => return Err(storage()),
    };
    let raw = row.get::<String>(10)?;
    let timestamp = NaiveDateTime::parse_from_str(&raw, "%Y-%m-%d %H:%M:%S%.f")
        .map_err(|_| storage())?
        .and_utc();
    if canonical_timestamp(&timestamp).as_deref() != Some(raw.as_str()) {
        return Err(storage());
    }
    let event_type =
        serde_json::from_value(serde_json::Value::String(row.get(15)?)).map_err(|_| storage())?;
    let quality_id = row.get::<Option<i64>>(19)?;
    let revision = row
        .get::<Option<String>>(20)?
        .map(|value| serde_json::from_str(&value).map_err(|_| storage()))
        .transpose()?;
    let quality = quality_id.map(|quality_id| crate::media_files::FileQuality {
        quality_id,
        revision,
    });
    let languages = row
        .get::<Option<String>>(21)?
        .map(|value| serde_json::from_str(&value).map_err(|_| storage()))
        .transpose()?;
    Ok(SourceHistoryEvent {
        id: SourceHistoryIdentity {
            application,
            fingerprint: row.get(13)?,
            source_id: positive(row.get(14)?)?,
        },
        event_type,
        source_event_type: row.get(16)?,
        target,
        occurred_at: timestamp.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true),
        source_title: row.get(17)?,
        download_id: row.get(18)?,
        quality,
        languages,
    })
}
fn event(row: libsql::Row) -> Result<HistoryEvent> {
    match row.get::<i64>(11)? {
        0 => native_event(row).map(HistoryEvent::NativeImport),
        1 => source_event(row).map(HistoryEvent::SourceSnapshot),
        _ => Err(storage()),
    }
}
// Bound compact identities before combining origins; fetching paths/metadata before LIMIT
// made SQLite materialize and sort the entire historical payload in the measured union plan.
pub(crate) fn page_sql(predicate: &str) -> String {
    let source_predicate = predicate.replace("h.imported_at", "h.occurred_at");
    format!("WITH native_keys AS (
        SELECT h.operation_id AS native_id,h.imported_at AS event_at FROM import_history h {predicate}
        ORDER BY h.imported_at DESC,h.operation_id DESC LIMIT ?
    ), source_keys AS (
        SELECT h.application,h.fingerprint,h.source_id,h.occurred_at AS event_at FROM snapshot_history_events h {source_predicate}
        ORDER BY h.occurred_at DESC,h.application ASC,h.fingerprint ASC,h.source_id DESC LIMIT ?
    ), selected AS (
        SELECT * FROM (
            SELECT 0 AS origin_rank,native_id,event_at,NULL AS application,NULL AS fingerprint,NULL AS source_id FROM native_keys
            UNION ALL
            SELECT 1,NULL,event_at,application,fingerprint,source_id FROM source_keys
        ) ORDER BY event_at DESC,origin_rank ASC,native_id DESC,application ASC,fingerprint ASC,source_id DESC LIMIT ? OFFSET ?
    ) SELECT n.operation_id,COALESCE(n.media_type,e.media_type),COALESCE(n.episode_id,e.episode_id),COALESCE(n.movie_id,e.movie_id),
        n.episode_file_id,n.movie_file_id,n.source,n.destination,n.size,n.sha256,s.event_at,s.origin_rank,
        e.application,e.fingerprint,e.source_id,e.event_type,e.source_event_type,e.source_title,e.download_id,e.quality_id,e.quality_revision_json,e.languages_json
    FROM selected s
    LEFT JOIN import_history n ON s.origin_rank=0 AND n.operation_id=s.native_id
    LEFT JOIN snapshot_history_events e ON s.origin_rank=1 AND e.application=s.application AND e.fingerprint=s.fingerprint AND e.source_id=s.source_id
    ORDER BY s.event_at DESC,s.origin_rank ASC,s.native_id DESC,s.application ASC,s.fingerprint ASC,s.source_id DESC")
}
fn count_sql(predicate: &str) -> String {
    let source_predicate = predicate.replace("h.imported_at", "h.occurred_at");
    format!(
        "SELECT (SELECT count(*) FROM import_history h {predicate}) + (SELECT count(*) FROM snapshot_history_events h {source_predicate})"
    )
}
pub fn router(db: Arc<Database>) -> Router {
    Router::new()
        .route("/api/v1/history", get(list))
        .with_state(db)
}
async fn list(
    State(db): State<Arc<Database>>,
    query: std::result::Result<Query<HistoryQuery>, QueryRejection>,
) -> Result<Json<ApiPage<HistoryEvent>>> {
    let q = query.map_err(|_| bad())?.0;
    let (predicate, values) = filter(&q)?;
    tokio::time::timeout(Duration::from_secs(5), async {
        let c = db.connect().await?;
        let tx = c
            .transaction_with_behavior(TransactionBehavior::ReadOnly)
            .await?;
        let result = async {
            let count_values = values
                .iter()
                .chain(values.iter())
                .cloned()
                .collect::<Vec<_>>();
            let total = tx
                .query(&count_sql(&predicate), count_values)
                .await?
                .next()
                .await?
                .ok_or_else(storage)?
                .get::<i64>(0)?;
            if !(0..=MAX_INTEGER).contains(&total) {
                return Err(storage());
            }
            let candidate_limit = i64::from(q.offset) + i64::from(q.limit);
            let mut page_values = values.clone();
            page_values.push(Value::Integer(candidate_limit));
            page_values.extend(values);
            page_values.push(Value::Integer(candidate_limit));
            page_values.push(Value::Integer(i64::from(q.limit)));
            page_values.push(Value::Integer(i64::from(q.offset)));
            let mut rows = tx.query(&page_sql(&predicate), page_values).await?;
            let mut items = Vec::new();
            while let Some(row) = rows.next().await? {
                items.push(event(row)?)
            }
            let page = ApiPage {
                items,
                total,
                limit: q.limit,
                offset: q.offset,
            };
            bounded(page)
        }
        .await;
        match result {
            Ok(page) => {
                tx.commit().await?;
                Ok(page)
            }
            Err(error) => {
                tx.rollback().await?;
                Err(error)
            }
        }
    })
    .await
    .map_err(|_| Error(StatusCode::SERVICE_UNAVAILABLE, "history_timeout"))?
}

fn bounded(page: ApiPage<HistoryEvent>) -> Result<Json<ApiPage<HistoryEvent>>> {
    if serde_json::to_vec(&page).map_err(|_| storage())?.len() > MAX_RESPONSE_BYTES {
        return Err(Error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "history_response_limit",
        ));
    }
    Ok(Json(page))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn escaped_history_paths_respect_serialized_response_limit() {
        let page = |length| ApiPage {
            items: (0..100)
                .map(|_| {
                    HistoryEvent::NativeImport(NativeHistoryEvent {
                        id: Uuid::new_v4(),
                        event_type: HistoryEventType::FileImported,
                        target: MediaTarget::Movie(1),
                        file: HistoricalFile {
                            media_type: MediaDomain::Movies,
                            id: 1,
                        },
                        source: "\"".repeat(length),
                        destination: "\"".repeat(length),
                        size_bytes: 1,
                        sha256: "a".repeat(64),
                        imported_at: "2026-09-25T00:00:00Z".into(),
                    })
                })
                .collect(),
            total: 100,
            limit: 100,
            offset: 0,
        };
        assert!(bounded(page(64)).is_ok());
        // Each path fits its storage byte limit; JSON escaping can still exceed the page limit.
        assert!(matches!(
            bounded(page(4096)),
            Err(Error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "history_response_limit"
            ))
        ));
    }
}
