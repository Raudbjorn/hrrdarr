//! Profile structure and explicit policy storage; evaluation/search remains separate.
use crate::{
    db::Database,
    qualities::{Error, Result, body, domain, invalid, validate_sizes, validate_title},
};
use axum::{
    Extension, Json, Router,
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
pub(crate) const MAX_NODES: usize = 64;

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
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[ts(rename = "QualityProfileCutoff")]
pub enum Cutoff {
    Quality { quality_id: i64 },
    Group { position: usize },
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "QualityProfilePolicy")]
pub struct Policy {
    pub upgrade_allowed: bool,
    pub cutoff: Cutoff,
    pub min_format_score: i32,
    pub cutoff_format_score: i32,
    pub min_upgrade_format_score: i32,
    #[ts(optional = nullable)]
    pub language_id: Option<i32>,
    pub format_items: Vec<crate::db::custom_formats::FormatScore>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename="QualityProfileInput",optional_fields=nullable)]
pub struct ProfileInput {
    pub name: String,
    pub items: Vec<Item>,
    pub policy: Option<Policy>,
}
#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(rename = "QualityProfile")]
pub struct Profile {
    pub id: i64,
    pub media_type: crate::api::MediaDomain,
    pub name: String,
    pub items: Vec<ProfileItem>,
    pub policy: Option<Policy>,
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
    let mut router = Router::new();
    for media in ["tv", "movies"] {
        let prefix = format!("/api/v1/{media}/quality-profiles");
        router = router.merge(
            Router::new()
                .route(&prefix, get(list).post(create))
                .route(&format!("{prefix}/schema"), get(schema))
                .route(
                    &format!("{prefix}/{{id}}"),
                    get(read).put(replace).delete(delete),
                )
                .layer(Extension(media.to_owned()))
                .layer(DefaultBodyLimit::max(32 * 1024))
                .with_state(db.clone()),
        );
    }
    router
        .route(
            "/api/v1/{media}/quality-profiles",
            get(unknown_domain).post(unknown_domain),
        )
        .route(
            "/api/v1/{media}/quality-profiles/{id}",
            get(unknown_domain)
                .put(unknown_domain)
                .delete(unknown_domain),
        )
}
async fn unknown_domain() -> Error {
    Error(
        StatusCode::NOT_FOUND,
        "media_type_not_found",
        "Media domain must be tv or movies",
    )
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
        .filter(|id| (1..=9_007_199_254_740_991).contains(id))
        .ok_or_else(|| invalid("Profile id must be a positive safe integer"))
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
pub(crate) async fn fetch(conn: &Connection, media: &str, id: i64) -> Result<Profile> {
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
        policy: fetch_policy(conn, media, id).await?,
    })
}
async fn list(
    State(db): State<Arc<Database>>,
    Extension(media): Extension<String>,
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
    Extension(media): Extension<String>,
    Path(id): Path<String>,
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
    let conn = db.connect().await?;
    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    let result = persist_on_connection(&tx, media, id, input).await;
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
    Extension(media): Extension<String>,
    payload: std::result::Result<Json<ProfileInput>, JsonRejection>,
) -> Result<(StatusCode, Json<Profile>)> {
    domain(&media)?;
    Ok((
        StatusCode::CREATED,
        Json(persist(&db, &media, None, profile_body(payload)?).await?),
    ))
}
async fn replace(
    State(db): State<Arc<Database>>,
    Extension(media): Extension<String>,
    Path(id): Path<String>,
    payload: std::result::Result<Json<ProfileInput>, JsonRejection>,
) -> Result<Json<Profile>> {
    domain(&media)?;
    Ok(Json(
        persist(&db, &media, Some(path_id(&id)?), profile_body(payload)?).await?,
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

fn validate_policy(input: &ProfileInput, media: &str) -> Result<()> {
    let Some(policy) = &input.policy else {
        return Ok(());
    };
    let mut ids = BTreeSet::new();
    if policy.format_items.len() > crate::custom_formats::MAX_FORMATS
        || policy.format_items.iter().any(|f| {
            f.format_id <= 0 || f.format_id > 9_007_199_254_740_991 || !ids.insert(f.format_id)
        })
    {
        return Err(invalid("Invalid or duplicate custom format score ids"));
    }
    let reachable: i64 = policy
        .format_items
        .iter()
        .map(|f| i64::from(f.score.max(0)))
        .sum();
    if i64::from(policy.min_format_score) > reachable {
        return Err(invalid("Minimum score exceeds configured positive scores"));
    }
    if policy.min_upgrade_format_score < 1 {
        return Err(invalid("Minimum upgrade format score must be at least one"));
    }
    if (media == "tv" && policy.language_id.is_some())
        || (media == "movies" && !policy.language_id.is_some_and(|id| (-2..=57).contains(&id)))
    {
        return Err(invalid("Invalid profile language for this media domain"));
    }
    let valid=match policy.cutoff {
        Cutoff::Quality{quality_id}=>input.items.iter().any(|item| matches!(item,Item::Quality(leaf) if leaf.quality_id==quality_id && leaf.allowed)),
        Cutoff::Group{position}=>matches!(input.items.get(position),Some(Item::Group{allowed:true,..})),
    };
    if !valid {
        return Err(invalid(
            "Cutoff must select an allowed top-level quality or group",
        ));
    }
    Ok(())
}
async fn fetch_policy(conn: &Connection, media: &str, id: i64) -> Result<Option<Policy>> {
    let row=conn.query("SELECT p.upgrade_allowed,p.cutoff_quality_id,g.position,p.min_format_score,p.cutoff_format_score,p.min_upgrade_format_score,p.language_id FROM quality_profile_policies p LEFT JOIN quality_profile_groups g ON g.id=p.cutoff_group_id AND g.profile_id=p.profile_id WHERE p.profile_id=? AND p.media_type=?",params![id,media]).await?.next().await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let cutoff = match row.get::<Option<i64>>(1)? {
        Some(quality_id) => Cutoff::Quality { quality_id },
        None => Cutoff::Group {
            position: row.get::<i64>(2)? as usize,
        },
    };
    let format_items = format_scores(conn, media, id).await?;
    Ok(Some(Policy {
        upgrade_allowed: row.get::<i64>(0)? != 0,
        cutoff,
        min_format_score: row.get(3)?,
        cutoff_format_score: row.get(4)?,
        min_upgrade_format_score: row.get(5)?,
        language_id: row.get(6)?,
        format_items,
    }))
}
async fn format_scores(
    conn: &Connection,
    media: &str,
    id: i64,
) -> Result<Vec<crate::db::custom_formats::FormatScore>> {
    let mut format_items = Vec::new();
    let mut scores=conn.query("SELECT f.id,COALESCE(s.score,0) FROM custom_formats f LEFT JOIN quality_profile_format_scores s ON s.format_id=f.id AND s.profile_id=? WHERE f.media_type=? ORDER BY f.id LIMIT 129",params![id,media]).await?;
    while let Some(score) = scores.next().await? {
        format_items.push(crate::db::custom_formats::FormatScore {
            format_id: score.get(0)?,
            score: score.get(1)?,
        });
    }
    if format_items.len() > crate::custom_formats::MAX_FORMATS {
        return Err(invalid("Custom format score limit"));
    }
    Ok(format_items)
}

async fn insert_policy(conn: &Connection, media: &str, id: i64, policy: Policy) -> Result<()> {
    let (quality, group) = match policy.cutoff {
        Cutoff::Quality { quality_id } => (Some(quality_id), None),
        Cutoff::Group { position } => {
            let group = conn
                .query(
                    "SELECT id FROM quality_profile_groups WHERE profile_id=? AND position=?",
                    params![id, position as i64],
                )
                .await?
                .next()
                .await?
                .ok_or_else(|| invalid("Cutoff group is absent"))?
                .get::<i64>(0)?;
            (None, Some(group))
        }
    };
    conn.execute("INSERT INTO quality_profile_policies(profile_id,media_type,upgrade_allowed,cutoff_quality_id,cutoff_group_id,min_format_score,cutoff_format_score,min_upgrade_format_score,language_id) VALUES(?,?,?,?,?,?,?,?,?)",params![id,media,i64::from(policy.upgrade_allowed),quality,group,policy.min_format_score,policy.cutoff_format_score,policy.min_upgrade_format_score,policy.language_id]).await?;
    for score in policy.format_items {
        conn.execute("INSERT INTO quality_profile_format_scores(profile_id,format_id,media_type,score)VALUES(?,?,?,?)",params![id,score.format_id,media,score.score]).await?;
    }
    Ok(())
}

fn profile_body(
    payload: std::result::Result<Json<ProfileInput>, JsonRejection>,
) -> Result<ProfileInput> {
    body(payload).map_err(|error| {
        if error.0 == StatusCode::BAD_REQUEST {
            invalid("Expected the documented profile fields and types")
        } else {
            error
        }
    })
}

pub(crate) async fn persist_on_connection(
    conn: &Connection,
    media: &str,
    id: Option<i64>,
    input: ProfileInput,
) -> Result<Profile> {
    validate_input(&input, media)?;

    if let Some(id) = id {
        let old = fetch(conn, media, id).await?;
        if old.policy.is_some() && input.policy.is_none() {
            return Err(invalid(
                "Configured profiles require a complete policy on replacement",
            ));
        }
    }
    if conn
        .query(
            "SELECT id FROM quality_profiles WHERE media_type=?1 AND name=?2 AND id!=?3",
            params![media, input.name.clone(), id.unwrap_or(0)],
        )
        .await?
        .next()
        .await?
        .is_some()
    {
        return Err(Error(
            StatusCode::CONFLICT,
            "profile_name_conflict",
            "Profile name is already used in this media domain",
        ));
    }
    // Validate catalog membership before replacing any old state; FKs enforce the same domain.
    let mut available = BTreeSet::new();
    let mut rows = conn
        .query(
            "SELECT quality_id FROM quality_definitions WHERE media_type=?1",
            params![media],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        available.insert(row.get::<i64>(0)?);
    }
    for item in &input.items {
        let leaves = match item {
            Item::Quality(leaf) => std::slice::from_ref(leaf),
            Item::Group { items, .. } => items.as_slice(),
        };
        if leaves.iter().any(|l| !available.contains(&l.quality_id)) {
            return Err(invalid(
                "Profile contains an unknown quality for this media domain",
            ));
        }
    }
    if let Some(policy) = &input.policy {
        for score in &policy.format_items {
            if conn
                .query(
                    "SELECT 1 FROM custom_formats WHERE id=? AND media_type=?",
                    params![score.format_id, media],
                )
                .await?
                .next()
                .await?
                .is_none()
            {
                return Err(invalid(
                    "Custom format does not belong to this media domain",
                ));
            }
        }
    }
    let id = match id {
        Some(id) => {
            conn.execute(
                "UPDATE quality_profiles SET name=?1 WHERE id=?2",
                params![input.name, id],
            )
            .await?;
            conn.execute(
                "DELETE FROM quality_profile_format_scores WHERE profile_id=?",
                [id],
            )
            .await?;
            conn.execute(
                "DELETE FROM quality_profile_policies WHERE profile_id=?1",
                params![id],
            )
            .await?;
            conn.execute(
                "DELETE FROM quality_profile_items WHERE profile_id=?1",
                params![id],
            )
            .await?;
            conn.execute(
                "DELETE FROM quality_profile_groups WHERE profile_id=?1",
                params![id],
            )
            .await?;
            id
        }
        None => {
            // ponytail: historical mappings are scanned per creation; index destination_table/id if archive volume makes this costly.
            let previous=conn.query("SELECT max(coalesce((SELECT max(id) FROM quality_profiles),0),coalesce((SELECT max(destination_id) FROM snapshot_mappings WHERE destination_table='quality_profiles'),0))",()).await?.next().await?.ok_or_else(||invalid("Profile identity allocation failed"))?.get::<i64>(0)?;
            let id = previous
                .checked_add(1)
                .filter(|id| *id <= 9_007_199_254_740_991)
                .ok_or(Error(
                    StatusCode::CONFLICT,
                    "profile_id_exhausted",
                    "No safe profile identity remains",
                ))?;
            conn.execute(
                "INSERT INTO quality_profiles(id,media_type,name) VALUES(?1,?2,?3)",
                params![id, media, input.name],
            )
            .await?;
            id
        }
    };
    for (position, item) in input.items.into_iter().enumerate() {
        match item {
            Item::Quality(leaf) => insert_leaf(conn, media, id, None, position, leaf).await?,
            Item::Group {
                name,
                allowed,
                items,
            } => {
                conn.execute("INSERT INTO quality_profile_groups(profile_id,name,position,allowed) VALUES(?1,?2,?3,?4)",params![id,name,position as i64,i64::from(allowed)]).await?;
                let group = conn.last_insert_rowid();
                for (pos, leaf) in items.into_iter().enumerate() {
                    insert_leaf(conn, media, id, Some(group), pos, leaf).await?;
                }
            }
        }
    }
    if let Some(policy) = input.policy {
        insert_policy(conn, media, id, policy).await?;
    }
    fetch(conn, media, id).await
}

pub(crate) fn validate_input(input: &ProfileInput, media: &str) -> Result<()> {
    validate(input, domain(media)?)?;
    validate_policy(input, media)
}
pub(crate) fn as_input(profile: Profile) -> ProfileInput {
    ProfileInput {
        name: profile.name,
        policy: profile.policy,
        items: profile
            .items
            .into_iter()
            .map(|item| match item {
                ProfileItem::Quality(leaf) => Item::Quality(input_leaf(leaf)),
                ProfileItem::Group {
                    name,
                    allowed,
                    items,
                } => Item::Group {
                    name,
                    allowed,
                    items: items.into_iter().map(input_leaf).collect(),
                },
            })
            .collect(),
    }
}
fn input_leaf(leaf: ProfileLeaf) -> Leaf {
    Leaf {
        quality_id: leaf.quality_id,
        allowed: leaf.allowed,
        min_size: leaf.min_size,
        max_size: leaf.max_size,
        preferred_size: leaf.preferred_size,
    }
}

/// Unsaved editor schema. Its empty name and disabled cutoff deliberately fail save validation.
async fn draft(conn: &Connection, media: &str) -> Result<ProfileInput> {
    let tv = media == "tv";
    // Immutable weights give the root order. TV WEB groups use Rip/DL; movies use DL/Rip.
    let mut rows=conn.query("SELECT quality_id,weight,group_name,default_min,default_max,default_preferred FROM quality_definitions WHERE media_type=? ORDER BY weight,CASE WHEN media_type='tv' AND source='webRip' THEN 0 WHEN media_type='movies' AND source='webdl' THEN 0 ELSE 1 END,quality_id LIMIT 65",[media]).await?;
    let mut groups: BTreeMap<i64, (Option<String>, Vec<Leaf>)> = BTreeMap::new();
    let mut count = 0;
    while let Some(row) = rows.next().await? {
        count += 1;
        if count > MAX_NODES {
            return Err(invalid("Quality catalog exceeds profile node limit"));
        }
        groups
            .entry(row.get(1)?)
            .or_insert((row.get(2)?, Vec::new()))
            .1
            .push(Leaf {
                quality_id: row.get(0)?,
                allowed: false,
                min_size: if tv { row.get(3)? } else { None },
                max_size: if tv { row.get(4)? } else { None },
                preferred_size: if tv { row.get(5)? } else { None },
            });
    }
    drop(rows);
    let mut items = Vec::new();
    for (_, (name, mut leaves)) in groups {
        if leaves.len() == 1 {
            items.push(Item::Quality(leaves.remove(0)));
        } else {
            count += 1;
            items.push(Item::Group {
                name: name.ok_or_else(|| invalid("Quality catalog group is unnamed"))?,
                allowed: false,
                items: leaves,
            });
        }
    }
    if items.is_empty() || count > MAX_NODES {
        return Err(invalid("Invalid profile quality catalog"));
    }
    Ok(ProfileInput {
        name: String::new(),
        items,
        policy: Some(Policy {
            upgrade_allowed: false,
            cutoff: Cutoff::Quality { quality_id: 0 },
            min_format_score: 0,
            cutoff_format_score: 0,
            min_upgrade_format_score: 1,
            language_id: (!tv).then_some(-2),
            format_items: format_scores(conn, media, 0).await?,
        }),
    })
}
async fn schema(
    State(db): State<Arc<Database>>,
    Extension(media): Extension<String>,
) -> Result<Json<ProfileInput>> {
    let conn = db.connect().await?;
    let tx = conn.transaction().await?;
    let result = draft(&tx, &media).await;
    tx.rollback().await?;
    result.map(Json)
}

/// Application startup only: preserve any configured domain, seed an entirely empty one.
pub async fn initialize_defaults(db: &Database) -> std::result::Result<(), crate::db::Error> {
    let conn = db.connect().await?;
    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    let result = async {
        for media in ["tv", "movies"] {
            if tx
                .query(
                    "SELECT 1 FROM quality_profiles WHERE media_type=? LIMIT 1",
                    [media],
                )
                .await?
                .next()
                .await?
                .is_some()
            {
                continue;
            }
            let template = draft(&tx, media).await?;
            let presets: &[(&str, i64, &[i64])] = if media == "tv" {
                &[
                    ("Any", 1, &[1, 12, 8, 2, 13, 22, 4, 9, 14, 5, 15, 3, 6, 7]),
                    ("SD", 1, &[1, 12, 8, 2, 13, 22]),
                    ("HD-720p", 4, &[4, 14, 5, 6]),
                    ("HD-1080p", 9, &[9, 15, 3, 7]),
                    ("Ultra-HD", 16, &[16, 17, 18, 19]),
                    ("HD - 720p/1080p", 4, &[4, 9, 14, 5, 15, 3, 6, 7]),
                ]
            } else {
                &[
                    (
                        "Any",
                        20,
                        &[
                            24, 25, 26, 27, 29, 28, 1, 2, 23, 8, 12, 20, 21, 4, 5, 14, 6, 9, 3, 15,
                            7, 30, 16, 18, 17, 19, 31, 22,
                        ],
                    ),
                    ("SD", 20, &[24, 25, 26, 27, 29, 28, 1, 2, 8, 12, 20, 21]),
                    ("HD-720p", 6, &[4, 5, 14, 6]),
                    ("HD-1080p", 7, &[9, 3, 15, 7, 30]),
                    ("Ultra-HD", 31, &[16, 18, 17, 19, 31]),
                    ("HD - 720p/1080p", 6, &[4, 5, 14, 6, 9, 3, 15, 7, 30]),
                ]
            };
            for (name, cutoff, allowed) in presets {
                let mut input = template.clone();
                input.name = (*name).into();
                for item in &mut input.items {
                    match item {
                        Item::Quality(leaf) => leaf.allowed = allowed.contains(&leaf.quality_id),
                        Item::Group {
                            allowed: group,
                            items,
                            ..
                        } => {
                            *group = items.iter().any(|leaf| allowed.contains(&leaf.quality_id));
                            for leaf in items {
                                leaf.allowed = *group;
                            }
                        }
                    }
                }
                if let Some(policy) = &mut input.policy {
                    policy.cutoff = Cutoff::Quality {
                        quality_id: *cutoff,
                    };
                }
                persist_on_connection(&tx, media, None, input).await?;
            }
        }
        Ok::<_, Error>(())
    }
    .await;
    match result {
        Ok(()) => {
            tx.commit().await?;
            Ok(())
        }
        Err(error) => {
            tx.rollback().await?;
            Err(error.1.into())
        }
    }
}

async fn delete(
    State(db): State<Arc<Database>>,
    Extension(media): Extension<String>,
    Path(raw): Path<String>,
) -> Result<StatusCode> {
    let id = path_id(&raw)?;
    let conn = db.connect().await?;
    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    let result = async {
        if tx
            .query(
                "SELECT 1 FROM quality_profiles WHERE id=? AND media_type=?",
                params![id, media.clone()],
            )
            .await?
            .next()
            .await?
            .is_none()
        {
            return Err(missing());
        }
        if tx
            .query(
                "SELECT 1 FROM library_settings WHERE quality_profile_id=? LIMIT 1",
                [id],
            )
            .await?
            .next()
            .await?
            .is_some()
        {
            return Err(Error(
                StatusCode::CONFLICT,
                "profile_in_use",
                "Quality profile is assigned to a library item",
            ));
        }
        tx.execute(
            "DELETE FROM quality_profiles WHERE id=? AND media_type=?",
            params![id, media],
        )
        .await?;
        Ok(StatusCode::NO_CONTENT)
    }
    .await;
    match result {
        Ok(status) => {
            tx.commit().await?;
            Ok(status)
        }
        Err(error) => {
            tx.rollback().await?;
            Err(error)
        }
    }
}
