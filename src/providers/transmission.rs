//! Transmission RPC operations over the bounded provider transport.
//!
//! The wire contract follows Transmission's published RPC specification (`docs/rpc-spec.md`):
//! one JSON document `{"method","arguments","tag"}` POSTed to `<endpoint>/rpc`, a `409` reply
//! carrying `X-Transmission-Session-Id` that must be echoed (the server does not execute the
//! method when it answers 409), optional HTTP basic authentication and `result:"success"`
//! envelopes. Which settings and behaviours exist (per-domain subfolder/label or directory,
//! recent/older queue priority, add paused, completion/seeding detection) was inventoried from
//! the pinned Sonarr/Radarr references; nothing here translates their code.
//!
//! No scheduler, automatic mutation retry or local filesystem access happens here: callers own
//! durable intent, path mapping and imports. Remote paths stay private protocol facts.
use super::{
    ClientRetryKind, Credentials, ProviderSettings, TorrentPlacement, TorrentQueuePriority,
    TransmissionScope,
    http::{HttpError, HttpOperation, HttpRequestBody},
    qbittorrent::{
        self, AddOutcome, AddSource, ClientStatus, ClientStatusSnapshot, Control,
        DownloadDiagnostic, DownloadItem, DownloadPage, DownloadQuery, DownloadStatus, QbitError,
        QbitOptions, Reconciliation, RootSource, SubmissionError, SubmissionIdentity, TorrentFile,
        digest_hex, endpoint_locality, target_domain, validate_remote_path,
    },
};
use crate::{api::MediaDomain, db::MediaTarget};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// Shared client error vocabulary (named after its first protocol; not qBittorrent-specific).
pub type TransmissionError = QbitError;
type Result<T> = std::result::Result<T, TransmissionError>;

/// Transmission 2.40 (RPC 14) introduced the 0–6 status codes, queues and `queue-move-*`; the
/// pinned references reject anything older.
pub const MIN_RPC_VERSION: u64 = 14;
/// The torrent `labels` field exists from RPC 16 (Transmission 3.00).
const LABEL_FIELD_RPC_VERSION: u64 = 16;
/// Labels are relied on for domain ownership from Transmission 4.0 (RPC 17), as observed in both
/// references; earlier servers are isolated by download directory only.
pub const LABELS_RPC_VERSION: u64 = 17;
/// The newest legacy-envelope protocol level this client speaks. A server whose oldest supported
/// level is newer cannot be driven safely.
const CLIENT_RPC_VERSION: u64 = 17;
const MAX_SESSION_ROTATIONS: usize = 8;
const MAX_LISTED_TORRENTS: usize = 8192;
const MAX_DOMAIN_ITEMS: usize = 1000;
const MAX_REFRESH_BYTES: usize = 1024 * 1024;
const MAX_FILES: usize = 4096;
const MAX_LABELS: usize = 256;
const MAX_ETA_SECONDS: i64 = 365 * 24 * 60 * 60;
const MAX_RATIO: f64 = 1_000_000.0;
const MAX_JSON_NUMBER: u64 = 9_007_199_254_740_991;
const LIST_FIELDS: [&str; 3] = ["id", "hashString", "downloadDir"];
const ITEM_FIELDS: [&str; 24] = [
    "id",
    "hashString",
    "name",
    "downloadDir",
    "status",
    "totalSize",
    "sizeWhenDone",
    "leftUntilDone",
    "percentDone",
    "metadataPercentComplete",
    "isFinished",
    "isStalled",
    "eta",
    "error",
    "errorString",
    "rateDownload",
    "rateUpload",
    "uploadRatio",
    "secondsSeeding",
    "queuePosition",
    "seedRatioLimit",
    "seedRatioMode",
    "seedIdleLimit",
    "seedIdleMode",
];

/// Torrent status codes from the RPC specification (RPC 14+).
const STOPPED: u64 = 0;
const CHECK_WAIT: u64 = 1;
const CHECK: u64 = 2;
const DOWNLOAD_WAIT: u64 = 3;
const DOWNLOADING: u64 = 4;
const SEED_WAIT: u64 = 5;
const SEEDING: u64 = 6;
/// `error` codes: 1 tracker warning, 2 tracker error, 3 local error.
const TRACKER_ERROR: u64 = 2;
const LOCAL_ERROR: u64 = 3;

#[derive(Serialize, ts_rs::TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransmissionTestWarning {
    /// The server predates labels (Transmission 4.0); category scopes are isolated by subfolder.
    LabelsUnsupported,
    /// A scope moves new torrents to the queue top, but the download queue is disabled.
    QueueDisabled,
}
#[derive(Serialize, ts_rs::TS, Debug)]
pub struct TransmissionTest {
    pub transmission_version: String,
    pub rpc_version: u32,
    pub domains: Vec<MediaDomain>,
    pub labels_supported: bool,
    pub warnings: Vec<TransmissionTestWarning>,
}
/// A static, user-actionable configuration problem detected by a connection test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransmissionProblem {
    /// A category scope needs the client's download directory, which is missing or unusable.
    DownloadDirInvalid,
    /// The TV and movie download locations nest inside each other.
    DomainsOverlap,
}
#[derive(Debug)]
pub struct TransmissionTestFailure {
    pub error: TransmissionError,
    pub problem: Option<TransmissionProblem>,
}
impl From<TransmissionError> for TransmissionTestFailure {
    fn from(error: TransmissionError) -> Self {
        Self {
            error,
            problem: None,
        }
    }
}

/// Retains the release payload in memory only; its identity is the journal-safe part.
pub struct PreparedTorrent {
    identity: SubmissionIdentity,
    source: AddSource,
    recent: bool,
    options: QbitOptions,
}
impl PreparedTorrent {
    pub fn identity(&self) -> &SubmissionIdentity {
        &self.identity
    }
    pub(crate) fn into_identity(self) -> SubmissionIdentity {
        self.identity
    }
}

