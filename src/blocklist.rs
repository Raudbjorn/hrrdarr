//! Imported blocklist facts and explicit removals. No automatic download policy is implied.
use crate::{
    api::{ApiErrorEnvelope, ApiPage, MediaDomain},
    db::Database,
    snapshots::Application,
};
use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, Path, Query, State,
        rejection::{JsonRejection, QueryRejection},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get},
};
use libsql::{Connection, TransactionBehavior, Value, params};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, sync::Arc, time::Duration};
const MAX_ID: i64 = 9007199254740991;
const MAX_BYTES: usize = 1024 * 1024;
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
            "event=blocklist_storage_error kind={:?}",
            std::mem::discriminant(&error)
        );
        storage()
    }
}
fn bad() -> Error {
    Error(StatusCode::BAD_REQUEST, "invalid_blocklist_request")
}
fn positive(value: i64) -> Result<i64> {
    if (1..=MAX_ID).contains(&value) {
        Ok(value)
    } else {
        Err(storage())
    }
}
fn storage() -> Error {
    Error(StatusCode::INTERNAL_SERVER_ERROR, "blocklist_storage_error")
}
fn limit() -> Error {
    Error(StatusCode::PAYLOAD_TOO_LARGE, "blocklist_response_limit")
}
fn application(app: Application) -> &'static str {
    match app {
        Application::Sonarr => "sonarr",
        Application::Radarr => "radarr",
    }
}
#[derive(Clone, Deserialize, Serialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct BlocklistIdentity {
    pub application: Application,
    pub fingerprint: String,
    pub source_id: i64,
}
impl BlocklistIdentity {
    fn validate(&self) -> Result<()> {
        if !(1..=MAX_ID).contains(&self.source_id)
            || self.fingerprint.len() != 64
            || !self
                .fingerprint
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(bad());
        }
        Ok(())
    }
}
#[derive(Serialize, ts_rs::TS)]
#[serde(tag = "media_type", rename_all = "lowercase")]
pub enum BlocklistTarget {
    Tv {
        series_id: i64,
        episode_ids: Vec<i64>,
    },
    Movies {
        movie_id: i64,
    },
}
#[derive(Clone, Copy, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum BlocklistProtocol {
    Unknown,
    Usenet,
    Torrent,
}
#[derive(Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum BlocklistOrigin {
    SourceSnapshot,
}
#[derive(Serialize, ts_rs::TS)]
pub struct BlocklistEntry {
    pub id: BlocklistIdentity,
    pub origin: BlocklistOrigin,
    pub target: BlocklistTarget,
    pub occurred_at: String,
    pub published_at: Option<String>,
    pub source_title: String,
    pub protocol: Option<BlocklistProtocol>,
    pub size_bytes: Option<i64>,
    pub quality: Option<crate::media_files::FileQuality>,
    pub languages: Option<Vec<i64>>,
}
#[derive(Default, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum BlocklistSort {
    #[default]
    Date,
    SourceTitle,
}
#[derive(Default, Deserialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
pub enum BlocklistSortDirection {
    Asc,
    #[default]
    Desc,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields)]
