//! Movie collections (api.036). Catalog membership is adopted from movie metadata and the
//! RefreshCollections command; the per-collection policy decides whether missing members are added
//! to the library. There is no TV counterpart. Search-on-add is stored and counted but this module
//! does not itself submit searches.
use crate::{
    api::{ApiErrorEnvelope, ApiPage, MediaDomain},
    db::Database,
    metadata::{CollectionAssociation, CollectionDetails},
};
use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, Path, Query, State,
        rejection::{JsonRejection, PathRejection, QueryRejection},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use libsql::{Connection, params};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, sync::Arc};

const MAX_ID: i64 = 9_007_199_254_740_991;
const DEFAULT_LIMIT: u16 = 50;
const MAX_LIMIT: u16 = 200;
const MAX_BULK: usize = 200;
const MAX_TAGS: usize = 64;
const MAX_FOLDER_NAME: usize = 200;

#[derive(Debug)]
pub struct Error(pub StatusCode, pub &'static str, pub &'static str);
pub type Result<T> = std::result::Result<T, Error>;
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (self.0, Json(ApiErrorEnvelope::new(self.1, self.2))).into_response()
    }
}
impl From<libsql::Error> for Error {
    fn from(_: libsql::Error) -> Self {
        Error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database_error",
            "Collection operation failed; no partial write committed",
        )
    }
}
fn bad(message: &'static str) -> Error {
    Error(StatusCode::BAD_REQUEST, "invalid_request", message)
}
fn missing() -> Error {
    Error(
        StatusCode::NOT_FOUND,
        "collection_not_found",
        "Collection does not exist",
    )
}

#[derive(Clone, Debug, Serialize, ts_rs::TS)]
#[ts(rename = "CollectionMovie")]
pub struct Member {
    pub metadata_id: i64,
    pub tmdb_id: Option<i64>,
    pub title: String,
    pub year: Option<i64>,
    /// Library movie id when the movie is in the library; null for catalog-only members.
    pub movie_id: Option<i64>,
    pub monitored: Option<bool>,
}
#[derive(Clone, Debug, Serialize, ts_rs::TS)]
#[ts(rename = "MovieCollection")]
pub struct Collection {
    pub id: i64,
    pub tmdb_id: Option<i64>,
    pub title: String,
    pub overview: Option<String>,
    pub monitored: bool,
    pub search_on_add: bool,
    pub quality_profile_id: Option<i64>,
    pub root_folder_id: Option<i64>,
    pub root_folder_path: Option<String>,
    pub minimum_availability: Option<String>,
    pub tag_ids: Vec<i64>,
    pub last_info_sync: Option<String>,
    pub movie_count: i64,
    pub library_movie_count: i64,
    pub missing_movie_count: i64,
    /// Members are listed by the detail endpoint only; list responses carry an empty array.
    pub movies: Vec<Member>,
}

fn nullable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> std::result::Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}
#[derive(Clone, Debug, Default, Deserialize, Serialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "CollectionPatch")]
pub struct Patch {
    #[serde(default)]
    #[ts(optional)]
    pub monitored: Option<bool>,
    #[serde(default)]
    #[ts(optional)]
    pub search_on_add: Option<bool>,
    #[serde(default, deserialize_with = "nullable")]
    #[ts(as = "Option<i64>", optional = nullable)]
    pub quality_profile_id: Option<Option<i64>>,
    #[serde(default, deserialize_with = "nullable")]
    #[ts(as = "Option<i64>", optional = nullable)]
    pub root_folder_id: Option<Option<i64>>,
    #[serde(default, deserialize_with = "nullable")]
    #[ts(as = "Option<crate::library::Availability>", optional = nullable)]
    pub minimum_availability: Option<Option<crate::library::Availability>>,
    /// Replaces the complete tag set applied to movies this collection adds.
    #[serde(default)]
    #[ts(optional)]
    pub tag_ids: Option<Vec<i64>>,
    /// One-shot bulk action: set monitoring on movies of this collection already in the library.
    #[serde(default)]
    #[ts(optional)]
    pub monitor_movies: Option<bool>,
}
#[derive(Debug, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "CollectionBulkPatch")]
pub struct BulkPatch {
    pub ids: Vec<i64>,
    pub patch: Patch,
}
#[derive(Debug, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "CollectionQuery", optional_fields)]
pub struct ListQuery {
    limit: Option<u16>,
    offset: Option<u32>,
    monitored: Option<bool>,
    q: Option<String>,
}

