//! Domain-scoped quality catalog and mutable definition settings (MiB/minute).
use crate::db::Database;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use libsql::{Connection, params};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, sync::Arc};

const MAX_BATCH: usize = 64; // Both fixed catalogs fit; oversized requests are rejected, never truncated.
const MAX_BODY_BYTES: usize = 32 * 1024;

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Quality {
    pub id: i64,
    pub name: String,
    pub source: String,
    pub resolution: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modifier: Option<String>,
}
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Definition {
    pub id: i64,
    pub media_type: String,
    pub quality: Quality,
    pub title: String,
    pub weight: i64,
    pub group_name: Option<String>,
    pub min_size: Option<f64>,
    pub max_size: Option<f64>,
    pub preferred_size: Option<f64>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Update {
    pub id: i64,
    pub title: String,
    pub min_size: Option<f64>,
    pub max_size: Option<f64>,
    pub preferred_size: Option<f64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reset {
    #[serde(default)]
    reset_titles: bool,
}

#[derive(Debug)]
pub(crate) struct Error(
    pub(crate) StatusCode,
    pub(crate) &'static str,
    pub(crate) &'static str,
);
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
            "Quality settings operation failed; no partial batch is committed",
        )
    }
}
pub(crate) type Result<T> = std::result::Result<T, Error>;
pub(crate) fn invalid(message: &'static str) -> Error {
    Error(StatusCode::BAD_REQUEST, "invalid_request", message)
}
fn absent() -> Error {
    Error(
        StatusCode::NOT_FOUND,
        "quality_not_found",
        "Quality does not exist in this media domain",
    )
}
pub(crate) fn domain(media: &str) -> Result<f64> {
    match media {
        "tv" => Ok(1000.),
        "movies" => Ok(2000.),
        _ => Err(Error(
            StatusCode::NOT_FOUND,
            "media_type_not_found",
            "Media domain must be tv or movies",
        )),
    }
}
pub(crate) fn body<T>(result: std::result::Result<Json<T>, JsonRejection>) -> Result<T> {
    result.map(|j| j.0).map_err(|e| {
        if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
            Error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "body_too_large",
                "Quality request exceeds 32 KiB",
            )
        } else {
            invalid("Expected JSON with the documented fields and finite numeric sizes")
        }
    })
}

pub fn router(db: Arc<Database>) -> Router {
    Router::new()
        .route("/api/v1/{media}/quality-definitions", get(list))
        .route("/api/v1/{media}/quality-definitions/limits", get(limits))
        .route(
            "/api/v1/{media}/quality-definitions/defaults",
            get(defaults),
        )
        .route("/api/v1/{media}/quality-definitions/bulk", put(bulk))
        .route("/api/v1/{media}/quality-definitions/reset", post(reset))
        .route(
            "/api/v1/{media}/quality-definitions/{id}",
            get(detail).put(update),
        )
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(db)
}

async fn fetch(c: &Connection, media: &str, defaults: bool) -> Result<Vec<Definition>> {
    let sizes = if defaults {
        "name,default_min,default_max,default_preferred"
    } else {
        "title,min_size,max_size,preferred_size"
    };
    let mut rows=c.query(&format!("SELECT quality_id,name,source,resolution,modifier,weight,group_name,{sizes} FROM quality_definitions WHERE media_type=?1 ORDER BY weight,quality_id"),params![media]).await?;
    let mut items = Vec::new();
    while let Some(row) = rows.next().await? {
        items.push(Definition {
            id: row.get(0)?,
            media_type: media.to_owned(),
            quality: Quality {
                id: row.get(0)?,
                name: row.get(1)?,
                source: row.get(2)?,
                resolution: row.get(3)?,
                modifier: row.get(4)?,
            },
            weight: row.get(5)?,
            group_name: row.get(6)?,
            title: row.get(7)?,
            min_size: row.get(8)?,
            max_size: row.get(9)?,
            preferred_size: row.get(10)?,
        });
    }
    Ok(items)
}
async fn list(
    State(db): State<Arc<Database>>,
    Path(media): Path<String>,
) -> Result<Json<Vec<Definition>>> {
    domain(&media)?;
    Ok(Json(fetch(&db.connect().await?, &media, false).await?))
}
async fn defaults(
    State(db): State<Arc<Database>>,
    Path(media): Path<String>,
) -> Result<Json<Vec<Definition>>> {
    domain(&media)?;
    Ok(Json(fetch(&db.connect().await?, &media, true).await?))
}
async fn limits(Path(media): Path<String>) -> Result<Json<serde_json::Value>> {
    Ok(Json(
        serde_json::json!({"min":0,"max":domain(&media)? as i64,"unit":"MiB/minute"}),
    ))
}
async fn detail(
    State(db): State<Arc<Database>>,
    Path((media, id)): Path<(String, String)>,
) -> Result<Json<Definition>> {
    domain(&media)?;
    let id = id
        .parse::<i64>()
        .map_err(|_| invalid("Quality id must be an integer"))?;
    Ok(Json(
        fetch(&db.connect().await?, &media, false)
            .await?
            .into_iter()
            .find(|d| d.id == id)
            .ok_or_else(absent)?,
    ))
}
pub(crate) fn validate_title(title: &str) -> Result<()> {
    if title.trim().is_empty() || title.chars().count() > 100 || title.chars().any(char::is_control)
    {
        return Err(invalid(
            "Title/name must contain 1..100 characters with no control characters",
        ));
    }
    Ok(())
}
pub(crate) fn validate_sizes(
    min: Option<f64>,
    max: Option<f64>,
    preferred: Option<f64>,
    limit: f64,
) -> Result<()> {
    for size in [min, max, preferred].into_iter().flatten() {
        if !size.is_finite() || size < 0. || size > limit {
            return Err(invalid("Sizes must be finite and inside the domain limits"));
        }
    }
    for (low, high) in [(min, preferred), (preferred, max), (min, max)] {
        if matches!((low,high),(Some(a),Some(b)) if a>b) {
            return Err(invalid(
                "Sizes must satisfy min <= preferred <= max, including min <= max when preferred is null",
            ));
        }
    }
    Ok(())
}
fn validate(update: &Update, limit: f64) -> Result<()> {
    validate_title(&update.title)?;
    validate_sizes(
        update.min_size,
        update.max_size,
        update.preferred_size,
        limit,
    )
}

