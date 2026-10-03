//! Persisted first-pair configuration. Saving configuration performs no network requests.
pub mod administration;
pub mod categories;
mod credentials;
pub mod draft;
pub mod http;
pub mod indexer;
pub mod qbittorrent;
use crate::{
    api::{ApiErrorEnvelope, ApiPage, MediaDomain},
    db::Database,
    library::{Change, change},
};
use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, Path, Query, State,
        rejection::{JsonRejection, QueryRejection},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
pub use credentials::{CredentialKey, Credentials, IndexerAccess, IndexerParameter};
use libsql::{Connection, params};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, sync::Arc};
use uuid::Uuid;

#[derive(Debug)]
enum Error {
    Plain(StatusCode, &'static str, &'static str),
    RateLimited(u32),
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        match self {
            Self::Plain(status, code, message) => {
                (status, Json(ApiErrorEnvelope::new(code, message))).into_response()
            }
            Self::RateLimited(seconds) => (
                StatusCode::TOO_MANY_REQUESTS,
                [(axum::http::header::RETRY_AFTER, seconds.to_string())],
                Json(ApiErrorEnvelope::new(
                    "rate_limited",
                    "Provider is rate limited; retry after the indicated interval",
                )),
            )
                .into_response(),
        }
    }
}
impl From<libsql::Error> for Error {
    fn from(error: libsql::Error) -> Self {
        eprintln!(
            "{}",
            serde_json::json!({"level":"ERROR","event":"provider_database_error","correlation_id":Uuid::new_v4(),"error_class":format!("{:?}",std::mem::discriminant(&error))})
        );
        Self::Plain(
            StatusCode::INTERNAL_SERVER_ERROR,
            "provider_database_error",
            "Provider operation failed; no partial configuration committed",
        )
    }
}
type Result<T> = std::result::Result<T, Error>;
fn bad() -> Error {
    Error::Plain(
        StatusCode::BAD_REQUEST,
        "invalid_provider_config",
        "Invalid provider configuration",
    )
}
fn conflict() -> Error {
    Error::Plain(
        StatusCode::CONFLICT,
        "provider_revision_conflict",
        "Provider revision or immutable implementation conflicts",
    )
}
fn missing() -> Error {
    Error::Plain(
        StatusCode::NOT_FOUND,
        "provider_not_found",
        "Provider does not exist",
    )
}
fn locked() -> Error {
    Error::Plain(
        StatusCode::SERVICE_UNAVAILABLE,
        "provider_credentials_locked",
        "Provider credentials cannot be unlocked; check the independently stored key",
    )
}
fn corrupt() -> Error {
    Error::Plain(
        StatusCode::INTERNAL_SERVER_ERROR,
        "invalid_stored_provider",
        "Stored provider configuration is invalid",
    )
}

fn enabled_by_default() -> bool {
    true
}

/// Server-owned intent: request payloads cannot select automatic authorization.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IndexerOperation {
    Rss,
    Automatic,
    Interactive,
}

pub(crate) fn indexer_operation_enabled(
    provider: &Provider,
    domain: MediaDomain,
    operation: IndexerOperation,
) -> bool {
    if !provider.enabled {
        return false;
    }
    let flags = match (&provider.settings, domain) {
        (
            ProviderSettings::Torznab {
                tv: Some(scope), ..
            }
            | ProviderSettings::Newznab {
                tv: Some(scope), ..
            },
            MediaDomain::Tv,
        ) => (
            scope.enable_rss,
            scope.enable_automatic_search,
            scope.enable_interactive_search,
        ),
        (
            ProviderSettings::Torznab {
                movies: Some(scope),
                ..
            }
            | ProviderSettings::Newznab {
                movies: Some(scope),
                ..
            },
            MediaDomain::Movies,
        ) => (
            scope.enable_rss,
            scope.enable_automatic_search,
            scope.enable_interactive_search,
        ),
        _ => return false,
    };
    match operation {
        IndexerOperation::Rss => flags.0,
        IndexerOperation::Automatic => flags.1,
        IndexerOperation::Interactive => flags.2,
    }
}

fn request_policy(
    provider: &Provider,
    operation: IndexerOperation,
    request: &indexer::IndexerSearch,
) -> bool {
    let (domain, rss) = match request {
        indexer::IndexerSearch::Rss { media_type, .. } => (*media_type, true),
        indexer::IndexerSearch::Tv { .. } => (MediaDomain::Tv, false),
        indexer::IndexerSearch::Movie { .. } => (MediaDomain::Movies, false),
    };
    rss == (operation == IndexerOperation::Rss)
        && indexer_operation_enabled(provider, domain, operation)
}

#[derive(Clone, Deserialize, Serialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct TvIndexerScope {
    #[serde(default = "enabled_by_default")]
    #[ts(as = "Option<bool>", optional)]
    pub enable_rss: bool,
    #[serde(default = "enabled_by_default")]
    #[ts(as = "Option<bool>", optional)]
    pub enable_automatic_search: bool,
    #[serde(default = "enabled_by_default")]
    #[ts(as = "Option<bool>", optional)]
    pub enable_interactive_search: bool,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub download_client_id: Option<Uuid>,
    pub categories: Vec<u32>,
    pub anime_categories: Vec<u32>,
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    pub anime_standard_format_search: bool,
}
#[derive(Clone, Deserialize, Serialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct MovieIndexerScope {
    #[serde(default = "enabled_by_default")]
    #[ts(as = "Option<bool>", optional)]
    pub enable_rss: bool,
    #[serde(default = "enabled_by_default")]
    #[ts(as = "Option<bool>", optional)]
    pub enable_automatic_search: bool,
    #[serde(default = "enabled_by_default")]
    #[ts(as = "Option<bool>", optional)]
    pub enable_interactive_search: bool,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub download_client_id: Option<Uuid>,
    pub categories: Vec<u32>,
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    pub remove_year: bool,
}
#[derive(Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum DownloadInitialState {
    #[default]
    Started,
    Stopped,
    Forced,
}
#[derive(Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum DownloadContentLayout {
    #[default]
    Default,
    Original,
    Subfolder,
}
#[derive(Clone, Default, Deserialize, Serialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct DownloadScope {
    pub category: String,
    #[serde(deserialize_with = "required_nullable")]
    pub imported_category: Option<String>,
    pub recent_priority: i8,
    pub older_priority: i8,
    #[serde(default)]
    #[ts(as = "Option<DownloadInitialState>", optional)]
    pub initial_state: DownloadInitialState,
    #[serde(default)]
    #[ts(as = "Option<DownloadContentLayout>", optional)]
    pub content_layout: DownloadContentLayout,
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    pub sequential_order: bool,
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    pub first_last_first: bool,
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    pub add_tags: bool,
}
#[derive(Clone, Deserialize, Serialize, ts_rs::TS)]
#[serde(tag = "implementation", rename_all = "lowercase", deny_unknown_fields)]
pub enum ProviderSettings {
    Torznab {
        endpoint: String,
        #[serde(deserialize_with = "required_nullable")]
        tv: Option<TvIndexerScope>,
        #[serde(deserialize_with = "required_nullable")]
        movies: Option<MovieIndexerScope>,
    },
    Newznab {
        endpoint: String,
        #[serde(deserialize_with = "required_nullable")]
        tv: Option<TvIndexerScope>,
        #[serde(deserialize_with = "required_nullable")]
        movies: Option<MovieIndexerScope>,
    },
    Qbittorrent {
        endpoint: String,
        #[serde(deserialize_with = "required_nullable")]
        tv: Option<DownloadScope>,
        #[serde(deserialize_with = "required_nullable")]
        movies: Option<DownloadScope>,
    },
}
impl ProviderSettings {
    fn implementation(&self) -> &'static str {
        match self {
            Self::Torznab { .. } => "torznab",
            Self::Newznab { .. } => "newznab",
            Self::Qbittorrent { .. } => "qbittorrent",
        }
    }
    fn endpoint(&self) -> &str {
        match self {
            Self::Torznab { endpoint, .. }
            | Self::Newznab { endpoint, .. }
            | Self::Qbittorrent { endpoint, .. } => endpoint,
        }
    }
    fn validate(&self) -> Result<()> {
        validate_endpoint(self.endpoint())?;
        let categories = |c: &[u32]| {
            c.len() <= 64
                && c.iter().all(|n| (1..=i32::MAX as u32).contains(n))
                && c.iter().collect::<BTreeSet<_>>().len() == c.len()
        };
        match self {
            Self::Torznab { tv, movies, .. } | Self::Newznab { tv, movies, .. } => {
                if tv.is_none() && movies.is_none() {
                    return Err(bad());
                }
                if tv.as_ref().is_some_and(|s| {
                    !categories(&s.categories)
                        || !categories(&s.anime_categories)
                        || (s.categories.is_empty() && s.anime_categories.is_empty())
                }) || movies
                    .as_ref()
                    .is_some_and(|s| s.categories.is_empty() || !categories(&s.categories))
                {
                    return Err(bad());
                }
            }
            Self::Qbittorrent { tv, movies, .. } => {
                if tv.is_none() && movies.is_none() {
                    return Err(bad());
                }
                if movies.as_ref().is_some_and(|scope| scope.add_tags) {
                    return Err(bad());
                }
                let overlaps = |a: &str, b: &str| {
                    a == b
                        || a.strip_prefix(b).is_some_and(|s| s.starts_with('/'))
                        || b.strip_prefix(a).is_some_and(|s| s.starts_with('/'))
                };
                let category = |c: &str| {
                    !c.trim().is_empty()
                        && c.len() <= 64
                        && !c.chars().any(char::is_control)
                        && !c.contains('\\')
                        && !c.contains("//")
                        && !c.starts_with('/')
                        && !c.ends_with('/')
                };
                for s in tv.iter().chain(movies.iter()) {
                    if !category(&s.category)
                        || s.imported_category
                            .as_ref()
                            .is_some_and(|c| !category(c) || overlaps(c, &s.category))
                        || !(0..=1).contains(&s.recent_priority)
                        || !(0..=1).contains(&s.older_priority)
                    {
                        return Err(bad());
                    }
                }
                if let (Some(tv), Some(movies)) = (tv, movies) {
                    let tvcats = std::iter::once(&tv.category).chain(tv.imported_category.iter());
                    let mcats: Vec<_> = std::iter::once(&movies.category)
                        .chain(movies.imported_category.iter())
                        .collect();
                    if tvcats
                        .into_iter()
                        .any(|c| mcats.iter().any(|m| overlaps(c, m)))
                    {
                        return Err(bad());
                    }
                }
            }
        }
        Ok(())
    }
}
fn validate_endpoint(endpoint: &str) -> Result<()> {
    let url = url::Url::parse(endpoint).map_err(|_| bad())?;
    if endpoint.trim() != endpoint
        || endpoint.contains('\\')
        || endpoint.len() > 2048
        || endpoint.chars().any(char::is_control)
        || !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(bad());
    }
    Ok(())
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ProviderInput {
    pub name: String,
    pub enabled: bool,
    pub priority: u8,
    pub settings: ProviderSettings,
    #[serde(default, deserialize_with = "change")]
    #[ts(as="Option<Credentials>",optional=nullable)]
    pub credentials: Change<Credentials>,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ProviderUpdate {
    pub revision: i64,
    #[serde(flatten)]
    pub config: ProviderInput,
}
#[derive(Serialize, ts_rs::TS)]
pub struct Provider {
    pub id: Uuid,
    pub revision: i64,
    pub name: String,
    pub enabled: bool,
    pub priority: u8,
    pub settings: ProviderSettings,
    pub has_credentials: bool,
    pub test_supported: bool,
    pub test_status: TestStatus,
    pub last_test: Option<ProviderTestObservation>,
}
#[derive(Clone, Copy, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum TestStatus {
    NeverTested,
    Success,
    Failure,
}
#[derive(Serialize, ts_rs::TS)]
pub struct ProviderTestObservation {
    pub revision: i64,
    pub tested_at: i64,
    pub status: TestStatus,
    pub error_code: Option<String>,
}
#[derive(Serialize, ts_rs::TS)]
pub struct ProviderTestResult {
    pub provider_id: Uuid,
    pub revision: i64,
    pub tested_at: i64,
    pub result: ProviderTestOutcome,
}
#[derive(Serialize, ts_rs::TS)]
#[serde(untagged)]
pub enum ProviderTestOutcome {
    Indexer(indexer::IndexerTest),
    DownloadClient(qbittorrent::ClientTest),
}
#[derive(Serialize, ts_rs::TS)]
pub struct ProviderDownloadResult {
    pub provider_id: Uuid,
    pub revision: i64,
    pub page: qbittorrent::DownloadPage,
}
#[derive(Serialize, ts_rs::TS)]
pub struct ProviderFilesResult {
    pub provider_id: Uuid,
    pub revision: i64,
    pub result: qbittorrent::DownloadFiles,
}
#[derive(Serialize, ts_rs::TS)]
pub struct ProviderSearchResult {
    pub provider_id: Uuid,
    pub revision: i64,
    pub page: indexer::IndexerPage,
}

