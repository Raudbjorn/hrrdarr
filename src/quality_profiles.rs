//! Persisted quality item/group editing and size settings; not cutoff/scoring/search policy.
use crate::{
    db::Database,
    qualities::{Error, Result, body, domain, invalid, validate_sizes, validate_title},
};
use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, Path, Query, State,
        rejection::{JsonRejection, QueryRejection},
    },
    http::StatusCode,
    routing::get,
};
use libsql::{Connection, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
const MAX_NODES: usize = 64;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename="QualityProfileLeafInput",optional_fields=nullable)]
pub struct Leaf {
    pub quality_id: i64,
    pub allowed: bool,
    pub min_size: Option<f64>,
    pub max_size: Option<f64>,
    pub preferred_size: Option<f64>,
}
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[ts(rename = "QualityProfileItemInput")]
pub enum Item {
    Quality(Leaf),
    Group {
        name: String,
        allowed: bool,
        items: Vec<Leaf>,
    },
}
#[derive(Debug, Deserialize, Serialize, PartialEq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename="QualityProfileInput",optional_fields=nullable)]
pub struct ProfileInput {
    pub name: String,
    pub items: Vec<Item>,
}
#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(rename = "QualityProfile")]
pub struct Profile {
    pub id: i64,
    pub media_type: crate::api::MediaDomain,
    pub name: String,
    pub items: Vec<ProfileItem>,
}
#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(rename = "QualityProfileSummary")]
pub struct Summary {
    id: i64,
    name: String,
    item_count: i64,
    group_count: i64,
}
#[derive(Deserialize, ts_rs::TS)]
#[ts(rename = "QualityProfileQuery")]
pub struct Page {
    #[serde(default)]
    #[ts(as = "Option<u32>", optional)]
    offset: u32,
    #[serde(default = "page_limit")]
    #[ts(as = "Option<u16>", optional)]
    limit: u16,
}
fn page_limit() -> u16 {
    50
}