pub fn router(db: Arc<Database>) -> Router {
    Router::new()
        .route("/api/v1/movies/collections", get(list).put(bulk))
        .route("/api/v1/movies/collections/{id}", get(detail).put(update))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .with_state(db)
}

fn positive(id: i64) -> Result<i64> {
    if (1..=MAX_ID).contains(&id) {
        Ok(id)
    } else {
        Err(bad("Identifiers must be positive integers"))
    }
}

const SELECT: &str = "SELECT c.id,c.tmdb_id,c.title,c.overview,c.monitored,c.search_on_add,c.quality_profile_id,c.root_folder_id,r.path,c.minimum_availability,c.last_info_sync,(SELECT count(*) FROM movie_collection_members m WHERE m.collection_id=c.id),(SELECT count(*) FROM movie_collection_members m JOIN movies mv ON mv.metadata_id=m.metadata_id WHERE m.collection_id=c.id) FROM movie_collections c LEFT JOIN root_folders r ON r.id=c.root_folder_id";

async fn decode(c: &Connection, row: &libsql::Row, with_members: bool) -> Result<Collection> {
    let id: i64 = row.get(0)?;
    let total: i64 = row.get(11)?;
    let library: i64 = row.get(12)?;
    let mut tag_ids = vec![];
    let mut rows = c
        .query(
            "SELECT tag_id FROM movie_collection_tags WHERE collection_id=? ORDER BY tag_id LIMIT 65",
            [id],
        )
        .await?;
    while let Some(r) = rows.next().await? {
        tag_ids.push(r.get(0)?);
    }
    drop(rows);
    let mut movies = vec![];
    if with_members {
        let mut rows = c
            .query(
                "SELECT d.id,d.tmdb_id,d.title,d.year,mv.id,mv.monitored FROM movie_collection_members m JOIN movie_metadata d ON d.id=m.metadata_id LEFT JOIN movies mv ON mv.metadata_id=d.id WHERE m.collection_id=? ORDER BY d.year IS NULL,d.year,d.title,d.id LIMIT 1001",
                [id],
            )
            .await?;
        while let Some(r) = rows.next().await? {
            movies.push(Member {
                metadata_id: r.get(0)?,
                tmdb_id: r.get(1)?,
                title: r.get(2)?,
                year: r.get(3)?,
                movie_id: r.get(4)?,
                monitored: r.get::<Option<i64>>(5)?.map(|v| v != 0),
            });
        }
    }
    Ok(Collection {
        id,
        tmdb_id: row.get(1)?,
        title: row.get(2)?,
        overview: row.get(3)?,
        monitored: row.get::<i64>(4)? != 0,
        search_on_add: row.get::<i64>(5)? != 0,
        quality_profile_id: row.get(6)?,
        root_folder_id: row.get(7)?,
        root_folder_path: row.get(8)?,
        minimum_availability: row.get(9)?,
        tag_ids,
        last_info_sync: row.get(10)?,
        movie_count: total,
        library_movie_count: library,
        missing_movie_count: total - library,
        movies,
    })
}
async fn read(c: &Connection, id: i64, with_members: bool) -> Result<Collection> {
    let row = c
        .query(&format!("{SELECT} WHERE c.id=?"), [id])
        .await?
        .next()
        .await?
        .ok_or_else(missing)?;
    decode(c, &row, with_members).await
}