#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ProviderQuery {
    #[serde(default = "default_limit")]
    #[ts(as = "Option<u16>", optional)]
    pub limit: u16,
    #[serde(default)]
    #[ts(as = "Option<u32>", optional)]
    pub offset: u32,
    #[ts(optional)]
    pub media_type: Option<MediaDomain>,
}
fn required_nullable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> std::result::Result<Option<T>, D::Error> {
    Option::<T>::deserialize(deserializer)
}
fn default_limit() -> u16 {
    50
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ProviderRevision {
    pub revision: i64,
}
#[derive(Clone)]
struct Context {
    db: Arc<Database>,
    key: Option<Arc<CredentialKey>>,
    transport: Arc<std::result::Result<http::HttpClient, http::HttpError>>,
}
pub fn router(db: Arc<Database>, key: Option<Arc<CredentialKey>>) -> Router {
    router_with_refresh(db, key).0
}

/// HTTP requests and durable refreshes share connection pools, provider lanes and cooldowns.
pub fn router_with_refresh(
    db: Arc<Database>,
    key: Option<Arc<CredentialKey>>,
) -> (Router, RefreshClient) {
    let context = Context {
        db,
        key,
        transport: Arc::new(http::HttpClient::new()),
    };
    let router = Router::new()
        .route("/api/v1/providers", get(list).post(create))
        .route("/api/v1/providers/schema", get(administration::schema))
        .route(
            "/api/v1/providers/bulk",
            axum::routing::put(administration::update).delete(administration::delete),
        )
        .route("/api/v1/providers/testall", post(administration::test_all))
        .route("/api/v1/providers/test-draft", post(draft::test))
        .route(
            "/api/v1/providers/indexer-categories",
            post(categories::discover),
        )
        .route(
            "/api/v1/providers/{id}",
            get(detail).put(update).delete(delete),
        )
        .route("/api/v1/providers/{id}/test", post(test))
        .route("/api/v1/providers/{id}/search", post(search))
        .route("/api/v1/providers/{id}/downloads", post(downloads))
        .route("/api/v1/providers/{id}/files", post(files))
        .route("/api/v1/providers/{id}/path-preview", post(path_preview))
        .layer(DefaultBodyLimit::max(32 * 1024))
        .with_state(context.clone());
    (router, RefreshClient(context))
}

#[derive(Clone)]
pub struct RefreshClient(Context);

#[derive(Debug)]
pub(crate) struct RefreshError {
    pub code: &'static str,
    pub retryable: bool,
    pub retry_after_seconds: Option<u32>,
}
pub(crate) type AutomationError = RefreshError;
impl RefreshError {
    fn new(code: &'static str, retryable: bool) -> Self {
        Self {
            code,
            retryable,
            retry_after_seconds: None,
        }
    }
}
fn refresh_error(error: Error) -> RefreshError {
    match error {
        Error::RateLimited(seconds) => RefreshError {
            code: "refresh_failed",
            retryable: true,
            retry_after_seconds: Some(seconds.clamp(1, 86400)),
        },
        Error::Plain(_, "provider_revision_conflict", _) => {
            RefreshError::new("provider_changed", false)
        }
        Error::Plain(_, "timeout", _) => RefreshError::new("refresh_timeout", true),
        Error::Plain(_, "response_too_large", _) => RefreshError::new("refresh_limit", false),
        Error::Plain(status, code, _)
            if status == StatusCode::NOT_FOUND
                || status == StatusCode::SERVICE_UNAVAILABLE
                || code == "authentication"
                || status == StatusCode::BAD_REQUEST
                || status == StatusCode::UNPROCESSABLE_ENTITY =>
        {
            RefreshError::new("provider_unavailable", false)
        }
        Error::Plain(_, "provider_busy", _) => RefreshError {
            code: "refresh_failed",
            retryable: true,
            retry_after_seconds: Some(1),
        },
        _ => RefreshError::new("refresh_failed", true),
    }
}
/// Private health distinction, before lossy public automation error mapping.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CommunicationProbeError {
    RemoteFailure,
    Unevaluated(&'static str),
}
fn communication_error(error: qbittorrent::QbitError) -> CommunicationProbeError {
    use CommunicationProbeError::{RemoteFailure, Unevaluated};
    use http::HttpError as H;
    use qbittorrent::QbitError as Q;
    match error {
        Q::Http(H::LocalUnavailable { timeout: true }) => Unevaluated("check_timeout"),
        Q::Http(
            H::LocalUnavailable { timeout: false }
            | H::Busy
            | H::InvalidRequest
            | H::ResponseTooLarge,
        )
        | Q::InvalidRequest
        | Q::ScopeConflict
        | Q::UnsupportedFeature
        | Q::UnsafeRetention
        | Q::NotFound
        | Q::MutationUnknown => Unevaluated("check_failed"),
        Q::Http(
            H::Authentication
            | H::RateLimited { .. }
            | H::Redirect
            | H::Transport
            | H::Timeout
            | H::InvalidResponse,
        )
        | Q::InvalidResponse
        | Q::UnsupportedVersion
        | Q::Rejected => RemoteFailure,
    }
}
// Identity mapping is valid only for local POSIX syntax; never reinterpret an unmapped UNC.
fn local_download_root(path: &str) -> Option<String> {
    if path.starts_with("//") {
        return None;
    }
    if path == "/" {
        return Some(path.into());
    }
    crate::library::normalized_path(path)
}

/// Private download paths and file facts; never directly serialize these into an API response.
pub(crate) struct OwnedDownloadDetails {
    pub details: qbittorrent::TorrentDetails,
    pub host: String,
}
impl RefreshClient {
    /// Read the actual scoped item list; connection/version success alone is insufficient.
    pub(crate) async fn probe_download_communication(
        &self,
        id: &str,
        revision: i64,
        domain: MediaDomain,
    ) -> std::result::Result<(), CommunicationProbeError> {
        use CommunicationProbeError::Unevaluated;
        let context = &self.0;
        let (provider, credentials) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            network_snapshot(context, id),
        )
        .await
        .map_err(|_| Unevaluated("check_timeout"))?
        .map_err(|error| match error {
            Error::Plain(_, "provider_database_error", _) => Unevaluated("storage_error"),
            _ => Unevaluated("check_failed"),
        })?;
        let scoped = match &provider.settings {
            ProviderSettings::Qbittorrent { tv, movies, .. } => match domain {
                MediaDomain::Tv => tv.is_some(),
                MediaDomain::Movies => movies.is_some(),
            },
            _ => false,
        };
        if provider.revision != revision || !provider.enabled || !scoped {
            return Err(Unevaluated("check_failed"));
        }
        provider
            .settings
            .validate()
            .map_err(|_| Unevaluated("check_failed"))?;
        let transport = context
            .transport
            .as_ref()
            .as_ref()
            .map_err(|_| Unevaluated("check_failed"))?;
        // Acquisition cooldown is old evidence; an HTTP429 returned by the probe is new evidence.
        let operation = transport
            .operation(provider.id)
            .map_err(|_| Unevaluated("check_failed"))?;
        let work = async {
            let result = qbittorrent::refresh_pages(
                &operation,
                &provider.settings,
                credentials.as_ref(),
                domain,
            )
            .await
            .map(|_| ())
            .map_err(communication_error);
            current_revision(&context.db, &provider)
                .await
                .map_err(|error| match error {
                    Error::Plain(_, "provider_database_error", _) => Unevaluated("storage_error"),
                    _ => Unevaluated("check_failed"),
                })?;
            operation
                .ensure_active()
                .map_err(|_| Unevaluated("check_timeout"))?;
            result
        };
        tokio::time::timeout_at(operation.deadline(), work)
            .await
            .map_err(|_| Unevaluated("check_timeout"))?
    }
    /// Fresh, revision-fenced status for health; never exposes remote paths or credentials.
    pub(crate) async fn inspect_client_status(
        &self,
        provider_id: &str,
        revision: i64,
        domain: MediaDomain,
    ) -> std::result::Result<qbittorrent::EndpointLocality, AutomationError> {
        self.inspect_client_status_snapshot(provider_id, revision, domain)
            .await
            .map(|(_, status, _)| status.locality)
    }

    /// Fresh active-scope output directories in the configured local path namespace.
    /// Private facts only: the health engine owns comparisons, invalidation and publication.
    pub(crate) async fn inspect_download_roots(
        &self,
        provider_id: &str,
        revision: i64,
        domain: MediaDomain,
    ) -> std::result::Result<Vec<String>, AutomationError> {
        let (provider, status, deadline) = self
            .inspect_client_status_snapshot(provider_id, revision, domain)
            .await?;
        let endpoint = url::Url::parse(provider.settings.endpoint())
            .map_err(|_| RefreshError::new("provider_unavailable", false))?;
        let host = crate::remote_paths::host(
            endpoint
                .host_str()
                .ok_or_else(|| RefreshError::new("provider_unavailable", false))?,
        )
        .map_err(|_| RefreshError::new("provider_unavailable", false))?;
        let work = async {
            // No DB connection or transaction spans the remote status request above.
            let connection = self
                .0
                .db
                .connect()
                .await
                .map_err(|_| RefreshError::new("storage_error", true))?;
            let resolution = crate::remote_paths::resolve(
                &connection,
                domain,
                crate::remote_paths::ResolveInput {
                    host,
                    path: status.remote_root,
                    direction: crate::remote_paths::Direction::RemoteToLocal,
                },
            )
            .await
            .map_err(|_| RefreshError::new("download_root_mapping_failed", false))?;
            let path = local_download_root(&resolution.output)
                .ok_or_else(|| RefreshError::new("download_root_unresolved", false))?;
            current_revision(&self.0.db, &provider)
                .await
                .map_err(refresh_error)?;
            Ok::<_, AutomationError>(vec![path])
        };
        tokio::time::timeout_at(deadline, work)
            .await
            .map_err(|_| RefreshError::new("refresh_timeout", true))?
    }

    async fn inspect_client_status_snapshot(
        &self,
        provider_id: &str,
        revision: i64,
        domain: MediaDomain,
    ) -> std::result::Result<
        (Provider, qbittorrent::ClientStatus, tokio::time::Instant),
        AutomationError,
    > {
        let context = &self.0;
        let (provider, credentials) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            network_snapshot(context, provider_id),
        )
        .await
        .map_err(|_| RefreshError::new("refresh_timeout", true))?
        .map_err(refresh_error)?;
        if provider.revision != revision {
            return Err(RefreshError::new("provider_changed", false));
        }
        if !provider.enabled || !matches!(provider.settings, ProviderSettings::Qbittorrent { .. }) {
            return Err(RefreshError::new("provider_unavailable", false));
        }
        let transport = context
            .transport
            .as_ref()
            .as_ref()
            .map_err(|e| refresh_error(http_error(*e).0))?;
        let operation = transport
            .operation(provider.id)
            .map_err(|e| refresh_error(http_error(e).0))?;
        let work = async {
            let status = qbittorrent::client_status(
                &operation,
                &provider.settings,
                credentials.as_ref(),
                domain,
            )
            .await
            .map_err(|e| refresh_error(qbit_error(e).0))?;
            current_revision(&context.db, &provider)
                .await
                .map_err(refresh_error)?;
            operation
                .ensure_active()
                .map_err(|e| refresh_error(http_error(e).0))?;
            Ok::<_, AutomationError>(status)
        };
        let deadline = operation.deadline();
        let status = tokio::time::timeout_at(deadline, work)
            .await
            .map_err(|_| RefreshError::new("refresh_timeout", true))??;
        Ok((provider, status, deadline))
    }
    pub(crate) async fn inspect_download(
        &self,
        provider_id: &str,
        revision: i64,
        target: &crate::db::MediaTarget,
        remote_id: &str,
    ) -> std::result::Result<OwnedDownloadDetails, AutomationError> {
        let context = &self.0;
        let (provider, credentials) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            network_snapshot(context, provider_id),
        )
        .await
        .map_err(|_| RefreshError::new("refresh_timeout", true))?
        .map_err(refresh_error)?;
        if provider.revision != revision {
            return Err(RefreshError::new("provider_changed", false));
        }
        if !provider.enabled || !matches!(provider.settings, ProviderSettings::Qbittorrent { .. }) {
            return Err(RefreshError::new("provider_unavailable", false));
        }
        let endpoint = url::Url::parse(provider.settings.endpoint())
            .map_err(|_| RefreshError::new("provider_unavailable", false))?;
        let host = crate::remote_paths::host(
            endpoint
                .host_str()
                .ok_or(RefreshError::new("provider_unavailable", false))?,
        )
        .map_err(|_| RefreshError::new("provider_unavailable", false))?;
        let transport = context
            .transport
            .as_ref()
            .as_ref()
            .map_err(|e| refresh_error(http_error(*e).0))?;
        let operation = transport
            .operation(provider.id)
            .map_err(|e| refresh_error(http_error(e).0))?;
        let work = async {
            let result = qbittorrent::details(
                &operation,
                &provider.settings,
                credentials.as_ref(),
                target,
                remote_id,
            )
            .await
            .map_err(|e| refresh_error(qbit_error(e).0));
            current_revision(&context.db, &provider)
                .await
                .map_err(refresh_error)?;
            operation
                .ensure_active()
                .map_err(|e| refresh_error(http_error(e).0))?;
            Ok(OwnedDownloadDetails {
                details: result?,
                host,
            })
        };
        tokio::time::timeout_at(operation.deadline(), work)
            .await
            .map_err(|_| RefreshError::new("refresh_timeout", true))?
    }
    pub(crate) fn matches_database(&self, db: &Database) -> bool {
        std::ptr::eq(self.0.db.as_ref(), db)
    }

    pub(crate) fn seal_release(
        &self,
        context: &str,
        plain: &[u8],
    ) -> std::result::Result<Vec<u8>, AutomationError> {
        self.0
            .key
            .as_ref()
            .ok_or_else(|| RefreshError::new("key_unavailable", false))?
            .seal_release(context, plain)
            .map_err(|code| RefreshError::new(code, false))
    }
    pub(crate) fn open_release(
        &self,
        context: &str,
        envelope: &[u8],
    ) -> std::result::Result<Vec<u8>, AutomationError> {
        self.0
            .key
            .as_ref()
            .ok_or_else(|| RefreshError::new("key_unavailable", false))?
            .open_release(context, envelope)
            .map_err(|code| RefreshError::new(code, false))
    }
    pub(crate) async fn prepare_download(
        &self,
        provider_id: &str,
        revision: i64,
        target: crate::db::MediaTarget,
        source: qbittorrent::AddSource,
        recent: bool,
        options: qbittorrent::QbitOptions,
    ) -> std::result::Result<qbittorrent::PreparedDownload, AutomationError> {
        let context = &self.0;
        let (provider, _credentials) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            network_snapshot(context, provider_id),
        )
        .await
        .map_err(|_| RefreshError::new("refresh_timeout", true))?
        .map_err(refresh_error)?;
        if provider.revision != revision {
            return Err(RefreshError::new("provider_changed", false));
        }
        if !provider.enabled || !matches!(provider.settings, ProviderSettings::Qbittorrent { .. }) {
            return Err(RefreshError::new("provider_unavailable", false));
        }
        if let ProviderSettings::Qbittorrent { tv, movies, .. } = &provider.settings {
            let scope = match &target {
                crate::db::MediaTarget::Episode(_) => tv.as_ref(),
                crate::db::MediaTarget::Movie(_) => movies.as_ref(),
            };
            if scope.is_some_and(|s| s.recent_priority != s.older_priority) {
                return Err(RefreshError::new("unsupported_target", false));
            }
        }
        let transport = context
            .transport
            .as_ref()
            .as_ref()
            .map_err(|e| refresh_error(http_error(*e).0))?;
        let operation = transport
            .operation(provider.id)
            .map_err(|e| refresh_error(http_error(e).0))?;
        let work = async {
            let result = qbittorrent::prepare(
                &operation,
                &provider.settings,
                target,
                source,
                recent,
                options,
            )
            .await
            .map_err(|e| refresh_error(qbit_error(e).0));
            current_revision(&context.db, &provider)
                .await
                .map_err(refresh_error)?;
            operation
                .ensure_active()
                .map_err(|e| refresh_error(http_error(e).0))?;
            result
        };
        tokio::time::timeout_at(operation.deadline(), work)
            .await
            .map_err(|_| RefreshError::new("refresh_timeout", true))?
    }
    pub(crate) async fn submit_download(
        &self,
        provider_id: &str,
        revision: i64,
        prepared: qbittorrent::PreparedDownload,
    ) -> std::result::Result<qbittorrent::AddOutcome, AutomationError> {
        let context = &self.0;
        let (provider, credentials) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            network_snapshot(context, provider_id),
        )
        .await
        .map_err(|_| RefreshError::new("refresh_timeout", true))?
        .map_err(refresh_error)?;
        if provider.revision != revision {
            return Err(RefreshError::new("provider_changed", false));
        }
        if !provider.enabled || !matches!(provider.settings, ProviderSettings::Qbittorrent { .. }) {
            return Err(RefreshError::new("provider_unavailable", false));
        }
        let transport = context
            .transport
            .as_ref()
            .as_ref()
            .map_err(|e| refresh_error(http_error(*e).0))?;
        let operation = transport
            .operation(provider.id)
            .map_err(|e| refresh_error(http_error(e).0))?;
        let work = async {
            let result = qbittorrent::submit(
                &operation,
                &provider.settings,
                credentials.as_ref(),
                prepared,
            )
            .await
            .map_err(|e| refresh_error(qbit_error(e).0));
            current_revision(&context.db, &provider)
                .await
                .map_err(refresh_error)?;
            operation
                .ensure_active()
                .map_err(|e| refresh_error(http_error(e).0))?;
            result
        };
        tokio::time::timeout_at(operation.deadline(), work)
            .await
            .map_err(|_| RefreshError::new("refresh_timeout", true))?
    }
    pub(crate) async fn reconcile_download(
        &self,
        provider_id: &str,
        revision: i64,
        identity: &qbittorrent::SubmissionIdentity,
    ) -> std::result::Result<qbittorrent::Reconciliation, AutomationError> {
        let context = &self.0;
        let (provider, credentials) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            network_snapshot(context, provider_id),
        )
        .await
        .map_err(|_| RefreshError::new("refresh_timeout", true))?
        .map_err(refresh_error)?;
        if provider.revision != revision {
            return Err(RefreshError::new("provider_changed", false));
        }
        if !provider.enabled || !matches!(provider.settings, ProviderSettings::Qbittorrent { .. }) {
            return Err(RefreshError::new("provider_unavailable", false));
        }
        let transport = context
            .transport
            .as_ref()
            .as_ref()
            .map_err(|e| refresh_error(http_error(*e).0))?;
        let operation = transport
            .operation(provider.id)
            .map_err(|e| refresh_error(http_error(e).0))?;
        let work = async {
            let result = qbittorrent::reconcile(
                &operation,
                &provider.settings,
                credentials.as_ref(),
                identity,
            )
            .await
            .map_err(|e| refresh_error(qbit_error(e).0));
            current_revision(&context.db, &provider)
                .await
                .map_err(refresh_error)?;
            operation
                .ensure_active()
                .map_err(|e| refresh_error(http_error(e).0))?;
            result
        };
        tokio::time::timeout_at(operation.deadline(), work)
            .await
            .map_err(|_| RefreshError::new("refresh_timeout", true))?
    }
    /// Raw release facts are internal; callers must project/redact before exposing them.
    pub(crate) async fn raw_search(
        &self,
        provider_id: &str,
        revision: i64,
        operation: IndexerOperation,
        request: &indexer::IndexerSearch,
    ) -> std::result::Result<indexer::RawIndexerPage, AutomationError> {
        let context = &self.0;
        let (provider, credentials) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            network_snapshot(context, provider_id),
        )
        .await
        .map_err(|_| RefreshError::new("refresh_timeout", true))?
        .map_err(refresh_error)?;
        if provider.revision != revision {
            return Err(RefreshError::new("provider_changed", false));
        }
        if !request_policy(&provider, operation, request) {
            return Err(RefreshError::new("provider_unavailable", false));
        }
        let transport = context
            .transport
            .as_ref()
            .as_ref()
            .map_err(|e| refresh_error(http_error(*e).0))?;
        let operation = transport
            .operation(provider.id)
            .map_err(|e| refresh_error(http_error(e).0))?;
        let work = async {
            let result = indexer::raw_search(
                &operation,
                &provider.settings,
                &IndexerAccess::from_credentials(&credentials),
                request,
            )
            .await
            .map_err(|e| refresh_error(indexer_error(e, &operation).0));
            current_revision(&context.db, &provider)
                .await
                .map_err(refresh_error)?;
            operation
                .ensure_active()
                .map_err(|e| refresh_error(http_error(e).0))?;
            result
        };
        tokio::time::timeout_at(operation.deadline(), work)
            .await
            .map_err(|_| RefreshError::new("refresh_timeout", true))?
    }

    /// A bounded observation, not proof of remote absence: client pagination is not atomic.
    pub(crate) async fn refresh(
        &self,
        id: Uuid,
        revision: i64,
        domain: MediaDomain,
    ) -> std::result::Result<Vec<qbittorrent::DownloadItem>, RefreshError> {
        let context = &self.0;
        let (provider, credentials) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            network_snapshot(context, &id.to_string()),
        )
        .await
        .map_err(|_| RefreshError::new("refresh_timeout", true))?
        .map_err(refresh_error)?;
        if provider.revision != revision {
            return Err(RefreshError::new("provider_changed", false));
        }
        let scoped = match &provider.settings {
            ProviderSettings::Qbittorrent { tv, movies, .. } => match domain {
                MediaDomain::Tv => tv.is_some(),
                MediaDomain::Movies => movies.is_some(),
            },
            _ => false,
        };
        if !provider.enabled || !scoped {
            return Err(RefreshError::new("provider_unavailable", false));
        }
        let transport = context
            .transport
            .as_ref()
            .as_ref()
            .map_err(|e| refresh_error(http_error(*e).0))?;
        let operation = transport
            .operation(id)
            .map_err(|e| refresh_error(http_error(e).0))?;
        let work = async {
            let result = qbittorrent::refresh_pages(
                &operation,
                &provider.settings,
                credentials.as_ref(),
                domain,
            )
            .await
            .map_err(|e| refresh_error(qbit_error(e).0));
            // Changed configuration invalidates failed observations too.
            current_revision(&context.db, &provider)
                .await
                .map_err(refresh_error)?;
            operation
                .ensure_active()
                .map_err(|e| refresh_error(http_error(e).0))?;
            result
        };
        tokio::time::timeout_at(operation.deadline(), work)
            .await
            .map_err(|_| RefreshError::new("refresh_timeout", true))?
    }
}
fn id(value: String) -> Result<String> {
    Uuid::parse_str(&value)
        .map(|v| v.to_string())
        .map_err(|_| bad())
}
fn validate(input: &ProviderInput) -> Result<()> {
    if input.name.trim().is_empty()
        || input.name.len() > 128
        || input.name.chars().any(char::is_control)
        || !(1..=100).contains(&input.priority)
    {
        return Err(bad());
    }
    input.settings.validate()?;
    if let Change::Value(c) = &input.credentials {
        if !c.valid(input.settings.implementation()) {
            return Err(bad());
        }
    }
    Ok(())
}
fn stored_bool(value: i64) -> Result<bool> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(corrupt()),
    }
}
async fn read(conn: &Connection, id: &str) -> Result<(Provider, Option<Vec<u8>>)> {
    let row=conn.query("SELECT implementation,name,enabled,priority,revision,endpoint,credentials FROM providers WHERE id=?",[id]).await?.next().await?.ok_or_else(missing)?;
    let implementation: String = row.get(0)?;
    let endpoint: String = row.get(5)?;
    let mut scopes=conn.query("SELECT media_type,categories,anime_categories,category,imported_category,recent_priority,older_priority,anime_standard_format_search,remove_year,initial_state,content_layout,sequential_order,first_last_first,add_tags,enable_rss,enable_automatic_search,enable_interactive_search,download_client_id FROM provider_scopes WHERE provider_id=?",[id]).await?;
    let mut tv_index = None;
    let mut movie_index = None;
    let mut tv_client = None;
    let mut movie_client = None;
    while let Some(scope) = scopes.next().await? {
        let media: String = scope.get(0)?;
        if implementation == "qbittorrent" {
            let item = DownloadScope {
                category: scope.get(3)?,
                imported_category: scope.get(4)?,
                recent_priority: scope.get::<i64>(5)?.try_into().map_err(|_| corrupt())?,
                older_priority: scope.get::<i64>(6)?.try_into().map_err(|_| corrupt())?,
                initial_state: match scope.get::<String>(9)?.as_str() {
                    "started" => DownloadInitialState::Started,
                    "stopped" => DownloadInitialState::Stopped,
                    "forced" => DownloadInitialState::Forced,
                    _ => return Err(corrupt()),
                },
                content_layout: match scope.get::<String>(10)?.as_str() {
                    "default" => DownloadContentLayout::Default,
                    "original" => DownloadContentLayout::Original,
                    "subfolder" => DownloadContentLayout::Subfolder,
                    _ => return Err(corrupt()),
                },
                sequential_order: stored_bool(scope.get(11)?)?,
                first_last_first: stored_bool(scope.get(12)?)?,
                add_tags: stored_bool(scope.get(13)?)?,
            };
            if media == "tv" {
                tv_client = Some(item)
            } else {
                movie_client = Some(item)
            }
        } else {
            let cats: Vec<u32> =
                serde_json::from_str(&scope.get::<String>(1)?).map_err(|_| corrupt())?;
            if media == "tv" {
                tv_index = Some(TvIndexerScope {
                    enable_rss: stored_bool(scope.get(14)?)?,
                    enable_automatic_search: stored_bool(scope.get(15)?)?,
                    enable_interactive_search: stored_bool(scope.get(16)?)?,
                    download_client_id: scope
                        .get::<Option<String>>(17)?
                        .map(|id| Uuid::parse_str(&id).map_err(|_| corrupt()))
                        .transpose()?,
                    categories: cats,
                    anime_standard_format_search: stored_bool(scope.get::<i64>(7)?)?,
                    anime_categories: serde_json::from_str(&scope.get::<String>(2)?)
                        .map_err(|_| corrupt())?,
                })
            } else {
                movie_index = Some(MovieIndexerScope {
                    enable_rss: stored_bool(scope.get(14)?)?,
                    enable_automatic_search: stored_bool(scope.get(15)?)?,
                    enable_interactive_search: stored_bool(scope.get(16)?)?,
                    download_client_id: scope
                        .get::<Option<String>>(17)?
                        .map(|id| Uuid::parse_str(&id).map_err(|_| corrupt()))
                        .transpose()?,
                    categories: cats,
                    remove_year: stored_bool(scope.get::<i64>(8)?)?,
                })
            }
        }
    }
    let settings = match implementation.as_str() {
        "torznab" => ProviderSettings::Torznab {
            endpoint,
            tv: tv_index,
            movies: movie_index,
        },
        "newznab" => ProviderSettings::Newznab {
            endpoint,
            tv: tv_index,
            movies: movie_index,
        },
        "qbittorrent" => ProviderSettings::Qbittorrent {
            endpoint,
            tv: tv_client,
            movies: movie_client,
        },
        _ => return Err(corrupt()),
    };
    settings.validate().map_err(|_| corrupt())?;
    let credentials: Option<Vec<u8>> = row.get(6)?;
    let revision: i64 = row.get(4)?;
    let last_test=conn.query("SELECT config_revision,tested_at,status,error_code FROM provider_tests WHERE provider_id=? AND config_revision=?",params![id,revision]).await?.next().await?.map(|r|->Result<ProviderTestObservation>{Ok(ProviderTestObservation{revision:r.get(0)?,tested_at:r.get(1)?,status:match r.get::<String>(2)?.as_str(){"success"=>TestStatus::Success,"failure"=>TestStatus::Failure,_=>return Err(corrupt())},error_code:r.get(3)?})}).transpose()?;
    let test_status = last_test
        .as_ref()
        .map_or(TestStatus::NeverTested, |t| t.status);

    Ok((
        Provider {
            id: Uuid::parse_str(id).map_err(|_| corrupt())?,
            revision: row.get(4)?,
            name: row.get(1)?,
            enabled: row.get::<i64>(2)? == 1,
            priority: row.get::<i64>(3)?.try_into().map_err(|_| corrupt())?,
            settings,
            has_credentials: credentials.is_some(),
            test_supported: true,
            test_status,
            last_test,
        },
        credentials,
    ))
}
async fn write_scopes(conn: &Connection, id: &str, settings: &ProviderSettings) -> Result<()> {
    let implementation = settings.implementation();
    if let ProviderSettings::Torznab { tv, movies, .. }
    | ProviderSettings::Newznab { tv, movies, .. } = settings
    {
        for (domain, client) in [
            ("tv", tv.as_ref().and_then(|s| s.download_client_id)),
            ("movies", movies.as_ref().and_then(|s| s.download_client_id)),
        ] {
            if let Some(client) = client {
                // Existence and native domain are configuration facts at write time; a later
                // disabled or removed client stays a stored preference that health reports.
                if conn.query("SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=? AND p.implementation='qbittorrent' AND s.media_type=?", params![client.to_string(), domain]).await?.next().await?.is_none() {
                    return Err(bad());
                }
            }
        }
    }
    conn.execute("DELETE FROM provider_scopes WHERE provider_id=?", [id])
        .await?;
    match settings {
        ProviderSettings::Torznab { tv, movies, .. }
        | ProviderSettings::Newznab { tv, movies, .. } => {
            for (
                domain,
                categories,
                anime,
                standard,
                remove_year,
                rss,
                automatic,
                interactive,
                client,
            ) in tv
                .iter()
                .map(|s| {
                    (
                        "tv",
                        &s.categories,
                        s.anime_categories.as_slice(),
                        Some(i64::from(s.anime_standard_format_search)),
                        None::<i64>,
                        i64::from(s.enable_rss),
                        i64::from(s.enable_automatic_search),
                        i64::from(s.enable_interactive_search),
                        s.download_client_id.map(|id| id.to_string()),
                    )
                })
                .chain(movies.iter().map(|s| {
                    (
                        "movies",
                        &s.categories,
                        &[][..],
                        None,
                        Some(i64::from(s.remove_year)),
                        i64::from(s.enable_rss),
                        i64::from(s.enable_automatic_search),
                        i64::from(s.enable_interactive_search),
                        s.download_client_id.map(|id| id.to_string()),
                    )
                }))
            {
                let categories = serde_json::to_string(categories).map_err(|_| bad())?;
                let anime = serde_json::to_string(anime).map_err(|_| bad())?;
                conn.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year,enable_rss,enable_automatic_search,enable_interactive_search,download_client_id) VALUES(?,?,?,?,?,?,?,?,?,?,?)",params![id,implementation,domain,categories,anime,standard,remove_year,rss,automatic,interactive,client]).await?;
            }
        }
        ProviderSettings::Qbittorrent { tv, movies, .. } => {
            for (domain, s) in tv
                .iter()
                .map(|s| ("tv", s))
                .chain(movies.iter().map(|s| ("movies", s)))
            {
                conn.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,imported_category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",params![id,implementation,domain,s.category.clone(),s.imported_category.clone(),i64::from(s.recent_priority),i64::from(s.older_priority),match s.initial_state { DownloadInitialState::Started => "started", DownloadInitialState::Stopped => "stopped", DownloadInitialState::Forced => "forced" },match s.content_layout { DownloadContentLayout::Default => "default", DownloadContentLayout::Original => "original", DownloadContentLayout::Subfolder => "subfolder" },i64::from(s.sequential_order),i64::from(s.first_last_first),i64::from(s.add_tags)]).await?;
            }
        }
    }
    Ok(())
}
fn credentials(
    context: &Context,
    id: &str,
    implementation: &str,
    old: Option<Vec<u8>>,
    change: &Change<Credentials>,
) -> Result<Option<Vec<u8>>> {
    // Authenticate before *any* mutation of a credential-bearing row: a wrong key cannot erase it.
    if let Some(bytes) = &old {
        context
            .key
            .as_ref()
            .ok_or_else(locked)?
            .open(id, implementation, bytes)
            .map_err(|_| locked())?;
    }
    match change {
        Change::Missing => Ok(old),
        Change::Null => Ok(None),
        Change::Value(c) => context
            .key
            .as_ref()
            .ok_or_else(locked)?
            .seal(id, implementation, c)
            .map(Some)
            .map_err(|_| locked()),
    }
}
// One collector per writer transaction deduplicates bulk/import invalidation. The old and new
// scope union and the indexer references to a client are captured before replacement/deletion.
#[derive(Default)]
pub(crate) struct HealthConfigurationChanges {
    // Indexer edits affect capability checks (0048) and the download-client binding check.
    indexers: [bool; 2],
    // Client edits affect client checks; the binding check is marked through `referenced`.
    clients: [bool; 2],
    // Domains where some indexer references the changed client; only the binding check applies.
    referenced: [bool; 2],
}
impl HealthConfigurationChanges {
    async fn capture(
        &mut self,
        c: &Connection,
        id: &str,
        old: Option<&ProviderSettings>,
        new: Option<&ProviderSettings>,
    ) -> Result<()> {
        let mut client = false;
        for settings in [old, new].into_iter().flatten() {
            let (selected, tv, movies) = match settings {
                ProviderSettings::Torznab { tv, movies, .. }
                | ProviderSettings::Newznab { tv, movies, .. } => {
                    (&mut self.indexers, tv.is_some(), movies.is_some())
                }
                ProviderSettings::Qbittorrent { tv, movies, .. } => {
                    client = true;
                    (&mut self.clients, tv.is_some(), movies.is_some())
                }
            };
            selected[0] |= tv;
            selected[1] |= movies;
        }
        if client {
            // The client may already lack a referenced domain, so read references before mutation.
            let mut rows = c.query("SELECT DISTINCT media_type FROM provider_scopes WHERE implementation IN ('torznab','newznab') AND download_client_id=?", [id]).await?;
            while let Some(row) = rows.next().await? {
                match row.get::<String>(0)?.as_str() {
                    "tv" => self.referenced[0] = true,
                    "movies" => self.referenced[1] = true,
                    _ => return Err(corrupt()),
                }
            }
        }
        Ok(())
    }
    async fn mark(self, c: &Connection) -> Result<()> {
        let recorded = |e: crate::health::Error| {
            Error::Plain(e.0, e.1, "Health configuration could not be recorded")
        };
        for (i, domain) in [MediaDomain::Tv, MediaDomain::Movies]
            .into_iter()
            .enumerate()
        {
            if self.indexers[i] {
                crate::health::indexer_configuration_changed(c, domain)
                    .await
                    .map_err(recorded)?;
            }
            if self.indexers[i] || self.referenced[i] {
                crate::health::indexer_client_configuration_changed(c, domain)
                    .await
                    .map_err(recorded)?;
            }
            if self.clients[i] {
                crate::health::provider_configuration_changed(c, domain)
                    .await
                    .map_err(recorded)?;
            }
        }
        Ok(())
    }
    pub(crate) async fn finish(self, c: &Connection) -> std::result::Result<(), &'static str> {
        self.mark(c)
            .await
            .map_err(|_| "provider health configuration could not be recorded")
    }
}
async fn create(
    State(context): State<Context>,
    input: std::result::Result<Json<ProviderInput>, JsonRejection>,
) -> Result<(StatusCode, Json<Provider>)> {
    let input = input.map_err(|_| bad())?.0;
    validate(&input)?;
    let id = Uuid::new_v4().to_string();
    let secret = credentials(
        &context,
        &id,
        input.settings.implementation(),
        None,
        &input.credentials,
    )?;
    let conn = context.db.connect().await?;
    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
    let mut changes = HealthConfigurationChanges::default();
    changes.capture(&tx, &id, None, Some(&input.settings)).await?;
    tx.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint,credentials) VALUES(?,?,?,?,?,1,1,?,?)",params![id.clone(),input.settings.implementation(),input.name,i64::from(input.enabled),i64::from(input.priority),input.settings.endpoint(),secret]).await?;
    write_scopes(&tx, &id, &input.settings).await?;
    crate::completed_download_handling::reconcile(&tx, Some(&id), None).await.map_err(|e| Error::Plain(e.0,e.1,"Completed download handling reconciliation failed"))?;
    changes.mark(&tx).await?;
    Ok((StatusCode::CREATED, bounded(read(&tx, &id).await?.0)?))
    }.await;
    finish(tx, outcome).await
}
async fn update(
    State(context): State<Context>,
    Path(value): Path<String>,
    input: std::result::Result<Json<ProviderUpdate>, JsonRejection>,
) -> Result<Json<Provider>> {
    let id = id(value)?;
    let input = input.map_err(|_| bad())?.0;
    validate(&input.config)?;
    if !(1..9007199254740991).contains(&input.revision) {
        return Err(bad());
    }
    let conn = context.db.connect().await?;
    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
    let (old, secret) = read(&tx, &id).await?;
    if old.revision != input.revision
        || old.settings.implementation() != input.config.settings.implementation()
    {
        return Err(conflict());
    }
    let secret = credentials(
        &context,
        &id,
        old.settings.implementation(),
        secret,
        &input.config.credentials,
    )?;
    release_profile_references(&tx,&id,Some(&input.config.settings)).await?;
    let mut changes = HealthConfigurationChanges::default();
    changes.capture(&tx, &id, Some(&old.settings), Some(&input.config.settings)).await?;
    let changed=tx.execute("UPDATE providers SET name=?,enabled=?,priority=?,revision=revision+1,endpoint=?,credentials=? WHERE id=? AND revision=?",params![input.config.name,i64::from(input.config.enabled),i64::from(input.config.priority),input.config.settings.endpoint(),secret,id.clone(),input.revision]).await?;
    if changed != 1 {
        return Err(conflict());
    }
    write_scopes(&tx, &id, &input.config.settings).await?;
    crate::completed_download_handling::reconcile(&tx, Some(&id), None).await.map_err(|e| Error::Plain(e.0,e.1,"Completed download handling reconciliation failed"))?;
    changes.mark(&tx).await?;
    bounded(read(&tx, &id).await?.0)
    }.await;
    finish(tx, outcome).await
}
async fn detail(
    State(context): State<Context>,
    Path(value): Path<String>,
) -> Result<Json<Provider>> {
    let id = id(value)?;
    let conn = context.db.connect().await?;
    let tx = conn.transaction().await?;
    let result = read(&tx, &id).await?.0;
    tx.commit().await?;
    bounded(result)
}
async fn list(
    State(context): State<Context>,
    query: std::result::Result<Query<ProviderQuery>, QueryRejection>,
) -> Result<Json<ApiPage<Provider>>> {
    let query = query.map_err(|_| bad())?.0;
    if !(1..=100).contains(&query.limit) {
        return Err(bad());
    }
    let domain = query.media_type.map(|d| match d {
        MediaDomain::Tv => "tv",
        MediaDomain::Movies => "movies",
    });
    let conn = context.db.connect().await?;
    let tx = conn.transaction().await?;
    let total=tx.query("SELECT count(*) FROM providers p WHERE ? IS NULL OR EXISTS(SELECT 1 FROM provider_scopes s WHERE s.provider_id=p.id AND s.media_type=?)",params![domain,domain]).await?.next().await?.ok_or_else(corrupt)?.get(0)?;
    let mut rows=tx.query("SELECT p.id FROM providers p WHERE ? IS NULL OR EXISTS(SELECT 1 FROM provider_scopes s WHERE s.provider_id=p.id AND s.media_type=?) ORDER BY p.priority,p.name,p.id LIMIT ? OFFSET ?",params![domain,domain,i64::from(query.limit),i64::from(query.offset)]).await?;
    let mut items = Vec::new();
    while let Some(row) = rows.next().await? {
        items.push(read(&tx, &row.get::<String>(0)?).await?.0);
    }
    tx.commit().await?;
    bounded(ApiPage {
        items,
        total,
        limit: query.limit,
        offset: query.offset,
    })
}
async fn release_profile_references(
    c: &Connection,
    id: &str,
    settings: Option<&ProviderSettings>,
) -> Result<()> {
    crate::release_profiles::provider_scopes_allowed(c, id, settings)
        .await
        .map_err(|error| {
            if error.code() == "provider_in_use" {
                Error::Plain(
                    StatusCode::CONFLICT,
                    "provider_in_use",
                    "Unassign release profiles before removing this provider scope",
                )
            } else {
                Error::Plain(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "release_profile_storage_error",
                    "Release profile references could not be checked",
                )
            }
        })
}
async fn delete(
    State(context): State<Context>,
    Path(value): Path<String>,
    query: std::result::Result<Query<ProviderRevision>, QueryRejection>,
) -> Result<StatusCode> {
    let id = id(value)?;
    let revision = query.map_err(|_| bad())?.0.revision;
    let conn = context.db.connect().await?;
    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        let (old, secret) = read(&tx, &id).await?;
        if old.revision != revision {
            return Err(conflict());
        }
        credentials(
            &context,
            &id,
            old.settings.implementation(),
            secret,
            &Change::Null,
        )?;
        release_profile_references(&tx, &id, None).await?;
        let mut changes = HealthConfigurationChanges::default();
        changes.capture(&tx, &id, Some(&old.settings), None).await?;
        if tx
            .execute(
                "DELETE FROM providers WHERE id=? AND revision=?",
                params![id, revision],
            )
            .await?
            != 1
        {
            return Err(conflict());
        }
        crate::completed_download_handling::reconcile_pending(&tx)
            .await
            .map_err(|e| {
                Error::Plain(
                    e.0,
                    e.1,
                    "Completed download handling reconciliation failed",
                )
            })?;
        changes.mark(&tx).await?;
        Ok(StatusCode::NO_CONTENT)
    }
    .await;
    finish(tx, outcome).await
}
// Bound serialization too, so future configuration fields cannot silently expand responses.
fn bounded<T: Serialize>(value: T) -> Result<Json<T>> {
    const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
    if serde_json::to_vec(&value).map_err(|_| corrupt())?.len() > MAX_RESPONSE_BYTES {
        return Err(Error::Plain(
            StatusCode::INTERNAL_SERVER_ERROR,
            "provider_response_too_large",
            "Provider response exceeds the size limit",
        ));
    }
    Ok(Json(value))
}
async fn finish<T>(tx: libsql::Transaction, outcome: Result<T>) -> Result<T> {
    match outcome {
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

#[cfg(test)]
mod tests {
    #[test]
    fn download_root_namespace_requires_posix_or_explicit_mapping() {
        assert_eq!(super::local_download_root("/"), Some("/".into()));
        assert_eq!(
            super::local_download_root("/media//downloads/"),
            Some("/media/downloads".into())
        );
        for invalid in [
            "//server/share",
            "\\\\server\\share",
            "C:/downloads",
            "relative",
            "/a/../b",
            "/a\0b",
        ] {
            assert_eq!(super::local_download_root(invalid), None, "{invalid}");
        }
    }
    use super::*;
    #[test]
    fn operation_policy_requires_intent_global_scope_and_strict_flags() {
        let value = serde_json::json!({"implementation":"torznab","endpoint":"https://example.test/api","tv":{"categories":[5030],"anime_categories":[],"enable_rss":false},"movies":{"categories":[2000],"enable_automatic_search":false,"enable_interactive_search":false}});
        let settings: ProviderSettings = serde_json::from_value(value.clone()).unwrap();
        let mut provider = Provider {
            id: Uuid::nil(),
            revision: 1,
            name: "policy".into(),
            enabled: true,
            priority: 1,
            settings,
            has_credentials: false,
            test_supported: true,
            test_status: TestStatus::NeverTested,
            last_test: None,
        };
        for (domain, expected) in [
            (MediaDomain::Tv, [false, true, true]),
            (MediaDomain::Movies, [true, false, false]),
        ] {
            for (operation, expected) in [
                IndexerOperation::Rss,
                IndexerOperation::Automatic,
                IndexerOperation::Interactive,
            ]
            .into_iter()
            .zip(expected)
            {
                assert_eq!(
                    indexer_operation_enabled(&provider, domain, operation),
                    expected
                );
            }
        }
        let rss = indexer::IndexerSearch::Rss {
            media_type: MediaDomain::Movies,
            offset: 0,
            query_index: 0,
            limit: 10,
        };
        assert!(request_policy(&provider, IndexerOperation::Rss, &rss));
        assert!(!request_policy(
            &provider,
            IndexerOperation::Interactive,
            &rss
        ));
        assert!(!request_policy(
            &provider,
            IndexerOperation::Automatic,
            &rss
        ));
        let tv: indexer::IndexerSearch = serde_json::from_value(serde_json::json!({"kind":"tv","title":"Show","numbering":{"kind":"episode","season":1,"episode":1}})).unwrap();
        assert!(!request_policy(&provider, IndexerOperation::Rss, &tv));
        assert!(request_policy(&provider, IndexerOperation::Automatic, &tv));
        provider.enabled = false;
        assert!(!request_policy(&provider, IndexerOperation::Rss, &rss));
        provider.enabled = true;
        if let ProviderSettings::Torznab { movies, .. } = &mut provider.settings {
            *movies = None;
        }
        assert!(!request_policy(&provider, IndexerOperation::Rss, &rss));
        provider.settings = serde_json::from_value(serde_json::json!({"implementation":"qbittorrent","endpoint":"https://example.test","tv":{"category":"tv","imported_category":null,"recent_priority":0,"older_priority":0},"movies":null})).unwrap();
        assert!(!request_policy(&provider, IndexerOperation::Automatic, &tv));
        for invalid in [
            serde_json::Value::Null,
            serde_json::json!(1),
            serde_json::json!("true"),
        ] {
            let mut malformed = value.clone();
            malformed["tv"]["enable_rss"] = invalid;
            assert!(serde_json::from_value::<ProviderSettings>(malformed).is_err());
        }
    }
    #[test]
    fn configuration_wire_and_domain_validation_are_closed() {
        let input = serde_json::json!({"revision":1,"name":"Indexer","enabled":true,"priority":1,"settings":{"implementation":"newznab","endpoint":"https://example.invalid/api","tv":null,"movies":{"categories":[2147483647]}}});
        let update: ProviderUpdate = serde_json::from_value(input.clone()).unwrap();
        validate(&update.config).unwrap();
        assert!(matches!(update.config.credentials, Change::Missing));
        for (field, value) in [
            ("credentials", serde_json::Value::Null),
            (
                "credentials",
                serde_json::json!({"kind":"api_key","api_key":"valid"}),
            ),
        ] {
            let mut value_input = input.clone();
            value_input[field] = value;
            let parsed: ProviderUpdate = serde_json::from_value(value_input).unwrap();
            validate(&parsed.config).unwrap();
        }
        let mut unknown = input.clone();
        unknown["ignored"] = true.into();
        assert!(serde_json::from_value::<ProviderUpdate>(unknown).is_err());
        let mut absent = input.clone();
        absent["settings"].as_object_mut().unwrap().remove("tv");
        assert!(
            serde_json::from_value::<ProviderUpdate>(absent).is_err(),
            "Generated required-nullable scopes match actual deserialization"
        );
        for endpoint in [
            " https://example.invalid/api",
            "https://example.invalid/api ",
            r"https://example.invalid\api",
            "https://key@example.invalid/api",
            "https://example.invalid/api?apikey=secret",
            "https://example.invalid/api#secret",
        ] {
            let mut invalid = input.clone();
            invalid["settings"]["endpoint"] = endpoint.into();
            assert!(
                validate(
                    &serde_json::from_value::<ProviderUpdate>(invalid)
                        .unwrap()
                        .config
                )
                .is_err()
            );
        }
        // qBittorrent allows nested categories, but not malformed separators (both reference validators).
        for (category, valid) in [
            ("tv/anime", true),
            ("/tv", false),
            ("tv/", false),
            ("tv//anime", false),
            (r"tv\anime", false),
        ] {
            let settings = ProviderSettings::Qbittorrent {
                endpoint: "https://example.invalid".into(),
                tv: Some(DownloadScope {
                    category: category.into(),
                    imported_category: None,
                    recent_priority: 0,
                    older_priority: 1,
                    ..Default::default()
                }),
                movies: None,
            };
            assert_eq!(settings.validate().is_ok(), valid);
        }
        // Migration rejects conflicting hierarchies before startup; stored and incoming settings use the same rule.
        let legacy = ProviderSettings::Qbittorrent {
            endpoint: "https://example.invalid".into(),
            tv: Some(DownloadScope {
                category: "media".into(),
                ..Default::default()
            }),
            movies: Some(DownloadScope {
                category: "media/movies".into(),
                ..Default::default()
            }),
        };
        assert!(legacy.validate().is_err());
        // Upstream category IDs are signed integers, not an invented six-digit ceiling.
        let mut invalid = input;
        invalid["settings"]["movies"]["categories"] = serde_json::json!([2147483648u32]);
        assert!(
            validate(
                &serde_json::from_value::<ProviderUpdate>(invalid)
                    .unwrap()
                    .config
            )
            .is_err()
        );
    }
}

fn unsupported_operation() -> Error {
    Error::Plain(
        StatusCode::UNPROCESSABLE_ENTITY,
        "unsupported",
        "Provider does not support this operation",
    )
}
async fn network_snapshot(context: &Context, id: &str) -> Result<(Provider, Option<Credentials>)> {
    let conn = context.db.connect().await?;
    let tx = conn.transaction().await?;
    let (provider, bytes) = read(&tx, id).await?;
    tx.commit().await?;
    let credentials = unlock(context, &provider, bytes)?;
    Ok((provider, credentials))
}
fn unlock(
    context: &Context,
    provider: &Provider,
    bytes: Option<Vec<u8>>,
) -> Result<Option<Credentials>> {
    let credentials = bytes
        .map(|bytes| {
            context
                .key
                .as_ref()
                .ok_or_else(locked)?
                .open(
                    &provider.id.to_string(),
                    provider.settings.implementation(),
                    &bytes,
                )
                .map_err(|_| locked())
        })
        .transpose()?;
    if credentials
        .as_ref()
        .is_some_and(|c| !c.valid(provider.settings.implementation()))
    {
        return Err(locked());
    }
    Ok(credentials)
}

fn http_error(error: http::HttpError) -> (Error, &'static str) {
    use http::HttpError as H;
    match error {
        H::InvalidRequest => (bad(), "invalid_request"),
        H::LocalUnavailable { timeout } => {
            http_error(if timeout { H::Timeout } else { H::Transport })
        }
        H::Authentication => (
            Error::Plain(
                StatusCode::BAD_GATEWAY,
                "authentication",
                "Provider rejected authentication",
            ),
            "authentication",
        ),
        H::RateLimited {
            retry_after_seconds,
        } => (
            Error::RateLimited(retry_after_seconds.unwrap_or(60)),
            "rate_limited",
        ),
        H::Busy => (
            Error::Plain(
                StatusCode::TOO_MANY_REQUESTS,
                "provider_busy",
                "Provider operation capacity is busy; retry later",
            ),
            "rate_limited",
        ),
        H::Timeout => (
            Error::Plain(
                StatusCode::GATEWAY_TIMEOUT,
                "timeout",
                "Provider operation timed out",
            ),
            "timeout",
        ),
        H::Redirect => (
            Error::Plain(
                StatusCode::BAD_GATEWAY,
                "redirect_rejected",
                "Provider redirect was rejected",
            ),
            "redirect_rejected",
        ),
        H::ResponseTooLarge => (
            Error::Plain(
                StatusCode::BAD_GATEWAY,
                "response_too_large",
                "Provider response exceeds the size limit",
            ),
            "response_too_large",
        ),
        H::InvalidResponse => (
            Error::Plain(
                StatusCode::BAD_GATEWAY,
                "invalid_response",
                "Provider response is invalid",
            ),
            "invalid_response",
        ),
        H::Transport => (
            Error::Plain(
                StatusCode::BAD_GATEWAY,
                "transport_error",
                "Provider connection failed",
            ),
            "transport_error",
        ),
    }
}
fn indexer_error(
    error: indexer::IndexerError,
    operation: &http::HttpOperation<'_>,
) -> (Error, &'static str) {
    use indexer::IndexerError as I;
    match error {
        I::InvalidRequest => (bad(), "invalid_request"),
        I::Unsupported => (
            Error::Plain(
                StatusCode::UNPROCESSABLE_ENTITY,
                "unsupported",
                "Provider does not support the requested scope or search",
            ),
            "unsupported",
        ),
        I::Authentication => http_error(http::HttpError::Authentication),
        I::RateLimited {
            retry_after_seconds,
        } => http_error(operation.rate_limit(retry_after_seconds)),
        I::InvalidResponse => http_error(http::HttpError::InvalidResponse),
        I::Transport(error) => http_error(error),
    }
}
async fn current_revision(db: &Database, provider: &Provider) -> Result<()> {
    let conn = db.connect().await?;
    if conn
        .query(
            "SELECT 1 FROM providers WHERE id=? AND revision=?",
            params![provider.id.to_string(), provider.revision],
        )
        .await?
        .next()
        .await?
        .is_none()
    {
        return Err(conflict());
    }
    Ok(())
}
async fn record_test(
    db: &Database,
    provider: &Provider,
    code: Option<&'static str>,
) -> Result<i64> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| corrupt())?
        .as_secs();
    let tested_at = i64::try_from(now).map_err(|_| corrupt())?;
    let conn = db.connect().await?;
    let status = if code.is_none() { "success" } else { "failure" };
    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    let previous = tx.query("SELECT status,error_code FROM provider_tests WHERE provider_id=? AND config_revision=?", params![provider.id.to_string(),provider.revision]).await?.next().await?
        .map(|row| -> Result<(String,Option<String>)> { Ok((row.get(0)?,row.get(1)?)) }).transpose()?;
    let changed=tx.execute("INSERT INTO provider_tests(provider_id,config_revision,tested_at,status,error_code) SELECT id,revision,?,?,? FROM providers WHERE id=? AND revision=? ON CONFLICT(provider_id) DO UPDATE SET config_revision=excluded.config_revision,tested_at=excluded.tested_at,status=excluded.status,error_code=excluded.error_code",params![tested_at,status,code,provider.id.to_string(),provider.revision]).await?;
    if changed != 1 {
        return Err(conflict());
    }
    let different = previous
        .as_ref()
        .is_none_or(|(old_status, old_code)| old_status != status || old_code.as_deref() != code);
    if different {
        if let ProviderSettings::Qbittorrent { tv, movies, .. } = &provider.settings {
            for (selected, domain) in [
                (tv.is_some(), MediaDomain::Tv),
                (movies.is_some(), MediaDomain::Movies),
            ] {
                if selected {
                    crate::health::communication_status_changed(&tx, domain)
                        .await
                        .map_err(|e| {
                            Error::Plain(e.0, e.1, "Health observation could not be recorded")
                        })?;
                }
            }
        }
    }
    tx.commit().await?;
    eprintln!(
        "{}",
        serde_json::json!({"level":if code.is_none(){"INFO"}else{"ERROR"},"event":"provider_test_completed","provider_id":provider.id,"revision":provider.revision,"result":status,"error_code":code,"correlation_id":Uuid::new_v4()})
    );
    Ok(tested_at)
}
async fn test(
    State(context): State<Context>,
    Path(value): Path<String>,
) -> Result<Json<ProviderTestResult>> {
    let id = id(value)?;
    let (provider, credentials) = network_snapshot(&context, &id).await?;
    run_test(&context, provider, credentials).await
}
async fn probe(
    operation: &http::HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: &Option<Credentials>,
) -> std::result::Result<ProviderTestOutcome, (Error, &'static str)> {
    if matches!(settings, ProviderSettings::Qbittorrent { .. }) {
        qbittorrent::test_connection(operation, settings, credentials.as_ref())
            .await
            .map(ProviderTestOutcome::DownloadClient)
            .map_err(qbit_error)
    } else {
        indexer::test(
            operation,
            settings,
            &IndexerAccess::from_credentials(credentials),
        )
        .await
        .map(ProviderTestOutcome::Indexer)
        .map_err(|e| indexer_error(e, operation))
    }
}
async fn run_test(
    context: &Context,
    provider: Provider,
    credentials: Option<Credentials>,
) -> Result<Json<ProviderTestResult>> {
    let transport = context
        .transport
        .as_ref()
        .as_ref()
        .map_err(|e| http_error(*e).0)?;
    let operation = transport
        .operation(provider.id)
        .map_err(|e| http_error(e).0)?;
    let work = async {
        let result = probe(&operation, &provider.settings, &credentials).await;
        operation.ensure_active().map_err(|e| http_error(e).0)?;
        match result {
            Ok(result) => {
                let tested_at = record_test(&context.db, &provider, None).await?;
                bounded(ProviderTestResult {
                    provider_id: provider.id,
                    revision: provider.revision,
                    tested_at,
                    result,
                })
            }
            Err(error) => {
                let (error, code) = error;
                record_test(&context.db, &provider, Some(code)).await?;
                Err(error)
            }
        }
    };
    tokio::time::timeout_at(operation.deadline(), work)
        .await
        .map_err(|_| http_error(http::HttpError::Timeout).0)?
}
async fn search(
    State(context): State<Context>,
    Path(value): Path<String>,
    input: std::result::Result<Json<indexer::IndexerSearch>, JsonRejection>,
) -> Result<Json<ProviderSearchResult>> {
    let id = id(value)?;
    let input = input.map_err(|_| bad())?.0;
    let (provider, credentials) = network_snapshot(&context, &id).await?;
    if matches!(provider.settings, ProviderSettings::Qbittorrent { .. }) {
        return Err(unsupported_operation());
    }
    let intent = match &input {
        indexer::IndexerSearch::Rss { .. } => IndexerOperation::Rss,
        _ => IndexerOperation::Interactive,
    };
    if !request_policy(&provider, intent, &input) {
        return Err(Error::Plain(
            StatusCode::CONFLICT,
            "provider_unavailable",
            "Provider operation is disabled or out of scope",
        ));
    }
    let transport = context
        .transport
        .as_ref()
        .as_ref()
        .map_err(|e| http_error(*e).0)?;
    let operation = transport
        .operation(provider.id)
        .map_err(|e| http_error(e).0)?;
    let work = async {
        let result = indexer::search(
            &operation,
            &provider.settings,
            &IndexerAccess::from_credentials(&credentials),
            &input,
        )
        .await;
        operation.ensure_active().map_err(|e| http_error(e).0)?;
        current_revision(&context.db, &provider).await?;
        bounded(ProviderSearchResult {
            provider_id: provider.id,
            revision: provider.revision,
            page: result.map_err(|e| indexer_error(e, &operation).0)?,
        })
    };
    tokio::time::timeout_at(operation.deadline(), work)
        .await
        .map_err(|_| http_error(http::HttpError::Timeout).0)?
}

