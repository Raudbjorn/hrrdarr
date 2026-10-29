//! Per-domain recycle-bin settings (Sonarr/Radarr Media Management `RecycleBin` and
//! `RecycleBinCleanupDays`). Saving settings never deletes or moves anything: deletion is an
//! explicit file request ([`deletion`]) and retention is the cleanup command.
pub(crate) mod deletion;
pub(crate) mod fsops;
#[cfg(test)]
mod tests;
use crate::{api::MediaDomain, db::Database};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Query, State, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use libsql::{Connection, params};
use serde::{Deserialize, Deserializer, Serialize};
use std::{collections::BTreeMap, sync::Arc};

/// Upstream default retention; inert until a bin path is configured.
pub const DEFAULT_CLEANUP_DAYS: i64 = 7;
pub const MAX_CLEANUP_DAYS: i64 = 36_500;
const MAX_REVISION: i64 = 9_007_199_254_740_991;
const MAX_PATH_BYTES: usize = 4096;
/// The first scheduled cleanup after configuring a bin waits one interval, so a newly chosen
/// folder is never purged the moment it is saved. Manual cleanup remains available.
pub(crate) const CLEANUP_INTERVAL_SECONDS: i64 = 24 * 60 * 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(rename = "RecycleBinSettings")]
pub struct Settings {
    pub media_type: MediaDomain,
    pub revision: i64,
    // Absent means files are deleted permanently, and only with explicit confirmation.
    pub path: Option<String>,
    // Days before cleanup removes recycled files; 0 disables automatic cleanup.
    pub cleanup_days: i64,
}
fn required<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Option<String>, D::Error> {
    Option::<String>::deserialize(d)
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "RecycleBinSettingsUpdate")]
pub struct Update {
    pub revision: i64,
    // Required; null or whitespace-only clears the bin.
    #[serde(deserialize_with = "required")]
    pub path: Option<String>,
    pub cleanup_days: i64,
}
#[derive(Debug)]
pub struct Error(
    pub(crate) StatusCode,
    pub(crate) &'static str,
    pub(crate) &'static str,
);
impl Error {
    pub(crate) fn code(&self) -> &'static str {
        self.1
    }
}
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
    fn from(error: libsql::Error) -> Self {
        eprintln!(
            "event=recycle_bin_storage_error kind={:?}",
            std::mem::discriminant(&error)
        );
        Self(
            StatusCode::SERVICE_UNAVAILABLE,
            "recycle_bin_storage_error",
            "Recycle bin storage is unavailable; nothing was changed",
        )
    }
}
pub type Result<T> = std::result::Result<T, Error>;
pub(crate) fn domain(media: MediaDomain) -> &'static str {
    match media {
        MediaDomain::Tv => "tv",
        MediaDomain::Movies => "movies",
    }
}
fn invalid(message: &'static str) -> Error {
    Error(StatusCode::BAD_REQUEST, "invalid_recycle_bin", message)
}
/// Normalizes a configured bin: absolute POSIX path, no `.`/`..`, controls or backslashes;
/// repeated and trailing separators collapse. Blank (upstream IsNullOrWhiteSpace) means none.
pub(crate) fn normalize(raw: Option<&str>) -> Result<Option<String>> {
    let Some(raw) = raw else { return Ok(None) };
    if raw.trim().is_empty() {
        return Ok(None);
    }
    if raw.len() > MAX_PATH_BYTES
        || !raw.starts_with('/')
        || raw.contains('\\')
        || raw.chars().any(char::is_control)
        || raw.split('/').any(|s| matches!(s, "." | ".."))
    {
        return Err(invalid(
            "Recycle bin must be an absolute POSIX folder path without traversal or control characters",
        ));
    }
    let value = format!(
        "/{}",
        raw.split('/')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("/")
    );
    if value == "/" {
        return Err(invalid("The filesystem root cannot be a recycle bin"));
    }
    Ok(Some(value))
}
/// Component-boundary overlap: equal, ancestor or descendant. Neither side ends in '/'.
pub(crate) fn overlaps(a: &str, b: &str) -> bool {
    let a = a.trim_end_matches('/');
    let b = b.trim_end_matches('/');
    a == b
        || a.is_empty()
        || b.is_empty()
        || b.strip_prefix(a).is_some_and(|rest| rest.starts_with('/'))
        || a.strip_prefix(b).is_some_and(|rest| rest.starts_with('/'))
}
pub(crate) fn validate(input: &Update) -> Result<Option<String>> {
    if !(1..=MAX_REVISION).contains(&input.revision) {
        return Err(invalid("Revision must be a positive integer"));
    }
    if !(0..=MAX_CLEANUP_DAYS).contains(&input.cleanup_days) {
        return Err(invalid(
            "Cleanup days must be a whole number from 0 to 36500",
        ));
    }
    normalize(input.path.as_deref())
}
pub async fn read(c: &Connection, media: MediaDomain) -> Result<Settings> {
    let row = c
        .query(
            "SELECT revision,path,cleanup_days FROM recycle_bin_settings WHERE media_type=?",
            [domain(media)],
        )
        .await?
        .next()
        .await?
        .ok_or(Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "recycle_bin_missing",
            "Recycle bin settings are missing",
        ))?;
    let settings = Settings {
        media_type: media,
        revision: row.get(0)?,
        path: row.get(1)?,
        cleanup_days: row.get(2)?,
    };
    let stored = normalize(settings.path.as_deref()).ok().flatten();
    if stored != settings.path
        || !(0..=MAX_CLEANUP_DAYS).contains(&settings.cleanup_days)
        || !(1..=MAX_REVISION).contains(&settings.revision)
    {
        return Err(Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "recycle_bin_invalid",
            "Stored recycle bin settings are invalid",
        ));
    }
    Ok(settings)
}
/// Library declarations (both domains) overlapping the candidate bin, checked in SQL so the
/// comparison is bounded and matches the migration's write guards exactly.
async fn library_overlap(c: &Connection, bin: &str) -> Result<bool> {
    Ok(c.query("SELECT EXISTS(SELECT 1 FROM (SELECT path p FROM root_folders UNION ALL SELECT path FROM series UNION ALL SELECT path FROM movies) WHERE rtrim(p,'/')=?1 OR substr(rtrim(p,'/'),1,length(?1)+1)=?1||'/' OR substr(?1,1,length(rtrim(p,'/'))+1)=rtrim(p,'/')||'/')", [bin]).await?.next().await?.ok_or(Error(StatusCode::SERVICE_UNAVAILABLE,"recycle_bin_storage_error","Recycle bin storage is unavailable; nothing was changed"))?.get::<i64>(0)? == 1)
}
/// Caller owns an IMMEDIATE transaction; the revision CAS makes this the only write path.
pub(crate) async fn set(
    c: &Connection,
    media: MediaDomain,
    input: &Update,
    now: i64,
) -> Result<Settings> {
    let path = validate(input)?;
    if c.is_autocommit() {
        return Err(Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "recycle_bin_transaction_required",
            "Recycle bin settings require a write transaction",
        ));
    }
    let old = read(c, media).await?;
    if input.revision != old.revision || input.revision == MAX_REVISION {
        return Err(Error(
            StatusCode::CONFLICT,
            "recycle_bin_conflict",
            "Recycle bin settings changed; reload before saving",
        ));
    }
    if let Some(bin) = &path {
        if library_overlap(c, bin).await? {
            return Err(Error(
                StatusCode::CONFLICT,
                "recycle_bin_overlaps_library",
                "The recycle bin cannot be, contain, or be inside a root folder or library path",
            ));
        }
        let other = match media {
            MediaDomain::Tv => MediaDomain::Movies,
            MediaDomain::Movies => MediaDomain::Tv,
        };
        if let Some(other_bin) = read(c, other).await?.path
            && other_bin != *bin && overlaps(&other_bin, bin) {
                return Err(Error(
                    StatusCode::CONFLICT,
                    "recycle_bin_nested",
                    "TV and movie recycle bins may be the same folder but cannot be nested",
                ));
            }
    }
    let changed = c.execute("UPDATE recycle_bin_settings SET path=?1,cleanup_days=?2,revision=revision+1,next_cleanup_at=CASE WHEN ?1 IS NULL THEN NULL WHEN path IS ?1 THEN next_cleanup_at ELSE ?3 END WHERE media_type=?4 AND revision=?5", params![path.clone(), input.cleanup_days, now + CLEANUP_INTERVAL_SECONDS, domain(media), input.revision]).await?;
    if changed != 1 {
        return Err(Error(
            StatusCode::CONFLICT,
            "recycle_bin_conflict",
            "Recycle bin settings changed; reload before saving",
        ));
    }
    crate::health::recycle_bin_changed(c, media)
        .await
        .map_err(|_| {
            Error(
                StatusCode::SERVICE_UNAVAILABLE,
                "recycle_bin_health_unavailable",
                "Health invalidation failed; settings were not saved",
            )
        })?;
    read(c, media).await
}
pub fn router(db: Arc<Database>) -> Router {
    let mut router = Router::new();
    for media in [MediaDomain::Tv, MediaDomain::Movies] {
        router = router.merge(
            Router::new()
                .route(
                    &format!("/api/v1/{}/recycle-bin", domain(media)),
                    get(fetch).put(update),
                )
                .layer(Extension(media))
                // A legal 4096-byte path can expand sixfold when JSON escaped.
                .layer(DefaultBodyLimit::max(32 * 1024))
                .with_state(db.clone()),
        );
    }
    router
}
fn no_query(query: &BTreeMap<String, String>) -> Result<()> {
    if query.is_empty() {
        Ok(())
    } else {
        Err(invalid("This route accepts no query parameters"))
    }
}
fn timeout() -> Error {
    Error(
        StatusCode::SERVICE_UNAVAILABLE,
        "recycle_bin_timeout",
        "Recycle bin settings request timed out",
    )
}
async fn fetch(
    State(db): State<Arc<Database>>,
    Extension(media): Extension<MediaDomain>,
    Query(q): Query<BTreeMap<String, String>>,
) -> Result<Json<Settings>> {
    no_query(&q)?;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let c = db.connect().await?;
        Ok(Json(read(&c, media).await?))
    })
    .await
    .map_err(|_| timeout())?
}
async fn update(
    State(db): State<Arc<Database>>,
    Extension(media): Extension<MediaDomain>,
    Query(q): Query<BTreeMap<String, String>>,
    body: std::result::Result<Json<Update>, JsonRejection>,
) -> Result<Json<Settings>> {
    no_query(&q)?;
    let Json(input) =
        body.map_err(|_| invalid("Body must contain revision, path and cleanup_days only"))?;
    validate(&input)?;
    let now = crate::commands::now().map_err(|_| timeout())?;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let c = db.connect().await?;
        let tx = c
            .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
            .await?;
        match set(&tx, media, &input, now).await {
            Ok(settings) => {
                tx.commit().await?;
                Ok(Json(settings))
            }
            Err(error) => {
                tx.rollback().await?;
                Err(error)
            }
        }
    })
    .await
    .map_err(|_| timeout())?
}