fn scope(settings: &ProviderSettings, domain: MediaDomain) -> Result<&TransmissionScope> {
    settings.validate().map_err(|_| QbitError::InvalidRequest)?;
    match settings {
        ProviderSettings::Transmission { tv, movies, .. } => match domain {
            MediaDomain::Tv => tv.as_ref(),
            MediaDomain::Movies => movies.as_ref(),
        }
        .ok_or(QbitError::ScopeConflict),
        _ => Err(QbitError::ScopeConflict),
    }
}
/// Eligibility tags do not change remote submission behaviour; exclude them like qBittorrent.
pub(crate) fn settings_fingerprint(settings: &ProviderSettings) -> Result<String> {
    let mut settings = settings.clone();
    if let ProviderSettings::Transmission { tv, movies, .. } = &mut settings {
        for scope in tv.iter_mut().chain(movies.iter_mut()) {
            scope.tags = None;
        }
    } else {
        return Err(QbitError::ScopeConflict);
    }
    let bytes = serde_json::to_vec(&settings).map_err(|_| QbitError::InvalidRequest)?;
    Ok(digest_hex(&ring::digest::SHA256, &bytes))
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
/// Standard padded base64 (RFC 4648 §4), used for basic authentication and `metainfo`.
fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for (index, shift) in [18u32, 12, 6, 0].into_iter().enumerate() {
            if index <= chunk.len() {
                out.push(BASE64[((n >> shift) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Path segments of a rooted remote path; drive-letter paths compare case-insensitively.
fn segments(path: &str) -> Option<(bool, Vec<String>)> {
    let drive = path.len() >= 3
        && path.as_bytes()[0].is_ascii_alphabetic()
        && path.as_bytes()[1] == b':'
        && matches!(path.as_bytes()[2], b'/' | b'\\');
    if !drive && !path.starts_with('/') {
        return None;
    }
    let parts = path
        .split(['/', '\\'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            if drive {
                part.to_lowercase()
            } else {
                part.to_owned()
            }
        })
        .collect();
    Some((drive, parts))
}
/// `path` is `root` or lies beneath it. Unrooted or differently-rooted paths never match.
pub(crate) fn within(path: &str, root: &str) -> bool {
    match (segments(path), segments(root)) {
        (Some((a, path)), Some((b, root))) => {
            a == b && path.len() >= root.len() && path[..root.len()] == root[..]
        }
        _ => false,
    }
}
fn join(directory: &str, child: &str) -> String {
    let separator = if directory.contains('\\') && !directory.starts_with('/') {
        '\\'
    } else {
        '/'
    };
    format!(
        "{}{separator}{child}",
        directory.trim_end_matches(['/', '\\'])
    )
}
fn rooted(path: &str) -> bool {
    segments(path).is_some()
}

struct Rpc<'o, 'a> {
    operation: &'o HttpOperation<'a>,
    url: String,
    authorization: Option<String>,
    secrets: Vec<String>,
    session: Option<String>,
    rotations: usize,
    tag: u64,
}
impl<'o, 'a> Rpc<'o, 'a> {
    fn new(
        operation: &'o HttpOperation<'a>,
        settings: &ProviderSettings,
        credentials: Option<&Credentials>,
    ) -> Result<Self> {
        let ProviderSettings::Transmission { endpoint, .. } = settings else {
            return Err(QbitError::ScopeConflict);
        };
        let (authorization, secrets) = match credentials {
            None => (None, Vec::new()),
            Some(Credentials::UsernamePassword { username, password }) => {
                // RFC 7617: the user-id cannot contain a colon.
                if username.contains(':')
                    || username.len() > 4096
                    || password.len() > 4096
                    || username.chars().any(char::is_control)
                    || password.chars().any(char::is_control)
                {
                    return Err(QbitError::InvalidRequest);
                }
                (
                    Some(format!(
                        "Basic {}",
                        base64(format!("{username}:{password}").as_bytes())
                    )),
                    [username, password]
                        .into_iter()
                        .filter(|s| !s.is_empty())
                        .cloned()
                        .collect(),
                )
            }
            Some(_) => return Err(QbitError::InvalidRequest),
        };
        let mut url = url::Url::parse(endpoint).map_err(|_| QbitError::InvalidRequest)?;
        let path = format!("{}/rpc", url.path().trim_end_matches('/'));
        url.set_path(&path);
        Ok(Self {
            operation,
            url: url.into(),
            authorization,
            secrets,
            session: None,
            rotations: 0,
            tag: 0,
        })
    }
    fn redact(&self, mut value: String) -> String {
        for secret in &self.secrets {
            value = value.replace(secret.as_str(), "[redacted]");
        }
        value
    }
    /// One RPC call, returning the server's `result` string and `arguments` object. A 409
    /// handshake is answered once per call with the newly issued session id; the specification
    /// guarantees the method was not executed, so even a mutation is safe to resend.
    async fn call_result(&mut self, method: &str, arguments: Value) -> Result<(String, Value)> {
        self.tag = self.tag.checked_add(1).ok_or(QbitError::InvalidRequest)?;
        let body =
            serde_json::to_vec(&json!({"method":method,"arguments":arguments,"tag":self.tag}))
                .map_err(|_| QbitError::InvalidRequest)?;
        for attempt in 0..2 {
            let mut headers = Vec::new();
            if let Some(value) = &self.authorization {
                headers.push(("Authorization".to_owned(), value.clone()));
            }
            if let Some(id) = &self.session {
                headers.push(("X-Transmission-Session-Id".to_owned(), id.clone()));
            }
            let response = self
                .operation
                .request(
                    &self.url,
                    &[],
                    &headers,
                    HttpRequestBody::Json(body.clone()),
                )
                .await?;
            if response.status == 409 {
                let id = response.session_id.ok_or(QbitError::InvalidResponse)?;
                if attempt == 1
                    || self.session.as_deref() == Some(id.as_str())
                    || self.rotations == MAX_SESSION_ROTATIONS
                {
                    return Err(QbitError::InvalidResponse);
                }
                self.rotations += 1;
                self.session = Some(id);
                continue;
            }
            if !(200..300).contains(&response.status) {
                // A wrong URL base or an unavailable daemon is a connection failure.
                return Err(if response.status == 404 || response.status >= 500 {
                    QbitError::Http(HttpError::Transport)
                } else {
                    QbitError::InvalidResponse
                });
            }
            let value: Value =
                serde_json::from_slice(&response.body).map_err(|_| QbitError::InvalidResponse)?;
            if value.get("tag").and_then(Value::as_u64) != Some(self.tag) {
                return Err(QbitError::InvalidResponse);
            }
            let result = value
                .get("result")
                .and_then(Value::as_str)
                .filter(|r| !r.is_empty() && r.len() <= 1024)
                .ok_or(QbitError::InvalidResponse)?
                .to_owned();
            let arguments = match value.get("arguments") {
                Some(Value::Object(_)) => value["arguments"].clone(),
                None | Some(Value::Null) => json!({}),
                Some(_) => return Err(QbitError::InvalidResponse),
            };
            return Ok((result, arguments));
        }
        Err(QbitError::InvalidResponse)
    }
    /// Server rejections (`result` other than success) are classified without reflecting text.
    async fn call(&mut self, method: &str, arguments: Value) -> Result<Value> {
        let (result, arguments) = self.call_result(method, arguments).await?;
        if result != "success" {
            return Err(QbitError::Rejected);
        }
        Ok(arguments)
    }
    async fn session(&mut self) -> Result<Session> {
        let value = self.call("session-get", json!({})).await?;
        let rpc_version = value
            .get("rpc-version")
            .and_then(Value::as_u64)
            .filter(|v| *v <= u64::from(u32::MAX))
            .ok_or(QbitError::InvalidResponse)?;
        let minimum = match value.get("rpc-version-minimum") {
            None | Some(Value::Null) => None,
            Some(v) => Some(v.as_u64().ok_or(QbitError::InvalidResponse)?),
        };
        if rpc_version < MIN_RPC_VERSION || minimum.is_some_and(|m| m > CLIENT_RPC_VERSION) {
            return Err(QbitError::UnsupportedVersion);
        }
        let version = match value.get("version") {
            Some(Value::String(v))
                if !v.is_empty()
                    && v.len() <= 64
                    && v.bytes().all(|b| (0x20..0x7f).contains(&b)) =>
            {
                v.clone()
            }
            None | Some(Value::Null) => "unknown".to_owned(),
            Some(_) => return Err(QbitError::InvalidResponse),
        };
        let download_dir = match value.get("download-dir") {
            None | Some(Value::Null) => String::new(),
            Some(Value::String(v)) if v.len() <= 4096 && !v.chars().any(char::is_control) => {
                v.clone()
            }
            Some(_) => return Err(QbitError::InvalidResponse),
        };
        Ok(Session {
            rpc_version,
            version: self.redact(version),
            download_dir,
            dht: optional_bool(&value, "dht-enabled")?,
            queue: optional_bool(&value, "download-queue-enabled")?,
        })
    }
    async fn torrents(&mut self, ids: Option<&[String]>, fields: &[&str]) -> Result<Vec<Value>> {
        let mut arguments = json!({ "fields": fields });
        if let Some(ids) = ids {
            arguments["ids"] = json!(ids);
        }
        let value = self.call("torrent-get", arguments).await?;
        let list = value
            .get("torrents")
            .and_then(Value::as_array)
            .ok_or(QbitError::InvalidResponse)?;
        if list.len() > MAX_LISTED_TORRENTS || ids.is_some_and(|ids| list.len() > ids.len()) {
            return Err(QbitError::InvalidResponse);
        }
        if list.iter().any(|t| !t.is_object()) {
            return Err(QbitError::InvalidResponse);
        }
        Ok(list.clone())
    }
}
fn optional_bool(value: &Value, key: &str) -> Result<Option<bool>> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(QbitError::InvalidResponse),
    }
}
struct Session {
    rpc_version: u64,
    version: String,
    download_dir: String,
    dht: Option<bool>,
    queue: Option<bool>,
}
impl Session {
    fn labels_supported(&self) -> bool {
        self.rpc_version >= LABELS_RPC_VERSION
    }
    fn fields(&self, base: &[&'static str]) -> Vec<&'static str> {
        let mut fields = base.to_vec();
        if self.rpc_version >= LABEL_FIELD_RPC_VERSION {
            fields.push("labels");
        }
        fields
    }
}

/// How one configured domain is placed on, and recognised in, a shared client.
struct Owner {
    directory: String,
    label: Option<String>,
    labels_supported: bool,
}
impl Owner {
    fn owns(&self, directory: &str, labels: &[String]) -> bool {
        if let (Some(label), true) = (&self.label, self.labels_supported) {
            if !labels.is_empty() {
                return labels.iter().any(|l| l.eq_ignore_ascii_case(label));
            }
        }
        within(directory, &self.directory)
    }
}
struct Owners {
    tv: Option<Owner>,
    movies: Option<Owner>,
}
fn owner(
    scope: &TransmissionScope,
    session: &Session,
) -> std::result::Result<Owner, TransmissionTestFailure> {
    Ok(match &scope.placement {
        TorrentPlacement::Category { category } => {
            let base = &session.download_dir;
            if base.is_empty() || validate_remote_path(base).is_err() || !rooted(base) {
                return Err(TransmissionTestFailure {
                    error: QbitError::UnsupportedFeature,
                    problem: Some(TransmissionProblem::DownloadDirInvalid),
                });
            }
            Owner {
                directory: join(base, category),
                label: Some(category.clone()),
                labels_supported: session.labels_supported(),
            }
        }
        TorrentPlacement::Directory { directory } => Owner {
            directory: directory.clone(),
            label: None,
            labels_supported: session.labels_supported(),
        },
    })
}
impl Owners {
    fn new(
        settings: &ProviderSettings,
        session: &Session,
    ) -> std::result::Result<Self, TransmissionTestFailure> {
        let ProviderSettings::Transmission { tv, movies, .. } = settings else {
            return Err(QbitError::ScopeConflict.into());
        };
        let tv = tv.as_ref().map(|s| owner(s, session)).transpose()?;
        let movies = movies.as_ref().map(|s| owner(s, session)).transpose()?;
        if let (Some(a), Some(b)) = (&tv, &movies) {
            if within(&a.directory, &b.directory) || within(&b.directory, &a.directory) {
                return Err(TransmissionTestFailure {
                    error: QbitError::ScopeConflict,
                    problem: Some(TransmissionProblem::DomainsOverlap),
                });
            }
        }
        Ok(Self { tv, movies })
    }
    fn plain(settings: &ProviderSettings, session: &Session) -> Result<Self> {
        Self::new(settings, session).map_err(|failure| match failure.problem {
            Some(TransmissionProblem::DownloadDirInvalid) => QbitError::UnsupportedFeature,
            Some(TransmissionProblem::DomainsOverlap) => QbitError::ScopeConflict,
            None => failure.error,
        })
    }
    fn get(&self, domain: MediaDomain) -> Result<&Owner> {
        match domain {
            MediaDomain::Tv => self.tv.as_ref(),
            MediaDomain::Movies => self.movies.as_ref(),
        }
        .ok_or(QbitError::ScopeConflict)
    }
    /// The single domain owning a torrent; a torrent claimed by both is a scope conflict.
    fn classify(&self, torrent: &Value) -> Result<Option<MediaDomain>> {
        let directory = text(torrent, "downloadDir", 4096)?;
        let labels = labels(torrent)?;
        let tv = self.tv.as_ref().is_some_and(|o| o.owns(directory, &labels));
        let movies = self
            .movies
            .as_ref()
            .is_some_and(|o| o.owns(directory, &labels));
        match (tv, movies) {
            (true, true) => Err(QbitError::ScopeConflict),
            (true, false) => Ok(Some(MediaDomain::Tv)),
            (false, true) => Ok(Some(MediaDomain::Movies)),
            (false, false) => Ok(None),
        }
    }
}

fn text<'v>(value: &'v Value, key: &str, max: usize) -> Result<&'v str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| s.len() <= max && !s.chars().any(char::is_control))
        .ok_or(QbitError::InvalidResponse)
}
fn number(value: &Value, key: &str) -> Result<u64> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .filter(|n| *n <= MAX_JSON_NUMBER)
        .ok_or(QbitError::InvalidResponse)
}
fn optional_number(value: &Value, key: &str) -> Result<Option<u64>> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(_) => number(value, key).map(Some),
    }
}
fn float(value: &Value, key: &str) -> Result<Option<f64>> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_f64()
            .filter(|n| n.is_finite())
            .map(Some)
            .ok_or(QbitError::InvalidResponse),
    }
}
fn torrent_hash(value: &Value) -> Result<String> {
    let raw = text(value, "hashString", 64)?;
    if raw.len() != 40 || !raw.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(QbitError::InvalidResponse);
    }
    Ok(raw.to_ascii_lowercase())
}
fn labels(value: &Value) -> Result<Vec<String>> {
    match value.get("labels") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(list)) if list.len() <= MAX_LABELS => list
            .iter()
            .map(|v| {
                v.as_str()
                    .filter(|s| s.len() <= 256 && !s.chars().any(char::is_control))
                    .map(str::to_owned)
                    .ok_or(QbitError::InvalidResponse)
            })
            .collect(),
        Some(_) => Err(QbitError::InvalidResponse),
    }
}

