//! Domain-specific restriction catalogs. Every applicable enabled profile participates.
use crate::{api::MediaDomain, db::Database};
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use libsql::{Connection, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
#[ts(rename = "ReleaseProfileSourceApplication")]
pub enum SourceApplication {
    Sonarr,
    Radarr,
}
impl SourceApplication {
    fn name(self) -> &'static str {
        match self {
            Self::Sonarr => "sonarr",
            Self::Radarr => "radarr",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[ts(rename = "ReleaseProfileIndexer")]
pub enum IndexerReference {
    Provider {
        id: uuid::Uuid,
    },
    UnresolvedSource {
        application: SourceApplication,
        fingerprint: String,
        source_id: i64,
    },
}
macro_rules! input_type {($name:ident,$export:literal,$($extra:tt)*)=>{
 #[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize,ts_rs::TS)]
 #[serde(deny_unknown_fields)]
 #[ts(rename=$export)]
 pub struct $name {
  pub name:Option<String>,pub enabled:bool,pub required:Vec<String>,pub ignored:Vec<String>,pub tag_ids:Vec<i64>,pub indexers:Vec<IndexerReference>,$($extra)*
 }
};}
input_type!(TvInput,"TvReleaseProfileInput",pub excluded_tag_ids:Vec<i64>,pub air_date_restriction:bool,pub air_date_grace_period_days:i32,pub allow_season_pack_without_all_episodes_aired:bool,);
input_type!(MovieInput, "MovieReleaseProfileInput",);
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(tag = "media_type", rename_all = "lowercase")]
#[ts(rename = "ReleaseProfile")]
pub enum Profile {
    Tv {
        id: i64,
        #[serde(flatten)]
        input: TvInput,
    },
    Movies {
        id: i64,
        #[serde(flatten)]
        input: MovieInput,
    },
}
#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(rename = "ReleaseProfileCatalog")]
pub struct Catalog {
    pub media_type: MediaDomain,
    pub revision: i64,
    pub profiles: Vec<Profile>,
}
#[derive(Debug, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "ReleaseProfileWrite")]
pub struct Write<T> {
    pub revision: i64,
    pub profile: T,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TvPolicy {
    pub excluded_tag_ids: Vec<i64>,
    pub air_date_restriction: bool,
    pub grace_days: i32,
    pub allow_unaired_pack: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Definition {
    pub name: Option<String>,
    pub enabled: bool,
    pub required: Vec<String>,
    pub ignored: Vec<String>,
    pub tag_ids: Vec<i64>,
    pub indexers: Vec<IndexerReference>,
    pub tv: Option<TvPolicy>,
}
impl From<TvInput> for Definition {
    fn from(v: TvInput) -> Self {
        Self {
            name: v.name,
            enabled: v.enabled,
            required: v.required,
            ignored: v.ignored,
            tag_ids: v.tag_ids,
            indexers: v.indexers,
            tv: Some(TvPolicy {
                excluded_tag_ids: v.excluded_tag_ids,
                air_date_restriction: v.air_date_restriction,
                grace_days: v.air_date_grace_period_days,
                allow_unaired_pack: v.allow_season_pack_without_all_episodes_aired,
            }),
        }
    }
}
impl From<MovieInput> for Definition {
    fn from(v: MovieInput) -> Self {
        Self {
            name: v.name,
            enabled: v.enabled,
            required: v.required,
            ignored: v.ignored,
            tag_ids: v.tag_ids,
            indexers: v.indexers,
            tv: None,
        }
    }
}
impl Definition {
    pub(crate) fn canonicalize(&mut self) {
        self.tag_ids.sort_unstable();
        if let Some(tv) = self.tv.as_mut() {
            tv.excluded_tag_ids.sort_unstable()
        }
        self.indexers.sort_by_key(IndexerReference::key);
    }
    fn resource(self, id: i64) -> Profile {
        match self.tv {
            Some(tv) => Profile::Tv {
                id,
                input: TvInput {
                    name: self.name,
                    enabled: self.enabled,
                    required: self.required,
                    ignored: self.ignored,
                    tag_ids: self.tag_ids,
                    indexers: self.indexers,
                    excluded_tag_ids: tv.excluded_tag_ids,
                    air_date_restriction: tv.air_date_restriction,
                    air_date_grace_period_days: tv.grace_days,
                    allow_season_pack_without_all_episodes_aired: tv.allow_unaired_pack,
                },
            },
            None => Profile::Movies {
                id,
                input: MovieInput {
                    name: self.name,
                    enabled: self.enabled,
                    required: self.required,
                    ignored: self.ignored,
                    tag_ids: self.tag_ids,
                    indexers: self.indexers,
                },
            },
        }
    }
    pub(crate) fn default_for(m: MediaDomain) -> Self {
        Self {
            name: None,
            enabled: true,
            required: vec![],
            ignored: vec![],
            tag_ids: vec![],
            indexers: vec![],
            tv: matches!(m, MediaDomain::Tv).then_some(TvPolicy {
                excluded_tag_ids: vec![],
                air_date_restriction: false,
                grace_days: 0,
                allow_unaired_pack: false,
            }),
        }
    }
}
impl Profile {
    pub(crate) fn id(&self) -> i64 {
        match self {
            Self::Tv { id, .. } | Self::Movies { id, .. } => *id,
        }
    }
    pub(crate) fn definition(&self) -> Definition {
        match self {
            Self::Tv { input, .. } => input.clone().into(),
            Self::Movies { input, .. } => input.clone().into(),
        }
    }
}
#[derive(Debug)]
pub struct Error(StatusCode, &'static str);
impl Error {
    pub(crate) fn code(&self) -> &'static str {
        self.1
    }
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (
            self.0,
            Json(crate::api::ApiErrorEnvelope::new(
                self.1,
                "Release profile operation failed",
            )),
        )
            .into_response()
    }
}
impl From<libsql::Error> for Error {
    fn from(_: libsql::Error) -> Self {
        Self(
            StatusCode::SERVICE_UNAVAILABLE,
            "release_profile_storage_error",
        )
    }
}
pub(crate) type Result<T> = std::result::Result<T, Error>;
fn invalid() -> Error {
    Error(StatusCode::BAD_REQUEST, "invalid_release_profile")
}
fn corrupt() -> Error {
    Error(StatusCode::SERVICE_UNAVAILABLE, "release_profile_invariant")
}
fn conflict() -> Error {
    Error(StatusCode::CONFLICT, "release_profile_conflict")
}
fn missing() -> Error {
    Error(StatusCode::NOT_FOUND, "release_profile_not_found")
}
fn domain(m: MediaDomain) -> &'static str {
    crate::revision_policy::domain(m)
}
const MAX_ID: i64 = 9_007_199_254_740_991;
const MAX_PROFILES: usize = 1024;
const MAX_MEMBERSHIPS: usize = 8192;
fn valid_ids(ids: &[i64]) -> bool {
    ids.iter().all(|id| (1..=MAX_ID).contains(id))
        && ids.iter().collect::<BTreeSet<_>>().len() == ids.len()
}
impl IndexerReference {
    fn key(&self) -> String {
        match self {
            Self::Provider { id } => format!("provider:{id}"),
            Self::UnresolvedSource {
                application,
                fingerprint,
                source_id,
            } => format!("source:{}:{fingerprint}:{source_id}", application.name()),
        }
    }
}
pub(crate) fn validate_shape(m: MediaDomain, v: &Definition) -> Result<()> {
    if v.tv.is_some() != matches!(m, MediaDomain::Tv)
        || v.name.as_ref().is_some_and(|n| n.len() > 256)
        || v.required.len() > 200
        || v.ignored.len() > 200
    {
        return Err(invalid());
    }
    if v.required
        .iter()
        .chain(&v.ignored)
        .any(|t| t.len() > 2048 || t.trim().is_empty())
    {
        return Err(invalid());
    }
    let exclusions =
        v.tv.as_ref()
            .map(|t| t.excluded_tag_ids.as_slice())
            .unwrap_or(&[]);
    if v.required.is_empty()
        && v.ignored.is_empty()
        && !v
            .tv
            .as_ref()
            .is_some_and(|t| t.air_date_restriction || t.allow_unaired_pack)
    {
        return Err(invalid());
    }
    if v.tag_ids.len() + exclusions.len() > 200
        || !valid_ids(&v.tag_ids)
        || !valid_ids(exclusions)
        || exclusions.iter().any(|i| v.tag_ids.contains(i))
    {
        return Err(invalid());
    }
    if v.indexers.len() > if matches!(m, MediaDomain::Tv) { 200 } else { 1 }
        || v.indexers
            .iter()
            .map(IndexerReference::key)
            .collect::<BTreeSet<_>>()
            .len()
            != v.indexers.len()
    {
        return Err(invalid());
    }
    for reference in &v.indexers {
        if let IndexerReference::UnresolvedSource {
            application,
            fingerprint,
            source_id,
        } = reference
        {
            if v.enabled
                || application.name()
                    != if matches!(m, MediaDomain::Tv) {
                        "sonarr"
                    } else {
                        "radarr"
                    }
                || fingerprint.len() != 64
                || !fingerprint
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                || !(1..=MAX_ID).contains(source_id)
            {
                return Err(invalid());
            }
        }
    }
    Ok(())
}
pub(crate) fn validate_catalog(m: MediaDomain, definitions: &[Definition]) -> Result<()> {
    if definitions.len() > MAX_PROFILES {
        return Err(invalid());
    }
    let (mut count, mut bytes, mut refs) = (0usize, 0usize, 0usize);
    for v in definitions {
        validate_shape(m, v)?;
        count += v.required.len() + v.ignored.len();
        bytes += v
            .required
            .iter()
            .chain(&v.ignored)
            .map(String::len)
            .sum::<usize>();
        refs += v.tag_ids.len()
            + v.indexers.len()
            + v.tv.as_ref().map_or(0, |t| t.excluded_tag_ids.len());
    }
    if count > 4096 || bytes > 1024 * 1024 || refs > MAX_MEMBERSHIPS {
        return Err(invalid());
    }
    Ok(())
}
async fn catalog_inner(c: &Connection, m: MediaDomain) -> Result<Catalog> {
    let revision = c
        .query(
            "SELECT revision FROM release_profile_domains WHERE media_type=?",
            [domain(m)],
        )
        .await?
        .next()
        .await?
        .ok_or_else(corrupt)?
        .get(0)?;
    let mut rows=c.query("SELECT id,name,enabled,required_json,ignored_json,air_date_restriction,air_date_grace_period_days,allow_season_pack_without_all_episodes_aired FROM release_profiles WHERE media_type=? ORDER BY id LIMIT 1025",[domain(m)]).await?;
    let mut profiles = vec![];
    let mut definitions = vec![];
    let (mut term_count, mut term_bytes, mut memberships) = (0usize, 0usize, 0usize);
    while let Some(r) = rows.next().await? {
        if profiles.len() == MAX_PROFILES {
            return Err(corrupt());
        }
        let id = r.get::<i64>(0)?;
        let mut definition = Definition {
            name: r.get(1)?,
            enabled: r.get::<i64>(2)? == 1,
            required: serde_json::from_str(&r.get::<String>(3)?).map_err(|_| corrupt())?,
            ignored: serde_json::from_str(&r.get::<String>(4)?).map_err(|_| corrupt())?,
            tag_ids: vec![],
            indexers: vec![],
            tv: if matches!(m, MediaDomain::Tv) {
                Some(TvPolicy {
                    excluded_tag_ids: vec![],
                    air_date_restriction: r.get::<i64>(5)? == 1,
                    grace_days: r.get(6)?,
                    allow_unaired_pack: r.get::<i64>(7)? == 1,
                })
            } else {
                None
            },
        };
        let mut tags=c.query("SELECT tag_id,kind FROM release_profile_tags WHERE profile_id=? ORDER BY tag_id LIMIT 201",[id]).await?;
        while let Some(t) = tags.next().await? {
            let tag = t.get(0)?;
            match t.get::<String>(1)?.as_str() {
                "include" => definition.tag_ids.push(tag),
                "exclude" => definition
                    .tv
                    .as_mut()
                    .ok_or_else(corrupt)?
                    .excluded_tag_ids
                    .push(tag),
                _ => return Err(corrupt()),
            }
        }
        let mut indexers=c.query("SELECT provider_id,source_application,source_fingerprint,source_id FROM release_profile_indexers WHERE profile_id=? ORDER BY reference_key LIMIT 201",[id]).await?;
        while let Some(i) = indexers.next().await? {
            let reference = if let Some(raw) = i.get::<Option<String>>(0)? {
                IndexerReference::Provider {
                    id: raw.parse().map_err(|_| corrupt())?,
                }
            } else {
                IndexerReference::UnresolvedSource {
                    application: match i.get::<String>(1)?.as_str() {
                        "sonarr" => SourceApplication::Sonarr,
                        "radarr" => SourceApplication::Radarr,
                        _ => return Err(corrupt()),
                    },
                    fingerprint: i.get(2)?,
                    source_id: i.get(3)?,
                }
            };
            definition.indexers.push(reference);
        }
        validate_shape(m, &definition).map_err(|_| corrupt())?;
        term_count += definition.required.len() + definition.ignored.len();
        term_bytes += definition
            .required
            .iter()
            .chain(&definition.ignored)
            .map(String::len)
            .sum::<usize>();
        memberships += definition.tag_ids.len()
            + definition.indexers.len()
            + definition
                .tv
                .as_ref()
                .map_or(0, |t| t.excluded_tag_ids.len());
        if term_count > 4096 || term_bytes > 1024 * 1024 || memberships > MAX_MEMBERSHIPS {
            return Err(corrupt());
        }
        validate_references(c, m, &definition)
            .await
            .map_err(|error| {
                if error.code() == "release_profile_storage_error" {
                    error
                } else {
                    corrupt()
                }
            })?;
        definitions.push(definition.clone());
        profiles.push(definition.resource(id));
    }
    validate_catalog(m, &definitions).map_err(|_| corrupt())?;
    let result = Catalog {
        media_type: m,
        revision,
        profiles,
    };
    if serde_json::to_vec(&result).map_err(|_| corrupt())?.len() > 4 * 1024 * 1024 {
        return Err(corrupt());
    }
    Ok(result)
}
pub(crate) async fn catalog(c: &Connection, m: MediaDomain) -> Result<Catalog> {
    if !c.is_autocommit() {
        return catalog_inner(c, m).await;
    }
    let tx = c.transaction().await?;
    let result = catalog_inner(&tx, m).await;
    tx.rollback().await?;
    result
}
async fn validate_references(c: &Connection, m: MediaDomain, v: &Definition) -> Result<()> {
    for tag in v
        .tag_ids
        .iter()
        .chain(v.tv.iter().flat_map(|t| &t.excluded_tag_ids))
    {
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
    }
    for reference in &v.indexers {
        if let IndexerReference::Provider { id } = reference {
            if c.query("SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=? AND s.media_type=? AND p.implementation IN ('torznab','newznab')",params![id.to_string(),domain(m)]).await?.next().await?.is_none(){return Err(Error(StatusCode::CONFLICT,"release_profile_indexer_unavailable"))}
        }
    }
    Ok(())
}
/// All enabled matches, not an ordered winner. Caller rereads under final admission transaction.
pub(crate) async fn applicable(
    c: &Connection,
    m: MediaDomain,
    owner: i64,
    indexer: uuid::Uuid,
) -> Result<Catalog> {
    if c.is_autocommit() {
        let tx = c.transaction().await?;
        let result = applicable_inner(&tx, m, owner, indexer).await;
        tx.rollback().await?;
        result
    } else {
        applicable_inner(c, m, owner, indexer).await
    }
}
async fn applicable_inner(
    c: &Connection,
    m: MediaDomain,
    owner: i64,
    indexer: uuid::Uuid,
) -> Result<Catalog> {
    let table = if matches!(m, MediaDomain::Tv) {
        "series"
    } else {
        "movies"
    };
    if c.query(&format!("SELECT 1 FROM {table} WHERE id=?"), [owner])
        .await?
        .next()
        .await?
        .is_none()
    {
        return Err(missing());
    }
    let tags = crate::tags::assigned(c, m, owner).await.map_err(|e| {
        if e.1 == "database_error" {
            Error(
                StatusCode::SERVICE_UNAVAILABLE,
                "release_profile_storage_error",
            )
        } else {
            corrupt()
        }
    })?;
    let mut all = catalog(c, m).await?;
    all.profiles.retain(|profile| {
        let p = profile.definition();
        p.enabled
            && (p.tag_ids.is_empty() || p.tag_ids.iter().any(|t| tags.contains(t)))
            && !p
                .tv
                .as_ref()
                .is_some_and(|t| t.excluded_tag_ids.iter().any(|id| tags.contains(id)))
            && (p.indexers.is_empty()
                || p.indexers
                    .iter()
                    .any(|r| matches!(r,IndexerReference::Provider{id}if *id==indexer)))
    });
    Ok(all)
}

pub(crate) async fn bump(c: &Connection, m: MediaDomain, expected: i64, local: bool) -> Result<()> {
    if c.is_autocommit() {
        return Err(corrupt());
    }
    let old = c
        .query(
            "SELECT revision FROM release_profile_domains WHERE media_type=?",
            [domain(m)],
        )
        .await?
        .next()
        .await?
        .ok_or_else(corrupt)?
        .get::<i64>(0)?;
    if old != expected || !(1..MAX_ID).contains(&expected) {
        return Err(conflict());
    }
    c.execute("UPDATE release_profile_domains SET revision=revision+1,locally_edited=CASE WHEN ? THEN 1 ELSE locally_edited END WHERE media_type=?",params![local,domain(m)]).await?;
    crate::revision_policy::wake_pending(c, m)
        .await
        .map_err(|_| {
            Error(
                StatusCode::SERVICE_UNAVAILABLE,
                "release_profile_storage_error",
            )
        })?;
    Ok(())
}
pub(crate) async fn allocate(c: &Connection) -> Result<i64> {
    let max=c.query("SELECT COALESCE(MAX(id),0) FROM (SELECT id FROM release_profiles UNION ALL SELECT destination_id FROM snapshot_mappings WHERE destination_table='release_profiles')",()).await?.next().await?.ok_or_else(corrupt)?.get::<i64>(0)?;
    max.checked_add(1)
        .filter(|id| *id <= MAX_ID)
        .ok_or(Error(StatusCode::CONFLICT, "release_profile_id_exhausted"))
}
/// Graph persistence only. Callers must carry real prepared matcher validation before activation.
/// No HTTP/snapshot caller is wired until that dependency is delivered.
pub(crate) async fn store(
    c: &Connection,
    m: MediaDomain,
    id: Option<i64>,
    mut v: Definition,
    source: bool,
) -> Result<i64> {
    if c.is_autocommit() {
        return Err(corrupt());
    }
    validate_shape(m, &v)?;
    validate_references(c, m, &v).await?;
    let mut all = catalog(c, m).await?;
    let old = id
        .map(|id| {
            all.profiles
                .iter()
                .find(|p| p.id() == id)
                .map(Profile::definition)
                .ok_or_else(missing)
        })
        .transpose()?;
    if !source {
        for reference in &v.indexers {
            if matches!(reference, IndexerReference::UnresolvedSource { .. })
                && !old.as_ref().is_some_and(|p| p.indexers.contains(reference))
            {
                return Err(invalid());
            }
        }
    }
    v.tag_ids.sort_unstable();
    if let Some(t) = v.tv.as_mut() {
        t.excluded_tag_ids.sort_unstable()
    }
    v.indexers.sort_by_key(IndexerReference::key);
    let id = match id {
        Some(id) => id,
        None => allocate(c).await?,
    };
    all.profiles.retain(|p| p.id() != id);
    all.profiles.push(v.clone().resource(id));
    all.profiles.sort_by_key(Profile::id);
    validate_catalog(
        m,
        &all.profiles
            .iter()
            .map(Profile::definition)
            .collect::<Vec<_>>(),
    )?;
    if serde_json::to_vec(&all).map_err(|_| invalid())?.len() > 4 * 1024 * 1024 {
        return Err(invalid());
    }
    // Temporarily disabled while replacing references: final desired state is set only after joins.
    c.execute("INSERT INTO release_profiles(id,media_type,name,enabled,required_json,ignored_json,air_date_restriction,air_date_grace_period_days,allow_season_pack_without_all_episodes_aired)VALUES(?,?,?,0,?,?,?,?,?) ON CONFLICT(id)DO UPDATE SET name=excluded.name,enabled=0,required_json=excluded.required_json,ignored_json=excluded.ignored_json,air_date_restriction=excluded.air_date_restriction,air_date_grace_period_days=excluded.air_date_grace_period_days,allow_season_pack_without_all_episodes_aired=excluded.allow_season_pack_without_all_episodes_aired",params![id,domain(m),v.name.clone(),serde_json::to_string(&v.required).map_err(|_|invalid())?,serde_json::to_string(&v.ignored).map_err(|_|invalid())?,v.tv.as_ref().map(|t|i64::from(t.air_date_restriction)),v.tv.as_ref().map(|t|t.grace_days),v.tv.as_ref().map(|t|i64::from(t.allow_unaired_pack))]).await?;
    c.execute("DELETE FROM release_profile_tags WHERE profile_id=?", [id])
        .await?;
    c.execute(
        "DELETE FROM release_profile_indexers WHERE profile_id=?",
        [id],
    )
    .await?;
    for (kind, ids) in [
        ("include", v.tag_ids.as_slice()),
        (
            "exclude",
            v.tv.as_ref()
                .map_or(&[][..], |t| t.excluded_tag_ids.as_slice()),
        ),
    ] {
        for tag in ids {
            c.execute(
                "INSERT INTO release_profile_tags VALUES(?,?,?,?)",
                params![id, domain(m), *tag, kind],
            )
            .await?;
        }
    }
    for reference in &v.indexers {
        let (provider, app, fingerprint, source_id) = match reference {
            IndexerReference::Provider { id } => (Some(id.to_string()), None, None, None),
            IndexerReference::UnresolvedSource {
                application,
                fingerprint,
                source_id,
            } => (
                None,
                Some(application.name()),
                Some(fingerprint.clone()),
                Some(*source_id),
            ),
        };
        c.execute(
            "INSERT INTO release_profile_indexers VALUES(?,?,?,?,?,?,?)",
            params![
                id,
                domain(m),
                reference.key(),
                provider,
                app,
                fingerprint,
                source_id
            ],
        )
        .await?;
    }
    c.execute(
        "UPDATE release_profiles SET enabled=? WHERE id=?",
        params![v.enabled, id],
    )
    .await?;
    catalog(c, m).await?;
    Ok(id)
}
/// Prospective provider settings check; do not put guards on transient scope DELETE/INSERT.
pub(crate) async fn provider_scopes_allowed(
    c: &Connection,
    id: &str,
    settings: Option<&crate::providers::ProviderSettings>,
) -> Result<()> {
    let mut rows = c
        .query(
            "SELECT DISTINCT media_type FROM release_profile_indexers WHERE provider_id=? LIMIT 3",
            [id],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        let media = row.get::<String>(0)?;
        let supported = match settings {
            Some(crate::providers::ProviderSettings::Torznab { tv, movies, .. })
            | Some(crate::providers::ProviderSettings::Newznab { tv, movies, .. }) => {
                match media.as_str() {
                    "tv" => tv.is_some(),
                    "movies" => movies.is_some(),
                    _ => false,
                }
            }
            _ => false,
        };
        if !supported {
            return Err(Error(StatusCode::CONFLICT, "provider_in_use"));
        }
    }
    Ok(())
}

pub(crate) fn terms(definitions: &[Definition]) -> Vec<crate::release_profile_terms::ProfileTerms> {
    definitions
        .iter()
        .map(|v| crate::release_profile_terms::ProfileTerms {
            required: v.required.clone(),
            ignored: v.ignored.clone(),
        })
        .collect()
}
fn matcher_error(error: crate::release_profile_terms::TermError) -> Error {
    Error(
        if error.retryable() {
            StatusCode::SERVICE_UNAVAILABLE
        } else {
            StatusCode::BAD_REQUEST
        },
        error.code(),
    )
}
use axum::{
    Extension, Router,
    extract::{DefaultBodyLimit, Path, Query, State, rejection::JsonRejection},
    routing::get,
};
use std::{collections::BTreeMap, sync::Arc};
#[derive(Serialize, ts_rs::TS)]
#[ts(rename = "ReleaseProfileDetail")]
pub struct Detail {
    pub revision: i64,
    pub profile: Profile,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RevisionQuery {
    revision: i64,
}
fn no_query(q: &BTreeMap<String, String>) -> Result<()> {
    if q.is_empty() { Ok(()) } else { Err(invalid()) }
}
fn input(m: MediaDomain, v: serde_json::Value) -> Result<Definition> {
    match m {
        MediaDomain::Tv => serde_json::from_value::<TvInput>(v).map(Into::into),
        MediaDomain::Movies => serde_json::from_value::<MovieInput>(v).map(Into::into),
    }
    .map_err(|_| invalid())
}
fn body(
    v: std::result::Result<Json<Write<serde_json::Value>>, JsonRejection>,
) -> Result<Write<serde_json::Value>> {
    v.map(|Json(v)| v).map_err(|_| invalid())
}
async fn bounded<T>(f: impl std::future::Future<Output = Result<T>>) -> Result<T> {
    tokio::time::timeout(std::time::Duration::from_secs(15), f)
        .await
        .map_err(|_| Error(StatusCode::SERVICE_UNAVAILABLE, "release_profile_timeout"))?
}
pub fn router(db: Arc<Database>) -> Router {
    let mut router = Router::new();
    for m in [MediaDomain::Tv, MediaDomain::Movies] {
        let p = format!("/api/v1/{}/release-profiles", domain(m));
        router = router.merge(
            Router::new()
                .route(&p, get(list).post(create))
                .route(&format!("{p}/schema"), get(schema))
                .route(&format!("{p}/{{id}}"), get(one).put(update).delete(delete))
                .layer(Extension(m))
                .layer(DefaultBodyLimit::max(128 * 1024))
                .with_state(db.clone()),
        );
    }
    router
}
async fn list(
    State(db): State<Arc<Database>>,
    Extension(m): Extension<MediaDomain>,
    Query(q): Query<BTreeMap<String, String>>,
) -> Result<Json<Catalog>> {
    no_query(&q)?;
    bounded(async { Ok(Json(catalog(&db.connect().await?, m).await?)) }).await
}
async fn schema(
    Extension(m): Extension<MediaDomain>,
    Query(q): Query<BTreeMap<String, String>>,
) -> Result<Json<serde_json::Value>> {
    no_query(&q)?;
    let value = match Definition::default_for(m).resource(0) {
        Profile::Tv { input, .. } => serde_json::to_value(input),
        Profile::Movies { input, .. } => serde_json::to_value(input),
    }
    .map_err(|_| corrupt())?;
    Ok(Json(value))
}
async fn one(
    State(db): State<Arc<Database>>,
    Extension(m): Extension<MediaDomain>,
    Path(id): Path<i64>,
    Query(q): Query<BTreeMap<String, String>>,
) -> Result<Json<Detail>> {
    no_query(&q)?;
    bounded(async {
        let all = catalog(&db.connect().await?, m).await?;
        Ok(Json(Detail {
            revision: all.revision,
            profile: all
                .profiles
                .into_iter()
                .find(|p| p.id() == id)
                .ok_or_else(missing)?,
        }))
    })
    .await
}
fn proposed(all: &Catalog, id: Option<i64>, value: Option<&Definition>) -> Result<Vec<Definition>> {
    let mut definitions = vec![];
    let mut found = id.is_none();
    for p in &all.profiles {
        if Some(p.id()) == id {
            found = true;
            if let Some(v) = value {
                definitions.push(v.clone())
            }
        } else {
            definitions.push(p.definition())
        }
    }
    if !found {
        return Err(missing());
    }
    if id.is_none() {
        if let Some(v) = value {
            definitions.push(v.clone())
        }
    }
    Ok(definitions)
}
async fn save(
    db: Arc<Database>,
    m: MediaDomain,
    id: Option<i64>,
    revision: i64,
    value: Option<Definition>,
) -> Result<Json<Catalog>> {
    bounded(async {
        let c = db.connect().await?;
        let before = catalog(&c, m).await?;
        if before.revision != revision {
            return Err(conflict());
        }
        let definitions = proposed(&before, id, value.as_ref())?;
        validate_catalog(m, &definitions)?;
        // Real bounded preparation outside the writer; no permissive placeholder validation.
        let prepared = crate::release_profile_terms::prepare_terms(m, terms(&definitions))
            .await
            .map_err(matcher_error)?;
        let tx = c
            .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
            .await?;
        let result = async {
            let current = catalog(&tx, m).await?;
            if current.revision != revision {
                return Err(conflict());
            }
            let definitions = proposed(&current, id, value.as_ref())?;
            crate::release_profile_terms::validate_prepared(&prepared, m, &terms(&definitions))
                .map_err(matcher_error)?;
            match value {
                Some(v) => {
                    store(&tx, m, id, v, false).await?;
                }
                None => {
                    let id = id.ok_or_else(invalid)?;
                    tx.execute(
                        "DELETE FROM release_profiles WHERE id=? AND media_type=?",
                        params![id, domain(m)],
                    )
                    .await?;
                }
            }
            bump(&tx, m, revision, true).await?;
            catalog(&tx, m).await
        }
        .await;
        match result {
            Ok(all) => {
                tx.commit().await?;
                Ok(Json(all))
            }
            Err(error) => {
                tx.rollback().await?;
                Err(error)
            }
        }
    })
    .await
}
async fn create(
    State(db): State<Arc<Database>>,
    Extension(m): Extension<MediaDomain>,
    Query(q): Query<BTreeMap<String, String>>,
    v: std::result::Result<Json<Write<serde_json::Value>>, JsonRejection>,
) -> Result<(StatusCode, Json<Catalog>)> {
    no_query(&q)?;
    let v = body(v)?;
    Ok((
        StatusCode::CREATED,
        save(db, m, None, v.revision, Some(input(m, v.profile)?)).await?,
    ))
}
async fn update(
    State(db): State<Arc<Database>>,
    Extension(m): Extension<MediaDomain>,
    Path(id): Path<i64>,
    Query(q): Query<BTreeMap<String, String>>,
    v: std::result::Result<Json<Write<serde_json::Value>>, JsonRejection>,
) -> Result<Json<Catalog>> {
    no_query(&q)?;
    let v = body(v)?;
    save(db, m, Some(id), v.revision, Some(input(m, v.profile)?)).await
}
async fn delete(
    State(db): State<Arc<Database>>,
    Extension(m): Extension<MediaDomain>,
    Path(id): Path<i64>,
    Query(q): Query<RevisionQuery>,
) -> Result<Json<Catalog>> {
    save(db, m, Some(id), q.revision, None).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn applicable_profiles_are_all_matching_and_unresolved_never_matches() {
        for media in [MediaDomain::Tv, MediaDomain::Movies] {
            struct Scratch(std::path::PathBuf);
            impl Drop for Scratch {
                fn drop(&mut self) {
                    std::fs::remove_dir_all(&self.0).unwrap();
                }
            }
            let scratch = Scratch(
                std::env::temp_dir().join(format!("release-applicable-{}", uuid::Uuid::new_v4())),
            );
            std::fs::create_dir(&scratch.0).unwrap();
            let db = crate::db::Database::open_local(scratch.0.join("db"))
                .await
                .unwrap();
            let c = db.connect().await.unwrap();
            let d = domain(media);
            if matches!(media, MediaDomain::Tv) {
                c.execute(
                    "INSERT INTO series(id,title,path)VALUES(1,'TV','/synthetic')",
                    (),
                )
                .await
                .unwrap();
            } else {
                c.execute(
                    "INSERT INTO movie_metadata(id,tmdb_id,title)VALUES(1,1,'Movie')",
                    (),
                )
                .await
                .unwrap();
                c.execute(
                    "INSERT INTO movies(id,metadata_id,path)VALUES(1,1,'/synthetic')",
                    (),
                )
                .await
                .unwrap();
            }
            c.execute(
                "INSERT INTO tags(id,media_type,label)VALUES(1,?,'one'),(2,?,'two')",
                params![d, d],
            )
            .await
            .unwrap();
            let joins = if matches!(media, MediaDomain::Tv) {
                "series_tags(series_id,tag_id,media_type)"
            } else {
                "movie_tags(movie_id,tag_id,media_type)"
            };
            c.execute(&format!("INSERT INTO {joins}VALUES(1,1,?)"), [d])
                .await
                .unwrap();
            let indexer = uuid::Uuid::new_v4();
            c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint)VALUES(?,'torznab','Indexer',0,1,1,1,'http://fixture.invalid')",[indexer.to_string()]).await.unwrap();
            c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year)VALUES(?,'torznab',?,'[5000]','[]',?,?)",params![indexer.to_string(),d,matches!(media,MediaDomain::Tv).then_some(0),matches!(media,MediaDomain::Movies).then_some(0)]).await.unwrap();
            let mut base = Definition::default_for(media);
            base.required = vec!["WEB".into()];
            let mut definitions = vec![base.clone(), base.clone(), base.clone(), base.clone()];
            definitions[1].tag_ids = vec![1];
            definitions[1].indexers = vec![IndexerReference::Provider { id: indexer }];
            definitions[2].tag_ids = vec![2];
            definitions[3].enabled = false;
            definitions[3].indexers = vec![IndexerReference::UnresolvedSource {
                application: if matches!(media, MediaDomain::Tv) {
                    SourceApplication::Sonarr
                } else {
                    SourceApplication::Radarr
                },
                fingerprint: "a".repeat(64),
                source_id: 7,
            }];
            if let Some(tv) = base.tv.as_mut() {
                tv.excluded_tag_ids = vec![1];
                definitions.push(base);
            }
            let tx = c
                .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
                .await
                .unwrap();
            for definition in definitions {
                store(&tx, media, None, definition, true).await.unwrap();
            }
            tx.commit().await.unwrap();
            assert_eq!(
                applicable(&c, media, 1, indexer)
                    .await
                    .unwrap()
                    .profiles
                    .len(),
                2
            );
            assert_eq!(
                applicable(&c, media, 1, uuid::Uuid::new_v4())
                    .await
                    .unwrap()
                    .profiles
                    .len(),
                1
            );
            // Query failure must stay transient, not become an empty set or permanent invariant.
            c.execute(
                if matches!(media, MediaDomain::Tv) {
                    "DROP TABLE series_tags"
                } else {
                    "DROP TABLE movie_tags"
                },
                (),
            )
            .await
            .unwrap();
            assert_eq!(
                applicable(&c, media, 1, indexer).await.unwrap_err().code(),
                "release_profile_storage_error"
            );
        }
    }
    #[test]
    fn aggregate_profile_limits_count_whole_catalog() {
        let mut p = Definition::default_for(MediaDomain::Movies);
        p.required = vec!["x".repeat(2048); 200];
        assert!(validate_catalog(MediaDomain::Movies, &[p.clone(), p.clone()]).is_ok());
        assert!(validate_catalog(MediaDomain::Movies, &[p.clone(), p.clone(), p]).is_err());
        let mut p = Definition::default_for(MediaDomain::Movies);
        p.required = vec!["x".into(); 200];
        assert!(validate_catalog(MediaDomain::Movies, &vec![p.clone(); 20]).is_ok());
        assert!(validate_catalog(MediaDomain::Movies, &vec![p; 21]).is_err());
    }
}