fn qbit_error(error: qbittorrent::QbitError) -> (Error, &'static str) {
    use qbittorrent::QbitError as Q;
    match error {
        Q::Http(e) => http_error(e),
        Q::InvalidRequest | Q::ScopeConflict => (bad(), "invalid_request"),
        Q::UnsafeRetention => (
            Error::Plain(
                StatusCode::UNPROCESSABLE_ENTITY,
                "unsafe_retention",
                "Client is configured to remove downloads before completed download handling can retain them",
            ),
            "unsupported",
        ),
        Q::UnsupportedVersion | Q::UnsupportedFeature => (
            Error::Plain(
                StatusCode::UNPROCESSABLE_ENTITY,
                "unsupported",
                "Client does not support the requested operation",
            ),
            "unsupported",
        ),
        Q::NotFound => (
            Error::Plain(
                StatusCode::NOT_FOUND,
                "not_found",
                "Download was not found in the requested scope",
            ),
            "invalid_request",
        ),
        Q::InvalidResponse | Q::Rejected | Q::MutationUnknown => {
            http_error(http::HttpError::InvalidResponse)
        }
    }
}

async fn downloads(
    State(context): State<Context>,
    Path(value): Path<String>,
    input: std::result::Result<Json<qbittorrent::DownloadQuery>, JsonRejection>,
) -> Result<Json<ProviderDownloadResult>> {
    let id = id(value)?;
    let input = input.map_err(|_| bad())?.0;
    let (provider, credentials) = network_snapshot(&context, &id).await?;
    if !matches!(provider.settings, ProviderSettings::Qbittorrent { .. }) {
        return Err(unsupported_operation());
    }
    let transport = context
        .transport
        .as_ref()
        .as_ref()
        .map_err(|e| http_error(*e).0)?;
    let operation = transport
        .operation(provider.id)
        .map_err(|e| http_error(e).0)?;
    let work = async {
        let result =
            qbittorrent::query(&operation, &provider.settings, credentials.as_ref(), &input).await;
        operation.ensure_active().map_err(|e| http_error(e).0)?;
        current_revision(&context.db, &provider).await?;
        bounded(ProviderDownloadResult {
            provider_id: provider.id,
            revision: provider.revision,
            page: result.map_err(|e| qbit_error(e).0)?,
        })
    };
    tokio::time::timeout_at(operation.deadline(), work)
        .await
        .map_err(|_| http_error(http::HttpError::Timeout).0)?
}