async fn list(
    State(db): State<Arc<Database>>,
    query: std::result::Result<Query<ListQuery>, QueryRejection>,
) -> Result<Json<ApiPage<Collection>>> {
    let q = query.map_err(|_| bad("Invalid collection query"))?.0;
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT);
    let offset = q.offset.unwrap_or(0);
    if limit == 0 || limit > MAX_LIMIT {
        return Err(bad("Limit must be 1 to 200"));
    }
    let needle = q.q.as_deref().map(str::trim).filter(|s| !s.is_empty());
    if needle.is_some_and(|s| s.len() > 128) {
        return Err(bad("Search text is limited to 128 bytes"));
    }
    let pattern = needle.map(|s| {
        format!(
            "%{}%",
            s.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        )
    });
    let monitored = q.monitored.map(i64::from);
    let c = db.connect().await?;
    let filter =
        "WHERE (?1 IS NULL OR c.monitored=?1) AND (?2 IS NULL OR c.title LIKE ?2 ESCAPE '\\')";
    let total = c
        .query(
            &format!("SELECT count(*) FROM movie_collections c {filter}"),
            params![monitored, pattern.clone()],
        )
        .await?
        .next()
        .await?
        .ok_or_else(missing)?
        .get::<i64>(0)?;
    let mut rows = c
        .query(
            &format!("{SELECT} {filter} ORDER BY c.title COLLATE NOCASE,c.id LIMIT ?3 OFFSET ?4"),
            params![monitored, pattern, i64::from(limit), i64::from(offset)],
        )
        .await?;
    let mut items = vec![];
    while let Some(row) = rows.next().await? {
        items.push(decode(&c, &row, false).await?);
    }
    Ok(Json(ApiPage {
        items,
        total,
        limit,
        offset,
    }))
}
async fn detail(
    State(db): State<Arc<Database>>,
    id: std::result::Result<Path<i64>, PathRejection>,
) -> Result<Json<Collection>> {
    let id = positive(id.map_err(|_| bad("Invalid collection id"))?.0)?;
    Ok(Json(read(&db.connect().await?, id, true).await?))
}
async fn update(
    State(db): State<Arc<Database>>,
    id: std::result::Result<Path<i64>, PathRejection>,
    body: std::result::Result<Json<Patch>, JsonRejection>,
) -> Result<Json<Collection>> {
    let id = positive(id.map_err(|_| bad("Invalid collection id"))?.0)?;
    let patch = body
        .map_err(|_| bad("Expected a collection patch object"))?
        .0;
    let c = db.connect().await?;
    let tx = c
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        apply_patch(&tx, id, &patch).await?;
        read(&tx, id, true).await
    }
    .await;
    finish(tx, outcome).await.map(Json)
}
async fn bulk(
    State(db): State<Arc<Database>>,
    body: std::result::Result<Json<BulkPatch>, JsonRejection>,
) -> Result<Json<Vec<Collection>>> {
    let req = body.map_err(|_| bad("Expected a bulk collection patch"))?.0;
    let ids: BTreeSet<i64> = req.ids.iter().copied().collect();
    if ids.is_empty() || ids.len() != req.ids.len() || ids.len() > MAX_BULK {
        return Err(bad("Choose 1 to 200 distinct collection ids"));
    }
    let c = db.connect().await?;
    let tx = c
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        let mut out = vec![];
        for id in &ids {
            positive(*id)?;
            apply_patch(&tx, *id, &req.patch).await?;
            out.push(read(&tx, *id, false).await?);
        }
        Ok(out)
    }
    .await;
    finish(tx, outcome).await.map(Json)
}
async fn finish<T>(tx: libsql::Transaction, r: Result<T>) -> Result<T> {
    match r {
        Ok(v) => {
            tx.commit().await?;
            Ok(v)
        }
        Err(e) => {
            tx.rollback().await?;
            Err(e)
        }
    }
}

