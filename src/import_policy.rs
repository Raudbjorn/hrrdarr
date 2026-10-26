//! Native domain-scoped import preflight settings. Settings never grant effect or
//! recovery authority, and editing them does not wake release-selection commands.
use crate::{api::MediaDomain, db::Database};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Query, State, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use libsql::{Connection, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Component, Path},
    sync::Arc,
};

pub(crate) const DEFAULT_WORKING_FOLDERS: &str = "_UNPACK_|_FAILED_";
pub(crate) const MINIMUM_FREE_SPACE_MIB: i64 = 100;
pub(crate) const MAXIMUM_FREE_SPACE_MIB: i64 = i32::MAX as i64;
const MAX_REVISION: i64 = 9_007_199_254_740_991;
const MAX_PREFIX_BYTES: usize = 4096;
const BYTES_PER_MIB: i64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(rename = "ImportPolicy")]
pub struct Policy {
    pub media_type: MediaDomain,
    pub revision: i64,
    pub skip_free_space_check_when_importing: bool,
    pub minimum_free_space_mib: i64,
    pub download_client_working_folders: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "ImportPolicyUpdate")]
pub struct Update {
    pub revision: i64,
    pub skip_free_space_check_when_importing: bool,
    pub minimum_free_space_mib: i64,
    pub download_client_working_folders: String,
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
                "Import policy operation failed",
            )),
        )
            .into_response()
    }
}
impl From<libsql::Error> for Error {
    fn from(_: libsql::Error) -> Self {
        Self(
            StatusCode::SERVICE_UNAVAILABLE,
            "import_policy_storage_error",
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
fn invalid() -> Error {
    Error(StatusCode::BAD_REQUEST, "invalid_import_policy")
}
pub(crate) fn canonical_working_folders(raw: &str) -> Result<&str> {
    if raw.len() > MAX_PREFIX_BYTES {
        return Err(invalid());
    }
    // ConfigService substitutes defaults only for an entirely empty value. Do not
    // trim whitespace or remove empty segments from a nonempty literal string.
    Ok(if raw.is_empty() {
        DEFAULT_WORKING_FOLDERS
    } else {
        raw
    })
}
pub(crate) fn validate(input: &Update) -> Result<()> {
    if !(1..=MAX_REVISION).contains(&input.revision)
        || !(MINIMUM_FREE_SPACE_MIB..=MAXIMUM_FREE_SPACE_MIB)
            .contains(&input.minimum_free_space_mib)
    {
        return Err(invalid());
    }
    canonical_working_folders(&input.download_client_working_folders)?;
    Ok(())
}
impl Policy {
    pub(crate) fn minimum_bytes(&self) -> Result<i64> {
        if !(MINIMUM_FREE_SPACE_MIB..=MAXIMUM_FREE_SPACE_MIB).contains(&self.minimum_free_space_mib)
        {
            return Err(invalid());
        }
        self.minimum_free_space_mib
            .checked_mul(BYTES_PER_MIB)
            .ok_or_else(invalid)
    }
    /// Pure Linux ancestor-prefix check. Caller supplies the actual absolute source
    /// path from its verified inventory; this method performs no filesystem reads.
    pub(crate) fn source_ancestor_matches(&self, source: &Path) -> Result<bool> {
        let raw = source
            .to_str()
            .ok_or(Error(StatusCode::BAD_REQUEST, "invalid_import_source_path"))?;
        if !source.is_absolute()
            || source.file_name().is_none()
            || raw.len() > 4096
            || raw.chars().any(char::is_control)
            || source
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        {
            return Err(Error(StatusCode::BAD_REQUEST, "invalid_import_source_path"));
        }
        let folders = canonical_working_folders(&self.download_client_working_folders)?;
        for ancestor in source.parent().into_iter().flat_map(Path::ancestors) {
            // The Unix root directory's name is '/', as in the pinned source. It
            // participates too; the media filename itself is deliberately excluded.
            let name = ancestor.file_name().and_then(|s| s.to_str()).unwrap_or("/");
            if folders.split('|').any(|prefix| name.starts_with(prefix)) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
pub async fn read(c: &Connection, media: MediaDomain) -> Result<Policy> {
    let row=c.query("SELECT revision,skip_free_space_check_when_importing,minimum_free_space_mib,substr(CAST(download_client_working_folders AS BLOB),1,4097),locally_edited FROM import_policies WHERE media_type=?",[domain(media)]).await?.next().await?.ok_or(Error(StatusCode::SERVICE_UNAVAILABLE,"import_policy_missing"))?;
    let revision: i64 = row.get(0)?;
    let skip: i64 = row.get(1)?;
    let minimum: i64 = row.get(2)?;
    let bytes: Vec<u8> = row.get(3)?;
    let locally_edited: i64 = row.get(4)?;
    let working = String::from_utf8(bytes)
        .map_err(|_| Error(StatusCode::SERVICE_UNAVAILABLE, "import_policy_invalid"))?;
    let input = Update {
        revision,
        skip_free_space_check_when_importing: skip == 1,
        minimum_free_space_mib: minimum,
        download_client_working_folders: working,
    };
    if !matches!(skip, 0 | 1) || !matches!(locally_edited, 0 | 1) || validate(&input).is_err() {
        return Err(Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "import_policy_invalid",
        ));
    }
    Ok(Policy {
        media_type: media,
        revision,
        skip_free_space_check_when_importing: input.skip_free_space_check_when_importing,
        minimum_free_space_mib: minimum,
        download_client_working_folders: canonical_working_folders(
            &input.download_client_working_folders,
        )
        .map_err(|_| Error(StatusCode::SERVICE_UNAVAILABLE, "import_policy_invalid"))?
        .into(),
    })
}
/// Caller owns an IMMEDIATE transaction. The transaction presence guard and exact
/// affected-row CAS prevent this setter from becoming an independent write path.
pub(crate) async fn set(
    c: &Connection,
    media: MediaDomain,
    input: &Update,
    local: bool,
) -> Result<Policy> {
    validate(input)?;
    if c.is_autocommit() {
        return Err(Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "import_policy_transaction_required",
        ));
    }
    let old = read(c, media).await?;
    if input.revision != old.revision || input.revision == MAX_REVISION {
        return Err(Error(StatusCode::CONFLICT, "import_policy_conflict"));
    }
    let changed=c.execute("UPDATE import_policies SET skip_free_space_check_when_importing=?,minimum_free_space_mib=?,download_client_working_folders=?,revision=revision+1,locally_edited=CASE WHEN ? THEN 1 ELSE locally_edited END WHERE media_type=? AND revision=?",params![input.skip_free_space_check_when_importing,input.minimum_free_space_mib,canonical_working_folders(&input.download_client_working_folders)?,local,domain(media),input.revision]).await?;
    if changed != 1 {
        return Err(Error(StatusCode::CONFLICT, "import_policy_conflict"));
    }
    read(c, media).await
}
pub fn router(db: Arc<Database>) -> Router {
    let mut router = Router::new();
    for media in [MediaDomain::Tv, MediaDomain::Movies] {
        router = router.merge(
            Router::new()
                .route(
                    &format!("/api/v1/{}/import-policy", domain(media)),
                    get(fetch).put(update),
                )
                .layer(Extension(media))
                // A legal 4096-byte string can expand sixfold when JSON escaped.
                .layer(DefaultBodyLimit::max(32 * 1024))
                .with_state(db.clone()),
        );
    }
    router
}
fn query(query: &BTreeMap<String, String>) -> Result<()> {
    if query.is_empty() {
        Ok(())
    } else {
        Err(Error(StatusCode::BAD_REQUEST, "invalid_query"))
    }
}
async fn fetch(
    State(db): State<Arc<Database>>,
    Extension(media): Extension<MediaDomain>,
    Query(q): Query<BTreeMap<String, String>>,
) -> Result<Json<Policy>> {
    query(&q)?;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let c = db.connect().await?;
        Ok(Json(read(&c, media).await?))
    })
    .await
    .map_err(|_| Error(StatusCode::SERVICE_UNAVAILABLE, "import_policy_timeout"))?
}
async fn update(
    State(db): State<Arc<Database>>,
    Extension(media): Extension<MediaDomain>,
    Query(q): Query<BTreeMap<String, String>>,
    body: std::result::Result<Json<Update>, JsonRejection>,
) -> Result<Json<Policy>> {
    query(&q)?;
    let Json(input) = body.map_err(|_| invalid())?;
    validate(&input)?;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let c = db.connect().await?;
        let tx = c
            .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
            .await?;
        match set(&tx, media, &input, true).await {
            Ok(policy) => {
                tx.commit().await?;
                Ok(Json(policy))
            }
            Err(error) => {
                tx.rollback().await?;
                Err(error)
            }
        }
    })
    .await
    .map_err(|_| Error(StatusCode::SERVICE_UNAVAILABLE, "import_policy_timeout"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    fn policy(prefix: &str) -> Policy {
        Policy {
            media_type: MediaDomain::Tv,
            revision: 1,
            skip_free_space_check_when_importing: false,
            minimum_free_space_mib: 100,
            download_client_working_folders: prefix.into(),
        }
    }
    #[test]
    fn literal_ancestors_and_whole_empty_preserve_source_semantics() {
        assert!(
            policy("")
                .source_ancestor_matches(Path::new("/downloads/_UNPACK_Film/sub/file.mkv"))
                .unwrap()
        );
        assert!(
            !policy("")
                .source_ancestor_matches(Path::new("/downloads/_UNPACK_File.mkv"))
                .unwrap()
        );
        assert!(
            !policy("_UNPACK_")
                .source_ancestor_matches(Path::new("/downloads/_unpack_Film/file.mkv"))
                .unwrap()
        );
        assert!(
            policy("a||b")
                .source_ancestor_matches(Path::new("/file.mkv"))
                .unwrap()
        );
        assert!(
            policy("a||b")
                .source_ancestor_matches(Path::new("/none/file.mkv"))
                .unwrap()
        );
        assert!(
            policy("/")
                .source_ancestor_matches(Path::new("/file.mkv"))
                .unwrap()
        );
        assert!(
            policy("  ")
                .source_ancestor_matches(Path::new("/  pending/file.mkv"))
                .unwrap()
        );
        assert!(
            !policy("  ")
                .source_ancestor_matches(Path::new("/pending/file.mkv"))
                .unwrap()
        );
        assert!(
            policy("待機")
                .source_ancestor_matches(Path::new("/待機中/file.mkv"))
                .unwrap()
        );
        for path in ["relative/file.mkv", "/a/../file.mkv", "/"] {
            assert!(policy("").source_ancestor_matches(Path::new(path)).is_err());
        }
    }
    #[test]
    fn byte_conversion_and_validation_are_bounded() {
        let mut p = policy("");
        assert_eq!(p.minimum_bytes().unwrap(), 104_857_600);
        p.minimum_free_space_mib = i32::MAX.into();
        assert_eq!(p.minimum_bytes().unwrap(), 2_251_799_812_636_672);
        p.minimum_free_space_mib = i64::MAX;
        assert!(p.minimum_bytes().is_err());
        assert_eq!(
            canonical_working_folders("").unwrap(),
            DEFAULT_WORKING_FOLDERS
        );
        assert_eq!(canonical_working_folders(" a||b ").unwrap(), " a||b ");
        assert!(canonical_working_folders(&"é".repeat(2048)).is_ok());
        assert!(canonical_working_folders(&"é".repeat(2049)).is_err());
        // Prefixes are literal comparisons, never executable paths. Preserve even
        // an inert NUL prefix rather than silently rewriting imported intent.
        assert_eq!(canonical_working_folders("a\n\t\0").unwrap(), "a\n\t\0");
    }
}