pub struct BlocklistQuery {
    pub media_type: Option<MediaDomain>,
    pub series_ids: Option<String>,
    pub movie_ids: Option<String>,
    pub protocols: Option<String>,
    #[serde(default)]
    #[ts(as = "Option<BlocklistSort>", optional)]
    pub sort: BlocklistSort,
    #[serde(default)]
    #[ts(as = "Option<BlocklistSortDirection>", optional)]
    pub sort_direction: BlocklistSortDirection,
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
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct BlocklistRemoval {
    pub ids: Vec<BlocklistIdentity>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
pub fn router(db: Arc<Database>) -> Router {
    Router::new()
        .route("/api/v1/blocklist", get(list))
        .route("/api/v1/blocklist/bulk", delete(remove_bulk))
        .route(
            "/api/v1/blocklist/{application}/{fingerprint}/{source_id}",
            delete(remove_one),
        )
        .layer(DefaultBodyLimit::max(32 * 1024))
        .with_state(db)
}
fn ids(raw: &str) -> Result<Vec<Value>> {
    if raw.len() > 1800 {
        return Err(bad());
    }
    let mut ids = BTreeSet::new();
    for part in raw.split(',') {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad());
        }
        let id = part.parse::<i64>().map_err(|_| bad())?;
        if !(1..=MAX_ID).contains(&id) || !ids.insert(id) || ids.len() > 100 {
            return Err(bad());
        }
    }
    Ok(ids.into_iter().map(Value::Integer).collect())
}
fn predicate(q: &BlocklistQuery) -> Result<(String, Vec<Value>)> {
    if !(1..=100).contains(&q.limit)
        || q.offset > 10000
        || q.series_ids.is_some()
            && (q.movie_ids.is_some() || q.media_type != Some(MediaDomain::Tv))
        || q.movie_ids.is_some() && q.media_type != Some(MediaDomain::Movies)
    {
        return Err(bad());
    }
    let mut conditions = vec![];
    let mut values = vec![];
    if let Some(media) = q.media_type {
        conditions.push("media_type=?".into());
        values.push(Value::Text(
            match media {
                MediaDomain::Tv => "tv",
                MediaDomain::Movies => "movies",
            }
            .into(),
        ));
    }
    for (column, raw) in [("series_id", &q.series_ids), ("movie_id", &q.movie_ids)] {
        if let Some(raw) = raw {
            let selected = ids(raw)?;
            conditions.push(format!(
                "{column} IN ({})",
                vec!["?"; selected.len()].join(",")
            ));
            values.extend(selected)
        }
    }
    if let Some(raw) = &q.protocols {
        if raw.len() > 32 {
            return Err(bad());
        }
        let mut protocols = BTreeSet::new();
        for word in raw.split(',') {
            let value = match word {
                "unknown" => 0,
                "usenet" => 1,
                "torrent" => 2,
                _ => return Err(bad()),
            };
            if !protocols.insert(value) {
                return Err(bad());
            }
        }
        conditions.push(format!(
            "protocol IN ({})",
            vec!["?"; protocols.len()].join(",")
        ));
        values.extend(protocols.into_iter().map(Value::Integer));
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
fn timestamp(raw: String) -> Result<String> {
    let value = chrono::NaiveDateTime::parse_from_str(&raw, "%Y-%m-%d %H:%M:%S%.f")
        .map_err(|_| storage())?
        .and_utc();
    if crate::history::canonical_timestamp(&value).as_deref() != Some(raw.as_str()) {
        return Err(storage());
    }
    Ok(value.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true))
}
const COLUMNS: &str = "application,fingerprint,source_id,media_type,series_id,movie_id,occurred_at,published_at,source_title,protocol,size,quality_id,quality_revision_json,languages_json";
async fn entry(c: &Connection, row: libsql::Row) -> Result<BlocklistEntry> {
    let app = match row.get::<String>(0)?.as_str() {
        "sonarr" => Application::Sonarr,
        "radarr" => Application::Radarr,
        _ => return Err(storage()),
    };
    let id = BlocklistIdentity {
        application: app,
        fingerprint: row.get(1)?,
        source_id: row.get(2)?,
    };
    id.validate().map_err(|_| storage())?;
    let target = match row.get::<String>(3)?.as_str() {
        "tv" => {
            let series_id = positive(row.get(4)?)?;
            let mut episodes=c.query("SELECT episode_id FROM blocklist_episodes WHERE application=? AND fingerprint=? AND source_id=? ORDER BY episode_id LIMIT 10001",params![application(app),id.fingerprint.clone(),id.source_id]).await?;
            let mut episode_ids = vec![];
            while let Some(row) = episodes.next().await? {
                if episode_ids.len() == 10000 {
                    return Err(limit());
                }
                episode_ids.push(positive(row.get(0)?)?)
            }
            BlocklistTarget::Tv {
                series_id,
                episode_ids,
            }
        }
        "movies" => BlocklistTarget::Movies {
            movie_id: positive(row.get(5)?)?,
        },
        _ => return Err(storage()),
    };
    let revision = row
        .get::<Option<String>>(12)?
        .map(|s| serde_json::from_str(&s).map_err(|_| storage()))
        .transpose()?;
    let quality = row
        .get::<Option<i64>>(11)?
        .map(|quality_id| crate::media_files::FileQuality {
            quality_id,
            revision,
        });
    let protocol = match row.get::<Option<i64>>(9)? {
        None => None,
        Some(0) => Some(BlocklistProtocol::Unknown),
        Some(1) => Some(BlocklistProtocol::Usenet),
        Some(2) => Some(BlocklistProtocol::Torrent),
        _ => return Err(storage()),
    };
    Ok(BlocklistEntry {
        id,
        origin: BlocklistOrigin::SourceSnapshot,
        target,
        occurred_at: timestamp(row.get(6)?)?,
        published_at: row.get::<Option<String>>(7)?.map(timestamp).transpose()?,
        source_title: row.get(8)?,
        protocol,
        size_bytes: row.get(10)?,
        quality,
        languages: row
            .get::<Option<String>>(13)?
            .map(|s| serde_json::from_str(&s).map_err(|_| storage()))
            .transpose()?,
    })
}
pub(crate) fn page_sql(
    predicate: &str,
    sort: &BlocklistSort,
    direction: &BlocklistSortDirection,
) -> String {
    let column = match sort {
        BlocklistSort::Date => "occurred_at",
        BlocklistSort::SourceTitle => "source_title",
    };
    let direction = match direction {
        BlocklistSortDirection::Asc => "ASC",
        BlocklistSortDirection::Desc => "DESC",
    };
    // Sort/page compact identities before loading quality, languages and episode sets.
    let columns = COLUMNS
        .split(',')
        .map(|column| format!("e.{column}"))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "WITH selected AS (SELECT application,fingerprint,source_id,{column} FROM blocklist_entries {predicate} ORDER BY {column} {direction},application,fingerprint,source_id DESC LIMIT ? OFFSET ?) SELECT {columns} FROM selected s JOIN blocklist_entries e USING(application,fingerprint,source_id) ORDER BY s.{column} {direction},s.application,s.fingerprint,s.source_id DESC"
    )
}
async fn list(
    State(db): State<Arc<Database>>,
    query: std::result::Result<Query<BlocklistQuery>, QueryRejection>,
) -> Result<Json<ApiPage<BlocklistEntry>>> {
    let q = query.map_err(|_| bad())?.0;
    let (predicate, values) = predicate(&q)?;
    tokio::time::timeout(Duration::from_secs(5), async {
        let c = db.connect().await.map_err(|_| storage())?;
        let tx = c
            .transaction_with_behavior(TransactionBehavior::ReadOnly)
            .await?;
        let total = tx
            .query(
                &format!("SELECT count(*) FROM blocklist_entries {predicate}"),
                values.clone(),
            )
            .await?
            .next()
            .await?
            .ok_or_else(storage)?
            .get(0)?;
        if !(0..=MAX_ID).contains(&total) {
            return Err(storage());
        }
        let mut page_values = values;
        page_values.extend([
            Value::Integer(i64::from(q.limit)),
            Value::Integer(i64::from(q.offset)),
        ]);
        let mut rows = tx
            .query(
                &page_sql(&predicate, &q.sort, &q.sort_direction),
                page_values,
            )
            .await?;
        let mut items = vec![];
        let mut bytes = 0;
        // Decode while the source cursor is positioned on this row: libSQL Rows share statement state.
        while let Some(row) = rows.next().await? {
            let item = entry(&tx, row).await?;
            bytes += serde_json::to_vec(&item).map_err(|_| storage())?.len();
            if bytes > MAX_BYTES {
                return Err(limit());
            }
            items.push(item);
        }
        drop(rows);
        tx.commit().await?;
        let page = ApiPage {
            items,
            total,
            limit: q.limit,
            offset: q.offset,
        };
        if serde_json::to_vec(&page).map_err(|_| storage())?.len() > MAX_BYTES {
            return Err(limit());
        }
        Ok(Json(page))
    })
    .await
    .map_err(|_| Error(StatusCode::SERVICE_UNAVAILABLE, "blocklist_timeout"))?
}
async fn remove_one(
    State(db): State<Arc<Database>>,
    Path((app, fingerprint, source_id)): Path<(String, String, String)>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
) -> Result<StatusCode> {
    q.map_err(|_| bad())?;
    let application = match app.as_str() {
        "sonarr" => Application::Sonarr,
        "radarr" => Application::Radarr,
        _ => return Err(bad()),
    };
    if source_id.is_empty() || !source_id.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    remove(
        &db,
        vec![BlocklistIdentity {
            application,
            fingerprint,
            source_id: source_id.parse().map_err(|_| bad())?,
        }],
    )
    .await
}
async fn remove_bulk(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
    input: std::result::Result<Json<BlocklistRemoval>, JsonRejection>,
) -> Result<StatusCode> {
    q.map_err(|_| bad())?;
    remove(&db, input.map_err(|_| bad())?.0.ids).await
}
async fn remove(db: &Database, ids: Vec<BlocklistIdentity>) -> Result<StatusCode> {
    if ids.is_empty() || ids.len() > 100 {
        return Err(bad());
    }
    let mut unique = BTreeSet::new();
    for id in &ids {
        id.validate()?;
        if !unique.insert((application(id.application), &id.fingerprint, id.source_id)) {
            return Err(bad());
        }
    }
    tokio::time::timeout(Duration::from_secs(5),async{
        let c=db.connect().await.map_err(|_|storage())?;let tx=c.transaction_with_behavior(TransactionBehavior::Immediate).await?;
        let outcome=async{
            let mut active=vec![];
            for id in &ids {
                let row=tx.query("SELECT removed_at FROM snapshot_blocklist WHERE application=? AND fingerprint=? AND source_id=?",params![application(id.application),id.fingerprint.clone(),id.source_id]).await?.next().await?.ok_or(Error(StatusCode::NOT_FOUND,"blocklist_not_found"))?;
                if row.get::<Option<String>>(0)?.is_none(){active.push(id)}
            }
            let mut removed=0;
            for id in active {
                let changed=tx.execute("DELETE FROM blocklist_entries WHERE application=? AND fingerprint=? AND source_id=?",params![application(id.application),id.fingerprint.clone(),id.source_id]).await?;
                if changed!=1{return Err(Error(StatusCode::CONFLICT,"blocklist_state_conflict"))}removed+=1;
            }Ok(removed)
        }.await;
        match outcome{
            Ok(removed)=>{tx.commit().await?;eprintln!("event=blocklist_removed requested={} removed={removed}",ids.len());Ok(StatusCode::NO_CONTENT)},
            Err(error)=>{tx.rollback().await?;Err(error)}
        }
    }).await.map_err(|_|Error(StatusCode::SERVICE_UNAVAILABLE,"blocklist_timeout"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_target_ids_reject_values_outside_javascript_integer_range() {
        assert!(positive(-1).is_err());
        assert!(positive(0).is_err());
        assert!(positive(MAX_ID + 1).is_err());
        assert_eq!(positive(MAX_ID).unwrap(), MAX_ID);
    }
}
