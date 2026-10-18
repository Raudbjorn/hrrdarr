//! Persisted first-pair configuration. Saving configuration performs no network requests.
pub mod administration;
pub(crate) mod backoff;
pub mod categories;
mod credentials;
pub(crate) mod download;
pub mod draft;
pub mod http;
pub mod indexer;
mod indexer_backoff;
pub mod label_client;
pub mod nzbget;
pub mod qbittorrent;
pub mod sabnzbd;
pub(crate) mod selection;
pub mod transmission;
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
pub use selection::ClientSelection;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, sync::Arc};
use uuid::Uuid;

#[derive(Debug)]
pub(crate) enum Error {
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
                    "Provider is temporarily unavailable; retry after the indicated interval",
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
pub(crate) type Result<T> = std::result::Result<T, Error>;
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
    #[serde(
        default,
        deserialize_with = "selection::tags_input",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(optional, as = "Option<Vec<i64>>")]
    pub tags: Option<Vec<i64>>,
}
/// SABnzbd job priority. Wire values are the documented `priority` API parameter integers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum UsenetPriority {
    /// The category's configured priority (wire -100).
    #[default]
    Default,
    Paused,
    Low,
    Normal,
    High,
    Force,
}
impl UsenetPriority {
    pub(crate) fn wire(self) -> i64 {
        match self {
            Self::Default => -100,
            Self::Paused => -2,
            Self::Low => -1,
            Self::Normal => 0,
            Self::High => 1,
            Self::Force => 2,
        }
    }
    fn stored(value: i64) -> Result<Self> {
        Ok(match value {
            -100 => Self::Default,
            -2 => Self::Paused,
            -1 => Self::Low,
            0 => Self::Normal,
            1 => Self::High,
            2 => Self::Force,
            _ => return Err(corrupt()),
        })
    }
}
/// One media domain of a usenet client. The category isolates TV from movies on a shared client.
#[derive(Clone, Default, Deserialize, Serialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct UsenetScope {
    pub category: String,
    #[serde(default)]
    #[ts(as = "Option<UsenetPriority>", optional)]
    pub recent_priority: UsenetPriority,
    #[serde(default)]
    #[ts(as = "Option<UsenetPriority>", optional)]
    pub older_priority: UsenetPriority,
    #[serde(
        default,
        deserialize_with = "selection::tags_input",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(optional, as = "Option<Vec<i64>>")]
    pub tags: Option<Vec<i64>>,
}
/// NZBGet job priority. Wire values are NZBGet's own integer priorities.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum NzbgetPriority {
    VeryLow,
    Low,
    #[default]
    Normal,
    High,
    VeryHigh,
    Force,
}
impl NzbgetPriority {
    pub(crate) fn wire(self) -> i64 {
        match self {
            Self::VeryLow => -100,
            Self::Low => -50,
            Self::Normal => 0,
            Self::High => 50,
            Self::VeryHigh => 100,
            Self::Force => 900,
        }
    }
    /// Storage code: the scope table's priority CHECK predates NZBGet (see migration 0064).
    fn code(self) -> i64 {
        match self {
            Self::VeryLow => -100,
            Self::Low => -1,
            Self::Normal => 0,
            Self::High => 1,
            Self::VeryHigh => 2,
            Self::Force => -2,
        }
    }
    fn stored(value: i64) -> Result<Self> {
        Ok(match value {
            -100 => Self::VeryLow,
            -1 => Self::Low,
            0 => Self::Normal,
            1 => Self::High,
            2 => Self::VeryHigh,
            -2 => Self::Force,
            _ => return Err(corrupt()),
        })
    }
}
/// One media domain of an NZBGet client. The category isolates TV from movies on a shared client.
#[derive(Clone, Default, Deserialize, Serialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct NzbgetScope {
    pub category: String,
    #[serde(default)]
    #[ts(as = "Option<NzbgetPriority>", optional)]
    pub recent_priority: NzbgetPriority,
    #[serde(default)]
    #[ts(as = "Option<NzbgetPriority>", optional)]
    pub older_priority: NzbgetPriority,
    /// The job is added paused and must be resumed in NZBGet.
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    pub add_paused: bool,
    #[serde(
        default,
        deserialize_with = "selection::tags_input",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(optional, as = "Option<Vec<i64>>")]
    pub tags: Option<Vec<i64>>,
}
/// Transmission queue placement for a new torrent; `first` moves it to the top of the queue.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum TorrentQueuePriority {
    #[default]
    Last,
    First,
}
impl TorrentQueuePriority {
    pub(crate) fn wire(self) -> i64 {
        match self {
            Self::Last => 0,
            Self::First => 1,
        }
    }
    fn stored(value: i64) -> Result<Self> {
        match value {
            0 => Ok(Self::Last),
            1 => Ok(Self::First),
            _ => Err(corrupt()),
        }
    }
}
/// One media domain of a label-isolated torrent client (Deluge and similar). The label isolates
/// TV from movies on a shared client; labels compare case-insensitively.
#[derive(Clone, Default, Deserialize, Serialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct TorrentLabelScope {
    pub category: String,
    #[serde(default)]
    #[ts(as = "Option<TorrentQueuePriority>", optional)]
    pub recent_priority: TorrentQueuePriority,
    #[serde(default)]
    #[ts(as = "Option<TorrentQueuePriority>", optional)]
    pub older_priority: TorrentQueuePriority,
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    pub add_paused: bool,
    #[serde(
        default,
        deserialize_with = "selection::tags_input",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(optional, as = "Option<Vec<i64>>")]
    pub tags: Option<Vec<i64>>,
}
/// Label rules shared by the label clients: lowercase letters, digits and `_ . -` (Deluge's
/// Label plugin rejects anything else), so one name works across the family.
fn valid_torrent_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}
/// Where one domain's torrents are placed on a shared Transmission client and how they are
/// recognised again.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize, ts_rs::TS)]
#[serde(tag = "strategy", rename_all = "snake_case", deny_unknown_fields)]
pub enum TorrentPlacement {
    /// A subfolder of the client's download directory, also applied as a torrent label when the
    /// server supports labels (Transmission 4.0+).
    Category { category: String },
    /// An absolute remote download directory; ownership is by directory only.
    Directory { directory: String },
}
/// One media domain of a Transmission client.
#[derive(Clone, Deserialize, Serialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct TransmissionScope {
    pub placement: TorrentPlacement,
    #[serde(default)]
    #[ts(as = "Option<TorrentQueuePriority>", optional)]
    pub recent_priority: TorrentQueuePriority,
    #[serde(default)]
    #[ts(as = "Option<TorrentQueuePriority>", optional)]
    pub older_priority: TorrentQueuePriority,
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    pub add_paused: bool,
    #[serde(
        default,
        deserialize_with = "selection::tags_input",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(optional, as = "Option<Vec<i64>>")]
    pub tags: Option<Vec<i64>>,
}
/// Transmission category: a single path segment that is also a label (no separators or commas).
fn valid_torrent_category(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}
/// Absolute remote directory in canonical form: POSIX `/...` or drive `X:/...`/`X:\...`, with no
/// empty, `.` or `..` segments and no trailing separator. A bare root is rejected.
fn valid_remote_directory(value: &str) -> bool {
    let bytes = value.as_bytes();
    let drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\');
    let rest = if drive {
        &value[3..]
    } else if let Some(rest) = value.strip_prefix('/') {
        rest
    } else {
        return false;
    };
    value.len() <= 1024
        && !rest.is_empty()
        && !value.chars().any(char::is_control)
        && !rest.contains(':')
        && rest
            .split(['/', '\\'])
            .all(|part| !part.is_empty() && part != "." && part != "..")
}
/// Release transport shared by an indexer and the clients that may receive its releases.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Protocol {
    Torrent,
    Usenet,
}
/// Implementations on the shared label-torrent engine, in registry order.
pub(crate) const LABEL_IMPLEMENTATIONS: [&str; 6] = [
    "deluge",
    "rtorrent",
    "utorrent",
    "aria2",
    "flood",
    "downloadstation",
];
impl Protocol {
    pub(crate) fn of_implementation(implementation: &str) -> Option<Self> {
        match implementation {
            "torznab" | "qbittorrent" | "transmission" => Some(Self::Torrent),
            label if LABEL_IMPLEMENTATIONS.contains(&label) => Some(Self::Torrent),
            "newznab" | "sabnzbd" | "nzbget" => Some(Self::Usenet),
            _ => None,
        }
    }
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
    /// Endpoint is the SABnzbd base URL (scheme, host, port and URL base); the API is `<endpoint>/api`.
    Sabnzbd {
        endpoint: String,
        #[serde(deserialize_with = "required_nullable")]
        tv: Option<UsenetScope>,
        #[serde(deserialize_with = "required_nullable")]
        movies: Option<UsenetScope>,
    },
    /// Endpoint is the NZBGet base URL (scheme, host, port and URL base); the API is
    /// `<endpoint>/jsonrpc`.
    Nzbget {
        endpoint: String,
        #[serde(deserialize_with = "required_nullable")]
        tv: Option<NzbgetScope>,
        #[serde(deserialize_with = "required_nullable")]
        movies: Option<NzbgetScope>,
    },
    /// Endpoint is the Transmission web base: scheme (SSL), host, port and URL base (default
    /// `/transmission`); the RPC is `<endpoint>/rpc`.
    Transmission {
        endpoint: String,
        #[serde(deserialize_with = "required_nullable")]
        tv: Option<TransmissionScope>,
        #[serde(deserialize_with = "required_nullable")]
        movies: Option<TransmissionScope>,
    },
    /// Endpoint is the Deluge Web UI base URL (the JSON-RPC API is `<endpoint>/json`). The
    /// credential is the Web UI password. Requires the Label plugin.
    Deluge {
        endpoint: String,
        #[serde(deserialize_with = "required_nullable")]
        tv: Option<TorrentLabelScope>,
        #[serde(deserialize_with = "required_nullable")]
        movies: Option<TorrentLabelScope>,
    },
    /// Endpoint is the rTorrent XML-RPC URL (for example an nginx or ruTorrent `/RPC2` mount).
    /// Optional HTTP basic credentials. Torrents are labelled with `d.custom1`.
    Rtorrent {
        endpoint: String,
        #[serde(deserialize_with = "required_nullable")]
        tv: Option<TorrentLabelScope>,
        #[serde(deserialize_with = "required_nullable")]
        movies: Option<TorrentLabelScope>,
    },
    /// Endpoint is the uTorrent Web UI base (for example `http://host:8080/gui`). Basic
    /// credentials are required; torrents are labelled with uTorrent labels.
    Utorrent {
        endpoint: String,
        #[serde(deserialize_with = "required_nullable")]
        tv: Option<TorrentLabelScope>,
        #[serde(deserialize_with = "required_nullable")]
        movies: Option<TorrentLabelScope>,
    },
    /// Endpoint is the aria2 JSON-RPC URL (usually `http://host:6800/jsonrpc`). The optional RPC
    /// secret is an API-key credential. A domain is the subfolder `<default dir>/<label>`.
    Aria2 {
        endpoint: String,
        #[serde(deserialize_with = "required_nullable")]
        tv: Option<TorrentLabelScope>,
        #[serde(deserialize_with = "required_nullable")]
        movies: Option<TorrentLabelScope>,
    },
    /// Endpoint is the Flood base URL (for example `http://host:3000`). Optional basic credentials
    /// are the Flood user name and password; torrents are labelled with Flood tags.
    Flood {
        endpoint: String,
        #[serde(deserialize_with = "required_nullable")]
        tv: Option<TorrentLabelScope>,
        #[serde(deserialize_with = "required_nullable")]
        movies: Option<TorrentLabelScope>,
    },
    /// Endpoint is the Synology DSM base URL (for example `https://nas.example:5001`). Basic
    /// credentials are required; a domain is the destination folder `<default>/<label>/<hash>`.
    Downloadstation {
        endpoint: String,
        #[serde(deserialize_with = "required_nullable")]
        tv: Option<TorrentLabelScope>,
        #[serde(deserialize_with = "required_nullable")]
        movies: Option<TorrentLabelScope>,
    },
}
impl ProviderSettings {
    fn implementation(&self) -> &'static str {
        match self {
            Self::Torznab { .. } => "torznab",
            Self::Newznab { .. } => "newznab",
            Self::Qbittorrent { .. } => "qbittorrent",
            Self::Sabnzbd { .. } => "sabnzbd",
            Self::Nzbget { .. } => "nzbget",
            Self::Transmission { .. } => "transmission",
            Self::Deluge { .. } => "deluge",
            Self::Rtorrent { .. } => "rtorrent",
            Self::Utorrent { .. } => "utorrent",
            Self::Aria2 { .. } => "aria2",
            Self::Flood { .. } => "flood",
            Self::Downloadstation { .. } => "downloadstation",
        }
    }
    fn endpoint(&self) -> &str {
        match self {
            Self::Torznab { endpoint, .. }
            | Self::Newznab { endpoint, .. }
            | Self::Qbittorrent { endpoint, .. }
            | Self::Sabnzbd { endpoint, .. }
            | Self::Nzbget { endpoint, .. }
            | Self::Transmission { endpoint, .. }
            | Self::Deluge { endpoint, .. }
            | Self::Rtorrent { endpoint, .. }
            | Self::Downloadstation { endpoint, .. }
            | Self::Flood { endpoint, .. }
            | Self::Aria2 { endpoint, .. }
            | Self::Utorrent { endpoint, .. } => endpoint,
        }
    }
    pub(crate) fn protocol(&self) -> Protocol {
        match self {
            Self::Torznab { .. }
            | Self::Qbittorrent { .. }
            | Self::Transmission { .. }
            | Self::Deluge { .. }
            | Self::Rtorrent { .. }
            | Self::Downloadstation { .. }
            | Self::Flood { .. }
            | Self::Aria2 { .. }
            | Self::Utorrent { .. } => Protocol::Torrent,
            Self::Newznab { .. } | Self::Sabnzbd { .. } | Self::Nzbget { .. } => Protocol::Usenet,
        }
    }
    pub(crate) fn is_download_client(&self) -> bool {
        matches!(
            self,
            Self::Qbittorrent { .. }
                | Self::Sabnzbd { .. }
                | Self::Nzbget { .. }
                | Self::Transmission { .. }
        ) || self.is_label_torrent()
    }
    /// The TV and movie scopes of a label-torrent client (one arm per client of the family).
    fn label_pair(&self) -> Option<(&Option<TorrentLabelScope>, &Option<TorrentLabelScope>)> {
        match self {
            Self::Deluge { tv, movies, .. }
            | Self::Rtorrent { tv, movies, .. }
            | Self::Downloadstation { tv, movies, .. }
            | Self::Flood { tv, movies, .. }
            | Self::Aria2 { tv, movies, .. }
            | Self::Utorrent { tv, movies, .. } => Some((tv, movies)),
            _ => None,
        }
    }
    /// A label-torrent client by registry name (one arm per client of the family).
    fn from_label_parts(
        implementation: &str,
        endpoint: String,
        tv: Option<TorrentLabelScope>,
        movies: Option<TorrentLabelScope>,
    ) -> Option<Self> {
        Some(match implementation {
            "deluge" => Self::Deluge {
                endpoint,
                tv,
                movies,
            },
            "rtorrent" => Self::Rtorrent {
                endpoint,
                tv,
                movies,
            },
            "utorrent" => Self::Utorrent {
                endpoint,
                tv,
                movies,
            },
            "aria2" => Self::Aria2 {
                endpoint,
                tv,
                movies,
            },
            "flood" => Self::Flood {
                endpoint,
                tv,
                movies,
            },
            "downloadstation" => Self::Downloadstation {
                endpoint,
                tv,
                movies,
            },
            _ => return None,
        })
    }
    /// The same client with other label scopes (one arm per client of the family).
    fn relabelled(
        &self,
        tv: Option<TorrentLabelScope>,
        movies: Option<TorrentLabelScope>,
    ) -> Option<Self> {
        match self {
            Self::Deluge { endpoint, .. } => Some(Self::Deluge {
                endpoint: endpoint.clone(),
                tv,
                movies,
            }),
            Self::Rtorrent { endpoint, .. } => Some(Self::Rtorrent {
                endpoint: endpoint.clone(),
                tv,
                movies,
            }),
            Self::Utorrent { endpoint, .. } => Some(Self::Utorrent {
                endpoint: endpoint.clone(),
                tv,
                movies,
            }),
            Self::Aria2 { endpoint, .. } => Some(Self::Aria2 {
                endpoint: endpoint.clone(),
                tv,
                movies,
            }),
            Self::Flood { endpoint, .. } => Some(Self::Flood {
                endpoint: endpoint.clone(),
                tv,
                movies,
            }),
            Self::Downloadstation { endpoint, .. } => Some(Self::Downloadstation {
                endpoint: endpoint.clone(),
                tv,
                movies,
            }),
            _ => None,
        }
    }
    /// Whether this client uses the shared label-torrent engine.
    pub(crate) fn is_label_torrent(&self) -> bool {
        self.label_pair().is_some()
    }
    /// The label scope of one domain on a label-torrent client.
    pub(crate) fn label_scope(&self, domain: MediaDomain) -> Option<&TorrentLabelScope> {
        let (tv, movies) = self.label_pair()?;
        match domain {
            MediaDomain::Tv => tv.as_ref(),
            MediaDomain::Movies => movies.as_ref(),
        }
    }
    /// Copy without eligibility tags, which never change remote submission behaviour.
    pub(crate) fn without_label_tags(&self) -> Option<Self> {
        let (tv, movies) = self.label_pair()?;
        let strip = |scope: &Option<TorrentLabelScope>| {
            scope.clone().map(|mut s| {
                s.tags = None;
                s
            })
        };
        self.relabelled(strip(tv), strip(movies))
    }
    /// Whether a download client configures this media domain; indexers always answer false.
    pub(crate) fn client_scoped(&self, domain: MediaDomain) -> bool {
        match (self, domain) {
            (Self::Qbittorrent { tv, .. }, MediaDomain::Tv) => tv.is_some(),
            (Self::Qbittorrent { movies, .. }, MediaDomain::Movies) => movies.is_some(),
            (Self::Sabnzbd { tv, .. }, MediaDomain::Tv) => tv.is_some(),
            (Self::Sabnzbd { movies, .. }, MediaDomain::Movies) => movies.is_some(),
            (Self::Nzbget { tv, .. }, MediaDomain::Tv) => tv.is_some(),
            (Self::Nzbget { movies, .. }, MediaDomain::Movies) => movies.is_some(),
            (Self::Transmission { tv, .. }, MediaDomain::Tv) => tv.is_some(),
            (Self::Transmission { movies, .. }, MediaDomain::Movies) => movies.is_some(),
            _ => self.label_scope(domain).is_some(),
        }
    }
    /// Domains configured by a download client, in TV/movies order.
    pub(crate) fn client_domains(&self) -> impl Iterator<Item = MediaDomain> + '_ {
        [MediaDomain::Tv, MediaDomain::Movies]
            .into_iter()
            .filter(|domain| self.client_scoped(*domain))
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
                    if let Some(ids) = &s.tags {
                        crate::tags::validate(&crate::tags::Assignment {
                            mode: crate::tags::Mode::Replace,
                            ids: ids.clone(),
                        })
                        .map_err(|_| bad())?;
                    }
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
            Self::Sabnzbd { tv, movies, .. } => {
                if tv.is_none() && movies.is_none() {
                    return Err(bad());
                }
                // A comma separates SABnzbd category filters and `*` selects the Default
                // category; neither can identify one isolated domain.
                let category = |c: &str| {
                    !c.is_empty()
                        && c.trim() == c
                        && c.len() <= 64
                        && c != "*"
                        && !c.chars().any(|ch| {
                            ch.is_control() || matches!(ch, ',' | '*' | '/' | '\\' | '{' | '}')
                        })
                };
                for s in tv.iter().chain(movies.iter()) {
                    if let Some(ids) = &s.tags {
                        crate::tags::validate(&crate::tags::Assignment {
                            mode: crate::tags::Mode::Replace,
                            ids: ids.clone(),
                        })
                        .map_err(|_| bad())?;
                    }
                    if !category(&s.category) {
                        return Err(bad());
                    }
                }
                if let (Some(tv), Some(movies)) = (tv, movies) {
                    if tv.category.to_lowercase() == movies.category.to_lowercase() {
                        return Err(bad());
                    }
                }
            }
            Self::Nzbget { tv, movies, .. } => {
                if tv.is_none() && movies.is_none() {
                    return Err(bad());
                }
                // Category names are free text in NZBGet, but a comma or path separator cannot
                // identify one isolated domain and braces/stars are option syntax.
                let category = |c: &str| {
                    !c.is_empty()
                        && c.trim() == c
                        && c.len() <= 64
                        && !c.chars().any(|ch| {
                            ch.is_control() || matches!(ch, ',' | '*' | '/' | '\\' | '{' | '}')
                        })
                };
                for s in tv.iter().chain(movies.iter()) {
                    if let Some(ids) = &s.tags {
                        crate::tags::validate(&crate::tags::Assignment {
                            mode: crate::tags::Mode::Replace,
                            ids: ids.clone(),
                        })
                        .map_err(|_| bad())?;
                    }
                    if !category(&s.category) {
                        return Err(bad());
                    }
                }
                if let (Some(tv), Some(movies)) = (tv, movies) {
                    if tv.category.to_lowercase() == movies.category.to_lowercase() {
                        return Err(bad());
                    }
                }
            }
            Self::Deluge { tv, movies, .. }
            | Self::Rtorrent { tv, movies, .. }
            | Self::Downloadstation { tv, movies, .. }
            | Self::Flood { tv, movies, .. }
            | Self::Aria2 { tv, movies, .. }
            | Self::Utorrent { tv, movies, .. } => {
                if tv.is_none() && movies.is_none() {
                    return Err(bad());
                }
                for s in tv.iter().chain(movies.iter()) {
                    if let Some(ids) = &s.tags {
                        crate::tags::validate(&crate::tags::Assignment {
                            mode: crate::tags::Mode::Replace,
                            ids: ids.clone(),
                        })
                        .map_err(|_| bad())?;
                    }
                    if !valid_torrent_label(&s.category) {
                        return Err(bad());
                    }
                }
                if let (Some(tv), Some(movies)) = (tv, movies) {
                    if tv.category == movies.category {
                        return Err(bad());
                    }
                }
            }
            Self::Transmission { tv, movies, .. } => {
                if tv.is_none() && movies.is_none() {
                    return Err(bad());
                }
                for s in tv.iter().chain(movies.iter()) {
                    if let Some(ids) = &s.tags {
                        crate::tags::validate(&crate::tags::Assignment {
                            mode: crate::tags::Mode::Replace,
                            ids: ids.clone(),
                        })
                        .map_err(|_| bad())?;
                    }
                    let valid = match &s.placement {
                        TorrentPlacement::Category { category } => valid_torrent_category(category),
                        TorrentPlacement::Directory { directory } => {
                            valid_remote_directory(directory)
                        }
                    };
                    if !valid {
                        return Err(bad());
                    }
                }
                // Labels compare case-insensitively and directories nest; neither may be shared.
                if let (Some(tv), Some(movies)) = (tv, movies) {
                    let conflict = match (&tv.placement, &movies.placement) {
                        (
                            TorrentPlacement::Category { category: a },
                            TorrentPlacement::Category { category: b },
                        ) => a.eq_ignore_ascii_case(b),
                        (
                            TorrentPlacement::Directory { directory: a },
                            TorrentPlacement::Directory { directory: b },
                        ) => {
                            // Case-insensitive, like the storage trigger: conservative on POSIX.
                            let (a, b) = (a.to_lowercase(), b.to_lowercase());
                            transmission::within(&a, &b) || transmission::within(&b, &a)
                        }
                        // Category directories depend on the client's download directory; Test
                        // and every operation check the effective locations.
                        _ => false,
                    };
                    if conflict {
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
    UsenetClient(sabnzbd::SabnzbdTest),
    NzbgetClient(nzbget::NzbgetTest),
    TorrentClient(transmission::TransmissionTest),
    LabelClient(label_client::LabelClientTest),
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
    backoff_started: std::time::Instant,
    selection_cursor: Arc<std::sync::Mutex<[Option<String>; 2]>>,
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
        backoff_started: std::time::Instant::now(),
        selection_cursor: Arc::new(std::sync::Mutex::new([None, None])),
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
    pub client_failure: bool,
    pub definitely_not_submitted: bool,
    pub client_retry_kind: ClientRetryKind,
    pub client_retry_eligible: bool,
}
/// Pending-release policy, independent of add safety and health attribution.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum ClientRetryKind {
    #[default]
    Other = 0,
    Unavailable = 1,
    Authentication = 2,
}
pub(crate) type AutomationError = RefreshError;
#[derive(Debug)]
pub(crate) struct PreparationError {
    pub error: AutomationError,
    pub identity: Option<qbittorrent::SubmissionIdentity>,
    pub client_changed: bool,
    pub local_storage_failure: bool,
}
impl PreparationError {
    fn release(error: AutomationError) -> Self {
        Self {
            error,
            identity: None,
            client_changed: false,
            local_storage_failure: false,
        }
    }
    fn lookup(error: Error) -> Self {
        let local_storage_failure = matches!(&error, Error::Plain(_, "provider_database_error", _));
        let mut failure = Self::configuration(refresh_error(error));
        failure.local_storage_failure = local_storage_failure;
        failure
    }
    fn configuration(error: AutomationError) -> Self {
        let client_changed = matches!(error.code, "provider_changed" | "provider_unavailable");
        Self {
            error,
            identity: None,
            client_changed,
            local_storage_failure: false,
        }
    }
}

// Keep local storage provenance at the lookup boundary; release/client HTTP
// failures can share refresh_failed and must never acquire this classification.
fn automation_lookup_error(error: Error) -> AutomationError {
    if matches!(&error, Error::Plain(_, "provider_database_error", _)) {
        RefreshError::new("provider_database_error", true)
    } else {
        refresh_error(error)
    }
}
fn preparation_error(error: qbittorrent::QbitError) -> PreparationError {
    let mapped = match error {
        qbittorrent::QbitError::InvalidRequest => RefreshError::new("invalid_release", false),
        // These requests fetch release bytes, never communicate with the download client.
        qbittorrent::QbitError::Http(error) => {
            let mut mapped = refresh_error(http_error(error).0);
            if mapped.code == "provider_unavailable" {
                mapped.code = "refresh_failed";
            }
            mapped
        }
        other => refresh_error(qbit_error(other).0),
    };
    PreparationError::release(mapped)
}

impl RefreshError {
    fn new(code: &'static str, retryable: bool) -> Self {
        Self {
            code,
            retryable,
            retry_after_seconds: None,
            client_failure: false,
            definitely_not_submitted: false,
            client_retry_kind: ClientRetryKind::Other,
            client_retry_eligible: false,
        }
    }
}
fn refresh_error(error: Error) -> RefreshError {
    match error {
        Error::RateLimited(seconds) => RefreshError {
            code: "refresh_failed",
            retryable: true,
            retry_after_seconds: Some(seconds.clamp(1, 86400)),
            client_failure: false,
            definitely_not_submitted: false,
            client_retry_kind: ClientRetryKind::Other,
            client_retry_eligible: false,
        },
        Error::Plain(_, "provider_revision_conflict", _) => {
            RefreshError::new("provider_changed", false)
        }
        Error::Plain(
            _,
            code @ ("download_client_unavailable"
            | "unsupported_target"
            | "refresh_limit"
            | "protocol_mismatch"),
            _,
        ) => RefreshError::new(code, false),
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
            client_failure: false,
            definitely_not_submitted: false,
            client_retry_kind: ClientRetryKind::Other,
            client_retry_eligible: false,
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
    pub details: download::DownloadDetails,
    pub host: String,
}
pub(crate) enum PathOutput {
    Local(String),
    Missing,
    Invalid,
}
pub(crate) struct PathOutputFacts {
    pub locality: qbittorrent::EndpointLocality,
    pub output: PathOutput,
}
impl RefreshClient {
    pub(crate) fn backoff_grace_elapsed(&self) -> bool {
        self.0.backoff_started.elapsed() >= backoff::STARTUP_GRACE
    }
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
        let scoped = provider.settings.client_scoped(domain);
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
            let result =
                refresh_items(&operation, &provider.settings, credentials.as_ref(), domain)
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
            .and_then(|(_, status, _)| match status {
                qbittorrent::ClientStatusSnapshot::Status(status) => Ok(status.locality),
                _ => Err(refresh_error(
                    qbit_error(qbittorrent::QbitError::InvalidResponse).0,
                )),
            })
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
        let qbittorrent::ClientStatusSnapshot::Status(status) = status else {
            return Err(refresh_error(
                qbit_error(qbittorrent::QbitError::InvalidResponse).0,
            ));
        };
        let output = self
            .resolve_download_root(&provider, status.remote_root, domain, deadline)
            .await?;
        match output {
            PathOutput::Local(path) => Ok(vec![path]),
            _ => Err(RefreshError::new("download_root_unresolved", false)),
        }
    }

    pub(crate) async fn inspect_path_output(
        &self,
        provider_id: &str,
        revision: i64,
        domain: MediaDomain,
    ) -> std::result::Result<PathOutputFacts, AutomationError> {
        use qbittorrent::ClientStatusSnapshot;
        let (provider, status, deadline) = self
            .inspect_client_status_snapshot(provider_id, revision, domain)
            .await?;
        let (locality, output) = match status {
            ClientStatusSnapshot::OutputMissing(locality) => (locality, PathOutput::Missing),
            ClientStatusSnapshot::OutputInvalid(locality) => (locality, PathOutput::Invalid),
            ClientStatusSnapshot::Status(status) => (
                status.locality,
                self.resolve_download_root(&provider, status.remote_root, domain, deadline)
                    .await?,
            ),
        };
        Ok(PathOutputFacts { locality, output })
    }

    async fn resolve_download_root(
        &self,
        provider: &Provider,
        remote_root: String,
        domain: MediaDomain,
        deadline: tokio::time::Instant,
    ) -> std::result::Result<PathOutput, AutomationError> {
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
                    path: remote_root,
                    direction: crate::remote_paths::Direction::RemoteToLocal,
                },
            )
            .await
            .map_err(|_| RefreshError::new("download_root_mapping_failed", false))?;
            let output = local_download_root(&resolution.output)
                .map_or(PathOutput::Invalid, PathOutput::Local);
            current_revision(&self.0.db, provider)
                .await
                .map_err(refresh_error)?;
            Ok::<_, AutomationError>(output)
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
        (
            Provider,
            qbittorrent::ClientStatusSnapshot,
            tokio::time::Instant,
        ),
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
        if !provider.enabled || !provider.settings.is_download_client() {
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
            let status = match &provider.settings {
                ProviderSettings::Sabnzbd { .. } => {
                    sabnzbd::client_status_snapshot(
                        &operation,
                        &provider.settings,
                        credentials.as_ref(),
                        domain,
                    )
                    .await
                }
                ProviderSettings::Nzbget { .. } => {
                    nzbget::client_status_snapshot(
                        &operation,
                        &provider.settings,
                        credentials.as_ref(),
                        domain,
                    )
                    .await
                }
                settings if settings.is_label_torrent() => {
                    label_client::client_status_snapshot(
                        &operation,
                        &provider.settings,
                        credentials.as_ref(),
                        domain,
                    )
                    .await
                }
                ProviderSettings::Transmission { .. } => {
                    transmission::client_status_snapshot(
                        &operation,
                        &provider.settings,
                        credentials.as_ref(),
                        domain,
                    )
                    .await
                }
                _ => {
                    qbittorrent::client_status_snapshot(
                        &operation,
                        &provider.settings,
                        credentials.as_ref(),
                        domain,
                    )
                    .await
                }
            }
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
        if !provider.enabled || !provider.settings.is_download_client() {
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
            let result = match &provider.settings {
                ProviderSettings::Sabnzbd { .. } => {
                    match sabnzbd::job(
                        &operation,
                        &provider.settings,
                        credentials.as_ref(),
                        target,
                        remote_id,
                    )
                    .await
                    {
                        Ok(job) => self.usenet_details(job, &host, target).await,
                        Err(e) => Err(refresh_error(qbit_error(e).0)),
                    }
                }
                ProviderSettings::Nzbget { .. } => {
                    match nzbget::job(
                        &operation,
                        &provider.settings,
                        credentials.as_ref(),
                        target,
                        remote_id,
                    )
                    .await
                    {
                        Ok(job) => self.usenet_details(job, &host, target).await,
                        Err(e) => Err(refresh_error(qbit_error(e).0)),
                    }
                }
                settings if settings.is_label_torrent() => label_client::details(
                    &operation,
                    &provider.settings,
                    credentials.as_ref(),
                    target,
                    remote_id,
                )
                .await
                .map(download::DownloadDetails::from)
                .map_err(|e| refresh_error(qbit_error(e).0)),
                ProviderSettings::Transmission { .. } => transmission::details(
                    &operation,
                    &provider.settings,
                    credentials.as_ref(),
                    target,
                    remote_id,
                )
                .await
                .map(download::DownloadDetails::from)
                .map_err(|e| refresh_error(qbit_error(e).0)),
                _ => qbittorrent::details(
                    &operation,
                    &provider.settings,
                    credentials.as_ref(),
                    target,
                    remote_id,
                )
                .await
                .map(download::DownloadDetails::from)
                .map_err(|e| refresh_error(qbit_error(e).0)),
            };
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
        source: download::ReleaseSource,
        recent: bool,
    ) -> std::result::Result<download::PreparedSubmission, PreparationError> {
        let context = &self.0;
        let (provider, _credentials) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            network_snapshot(context, provider_id),
        )
        .await
        .map_err(|_| PreparationError::release(RefreshError::new("refresh_timeout", true)))?
        .map_err(PreparationError::lookup)?;
        if provider.revision != revision {
            return Err(PreparationError::configuration(RefreshError::new(
                "provider_changed",
                false,
            )));
        }
        if !provider.enabled || !provider.settings.is_download_client() {
            return Err(PreparationError::configuration(RefreshError::new(
                "provider_unavailable",
                false,
            )));
        }
        // Selection is protocol-aware; a mismatch here is a visible terminal rejection, stored
        // with the closed candidate vocabulary's `unsupported_target` code.
        if source.protocol() != provider.settings.protocol() {
            return Err(PreparationError::release(RefreshError::new(
                "unsupported_target",
                false,
            )));
        }
        if let ProviderSettings::Qbittorrent { tv, movies, .. } = &provider.settings {
            let scope = match &target {
                crate::db::MediaTarget::Episode(_) => tv.as_ref(),
                crate::db::MediaTarget::Movie(_) => movies.as_ref(),
            };
            if scope.is_some_and(|s| s.recent_priority != s.older_priority) {
                return Err(PreparationError::release(RefreshError::new(
                    "unsupported_target",
                    false,
                )));
            }
        }
        let transport = context
            .transport
            .as_ref()
            .as_ref()
            .map_err(|e| PreparationError::release(refresh_error(http_error(*e).0)))?;
        let operation = transport
            .operation(provider.id)
            .map_err(|e| PreparationError::release(refresh_error(http_error(e).0)))?;
        let work = async {
            let prepared = match source {
                download::ReleaseSource::Torrent { source, options }
                    if provider.settings.is_label_torrent() =>
                {
                    label_client::prepare(
                        &operation,
                        &provider.settings,
                        target,
                        source,
                        recent,
                        options,
                    )
                    .await
                    .map(download::PreparedSubmission::Label)
                }
                download::ReleaseSource::Torrent { source, options }
                    if matches!(provider.settings, ProviderSettings::Transmission { .. }) =>
                {
                    transmission::prepare(
                        &operation,
                        &provider.settings,
                        target,
                        source,
                        recent,
                        options,
                    )
                    .await
                    .map(download::PreparedSubmission::Transmission)
                }
                download::ReleaseSource::Torrent { source, options } => qbittorrent::prepare(
                    &operation,
                    &provider.settings,
                    target,
                    source,
                    recent,
                    options,
                )
                .await
                .map(download::PreparedSubmission::Torrent),
                download::ReleaseSource::Usenet { url, title }
                    if matches!(provider.settings, ProviderSettings::Nzbget { .. }) =>
                {
                    nzbget::prepare(provider.id, &provider.settings, target, url, &title, recent)
                        .map(download::PreparedSubmission::Nzbget)
                }
                download::ReleaseSource::Usenet { url, title } => {
                    sabnzbd::prepare(provider.id, &provider.settings, target, url, &title, recent)
                        .map(download::PreparedSubmission::Usenet)
                }
            }
            .map_err(preparation_error)?;
            if let Err(error) = current_revision(&context.db, &provider).await {
                let mut failure = PreparationError::lookup(error);
                failure.identity = Some(prepared.into_identity());
                return Err(failure);
            }
            if let Err(error) = operation.ensure_active() {
                return Err(PreparationError {
                    error: refresh_error(http_error(error).0),
                    identity: Some(prepared.into_identity()),
                    client_changed: false,
                    local_storage_failure: false,
                });
            }
            Ok(prepared)
        };
        tokio::time::timeout_at(operation.deadline(), work)
            .await
            .map_err(|_| PreparationError::release(RefreshError::new("refresh_timeout", true)))?
    }
    pub(crate) async fn submit_download(
        &self,
        provider_id: &str,
        revision: i64,
        prepared: download::PreparedSubmission,
    ) -> std::result::Result<qbittorrent::AddOutcome, AutomationError> {
        fn before_add(mut error: AutomationError) -> AutomationError {
            error.definitely_not_submitted = true;
            if matches!(error.code, "provider_changed" | "provider_unavailable") {
                error.client_retry_eligible = true;
                error.client_retry_kind = ClientRetryKind::Unavailable;
            }
            error
        }
        let context = &self.0;
        let (provider, credentials) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            network_snapshot(context, provider_id),
        )
        .await
        .map_err(|_| before_add(RefreshError::new("refresh_timeout", true)))?
        .map_err(|e| before_add(automation_lookup_error(e)))?;
        if provider.revision != revision {
            return Err(before_add(RefreshError::new("provider_changed", false)));
        }
        if !provider.enabled || !provider.settings.is_download_client() {
            return Err(before_add(RefreshError::new("provider_unavailable", false)));
        }
        let transport = context
            .transport
            .as_ref()
            .as_ref()
            .map_err(|e| before_add(refresh_error(http_error(*e).0)))?;
        let operation = transport
            .operation(provider.id)
            .map_err(|e| before_add(refresh_error(http_error(e).0)))?;
        let work = async {
            let result = match (&provider.settings, prepared) {
                (
                    ProviderSettings::Qbittorrent { .. },
                    download::PreparedSubmission::Torrent(prepared),
                ) => {
                    qbittorrent::submit_classified(
                        &operation,
                        &provider.settings,
                        credentials.as_ref(),
                        prepared,
                    )
                    .await
                }
                (
                    ProviderSettings::Sabnzbd { .. },
                    download::PreparedSubmission::Usenet(prepared),
                ) => {
                    sabnzbd::submit_classified(
                        &operation,
                        &provider.settings,
                        credentials.as_ref(),
                        prepared,
                    )
                    .await
                }
                (
                    ProviderSettings::Nzbget { .. },
                    download::PreparedSubmission::Nzbget(prepared),
                ) => {
                    nzbget::submit_classified(
                        &operation,
                        &provider.settings,
                        credentials.as_ref(),
                        prepared,
                    )
                    .await
                }
                (settings, download::PreparedSubmission::Label(prepared))
                    if settings.is_label_torrent() =>
                {
                    label_client::submit_classified(
                        &operation,
                        &provider.settings,
                        credentials.as_ref(),
                        prepared,
                    )
                    .await
                }
                (
                    ProviderSettings::Transmission { .. },
                    download::PreparedSubmission::Transmission(prepared),
                ) => {
                    transmission::submit_classified(
                        &operation,
                        &provider.settings,
                        credentials.as_ref(),
                        prepared,
                    )
                    .await
                }
                _ => Err(qbittorrent::SubmissionError {
                    error: qbittorrent::QbitError::ScopeConflict,
                    definitely_not_submitted: true,
                    client_retry_kind: ClientRetryKind::Other,
                    client_retry_eligible: false,
                }),
            }
            .map_err(|e| {
                let attributable = matches!(
                    communication_error(e.error),
                    CommunicationProbeError::RemoteFailure
                );
                let mut mapped = refresh_error(qbit_error(e.error).0);
                mapped.definitely_not_submitted = e.definitely_not_submitted;
                mapped.client_retry_kind = e.client_retry_kind;
                mapped.client_retry_eligible = e.client_retry_eligible;
                mapped.client_failure = attributable;
                mapped
            });
            let safe = result.as_ref().is_err_and(|e| e.definitely_not_submitted);
            let preserve_phase = |mut error: AutomationError| {
                error.definitely_not_submitted = safe;
                if safe { before_add(error) } else { error }
            };
            current_revision(&context.db, &provider)
                .await
                .map_err(|e| preserve_phase(automation_lookup_error(e)))?;
            operation
                .ensure_active()
                .map_err(|e| preserve_phase(refresh_error(http_error(e).0)))?;
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
        if !provider.enabled || !provider.settings.is_download_client() {
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
            let result = match &provider.settings {
                ProviderSettings::Sabnzbd { .. } => {
                    sabnzbd::reconcile(
                        &operation,
                        &provider.settings,
                        credentials.as_ref(),
                        identity,
                    )
                    .await
                }
                ProviderSettings::Nzbget { .. } => {
                    nzbget::reconcile(
                        &operation,
                        &provider.settings,
                        credentials.as_ref(),
                        identity,
                    )
                    .await
                }
                settings if settings.is_label_torrent() => {
                    label_client::reconcile(
                        &operation,
                        &provider.settings,
                        credentials.as_ref(),
                        identity,
                    )
                    .await
                }
                ProviderSettings::Transmission { .. } => {
                    transmission::reconcile(
                        &operation,
                        &provider.settings,
                        credentials.as_ref(),
                        identity,
                    )
                    .await
                }
                _ => {
                    qbittorrent::reconcile(
                        &operation,
                        &provider.settings,
                        credentials.as_ref(),
                        identity,
                    )
                    .await
                }
            }
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
            .indexer_operation(provider.id)
            .map_err(|e| refresh_error(http_error(e).0))?;
        let work = async {
            indexer_backoff::admit(&context.db, &provider, indexer_backoff::domain(request))
                .await
                .map_err(refresh_error)?;
            let result = indexer::raw_search(
                &operation,
                &provider.settings,
                &IndexerAccess::from_credentials(&credentials),
                request,
            )
            .await;
            current_revision(&context.db, &provider)
                .await
                .map_err(refresh_error)?;
            operation
                .ensure_active()
                .map_err(|e| refresh_error(http_error(e).0))?;
            indexer_backoff::record(
                &context.db,
                &provider,
                indexer_backoff::domain(request),
                indexer_backoff::observation(&result),
            )
            .await
            .map_err(refresh_error)?;
            result.map_err(|e| refresh_error(indexer_error(e, &operation).0))
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
        if !provider.enabled || !provider.settings.client_scoped(domain) {
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
            let result =
                refresh_items(&operation, &provider.settings, credentials.as_ref(), domain)
                    .await
                    .map_err(|error| {
                        let client_failure =
                            communication_error(error) == CommunicationProbeError::RemoteFailure;
                        let mut mapped = refresh_error(qbit_error(error).0);
                        mapped.client_failure = client_failure;
                        mapped
                    });
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
/// Bounded scoped item observation for any configured download client.
async fn refresh_items(
    operation: &http::HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
    domain: MediaDomain,
) -> std::result::Result<Vec<qbittorrent::DownloadItem>, qbittorrent::QbitError> {
    match settings {
        ProviderSettings::Sabnzbd { .. } => {
            sabnzbd::refresh_pages(operation, settings, credentials, domain).await
        }
        ProviderSettings::Nzbget { .. } => {
            nzbget::refresh_pages(operation, settings, credentials, domain).await
        }
        settings if settings.is_label_torrent() => {
            label_client::refresh_pages(operation, settings, credentials, domain).await
        }
        ProviderSettings::Transmission { .. } => {
            transmission::refresh_pages(operation, settings, credentials, domain).await
        }
        _ => qbittorrent::refresh_pages(operation, settings, credentials, domain).await,
    }
}
impl RefreshClient {
    /// SABnzbd reports only a completed job's final folder. The files beneath it are listed from
    /// the explicitly mapped local path; an unmapped output is a visible blocker, never a guess.
    async fn usenet_details(
        &self,
        job: sabnzbd::UsenetJob,
        host: &str,
        target: &crate::db::MediaTarget,
    ) -> std::result::Result<download::DownloadDetails, AutomationError> {
        let Some(storage) = job.storage else {
            return Ok(download::DownloadDetails {
                item: job.item,
                save_path: String::new(),
                files: Vec::new(),
            });
        };
        let domain = match target {
            crate::db::MediaTarget::Episode(_) => MediaDomain::Tv,
            crate::db::MediaTarget::Movie(_) => MediaDomain::Movies,
        };
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
                host: host.to_owned(),
                path: storage.clone(),
                direction: crate::remote_paths::Direction::RemoteToLocal,
            },
        )
        .await
        .map_err(|_| RefreshError::new("path_mapping_missing", false))?;
        drop(connection);
        if resolution.mapping_id.is_none() {
            return Err(RefreshError::new("path_mapping_missing", false));
        }
        let local = local_download_root(&resolution.output)
            .ok_or_else(|| RefreshError::new("path_mapping_missing", false))?;
        let listing = tokio::task::spawn_blocking(move || {
            download::list_local_output(std::path::Path::new(&local))
        })
        .await
        .map_err(|_| RefreshError::new("download_unavailable", true))?
        .map_err(|_| RefreshError::new("download_unavailable", true))?;
        let separator = if storage.contains('\\') && !storage.starts_with('/') {
            '\\'
        } else {
            '/'
        };
        let file = |index: usize, name: String, size_bytes: u64| qbittorrent::TorrentFile {
            index: index as u32,
            name,
            size_bytes,
            progress: 1.0,
            priority: 1,
        };
        let (save_path, files) = match listing {
            download::LocalListing::Directory(entries) => (
                storage,
                entries
                    .into_iter()
                    .enumerate()
                    .map(|(i, (name, size))| file(i, name, size))
                    .collect(),
            ),
            download::LocalListing::File(size) => {
                let trimmed = storage.trim_end_matches(['/', '\\']);
                let (parent, name) = trimmed
                    .rsplit_once(separator)
                    .ok_or_else(|| RefreshError::new("download_unavailable", false))?;
                (parent.to_owned(), vec![file(0, name.to_owned(), size)])
            }
            download::LocalListing::Missing => {
                return Err(RefreshError::new("download_unavailable", true));
            }
        };
        Ok(download::DownloadDetails {
            item: job.item,
            save_path,
            files,
        })
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
    let mut tv_usenet = None;
    let mut movie_usenet = None;
    let mut tv_nzbget = None;
    let mut movie_nzbget = None;
    let mut tv_torrent = None;
    let mut movie_torrent = None;
    let mut tv_label = None;
    let mut movie_label = None;
    while let Some(scope) = scopes.next().await? {
        let media: String = scope.get(0)?;
        if implementation == "transmission" {
            let directory: Option<String> = conn
                .query(
                    "SELECT directory FROM provider_scope_directories WHERE provider_id=? AND media_type=?",
                    params![id, media.clone()],
                )
                .await?
                .next()
                .await?
                .map(|row| row.get(0))
                .transpose()?;
            let placement = match (scope.get::<Option<String>>(3)?, directory) {
                (Some(category), None) => TorrentPlacement::Category { category },
                (None, Some(directory)) => TorrentPlacement::Directory { directory },
                _ => return Err(corrupt()),
            };
            let item = TransmissionScope {
                placement,
                recent_priority: TorrentQueuePriority::stored(scope.get::<i64>(5)?)?,
                older_priority: TorrentQueuePriority::stored(scope.get::<i64>(6)?)?,
                add_paused: match scope.get::<String>(9)?.as_str() {
                    "started" => false,
                    "stopped" => true,
                    _ => return Err(corrupt()),
                },
                tags: Some(selection::read_tags(conn, id, &media).await?),
            };
            if media == "tv" {
                tv_torrent = Some(item)
            } else {
                movie_torrent = Some(item)
            }
        } else if LABEL_IMPLEMENTATIONS.contains(&implementation.as_str()) {
            let item = TorrentLabelScope {
                category: scope.get(3)?,
                recent_priority: TorrentQueuePriority::stored(scope.get::<i64>(5)?)?,
                older_priority: TorrentQueuePriority::stored(scope.get::<i64>(6)?)?,
                add_paused: match scope.get::<String>(9)?.as_str() {
                    "started" => false,
                    "stopped" => true,
                    _ => return Err(corrupt()),
                },
                tags: Some(selection::read_tags(conn, id, &media).await?),
            };
            if media == "tv" {
                tv_label = Some(item)
            } else {
                movie_label = Some(item)
            }
        } else if implementation == "nzbget" {
            let item = NzbgetScope {
                category: scope.get(3)?,
                recent_priority: NzbgetPriority::stored(scope.get::<i64>(5)?)?,
                older_priority: NzbgetPriority::stored(scope.get::<i64>(6)?)?,
                add_paused: match scope.get::<String>(9)?.as_str() {
                    "started" => false,
                    "stopped" => true,
                    _ => return Err(corrupt()),
                },
                tags: Some(selection::read_tags(conn, id, &media).await?),
            };
            if media == "tv" {
                tv_nzbget = Some(item)
            } else {
                movie_nzbget = Some(item)
            }
        } else if implementation == "sabnzbd" {
            let item = UsenetScope {
                category: scope.get(3)?,
                recent_priority: UsenetPriority::stored(scope.get::<i64>(5)?)?,
                older_priority: UsenetPriority::stored(scope.get::<i64>(6)?)?,
                tags: Some(selection::read_tags(conn, id, &media).await?),
            };
            if media == "tv" {
                tv_usenet = Some(item)
            } else {
                movie_usenet = Some(item)
            }
        } else if implementation == "qbittorrent" {
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
                tags: Some(selection::read_tags(conn, id, &media).await?),
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
        "sabnzbd" => ProviderSettings::Sabnzbd {
            endpoint,
            tv: tv_usenet,
            movies: movie_usenet,
        },
        "nzbget" => ProviderSettings::Nzbget {
            endpoint,
            tv: tv_nzbget,
            movies: movie_nzbget,
        },
        "transmission" => ProviderSettings::Transmission {
            endpoint,
            tv: tv_torrent,
            movies: movie_torrent,
        },
        label => ProviderSettings::from_label_parts(label, endpoint, tv_label, movie_label)
            .ok_or_else(corrupt)?,
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
                // A protocol mismatch is likewise stored, not erased (the IndexerDownloadClient
                // health check, like the references', checks presence only); selection rejects
                // it at dispatch as `protocol_mismatch`.
                if conn.query("SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=? AND p.implementation IN (SELECT implementation FROM provider_implementations WHERE kind='download_client') AND s.media_type=?", params![client.to_string(), domain]).await?.next().await?.is_none() {
                    return Err(bad());
                }
            }
        }
    }
    let mut retained_tags = std::collections::BTreeMap::new();
    let client_tags: [(&str, Option<&Option<Vec<i64>>>); 2] = match settings {
        ProviderSettings::Qbittorrent { tv, movies, .. } => [
            ("tv", tv.as_ref().map(|s| &s.tags)),
            ("movies", movies.as_ref().map(|s| &s.tags)),
        ],
        ProviderSettings::Sabnzbd { tv, movies, .. } => [
            ("tv", tv.as_ref().map(|s| &s.tags)),
            ("movies", movies.as_ref().map(|s| &s.tags)),
        ],
        ProviderSettings::Nzbget { tv, movies, .. } => [
            ("tv", tv.as_ref().map(|s| &s.tags)),
            ("movies", movies.as_ref().map(|s| &s.tags)),
        ],
        ProviderSettings::Transmission { tv, movies, .. } => [
            ("tv", tv.as_ref().map(|s| &s.tags)),
            ("movies", movies.as_ref().map(|s| &s.tags)),
        ],
        ProviderSettings::Deluge { tv, movies, .. }
        | ProviderSettings::Rtorrent { tv, movies, .. }
        | ProviderSettings::Downloadstation { tv, movies, .. }
        | ProviderSettings::Flood { tv, movies, .. }
        | ProviderSettings::Aria2 { tv, movies, .. }
        | ProviderSettings::Utorrent { tv, movies, .. } => [
            ("tv", tv.as_ref().map(|s| &s.tags)),
            ("movies", movies.as_ref().map(|s| &s.tags)),
        ],
        _ => [("tv", None), ("movies", None)],
    };
    if settings.is_download_client() {
        for (media, scope) in client_tags {
            if let Some(scope_tags) = scope {
                let tags = match scope_tags {
                    Some(tags) => tags.clone(),
                    None => selection::read_tags(conn, id, media).await?,
                };
                selection::validate_tags(conn, media, &tags).await?;
                retained_tags.insert(media, tags);
            }
        }
    }
    // Directory placements reference their scope row; remove them explicitly before the scopes.
    conn.execute(
        "DELETE FROM provider_scope_directories WHERE provider_id=?",
        [id],
    )
    .await?;
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
        ProviderSettings::Sabnzbd { tv, movies, .. } => {
            for (domain, s) in tv
                .iter()
                .map(|s| ("tv", s))
                .chain(movies.iter().map(|s| ("movies", s)))
            {
                conn.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority) VALUES(?,?,?,?,?,?)",params![id,implementation,domain,s.category.clone(),s.recent_priority.wire(),s.older_priority.wire()]).await?;
            }
        }
        ProviderSettings::Nzbget { tv, movies, .. } => {
            for (domain, s) in tv
                .iter()
                .map(|s| ("tv", s))
                .chain(movies.iter().map(|s| ("movies", s)))
            {
                let state = if s.add_paused { "stopped" } else { "started" };
                conn.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state) VALUES(?,?,?,?,?,?,?)",params![id,implementation,domain,s.category.clone(),s.recent_priority.code(),s.older_priority.code(),state]).await?;
            }
        }
        ProviderSettings::Deluge { tv, movies, .. }
        | ProviderSettings::Rtorrent { tv, movies, .. }
        | ProviderSettings::Downloadstation { tv, movies, .. }
        | ProviderSettings::Flood { tv, movies, .. }
        | ProviderSettings::Aria2 { tv, movies, .. }
        | ProviderSettings::Utorrent { tv, movies, .. } => {
            for (domain, s) in tv
                .iter()
                .map(|s| ("tv", s))
                .chain(movies.iter().map(|s| ("movies", s)))
            {
                let state = if s.add_paused { "stopped" } else { "started" };
                conn.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state) VALUES(?,?,?,?,?,?,?)",params![id,implementation,domain,s.category.clone(),s.recent_priority.wire(),s.older_priority.wire(),state]).await?;
            }
        }
        ProviderSettings::Transmission { tv, movies, .. } => {
            for (domain, s) in tv
                .iter()
                .map(|s| ("tv", s))
                .chain(movies.iter().map(|s| ("movies", s)))
            {
                let category = match &s.placement {
                    TorrentPlacement::Category { category } => Some(category.clone()),
                    TorrentPlacement::Directory { .. } => None,
                };
                let state = if s.add_paused { "stopped" } else { "started" };
                conn.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state) VALUES(?,?,?,?,?,?,?)",params![id,implementation,domain,category,s.recent_priority.wire(),s.older_priority.wire(),state]).await?;
                if let TorrentPlacement::Directory { directory } = &s.placement {
                    conn.execute("INSERT INTO provider_scope_directories(provider_id,media_type,implementation,directory) VALUES(?,?,?,?)",params![id,domain,implementation,directory.clone()]).await?;
                }
            }
        }
    }
    if settings.is_download_client() {
        for (media, tags) in retained_tags {
            for tag in tags {
                conn.execute(
                    "INSERT INTO download_client_tags(provider_id,media_type,tag_id) VALUES(?,?,?)",
                    params![id, media, tag],
                )
                .await?;
            }
        }
        backoff::reconcile(conn, id).await?;
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
                ProviderSettings::Sabnzbd { tv, movies, .. } => {
                    client = true;
                    (&mut self.clients, tv.is_some(), movies.is_some())
                }
                ProviderSettings::Nzbget { tv, movies, .. } => {
                    client = true;
                    (&mut self.clients, tv.is_some(), movies.is_some())
                }
                ProviderSettings::Transmission { tv, movies, .. } => {
                    client = true;
                    (&mut self.clients, tv.is_some(), movies.is_some())
                }
                ProviderSettings::Deluge { tv, movies, .. }
                | ProviderSettings::Rtorrent { tv, movies, .. }
                | ProviderSettings::Downloadstation { tv, movies, .. }
                | ProviderSettings::Flood { tv, movies, .. }
                | ProviderSettings::Aria2 { tv, movies, .. }
                | ProviderSettings::Utorrent { tv, movies, .. } => {
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
    indexer_observation: Option<(indexer_backoff::Observation, Option<MediaDomain>)>,
    client_observation: Option<backoff::Observation>,
    grace_elapsed: bool,
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
        for domain in provider.settings.client_domains() {
            crate::health::communication_status_changed(&tx, domain)
                .await
                .map_err(|e| Error::Plain(e.0, e.1, "Health observation could not be recorded"))?;
        }
    }
    if let Some((observation, failed_domain)) = indexer_observation {
        for media in [MediaDomain::Tv, MediaDomain::Movies] {
            if indexer_backoff::has_scope(&provider.settings, media)
                && failed_domain.is_none_or(|domain| domain == media)
            {
                indexer_backoff::apply(&tx, provider, media, Some(observation), tested_at).await?;
            }
        }
    }
    if let Some(observation) = client_observation {
        for domain in provider.settings.client_domains() {
            backoff::apply(
                &tx,
                &provider.id.to_string(),
                provider.revision,
                domain,
                observation,
                tested_at,
                grace_elapsed,
            )
            .await?;
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
    if matches!(settings, ProviderSettings::Sabnzbd { .. }) {
        sabnzbd::test_connection(operation, settings, credentials.as_ref())
            .await
            .map(ProviderTestOutcome::UsenetClient)
            .map_err(sab_test_error)
    } else if matches!(settings, ProviderSettings::Nzbget { .. }) {
        nzbget::test_connection(operation, settings, credentials.as_ref())
            .await
            .map(ProviderTestOutcome::NzbgetClient)
            .map_err(nzbget_test_error)
    } else if settings.is_label_torrent() {
        label_client::test_connection(operation, settings, credentials.as_ref())
            .await
            .map(ProviderTestOutcome::LabelClient)
            .map_err(label_test_error)
    } else if matches!(settings, ProviderSettings::Transmission { .. }) {
        transmission::test_connection(operation, settings, credentials.as_ref())
            .await
            .map(ProviderTestOutcome::TorrentClient)
            .map_err(transmission_test_error)
    } else if matches!(settings, ProviderSettings::Qbittorrent { .. }) {
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
    let is_indexer = !provider.settings.is_download_client();
    let operation = if is_indexer {
        transport.indexer_operation(provider.id)
    } else {
        transport.operation(provider.id)
    }
    .map_err(|e| http_error(e).0)?;
    let work = async {
        let (result, observation, client_observation) = if is_indexer {
            let scoped = indexer::test_with_scope(
                &operation,
                &provider.settings,
                &IndexerAccess::from_credentials(&credentials),
            )
            .await;
            let failed_domain = scoped.as_ref().err().and_then(|(_, domain)| *domain);
            let result = scoped.map_err(|(error, _)| error);
            let observation =
                indexer_backoff::observation(&result).map(|value| (value, failed_domain));
            (
                result
                    .map(ProviderTestOutcome::Indexer)
                    .map_err(|error| indexer_error(error, &operation)),
                observation,
                None,
            )
        } else if matches!(provider.settings, ProviderSettings::Nzbget { .. }) {
            let result =
                nzbget::test_connection(&operation, &provider.settings, credentials.as_ref()).await;
            let observation = match result.as_ref() {
                Ok(_) => Some(backoff::Observation::Success),
                Err(failure)
                    if communication_error(failure.error)
                        == CommunicationProbeError::RemoteFailure =>
                {
                    Some(backoff::Observation::Failure)
                }
                Err(_) => None,
            };
            (
                result
                    .map(ProviderTestOutcome::NzbgetClient)
                    .map_err(nzbget_test_error),
                None,
                observation,
            )
        } else if provider.settings.is_label_torrent() {
            let result =
                label_client::test_connection(&operation, &provider.settings, credentials.as_ref())
                    .await;
            let observation = match result.as_ref() {
                Ok(_) => Some(backoff::Observation::Success),
                Err(failure)
                    if communication_error(failure.error)
                        == CommunicationProbeError::RemoteFailure =>
                {
                    Some(backoff::Observation::Failure)
                }
                Err(_) => None,
            };
            (
                result
                    .map(ProviderTestOutcome::LabelClient)
                    .map_err(label_test_error),
                None,
                observation,
            )
        } else if matches!(provider.settings, ProviderSettings::Transmission { .. }) {
            let result =
                transmission::test_connection(&operation, &provider.settings, credentials.as_ref())
                    .await;
            let observation = match result.as_ref() {
                Ok(_) => Some(backoff::Observation::Success),
                Err(failure)
                    if communication_error(failure.error)
                        == CommunicationProbeError::RemoteFailure =>
                {
                    Some(backoff::Observation::Failure)
                }
                Err(_) => None,
            };
            (
                result
                    .map(ProviderTestOutcome::TorrentClient)
                    .map_err(transmission_test_error),
                None,
                observation,
            )
        } else if matches!(provider.settings, ProviderSettings::Sabnzbd { .. }) {
            let result =
                sabnzbd::test_connection(&operation, &provider.settings, credentials.as_ref())
                    .await;
            let observation = match result.as_ref() {
                Ok(_) => Some(backoff::Observation::Success),
                Err(failure)
                    if communication_error(failure.error)
                        == CommunicationProbeError::RemoteFailure =>
                {
                    Some(backoff::Observation::Failure)
                }
                Err(_) => None,
            };
            (
                result
                    .map(ProviderTestOutcome::UsenetClient)
                    .map_err(sab_test_error),
                None,
                observation,
            )
        } else {
            let result =
                qbittorrent::test_connection(&operation, &provider.settings, credentials.as_ref())
                    .await;
            let observation = match result.as_ref() {
                Ok(_) => Some(backoff::Observation::Success),
                Err(error)
                    if communication_error(*error) == CommunicationProbeError::RemoteFailure =>
                {
                    Some(backoff::Observation::Failure)
                }
                Err(_) => None,
            };
            (
                result
                    .map(ProviderTestOutcome::DownloadClient)
                    .map_err(qbit_error),
                None,
                observation,
            )
        };
        operation.ensure_active().map_err(|e| http_error(e).0)?;
        match result {
            Ok(result) => {
                let tested_at = record_test(
                    &context.db,
                    &provider,
                    None,
                    observation,
                    client_observation,
                    context.backoff_started.elapsed() >= backoff::STARTUP_GRACE,
                )
                .await?;
                bounded(ProviderTestResult {
                    provider_id: provider.id,
                    revision: provider.revision,
                    tested_at,
                    result,
                })
            }
            Err(error) => {
                let (error, code) = error;
                record_test(
                    &context.db,
                    &provider,
                    Some(code),
                    observation,
                    client_observation,
                    context.backoff_started.elapsed() >= backoff::STARTUP_GRACE,
                )
                .await?;
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
    if provider.settings.is_download_client() {
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
        .indexer_operation(provider.id)
        .map_err(|e| http_error(e).0)?;
    let work = async {
        indexer_backoff::admit(&context.db, &provider, indexer_backoff::domain(&input)).await?;
        let result = indexer::search(
            &operation,
            &provider.settings,
            &IndexerAccess::from_credentials(&credentials),
            &input,
        )
        .await;
        operation.ensure_active().map_err(|e| http_error(e).0)?;
        current_revision(&context.db, &provider).await?;
        indexer_backoff::record(
            &context.db,
            &provider,
            indexer_backoff::domain(&input),
            indexer_backoff::observation(&result),
        )
        .await?;
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

/// Usenet test problems keep the stored static code set while giving an actionable message.
fn sab_test_error(failure: sabnzbd::SabTestFailure) -> (Error, &'static str) {
    use sabnzbd::SabnzbdProblem as P;
    let (code, message) = match failure.problem {
        None => return qbit_error(failure.error),
        Some(P::CategoryMissing) => (
            "sabnzbd_category_missing",
            "A configured SABnzbd category does not exist; create it in SABnzbd",
        ),
        Some(P::JobFoldersDisabled) => (
            "sabnzbd_job_folders_disabled",
            "A configured SABnzbd category does not create job folders; enable them",
        ),
        Some(P::SortingEnabled) => (
            "sabnzbd_sorting_enabled",
            "SABnzbd sorting applies to a configured category; disable it so imports find completed files",
        ),
    };
    (
        Error::Plain(StatusCode::UNPROCESSABLE_ENTITY, code, message),
        "unsupported",
    )
}
/// NZBGet test problems keep the stored static code set while giving an actionable message.
fn nzbget_test_error(failure: nzbget::NzbgetTestFailure) -> (Error, &'static str) {
    use nzbget::NzbgetProblem as P;
    let (code, message) = match failure.problem {
        None => return qbit_error(failure.error),
        Some(P::CategoryMissing) => (
            "nzbget_category_missing",
            "A configured NZBGet category does not exist; create it in NZBGet",
        ),
        Some(P::KeepHistoryZero) => (
            "nzbget_keep_history_zero",
            "NZBGet KeepHistory is 0; completed downloads vanish before they can be imported",
        ),
        Some(P::KeepHistoryOverMax) => (
            "nzbget_keep_history_over_max",
            "NZBGet KeepHistory exceeds 25000 days; lower it",
        ),
    };
    (
        Error::Plain(StatusCode::UNPROCESSABLE_ENTITY, code, message),
        "unsupported",
    )
}
/// Label-client test problems keep the stored static code set while giving an actionable message.
fn label_test_error(failure: label_client::LabelTestFailure) -> (Error, &'static str) {
    use label_client::LabelProblem as P;
    let (code, message) = match failure.problem {
        None => return qbit_error(failure.error),
        Some(P::LabelsUnavailable) => (
            "label_plugin_unavailable",
            "The client cannot hold labels; enable its Label plugin so TV and movies stay isolated",
        ),
    };
    (
        Error::Plain(StatusCode::UNPROCESSABLE_ENTITY, code, message),
        "unsupported",
    )
}
/// Transmission test problems keep the stored static code set while giving an actionable message.
fn transmission_test_error(
    failure: transmission::TransmissionTestFailure,
) -> (Error, &'static str) {
    use transmission::TransmissionProblem as P;
    let (code, message) = match failure.problem {
        None => return qbit_error(failure.error),
        Some(P::DownloadDirInvalid) => (
            "transmission_download_dir_invalid",
            "A category scope needs Transmission's download directory, which is missing or not an absolute path",
        ),
        Some(P::DomainsOverlap) => (
            "transmission_domains_overlap",
            "TV and movie download locations overlap on this Transmission client; use separate categories or directories",
        ),
    };
    (
        Error::Plain(StatusCode::UNPROCESSABLE_ENTITY, code, message),
        "unsupported",
    )
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
    if !provider.settings.is_download_client() {
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
        let result = match &provider.settings {
            ProviderSettings::Sabnzbd { .. } => {
                sabnzbd::query(&operation, &provider.settings, credentials.as_ref(), &input).await
            }
            ProviderSettings::Nzbget { .. } => {
                nzbget::query(&operation, &provider.settings, credentials.as_ref(), &input).await
            }
            settings if settings.is_label_torrent() => {
                label_client::query(&operation, &provider.settings, credentials.as_ref(), &input)
                    .await
            }
            ProviderSettings::Transmission { .. } => {
                transmission::query(&operation, &provider.settings, credentials.as_ref(), &input)
                    .await
            }
            _ => {
                qbittorrent::query(&operation, &provider.settings, credentials.as_ref(), &input)
                    .await
            }
        };
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
    download_client_tags: Option<Vec<i64>>,
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
    if let ProviderSettings::Qbittorrent { tv, movies, .. } = &mut settings {
        let scope = tv.as_mut().or(movies.as_mut()).ok_or(INVALID)?;
        scope.tags = download_client_tags;
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
    if !provider.settings.client_scoped(input.media_type) {
        return Err(bad().into_response());
    }
    let url = url::Url::parse(provider.settings.endpoint()).map_err(|_| bad().into_response())?;
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