async fn edit(db: &Database, media: &str, updates: Vec<Update>) -> Result<Vec<Definition>> {
    let limit = domain(media)?;
    if updates.is_empty() || updates.len() > MAX_BATCH {
        return Err(invalid("A batch must contain between 1 and 64 definitions"));
    }
    let mut seen = BTreeSet::new();
    for item in &updates {
        validate(item, limit)?;
        if !seen.insert(item.id) {
            return Err(invalid("A batch cannot contain duplicate quality ids"));
        }
    }
    let conn = db.connect().await?;
    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    let result:Result<Vec<Definition>>=async {
        for item in updates {
            let changed=tx.execute("UPDATE quality_definitions SET title=?1,min_size=?2,max_size=?3,preferred_size=?4 WHERE media_type=?5 AND quality_id=?6",params![item.title,item.min_size,item.max_size,item.preferred_size,media,item.id]).await?;
            if changed!=1 {return Err(absent());}
            // TV edits copy all three values, including nulls, iff at least one is set.
            // Grouped and disabled leaves share this relation and are included.
            if media == "tv" && [item.min_size,item.max_size,item.preferred_size].iter().any(Option::is_some) {
                tx.execute("UPDATE quality_profile_items SET min_size=?1,max_size=?2,preferred_size=?3 WHERE media_type='tv' AND quality_id=?4",params![item.min_size,item.max_size,item.preferred_size,item.id]).await?;
            }
        }
        fetch(&tx,media,false).await
    }.await;
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
async fn bulk(
    State(db): State<Arc<Database>>,
    Path(media): Path<String>,
    payload: std::result::Result<Json<Vec<Update>>, JsonRejection>,
) -> Result<Json<Vec<Definition>>> {
    domain(&media)?;
    Ok(Json(edit(&db, &media, body(payload)?).await?))
}
async fn update(
    State(db): State<Arc<Database>>,
    Path((media, id)): Path<(String, String)>,
    payload: std::result::Result<Json<Update>, JsonRejection>,
) -> Result<Json<Definition>> {
    domain(&media)?;
    let id = id
        .parse::<i64>()
        .map_err(|_| invalid("Quality id must be an integer"))?;
    let item = body(payload)?;
    if item.id != id {
        return Err(invalid("Body id must match route id"));
    }
    Ok(Json(
        edit(&db, &media, vec![item])
            .await?
            .into_iter()
            .find(|d| d.id == id)
            .ok_or_else(absent)?,
    ))
}
async fn reset(
    State(db): State<Arc<Database>>,
    Path(media): Path<String>,
    payload: std::result::Result<Json<Reset>, JsonRejection>,
) -> Result<Json<Vec<Definition>>> {
    domain(&media)?;
    let request = body(payload)?;
    let conn = db.connect().await?;
    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    let result:Result<Vec<Definition>>=async {
        // Pinned TV reset preserves sizes; movie reset restores defaults regardless of titles.
        if media=="movies" {
            tx.execute("UPDATE quality_definitions SET min_size=default_min,max_size=default_max,preferred_size=default_preferred WHERE media_type='movies'",()).await?;
        }
        if request.reset_titles {tx.execute("UPDATE quality_definitions SET title=name WHERE media_type=?1",params![media.clone()]).await?;}
        fetch(&tx,&media,false).await
    }.await;
    match result {
        Ok(items) => {
            tx.commit().await?;
            Ok(Json(items))
        }
        Err(error) => {
            tx.rollback().await?;
            Err(error)
        }
    }
}