/// Maps one torrent into the shared download model. Error strings can carry tracker URLs with
/// passkeys, so only the numeric error class is projected, never the text.
fn item(
    rpc: &Rpc<'_, '_>,
    value: &Value,
    domain: MediaDomain,
    owner: &Owner,
) -> Result<DownloadItem> {
    let hash = torrent_hash(value)?;
    let code = number(value, "status")?;
    let left = number(value, "leftUntilDone")?;
    let size = match optional_number(value, "sizeWhenDone")? {
        Some(size) => size,
        None => number(value, "totalSize")?,
    };
    let percent = float(value, "percentDone")?
        .filter(|p| (0.0..=1.0).contains(p))
        .ok_or(QbitError::InvalidResponse)?;
    let metadata = float(value, "metadataPercentComplete")?.unwrap_or(1.0);
    if !(0.0..=1.0).contains(&metadata) {
        return Err(QbitError::InvalidResponse);
    }
    let error = optional_number(value, "error")?.unwrap_or(0);
    let stalled = optional_bool(value, "isStalled")?.unwrap_or(false);
    let data_complete = left == 0 && percent >= 1.0 && size > 0 && metadata >= 1.0;
    let (status, diagnostic) = if metadata < 1.0 || size == 0 {
        (
            if code == STOPPED {
                DownloadStatus::Paused
            } else {
                DownloadStatus::Queued
            },
            Some(DownloadDiagnostic::Metadata),
        )
    } else if error == LOCAL_ERROR || (error == TRACKER_ERROR && !data_complete) {
        (DownloadStatus::Warning, Some(DownloadDiagnostic::Error))
    } else {
        match code {
            CHECK_WAIT | CHECK => (DownloadStatus::Queued, None),
            STOPPED | SEED_WAIT | SEEDING if data_complete => (DownloadStatus::Completed, None),
            STOPPED => (DownloadStatus::Paused, None),
            DOWNLOAD_WAIT => (DownloadStatus::Queued, None),
            DOWNLOADING if stalled => (DownloadStatus::Warning, Some(DownloadDiagnostic::Stalled)),
            DOWNLOADING | SEED_WAIT | SEEDING => (DownloadStatus::Downloading, None),
            _ => (
                DownloadStatus::Unknown,
                Some(DownloadDiagnostic::UnknownState),
            ),
        }
    };
    let completed = status == DownloadStatus::Completed;
    let eta = if completed {
        Some(0)
    } else {
        match value.get("eta") {
            Some(v) => v
                .as_i64()
                .filter(|n| (0..=MAX_ETA_SECONDS).contains(n))
                .map(|n| n as u64),
            None => None,
        }
    };
    let ratio = float(value, "uploadRatio")?.unwrap_or(0.0);
    Ok(DownloadItem {
        hash,
        domain,
        name: rpc.redact(text(value, "name", 1024)?.to_owned()),
        category: owner.label.clone().unwrap_or_default(),
        status,
        diagnostic,
        progress: percent,
        size_bytes: size,
        remaining_bytes: Some(left.min(size)),
        download_bytes_per_second: optional_number(value, "rateDownload")?.unwrap_or(0),
        upload_bytes_per_second: optional_number(value, "rateUpload")?.unwrap_or(0),
        eta_seconds: eta,
        // -1 means not available and -2 infinite; neither is a measured ratio.
        ratio: if ratio < 0.0 {
            0.0
        } else {
            ratio.min(MAX_RATIO)
        },
        seeding_seconds: optional_number(value, "secondsSeeding")?,
        completed,
    })
}

