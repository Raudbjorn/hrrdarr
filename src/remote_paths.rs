//! Explicit lexical path translation; results confer no authority to access media.
use crate::{
    api::{ApiErrorEnvelope, ApiPage, MediaDomain},
    db::Database,
};
use axum::{
    Extension, Json, Router,
    extract::{
        DefaultBodyLimit, Path, Query, State,
        rejection::{JsonRejection, QueryRejection},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use libsql::{Connection, params};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
#[derive(Debug)]
pub(crate) struct Error(StatusCode, &'static str);
type Result<T> = std::result::Result<T, Error>;
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (self.0, Json(ApiErrorEnvelope::new(self.1, self.1))).into_response()
    }
}
impl From<libsql::Error> for Error {
    fn from(_: libsql::Error) -> Self {
        Self(StatusCode::INTERNAL_SERVER_ERROR, "mapping_storage_error")
    }
}
fn bad() -> Error {
    Error(StatusCode::BAD_REQUEST, "invalid_mapping_request")
}
fn conflict() -> Error {
    Error(StatusCode::CONFLICT, "mapping_conflict")
}
#[derive(Clone, Deserialize, Serialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct MappingInput {
    pub host: String,
    pub remote_path: String,
    pub local_path: String,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct MappingUpdate {
    pub revision: i64,
    #[serde(flatten)]
    pub config: MappingInput,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct MappingRevision {
    pub revision: i64,
}
#[derive(Clone, Serialize, ts_rs::TS)]
pub struct Mapping {
    pub id: i64,
    pub media_type: MediaDomain,
    pub host: String,
    pub remote_path: String,
    pub local_path: String,
    pub revision: i64,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct MappingQuery {
    #[serde(default = "limit")]
    #[ts(as = "Option<u16>", optional)]
    pub limit: u16,
    #[serde(default)]
    #[ts(as = "Option<u32>", optional)]
    pub offset: u32,
}
fn limit() -> u16 {
    50
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Clone, Copy, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    RemoteToLocal,
    LocalToRemote,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ResolveInput {
    pub host: String,
    pub path: String,
    pub direction: Direction,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ProviderPathInput {
    pub media_type: MediaDomain,
    pub remote_path: String,
}
#[derive(Serialize, ts_rs::TS)]
pub struct ProviderPathPreview {
    pub provider_id: uuid::Uuid,
    pub provider_revision: i64,
    pub resolution: Resolution,
}
#[derive(Serialize, ts_rs::TS)]
pub struct Resolution {
    pub input: String,
    pub output: String,
    pub mapping_id: Option<i64>,
    pub mapping_revision: Option<i64>,
    pub lexical_only: bool,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Posix,
    Windows,
}
struct Parsed {
    kind: Kind,
    prefix: String,
    parts: Vec<String>,
}
impl Parsed {
    fn render(&self) -> String {
        let separator = if self.kind == Kind::Windows {
            "\\"
        } else {
            "/"
        };
        if self.prefix.is_empty() && self.kind == Kind::Windows && self.parts.len() == 1 {
            return format!("{}\\", self.parts[0]);
        }
        if self.parts.is_empty() {
            self.prefix.clone()
        } else {
            format!("{}{}", self.prefix, self.parts.join(separator))
        }
    }
    fn suffix<'a>(&self, other: &'a Self) -> Option<&'a [String]> {
        // Source containment requires both paths rooted; relative configurations
        // can only be translation outputs in the reverse direction.
        if self.prefix.is_empty() || other.prefix.is_empty() {
            return None;
        }
        let eq = |a: &str, b: &str| {
            if self.kind == Kind::Windows {
                a.to_lowercase() == b.to_lowercase()
            } else {
                a == b
            }
        };
        (self.kind == other.kind
            && eq(&self.prefix, &other.prefix)
            && self.parts.len() <= other.parts.len()
            && self.parts.iter().zip(&other.parts).all(|(a, b)| eq(a, b)))
        .then(|| &other.parts[self.parts.len()..])
    }
}
fn parse(raw: &str) -> Result<Parsed> {
    if raw.is_empty() || raw.len() > 4096 || raw.trim() != raw || raw.chars().any(char::is_control)
    {
        return Err(bad());
    }
    let windows = (!raw.starts_with('/') && raw.contains('\\'))
        || raw.starts_with("\\\\")
        || raw.starts_with("//")
        || (raw.as_bytes()[0].is_ascii_alphabetic() && raw.as_bytes().get(1) == Some(&b':'));
    let value = if windows {
        raw.replace('\\', "/")
    } else {
        if raw.contains('\\') {
            return Err(bad());
        }
        raw.into()
    };
    let (kind, prefix, tail) = if windows {
        if value.starts_with("//") {
            let mut pieces = value[2..].split('/').filter(|s| !s.is_empty());
            let server = pieces.next().ok_or_else(bad)?;
            let share = pieces.next().ok_or_else(bad)?;
            if [server, share]
                .iter()
                .any(|s| matches!(*s, "." | ".." | "?") || s.contains(':'))
            {
                return Err(bad());
            }
            (
                Kind::Windows,
                format!("\\\\{server}\\{share}\\"),
                pieces.collect::<Vec<_>>().join("/"),
            )
        } else if value.as_bytes().get(1) == Some(&b':') {
            if value.len() < 3
                || !value.as_bytes()[0].is_ascii_alphabetic()
                || value.as_bytes()[2] != b'/'
            {
                return Err(bad());
            }
            (
                Kind::Windows,
                format!("{}:\\", value[..1].to_ascii_uppercase()),
                value[3..].into(),
            )
        } else {
            if value.starts_with('/') {
                return Err(bad());
            }
            (Kind::Windows, String::new(), value)
        }
    } else {
        if let Some(tail) = value.strip_prefix('/') {
            (Kind::Posix, "/".into(), tail.into())
        } else {
            if value.contains(':') {
                return Err(bad());
            }
            (Kind::Posix, String::new(), value)
        }
    };
    let parts = tail
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if parts
        .iter()
        .any(|s| matches!(s.as_str(), "." | "..") || (kind == Kind::Windows && s.contains(':')))
    {
        return Err(bad());
    }
    Ok(Parsed {
        kind,
        prefix,
        parts,
    })
}
pub(crate) fn host(raw: &str) -> std::result::Result<String, &'static str> {
    if raw.is_empty()
        || raw.len() > 253
        || raw.trim() != raw
        || raw.contains(['/', '\\', '@', '?', '#'])
        || raw.chars().any(char::is_control)
    {
        return Err("invalid mapping host");
    }
    let value = if raw.starts_with('[') && raw.ends_with(']') {
        let inner = &raw[1..raw.len() - 1];
        if inner.parse::<std::net::Ipv6Addr>().is_err() {
            return Err("invalid mapping host");
        }
        inner
    } else {
        raw
    };
    if let Ok(ip) = value.parse::<std::net::IpAddr>() {
        return Ok(ip.to_string());
    }
    if raw.contains([':', '[', ']']) {
        return Err("invalid mapping host");
    }
    match url::Host::parse(raw) {
        Ok(url::Host::Domain(name)) if name.len() <= 253 => Ok(name.to_ascii_lowercase()),
        Ok(url::Host::Domain(_)) => Err("invalid mapping host"),
        Ok(other) => Ok(other.to_string()),
        Err(_) => Err("invalid mapping host"),
    }
}
pub(crate) fn normalize(
    input: MappingInput,
) -> std::result::Result<(MappingInput, String, String), &'static str> {
    let host = host(&input.host)?;
    let remote = parse(&input.remote_path).map_err(|_| "invalid remote mapping path")?;
    let local_path =
        crate::library::normalized_path(&input.local_path).ok_or("invalid local mapping path")?;
    if ["/bin", "/boot", "/lib", "/sbin", "/proc", "/usr/bin"]
        .iter()
        .any(|p| std::path::Path::new(&local_path).starts_with(p))
    {
        return Err("local mapping targets a system directory");
    }
    let remote_path = remote.render();
    if remote_path.len() > 4096 {
        return Err("invalid remote mapping path");
    }
    let kind = if remote.kind == Kind::Windows {
        "windows"
    } else {
        "posix"
    };
    let key = if remote.kind == Kind::Windows {
        remote_path.to_lowercase()
    } else {
        remote_path.clone()
    };
    Ok((
        MappingInput {
            host,
            remote_path,
            local_path,
        },
        kind.into(),
        key,
    ))
}
fn name(media: MediaDomain) -> &'static str {
    if media == MediaDomain::Tv {
        "tv"
    } else {
        "movies"
    }
}
fn id(raw: &str) -> Result<i64> {
    raw.parse()
        .ok()
        .filter(|v| (1..=9007199254740991).contains(v))
        .ok_or_else(bad)
}
fn bounded<T: Serialize>(v: T) -> Result<Json<T>> {
    if serde_json::to_vec(&v).map_err(|_| bad())?.len() > 1024 * 1024 {
        return Err(Error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "mapping_response_limit",
        ));
    }
    Ok(Json(v))
}
fn row(row: libsql::Row, media: MediaDomain) -> Result<Mapping> {
    let mapping = Mapping {
        id: row.get(0)?,
        media_type: media,
        host: row.get(1)?,
        remote_path: row.get(2)?,
        local_path: row.get(3)?,
        revision: row.get(4)?,
    };
    let (normalized, kind, key) = normalize(MappingInput {
        host: mapping.host.clone(),
        remote_path: mapping.remote_path.clone(),
        local_path: mapping.local_path.clone(),
    })
    .map_err(|_| Error(StatusCode::INTERNAL_SERVER_ERROR, "invalid_stored_mapping"))?;
    if normalized.host != mapping.host
        || normalized.remote_path != mapping.remote_path
        || normalized.local_path != mapping.local_path
        || kind != row.get::<String>(5)?
        || key != row.get::<String>(6)?
    {
        return Err(Error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "invalid_stored_mapping",
        ));
    }
    Ok(mapping)
}
async fn read(c: &Connection, media: MediaDomain, key: i64) -> Result<Mapping> {
    row(c.query("SELECT id,host,remote_path,local_path,revision,remote_kind,remote_key FROM remote_path_mappings WHERE media_type=? AND id=?",params![name(media),key]).await?.next().await?.ok_or(Error(StatusCode::NOT_FOUND,"mapping_not_found"))?,media)
}
pub(crate) async fn resolve(
    c: &Connection,
    media: MediaDomain,
    input: ResolveInput,
) -> Result<Resolution> {
    let hostname = host(&input.host).map_err(|_| bad())?;
    if input.path.len() > 4096 {
        return Err(bad());
    }
    let mut result = Resolution {
        input: input.path.clone(),
        output: input.path.clone(),
        mapping_id: None,
        mapping_revision: None,
        lexical_only: true,
    };
    if input.path.is_empty() {
        return Ok(result);
    }
    let local_input;
    let target = if matches!(input.direction, Direction::LocalToRemote) {
        // Root is a valid lookup input even though it cannot be a configured directory.
        local_input = if input.path.bytes().all(|b| b == b'/') {
            "/".to_owned()
        } else {
            crate::library::normalized_path(&input.path).ok_or_else(bad)?
        };
        parse(&local_input)?
    } else {
        parse(&input.path)?
    };
    if matches!(input.direction, Direction::LocalToRemote) && target.kind != Kind::Posix {
        return Err(bad());
    }
    let mut rows=c.query("SELECT id,host,remote_path,local_path,revision,remote_kind,remote_key FROM remote_path_mappings WHERE media_type=? AND host=? ORDER BY id LIMIT 1001",params![name(media),hostname]).await?;
    let mut candidates = Vec::new();
    while let Some(r) = rows.next().await? {
        if candidates.len() == 1000 {
            return Err(Error(
                StatusCode::SERVICE_UNAVAILABLE,
                "mapping_resolve_limit",
            ));
        }
        candidates.push(row(r, media)?);
    }
    for mapping in candidates {
        let from = parse(match input.direction {
            Direction::RemoteToLocal => &mapping.remote_path,
            Direction::LocalToRemote => &mapping.local_path,
        })?;
        if let Some(suffix) = from.suffix(&target) {
            let mut to = parse(match input.direction {
                Direction::RemoteToLocal => &mapping.local_path,
                Direction::LocalToRemote => &mapping.remote_path,
            })?;
            to.parts.extend_from_slice(suffix);
            let output = to.render();
            parse(&output)?;
            if matches!(input.direction, Direction::RemoteToLocal)
                && crate::library::normalized_path(&output).is_none()
            {
                return Err(bad());
            }
            result.output = output;
            result.mapping_id = Some(mapping.id);
            result.mapping_revision = Some(mapping.revision);
            break;
        }
    }
    Ok(result)
}
pub fn router(db: Arc<Database>) -> Router {
    let mut router = Router::new();
    for media in [MediaDomain::Tv, MediaDomain::Movies] {
        let p = format!("/api/v1/{}/remote-path-mappings", name(media));
        router = router.merge(
            Router::new()
                .route(&p, get(list).post(create))
                .route(&format!("{p}/resolve"), post(preview))
                .route(
                    &format!("{p}/{{id}}"),
                    get(detail).put(update).delete(delete),
                )
                .layer(Extension(media))
                .layer(DefaultBodyLimit::max(16384))
                .with_state(db.clone()),
        );
    }
    router
}
async fn connection(db: &Database) -> Result<Connection> {
    db.connect()
        .await
        .map_err(|_| Error(StatusCode::INTERNAL_SERVER_ERROR, "mapping_storage_error"))
}
async fn list(
    State(db): State<Arc<Database>>,
    Extension(media): Extension<MediaDomain>,
    q: std::result::Result<Query<MappingQuery>, QueryRejection>,
) -> Result<Json<ApiPage<Mapping>>> {
    let q = q.map_err(|_| bad())?.0;
    if !(1..=100).contains(&q.limit) {
        return Err(bad());
    }
    let c = connection(&db).await?;
    let tx = c.transaction().await?;
    let total = tx
        .query(
            "SELECT count(*) FROM remote_path_mappings WHERE media_type=?",
            [name(media)],
        )
        .await?
        .next()
        .await?
        .ok_or_else(bad)?
        .get(0)?;
    let mut rows=tx.query("SELECT id,host,remote_path,local_path,revision,remote_kind,remote_key FROM remote_path_mappings WHERE media_type=? ORDER BY id LIMIT ? OFFSET ?",params![name(media),i64::from(q.limit),i64::from(q.offset)]).await?;
    let mut items = Vec::new();
    while let Some(r) = rows.next().await? {
        items.push(row(r, media)?)
    }
    drop(rows);
    tx.commit().await?;
    bounded(ApiPage {
        items,
        total,
        limit: q.limit,
        offset: q.offset,
    })
}
async fn detail(
    State(db): State<Arc<Database>>,
    Extension(media): Extension<MediaDomain>,
    Path(key): Path<String>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
) -> Result<Json<Mapping>> {
    q.map_err(|_| bad())?;
    bounded(read(&connection(&db).await?, media, id(&key)?).await?)
}
async fn write(
    c: &Connection,
    media: MediaDomain,
    input: MappingInput,
    old: Option<(i64, i64)>,
) -> Result<Mapping> {
    let (input, kind, key) = normalize(input).map_err(|_| bad())?;
    let tx = c
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    let result=async{
  if tx.query("SELECT id FROM remote_path_mappings WHERE media_type=? AND host=? AND remote_key=? AND id!=?",params![name(media),input.host.clone(),key.clone(),old.map_or(0,|v|v.0)]).await?.next().await?.is_some(){return Err(conflict())}
  let id=if let Some((id,revision))=old{
   if revision<1||revision>=9007199254740991{return Err(bad())}
   if tx.execute("UPDATE remote_path_mappings SET host=?,remote_path=?,remote_kind=?,remote_key=?,local_path=?,revision=revision+1 WHERE id=? AND media_type=? AND revision=?",params![input.host,input.remote_path,kind,key,input.local_path,id,name(media),revision]).await?!=1{return Err(conflict())}id
  }else{tx.execute("INSERT INTO remote_path_mappings(media_type,host,remote_path,remote_kind,remote_key,local_path) VALUES(?,?,?,?,?,?)",params![name(media),input.host,input.remote_path,kind,key,input.local_path]).await?;tx.last_insert_rowid()};
  read(&tx,media,id).await
 }.await;
    match result {
        Ok(value) => {
            tx.commit().await?;
            Ok(value)
        }
        Err(error) => {
            tx.rollback().await?;
            Err(error)
        }
    }
}
async fn create(
    State(db): State<Arc<Database>>,
    Extension(media): Extension<MediaDomain>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
    input: std::result::Result<Json<MappingInput>, JsonRejection>,
) -> Result<(StatusCode, Json<Mapping>)> {
    q.map_err(|_| bad())?;
    let (input, _, _) = normalize(input.map_err(|_| bad())?.0).map_err(|_| bad())?;
    if !crate::root_folders::existing_directory(input.local_path.clone()).await {
        return Err(Error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "mapping_local_directory_unavailable",
        ));
    }
    Ok((
        StatusCode::CREATED,
        bounded(write(&connection(&db).await?, media, input, None).await?)?,
    ))
}
async fn update(
    State(db): State<Arc<Database>>,
    Extension(media): Extension<MediaDomain>,
    Path(key): Path<String>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
    input: std::result::Result<Json<MappingUpdate>, JsonRejection>,
) -> Result<Json<Mapping>> {
    q.map_err(|_| bad())?;
    let update = input.map_err(|_| bad())?.0;
    let (input, _, _) = normalize(update.config).map_err(|_| bad())?;
    if !crate::root_folders::existing_directory(input.local_path.clone()).await {
        return Err(Error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "mapping_local_directory_unavailable",
        ));
    }
    bounded(
        write(
            &connection(&db).await?,
            media,
            input,
            Some((id(&key)?, update.revision)),
        )
        .await?,
    )
}
async fn delete(
    State(db): State<Arc<Database>>,
    Extension(media): Extension<MediaDomain>,
    Path(key): Path<String>,
    q: std::result::Result<Query<MappingRevision>, QueryRejection>,
) -> Result<StatusCode> {
    let revision = q.map_err(|_| bad())?.0.revision;
    if !(1..=9007199254740991).contains(&revision) {
        return Err(bad());
    }
    if connection(&db)
        .await?
        .execute(
            "DELETE FROM remote_path_mappings WHERE id=? AND media_type=? AND revision=?",
            params![id(&key)?, name(media), revision],
        )
        .await?
        != 1
    {
        return Err(conflict());
    }
    Ok(StatusCode::NO_CONTENT)
}
async fn preview(
    State(db): State<Arc<Database>>,
    Extension(media): Extension<MediaDomain>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
    input: std::result::Result<Json<ResolveInput>, JsonRejection>,
) -> Result<Json<Resolution>> {
    q.map_err(|_| bad())?;
    bounded(resolve(&connection(&db).await?, media, input.map_err(|_| bad())?.0).await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn foreign_path_boundaries_case_and_host_canonicalization() {
        for (base, path, expected) in [
            ("/storage", "/storage/movie/file.mkv", true),
            ("/storage", "/storageabc/file", false),
            ("/Storage", "/storage/file", false),
            (r"C:\Ä\İ", r"c:/ä/i̇/Movie/file.mkv", true),
            (
                r"\\Server\Share\Folder",
                r"//server/share/folder/File",
                true,
            ),
        ] {
            let b = parse(base).unwrap();
            let p = parse(path).unwrap();
            assert_eq!(b.suffix(&p).is_some(), expected, "{base} {path}");
        }
        assert_eq!(parse("/:folder/file").unwrap().render(), "/:folder/file");
        assert_eq!(host("[::1]").unwrap(), "::1");
        assert_eq!(host("CLIENT.Example").unwrap(), "client.example");
        for h in [
            "[[::1]]",
            "[::1",
            "::1]",
            "http://host",
            "host:8080",
            "host ",
        ] {
            assert!(host(h).is_err(), "{h}")
        }
        for path in [
            "../file",
            "C:file",
            r"\\?\C:\file",
            r"\\server:bad\share\file",
            r"\\server\share:bad\file",
            "/data/../file",
            "/data/./file",
            r"C:\data\file:stream",
            " /data",
        ] {
            assert!(parse(path).is_err(), "{path}")
        }
        for local in [
            "/",
            "/bin",
            "/bin/child",
            "/boot/a",
            "/lib/a",
            "/sbin/a",
            "/proc/a",
            "/usr/bin/a",
        ] {
            assert!(
                normalize(MappingInput {
                    host: "client".into(),
                    remote_path: "/remote".into(),
                    local_path: local.into()
                })
                .is_err()
            )
        }
        assert!(
            normalize(MappingInput {
                host: "client".into(),
                remote_path: "/remote".into(),
                local_path: "/binary/library".into()
            })
            .is_ok()
        );
    }
}

/// Preserve source precedence without reordering or overwriting destination configuration.
pub(crate) async fn snapshot_order(
    c: &Connection,
    media: MediaDomain,
    expected: &[(i64, MappingInput)],
) -> std::result::Result<bool, &'static str> {
    let Some((_, first)) = expected.first() else {
        return Ok(true);
    };
    let parsed = expected
        .iter()
        .map(|(id, m)| {
            Ok((
                *id,
                parse(&m.remote_path).map_err(|_| "invalid mapping order path")?,
                parse(&m.local_path).map_err(|_| "invalid mapping order path")?,
            ))
        })
        .collect::<std::result::Result<Vec<_>, &'static str>>()?;
    let overlaps = |a: &Parsed, b: &Parsed| a.suffix(b).is_some() || b.suffix(a).is_some();
    // ponytail: pairwise checks stay simple under the 1000-row snapshot cap;
    // replace with a prefix index only if the supported import cap grows.
    for (position, (id, remote, local)) in parsed.iter().enumerate() {
        for (later_id, later_remote, later_local) in &parsed[position + 1..] {
            if id > later_id && (overlaps(remote, later_remote) || overlaps(local, later_local)) {
                return Ok(false);
            }
        }
    }
    let mut rows=c.query("SELECT id,host,remote_path,local_path,revision,remote_kind,remote_key FROM remote_path_mappings WHERE media_type=? AND host=? ORDER BY id LIMIT 1001",params![name(media),first.host.clone()]).await.map_err(|_|"mapping precedence query failed")?;
    let expected_ids = expected
        .iter()
        .map(|(id, _)| *id)
        .collect::<std::collections::HashSet<_>>();
    let mut count = 0;
    while let Some(r) = rows
        .next()
        .await
        .map_err(|_| "mapping precedence query failed")?
    {
        count += 1;
        if count > 1000 {
            return Err("mapping precedence limit exceeded");
        }
        let m = row(r, media).map_err(|_| "invalid stored mapping")?;
        if expected_ids.contains(&m.id) {
            continue;
        }
        let remote = parse(&m.remote_path).map_err(|_| "invalid stored mapping")?;
        let local = parse(&m.local_path).map_err(|_| "invalid stored mapping")?;
        if parsed
            .iter()
            .any(|(_, r, l)| overlaps(r, &remote) || overlaps(l, &local))
        {
            return Ok(false);
        }
    }
    Ok(true)
}
