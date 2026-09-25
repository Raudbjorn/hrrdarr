//! Native, domain-scoped file metadata. No endpoint here changes the filesystem.
pub(crate) mod media_info;
use crate::db::Database;
use axum::{
    Extension, Json, Router,
    extract::{
        DefaultBodyLimit, Path, Query, State,
        rejection::{JsonRejection, QueryRejection},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, put},
};
use libsql::{Connection, Value, params};
use serde::Deserialize;
use serde_json::Value as JsonValue;
use std::{collections::BTreeSet, sync::Arc};
const MAX_IDS: usize = 200;
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
// Queries alias the file table as f. Unattached snapshot files remain active;
// only a journaled replacement retirement removes a file from library views.
pub(crate) fn active_file_sql(media: &str) -> &'static str {
    if media == "tv" {
        "NOT EXISTS(SELECT 1 FROM rss_candidate_imports ri WHERE ri.old_episode_file_id=f.id AND (ri.retirement_state='quarantined' OR EXISTS(SELECT 1 FROM same_path_replacements sp JOIN import_journal sj ON sj.operation_id=sp.operation_id WHERE sp.operation_id=ri.operation_id AND sp.state='installed' AND sj.phase IN ('committed','complete'))))"
    } else {
        "1"
    }
}
#[derive(Debug)]
struct Error(StatusCode, &'static str, &'static str);
type Result<T> = std::result::Result<T, Error>;
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (
            self.0,
            Json(crate::api::ApiErrorEnvelope::new(self.1, self.2)),
        )
            .into_response()
    }
}
impl From<libsql::Error> for Error {
    fn from(_: libsql::Error) -> Self {
        Self(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database_error",
            "File metadata operation failed; no partial update committed",
        )
    }
}
fn bad(message: &'static str) -> Error {
    Error(StatusCode::BAD_REQUEST, "invalid_request", message)
}
fn missing() -> Error {
    Error(
        StatusCode::NOT_FOUND,
        "file_not_found",
        "One or more files do not exist",
    )
}
#[derive(Clone, Copy)]
struct Domain {
    name: &'static str,
    table: &'static str,
    owner: &'static str,
    target: &'static str,
    root: &'static str,
}
fn domain(s: &str) -> Result<Domain> {
    match s {
        "tv" => Ok(Domain {
            name: "tv",
            table: "episode_files",
            owner: "series_id",
            target: "episode_file_id",
            root: "series",
        }),
        "movies" => Ok(Domain {
            name: "movies",
            table: "movie_files",
            owner: "movie_id",
            target: "movie_file_id",
            root: "movies",
        }),
        _ => Err(bad("Media domain must be tv or movies")),
    }
}
pub fn router(db: Arc<Database>) -> Router {
    let mut router = Router::new();
    for media in ["tv", "movies"] {
        let prefix = format!("/api/v1/{media}/files");
        router = router.merge(
            Router::new()
                .route(&format!("{prefix}"), get(list))
                .route(&format!("{prefix}/bulk"), put(bulk))
                .route(&format!("{prefix}/editor"), put(editor))
                .route(&format!("{prefix}/{{id}}"), get(detail).put(update))
                .layer(Extension(media.to_owned()))
                .layer(DefaultBodyLimit::max(256 * 1024))
                .with_state(db.clone()),
        );
    }
    // Keep documented JSON errors for unsupported domains; real domains use static prefixes.
    router
        .route("/api/v1/{media}/files", get(unknown_domain))
        .route("/api/v1/{media}/files/bulk", put(unknown_domain))
        .route("/api/v1/{media}/files/editor", put(unknown_domain))
        .route(
            "/api/v1/{media}/files/{id}",
            get(unknown_domain).put(unknown_domain),
        )
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "FileQuery", optional_fields)]
pub struct Select {
    series_id: Option<i64>,
    movie_ids: Option<String>,
    file_ids: Option<String>,
    offset: Option<u32>,
    limit: Option<u16>,
}
fn ids(values: Vec<i64>) -> Result<Vec<i64>> {
    if values.is_empty()
        || values.len() > MAX_IDS
        || values.iter().any(|v| *v <= 0)
        || values.iter().copied().collect::<BTreeSet<_>>().len() != values.len()
    {
        return Err(bad("Expected 1 to 200 distinct positive ids"));
    }
    Ok(values)
}
fn csv(s: &str) -> Result<Vec<i64>> {
    if s.len() > 4200 || s.split(',').count() > MAX_IDS {
        return Err(bad("Too many file selector ids"));
    }
    ids(s
        .split(',')
        .map(|s| s.parse().map_err(|_| bad("Invalid id list")))
        .collect::<Result<_>>()?)
}
fn placeholders(n: usize) -> String {
    vec!["?"; n].join(",")
}
async fn list(
    State(db): State<Arc<Database>>,
    Extension(media): Extension<String>,
    q: std::result::Result<Query<Select>, QueryRejection>,
) -> Result<Json<crate::api::ApiPage<FileResource>>> {
    let d = domain(&media)?;
    let q = q.map_err(|_| bad("Invalid file query parameters"))?.0;
    let selectors = usize::from(q.series_id.is_some())
        + usize::from(q.movie_ids.is_some())
        + usize::from(q.file_ids.is_some());
    if selectors != 1
        || (d.name == "tv" && q.movie_ids.is_some())
        || (d.name == "movies" && q.series_id.is_some())
    {
        return Err(bad(
            "Choose exactly one domain-appropriate owner or file selector",
        ));
    }
    let (column, selected) = if let Some(s) = q.file_ids {
        ("id", csv(&s)?)
    } else if let Some(s) = q.movie_ids {
        (d.owner, csv(&s)?)
    } else {
        (d.owner, ids(vec![q.series_id.unwrap()])?)
    };
    let offset = q.offset.unwrap_or(0);
    let limit = q.limit.unwrap_or(100);
    if limit == 0 || limit > 500 {
        return Err(bad("Limit must be 1 to 500"));
    }
    let filter = format!("f.{column} IN ({})", placeholders(selected.len()));
    let values: Vec<Value> = selected.into_iter().map(Value::Integer).collect();
    let c = db.connect().await?;
    let tx = c.transaction().await?;
    let mut rows = tx
        .query(
            &format!(
                "SELECT count(*) FROM {} f WHERE {filter} AND {}",
                d.table,
                active_file_sql(d.name)
            ),
            values.clone(),
        )
        .await?;
    let total = rows.next().await?.ok_or_else(missing)?.get::<i64>(0)?;
    drop(rows);
    let records = fetch(&tx, d, &filter, values, i64::from(limit), i64::from(offset)).await?;
    tx.commit().await?;
    Ok(Json(crate::api::ApiPage {
        items: records,
        total,
        limit,
        offset,
    }))
}
async fn detail(
    State(db): State<Arc<Database>>,
    Extension(media): Extension<String>,
    Path(id): Path<String>,
) -> Result<Json<FileResource>> {
    let d = domain(&media)?;
    let id = parse_id(&id)?;
    let c = db.connect().await?;
    let mut rows = fetch(&c, d, "f.id=?", vec![Value::Integer(id)], 1, 0).await?;
    Ok(Json(rows.pop().ok_or_else(missing)?))
}
fn parse_id(id: &str) -> Result<i64> {
    id.parse()
        .ok()
        .filter(|v| *v > 0)
        .ok_or_else(|| bad("File id must be positive"))
}
async fn fetch(
    c: &Connection,
    d: Domain,
    filter: &str,
    mut values: Vec<Value>,
    limit: i64,
    offset: i64,
) -> Result<Vec<FileResource>> {
    let edition = if d.name == "movies" {
        "f.edition"
    } else {
        "NULL"
    };
    let sql = format!(
        "SELECT f.id,f.{owner},f.path,p.path,{edition},m.quality_id,m.revision_json,m.languages_json,m.size,m.date_added,m.season_number,m.original_file_path,m.release_group,m.indexer_flags,m.release_type,m.media_info_json FROM {table} f JOIN {root} p ON p.id=f.{owner} LEFT JOIN file_metadata m ON m.{target}=f.id AND m.media_type=? WHERE {filter} AND {active} ORDER BY f.id LIMIT ? OFFSET ?",
        owner = d.owner,
        table = d.table,
        root = d.root,
        target = d.target,
        active = active_file_sql(d.name)
    );
    values.insert(0, Value::Text(d.name.into()));
    values.push(Value::Integer(limit));
    values.push(Value::Integer(offset));
    let mut rows = c.query(&sql, values).await?;
    let mut result = vec![];
    // Reserve framing for list/bulk envelopes and separators as well as escaped items.
    let mut bytes = 1024;
    while let Some(row) = rows.next().await? {
        let path = row.get::<String>(2)?;
        let root = row.get::<String>(3)?;
        let relative = path
            .strip_prefix(&format!("{}/", root.trim_end_matches('/')))
            .map(str::to_owned);
        let quality = row.get::<Option<i64>>(5)?;
        let revision = parse_stored(row.get::<Option<String>>(6)?)?;
        let languages = parse_stored(row.get::<Option<String>>(7)?)?;
        let core = FileCore {
            id: row.get(0)?,
            path,
            relative_path: relative,
            quality: quality.map(|quality_id| FileQuality {
                quality_id,
                revision,
            }),
            languages,
            size: row.get(8)?,
            date_added: row.get(9)?,
            release_group: row.get(12)?,
            indexer_flags: row.get(13)?,
            scene_name: (),
            media_info: media_info::public(row.get::<Option<String>>(15)?).map_err(|_| {
                Error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "database_error",
                    "Invalid stored media info",
                )
            })?,
            custom_formats: (),
            custom_format_score: (),
            quality_cutoff_not_met: (),
        };
        let item = if d.name == "tv" {
            FileResource::Tv(TvFileResource {
                core,
                series_id: row.get(1)?,
                season_number: row.get(10)?,
                release_type: row.get(14)?,
            })
        } else {
            FileResource::Movies(MovieFileResource {
                core,
                movie_id: row.get(1)?,
                edition: row.get(4)?,
                original_file_path: row.get(11)?,
            })
        };
        bytes += serde_json::to_vec(&item)
            .map_err(|_| bad("Invalid stored metadata"))?
            .len()
            + 1;
        if bytes > MAX_RESPONSE_BYTES {
            return Err(Error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "response_too_large",
                "Select a smaller page; file response exceeds 8 MiB",
            ));
        }
        result.push(item);
    }
    Ok(result)
}
fn parse_stored<T: serde::de::DeserializeOwned>(value: Option<String>) -> Result<Option<T>> {
    value
        .map(|v| {
            serde_json::from_str(&v).map_err(|_| {
                Error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "database_error",
                    "Invalid stored file metadata",
                )
            })
        })
        .transpose()
}
fn body<T>(value: std::result::Result<Json<T>, JsonRejection>) -> Result<T> {
    value.map(|j| j.0).map_err(|e| {
        if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
            Error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "body_too_large",
                "File request exceeds 256 KiB",
            )
        } else {
            bad("Expected documented JSON file update")
        }
    })
}
fn patch_json(p: FilePatch) -> Result<JsonValue> {
    serde_json::to_value(p).map_err(|_| bad("Invalid file patch"))
}
async fn update(
    State(db): State<Arc<Database>>,
    Extension(media): Extension<String>,
    Path(id): Path<String>,
    b: std::result::Result<Json<FilePatch>, JsonRejection>,
) -> Result<Json<FileResource>> {
    let d = domain(&media)?;
    let id = parse_id(&id)?;
    let patch = patch_json(body(b)?)?;
    let mut result = persist(&db, d, vec![(id, patch)]).await?;
    Ok(Json(result.remove(0)))
}
async fn bulk(
    State(db): State<Arc<Database>>,
    Extension(media): Extension<String>,
    b: std::result::Result<Json<FileBulk>, JsonRejection>,
) -> Result<Json<Vec<FileResource>>> {
    let d = domain(&media)?;
    let files = body(b)?.files;
    if files.is_empty() || files.len() > MAX_IDS {
        return Err(bad("Expected 1 to 200 files"));
    }
    let changes = files
        .into_iter()
        .map(|f| Ok((f.id, patch_json(f.patch)?)))
        .collect::<Result<_>>()?;
    Ok(Json(persist(&db, d, changes).await?))
}
async fn editor(
    State(db): State<Arc<Database>>,
    Extension(media): Extension<String>,
    b: std::result::Result<Json<FileEditor>, JsonRejection>,
) -> Result<Json<Vec<FileResource>>> {
    let d = domain(&media)?;
    let req = body(b)?;
    let patch = patch_json(req.patch)?;
    let changes = ids(req.file_ids)?
        .into_iter()
        .map(|id| (id, patch.clone()))
        .collect();
    Ok(Json(persist(&db, d, changes).await?))
}
// Omitted fields preserve values; explicit null clears them. Identity and factual scan fields are read-only.
async fn persist(
    db: &Database,
    d: Domain,
    changes: Vec<(i64, JsonValue)>,
) -> Result<Vec<FileResource>> {
    let selected = ids(changes.iter().map(|(id, _)| *id).collect())?;
    let mut validated = vec![];
    for (id, patch) in changes {
        validated.push((id, validate_patch(d, &patch)?));
    }
    let c = db.connect().await?;
    let tx = c
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    let outcome: Result<Vec<FileResource>> = async {
        for (id, patch) in &validated {
            let mut found = tx
                .query(
                    &format!(
                        "SELECT f.id FROM {} f WHERE f.id=? AND {}",
                        d.table,
                        active_file_sql(d.name)
                    ),
                    [*id],
                )
                .await?;
            if found.next().await?.is_none() {
                return Err(missing());
            }
            drop(found);
            tx.execute(
            &format!(
                "INSERT INTO file_metadata(media_type,{}) VALUES(?,?) ON CONFLICT({}) DO NOTHING",
                d.target, d.target
            ),
            params![d.name, *id],
        )
        .await?;
            for (column, value) in patch {
                if *column == "edition" {
                    tx.execute(
                        "UPDATE movie_files SET edition=? WHERE id=?",
                        vec![value.clone(), Value::Integer(*id)],
                    )
                    .await?;
                } else {
                    if *column == "quality_id" && *value != Value::Null {
                        let mut q = tx
                        .query(
                            "SELECT 1 FROM quality_definitions WHERE media_type=? AND quality_id=?",
                            vec![Value::Text(d.name.into()), value.clone()],
                        )
                        .await?;
                        if q.next().await?.is_none() {
                            return Err(bad("Quality id is not in this domain catalog"));
                        }
                    }
                    tx.execute(
                        &format!(
                            "UPDATE file_metadata SET {column}=? WHERE {}=? AND media_type=?",
                            d.target
                        ),
                        vec![
                            value.clone(),
                            Value::Integer(*id),
                            Value::Text(d.name.into()),
                        ],
                    )
                    .await?;
                }
            }
        }
        let filter = format!("f.id IN ({})", placeholders(selected.len()));
        let result = fetch(
            &tx,
            d,
            &filter,
            selected.into_iter().map(Value::Integer).collect(),
            MAX_IDS as i64,
            0,
        )
        .await?;
        Ok(result)
    }
    .await;
    match outcome {
        Ok(result) => {
            tx.commit().await?;
            Ok(result)
        }
        Err(error) => {
            tx.rollback().await?;
            Err(error)
        }
    }
}
fn validate_patch(d: Domain, patch: &JsonValue) -> Result<Vec<(&'static str, Value)>> {
    let fields = patch
        .as_object()
        .filter(|o| !o.is_empty())
        .ok_or_else(|| bad("Update requires at least one metadata field"))?;
    let mut out = vec![];
    for (key, value) in fields {
        match key.as_str() {
            "quality" => {
                if value.is_null() {
                    out.push(("quality_id", Value::Null));
                    out.push(("revision_json", Value::Null));
                } else {
                    let (id, revision) = quality(value).map_err(bad)?;
                    out.push(("quality_id", Value::Integer(id)));
                    out.push((
                        "revision_json",
                        revision.map(Value::Text).unwrap_or(Value::Null),
                    ));
                }
            }
            "languages" => out.push((
                "languages_json",
                languages(value, d.name)
                    .map_err(bad)?
                    .map(Value::Text)
                    .unwrap_or(Value::Null),
            )),
            "release_group" => out.push(("release_group", string(value, 1024).map_err(bad)?)),
            "edition" if d.name == "movies" => {
                out.push(("edition", string(value, 1024).map_err(bad)?))
            }
            "indexer_flags" => out.push((
                "indexer_flags",
                number(value, if d.name == "tv" { 511 } else { 4095 }).map_err(bad)?,
            )),
            "release_type" if d.name == "tv" => {
                out.push(("release_type", number(value, 3).map_err(bad)?))
            }
            "scene_name" => {
                return Err(Error(
                    StatusCode::BAD_REQUEST,
                    "unsupported_field",
                    "Scene-name edits require the release parser and are not available",
                ));
            }
            _ => return Err(bad("Unknown, immutable or wrong-domain file field")),
        }
    }
    Ok(out)
}
pub(crate) fn string(value: &JsonValue, max: usize) -> std::result::Result<Value, &'static str> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    value
        .as_str()
        .filter(|s| s.len() <= max && !s.contains('\0'))
        .map(|s| Value::Text(s.to_owned()))
        .ok_or("Invalid file metadata text")
}
pub(crate) fn number(value: &JsonValue, max: i64) -> std::result::Result<Value, &'static str> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    value
        .as_i64()
        .filter(|v| *v >= 0 && *v <= max)
        .map(Value::Integer)
        .ok_or("Invalid file metadata integer")
}
pub(crate) fn quality(
    value: &JsonValue,
) -> std::result::Result<(i64, Option<String>), &'static str> {
    let o = value.as_object().ok_or("Invalid quality object")?;
    if o.keys()
        .any(|k| !matches!(k.as_str(), "quality_id" | "revision"))
    {
        return Err("Unknown quality fields");
    }
    let id = o
        .get("quality_id")
        .and_then(JsonValue::as_i64)
        .filter(|v| *v >= 0)
        .ok_or("Invalid quality id")?;
    let revision = match o.get("revision") {
        None | Some(JsonValue::Null) => None,
        Some(v) => {
            let r = v.as_object().ok_or("Invalid quality revision")?;
            if r.len() != 3
                || r.get("version")
                    .and_then(JsonValue::as_i64)
                    .filter(|v| *v >= 1 && *v <= i32::MAX as i64)
                    .is_none()
                || r.get("real")
                    .and_then(JsonValue::as_i64)
                    .filter(|v| *v >= 0 && *v <= i32::MAX as i64)
                    .is_none()
                || r.get("is_repack").and_then(JsonValue::as_bool).is_none()
            {
                return Err("Invalid quality revision");
            }
            Some(v.to_string())
        }
    };
    Ok((id, revision))
}
pub(crate) fn languages(
    value: &JsonValue,
    media: &str,
) -> std::result::Result<Option<String>, &'static str> {
    if value.is_null() {
        return Ok(None);
    }
    let a = value
        .as_array()
        .filter(|a| a.len() <= 64)
        .ok_or("Invalid language ids")?;
    let max = if media == "tv" { 52 } else { 57 };
    let mut ids = BTreeSet::new();
    for v in a {
        let id = v
            .as_i64()
            .filter(|id| *id >= 0 && *id <= max)
            .ok_or("Language id must be a concrete language in this domain")?;
        if !ids.insert(id) {
            return Err("Duplicate language id");
        }
    }
    Ok(Some(value.to_string()))
}