/// Bounded two-phase observation: list every torrent's identity and placement, then read full
/// state for this domain's torrents only. Not proof of remote absence.
async fn observe(
    rpc: &mut Rpc<'_, '_>,
    settings: &ProviderSettings,
    domain: MediaDomain,
) -> Result<Vec<DownloadItem>> {
    let session = rpc.session().await?;
    let owners = Owners::plain(settings, &session)?;
    let owner = owners.get(domain)?;
    let mut hashes = Vec::new();
    let mut seen = BTreeSet::new();
    for torrent in rpc.torrents(None, &session.fields(&LIST_FIELDS)).await? {
        let hash = torrent_hash(&torrent)?;
        if !seen.insert(hash.clone()) {
            return Err(QbitError::InvalidResponse);
        }
        if owners.classify(&torrent)? == Some(domain) {
            if hashes.len() == MAX_DOMAIN_ITEMS {
                return Err(HttpError::ResponseTooLarge.into());
            }
            hashes.push(hash);
        }
    }
    if hashes.is_empty() {
        return Ok(Vec::new());
    }
    let mut items = Vec::new();
    let mut observed = BTreeSet::new();
    for torrent in rpc
        .torrents(Some(&hashes), &session.fields(&ITEM_FIELDS))
        .await?
    {
        // A torrent moved between the two reads is reported by whichever domain now owns it.
        if owners.classify(&torrent)? != Some(domain) {
            continue;
        }
        let item = item(rpc, &torrent, domain, owner)?;
        if !hashes.contains(&item.hash) || !observed.insert(item.hash.clone()) {
            return Err(QbitError::InvalidResponse);
        }
        items.push(item);
    }
    Ok(items)
}

