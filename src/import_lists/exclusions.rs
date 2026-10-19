//! Import-list exclusions: items no list of the domain may add. TV exclusions are TVDB ids and movie
//! exclusions TMDb ids; the same number in the other domain is a different row.
use super::{
    error::{ApiError, Result},
    model::*,
    store::{domain_name, now, scalar},
};
use crate::{api::MediaDomain, db::Database};
use libsql::{Connection, TransactionBehavior, params};
use std::sync::Arc;

pub const MAX_PAGE: u16 = 500;

fn from_row(r: &libsql::Row) -> Result<Exclusion> {
    Ok(Exclusion {
        id: r.get(0)?,
        media_type: MediaDomain::parse(&r.get::<String>(1)?).map_err(|_| ApiError::not_found())?,
        external_id: r.get(2)?,
        title: r.get(3)?,
        year: r.get(4)?,
        created_at: r.get(5)?,
    })
}

pub async fn list(
    db: &Arc<Database>,
    media: Option<MediaDomain>,
    limit: u16,
    offset: u32,
) -> Result<ExclusionPage> {
    if !(1..=MAX_PAGE).contains(&limit) {
        return Err(ApiError::invalid("limit must be between 1 and 500"));
    }
    let c = db.connect().await?;
    let filter = media.map(|m| domain_name(m).to_owned());
    let total = match &filter {
        Some(m) => {
            scalar(
                &c,
                "SELECT count(*) FROM import_list_exclusions WHERE media_type=?",
                [m.clone()],
            )
            .await?
        }
        None => scalar(&c, "SELECT count(*) FROM import_list_exclusions", ()).await?,
    };
    let sql = "SELECT id,media_type,external_id,title,year,created_at FROM import_list_exclusions WHERE (?1 IS NULL OR media_type=?1) ORDER BY lower(title),id LIMIT ?2 OFFSET ?3";
    let mut rows = c
        .query(sql, params![filter, i64::from(limit), i64::from(offset)])
        .await?;
    let mut items = Vec::new();
    while let Some(r) = rows.next().await? {
        items.push(from_row(&r)?);
    }
    Ok(ExclusionPage {
        items,
        total,
        limit,
        offset,
    })
}

fn validate(input: &ExclusionInput) -> Result<()> {
    if !(1..=9_007_199_254_740_991).contains(&input.external_id) {
        return Err(ApiError::invalid(
            "external_id must be a positive safe integer",
        ));
    }
    let title = input.title.as_str();
    if title.is_empty()
        || title.len() > 1024
        || title != title.trim()
        || title.chars().any(char::is_control)
    {
        return Err(ApiError::invalid(
            "Title requires 1 to 1024 bytes without control characters or surrounding whitespace",
        ));
    }
    if input.year.is_some_and(|y| !(1..=9999).contains(&y)) {
        return Err(ApiError::invalid("Year must be 1 to 9999"));
    }
    Ok(())
}

/// Adds one exclusion. Idempotent: an existing row for the same domain and id is returned as is.
pub async fn add(db: &Arc<Database>, input: &ExclusionInput) -> Result<(Exclusion, bool)> {
    validate(input)?;
    let c = db.connect().await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let media = domain_name(input.media_type);
    if let Some(existing) = find(&tx, media, input.external_id).await? {
        tx.commit().await?;
        return Ok((existing, false));
    }
    let count = scalar(&tx, "SELECT count(*) FROM import_list_exclusions", ()).await?;
    if count >= 20_000 {
        return Err(ApiError::conflict(
            "import_list_exclusion_limit",
            "At most 20000 exclusions may be stored",
        ));
    }
    tx.execute(
        "INSERT INTO import_list_exclusions(media_type,external_id,title,year,created_at) VALUES(?,?,?,?,?)",
        params![media, input.external_id, input.title.clone(), input.year, now()],
    )
    .await?;
    let created = find(&tx, media, input.external_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    tx.commit().await?;
    Ok((created, true))
}
async fn find(c: &Connection, media: &str, external_id: i64) -> Result<Option<Exclusion>> {
    let mut rows = c
        .query(
            "SELECT id,media_type,external_id,title,year,created_at FROM import_list_exclusions WHERE media_type=? AND external_id=?",
            params![media, external_id],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(r) => Some(from_row(&r)?),
        None => None,
    })
}

pub async fn get(db: &Arc<Database>, id: i64) -> Result<Exclusion> {
    let c = db.connect().await?;
    let mut rows = c
        .query(
            "SELECT id,media_type,external_id,title,year,created_at FROM import_list_exclusions WHERE id=?",
            [id],
        )
        .await?;
    match rows.next().await? {
        Some(r) => from_row(&r),
        None => Err(not_found()),
    }
}
fn not_found() -> ApiError {
    ApiError::new(
        axum::http::StatusCode::NOT_FOUND,
        "import_list_exclusion_not_found",
        "Exclusion does not exist",
    )
}

pub async fn delete(db: &Arc<Database>, id: i64) -> Result<()> {
    let c = db.connect().await?;
    if c.execute("DELETE FROM import_list_exclusions WHERE id=?", [id])
        .await?
        == 0
    {
        return Err(not_found());
    }
    Ok(())
}

/// Removes several exclusions atomically; any unknown id fails the whole request.
pub async fn delete_many(db: &Arc<Database>, ids: &[i64]) -> Result<usize> {
    if ids.is_empty() || ids.len() > MAX_EXCLUSIONS_PER_REQUEST {
        return Err(ApiError::invalid("Provide 1 to 500 exclusion IDs"));
    }
    let unique: std::collections::BTreeSet<i64> = ids.iter().copied().collect();
    if unique.len() != ids.len() || unique.iter().any(|id| *id < 1) {
        return Err(ApiError::invalid(
            "Exclusion IDs must be distinct and positive",
        ));
    }
    let c = db.connect().await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    for id in &unique {
        if tx
            .execute("DELETE FROM import_list_exclusions WHERE id=?", [*id])
            .await?
            == 0
        {
            return Err(not_found());
        }
    }
    tx.commit().await?;
    Ok(unique.len())
}