#[derive(Debug, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct FileRevision {
    pub version: i64,
    pub real: i64,
    pub is_repack: bool,
}
#[derive(Debug, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields=nullable)]
pub struct FileQualityInput {
    pub quality_id: i64,
    pub revision: Option<FileRevision>,
}
#[derive(Debug, serde::Serialize, ts_rs::TS)]
pub struct FileQuality {
    pub quality_id: i64,
    pub revision: Option<FileRevision>,
}
#[derive(Debug, serde::Serialize, ts_rs::TS)]
pub struct FileCore {
    pub id: i64,
    pub path: String,
    pub relative_path: Option<String>,
    pub quality: Option<FileQuality>,
    pub languages: Option<Vec<i64>>,
    pub size: Option<i64>,
    pub date_added: Option<String>,
    pub release_group: Option<String>,
    pub indexer_flags: Option<i64>,
    pub scene_name: (),
    pub media_info: Option<media_info::MediaInfo>,
    pub custom_formats: (),
    pub custom_format_score: (),
    pub quality_cutoff_not_met: (),
}
#[derive(Debug, serde::Serialize, ts_rs::TS)]
pub struct TvFileResource {
    #[serde(flatten)]
    pub core: FileCore,
    pub series_id: i64,
    pub season_number: Option<i64>,
    pub release_type: Option<i64>,
}
#[derive(Debug, serde::Serialize, ts_rs::TS)]
pub struct MovieFileResource {
    #[serde(flatten)]
    pub core: FileCore,
    pub movie_id: i64,
    pub edition: Option<String>,
    pub original_file_path: Option<String>,
}
#[derive(Debug, serde::Serialize, ts_rs::TS)]
#[serde(untagged)]
pub enum FileResource {
    Tv(TvFileResource),
    Movies(MovieFileResource),
}

