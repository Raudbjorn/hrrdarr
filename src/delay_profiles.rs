//! Ordered scoped delay configuration. Global minutes remain in release_delay_policies.
use crate::api::MediaDomain;
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename = "DelayProtocol")]
pub enum Protocol {
    Usenet,
    Torrent,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "DelayProfileSettings")]
pub struct FullSettings {
    pub torrent_delay_minutes: u32,
    pub usenet_delay_minutes: u32,
    pub enable_torrent: bool,
    pub enable_usenet: bool,
    pub preferred_protocol: Protocol,
    pub bypass_if_highest_quality: bool,
    pub bypass_if_above_custom_format_score: bool,
    pub minimum_custom_format_score: i32,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "semantics", rename_all = "snake_case")]
#[ts(rename = "DelaySettings")]
pub enum Settings {
    LegacyAgeOnly {
        torrent_delay_minutes: Option<u32>,
        usenet_delay_minutes: Option<u32>,
    },
    Profile(FullSettings),
}
#[derive(Clone, Debug, Serialize, ts_rs::TS)]
#[ts(rename = "DelayProfile")]
pub struct Profile {
    pub id: i64,
    pub media_type: MediaDomain,
    pub is_global: bool,
    pub position: i64,
    pub tag_ids: Vec<i64>,
    pub settings: Settings,
}
#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(rename = "DelayProfileCatalog")]
pub struct Catalog {
    pub revision: i64,
    pub configured: bool,
    pub availability_delay_days: Option<i32>,
    pub profiles: Vec<Profile>,
}
#[derive(Clone, Debug, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "DelayProfileInput")]
pub struct Input {
    pub settings: FullSettings,
    pub tag_ids: Vec<i64>,
}
#[derive(Debug, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "DelayProfileWrite")]
pub struct Write {
    pub revision: i64,
    pub profile: Input,
}
#[derive(Debug, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "DelayProfileReorder")]
pub struct Reorder {
    pub revision: i64,
    pub ids: Vec<i64>,
}
/// Consumer must reread this selection in its final immediate writer transaction.
#[derive(Clone, Debug)]
pub enum EffectiveDelay {
    Unconfigured {
        revision: i64,
    },
    LegacyAgeOnly {
        revision: i64,
        torrent_delay_minutes: u32,
        usenet_delay_minutes: u32,
    },
    Profile {
        revision: i64,
        id: i64,
        settings: FullSettings,
    },
}

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use libsql::{Connection, params};
#[derive(Debug)]
pub struct Error(StatusCode, &'static str);
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (
            self.0,
            Json(crate::api::ApiErrorEnvelope::new(
                self.1,
                "Delay profile operation failed",
            )),
        )
            .into_response()
    }
}
impl From<libsql::Error> for Error {
    fn from(_: libsql::Error) -> Self {
        Self(
            StatusCode::SERVICE_UNAVAILABLE,
            "delay_profile_storage_error",
        )
    }
}
pub type Result<T> = std::result::Result<T, Error>;
fn invalid() -> Error {
    Error(StatusCode::BAD_REQUEST, "invalid_delay_profile")
}
fn corrupt() -> Error {
    Error(StatusCode::SERVICE_UNAVAILABLE, "delay_profile_invariant")
}
fn missing() -> Error {
    Error(StatusCode::NOT_FOUND, "delay_profile_not_found")
}
fn conflict() -> Error {
    Error(StatusCode::CONFLICT, "delay_profile_conflict")
}
fn domain(m: MediaDomain) -> &'static str {
    crate::revision_policy::domain(m)
}
const MAX_PROFILES: usize = 1024;
const MAX_ID: i64 = 9_007_199_254_740_991;
impl Protocol {
    fn text(self) -> &'static str {
        match self {
            Self::Usenet => "usenet",
            Self::Torrent => "torrent",
        }
    }
}
impl Default for Input {
    fn default() -> Self {
        Self {
            settings: FullSettings {
                torrent_delay_minutes: 0,
                usenet_delay_minutes: 0,
                enable_torrent: true,
                enable_usenet: true,
                preferred_protocol: Protocol::Usenet,
                bypass_if_highest_quality: false,
                bypass_if_above_custom_format_score: false,
                minimum_custom_format_score: 0,
            },
            tag_ids: vec![],
        }
    }
}
async fn catalog_inner(c: &Connection, m: MediaDomain) -> Result<Catalog> {
    let r=c.query("SELECT d.revision,p.availability_delay_days,p.torrent_delay_minutes,p.usenet_delay_minutes FROM delay_profile_domains d LEFT JOIN release_delay_policies p ON p.media_type=d.media_type WHERE d.media_type=?",[domain(m)]).await?.next().await?.ok_or_else(corrupt)?;
    let revision = r.get(0)?;
    let availability = r.get::<Option<i32>>(1)?;
    let torrent = r.get::<Option<u32>>(2)?;
    let usenet = r.get::<Option<u32>>(3)?;
    let mut rows=c.query("SELECT id,is_global,position,semantics,torrent_delay_minutes,usenet_delay_minutes,enable_torrent,enable_usenet,preferred_protocol,bypass_if_highest_quality,bypass_if_above_custom_format_score,minimum_custom_format_score FROM delay_profiles WHERE media_type=? ORDER BY is_global,position LIMIT 1025",[domain(m)]).await?;
    let mut profiles = vec![];
    let mut globals = 0;
    while let Some(r) = rows.next().await? {
        if profiles.len() == MAX_PROFILES {
            return Err(corrupt());
        }
        let id = r.get::<i64>(0)?;
        let global = r.get::<i64>(1)? == 1;
        let position = r.get(2)?;
        let (t, u) = if global {
            globals += 1;
            (torrent, usenet)
        } else {
            (r.get::<Option<u32>>(4)?, r.get::<Option<u32>>(5)?)
        };
        let settings = match r.get::<String>(3)?.as_str() {
            "legacy_age_only" => Settings::LegacyAgeOnly {
                torrent_delay_minutes: t,
                usenet_delay_minutes: u,
            },
            "profile" => Settings::Profile(FullSettings {
                torrent_delay_minutes: t.ok_or_else(corrupt)?,
                usenet_delay_minutes: u.ok_or_else(corrupt)?,
                enable_torrent: r.get::<i64>(6)? == 1,
                enable_usenet: r.get::<i64>(7)? == 1,
                preferred_protocol: match r.get::<String>(8)?.as_str() {
                    "usenet" => Protocol::Usenet,
                    "torrent" => Protocol::Torrent,
                    _ => return Err(corrupt()),
                },
                bypass_if_highest_quality: r.get::<i64>(9)? == 1,
                bypass_if_above_custom_format_score: r.get::<i64>(10)? == 1,
                minimum_custom_format_score: r.get(11)?,
            }),
            _ => return Err(corrupt()),
        };
        let mut tags=c.query("SELECT tag_id FROM delay_profile_tags WHERE profile_id=? ORDER BY tag_id LIMIT 201",[id]).await?;
        let mut tag_ids = vec![];
        while let Some(t) = tags.next().await? {
            tag_ids.push(t.get::<i64>(0)?);
        }
        if tag_ids.len() > 200 || global && !tag_ids.is_empty() || !global && tag_ids.is_empty() {
            return Err(corrupt());
        }
        profiles.push(Profile {
            id,
            media_type: m,
            is_global: global,
            position,
            tag_ids,
            settings,
        });
    }
    if globals != 1 {
        return Err(corrupt());
    }
    // Writers use contiguous positions and reserve1024 for a single transactional swap.
    if profiles
        .iter()
        .filter(|p| !p.is_global)
        .enumerate()
        .any(|(i, p)| p.position != i as i64 + 1)
    {
        return Err(corrupt());
    }
    Ok(Catalog {
        revision,
        configured: availability.is_some(),
        availability_delay_days: availability,
        profiles,
    })
}
/// Reads owner tags and chosen policy under the caller's transaction/snapshot.
async fn select_inner(c: &Connection, m: MediaDomain, owner_id: i64) -> Result<EffectiveDelay> {
    let owner = if matches!(m, MediaDomain::Tv) {
        "series"
    } else {
        "movies"
    };
    if c.query(&format!("SELECT 1 FROM {owner} WHERE id=?"), [owner_id])
        .await?
        .next()
        .await?
        .is_none()
    {
        return Err(Error(StatusCode::NOT_FOUND, "delay_owner_not_found"));
    }
    let all = catalog(c, m).await?;
    if !all.configured {
        return Ok(EffectiveDelay::Unconfigured {
            revision: all.revision,
        });
    }
    let tags = crate::tags::assigned(c, m, owner_id)
        .await
        .map_err(|_| corrupt())?;
    let selected = all
        .profiles
        .into_iter()
        .find(|p| p.is_global || p.tag_ids.iter().any(|id| tags.contains(id)))
        .ok_or_else(corrupt)?;
    match selected.settings {
        Settings::LegacyAgeOnly {
            torrent_delay_minutes,
            usenet_delay_minutes,
        } => Ok(EffectiveDelay::LegacyAgeOnly {
            revision: all.revision,
            torrent_delay_minutes: torrent_delay_minutes.ok_or_else(corrupt)?,
            usenet_delay_minutes: usenet_delay_minutes.ok_or_else(corrupt)?,
        }),
        Settings::Profile(settings) => Ok(EffectiveDelay::Profile {
            revision: all.revision,
            id: selected.id,
            settings,
        }),
    }
}
pub(crate) fn validate(input: &Input, global: bool) -> Result<()> {
    let s = &input.settings;
    if s.torrent_delay_minutes > 10080
        || s.usenet_delay_minutes > 10080
        || !s.enable_torrent && !s.enable_usenet
        || input.tag_ids.len() > 200
        || global && !input.tag_ids.is_empty()
        || !global && input.tag_ids.is_empty()
    {
        return Err(invalid());
    }
    let mut unique = std::collections::BTreeSet::new();
    if input
        .tag_ids
        .iter()
        .any(|id| !(1..=MAX_ID).contains(id) || !unique.insert(*id))
    {
        return Err(invalid());
    }
    Ok(())
}
pub(crate) async fn bump(
    c: &Connection,
    m: MediaDomain,
    expected: i64,
    local: bool,
    changed: bool,
) -> Result<()> {
    if c.is_autocommit() {
        return Err(corrupt());
    }
    let current = c
        .query(
            "SELECT revision FROM delay_profile_domains WHERE media_type=?",
            [domain(m)],
        )
        .await?
        .next()
        .await?
        .ok_or_else(corrupt)?
        .get::<i64>(0)?;
    if expected != current || !(1..MAX_ID).contains(&expected) {
        return Err(conflict());
    }
    c.execute("UPDATE delay_profile_domains SET revision=revision+1,locally_edited=CASE WHEN ? THEN 1 ELSE locally_edited END WHERE media_type=?",params![local,domain(m)]).await?;
    if changed {
        crate::revision_policy::wake_pending(c, m)
            .await
            .map_err(|_| corrupt())?;
    }
    Ok(())
}
async fn allocate(c: &Connection) -> Result<i64> {
    let max=c.query("SELECT COALESCE(MAX(id),0) FROM (SELECT id FROM delay_profiles UNION ALL SELECT destination_id FROM snapshot_mappings WHERE destination_table='delay_profiles')",()).await?.next().await?.ok_or_else(corrupt)?.get::<i64>(0)?;
    max.checked_add(1)
        .filter(|id| *id <= MAX_ID)
        .ok_or(Error(StatusCode::CONFLICT, "delay_profile_id_exhausted"))
}
/// Shared validated graph writer; caller owns immediate transaction and final catalog validation.
pub(crate) async fn store(
    c: &Connection,
    m: MediaDomain,
    id: Option<i64>,
    input: &Input,
    availability: Option<i32>,
) -> Result<i64> {
    if c.is_autocommit() {
        return Err(corrupt());
    }
    let all = catalog(c, m).await?;
    let prior = id
        .map(|id| all.profiles.iter().find(|p| p.id == id).ok_or_else(missing))
        .transpose()?;
    let global = prior.is_some_and(|p| p.is_global);
    validate(input, global)?;
    if prior.is_none() && (!all.configured || all.profiles.len() >= MAX_PROFILES) {
        return Err(conflict());
    }
    let id = match id {
        Some(id) => id,
        None => allocate(c).await?,
    };
    for tag in &input.tag_ids {
        if c.query(
            "SELECT 1 FROM tags WHERE id=? AND media_type=?",
            params![*tag, domain(m)],
        )
        .await?
        .next()
        .await?
        .is_none()
        {
            return Err(invalid());
        }
        if c.query(
            "SELECT 1 FROM delay_profile_tags WHERE tag_id=? AND profile_id<>?",
            params![*tag, id],
        )
        .await?
        .next()
        .await?
        .is_some()
        {
            return Err(conflict());
        }
    }
    let s = &input.settings;
    if global {
        let days = availability.or(all.availability_delay_days).unwrap_or(0);
        if !(-365..=365).contains(&days) || matches!(m, MediaDomain::Tv) && days != 0 {
            return Err(invalid());
        }
        c.execute("INSERT INTO release_delay_policies VALUES(?,?,?,?) ON CONFLICT(media_type)DO UPDATE SET torrent_delay_minutes=excluded.torrent_delay_minutes,usenet_delay_minutes=excluded.usenet_delay_minutes,availability_delay_days=excluded.availability_delay_days",params![domain(m),i64::from(s.torrent_delay_minutes),i64::from(s.usenet_delay_minutes),days]).await?;
    }
    let position = prior
        .map(|p| p.position)
        .unwrap_or(all.profiles.len() as i64);
    c.execute("INSERT INTO delay_profiles(id,media_type,is_global,position,semantics,torrent_delay_minutes,usenet_delay_minutes,enable_torrent,enable_usenet,preferred_protocol,bypass_if_highest_quality,bypass_if_above_custom_format_score,minimum_custom_format_score)VALUES(?,?,?,?,'profile',?,?,?,?,?,?,?,?) ON CONFLICT(id)DO UPDATE SET semantics=excluded.semantics,torrent_delay_minutes=excluded.torrent_delay_minutes,usenet_delay_minutes=excluded.usenet_delay_minutes,enable_torrent=excluded.enable_torrent,enable_usenet=excluded.enable_usenet,preferred_protocol=excluded.preferred_protocol,bypass_if_highest_quality=excluded.bypass_if_highest_quality,bypass_if_above_custom_format_score=excluded.bypass_if_above_custom_format_score,minimum_custom_format_score=excluded.minimum_custom_format_score",params![id,domain(m),global,position,(!global).then_some(i64::from(s.torrent_delay_minutes)),(!global).then_some(i64::from(s.usenet_delay_minutes)),s.enable_torrent,s.enable_usenet,s.preferred_protocol.text(),s.bypass_if_highest_quality,s.bypass_if_above_custom_format_score,s.minimum_custom_format_score]).await?;
    c.execute("DELETE FROM delay_profile_tags WHERE profile_id=?", [id])
        .await?;
    for tag in &input.tag_ids {
        c.execute(
            "INSERT INTO delay_profile_tags VALUES(?,?,?)",
            params![id, domain(m), *tag],
        )
        .await?;
    }
    Ok(id)
}
async fn reorder_ids(c: &Connection, m: MediaDomain, ids: &[i64]) -> Result<()> {
    let all = catalog(c, m).await?;
    let expected = all
        .profiles
        .iter()
        .filter(|p| !p.is_global)
        .map(|p| p.id)
        .collect::<std::collections::BTreeSet<_>>();
    if ids.len() != expected.len()
        || ids
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            != expected
    {
        return Err(invalid());
    }
    // At most1023 tagged rows: position1024 is always an in-range spare, even at capacity.
    for (index, id) in ids.iter().enumerate() {
        let wanted = index as i64 + 1;
        let current = c
            .query("SELECT position FROM delay_profiles WHERE id=?", [*id])
            .await?
            .next()
            .await?
            .ok_or_else(corrupt)?
            .get::<i64>(0)?;
        if current == wanted {
            continue;
        }
        c.execute("UPDATE delay_profiles SET position=1024 WHERE id=?", [*id])
            .await?;
        c.execute(
            "UPDATE delay_profiles SET position=? WHERE media_type=? AND position=?",
            params![current, domain(m), wanted],
        )
        .await?;
        c.execute(
            "UPDATE delay_profiles SET position=? WHERE id=?",
            params![wanted, *id],
        )
        .await?;
    }
    Ok(())
}
/// Compatibility writes retain existing full metadata and tagged profiles.
pub(crate) async fn write_legacy(
    c: &Connection,
    m: MediaDomain,
    p: &crate::search::ReleasePolicy,
) -> Result<()> {
    let all = catalog(c, m).await?;
    c.execute("INSERT INTO release_delay_policies VALUES(?,?,?,?) ON CONFLICT(media_type)DO UPDATE SET torrent_delay_minutes=excluded.torrent_delay_minutes,usenet_delay_minutes=excluded.usenet_delay_minutes,availability_delay_days=excluded.availability_delay_days",params![domain(m),i64::from(p.torrent_delay_minutes),i64::from(p.usenet_delay_minutes),p.availability_delay_days]).await?;
    bump(c, m, all.revision, true, true).await?;
    catalog(c, m).await?;
    Ok(())
}

