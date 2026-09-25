//! Scoped root declarations and bounded, transient local filesystem observations.
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
    routing::get,
};
use libsql::params;
use rustix::fs::{self, Access, AtFlags, FileType, Mode, OFlags};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    path::Path as FsPath,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;
const BUDGET: Duration = Duration::from_secs(5);
const MAX_ENTRIES: usize = 10_000;
const MAX_RESPONSE: usize = 1024 * 1024;
// ponytail: two blocking observations process-wide; retain permits until actual workers finish.
static OBSERVERS: Semaphore = Semaphore::const_new(2);
#[derive(Debug)]
struct Error(StatusCode, &'static str);
type Result<T> = std::result::Result<T, Error>;
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (self.0, Json(ApiErrorEnvelope::new(self.1, self.1))).into_response()
    }
}
impl From<libsql::Error> for Error {
    fn from(_: libsql::Error) -> Self {
        Self(StatusCode::INTERNAL_SERVER_ERROR, "root_storage_error")
    }
}
fn bad() -> Error {
    Error(StatusCode::BAD_REQUEST, "invalid_root_request")
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct RootInput {
    pub path: String,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct RootQuery {
    #[serde(default = "default_limit")]
    #[ts(as = "Option<u16>", optional)]
    pub limit: u16,
    #[serde(default)]
    #[ts(as = "Option<u32>", optional)]
    pub offset: u32,
}
fn default_limit() -> u16 {
    50
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyQuery {}
#[derive(Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum RootObservationStatus {
    Available,
    Inaccessible,
    Timeout,
    Busy,
    Limited,
}
#[derive(Clone, Serialize, ts_rs::TS)]
pub struct UnmappedFolder {
    pub name: String,
    pub path: String,
    pub relative_path: String,
}
#[derive(Clone, Serialize, ts_rs::TS)]
pub struct RootFolder {
    pub id: i64,
    pub media_type: MediaDomain,
    pub path: String,
    pub observation: RootObservationStatus,
    pub accessible: Option<bool>,
    pub writable: Option<bool>,
    pub free_space: Option<u64>,
    pub total_space: Option<u64>,
    pub unmapped_folders: Option<Vec<UnmappedFolder>>,
}
impl RootFolder {
    fn unknown(
        id: i64,
        media_type: MediaDomain,
        path: String,
        status: RootObservationStatus,
    ) -> Self {
        Self {
            id,
            media_type,
            path,
            observation: status,
            accessible: None,
            writable: None,
            free_space: None,
            total_space: None,
            unmapped_folders: None,
        }
    }
    fn uncertain(&mut self, status: RootObservationStatus) {
        *self = Self::unknown(self.id, self.media_type, self.path.clone(), status);
    }
}
pub fn router(db: Arc<Database>) -> Router {
    let mut router = Router::new();
    for media in [MediaDomain::Tv, MediaDomain::Movies] {
        // Explicit prefixes coexist with the library's /tv/{id} and /movies/{id} routes.
        let prefix = format!("/api/v1/{}/root-folders", domain_name(media));
        router = router.merge(
            Router::new()
                .route(&prefix, get(list).post(create))
                .route(&format!("{prefix}/{{id}}"), get(detail).delete(delete))
                .layer(Extension(media))
                .layer(DefaultBodyLimit::max(8192))
                .with_state(db.clone()),
        );
    }
    router
}
fn domain_name(value: MediaDomain) -> &'static str {
    match value {
        MediaDomain::Tv => "tv",
        MediaDomain::Movies => "movies",
    }
}
fn id(value: &str) -> Result<i64> {
    value
        .parse::<i64>()
        .ok()
        .filter(|v| (1..=9007199254740991).contains(v))
        .ok_or_else(bad)
}
fn bounded<T: Serialize>(value: T) -> Result<Json<T>> {
    if serde_json::to_vec(&value).map_err(|_| bad())?.len() > MAX_RESPONSE {
        return Err(Error(StatusCode::PAYLOAD_TOO_LARGE, "root_response_limit"));
    }
    Ok(Json(value))
}
async fn mapped(db: &Database, media: MediaDomain) -> Result<HashSet<String>> {
    let c = db
        .connect()
        .await
        .map_err(|_| Error(StatusCode::INTERNAL_SERVER_ERROR, "root_storage_error"))?;
    let table = if media == MediaDomain::Tv {
        "series"
    } else {
        "movies"
    };
    let mut rows = c
        .query(&format!("SELECT path FROM {table} LIMIT 10001"), ())
        .await?;
    let mut paths = HashSet::new();
    let mut count = 0;
    while let Some(row) = rows.next().await? {
        if count >= MAX_ENTRIES {
            return Err(Error(StatusCode::SERVICE_UNAVAILABLE, "root_library_limit"));
        }
        count += 1;
        let path = row.get::<String>(0)?;
        paths.insert(crate::library::normalized_path(&path).unwrap_or(path));
    }
    Ok(paths)
}
fn observe_one(
    root: &mut RootFolder,
    mapped: &HashSet<String>,
    deadline: Instant,
    probe: bool,
) -> Result<()> {
    if Instant::now() >= deadline {
        root.uncertain(RootObservationStatus::Timeout);
        return Ok(());
    }
    let Some(fd) = crate::import::root_directory(FsPath::new(&root.path)) else {
        root.observation = RootObservationStatus::Inaccessible;
        root.accessible = Some(false);
        return Ok(());
    };
    root.accessible = Some(true);
    root.observation = RootObservationStatus::Available;
    root.writable = Some(
        fs::accessat(
            &fd,
            ".",
            Access::WRITE_OK | Access::EXEC_OK,
            AtFlags::EACCESS,
        )
        .is_ok(),
    );
    if probe {
        let name = format!(".hrrdarr-root-check-{}", uuid::Uuid::new_v4());
        let file = fs::openat(
            &fd,
            name.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        );
        match file {
            Ok(file) => {
                drop(file);
                fs::unlinkat(&fd, name.as_str(), AtFlags::empty()).map_err(|_| {
                    Error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "root_probe_cleanup_failed",
                    )
                })?;
                root.writable = Some(true);
            }
            Err(_) => {
                root.writable = Some(false);
                return Ok(());
            }
        }
    }
    if let Ok(stat) = fs::fstatvfs(&fd) {
        root.free_space = stat
            .f_bavail
            .checked_mul(stat.f_frsize)
            .filter(|v| *v <= 9007199254740991);
        root.total_space = stat
            .f_blocks
            .checked_mul(stat.f_frsize)
            .filter(|v| *v <= 9007199254740991);
    }
    let entries = fs::Dir::read_from(&fd)
        .map_err(|_| Error(StatusCode::SERVICE_UNAVAILABLE, "root_scan_failed"))?;
    let mut unmapped = Vec::new();
    let mut bytes = 0;
    for (count, entry) in entries.enumerate() {
        if count >= MAX_ENTRIES || bytes >= MAX_RESPONSE / 2 {
            root.observation = RootObservationStatus::Limited;
            return Ok(());
        }
        if Instant::now() >= deadline {
            root.uncertain(RootObservationStatus::Timeout);
            return Ok(());
        }
        let entry =
            entry.map_err(|_| Error(StatusCode::SERVICE_UNAVAILABLE, "root_scan_failed"))?;
        let name = entry
            .file_name()
            .to_str()
            .map_err(|_| Error(StatusCode::SERVICE_UNAVAILABLE, "root_non_utf8_name"))?;
        if name == "." || name == ".." {
            continue;
        }
        let lower = name.to_lowercase();
        if [
            "$recycle.bin",
            "system volume information",
            "recycler",
            "lost+found",
            ".appledb",
            ".appledesktop",
            ".appledouble",
            "@eadir",
            ".grab",
        ]
        .contains(&lower.as_str())
            || (root.media_type == MediaDomain::Movies && name.starts_with('.'))
        {
            continue;
        }
        // statat handles filesystems with unknown d_type without following child symlinks.
        let stat = fs::statat(&fd, entry.file_name(), AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| Error(StatusCode::SERVICE_UNAVAILABLE, "root_scan_changed"))?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
            continue;
        }
        let path = format!("{}/{name}", root.path);
        if !mapped.contains(&path) {
            bytes += path.len() + name.len() * 2 + 64;
            unmapped.push(UnmappedFolder {
                name: name.into(),
                path,
                relative_path: name.into(),
            });
        }
    }
    unmapped.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.name.cmp(&b.name))
    });
    root.unmapped_folders = Some(unmapped);
    Ok(())
}
async fn observe(
    mut roots: Vec<RootFolder>,
    mapped: HashSet<String>,
    probe: bool,
) -> Result<Vec<RootFolder>> {
    let Ok(permit) = OBSERVERS.try_acquire() else {
        for root in &mut roots {
            root.uncertain(RootObservationStatus::Busy)
        }
        return Ok(roots);
    };
    let fallback = roots.clone();
    let job = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let deadline = Instant::now() + BUDGET;
        for root in &mut roots {
            if let Err(error) = observe_one(root, &mapped, deadline, probe) {
                if probe {
                    return Err(error);
                }
                root.uncertain(RootObservationStatus::Inaccessible);
            }
        }
        Ok(roots)
    });
    match tokio::time::timeout(BUDGET, job).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err(Error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "root_worker_failed",
        )),
        Err(_) => Ok(fallback
            .into_iter()
            .map(|mut r| {
                r.uncertain(RootObservationStatus::Timeout);
                r
            })
            .collect()),
    }
}
async fn list(
    State(db): State<Arc<Database>>,
    Extension(media): Extension<MediaDomain>,
    query: std::result::Result<Query<RootQuery>, QueryRejection>,
) -> Result<Json<ApiPage<RootFolder>>> {
    let q = query.map_err(|_| bad())?.0;
    if !(1..=100).contains(&q.limit) {
        return Err(bad());
    }
    let c = db
        .connect()
        .await
        .map_err(|_| Error(StatusCode::INTERNAL_SERVER_ERROR, "root_storage_error"))?;
    let tx = c.transaction().await?;
    let total = tx
        .query(
            "SELECT count(*) FROM root_folders WHERE media_type=?",
            [domain_name(media)],
        )
        .await?
        .next()
        .await?
        .ok_or_else(bad)?
        .get(0)?;
    let mut rows = tx
        .query(
            "SELECT id,path FROM root_folders WHERE media_type=? ORDER BY id LIMIT ? OFFSET ?",
            params![domain_name(media), i64::from(q.limit), i64::from(q.offset)],
        )
        .await?;
    let mut roots = Vec::new();
    while let Some(row) = rows.next().await? {
        roots.push(RootFolder::unknown(
            row.get(0)?,
            media,
            row.get(1)?,
            RootObservationStatus::Busy,
        ));
    }
    drop(rows);
    tx.commit().await?;
    let roots = observe(roots, mapped(&db, media).await?, false).await?;
    bounded(ApiPage {
        items: roots,
        total,
        limit: q.limit,
        offset: q.offset,
    })
}
async fn read(db: &Database, media: MediaDomain, id: i64) -> Result<RootFolder> {
    let c = db
        .connect()
        .await
        .map_err(|_| Error(StatusCode::INTERNAL_SERVER_ERROR, "root_storage_error"))?;
    let row = c
        .query(
            "SELECT path FROM root_folders WHERE media_type=? AND id=?",
            params![domain_name(media), id],
        )
        .await?
        .next()
        .await?
        .ok_or(Error(StatusCode::NOT_FOUND, "root_not_found"))?;
    Ok(RootFolder::unknown(
        id,
        media,
        row.get(0)?,
        RootObservationStatus::Busy,
    ))
}
async fn detail(
    State(db): State<Arc<Database>>,
    Extension(media): Extension<MediaDomain>,
    Path(key): Path<String>,
    q: std::result::Result<Query<EmptyQuery>, QueryRejection>,
) -> Result<Json<RootFolder>> {
    q.map_err(|_| bad())?;
    let root = read(&db, media, id(&key)?).await?;
    let mut roots = observe(vec![root], mapped(&db, media).await?, false).await?;
    bounded(roots.remove(0))
}
async fn create(
    State(db): State<Arc<Database>>,
    Extension(media): Extension<MediaDomain>,
    q: std::result::Result<Query<EmptyQuery>, QueryRejection>,
    input: std::result::Result<Json<RootInput>, JsonRejection>,
) -> Result<(StatusCode, Json<RootFolder>)> {
    q.map_err(|_| bad())?;
    let input = input.map_err(|_| bad())?.0;
    let path = crate::library::normalized_path(&input.path).ok_or_else(bad)?;
    let mut roots = observe(
        vec![RootFolder::unknown(
            0,
            media,
            path.clone(),
            RootObservationStatus::Busy,
        )],
        mapped(&db, media).await?,
        true,
    )
    .await?;
    let mut root = roots.remove(0);
    if root.accessible != Some(true) || root.writable != Some(true) {
        return Err(Error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "root_unavailable_or_not_writable",
        ));
    }
    let c = db
        .connect()
        .await
        .map_err(|_| Error(StatusCode::INTERNAL_SERVER_ERROR, "root_storage_error"))?;
    let tx = c
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    let changed=tx.execute("INSERT INTO root_folders(media_type,path) VALUES(?,?) ON CONFLICT(media_type,path) DO NOTHING",params![domain_name(media),path]).await?;
    if changed == 0 {
        tx.rollback().await?;
        return Err(Error(StatusCode::CONFLICT, "root_exists"));
    }
    root.id = tx.last_insert_rowid();
    let response = match bounded(root) {
        Ok(response) => response,
        Err(error) => {
            tx.rollback().await?;
            return Err(error);
        }
    };
    tx.commit().await?;
    Ok((StatusCode::CREATED, response))
}
async fn delete(
    State(db): State<Arc<Database>>,
    Extension(media): Extension<MediaDomain>,
    Path(key): Path<String>,
    q: std::result::Result<Query<EmptyQuery>, QueryRejection>,
) -> Result<StatusCode> {
    q.map_err(|_| bad())?;
    let key = id(&key)?;
    let c = db
        .connect()
        .await
        .map_err(|_| Error(StatusCode::INTERNAL_SERVER_ERROR, "root_storage_error"))?;
    if c.execute(
        "DELETE FROM root_folders WHERE media_type=? AND id=?",
        params![domain_name(media), key],
    )
    .await?
        == 0
    {
        return Err(Error(StatusCode::NOT_FOUND, "root_not_found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn observation_limits_and_busy_are_not_empty_successes() {
        let path =
            std::env::temp_dir().join(format!("hrrdarr-root-bounds-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                std::fs::remove_dir_all(&self.0).unwrap()
            }
        }
        let _cleanup = Cleanup(path.clone());
        let root = RootFolder::unknown(
            1,
            MediaDomain::Tv,
            path.to_str().unwrap().into(),
            RootObservationStatus::Busy,
        );
        let permit1 = OBSERVERS.try_acquire().unwrap();
        let permit2 = OBSERVERS.try_acquire().unwrap();
        let result = observe(vec![root.clone()], HashSet::new(), false)
            .await
            .unwrap();
        assert!(matches!(result[0].observation, RootObservationStatus::Busy));
        assert!(result[0].accessible.is_none() && result[0].unmapped_folders.is_none());
        drop(permit1);
        drop(permit2);
        let mut expired = root.clone();
        observe_one(&mut expired, &HashSet::new(), Instant::now(), false).unwrap();
        assert!(matches!(
            expired.observation,
            RootObservationStatus::Timeout
        ));
        assert!(expired.accessible.is_none() && expired.unmapped_folders.is_none());
        for n in 0..=MAX_ENTRIES {
            std::fs::File::create(path.join(format!("file-{n}"))).unwrap();
        }
        let mut capped = root;
        observe_one(&mut capped, &HashSet::new(), Instant::now() + BUDGET, false).unwrap();
        assert!(matches!(capped.observation, RootObservationStatus::Limited));
        assert!(capped.unmapped_folders.is_none());
        // Legacy paths can collapse to one normalized key; count source rows, not set entries.
        let db = Database::open_local(path.join("mapping.db")).await.unwrap();
        let c = db.connect().await.unwrap();
        let mut sql = String::from("BEGIN;");
        for n in 0..=MAX_ENTRIES {
            let mut alias = String::new();
            for bit in 0..14 {
                alias.push('/');
                if n & (1 << bit) != 0 {
                    alias.push('/')
                }
                alias.push('a');
            }
            sql.push_str(&format!(
                "INSERT INTO series(title,path) VALUES('Legacy','{alias}');"
            ));
        }
        sql.push_str("COMMIT;");
        c.execute_batch(&sql).await.unwrap();
        assert!(matches!(
            mapped(&db, MediaDomain::Tv).await,
            Err(Error(StatusCode::SERVICE_UNAVAILABLE, "root_library_limit"))
        ));
    }
}

/// Existence-only check shares the bounded worker pool; no write probe or enumeration.
pub(crate) async fn existing_directory(path: String) -> bool {
    let Ok(permit) = OBSERVERS.try_acquire() else {
        return false;
    };
    let worker = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        crate::import::root_directory(FsPath::new(&path)).is_some()
    });
    matches!(tokio::time::timeout(BUDGET, worker).await, Ok(Ok(true)))
}
