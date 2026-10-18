//! Read-only movie credits. Rows are keyed to catalog metadata and written only by the metadata
//! refresh/add transaction; there is no TV counterpart, so a TV id can never select a credit.
//! Images are remote provider references; local cover mapping is intentionally absent.
use crate::{
    api::{ApiErrorEnvelope, ApiPage},
    db::Database,
    metadata::{CreditImage, CreditKind},
};
use axum::{
    Json, Router,
    extract::{
        Path, Query, State,
        rejection::{PathRejection, QueryRejection},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use libsql::{TransactionBehavior, Value};
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};

const MAX_ID: i64 = 9_007_199_254_740_991;
const DEFAULT_LIMIT: u16 = 100;
const MAX_LIMIT: u16 = 500;
const TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
struct Error(StatusCode, &'static str, &'static str);
type Result<T> = std::result::Result<T, Error>;
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (self.0, Json(ApiErrorEnvelope::new(self.1, self.2))).into_response()
    }
}
impl From<libsql::Error> for Error {
    fn from(_: libsql::Error) -> Self {
        storage()
    }
}
fn storage() -> Error {
    Error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "database_error",
        "Credit operation failed",
    )
}
fn bad(message: &'static str) -> Error {
    Error(StatusCode::BAD_REQUEST, "invalid_request", message)
}
fn missing(message: &'static str) -> Error {
    Error(StatusCode::NOT_FOUND, "credit_not_found", message)
}

#[derive(Debug, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "MovieCreditQuery", optional_fields)]
pub struct CreditQuery {
    limit: Option<u16>,
    offset: Option<u32>,
    movie_id: Option<i64>,
    metadata_id: Option<i64>,
}

#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(rename = "MovieCredit")]
pub struct Credit {
    pub id: i64,
    pub metadata_id: i64,
    pub credit_tmdb_id: String,
    pub person_tmdb_id: i64,
    pub person_name: String,
    pub department: Option<String>,
    pub job: Option<String>,
    pub character: Option<String>,
    pub order: i64,
    #[serde(rename = "type")]
    #[ts(rename = "type")]
    pub kind: CreditKind,
    pub images: Vec<CreditImage>,
}

pub fn router(db: Arc<Database>) -> Router {
    Router::new()
        .route("/api/v1/movies/credits", get(list))
        .route("/api/v1/movies/credits/{id}", get(detail))
        .with_state(db)
}

fn positive(value: i64) -> Result<i64> {
    if (1..=MAX_ID).contains(&value) {
        Ok(value)
    } else {
        Err(bad("Identifiers must be positive integers"))
    }
}

const COLUMNS: &str = "c.id,c.metadata_id,c.credit_tmdb_id,c.person_tmdb_id,c.person_name,c.department,c.job,c.character,c.credit_order,c.credit_type,c.images_json";

fn decode(row: &libsql::Row) -> Result<Credit> {
    let kind = match row.get::<String>(9)?.as_str() {
        "cast" => CreditKind::Cast,
        "crew" => CreditKind::Crew,
        _ => return Err(storage()),
    };
    Ok(Credit {
        id: row.get(0)?,
        metadata_id: row.get(1)?,
        credit_tmdb_id: row.get(2)?,
        person_tmdb_id: row.get(3)?,
        person_name: row.get(4)?,
        department: row.get(5)?,
        job: row.get(6)?,
        character: row.get(7)?,
        order: row.get(8)?,
        kind,
        images: serde_json::from_str(&row.get::<String>(10)?).map_err(|_| storage())?,
    })
}

async fn list(
    State(db): State<Arc<Database>>,
    query: std::result::Result<Query<CreditQuery>, QueryRejection>,
) -> Result<Json<ApiPage<Credit>>> {
    let q = query.map_err(|_| bad("Invalid credit query"))?.0;
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT);
    let offset = q.offset.unwrap_or(0);
    if limit == 0 || limit > MAX_LIMIT {
        return Err(bad("Limit must be 1 to 500"));
    }
    if q.movie_id.is_some() && q.metadata_id.is_some() {
        return Err(bad("Choose at most one of movie_id and metadata_id"));
    }
    let movie_id = q.movie_id.map(positive).transpose()?;
    let metadata_id = q.metadata_id.map(positive).transpose()?;
    tokio::time::timeout(TIMEOUT, async {
        let c = db.connect().await?;
        let tx = c
            .transaction_with_behavior(TransactionBehavior::ReadOnly)
            .await?;
        let resolved = if let Some(id) = movie_id {
            let row = tx
                .query("SELECT metadata_id FROM movies WHERE id=?", [id])
                .await?
                .next()
                .await?
                .ok_or_else(|| missing("Movie does not exist"))?;
            Some(row.get::<i64>(0)?)
        } else if let Some(id) = metadata_id {
            tx.query("SELECT 1 FROM movie_metadata WHERE id=?", [id])
                .await?
                .next()
                .await?
                .ok_or_else(|| missing("Movie metadata does not exist"))?;
            Some(id)
        } else {
            None
        };
        let (filter, mut values) = match resolved {
            Some(id) => ("WHERE c.metadata_id=?", vec![Value::Integer(id)]),
            None => ("", vec![]),
        };
        let total = tx
            .query(
                &format!("SELECT count(*) FROM movie_credits c {filter}"),
                values.clone(),
            )
            .await?
            .next()
            .await?
            .ok_or_else(storage)?
            .get::<i64>(0)?;
        values.extend([
            Value::Integer(i64::from(limit)),
            Value::Integer(i64::from(offset)),
        ]);
        let mut rows = tx
            .query(
                &format!(
                    "SELECT {COLUMNS} FROM movie_credits c {filter} ORDER BY c.metadata_id,c.credit_type,c.credit_order,c.id LIMIT ? OFFSET ?"
                ),
                values,
            )
            .await?;
        let mut items = Vec::new();
        while let Some(row) = rows.next().await? {
            items.push(decode(&row)?);
        }
        Ok(Json(ApiPage {
            items,
            total,
            limit,
            offset,
        }))
    })
    .await
    .map_err(|_| storage())?
}

async fn detail(
    State(db): State<Arc<Database>>,
    id: std::result::Result<Path<i64>, PathRejection>,
) -> Result<Json<Credit>> {
    let id = positive(id.map_err(|_| bad("Invalid credit id"))?.0)?;
    tokio::time::timeout(TIMEOUT, async {
        let c = db.connect().await?;
        let row = c
            .query(
                &format!("SELECT {COLUMNS} FROM movie_credits c WHERE c.id=?"),
                [id],
            )
            .await?
            .next()
            .await?
            .ok_or_else(|| missing("Credit does not exist"))?;
        Ok(Json(decode(&row)?))
    })
    .await
    .map_err(|_| storage())?
}