pub async fn catalog(c: &Connection, m: MediaDomain) -> Result<Catalog> {
    if !c.is_autocommit() {
        return catalog_inner(c, m).await;
    }
    let tx = c.transaction().await?;
    let out = catalog_inner(&tx, m).await;
    tx.rollback().await?;
    out
}
pub async fn select(c: &Connection, m: MediaDomain, owner_id: i64) -> Result<EffectiveDelay> {
    if !c.is_autocommit() {
        return select_inner(c, m, owner_id).await;
    }
    let tx = c.transaction().await?;
    let out = select_inner(&tx, m, owner_id).await;
    tx.rollback().await?;
    out
}

use crate::db::Database;
use axum::{
    Extension, Router,
    extract::{DefaultBodyLimit, Path, Query, State, rejection::JsonRejection},
    routing::get,
};
use std::{collections::BTreeMap, sync::Arc};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RevisionQuery {
    revision: i64,
}
fn no_query(q: &BTreeMap<String, String>) -> Result<()> {
    if q.is_empty() { Ok(()) } else { Err(invalid()) }
}
fn body<T>(v: std::result::Result<Json<T>, JsonRejection>) -> Result<T> {
    v.map(|Json(v)| v).map_err(|_| invalid())
}
fn valid_id(id: i64) -> Result<()> {
    if (1..=MAX_ID).contains(&id) {
        Ok(())
    } else {
        Err(invalid())
    }
}
pub fn router(db: Arc<Database>) -> Router {
    let mut router = Router::new();
    for m in [MediaDomain::Tv, MediaDomain::Movies] {
        let p = format!("/api/v1/{}/delay-profiles", domain(m));
        router = router.merge(
            Router::new()
                .route(&p, get(list).post(create))
                .route(&format!("{p}/schema"), get(schema))
                .route(&format!("{p}/reorder"), axum::routing::put(reorder))
                .route(&format!("{p}/{{id}}"), get(one).put(update).delete(delete))
                .layer(Extension(m))
                .layer(DefaultBodyLimit::max(32 * 1024))
                .with_state(db.clone()),
        );
    }
    router
}
async fn bounded<T>(f: impl std::future::Future<Output = Result<T>>) -> Result<T> {
    tokio::time::timeout(std::time::Duration::from_secs(5), f)
        .await
        .map_err(|_| Error(StatusCode::SERVICE_UNAVAILABLE, "delay_profile_timeout"))?
}
async fn list(
    State(db): State<Arc<Database>>,
    Extension(m): Extension<MediaDomain>,
    Query(q): Query<BTreeMap<String, String>>,
) -> Result<Json<Catalog>> {
    no_query(&q)?;
    bounded(async { Ok(Json(catalog(&db.connect().await?, m).await?)) }).await
}
async fn schema(Query(q): Query<BTreeMap<String, String>>) -> Result<Json<Input>> {
    no_query(&q)?;
    Ok(Json(Input::default()))
}
async fn one(
    State(db): State<Arc<Database>>,
    Extension(m): Extension<MediaDomain>,
    Path(id): Path<i64>,
    Query(q): Query<BTreeMap<String, String>>,
) -> Result<Json<Profile>> {
    no_query(&q)?;
    valid_id(id)?;
    bounded(async {
        Ok(Json(
            catalog(&db.connect().await?, m)
                .await?
                .profiles
                .into_iter()
                .find(|p| p.id == id)
                .ok_or_else(missing)?,
        ))
    })
    .await
}
async fn save(
    db: Arc<Database>,
    m: MediaDomain,
    id: Option<i64>,
    v: Write,
) -> Result<Json<Catalog>> {
    if let Some(id) = id {
        valid_id(id)?
    }
    bounded(async {
        let c = db.connect().await?;
        let tx = c
            .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
            .await?;
        let out = async {
            let before = catalog(&tx, m).await?;
            if before.revision != v.revision {
                return Err(conflict());
            }
            store(&tx, m, id, &v.profile, None).await?;
            bump(&tx, m, v.revision, true, true).await?;
            catalog(&tx, m).await
        }
        .await;
        match out {
            Ok(v) => {
                tx.commit().await?;
                Ok(Json(v))
            }
            Err(e) => {
                tx.rollback().await?;
                Err(e)
            }
        }
    })
    .await
}
async fn create(
    State(db): State<Arc<Database>>,
    Extension(m): Extension<MediaDomain>,
    Query(q): Query<BTreeMap<String, String>>,
    v: std::result::Result<Json<Write>, JsonRejection>,
) -> Result<(StatusCode, Json<Catalog>)> {
    no_query(&q)?;
    Ok((StatusCode::CREATED, save(db, m, None, body(v)?).await?))
}
async fn update(
    State(db): State<Arc<Database>>,
    Extension(m): Extension<MediaDomain>,
    Path(id): Path<i64>,
    Query(q): Query<BTreeMap<String, String>>,
    v: std::result::Result<Json<Write>, JsonRejection>,
) -> Result<Json<Catalog>> {
    no_query(&q)?;
    save(db, m, Some(id), body(v)?).await
}
async fn reorder(
    State(db): State<Arc<Database>>,
    Extension(m): Extension<MediaDomain>,
    Query(q): Query<BTreeMap<String, String>>,
    v: std::result::Result<Json<Reorder>, JsonRejection>,
) -> Result<Json<Catalog>> {
    no_query(&q)?;
    let v = body(v)?;
    if v.ids.len() >= MAX_PROFILES {
        return Err(invalid());
    }
    bounded(async {
        let c = db.connect().await?;
        let tx = c
            .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
            .await?;
        let out = async {
            bump(&tx, m, v.revision, true, true).await?;
            reorder_ids(&tx, m, &v.ids).await?;
            catalog(&tx, m).await
        }
        .await;
        match out {
            Ok(v) => {
                tx.commit().await?;
                Ok(Json(v))
            }
            Err(e) => {
                tx.rollback().await?;
                Err(e)
            }
        }
    })
    .await
}
async fn delete(
    State(db): State<Arc<Database>>,
    Extension(m): Extension<MediaDomain>,
    Path(id): Path<i64>,
    Query(q): Query<RevisionQuery>,
) -> Result<Json<Catalog>> {
    valid_id(id)?;
    bounded(async {
        let c = db.connect().await?;
        let tx = c
            .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
            .await?;
        let out = async {
            let all = catalog(&tx, m).await?;
            let p = all
                .profiles
                .iter()
                .find(|p| p.id == id)
                .ok_or_else(missing)?;
            if p.is_global {
                return Err(Error(
                    StatusCode::CONFLICT,
                    "global_delay_profile_protected",
                ));
            }
            let removed_position = p.position;
            bump(&tx, m, q.revision, true, true).await?;
            tx.execute("DELETE FROM delay_profiles WHERE id=?", [id])
                .await?;
            for p in all
                .profiles
                .iter()
                .filter(|p| !p.is_global && p.position > removed_position)
            {
                tx.execute(
                    "UPDATE delay_profiles SET position=position-1 WHERE id=?",
                    [p.id],
                )
                .await?;
            }
            catalog(&tx, m).await
        }
        .await;
        match out {
            Ok(v) => {
                tx.commit().await?;
                Ok(Json(v))
            }
            Err(e) => {
                tx.rollback().await?;
                Err(e)
            }
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn capacity_reorder_legacy_adapter_and_missing_metadata() {
        struct Scratch(std::path::PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                std::fs::remove_dir_all(&self.0).unwrap();
            }
        }
        let s =
            Scratch(std::env::temp_dir().join(format!("delay-capacity-{}", uuid::Uuid::new_v4())));
        std::fs::create_dir(&s.0).unwrap();
        let db = Database::open_local(s.0.join("db")).await.unwrap();
        let c = db.connect().await.unwrap();
        let tx = c
            .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
            .await
            .unwrap();
        let old = crate::search::ReleasePolicy {
            torrent_delay_minutes: 17,
            usenet_delay_minutes: 23,
            availability_delay_days: 0,
        };
        write_legacy(&tx, MediaDomain::Tv, &old).await.unwrap();
        assert!(matches!(
            catalog(&tx, MediaDomain::Tv).await.unwrap().profiles[0].settings,
            Settings::LegacyAgeOnly {
                torrent_delay_minutes: Some(17),
                usenet_delay_minutes: Some(23)
            }
        ));
        store(&tx, MediaDomain::Tv, Some(1), &Input::default(), None)
            .await
            .unwrap();
        // Fill using validated schema constraints; avoid quadratic API setup at this boundary.
        for i in 1..1024i64 {
            tx.execute(
                "INSERT INTO tags(id,media_type,label)VALUES(?,'tv',?)",
                params![i, format!("t{i}")],
            )
            .await
            .unwrap();
            tx.execute("INSERT INTO delay_profiles(id,media_type,is_global,position,semantics,torrent_delay_minutes,usenet_delay_minutes,enable_torrent,enable_usenet,preferred_protocol,bypass_if_highest_quality,bypass_if_above_custom_format_score,minimum_custom_format_score)VALUES(?,'tv',0,?,'profile',0,0,1,1,'usenet',0,0,0)",params![i+2,i]).await.unwrap();
            tx.execute(
                "INSERT INTO delay_profile_tags VALUES(?,'tv',?)",
                params![i + 2, i],
            )
            .await
            .unwrap();
        }
        let mut ids = (3..1026).collect::<Vec<i64>>();
        ids.reverse();
        reorder_ids(&tx, MediaDomain::Tv, &ids).await.unwrap();
        assert_eq!(
            catalog(&tx, MediaDomain::Tv).await.unwrap().profiles[0].id,
            1025
        );
        // UPSERT's BEFORE INSERT trigger must permit editing an existing row at capacity.
        store(&tx, MediaDomain::Tv, Some(1), &Input::default(), None)
            .await
            .unwrap();
        assert!(
            store(
                &tx,
                MediaDomain::Tv,
                None,
                &Input {
                    tag_ids: vec![1],
                    ..Input::default()
                },
                None
            )
            .await
            .is_err()
        );
        write_legacy(&tx, MediaDomain::Tv, &old).await.unwrap();
        let all = catalog(&tx, MediaDomain::Tv).await.unwrap();
        assert_eq!(all.profiles.len(), 1024);
        assert!(
            matches!(all.profiles.last().unwrap().settings,Settings::Profile(ref s)if s.torrent_delay_minutes==17&&s.preferred_protocol==Protocol::Usenet)
        );
        tx.execute("UPDATE delay_profiles SET position=1024 WHERE id=1025", ())
            .await
            .unwrap();
        assert!(catalog(&tx, MediaDomain::Tv).await.is_err());
        tx.rollback().await.unwrap();
        // Corruption must not synthesize metadata for a configured legacy row.
        c.execute("INSERT INTO release_delay_policies VALUES('tv',1,2,0)", ())
            .await
            .unwrap();
        c.execute_batch("DROP TRIGGER delay_global_delete;DELETE FROM delay_profiles WHERE id=1;")
            .await
            .unwrap();
        assert!(catalog(&c, MediaDomain::Tv).await.is_err());
    }
}