#[derive(Default, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct FilePatch {
    #[serde(
        default,
        deserialize_with = "crate::library::change",
        skip_serializing_if = "crate::library::Change::is_missing"
    )]
    #[ts(as="Option<FileQualityInput>",optional=nullable)]
    pub quality: crate::library::Change<FileQualityInput>,
    #[serde(
        default,
        deserialize_with = "crate::library::change",
        skip_serializing_if = "crate::library::Change::is_missing"
    )]
    #[ts(as="Option<Vec<i64>>",optional=nullable)]
    pub languages: crate::library::Change<Vec<i64>>,
    #[serde(
        default,
        deserialize_with = "crate::library::change",
        skip_serializing_if = "crate::library::Change::is_missing"
    )]
    #[ts(as="Option<String>",optional=nullable)]
    pub release_group: crate::library::Change<String>,
    #[serde(
        default,
        deserialize_with = "crate::library::change",
        skip_serializing_if = "crate::library::Change::is_missing"
    )]
    #[ts(as="Option<String>",optional=nullable)]
    pub edition: crate::library::Change<String>,
    #[serde(
        default,
        deserialize_with = "crate::library::change",
        skip_serializing_if = "crate::library::Change::is_missing"
    )]
    #[ts(as="Option<i64>",optional=nullable)]
    pub indexer_flags: crate::library::Change<i64>,
    #[serde(
        default,
        deserialize_with = "crate::library::change",
        skip_serializing_if = "crate::library::Change::is_missing"
    )]
    #[ts(as="Option<i64>",optional=nullable)]
    pub release_type: crate::library::Change<i64>,
    // This name is recognized solely to return the existing unsupported_field error.
    #[serde(
        default,
        deserialize_with = "crate::library::change",
        skip_serializing_if = "crate::library::Change::is_missing"
    )]
    #[ts(skip)]
    pub scene_name: crate::library::Change<JsonValue>,
}
#[derive(serde::Deserialize, serde::Serialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct FileUpdate {
    pub id: i64,
    #[serde(flatten)]
    pub patch: FilePatch,
}
#[derive(serde::Deserialize, serde::Serialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct FileBulk {
    pub files: Vec<FileUpdate>,
}
#[derive(serde::Deserialize, serde::Serialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct FileEditor {
    pub file_ids: Vec<i64>,
    #[serde(flatten)]
    pub patch: FilePatch,
}

async fn unknown_domain() -> Error {
    bad("Media domain must be tv or movies")
}