pub fn router(db: Arc<Database>) -> Router {
    Router::new()
        .route("/api/v1/{media}/quality-profiles", get(list).post(create))
        .route(
            "/api/v1/{media}/quality-profiles/{id}",
            get(read).put(replace),
        )
        .layer(DefaultBodyLimit::max(32 * 1024))
        .with_state(db)
}
fn missing() -> Error {
    Error(
        StatusCode::NOT_FOUND,
        "profile_not_found",
        "Quality profile does not exist in this media domain",
    )
}
fn path_id(raw: &str) -> Result<i64> {
    raw.parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| invalid("Profile id must be a positive integer"))
}
fn validate(input: &ProfileInput, limit: f64) -> Result<()> {
    validate_title(&input.name)?;
    if input.items.is_empty() {
        return Err(invalid("Profile must contain at least one quality"));
    }
    let mut count = input.items.len();
    let mut qualities = BTreeSet::new();
    for node in &input.items {
        let leaves = match node {
            Item::Quality(leaf) => std::slice::from_ref(leaf),
            Item::Group { name, items, .. } => {
                validate_title(name)?;
                if items.is_empty() {
                    return Err(invalid("Group must contain at least one quality"));
                }
                count += items.len();
                items.as_slice()
            }
        };
        for leaf in leaves {
            if !qualities.insert(leaf.quality_id) {
                return Err(invalid("Each quality may appear only once per profile"));
            }
            validate_sizes(leaf.min_size, leaf.max_size, leaf.preferred_size, limit)?;
        }
    }
    if count > MAX_NODES {
        return Err(invalid("Profile exceeds 64 total quality/group nodes"));
    }
    Ok(())
}
async fn fetch(conn: &Connection, media: &str, id: i64) -> Result<Profile> {
    let row = conn
        .query(
            "SELECT name FROM quality_profiles WHERE id=?1 AND media_type=?2",
            params![id, media],
        )
        .await?
        .next()
        .await?
        .ok_or_else(missing)?;
    let mut groups = BTreeMap::new();
    let mut rows=conn.query("SELECT id,name,position,allowed FROM quality_profile_groups WHERE profile_id=?1 ORDER BY position",params![id]).await?;
    while let Some(row) = rows.next().await? {
        groups.insert(
            row.get::<i64>(0)?,
            (
                row.get::<i64>(2)?,
                row.get::<String>(1)?,
                row.get::<i64>(3)? != 0,
                Vec::new(),
            ),
        );
    }
    let mut roots = BTreeMap::new();
    let mut rows=conn.query("SELECT quality_id,group_id,position,allowed,min_size,max_size,preferred_size FROM quality_profile_items WHERE profile_id=?1 ORDER BY position",params![id]).await?;
    while let Some(row) = rows.next().await? {
        let leaf = ProfileLeaf {
            quality_id: row.get(0)?,
            allowed: row.get::<i64>(3)? != 0,
            min_size: row.get(4)?,
            max_size: row.get(5)?,
            preferred_size: row.get(6)?,
        };
        if let Some(group) = row.get::<Option<i64>>(1)? {
            groups
                .get_mut(&group)
                .ok_or(Error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "profile_integrity",
                    "Profile group is absent",
                ))?
                .3
                .push(leaf);
        } else {
            roots.insert(row.get::<i64>(2)?, ProfileItem::Quality(leaf));
        }
    }
    for (_, (position, name, allowed, items)) in groups {
        if roots
            .insert(
                position,
                ProfileItem::Group {
                    name,
                    allowed,
                    items,
                },
            )
            .is_some()
        {
            return Err(Error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "profile_integrity",
                "Profile has overlapping top-level positions",
            ));
        }
    }
    Ok(Profile {
        id,
        media_type: crate::api::MediaDomain::parse(media).map_err(invalid)?,
        name: row.get(0)?,
        items: roots.into_values().collect(),
    })
}
async fn list(
    State(db): State<Arc<Database>>,
    Path(media): Path<String>,
    page: std::result::Result<Query<Page>, QueryRejection>,
) -> Result<Json<ProfilePage>> {
    domain(&media)?;
    let Query(page) = page
        .map_err(|_| invalid("Pagination requires nonnegative integer offset and integer limit"))?;
    if page.limit == 0 || page.limit > 100 {
        return Err(invalid("Page limit must be between 1 and 100"));
    }
    let database_connection = db.connect().await?;
    let conn = database_connection.transaction().await?;
    let total = conn
        .query(
            "SELECT count(*) FROM quality_profiles WHERE media_type=?1",
            params![media.clone()],
        )
        .await?
        .next()
        .await?
        .ok_or_else(missing)?
        .get::<i64>(0)?;
    let mut rows=conn.query("SELECT p.id,p.name,(SELECT count(*) FROM quality_profile_items i WHERE i.profile_id=p.id),(SELECT count(*) FROM quality_profile_groups g WHERE g.profile_id=p.id) FROM quality_profiles p WHERE p.media_type=?1 ORDER BY p.id LIMIT ?2 OFFSET ?3",params![media.clone(),i64::from(page.limit),i64::from(page.offset)]).await?;
    let mut items = Vec::new();
    while let Some(row) = rows.next().await? {
        items.push(Summary {
            id: row.get(0)?,
            name: row.get(1)?,
            item_count: row.get(2)?,
            group_count: row.get(3)?,
        });
    }
    conn.rollback().await?;
    Ok(Json(ProfilePage {
        media_type: crate::api::MediaDomain::parse(&media).map_err(invalid)?,
        items,
        total,
        offset: page.offset,
        limit: page.limit,
    }))
}
async fn read(
    State(db): State<Arc<Database>>,
    Path((media, id)): Path<(String, String)>,
) -> Result<Json<Profile>> {
    domain(&media)?;
    let id = path_id(&id)?;
    let conn = db.connect().await?;
    let tx = conn.transaction().await?;
    let result = fetch(&tx, &media, id).await;
    tx.rollback().await?;
    result.map(Json)
}
async fn persist(
    db: &Database,
    media: &str,
    id: Option<i64>,
    input: ProfileInput,
) -> Result<Profile> {
    validate(&input, domain(media)?)?;
    let conn = db.connect().await?;
    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    let result:Result<Profile>=async {
        if let Some(id)=id {fetch(&tx,media,id).await?;}
        if tx.query("SELECT id FROM quality_profiles WHERE media_type=?1 AND name=?2 AND id!=?3",params![media,input.name.clone(),id.unwrap_or(0)]).await?.next().await?.is_some(){return Err(Error(StatusCode::CONFLICT,"profile_name_conflict","Profile name is already used in this media domain"));}
        // Validate catalog membership before replacing any old state; FKs enforce the same domain.
        let mut available=BTreeSet::new();let mut rows=tx.query("SELECT quality_id FROM quality_definitions WHERE media_type=?1",params![media]).await?;
        while let Some(row)=rows.next().await? {available.insert(row.get::<i64>(0)?);}
        for item in &input.items {
            let leaves=match item{Item::Quality(leaf)=>std::slice::from_ref(leaf),Item::Group{items,..}=>items.as_slice()};
            if leaves.iter().any(|l|!available.contains(&l.quality_id)){return Err(invalid("Profile contains an unknown quality for this media domain"));}
        }
        let id=match id {
            Some(id)=>{
                tx.execute("UPDATE quality_profiles SET name=?1 WHERE id=?2",params![input.name,id]).await?;
                tx.execute("DELETE FROM quality_profile_items WHERE profile_id=?1",params![id]).await?;
                tx.execute("DELETE FROM quality_profile_groups WHERE profile_id=?1",params![id]).await?;id
            }
            None=>{tx.execute("INSERT INTO quality_profiles(media_type,name) VALUES(?1,?2)",params![media,input.name]).await?;tx.last_insert_rowid()},
        };
        for (position,item) in input.items.into_iter().enumerate() {
            match item {
                Item::Quality(leaf)=>insert_leaf(&tx,media,id,None,position,leaf).await?,
                Item::Group{name,allowed,items}=>{
                    tx.execute("INSERT INTO quality_profile_groups(profile_id,name,position,allowed) VALUES(?1,?2,?3,?4)",params![id,name,position as i64,i64::from(allowed)]).await?;
                    let group=tx.last_insert_rowid();
                    for (pos,leaf) in items.into_iter().enumerate(){insert_leaf(&tx,media,id,Some(group),pos,leaf).await?;}
                }
            }
        }
        fetch(&tx,media,id).await
    }.await;
    match result {
        Ok(profile) => {
            tx.commit().await?;
            Ok(profile)
        }
        Err(error) => {
            tx.rollback().await?;
            Err(error)
        }
    }
}
async fn insert_leaf(
    conn: &Connection,
    media: &str,
    profile: i64,
    group: Option<i64>,
    position: usize,
    leaf: Leaf,
) -> Result<()> {
    conn.execute("INSERT INTO quality_profile_items(profile_id,media_type,quality_id,group_id,position,allowed,min_size,max_size,preferred_size) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![profile,media,leaf.quality_id,group,position as i64,i64::from(leaf.allowed),leaf.min_size,leaf.max_size,leaf.preferred_size]).await?;
    Ok(())
}
async fn create(
    State(db): State<Arc<Database>>,
    Path(media): Path<String>,
    payload: std::result::Result<Json<ProfileInput>, JsonRejection>,
) -> Result<(StatusCode, Json<Profile>)> {
    domain(&media)?;
    Ok((
        StatusCode::CREATED,
        Json(persist(&db, &media, None, body(payload)?).await?),
    ))
}
async fn replace(
    State(db): State<Arc<Database>>,
    Path((media, id)): Path<(String, String)>,
    payload: std::result::Result<Json<ProfileInput>, JsonRejection>,
) -> Result<Json<Profile>> {
    domain(&media)?;
    Ok(Json(
        persist(&db, &media, Some(path_id(&id)?), body(payload)?).await?,
    ))
}

#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(rename = "QualityProfileLeaf")]
pub struct ProfileLeaf {
    pub quality_id: i64,
    pub allowed: bool,
    pub min_size: Option<f64>,
    pub max_size: Option<f64>,
    pub preferred_size: Option<f64>,
}
#[derive(Debug, Serialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(rename = "QualityProfileItem")]
pub enum ProfileItem {
    Quality(ProfileLeaf),
    Group {
        name: String,
        allowed: bool,
        items: Vec<ProfileLeaf>,
    },
}
#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(rename = "QualityProfilePage")]
pub struct ProfilePage {
    pub media_type: crate::api::MediaDomain,
    pub items: Vec<Summary>,
    pub total: i64,
    pub offset: u32,
    pub limit: u16,
}
