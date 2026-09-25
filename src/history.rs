//! Immutable initial-import facts. An association commit is not proof of completed cleanup.
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
pub struct HistoryEvent {
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
    if !(0..=9999).contains(&value.year()) {
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
            values.push(Value::Text(
                value.format("%Y-%m-%d %H:%M:%S%.f").to_string(),
            ));
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
fn event(row: libsql::Row) -> Result<HistoryEvent> {
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
    Ok(HistoryEvent {
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
    tokio::time::timeout(Duration::from_secs(5),async{
        let c=db.connect().await?;let tx=c.transaction_with_behavior(TransactionBehavior::ReadOnly).await?;
        let result=async{
            let total=tx.query(&format!("SELECT count(*) FROM import_history h {predicate}"),values.clone()).await?.next().await?.ok_or_else(storage)?.get::<i64>(0)?;
            if !(0..=MAX_INTEGER).contains(&total){return Err(storage())}
            let mut values=values;values.push(Value::Integer(i64::from(q.limit)));values.push(Value::Integer(i64::from(q.offset)));
            let mut rows=tx.query(&format!("SELECT h.operation_id,h.media_type,h.episode_id,h.movie_id,h.episode_file_id,h.movie_file_id,h.source,h.destination,h.size,h.sha256,h.imported_at FROM import_history h {predicate} ORDER BY h.imported_at DESC,h.operation_id DESC LIMIT ? OFFSET ?"),values).await?;
            let mut items=Vec::new();while let Some(row)=rows.next().await?{items.push(event(row)?)}
            let page=ApiPage{items,total,limit:q.limit,offset:q.offset};
            bounded(page)
        }.await;
        match result{Ok(page)=>{tx.commit().await?;Ok(page)},Err(error)=>{tx.rollback().await?;Err(error)}}
    }).await.map_err(|_|Error(StatusCode::SERVICE_UNAVAILABLE,"history_timeout"))?
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
                .map(|_| HistoryEvent {
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