async fn files(
    State(context): State<Context>,
    Path(value): Path<String>,
    input: std::result::Result<Json<qbittorrent::DownloadFilesQuery>, JsonRejection>,
) -> Result<Json<ProviderFilesResult>> {
    let id = id(value)?;
    let input = input.map_err(|_| bad())?.0;
    let (provider, credentials) = network_snapshot(&context, &id).await?;
    if !matches!(provider.settings, ProviderSettings::Qbittorrent { .. }) {
        return Err(unsupported_operation());
    }
    let transport = context
        .transport
        .as_ref()
        .as_ref()
        .map_err(|e| http_error(*e).0)?;
    let operation = transport
        .operation(provider.id)
        .map_err(|e| http_error(e).0)?;
    let work = async {
        let result =
            qbittorrent::files(&operation, &provider.settings, credentials.as_ref(), &input).await;
        operation.ensure_active().map_err(|e| http_error(e).0)?;
        current_revision(&context.db, &provider).await?;
        bounded(ProviderFilesResult {
            provider_id: provider.id,
            revision: provider.revision,
            result: result.map_err(|e| qbit_error(e).0)?,
        })
    };
    tokio::time::timeout_at(operation.deadline(), work)
        .await
        .map_err(|_| http_error(http::HttpError::Timeout).0)?
}