pub async fn refresh_pages(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
    domain: MediaDomain,
) -> Result<Vec<DownloadItem>> {
    scope(settings, domain)?;
    let mut rpc = Rpc::new(operation, settings, credentials)?;
    let items = observe(&mut rpc, settings, domain).await?;
    if serde_json::to_vec(&items)
        .map_err(|_| QbitError::InvalidResponse)?
        .len()
        > MAX_REFRESH_BYTES
    {
        return Err(HttpError::ResponseTooLarge.into());
    }
    operation.ensure_active()?;
    Ok(items)
}
/// Public scoped page view over the same bounded observation. There is no imported category.
pub async fn query(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
    request: &DownloadQuery,
) -> Result<DownloadPage> {
    if !(1..=500).contains(&request.limit) || request.offset > i32::MAX as u32 {
        return Err(QbitError::InvalidRequest);
    }
    scope(settings, request.domain)?;
    if request.imported {
        return Err(QbitError::ScopeConflict);
    }
    let mut all = refresh_pages(operation, settings, credentials, request.domain).await?;
    all.sort_by(|a, b| a.hash.cmp(&b.hash));
    let start = (request.offset as usize).min(all.len());
    let end = start.saturating_add(request.limit as usize).min(all.len());
    let next_offset = (end < all.len()).then_some(end as u32);
    Ok(DownloadPage {
        domain: request.domain,
        offset: request.offset,
        limit: request.limit,
        next_offset,
        items: all.into_iter().skip(start).take(end - start).collect(),
    })
}