/// Validates and writes one patch inside the caller's transaction.
async fn apply_patch(tx: &Connection, id: i64, p: &Patch) -> Result<()> {
    read(tx, id, false).await?;
    // A native policy edit is sticky: a later snapshot import never overwrites it (migration 0069).
    if p.monitored.is_some()
        || p.search_on_add.is_some()
        || p.quality_profile_id.is_some()
        || p.root_folder_id.is_some()
        || p.minimum_availability.is_some()
        || p.tag_ids.is_some()
    {
        tx.execute("UPDATE movie_collections SET local_edit=1 WHERE id=?", [id])
            .await?;
    }
    if let Some(Some(profile)) = p.quality_profile_id {
        if tx
            .query(
                "SELECT 1 FROM quality_profiles WHERE id=? AND media_type='movies'",
                [positive(profile)?],
            )
            .await?
            .next()
            .await?
            .is_none()
        {
            return Err(bad("Quality profile does not exist in the movies domain"));
        }
    }
    if let Some(Some(root)) = p.root_folder_id {
        if tx
            .query(
                "SELECT 1 FROM root_folders WHERE id=? AND media_type='movies'",
                [positive(root)?],
            )
            .await?
            .next()
            .await?
            .is_none()
        {
            return Err(bad("Root folder does not exist in the movies domain"));
        }
    }
    // Unmonitor first so that clearing a policy field and monitoring stay consistent.
    if p.monitored == Some(false) {
        tx.execute("UPDATE movie_collections SET monitored=0 WHERE id=?", [id])
            .await?;
    }
    if let Some(v) = p.search_on_add {
        tx.execute(
            "UPDATE movie_collections SET search_on_add=? WHERE id=?",
            params![i64::from(v), id],
        )
        .await?;
    }
    if let Some(v) = p.quality_profile_id {
        tx.execute(
            "UPDATE movie_collections SET quality_profile_id=? WHERE id=?",
            params![v, id],
        )
        .await?;
    }
    if let Some(v) = p.root_folder_id {
        tx.execute(
            "UPDATE movie_collections SET root_folder_id=? WHERE id=?",
            params![v, id],
        )
        .await?;
    }
    if let Some(v) = &p.minimum_availability {
        let text = v.as_ref().map(|a| {
            serde_json::to_value(a)
                .ok()
                .and_then(|j| j.as_str().map(str::to_owned))
                .unwrap_or_default()
        });
        tx.execute(
            "UPDATE movie_collections SET minimum_availability=? WHERE id=?",
            params![text, id],
        )
        .await?;
    }
    if let Some(tags) = &p.tag_ids {
        let set: BTreeSet<i64> = tags.iter().copied().collect();
        if set.len() != tags.len() || set.len() > MAX_TAGS {
            return Err(bad("Tags require at most 64 distinct ids"));
        }
        for tag in &set {
            if tx
                .query(
                    "SELECT 1 FROM tags WHERE id=? AND media_type='movies'",
                    [positive(*tag)?],
                )
                .await?
                .next()
                .await?
                .is_none()
            {
                return Err(bad("Tag does not exist in the movies domain"));
            }
        }
        tx.execute(
            "DELETE FROM movie_collection_tags WHERE collection_id=?",
            [id],
        )
        .await?;
        for tag in set {
            tx.execute(
                "INSERT INTO movie_collection_tags(collection_id,tag_id)VALUES(?,?)",
                params![id, tag],
            )
            .await?;
        }
    }
    if p.monitored == Some(true) {
        let ready = tx
            .query(
                "SELECT quality_profile_id IS NOT NULL AND root_folder_id IS NOT NULL AND minimum_availability IS NOT NULL FROM movie_collections WHERE id=?",
                [id],
            )
            .await?
            .next()
            .await?
            .ok_or_else(missing)?
            .get::<i64>(0)?;
        if ready != 1 {
            return Err(Error(
                StatusCode::CONFLICT,
                "collection_policy_incomplete",
                "Monitoring a collection requires a quality profile, root folder and minimum availability",
            ));
        }
        tx.execute("UPDATE movie_collections SET monitored=1 WHERE id=?", [id])
            .await?;
    }
    if let Some(v) = p.monitor_movies {
        tx.execute(
            "UPDATE movies SET monitored=? WHERE metadata_id IN (SELECT metadata_id FROM movie_collection_members WHERE collection_id=?)",
            params![i64::from(v), id],
        )
        .await?;
    }
    let consistent = tx
        .query(
            "SELECT monitored=0 OR (quality_profile_id IS NOT NULL AND root_folder_id IS NOT NULL AND minimum_availability IS NOT NULL) FROM movie_collections WHERE id=?",
            [id],
        )
        .await?
        .next()
        .await?
        .ok_or_else(missing)?
        .get::<i64>(0)?;
    if consistent != 1 {
        return Err(Error(
            StatusCode::CONFLICT,
            "collection_policy_incomplete",
            "A monitored collection requires a quality profile, root folder and minimum availability",
        ));
    }
    // A monitored collection with complete policy adopts its already-known missing members now.
    if p.monitored == Some(true) {
        add_missing(tx, id).await?;
    }
    Ok(())
}

