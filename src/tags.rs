//! Scoped tag catalog and shared transactional library assignment writer.
use crate::{
    api::MediaDomain,
    db::Database,
    qualities::{Error, Result, invalid},
};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State, rejection::JsonRejection},
    http::StatusCode,
    routing::get,
};
use libsql::{Connection, params};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, sync::Arc};
pub const MAX_TAGS: usize = 1024;
pub const MAX_ASSIGNMENTS: usize = 200;
#[derive(Clone, Debug, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "TagInput")]
pub struct Input {
    pub label: String,
}
#[derive(Clone, Debug, Serialize, ts_rs::TS)]
#[ts(rename = "Tag")]
pub struct Tag {
    pub id: i64,
    pub media_type: MediaDomain,
    pub label: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename = "TagAssignmentMode")]
pub enum Mode {
    Add,
    Remove,
    Replace,
}
#[derive(Clone, Debug, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "TagAssignment")]
pub struct Assignment {
    pub mode: Mode,
    pub ids: Vec<i64>,
}
#[derive(Serialize, ts_rs::TS)]
#[ts(rename = "TagDetail")]
pub struct Detail {
    pub tag: Tag,
    pub in_use: bool,
    pub owner_count: i64,
    pub delay_profile_ids: Vec<i64>,
    pub release_profile_ids: Vec<i64>,
    pub excluded_release_profile_ids: Vec<i64>,
}
#[derive(Serialize, ts_rs::TS)]
#[ts(rename = "TagOwners")]
pub struct Owners {
    pub ids: Vec<i64>,
    pub total: i64,
    pub limit: u16,
    pub offset: u32,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "TagOwnersQuery", optional_fields)]