/// Looks up one info hash. Present in this domain → `Some`; present elsewhere (other domain or
/// unowned) is a visible conflict, never absence that could authorize another submission.
async fn find(
    rpc: &mut Rpc<'_, '_>,
    session: &Session,
    owners: &Owners,
    domain: MediaDomain,
    hash: &str,
    fields: &[&'static str],
) -> Result<Option<Value>> {
    let ids = [hash.to_owned()];
    let mut list = rpc.torrents(Some(&ids), &session.fields(fields)).await?;
    match list.len() {
        0 => Ok(None),
        1 => {
            let torrent = list.remove(0);
            if torrent_hash(&torrent)? != hash {
                return Err(QbitError::InvalidResponse);
            }
            if owners.classify(&torrent)? != Some(domain) {
                return Err(QbitError::ScopeConflict);
            }
            Ok(Some(torrent))
        }
        _ => Err(QbitError::InvalidResponse),
    }
}

pub async fn test_connection(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
) -> std::result::Result<TransmissionTest, TransmissionTestFailure> {
    settings.validate().map_err(|_| QbitError::InvalidRequest)?;
    let ProviderSettings::Transmission { tv, movies, .. } = settings else {
        return Err(QbitError::ScopeConflict.into());
    };
    let mut rpc = Rpc::new(operation, settings, credentials)?;
    let session = rpc.session().await?;
    let owners = Owners::new(settings, &session)?;
    // Authentication must also authorize torrent reads, and no torrent may claim both domains.
    for torrent in rpc.torrents(None, &session.fields(&LIST_FIELDS)).await? {
        torrent_hash(&torrent)?;
        owners
            .classify(&torrent)
            .map_err(|error| TransmissionTestFailure {
                error,
                problem: (error == QbitError::ScopeConflict)
                    .then_some(TransmissionProblem::DomainsOverlap),
            })?;
    }
    let scopes: Vec<&TransmissionScope> = tv.iter().chain(movies.iter()).collect();
    let mut warnings = Vec::new();
    if !session.labels_supported()
        && scopes
            .iter()
            .any(|s| matches!(s.placement, TorrentPlacement::Category { .. }))
    {
        warnings.push(TransmissionTestWarning::LabelsUnsupported);
    }
    if session.queue == Some(false)
        && scopes.iter().any(|s| {
            s.recent_priority == TorrentQueuePriority::First
                || s.older_priority == TorrentQueuePriority::First
        })
    {
        warnings.push(TransmissionTestWarning::QueueDisabled);
    }
    let domains = [MediaDomain::Tv, MediaDomain::Movies]
        .into_iter()
        .filter(|d| owners.get(*d).is_ok())
        .collect();
    Ok(TransmissionTest {
        labels_supported: session.labels_supported(),
        rpc_version: session.rpc_version as u32,
        transmission_version: session.version,
        domains,
        warnings,
    })
}

/// Validates target/scope and payload, deriving the info-hash identity. Network I/O only fetches
/// a `.torrent` locator from the indexer; the download client is not contacted.
pub async fn prepare(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    target: MediaTarget,
    source: AddSource,
    recent: bool,
    options: QbitOptions,
) -> Result<PreparedTorrent> {
    let domain = target_domain(&target)?;
    scope(settings, domain)?;
    if options
        .ratio_limit
        .is_some_and(|n| !n.is_finite() || n > MAX_RATIO || (n < 0.0 && n != -1.0 && n != -2.0))
        || options
            .seeding_minutes
            .is_some_and(|n| !(-2..=i32::MAX as i64).contains(&n))
    {
        return Err(QbitError::InvalidRequest);
    }
    let (source, identities) = qbittorrent::resolve_source(operation, source).await?;
    // Transmission identifies torrents by their v1 info hash; a v2-only torrent cannot be added.
    let hash = identities
        .iter()
        .find(|id| id.len() == 40)
        .cloned()
        .ok_or(QbitError::UnsupportedFeature)?;
    let payload = match &source {
        AddSource::Magnet(value) => value.as_bytes(),
        AddSource::Torrent { bytes, .. } => bytes,
        AddSource::Url(_) => return Err(QbitError::InvalidRequest),
    };
    let identity = SubmissionIdentity::tokenized(
        target,
        hash,
        settings_fingerprint(settings)?,
        digest_hex(&ring::digest::SHA256, payload),
    );
    Ok(PreparedTorrent {
        identity,
        source,
        recent,
        options,
    })
}
fn validate_identity(
    identity: &SubmissionIdentity,
    settings: &ProviderSettings,
) -> Result<(MediaDomain, String)> {
    let domain = target_domain(identity.target())?;
    scope(settings, domain)?;
    let [hash] = identity.hashes() else {
        return Err(QbitError::InvalidRequest);
    };
    let hex = |s: &str, n: usize| {
        s.len() == n
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    if identity.version() != 1 || !hex(hash, 40) || !hex(identity.payload_sha256(), 64) {
        return Err(QbitError::InvalidRequest);
    }
    if identity.settings_fingerprint() != settings_fingerprint(settings)? {
        return Err(QbitError::ScopeConflict);
    }
    Ok((domain, hash.clone()))
}
pub async fn reconcile(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
    identity: &SubmissionIdentity,
) -> Result<Reconciliation> {
    let (domain, hash) = validate_identity(identity, settings)?;
    let mut rpc = Rpc::new(operation, settings, credentials)?;
    let session = rpc.session().await?;
    let owners = Owners::plain(settings, &session)?;
    Ok(
        match find(&mut rpc, &session, &owners, domain, &hash, &LIST_FIELDS).await? {
            Some(_) => Reconciliation::Observed { hash },
            None => Reconciliation::NotObserved,
        },
    )
}
pub async fn submit(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
    prepared: PreparedTorrent,
) -> Result<AddOutcome> {
    submit_classified(operation, settings, credentials, prepared)
        .await
        .map_err(|e| e.error)
}
fn local_submission_failure(error: QbitError) -> bool {
    matches!(
        error,
        QbitError::InvalidRequest
            | QbitError::Http(
                HttpError::InvalidRequest
                    | HttpError::LocalUnavailable { .. }
                    | HttpError::Busy
                    | HttpError::ResponseTooLarge
            )
    )
}
/// Never retried: once `torrent-add` is dispatched any failure is `MutationUnknown`-class and the
/// caller must reconcile by info hash instead of submitting again.
pub(crate) async fn submit_classified(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
    prepared: PreparedTorrent,
) -> std::result::Result<AddOutcome, SubmissionError> {
    let mut add_started = false;
    submit_inner(operation, settings, credentials, prepared, &mut add_started)
        .await
        .map_err(|error| SubmissionError {
            error,
            definitely_not_submitted: !add_started,
            client_retry_kind: match error {
                QbitError::Http(HttpError::Authentication) => ClientRetryKind::Authentication,
                QbitError::Http(HttpError::Transport | HttpError::Timeout) => {
                    ClientRetryKind::Unavailable
                }
                _ => ClientRetryKind::Other,
            },
            client_retry_eligible: !local_submission_failure(error),
        })
}
async fn submit_inner(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
    prepared: PreparedTorrent,
    add_started: &mut bool,
) -> Result<AddOutcome> {
    let (domain, hash) = validate_identity(&prepared.identity, settings)?;
    let PreparedTorrent {
        identity,
        source,
        recent,
        options,
    } = prepared;
    let payload = match &source {
        AddSource::Magnet(value) => value.as_bytes(),
        AddSource::Torrent { bytes, .. } => bytes,
        AddSource::Url(_) => return Err(QbitError::InvalidRequest),
    };
    if identity.payload_sha256() != digest_hex(&ring::digest::SHA256, payload) {
        return Err(QbitError::InvalidRequest);
    }
    let configured = scope(settings, domain)?;
    let mut rpc = Rpc::new(operation, settings, credentials)?;
    let session = rpc.session().await?;
    let owners = Owners::plain(settings, &session)?;
    let owner = owners.get(domain)?;
    if find(&mut rpc, &session, &owners, domain, &hash, &LIST_FIELDS)
        .await?
        .is_some()
    {
        return Ok(AddOutcome::AlreadyPresent { hash });
    }
    let mut arguments = json!({
        "download-dir": owner.directory,
        "paused": configured.add_paused,
    });
    match &source {
        AddSource::Magnet(value) => {
            let url = url::Url::parse(value).map_err(|_| QbitError::InvalidRequest)?;
            // A trackerless magnet can only resolve through DHT.
            if !url.query_pairs().any(|(k, v)| k == "tr" && !v.is_empty())
                && session.dht == Some(false)
            {
                return Err(QbitError::Rejected);
            }
            arguments["filename"] = json!(value);
        }
        AddSource::Torrent { bytes, .. } => arguments["metainfo"] = json!(base64(bytes)),
        AddSource::Url(_) => return Err(QbitError::InvalidRequest),
    }
    let labelled = owner.label.is_some() && session.labels_supported();
    if let (Some(label), true) = (&owner.label, labelled) {
        arguments["labels"] = json!([label]);
    }
    let first = if recent {
        configured.recent_priority
    } else {
        configured.older_priority
    } == TorrentQueuePriority::First;
    operation.ensure_active()?;
    // Once add begins, even rejection-shaped failures cannot authorize another client.
    *add_started = true;
    let (result, reply) = rpc
        .call_result("torrent-add", arguments)
        .await
        .map_err(|error| match error {
            QbitError::Http(HttpError::Authentication) => error,
            _ => QbitError::MutationUnknown,
        })?;
    let (returned, duplicate) = if result == "success" {
        match (reply.get("torrent-added"), reply.get("torrent-duplicate")) {
            (Some(added), None) => (Some(added), false),
            (None, Some(existing)) => (Some(existing), true),
            _ => return Err(QbitError::MutationUnknown),
        }
    } else if result.eq_ignore_ascii_case("duplicate torrent") {
        // Servers before RPC 15 report an existing torrent as this failure string.
        (None, true)
    } else {
        return Err(QbitError::Rejected);
    };
    if let Some(returned) = returned {
        if torrent_hash(returned).map_err(|_| QbitError::MutationUnknown)? != hash {
            return Err(QbitError::MutationUnknown);
        }
    }
    if duplicate {
        // Someone else added it between the lookup and add: never mutate a torrent this call
        // did not create. It is ours only if it sits in this domain.
        return match find(&mut rpc, &session, &owners, domain, &hash, &LIST_FIELDS).await {
            Ok(Some(_)) => Ok(AddOutcome::AlreadyPresent { hash }),
            Ok(None) => Err(QbitError::MutationUnknown),
            Err(QbitError::ScopeConflict) => Err(QbitError::ScopeConflict),
            Err(_) => Err(QbitError::MutationUnknown),
        };
    }
    // Follow-ups are explicit, bounded and never retried; failure requires reconciliation.
    let ids = json!([hash]);
    let ratio = options.ratio_limit.filter(|r| *r >= 0.0);
    // Transmission has no seeding-time limit; the references use the per-torrent idle limit.
    let idle = options.seeding_minutes.filter(|m| *m >= 0);
    if ratio.is_some() || idle.is_some() {
        let mut set = json!({ "ids": ids });
        if let Some(ratio) = ratio {
            set["seedRatioLimit"] = json!(ratio);
            set["seedRatioMode"] = json!(1);
        }
        if let Some(minutes) = idle {
            set["seedIdleLimit"] = json!(minutes);
            set["seedIdleMode"] = json!(1);
        }
        rpc.call("torrent-set", set)
            .await
            .map_err(|_| QbitError::MutationUnknown)?;
    }
    if first {
        rpc.call("queue-move-top", json!({ "ids": ids }))
            .await
            .map_err(|_| QbitError::MutationUnknown)?;
    }
    let after = find(&mut rpc, &session, &owners, domain, &hash, &ITEM_FIELDS)
        .await
        .map_err(|_| QbitError::MutationUnknown)?
        .ok_or(QbitError::MutationUnknown)?;
    let status = number(&after, "status").map_err(|_| QbitError::MutationUnknown)?;
    let observed_float = |key: &str| float(&after, key).ok().flatten();
    if configured.add_paused != (status == STOPPED)
        || ratio.is_some_and(|r| {
            observed_float("seedRatioLimit").is_none_or(|v| (v - r).abs() > 0.0001)
                || after.get("seedRatioMode").and_then(Value::as_u64) != Some(1)
        })
        || idle.is_some_and(|m| {
            after.get("seedIdleLimit").and_then(Value::as_i64) != Some(m)
                || after.get("seedIdleMode").and_then(Value::as_u64) != Some(1)
        })
        || (first && after.get("queuePosition").and_then(Value::as_u64) != Some(0))
        || (labelled
            && !labels(&after)
                .map_err(|_| QbitError::MutationUnknown)?
                .iter()
                .any(|l| {
                    owner
                        .label
                        .as_deref()
                        .is_some_and(|c| l.eq_ignore_ascii_case(c))
                }))
    {
        return Err(QbitError::MutationUnknown);
    }
    Ok(AddOutcome::Observed { hash })
}

/// Private protocol facts: the remote download directory and files relative to it.
pub struct TransmissionDetails {
    pub item: DownloadItem,
    pub save_path: String,
    pub files: Vec<TorrentFile>,
}
/// Completed-download facts for one torrent of this domain. Paths are remote; file names are
/// relative to `save_path` (the torrent's download directory).
pub async fn details(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
    target: &MediaTarget,
    expected: &str,
) -> Result<TransmissionDetails> {
    let domain = target_domain(target)?;
    scope(settings, domain)?;
    let expected = qbittorrent::hash(expected)?;
    if expected.len() != 40 {
        return Err(QbitError::InvalidRequest);
    }
    let mut rpc = Rpc::new(operation, settings, credentials)?;
    let session = rpc.session().await?;
    let owners = Owners::plain(settings, &session)?;
    let owner = owners.get(domain)?;
    let mut fields = ITEM_FIELDS.to_vec();
    fields.extend(["files", "fileStats"]);
    let torrent = find(&mut rpc, &session, &owners, domain, &expected, &fields)
        .await?
        .ok_or(QbitError::NotFound)?;
    let item = item(&rpc, &torrent, domain, owner)?;
    let save_path = text(&torrent, "downloadDir", 4096)?.to_owned();
    validate_remote_path(&save_path)?;
    if !rooted(&save_path) {
        return Err(QbitError::InvalidResponse);
    }
    let files = torrent
        .get("files")
        .and_then(Value::as_array)
        .ok_or(QbitError::InvalidResponse)?;
    let stats = torrent
        .get("fileStats")
        .and_then(Value::as_array)
        .ok_or(QbitError::InvalidResponse)?;
    if files.len() > MAX_FILES || files.len() != stats.len() {
        return Err(QbitError::InvalidResponse);
    }
    let mut result = Vec::with_capacity(files.len());
    for (index, (file, stat)) in files.iter().zip(stats).enumerate() {
        let name = text(file, "name", 4096)?;
        // Remote names remain relative; a later path mapper decides filesystem ownership.
        if name.is_empty()
            || name.starts_with(['/', '\\'])
            || name.contains(':')
            || name
                .split(['/', '\\'])
                .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err(QbitError::InvalidResponse);
        }
        let length = number(file, "length")?;
        let done = number(stat, "bytesCompleted")?.min(length);
        let wanted = optional_bool(stat, "wanted")?.ok_or(QbitError::InvalidResponse)?;
        result.push(TorrentFile {
            index: index as u32,
            name: rpc.redact(name.to_owned()),
            size_bytes: length,
            progress: if length == 0 {
                1.0
            } else {
                done as f64 / length as f64
            },
            priority: u8::from(wanted),
        });
    }
    Ok(TransmissionDetails {
        item,
        save_path,
        files: result,
    })
}

/// Private output-root facts for health: the domain's effective download directory.
/// Transmission stops (never removes) torrents at seed limits, so retention is always safe.
pub(crate) async fn client_status_snapshot(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
    domain: MediaDomain,
) -> Result<ClientStatusSnapshot> {
    let configured = scope(settings, domain)?;
    let locality = endpoint_locality(settings.endpoint())?;
    let mut rpc = Rpc::new(operation, settings, credentials)?;
    let session = rpc.session().await?;
    let remote_root = match &configured.placement {
        TorrentPlacement::Directory { directory } => directory.clone(),
        TorrentPlacement::Category { category } => {
            if session.download_dir.is_empty() {
                return Ok(ClientStatusSnapshot::OutputMissing(locality));
            }
            if validate_remote_path(&session.download_dir).is_err()
                || !rooted(&session.download_dir)
            {
                return Ok(ClientStatusSnapshot::OutputInvalid(locality));
            }
            join(&session.download_dir, category)
        }
    };
    if validate_remote_path(&remote_root).is_err() {
        return Ok(ClientStatusSnapshot::OutputInvalid(locality));
    }
    Ok(ClientStatusSnapshot::Status(ClientStatus {
        domain,
        remote_root,
        root_source: RootSource::Category,
        locality,
        removes_completed_downloads: false,
    }))
}

/// Public status view; missing or invalid output roots are errors here.
pub async fn client_status(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
    domain: MediaDomain,
) -> Result<ClientStatus> {
    match client_status_snapshot(operation, settings, credentials, domain).await? {
        ClientStatusSnapshot::Status(status) => Ok(status),
        _ => Err(QbitError::InvalidResponse),
    }
}
/// Explicit queue/state controls. A caller must persist target + identity + intent first.
/// Imported-category and per-file controls are not supported by this client.
pub async fn control(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
    target: &MediaTarget,
    expected: &str,
    action: Control,
) -> Result<()> {
    let domain = target_domain(target)?;
    scope(settings, domain)?;
    let expected = qbittorrent::hash(expected)?;
    if expected.len() != 40 {
        return Err(QbitError::InvalidRequest);
    }
    let (method, mut arguments) = match &action {
        Control::Pause => ("torrent-stop", json!({})),
        Control::Resume => ("torrent-start", json!({})),
        Control::Remove { delete_files } => (
            "torrent-remove",
            json!({ "delete-local-data": delete_files }),
        ),
        Control::Priority { first: true } => ("queue-move-top", json!({})),
        Control::Priority { first: false } => ("queue-move-bottom", json!({})),
        _ => return Err(QbitError::UnsupportedFeature),
    };
    arguments["ids"] = json!([expected]);
    let mut rpc = Rpc::new(operation, settings, credentials)?;
    let session = rpc.session().await?;
    let owners = Owners::plain(settings, &session)?;
    find(&mut rpc, &session, &owners, domain, &expected, &LIST_FIELDS)
        .await?
        .ok_or(QbitError::NotFound)?;
    rpc.call(method, arguments)
        .await
        .map_err(|error| match error {
            QbitError::Http(
                HttpError::Authentication
                | HttpError::RateLimited { .. }
                | HttpError::LocalUnavailable { .. }
                | HttpError::Busy
                | HttpError::ResponseTooLarge
                | HttpError::InvalidRequest,
            ) => error,
            _ => QbitError::MutationUnknown,
        })?;
    let after = find(&mut rpc, &session, &owners, domain, &expected, &ITEM_FIELDS)
        .await
        .map_err(|_| QbitError::MutationUnknown)?;
    let status = after
        .as_ref()
        .and_then(|t| t.get("status"))
        .and_then(Value::as_u64);
    let position = after
        .as_ref()
        .and_then(|t| t.get("queuePosition"))
        .and_then(Value::as_u64);
    let confirmed = match action {
        Control::Remove { .. } => after.is_none(),
        Control::Pause => status == Some(STOPPED),
        Control::Resume => status.is_some_and(|s| s != STOPPED),
        Control::Priority { first: true } => position == Some(0),
        Control::Priority { first: false } => after.is_some(),
        _ => false,
    };
    if confirmed {
        Ok(())
    } else {
        Err(QbitError::MutationUnknown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn base64_matches_rfc4648_vectors() {
        for (input, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), expected);
        }
    }
    #[test]
    fn containment_is_segment_wise_and_rooted() {
        assert!(within("/data/tv", "/data/tv"));
        assert!(within("/data/tv/Show", "/data/tv/"));
        assert!(!within("/data/tv-other", "/data/tv"));
        assert!(!within("/data", "/data/tv"));
        assert!(!within("data/tv", "/data"));
        assert!(within("C:\\Down\\TV\\x", "c:/down/tv"));
        assert!(!within("/c/down", "C:/down"));
        assert_eq!(join("/data/", "tv"), "/data/tv");
        assert_eq!(join("C:\\Down", "tv"), "C:\\Down\\tv");
    }
}