// User removals survive rediscovery; lifecycle removals may be restored by fresh metadata.
async fn restore_discovered_collection(c: &Connection, tmdb_id: i64) -> libsql::Result<bool> {
    if c.query("SELECT 1 FROM movie_collection_intents WHERE tmdb_id=? AND removed=1 AND removal_reason='user'", [tmdb_id]).await?.next().await?.is_some() {
        return Ok(false);
    }
    c.execute("UPDATE movie_collection_intents SET removed=0,removal_reason=NULL,revision=revision+1 WHERE tmdb_id=? AND removed=1 AND removal_reason='metadata_missing'", [tmdb_id]).await?;
    Ok(true)
}

/// Stores movie -> collection membership from metadata. Absent preserves, Clear removes.
/// A movie belongs to at most one collection. Caller owns the transaction.
pub(crate) async fn associate(
    c: &Connection,
    metadata_id: i64,
    association: &CollectionAssociation,
) -> std::result::Result<bool, libsql::Error> {
    match association {
        CollectionAssociation::Absent => Ok(false),
        CollectionAssociation::Clear => Ok(c
            .execute(
                "DELETE FROM movie_collection_members WHERE metadata_id=?",
                [metadata_id],
            )
            .await?
            > 0),
        CollectionAssociation::Present(reference) => {
            if !restore_discovered_collection(c, reference.tmdb_id).await? {
                return Ok(c.execute("DELETE FROM movie_collection_members WHERE metadata_id=?1 AND collection_id IN (SELECT id FROM movie_collections WHERE tmdb_id IS NOT NULL AND tmdb_id<>?2)", params![metadata_id, reference.tmdb_id]).await? > 0);
            }
            c.execute(
                "INSERT INTO movie_collections(tmdb_id,title,added)VALUES(?1,?2,strftime('%Y-%m-%dT%H:%M:%SZ','now')) ON CONFLICT(tmdb_id) DO UPDATE SET title=excluded.title,metadata_revision=metadata_revision+1 WHERE title IS NOT excluded.title",
                params![reference.tmdb_id, reference.title.clone()],
            )
            .await?;
            let id: i64 = c
                .query(
                    "SELECT id FROM movie_collections WHERE tmdb_id=?",
                    [reference.tmdb_id],
                )
                .await?
                .next()
                .await?
                .ok_or(libsql::Error::QueryReturnedNoRows)?
                .get(0)?;
            let removed = c
                .execute(
                    "DELETE FROM movie_collection_members WHERE metadata_id=? AND collection_id<>?",
                    params![metadata_id, id],
                )
                .await?;
            let added = c
                .execute(
                    "INSERT OR IGNORE INTO movie_collection_members(collection_id,metadata_id,origin)VALUES(?,?,'discovery')",
                    params![id, metadata_id],
                )
                .await?;
            Ok(removed > 0 || added > 0)
        }
    }
}

fn folder_name(title: &str, year: Option<i64>) -> String {
    let mut cleaned: String = title
        .chars()
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) {
                ' '
            } else {
                c
            }
        })
        .collect();
    cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut cleaned = cleaned;
    loop {
        let next = cleaned.trim_matches('.').trim().to_owned();
        if next == cleaned {
            break;
        }
        cleaned = next;
    }
    if cleaned.is_empty() {
        cleaned = "Untitled".into();
    }
    let suffix = year.map(|y| format!(" ({y})")).unwrap_or_default();
    let mut end = MAX_FOLDER_NAME
        .saturating_sub(suffix.len())
        .min(cleaned.len());
    while !cleaned.is_char_boundary(end) {
        end -= 1;
    }
    cleaned.truncate(end);
    format!("{}{suffix}", cleaned.trim_end())
}