pub struct Page {
    limit: Option<u16>,
    offset: Option<u32>,
}
pub(crate) fn names(d: MediaDomain) -> (&'static str, &'static str, &'static str) {
    match d {
        MediaDomain::Tv => ("tv", "series_tags", "series_id"),
        MediaDomain::Movies => ("movies", "movie_tags", "movie_id"),
    }
}
pub(crate) fn label(raw: &str) -> std::result::Result<String, &'static str> {
    if raw.is_empty()
        || raw.len() > 128
        || !raw.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err("Tag labels require 1 to 128 ASCII letters, digits or hyphens");
    };
    Ok(raw.to_ascii_lowercase())
}
pub(crate) fn validate(a: &Assignment) -> std::result::Result<(), &'static str> {
    if a.ids.len() > MAX_ASSIGNMENTS
        || a.ids
            .iter()
            .any(|id| !(1..=9_007_199_254_740_991).contains(id))
        || a.ids.iter().collect::<BTreeSet<_>>().len() != a.ids.len()
    {
        return Err("Tags require at most 200 distinct positive safe IDs");
    };
    Ok(())
}
fn missing() -> Error {
    Error(
        StatusCode::NOT_FOUND,
        "tag_not_found",
        "Tag does not exist in this media domain",
    )
}
fn domain(s: &str) -> Result<MediaDomain> {
    MediaDomain::parse(s).map_err(invalid)
}
fn id(s: &str) -> Result<i64> {
    s.parse::<i64>()
        .ok()
        .filter(|id| (1..=9_007_199_254_740_991).contains(id))
        .ok_or_else(|| invalid("Invalid tag ID"))
}
fn body(v: std::result::Result<Json<Input>, JsonRejection>) -> Result<Input> {
    v.map(|v| v.0)
        .map_err(|_| invalid("Expected a tag label object"))
}
pub fn router(db: Arc<Database>) -> Router {
    let mut r = Router::new();
    for d in ["tv", "movies"] {
        let p = format!("/api/v1/{d}/tags");
        r = r.merge(
            Router::new()
                .route(&p, get(list).post(create))
                .route(&format!("{p}/detail"), get(details))
                .route(&format!("{p}/detail/{{id}}"), get(detail))
                .route(&format!("{p}/{{id}}/owners"), get(owners))
                .route(&format!("{p}/{{id}}"), get(read).put(rename).delete(delete))
                .layer(Extension(d.to_owned()))
                .layer(DefaultBodyLimit::max(4096))
                .with_state(db.clone()),
        );
    }
    r
}
pub(crate) async fn catalog(c: &Connection, d: MediaDomain) -> Result<Vec<Tag>> {
    let mut rows = c
        .query(
            "SELECT id,label FROM tags WHERE media_type=? ORDER BY label LIMIT 1025",
            [names(d).0],
        )
        .await?;
    let mut tags = vec![];
    while let Some(r) = rows.next().await? {
        if tags.len() == MAX_TAGS {
            return Err(invalid("Tag catalog exceeds 1024 entries"));
        };
        tags.push(Tag {
            id: r.get(0)?,
            label: r.get(1)?,
            media_type: d,
        });
    }
    Ok(tags)
}
pub(crate) async fn insert(c: &Connection, d: MediaDomain, normalized: &str) -> Result<i64> {
    if let Some(r) = c
        .query(
            "SELECT id FROM tags WHERE media_type=? AND label=?",
            params![names(d).0, normalized],
        )
        .await?
        .next()
        .await?
    {
        return Ok(r.get(0)?);
    };
    if catalog(c, d).await?.len() >= MAX_TAGS {
        return Err(invalid("Tag catalog limit reached"));
    };
    let id=c.query("SELECT COALESCE(MAX(id),0) FROM (SELECT id FROM tags UNION ALL SELECT destination_id AS id FROM snapshot_mappings WHERE destination_table='tags')",()).await?.next().await?.ok_or_else(missing)?.get::<i64>(0)?;
    let id = id
        .checked_add(1)
        .filter(|id| (1..=9_007_199_254_740_991).contains(id))
        .ok_or(Error(
            StatusCode::CONFLICT,
            "tag_id_exhausted",
            "Tag identity space is exhausted",
        ))?;
    c.execute(
        "INSERT INTO tags(id,media_type,label)VALUES(?,?,?)",
        params![id, names(d).0, normalized],
    )
    .await?;
    Ok(id)
}
pub(crate) async fn assigned(c: &Connection, d: MediaDomain, owner: i64) -> Result<Vec<i64>> {
    let (_, t, k) = names(d);
    let mut rows = c
        .query(
            &format!("SELECT tag_id FROM {t} WHERE {k}=? ORDER BY tag_id LIMIT 201"),
            [owner],
        )
        .await?;
    let mut ids = vec![];
    while let Some(r) = rows.next().await? {
        ids.push(r.get(0)?)
    }
    if ids.len() > MAX_ASSIGNMENTS {
        return Err(invalid("Library tag assignment exceeds 200 entries"));
    }
    Ok(ids)
}
pub(crate) async fn assign(
    c: &Connection,
    d: MediaDomain,
    owner: i64,
    a: &Assignment,
    local: bool,
) -> Result<()> {
    validate(a).map_err(invalid)?;
    let (media, t, k) = names(d);
    let mut next = assigned(c, d, owner)
        .await?
        .into_iter()
        .collect::<BTreeSet<_>>();
    let before = next.clone();
    for id in &a.ids {
        if c.query(
            "SELECT 1 FROM tags WHERE id=? AND media_type=?",
            params![*id, media],
        )
        .await?
        .next()
        .await?
        .is_none()
        {
            return Err(invalid("Tag does not exist in this media domain"));
        }
    }
    match a.mode {
        Mode::Replace => next = a.ids.iter().copied().collect(),
        Mode::Add => next.extend(&a.ids),
        Mode::Remove => {
            for id in &a.ids {
                next.remove(id);
            }
        }
    };
    if next.len() > MAX_ASSIGNMENTS {
        return Err(invalid("Library tag assignment exceeds 200 entries"));
    }
    c.execute(&format!("DELETE FROM {t} WHERE {k}=?"), [owner])
        .await?;
    let changed = next != before;
    for id in next {
        c.execute(
            &format!("INSERT INTO {t}({k},tag_id)VALUES(?,?)"),
            params![owner, id],
        )
        .await?;
    }
    if changed {
        crate::revision_policy::wake_pending(c, d)
            .await
            .map_err(|_| {
                Error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "tag_policy_wake_failed",
                    "Tag policy reconsideration failed",
                )
            })?;
    }
    if local {
        c.execute(
            &format!("INSERT INTO library_tag_edits({k})VALUES(?) ON CONFLICT({k}) DO NOTHING"),
            [owner],
        )
        .await?;
    }
    Ok(())
}
async fn list(
    State(db): State<Arc<Database>>,
    Extension(m): Extension<String>,
) -> Result<Json<Vec<Tag>>> {
    Ok(Json(catalog(&db.connect().await?, domain(&m)?).await?))
}
async fn find(c: &Connection, d: MediaDomain, id: i64) -> Result<Tag> {
    catalog(c, d)
        .await?
        .into_iter()
        .find(|t| t.id == id)
        .ok_or_else(missing)
}
async fn read(
    State(db): State<Arc<Database>>,
    Extension(m): Extension<String>,
    Path(raw): Path<String>,
) -> Result<Json<Tag>> {
    Ok(Json(
        find(&db.connect().await?, domain(&m)?, id(&raw)?).await?,
    ))
}
async fn create(
    State(db): State<Arc<Database>>,
    Extension(m): Extension<String>,
    v: std::result::Result<Json<Input>, JsonRejection>,
) -> Result<(StatusCode, Json<Tag>)> {
    let d = domain(&m)?;
    let label = label(&body(v)?.label).map_err(invalid)?;
    let c = db.connect().await?;
    let tx = c
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    let id = insert(&tx, d, &label).await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(Tag {
            id,
            media_type: d,
            label,
        }),
    ))
}
async fn rename(
    State(db): State<Arc<Database>>,
    Extension(m): Extension<String>,
    Path(raw): Path<String>,
    v: std::result::Result<Json<Input>, JsonRejection>,
) -> Result<Json<Tag>> {
    let d = domain(&m)?;
    let id = id(&raw)?;
    let label = label(&body(v)?.label).map_err(invalid)?;
    let c = db.connect().await?;
    let tx = c
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    find(&tx, d, id).await?;
    if tx
        .query(
            "SELECT 1 FROM tags WHERE media_type=? AND label=? AND id<>?",
            params![names(d).0, label.clone(), id],
        )
        .await?
        .next()
        .await?
        .is_some()
    {
        return Err(Error(
            StatusCode::CONFLICT,
            "tag_conflict",
            "Tag label already exists",
        ));
    }
    tx.execute(
        "UPDATE tags SET label=? WHERE id=?",
        params![label.clone(), id],
    )
    .await?;
    tx.commit().await?;
    Ok(Json(Tag {
        id,
        media_type: d,
        label,
    }))
}
async fn usage(c: &Connection, t: Tag) -> Result<Detail> {
    let (_, table, _) = names(t.media_type);
    let count = c
        .query(
            &format!("SELECT count(*) FROM {table} WHERE tag_id=?"),
            [t.id],
        )
        .await?
        .next()
        .await?
        .ok_or_else(missing)?
        .get(0)?;
    let delay_profile_ids = c
        .query(
            "SELECT profile_id FROM delay_profile_tags WHERE tag_id=?",
            [t.id],
        )
        .await?
        .next()
        .await?
        .map(|r| r.get::<i64>(0))
        .transpose()?
        .into_iter()
        .collect::<Vec<_>>();
    let mut release_profile_ids = vec![];
    let mut excluded_release_profile_ids = vec![];
    let mut rows=c.query("SELECT profile_id,kind FROM release_profile_tags WHERE tag_id=? ORDER BY profile_id LIMIT 1025",[t.id]).await?;
    while let Some(row) = rows.next().await? {
        let id = row.get(0)?;
        match row.get::<String>(1)?.as_str() {
            "include" => release_profile_ids.push(id),
            "exclude" => excluded_release_profile_ids.push(id),
            _ => {
                return Err(Error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "tag_reference_invariant",
                    "Tag reference graph is invalid",
                ));
            }
        }
    }
    if release_profile_ids.len() + excluded_release_profile_ids.len() > 1024 {
        return Err(Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "tag_reference_invariant",
            "Tag reference graph exceeds its bound",
        ));
    }
    Ok(Detail {
        in_use: count > 0
            || !delay_profile_ids.is_empty()
            || !release_profile_ids.is_empty()
            || !excluded_release_profile_ids.is_empty(),
        release_profile_ids,
        excluded_release_profile_ids,
        delay_profile_ids,
        tag: t,
        owner_count: count,
    })
}
async fn details(
    State(db): State<Arc<Database>>,
    Extension(m): Extension<String>,
) -> Result<Json<Vec<Detail>>> {
    let c = db.connect().await?;
    let mut v = vec![];
    for t in catalog(&c, domain(&m)?).await? {
        v.push(usage(&c, t).await?)
    }
    Ok(Json(v))
}
async fn detail(
    State(db): State<Arc<Database>>,
    Extension(m): Extension<String>,
    Path(raw): Path<String>,
) -> Result<Json<Detail>> {
    let c = db.connect().await?;
    Ok(Json(
        usage(&c, find(&c, domain(&m)?, id(&raw)?).await?).await?,
    ))
}
async fn owners(
    State(db): State<Arc<Database>>,
    Extension(m): Extension<String>,
    Path(raw): Path<String>,
    Query(p): Query<Page>,
) -> Result<Json<Owners>> {
    let d = domain(&m)?;
    let c = db.connect().await?;
    let t = find(&c, d, id(&raw)?).await?;
    let total = usage(&c, t.clone()).await?.owner_count;
    let limit = p.limit.unwrap_or(100);
    if !(1..=200).contains(&limit) {
        return Err(invalid("Owner page limit must be 1 to 200"));
    }
    let offset = p.offset.unwrap_or(0);
    let (_, table, key) = names(d);
    let mut rows = c
        .query(
            &format!("SELECT {key} FROM {table} WHERE tag_id=? ORDER BY {key} LIMIT ? OFFSET ?"),
            params![t.id, i64::from(limit), i64::from(offset)],
        )
        .await?;
    let mut ids = vec![];
    while let Some(r) = rows.next().await? {
        ids.push(r.get(0)?)
    }
    Ok(Json(Owners {
        ids,
        total,
        limit,
        offset,
    }))
}
async fn delete(
    State(db): State<Arc<Database>>,
    Extension(m): Extension<String>,
    Path(raw): Path<String>,
) -> Result<StatusCode> {
    let c = db.connect().await?;
    let tx = c
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    let t = find(&tx, domain(&m)?, id(&raw)?).await?;
    if usage(&tx, t.clone()).await?.in_use {
        return Err(Error(
            StatusCode::CONFLICT,
            "tag_in_use",
            "Unassign this tag before deleting it",
        ));
    }
    tx.execute("DELETE FROM tags WHERE id=?", [t.id]).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