/// Snapshot writer supplies the transaction; no updates, network calls or independent commit.
pub(crate) async fn import_configuration(
    conn: &Connection,
    key: Option<&CredentialKey>,
    input: &ProviderInput,
    download_client_id: Option<Uuid>,
    changes: &mut HealthConfigurationChanges,
    mapped: Option<(&str, i64)>,
) -> std::result::Result<Option<(String, i64, bool)>, &'static str> {
    const INVALID: &str = "invalid source provider configuration";
    const FAILED: &str = "provider reconstruction failed; transaction not committed";
    const LOCKED: &str = "provider reconstruction requires the matching credential key";
    validate(input).map_err(|_| INVALID)?;
    let mut settings = input.settings.clone();
    if let ProviderSettings::Torznab { tv, movies, .. }
    | ProviderSettings::Newznab { tv, movies, .. } = &mut settings
    {
        if let Some(scope) = tv {
            scope.download_client_id = download_client_id;
        }
        if let Some(scope) = movies {
            scope.download_client_id = download_client_id;
        }
    } else if download_client_id.is_some() {
        return Err(INVALID);
    }
    if input.enabled {
        return Err(INVALID);
    }
    let implementation = settings.implementation();
    let wanted_secret = match &input.credentials {
        Change::Value(value) => Some(value),
        Change::Null => None,
        Change::Missing => return Err(INVALID),
    };
    if wanted_secret.is_some() && key.is_none() {
        return Err(LOCKED);
    }
    let domain = match &settings {
        ProviderSettings::Torznab {
            tv: Some(_),
            movies: None,
            ..
        }
        | ProviderSettings::Newznab {
            tv: Some(_),
            movies: None,
            ..
        }
        | ProviderSettings::Qbittorrent {
            tv: Some(_),
            movies: None,
            ..
        } => "tv",
        ProviderSettings::Torznab {
            tv: None,
            movies: Some(_),
            ..
        }
        | ProviderSettings::Newznab {
            tv: None,
            movies: Some(_),
            ..
        }
        | ProviderSettings::Qbittorrent {
            tv: None,
            movies: Some(_),
            ..
        } => "movies",
        _ => return Err(INVALID),
    };
    // Endpoint + implementation + source domain is a conservative reconciliation identity.
    // Never merge independent TV/movie configurations or pick among ambiguous candidates.
    let mut rows = if let Some((id, _)) = mapped {
        conn.query("SELECT id FROM providers WHERE id=?", [id])
            .await
            .map_err(|_| FAILED)?
    } else {
        conn.query("SELECT p.id FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.implementation=? AND p.endpoint=? AND s.media_type=? LIMIT 2", params![implementation,settings.endpoint(),domain]).await.map_err(|_| FAILED)?
    };
    let candidate = rows
        .next()
        .await
        .map_err(|_| FAILED)?
        .map(|row| row.get::<String>(0).map_err(|_| FAILED))
        .transpose()?;
    if candidate.is_some() && rows.next().await.map_err(|_| FAILED)?.is_some() {
        return Ok(None);
    }
    drop(rows);
    if let Some(id) = candidate {
        let (existing, cipher) = read(conn, &id).await.map_err(|_| FAILED)?;
        let secret = cipher
            .as_ref()
            .map(|bytes| {
                key.ok_or(LOCKED)?
                    .open(&id, existing.settings.implementation(), bytes)
                    .map_err(|_| LOCKED)
            })
            .transpose()?;
        let equals = existing.name == input.name
            && !existing.enabled
            && existing.priority == input.priority
            && existing.last_test.is_none()
            && mapped.is_none_or(|(_, revision)| existing.revision == revision)
            && serde_json::to_value(&existing.settings).map_err(|_| FAILED)?
                == serde_json::to_value(&settings).map_err(|_| FAILED)?
            && serde_json::to_value(&secret).map_err(|_| FAILED)?
                == serde_json::to_value(wanted_secret).map_err(|_| FAILED)?;
        return Ok(equals.then_some((id, existing.revision, false)));
    }
    if mapped.is_some() {
        return Ok(None);
    }
    let id = Uuid::new_v4().to_string();
    let encrypted = wanted_secret
        .map(|value| {
            key.ok_or(LOCKED)?
                .seal(&id, implementation, value)
                .map_err(|_| LOCKED)
        })
        .transpose()?;
    changes
        .capture(conn, &id, None, Some(&settings))
        .await
        .map_err(|_| FAILED)?;
    conn.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint,credentials) VALUES(?,?,?,0,?,1,1,?,?)",params![id.clone(),implementation,input.name.clone(),i64::from(input.priority),settings.endpoint(),encrypted]).await.map_err(|_| FAILED)?;
    write_scopes(conn, &id, &settings)
        .await
        .map_err(|_| FAILED)?;
    crate::completed_download_handling::reconcile(conn, Some(&id), None)
        .await
        .map_err(|_| FAILED)?;
    Ok(Some((id, 1, true)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathPreviewQuery {}

async fn path_preview(
    State(context): State<Context>,
    Path(value): Path<String>,
    query: std::result::Result<Query<PathPreviewQuery>, QueryRejection>,
    input: std::result::Result<Json<crate::remote_paths::ProviderPathInput>, JsonRejection>,
) -> std::result::Result<Json<crate::remote_paths::ProviderPathPreview>, Response> {
    let id = id(value).map_err(IntoResponse::into_response)?;
    query.map_err(|_| bad().into_response())?;
    let input = input.map_err(|_| bad().into_response())?.0;
    let c = context
        .db
        .connect()
        .await
        .map_err(|_| corrupt().into_response())?;
    let tx = c
        .transaction()
        .await
        .map_err(|_| corrupt().into_response())?;
    let (provider, _) = read(&tx, &id).await.map_err(IntoResponse::into_response)?;
    let ProviderSettings::Qbittorrent {
        endpoint,
        tv,
        movies,
    } = &provider.settings
    else {
        return Err(bad().into_response());
    };
    if match input.media_type {
        MediaDomain::Tv => tv.is_none(),
        MediaDomain::Movies => movies.is_none(),
    } {
        return Err(bad().into_response());
    }
    let url = url::Url::parse(endpoint).map_err(|_| bad().into_response())?;
    let host = url
        .host_str()
        .ok_or_else(|| bad().into_response())?
        .to_string();
    let resolution = crate::remote_paths::resolve(
        &tx,
        input.media_type,
        crate::remote_paths::ResolveInput {
            host,
            path: input.remote_path,
            direction: crate::remote_paths::Direction::RemoteToLocal,
        },
    )
    .await
    .map_err(IntoResponse::into_response)?;
    tx.commit().await.map_err(|_| corrupt().into_response())?;
    Ok(Json(crate::remote_paths::ProviderPathPreview {
        provider_id: provider.id,
        provider_revision: provider.revision,
        resolution,
    }))
}

#[cfg(test)]
mod communication_error_tests {
    use super::*;
    #[test]
    fn raw_probe_classification_keeps_local_limits_unevaluated_and_legacy_errors() {
        use CommunicationProbeError::*;
        use http::HttpError as H;
        use qbittorrent::QbitError as Q;
        for error in [
            H::Authentication,
            H::Transport,
            H::Timeout,
            H::Redirect,
            H::InvalidResponse,
            H::RateLimited {
                retry_after_seconds: Some(60),
            },
        ] {
            assert_eq!(communication_error(Q::Http(error)), RemoteFailure);
        }
        for error in [Q::UnsupportedVersion, Q::InvalidResponse, Q::Rejected] {
            assert_eq!(communication_error(error), RemoteFailure);
        }
        for error in [
            Q::UnsupportedFeature,
            Q::InvalidRequest,
            Q::ScopeConflict,
            Q::Http(H::Busy),
            Q::Http(H::ResponseTooLarge),
            Q::Http(H::LocalUnavailable { timeout: false }),
        ] {
            assert_eq!(communication_error(error), Unevaluated("check_failed"));
        }
        assert_eq!(
            communication_error(Q::Http(H::LocalUnavailable { timeout: true })),
            Unevaluated("check_timeout")
        );
        for timeout in [false, true] {
            let local = refresh_error(http_error(H::LocalUnavailable { timeout }).0);
            let prior =
                refresh_error(http_error(if timeout { H::Timeout } else { H::Transport }).0);
            assert_eq!(
                (local.code, local.retryable, local.retry_after_seconds),
                (prior.code, prior.retryable, prior.retry_after_seconds)
            );
        }
    }
}