/// Adds catalog-only members of a monitored, fully configured collection to the library.
/// Returns (movies added, searches left pending). Existing library movies and path collisions
/// are skipped, never replaced.
pub(crate) async fn add_missing(c: &Connection, id: i64) -> Result<(i64, i64)> {
    let Some(row) = c
        .query(
            "SELECT c.quality_profile_id,r.path,c.minimum_availability,c.search_on_add FROM movie_collections c JOIN root_folders r ON r.id=c.root_folder_id AND r.media_type='movies' WHERE c.id=? AND c.monitored=1 AND c.quality_profile_id IS NOT NULL AND c.minimum_availability IS NOT NULL AND NOT EXISTS(SELECT 1 FROM movie_collection_intents i WHERE i.tmdb_id=c.tmdb_id AND i.removed=1)",
            [id],
        )
        .await?
        .next()
        .await?
    else {
        return Ok((0, 0));
    };
    let profile: i64 = row.get(0)?;
    let root: String = row.get(1)?;
    let availability: String = row.get(2)?;
    let search = row.get::<i64>(3)? != 0;
    drop(row);
    let mut tag_ids = vec![];
    let mut rows = c
        .query(
            "SELECT tag_id FROM movie_collection_tags WHERE collection_id=? ORDER BY tag_id",
            [id],
        )
        .await?;
    while let Some(r) = rows.next().await? {
        tag_ids.push(r.get::<i64>(0)?);
    }
    drop(rows);
    let mut candidates = vec![];
    let mut rows = c
        .query(
            "SELECT d.id,d.title,d.year FROM movie_collection_members m JOIN movie_metadata d ON d.id=m.metadata_id WHERE m.collection_id=? AND NOT EXISTS(SELECT 1 FROM movies mv WHERE mv.metadata_id=d.id) ORDER BY d.id LIMIT 1000",
            [id],
        )
        .await?;
    while let Some(r) = rows.next().await? {
        candidates.push((
            r.get::<i64>(0)?,
            r.get::<String>(1)?,
            r.get::<Option<i64>>(2)?,
        ));
    }
    drop(rows);
    let mut added = 0;
    for (metadata_id, title, year) in candidates {
        let path = format!("{root}/{}", folder_name(&title, year));
        let clash = c
            .query(
                "SELECT 1 FROM (SELECT rtrim(path,'/') AS path FROM series UNION ALL SELECT rtrim(path,'/') AS path FROM movies) WHERE path=?1 OR substr(path,1,length(?1)+1)=?1||'/' OR substr(?1,1,length(path)+1)=path||'/' LIMIT 1",
                [path.clone()],
            )
            .await?
            .next()
            .await?
            .is_some();
        if clash {
            continue;
        }
        c.execute(
            "INSERT INTO movies(metadata_id,path,monitored)VALUES(?,?,1)",
            params![metadata_id, path],
        )
        .await?;
        let movie_id = c.last_insert_rowid();
        c.execute(
            "INSERT INTO library_settings(media_type,movie_id,quality_profile_id,minimum_availability,added)VALUES('movies',?,?,?,strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
            params![movie_id, profile, availability.clone()],
        )
        .await?;
        let tags = crate::tags::Assignment {
            mode: crate::tags::Mode::Replace,
            ids: tag_ids.clone(),
        };
        crate::tags::assign(c, MediaDomain::Movies, movie_id, &tags, false)
            .await
            .map_err(|e| Error(e.0, e.1, e.2))?;
        crate::autotagging::apply_item(c, MediaDomain::Movies, movie_id)
            .await
            .map_err(|e| Error(e.0, e.1, e.2))?;
        added += 1;
    }
    Ok((added, if search { added } else { 0 }))
}

/// Applies a fetched collection graph. Returns (movies added, searches pending).
pub(crate) async fn apply_graph(
    c: &Connection,
    id: i64,
    graph: &CollectionDetails,
) -> Result<(i64, i64)> {
    let tmdb: Option<i64> = c
        .query("SELECT tmdb_id FROM movie_collections WHERE id=?", [id])
        .await?
        .next()
        .await?
        .ok_or_else(missing)?
        .get(0)?;
    if tmdb != Some(graph.tmdb_id) {
        return Err(Error(
            StatusCode::CONFLICT,
            "collection_changed",
            "Collection identity changed during refresh",
        ));
    }
    if !restore_discovered_collection(c, graph.tmdb_id).await? {
        return Ok((0, 0));
    }
    c.execute(
        "UPDATE movie_collections SET title=?,overview=?,last_info_sync=strftime('%Y-%m-%dT%H:%M:%SZ','now'),metadata_revision=metadata_revision+1 WHERE id=?",
        params![graph.title.clone(), graph.overview.clone(), id],
    )
    .await?;
    let mut keep = BTreeSet::new();
    for part in &graph.parts {
        let existing = c
            .query(
                "SELECT id FROM movie_metadata WHERE tmdb_id=?",
                [part.tmdb_id],
            )
            .await?
            .next()
            .await?
            .map(|r| r.get::<i64>(0))
            .transpose()?;
        let metadata_id = if let Some(existing) = existing {
            existing
        } else {
            // An IMDb id already claimed by a different catalog row is dropped, never reassigned.
            let imdb = match &part.imdb_id {
                Some(imdb)
                    if c.query(
                        "SELECT 1 FROM movie_metadata WHERE imdb_id=?",
                        [imdb.clone()],
                    )
                    .await?
                    .next()
                    .await?
                    .is_none() =>
                {
                    Some(imdb.clone())
                }
                _ => None,
            };
            c.execute(
                "INSERT INTO movie_metadata(tmdb_id,imdb_id,title,year)VALUES(?,?,?,?)",
                params![part.tmdb_id, imdb, part.title.clone(), part.year],
            )
            .await?;
            c.last_insert_rowid()
        };
        keep.insert(metadata_id);
        c.execute(
            "DELETE FROM movie_collection_members WHERE metadata_id=? AND collection_id<>?",
            params![metadata_id, id],
        )
        .await?;
        c.execute(
            "INSERT INTO movie_collection_members(collection_id,metadata_id,origin)VALUES(?,?,'graph') ON CONFLICT(collection_id,metadata_id) DO UPDATE SET origin='graph'",
            params![id, metadata_id],
        )
        .await?;
    }
    // The complete graph is authoritative: members it no longer lists leave the collection.
    let mut stale = vec![];
    let mut rows = c
        .query(
            "SELECT metadata_id FROM movie_collection_members WHERE collection_id=?",
            [id],
        )
        .await?;
    while let Some(r) = rows.next().await? {
        let m: i64 = r.get(0)?;
        if !keep.contains(&m) {
            stale.push(m);
        }
    }
    drop(rows);
    for m in stale {
        c.execute(
            "DELETE FROM movie_collection_members WHERE collection_id=? AND metadata_id=?",
            params![id, m],
        )
        .await?;
    }
    add_missing(c, id).await
}

/// Collections with a metadata identity after `cursor`, in id order.
pub(crate) async fn refreshable(
    c: &Connection,
    cursor: i64,
    limit: i64,
) -> Result<Vec<(i64, i64)>> {
    let mut rows = c
        .query(
            "SELECT id,tmdb_id FROM movie_collections c WHERE tmdb_id IS NOT NULL AND id>? AND NOT EXISTS(SELECT 1 FROM movie_collection_intents i WHERE i.tmdb_id=c.tmdb_id AND i.removed=1 AND i.removal_reason='user') ORDER BY id LIMIT ?",
            params![cursor, limit],
        )
        .await?;
    let mut out = vec![];
    while let Some(r) = rows.next().await? {
        out.push((r.get(0)?, r.get(1)?));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::folder_name;
    #[test]
    fn folder_names_are_portable_and_bounded() {
        assert_eq!(
            folder_name("Alien: Resurrection", Some(1997)),
            "Alien Resurrection (1997)"
        );
        assert_eq!(folder_name("../../etc", None), "etc");
        assert_eq!(folder_name("???", Some(2000)), "Untitled (2000)");
        assert!(folder_name(&"é".repeat(500), Some(1999)).len() <= 200 + 7);
    }
}
