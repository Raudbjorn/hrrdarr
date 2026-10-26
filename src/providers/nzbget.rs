//! NZBGet JSON-RPC operations over the bounded provider transport.
//!
//! Requirements were inventoried from the pinned Sonarr/Radarr NZBGet clients; the wire contract
//! follows NZBGet's published API reference (`/jsonrpc`, HTTP basic authentication). No scheduler,
//! automatic mutation retry or local filesystem access happens here: callers own durable intent,
//! path mapping and imports.
use super::{
    ClientRetryKind, Credentials, NzbgetScope, ProviderSettings,
    http::{HttpError, HttpOperation, HttpRequestBody},
    qbittorrent::{
        AddOutcome, ClientStatus, ClientStatusSnapshot, Control, DownloadDiagnostic, DownloadItem,
        DownloadPage, DownloadQuery, DownloadStatus, QbitError, Reconciliation, RootSource,
        SubmissionError, SubmissionIdentity, digest_hex, endpoint_locality, remote_root,
        target_domain,
    },
    sabnzbd::{UsenetJob, output_path},
    transmission::base64,
};
use crate::{api::MediaDomain, db::MediaTarget};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

/// Shared client error vocabulary (named after its first protocol; not qBittorrent-specific).
pub type NzbgetError = QbitError;
type Result<T> = std::result::Result<T, NzbgetError>;

/// `append` in the nine-argument form exists from NZBGet 13; older servers return a boolean.
const MIN_MAJOR_VERSION: u32 = 13;
/// Queue and history are unpaged in NZBGet: more entries than this fail closed.
const MAX_GROUPS: usize = 4096;
const MAX_HISTORY: usize = 8192;
const MAX_CONFIG_ITEMS: usize = 8192;
const MAX_CATEGORIES: usize = 99;
const MAX_REFRESH_BYTES: usize = 1024 * 1024;
const TOKEN_MARKER: &str = ".hrrdarr-";
const TOKEN_HEX: usize = 40;
const MAX_JOB_TITLE_BYTES: usize = 180;
const MAX_NZB_ID: i64 = i32::MAX as i64;
const MAX_REPORTED_BYTES: u64 = 1_000_000_000_000_000;
const MAX_ETA_SECONDS: u64 = 365 * 24 * 60 * 60;
const BYTES_PER_MIB: f64 = 1_048_576.0;
/// NZBGet's own bounds for `KeepHistory` (days); the references reject 0 and more than 25000.
const MAX_KEEP_HISTORY_DAYS: i64 = 25_000;
/// Completed jobs must survive until the next refresh imports them; below this is worth a warning.
const MIN_COMFORTABLE_KEEP_HISTORY_DAYS: i64 = 2;
const DUPE_MODE: &str = "FORCE";

#[derive(Serialize, ts_rs::TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NzbgetTestWarning {
    /// `testing` builds do not report a comparable version.
    TestingVersion,
    /// History retention may remove completed jobs before they can be imported.
    ShortHistoryRetention,
}
#[derive(Serialize, ts_rs::TS, Debug)]
pub struct NzbgetTest {
    pub nzbget_version: String,
    pub domains: Vec<MediaDomain>,
    pub warnings: Vec<NzbgetTestWarning>,
}
/// A static, user-actionable configuration problem detected by a connection test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NzbgetProblem {
    CategoryMissing,
    KeepHistoryZero,
    KeepHistoryOverMax,
}
#[derive(Debug)]
pub struct NzbgetTestFailure {
    pub error: NzbgetError,
    pub problem: Option<NzbgetProblem>,
}
impl From<NzbgetError> for NzbgetTestFailure {
    fn from(error: NzbgetError) -> Self {
        Self {
            error,
            problem: None,
        }
    }
}
fn problem(problem: NzbgetProblem) -> NzbgetTestFailure {
    NzbgetTestFailure {
        error: QbitError::UnsupportedFeature,
        problem: Some(problem),
    }
}

/// Retains the NZB locator in memory only; its identity is the journal-safe part.
pub struct PreparedNzb {
    identity: SubmissionIdentity,
    url: String,
    job_name: String,
    recent: bool,
}
impl PreparedNzb {
    pub fn identity(&self) -> &SubmissionIdentity {
        &self.identity
    }
    pub(crate) fn into_identity(self) -> SubmissionIdentity {
        self.identity
    }
}

fn scope(settings: &ProviderSettings, domain: MediaDomain) -> Result<&NzbgetScope> {
    settings.validate().map_err(|_| QbitError::InvalidRequest)?;
    match settings {
        ProviderSettings::Nzbget { tv, movies, .. } => match domain {
            MediaDomain::Tv => tv.as_ref(),
            MediaDomain::Movies => movies.as_ref(),
        }
        .ok_or(QbitError::ScopeConflict),
        _ => Err(QbitError::ScopeConflict),
    }
}
fn same_category(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}
/// Remote identity encoding: the journal stores a fixed 40-hex digest of the NZBGet job id.
pub fn remote_id(nzb_id: i64) -> String {
    digest_hex(
        &ring::digest::SHA1_FOR_LEGACY_USE_ONLY,
        format!("nzbget-nzb:{nzb_id}").as_bytes(),
    )
}
/// Eligibility tags do not change remote submission behaviour; exclude them like the others.
pub(crate) fn settings_fingerprint(settings: &ProviderSettings) -> Result<String> {
    let mut settings = settings.clone();
    if let ProviderSettings::Nzbget { tv, movies, .. } = &mut settings {
        for scope in tv.iter_mut().chain(movies.iter_mut()) {
            scope.tags = None;
        }
    } else {
        return Err(QbitError::ScopeConflict);
    }
    let bytes = serde_json::to_vec(&settings).map_err(|_| QbitError::InvalidRequest)?;
    Ok(digest_hex(&ring::digest::SHA256, &bytes))
}
/// Deterministic per client, configuration, typed target and release locator, so a retried
/// preparation reproduces its journal identity and a lost response can be reconciled by name.
fn submission_token(provider: Uuid, fingerprint: &str, target: &MediaTarget, url: &str) -> String {
    let target = match target {
        MediaTarget::Episode(id) => format!("episode:{id}"),
        MediaTarget::Movie(id) => format!("movie:{id}"),
    };
    let mut token = digest_hex(
        &ring::digest::SHA256,
        format!("hrrdarr/nzbget/v1\0{provider}\0{fingerprint}\0{target}\0{url}").as_bytes(),
    );
    token.truncate(TOKEN_HEX);
    token
}
fn hex_token(value: &str) -> bool {
    value.len() == TOKEN_HEX
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
/// Visible job name: a bounded release title followed by the reconciliation marker.
fn job_name(title: &str, token: &str) -> String {
    let mut clean: String = title
        .chars()
        .map(|c| {
            if c.is_control()
                || matches!(
                    c,
                    '/' | '\\' | '{' | '}' | '"' | '*' | '?' | '<' | '>' | '|' | ':'
                )
            {
                '_'
            } else {
                c
            }
        })
        .collect();
    while clean.len() > MAX_JOB_TITLE_BYTES {
        clean.pop();
    }
    let clean = clean.trim_matches(|c: char| c == '.' || c.is_whitespace());
    let clean = if clean.is_empty() { "release" } else { clean };
    format!("{clean}{TOKEN_MARKER}{token}")
}
fn name_has_token(name: &str, token: &str) -> bool {
    name.contains(&format!("{TOKEN_MARKER}{token}"))
}

struct Rpc<'o, 'a> {
    operation: &'o HttpOperation<'a>,
    url: String,
    authorization: Option<String>,
    secrets: Vec<String>,
    request_id: u64,
}
impl<'o, 'a> Rpc<'o, 'a> {
    fn new(
        operation: &'o HttpOperation<'a>,
        settings: &ProviderSettings,
        credentials: Option<&Credentials>,
    ) -> Result<Self> {
        let ProviderSettings::Nzbget { endpoint, .. } = settings else {
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
        let path = format!("{}/jsonrpc", url.path().trim_end_matches('/'));
        url.set_path(&path);
        Ok(Self {
            operation,
            url: url.into(),
            authorization,
            secrets,
            request_id: 0,
        })
    }
    fn redact(&self, mut value: String) -> String {
        for secret in &self.secrets {
            value = value.replace(secret.as_str(), "[redacted]");
        }
        value
    }
    /// One JSON-RPC call; the server's `error` object is a rejection and is never reflected.
    async fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.request_id = self
            .request_id
            .checked_add(1)
            .ok_or(QbitError::InvalidRequest)?;
        let body = serde_json::to_vec(
            &json!({"jsonrpc":"2.0","method":method,"params":params,"id":self.request_id}),
        )
        .map_err(|_| QbitError::InvalidRequest)?;
        let mut headers = Vec::new();
        if let Some(value) = &self.authorization {
            headers.push(("Authorization".to_owned(), value.clone()));
        }
        let response = self
            .operation
            .request(&self.url, &[], &headers, HttpRequestBody::Json(body))
            .await?;
        if !(200..300).contains(&response.status) {
            // A wrong URL base or an unavailable daemon is a connection failure.
            return Err(if response.status == 404 || response.status >= 500 {
                QbitError::Http(HttpError::Transport)
            } else {
                QbitError::InvalidResponse
            });
        }
        let mut value: Value =
            serde_json::from_slice(&response.body).map_err(|_| QbitError::InvalidResponse)?;
        if let Some(id) = value.get("id")
            && id.as_u64() != Some(self.request_id)
        {
            return Err(QbitError::InvalidResponse);
        }
        if value.get("error").is_some_and(|e| !e.is_null()) {
            return Err(QbitError::Rejected);
        }
        value
            .get_mut("result")
            .map(Value::take)
            .ok_or(QbitError::InvalidResponse)
    }
    async fn version(&mut self) -> Result<String> {
        let value = self.call("version", json!([])).await?;
        let raw = value.as_str().ok_or(QbitError::InvalidResponse)?.trim();
        if raw.is_empty() || raw.len() > 64 || !raw.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(QbitError::InvalidResponse);
        }
        Ok(raw.to_owned())
    }
    async fn config(&mut self) -> Result<BTreeMap<String, String>> {
        let value = self.call("config", json!([])).await?;
        let list = value.as_array().ok_or(QbitError::InvalidResponse)?;
        if list.len() > MAX_CONFIG_ITEMS {
            return Err(HttpError::ResponseTooLarge.into());
        }
        let mut config = BTreeMap::new();
        for item in list {
            let name = text(item, "Name", 256)?;
            let value = optional_text(item, "Value", 4096)?;
            config.insert(name.to_owned(), value.to_owned());
        }
        Ok(config)
    }
    async fn groups(&mut self) -> Result<Vec<Value>> {
        let value = self.call("listgroups", json!([0])).await?;
        let list = value.as_array().ok_or(QbitError::InvalidResponse)?;
        if list.len() > MAX_GROUPS {
            return Err(HttpError::ResponseTooLarge.into());
        }
        Ok(list.clone())
    }
    async fn history(&mut self) -> Result<Vec<Value>> {
        let value = self.call("history", json!([])).await?;
        let list = value.as_array().ok_or(QbitError::InvalidResponse)?;
        if list.len() > MAX_HISTORY {
            return Err(HttpError::ResponseTooLarge.into());
        }
        Ok(list.clone())
    }
    async fn global(&mut self) -> Result<Global> {
        let value = self.call("status", json!([])).await?;
        Ok(Global {
            paused: flag(&value, "DownloadPaused")?,
            rate: unsigned(&value, "DownloadRate").unwrap_or(0),
        })
    }
}
struct Global {
    paused: bool,
    /// Bytes per second across all jobs; zero or absent means no estimate.
    rate: u64,
}

fn text<'v>(value: &'v Value, key: &str, max: usize) -> Result<&'v str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| s.len() <= max && !s.chars().any(char::is_control))
        .ok_or(QbitError::InvalidResponse)
}
fn optional_text<'v>(value: &'v Value, key: &str, max: usize) -> Result<&'v str> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(""),
        Some(_) => text(value, key, max),
    }
}
fn flag(value: &Value, key: &str) -> Result<bool> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(Value::Number(n)) => Ok(n.as_i64().ok_or(QbitError::InvalidResponse)? != 0),
        Some(Value::String(s)) => Ok(matches!(s.trim(), "1" | "true" | "True" | "yes")),
        _ => Err(QbitError::InvalidResponse),
    }
}
fn unsigned(value: &Value, key: &str) -> Option<u64> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .filter(|n| *n <= MAX_REPORTED_BYTES)
}
/// NZBGet splits 64-bit sizes into two unsigned 32-bit halves (`...Lo`/`...Hi`) because JSON
/// cannot carry them natively; megabyte rounding (`...MB`) is only a fallback.
fn size64(value: &Value, prefix: &str) -> Result<u64> {
    let low = unsigned(value, &format!("{prefix}Lo"));
    let high = unsigned(value, &format!("{prefix}Hi"));
    let bytes = match (low, high) {
        (Some(low), Some(high)) if low <= u64::from(u32::MAX) && high <= u64::from(u32::MAX) => {
            (high << 32) | low
        }
        (Some(low), None) if low <= u64::from(u32::MAX) => low,
        (None, None) => {
            let mib = value
                .get(format!("{prefix}MB"))
                .and_then(Value::as_u64)
                .ok_or(QbitError::InvalidResponse)?;
            (mib as f64 * BYTES_PER_MIB) as u64
        }
        _ => return Err(QbitError::InvalidResponse),
    };
    if bytes > MAX_REPORTED_BYTES {
        return Err(QbitError::InvalidResponse);
    }
    Ok(bytes)
}
fn nzb_id(value: &Value, key: &str) -> Result<i64> {
    value
        .get(key)
        .and_then(Value::as_i64)
        .filter(|id| (1..=MAX_NZB_ID).contains(id))
        .ok_or(QbitError::InvalidResponse)
}

#[derive(Debug, PartialEq, Eq)]
enum Version {
    Release(u32, u32),
    Testing,
}
/// `21.1`, `20.0-r2312` and `21.0-testing-r2345` carry the number before the first `-`; a build
/// that reports only `testing-...` has none.
fn parse_version(raw: &str) -> Result<Version> {
    let head = raw.split('-').next().unwrap_or("");
    if head.eq_ignore_ascii_case("testing") {
        return Ok(Version::Testing);
    }
    let mut parts = head.split('.');
    let number = |part: Option<&str>| -> Result<u32> {
        let digits: String = part
            .ok_or(QbitError::InvalidResponse)?
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse().map_err(|_| QbitError::InvalidResponse)
    };
    let major = number(parts.next())?;
    // A bare "21" is accepted; a missing minor component is zero.
    let minor = parts.next().map_or(Ok(0), |p| number(Some(p)))?;
    Ok(Version::Release(major, minor))
}
async fn supported_version(rpc: &mut Rpc<'_, '_>) -> Result<(Version, String)> {
    let raw = rpc.version().await?;
    let version = parse_version(&raw)?;
    match version {
        Version::Release(major, _) if major < MIN_MAJOR_VERSION => {
            Err(QbitError::UnsupportedVersion)
        }
        version => Ok((version, raw)),
    }
}

/// `Category<N>.Name` entries (the references stop at the first gap, at most 99).
struct ConfigCategory {
    name: String,
    dest_dir: String,
    explicit_dest: bool,
}
fn categories(config: &BTreeMap<String, String>) -> Vec<ConfigCategory> {
    let mut list = Vec::new();
    for index in 1..=MAX_CATEGORIES {
        let Some(name) = config.get(&format!("Category{index}.Name")) else {
            break;
        };
        let explicit = config
            .get(&format!("Category{index}.DestDir"))
            .map(|d| d.trim())
            .filter(|d| !d.is_empty());
        let (dest_dir, explicit_dest) = match explicit {
            Some(dir) => (dir.to_owned(), true),
            None => {
                let main = config.get("MainDir").map_or("", String::as_str);
                let base = config
                    .get("DestDir")
                    .map_or("", String::as_str)
                    .replace("${MainDir}", main);
                // `AppendCategoryDir` defaults to yes.
                let append = config
                    .get("AppendCategoryDir")
                    .is_none_or(|v| !v.trim().eq_ignore_ascii_case("no"));
                let dest = if append && !base.is_empty() {
                    let separator = if base.contains('\\') && !base.starts_with('/') {
                        '\\'
                    } else {
                        '/'
                    };
                    format!("{}{separator}{name}", base.trim_end_matches(['/', '\\']))
                } else {
                    base
                };
                (dest, false)
            }
        };
        list.push(ConfigCategory {
            name: name.clone(),
            dest_dir,
            explicit_dest,
        });
    }
    list
}
fn keep_history_days(config: &BTreeMap<String, String>) -> Result<i64> {
    // NZBGet's default is 7 days when the option is absent.
    match config.get("KeepHistory") {
        None => Ok(7),
        Some(value) => value
            .trim()
            .parse::<i64>()
            .ok()
            .filter(|n| *n >= 0)
            .ok_or(QbitError::InvalidResponse),
    }
}

/// A job name shown while a URL is still being fetched can be the locator, which may carry an
/// indexer credential. Never project it. The trailing reconciliation marker is internal and is
/// removed from the projected name so it is not mistaken for release facts.
fn display_name(rpc: &Rpc<'_, '_>, raw: &str) -> String {
    if raw.contains("://") {
        return "(fetching NZB)".into();
    }
    let shown = match raw.rfind(TOKEN_MARKER) {
        Some(at) if at > 0 && hex_token(&raw[at + TOKEN_MARKER.len()..]) => &raw[..at],
        _ => raw,
    };
    rpc.redact(shown.to_owned())
}

struct Active {
    id: i64,
    raw_name: String,
    filename: String,
    category: String,
    item: DownloadItem,
}
/// Queue groups in server order; `ahead` accumulates remaining bytes ahead of each job across
/// every category because NZBGet shares one download rate across the whole queue.
struct QueueProgress {
    size: u64,
    remaining: u64,
    waiting: u64,
    paused: bool,
    status: DownloadStatus,
    diagnostic: Option<DownloadDiagnostic>,
}
fn queue_progress(group: &Value, global: &Global) -> Result<QueueProgress> {
    let state = text(group, "Status", 64)?.to_ascii_uppercase();
    let size = size64(group, "FileSize")?;
    let remaining = size64(group, "RemainingSize")?.min(size);
    let paused_size = size64(group, "PausedSize").unwrap_or(0).min(remaining);
    let active_downloads = unsigned(group, "ActiveDownloads").unwrap_or(0);
    let paused = state == "PAUSED" || global.paused || (remaining == paused_size && remaining != 0);
    let (status, diagnostic) = if paused {
        (DownloadStatus::Paused, None)
    } else {
        match state.as_str() {
            "QUEUED" => (DownloadStatus::Queued, None),
            "DOWNLOADING" if active_downloads == 0 && remaining != 0 => {
                (DownloadStatus::Queued, None)
            }
            "DOWNLOADING" | "FETCHING" => (DownloadStatus::Downloading, None),
            "LOADING_PARS" | "VERIFYING_SOURCES" | "VERIFYING_REPAIRED" => (
                DownloadStatus::Downloading,
                Some(DownloadDiagnostic::Verifying),
            ),
            "REPAIRING" => (
                DownloadStatus::Downloading,
                Some(DownloadDiagnostic::Repairing),
            ),
            "UNPACKING" => (
                DownloadStatus::Downloading,
                Some(DownloadDiagnostic::Extracting),
            ),
            "MOVING" => (
                DownloadStatus::Downloading,
                Some(DownloadDiagnostic::Moving),
            ),
            "PP_QUEUED"
            | "RENAMING"
            | "POST_UNPACK_RENAMING"
            | "POST_DOWNLOAD_RENAMING"
            | "EXECUTING_SCRIPT"
            | "PP_FINISHED" => (
                DownloadStatus::Downloading,
                Some(DownloadDiagnostic::PostProcessing),
            ),
            _ => (
                DownloadStatus::Unknown,
                Some(DownloadDiagnostic::UnknownState),
            ),
        }
    };
    Ok(QueueProgress {
        size,
        remaining,
        waiting: remaining - paused_size,
        paused,
        status,
        diagnostic,
    })
}
fn queue_item(
    rpc: &Rpc<'_, '_>,
    group: &Value,
    domain: MediaDomain,
    global: &Global,
    ahead: u64,
    progress: QueueProgress,
) -> Result<Active> {
    let id = nzb_id(group, "NZBID")?;
    let raw_name = text(group, "NZBName", 2048)?.to_owned();
    let filename = optional_text(group, "NZBFilename", 2048)?.to_owned();
    let category = optional_text(group, "Category", 256)?.to_owned();
    let eta = if progress.paused || global.rate == 0 {
        None
    } else {
        let total = ahead.saturating_add(progress.waiting) / global.rate;
        Some(total).filter(|seconds| *seconds <= MAX_ETA_SECONDS)
    };
    let item = DownloadItem {
        hash: remote_id(id),
        domain,
        name: display_name(rpc, &raw_name),
        category: category.clone(),
        status: progress.status,
        diagnostic: progress.diagnostic,
        progress: if progress.size == 0 {
            0.0
        } else {
            (progress.size - progress.remaining) as f64 / progress.size as f64
        },
        size_bytes: progress.size,
        remaining_bytes: Some(progress.remaining),
        download_bytes_per_second: 0,
        upload_bytes_per_second: 0,
        eta_seconds: eta,
        ratio: 0.0,
        seeding_seconds: None,
        completed: false,
    };
    Ok(Active {
        id,
        raw_name,
        filename,
        category,
        item,
    })
}

struct Finished {
    id: i64,
    raw_name: String,
    category: String,
    item: DownloadItem,
    storage: String,
    deleted: bool,
}
/// Detail statuses default to success when absent (older servers omit some of them).
fn detail<'v>(entry: &'v Value, key: &str) -> Result<&'v str> {
    optional_text(entry, key, 64)
}
fn history_item(rpc: &Rpc<'_, '_>, entry: &Value, domain: MediaDomain) -> Result<Finished> {
    let id = nzb_id(entry, "NZBID")?;
    let name = text(entry, "Name", 2048)?.to_owned();
    let category = optional_text(entry, "Category", 256)?.to_owned();
    let size = size64(entry, "FileSize")?;
    let par = detail(entry, "ParStatus")?;
    let unpack = detail(entry, "UnpackStatus")?;
    let moved = detail(entry, "MoveStatus")?;
    let script = detail(entry, "ScriptStatus")?;
    let delete = detail(entry, "DeleteStatus")?;
    let mark = detail(entry, "MarkStatus")?;
    let url = detail(entry, "UrlStatus")?;
    let summary = detail(entry, "Status")?.to_ascii_uppercase();
    let ok = |s: &str| matches!(s, "" | "SUCCESS" | "NONE");
    let (mut status, mut diagnostic) = (DownloadStatus::Completed, None);
    let mut deleted_by_user = false;
    let has_details = [par, unpack, moved, script, delete, mark]
        .iter()
        .any(|s| !s.is_empty());
    if has_details {
        if delete == "MANUAL" {
            // A job the user deleted is only evidence of failure when it was marked bad.
            if mark == "BAD" {
                status = DownloadStatus::Failed;
                diagnostic = Some(DownloadDiagnostic::Error);
            } else {
                deleted_by_user = true;
            }
        } else {
            // NZBGet grades each detail status itself (daemon/queue/DownloadInfo.cpp,
            // `NzbInfo::MakeTextStatus`), and the most severe finding wins: failure over warning
            // over success.
            let mut failed = matches!(url, "FAILURE" | "SCAN_FAILURE");
            let mut warning = url == "SCAN_SKIPPED";
            // ParStatus: MANUAL is a damaged file the user allowed, REPAIR_POSSIBLE is repairable;
            // NZBGet reports both as warnings.
            if par == "MANUAL" || par == "REPAIR_POSSIBLE" {
                warning = true;
            } else if !ok(par) {
                failed = true;
            }
            // UnpackStatus: SPACE (no disk space) and PASSWORD (encrypted archive) are warnings.
            if unpack == "SPACE" {
                warning = true;
                diagnostic = Some(DownloadDiagnostic::DiskFull);
            } else if unpack == "PASSWORD" {
                warning = true;
            } else if !ok(unpack) {
                failed = true;
            }
            // MoveStatus: the files never reached the destination.
            if !ok(moved) {
                failed = true;
            }
            // ScriptStatus: only a post-processing script failed, the download itself is intact.
            if script == "FAILURE" {
                warning = true;
            } else if !ok(script) {
                failed = true;
            }
            // MarkStatus: the user marked the download as bad.
            if mark == "BAD" {
                failed = true;
            }
            if !ok(delete) {
                if matches!(delete, "HEALTH" | "DUPE" | "SCAN" | "COPY" | "BAD") {
                    failed = true;
                } else {
                    warning = true;
                }
            }
            if failed {
                status = DownloadStatus::Failed;
                diagnostic = Some(DownloadDiagnostic::Error);
            } else if warning {
                status = DownloadStatus::Warning;
            }
        }
    } else {
        // No detail fields: fall back to the combined `KIND/DETAIL` summary.
        let (kind, reason) = summary.split_once('/').unwrap_or((summary.as_str(), ""));
        match kind {
            "SUCCESS" => {}
            "WARNING" => status = DownloadStatus::Warning,
            "FAILURE" => {
                status = DownloadStatus::Failed;
                diagnostic = Some(DownloadDiagnostic::Error);
            }
            "DELETED" if matches!(reason, "BAD" | "HEALTH" | "DUPE" | "SCAN" | "COPY") => {
                status = DownloadStatus::Failed;
                diagnostic = Some(DownloadDiagnostic::Error);
            }
            "DELETED" => deleted_by_user = true,
            _ => {
                status = DownloadStatus::Unknown;
                diagnostic = Some(DownloadDiagnostic::UnknownState);
            }
        }
    }
    let completed = status == DownloadStatus::Completed;
    let storage = {
        let final_dir = optional_text(entry, "FinalDir", 4096)?.trim();
        if final_dir.is_empty() {
            optional_text(entry, "DestDir", 4096)?.trim()
        } else {
            final_dir
        }
        .to_owned()
    };
    let item = DownloadItem {
        hash: remote_id(id),
        domain,
        name: display_name(rpc, &name),
        category: category.clone(),
        status,
        diagnostic,
        progress: if completed { 1.0 } else { 0.0 },
        size_bytes: size,
        remaining_bytes: Some(0),
        download_bytes_per_second: 0,
        upload_bytes_per_second: 0,
        eta_seconds: if completed { Some(0) } else { None },
        ratio: 0.0,
        seeding_seconds: None,
        completed,
    };
    Ok(Finished {
        id,
        raw_name: name,
        category,
        item,
        storage,
        deleted: deleted_by_user,
    })
}

/// Read-only domain-isolated observation of the whole queue and history in the configured
/// category. NZBGet has no server-side category filter or paging, so oversized lists fail closed.
async fn observe(
    rpc: &mut Rpc<'_, '_>,
    configured: &NzbgetScope,
    domain: MediaDomain,
) -> Result<(Vec<Active>, Vec<Finished>)> {
    let global = rpc.global().await?;
    let groups = rpc.groups().await?;
    let mut active = Vec::new();
    let mut cumulative = 0u64;
    for group in &groups {
        let category = optional_text(group, "Category", 256)?;
        let progress = queue_progress(group, &global)?;
        let ahead = cumulative;
        if !progress.paused && global.rate != 0 {
            cumulative = cumulative.saturating_add(progress.waiting);
        }
        if !same_category(category, &configured.category) {
            continue;
        }
        active.push(queue_item(rpc, group, domain, &global, ahead, progress)?);
    }
    let history = rpc.history().await?;
    let mut finished = Vec::new();
    for entry in &history {
        // Duplicate-check records are not downloads.
        if entry.get("Kind").and_then(Value::as_str) == Some("DUP") {
            continue;
        }
        let category = optional_text(entry, "Category", 256)?;
        if !same_category(category, &configured.category) {
            continue;
        }
        let item = history_item(rpc, entry, domain)?;
        if !item.deleted {
            finished.push(item);
        }
    }
    Ok((active, finished))
}

pub async fn refresh_pages(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
    domain: MediaDomain,
) -> Result<Vec<DownloadItem>> {
    let configured = scope(settings, domain)?;
    let mut rpc = Rpc::new(operation, settings, credentials)?;
    let (active, finished) = observe(&mut rpc, configured, domain).await?;
    let mut seen = BTreeSet::new();
    let mut items = Vec::new();
    for entry in active {
        if !seen.insert(entry.id) {
            return Err(QbitError::InvalidResponse);
        }
        items.push(entry.item);
    }
    let mut history_seen = BTreeSet::new();
    for entry in finished {
        if !history_seen.insert(entry.id) {
            return Err(QbitError::InvalidResponse);
        }
        // A job moving from queue to history between the reads is reported once, as active.
        if seen.insert(entry.id) {
            items.push(entry.item);
        }
    }
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
    let all = refresh_pages(operation, settings, credentials, request.domain).await?;
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

/// Search queue and history by job-name marker. The category is not filtered, so a job moved to
/// another category is a visible scope conflict rather than an absent job.
async fn find_token(
    rpc: &mut Rpc<'_, '_>,
    configured: &NzbgetScope,
    token: &str,
) -> Result<Option<i64>> {
    let mut found = BTreeSet::new();
    let mut foreign = false;
    let groups = rpc.groups().await?;
    for group in &groups {
        let name = text(group, "NZBName", 2048)?;
        let filename = optional_text(group, "NZBFilename", 2048)?;
        if !name_has_token(name, token) && !name_has_token(filename, token) {
            continue;
        }
        if same_category(optional_text(group, "Category", 256)?, &configured.category) {
            found.insert(nzb_id(group, "NZBID")?);
        } else {
            foreign = true;
        }
    }
    let history = rpc.history().await?;
    for entry in &history {
        if entry.get("Kind").and_then(Value::as_str) == Some("DUP") {
            continue;
        }
        if !name_has_token(text(entry, "Name", 2048)?, token) {
            continue;
        }
        // A job the user deleted from history no longer exists for reconciliation.
        if detail(entry, "DeleteStatus")? == "MANUAL" && detail(entry, "MarkStatus")? != "BAD" {
            continue;
        }
        if same_category(optional_text(entry, "Category", 256)?, &configured.category) {
            found.insert(nzb_id(entry, "NZBID")?);
        } else {
            foreign = true;
        }
    }
    if foreign {
        return Err(QbitError::ScopeConflict);
    }
    match found.len() {
        0 => Ok(None),
        1 => Ok(found.into_iter().next()),
        _ => Err(QbitError::InvalidResponse),
    }
}

pub async fn test_connection(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
) -> std::result::Result<NzbgetTest, NzbgetTestFailure> {
    settings.validate().map_err(|_| QbitError::InvalidRequest)?;
    let mut rpc = Rpc::new(operation, settings, credentials)?;
    let (version, raw_version) = supported_version(&mut rpc).await?;
    let mut warnings = Vec::new();
    if version == Version::Testing {
        warnings.push(NzbgetTestWarning::TestingVersion);
    }
    let config = rpc.config().await?;
    let listed = categories(&config);
    let days = keep_history_days(&config)?;
    if days == 0 {
        return Err(problem(NzbgetProblem::KeepHistoryZero));
    }
    if days > MAX_KEEP_HISTORY_DAYS {
        return Err(problem(NzbgetProblem::KeepHistoryOverMax));
    }
    let mut domains = Vec::new();
    for domain in [MediaDomain::Tv, MediaDomain::Movies] {
        let Ok(configured) = scope(settings, domain) else {
            continue;
        };
        if !listed
            .iter()
            .any(|c| same_category(&c.name, &configured.category))
        {
            return Err(problem(NzbgetProblem::CategoryMissing));
        }
        domains.push(domain);
    }
    // Authentication must also authorize reading jobs, and the lists must be within bounds.
    rpc.global().await?;
    rpc.groups().await?;
    rpc.history().await?;
    if days < MIN_COMFORTABLE_KEEP_HISTORY_DAYS {
        warnings.push(NzbgetTestWarning::ShortHistoryRetention);
    }
    Ok(NzbgetTest {
        nzbget_version: rpc.redact(raw_version),
        domains,
        warnings,
    })
}

/// Validates target/scope and locator, deriving the deterministic identity. No network I/O.
pub fn prepare(
    provider: Uuid,
    settings: &ProviderSettings,
    target: MediaTarget,
    url: String,
    title: &str,
    recent: bool,
) -> Result<PreparedNzb> {
    let domain = target_domain(&target)?;
    scope(settings, domain)?;
    if url.len() > 8192 || url.chars().any(char::is_control) {
        return Err(QbitError::InvalidRequest);
    }
    let parsed = url::Url::parse(&url).map_err(|_| QbitError::InvalidRequest)?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || parsed.fragment().is_some()
    {
        return Err(QbitError::InvalidRequest);
    }
    let fingerprint = settings_fingerprint(settings)?;
    let token = submission_token(provider, &fingerprint, &target, &url);
    let identity = SubmissionIdentity::tokenized(
        target,
        token.clone(),
        fingerprint,
        digest_hex(&ring::digest::SHA256, url.as_bytes()),
    );
    Ok(PreparedNzb {
        identity,
        job_name: job_name(title, &token),
        url,
        recent,
    })
}
fn validate_identity(
    identity: &SubmissionIdentity,
    settings: &ProviderSettings,
) -> Result<(MediaDomain, String)> {
    let domain = target_domain(identity.target())?;
    scope(settings, domain)?;
    let [token] = identity.hashes() else {
        return Err(QbitError::InvalidRequest);
    };
    if identity.version() != 1
        || !hex_token(token)
        || identity.payload_sha256().len() != 64
        || !identity
            .payload_sha256()
            .bytes()
            .all(|b| b.is_ascii_hexdigit())
    {
        return Err(QbitError::InvalidRequest);
    }
    if identity.settings_fingerprint() != settings_fingerprint(settings)? {
        return Err(QbitError::ScopeConflict);
    }
    Ok((domain, token.clone()))
}
pub async fn reconcile(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
    identity: &SubmissionIdentity,
) -> Result<Reconciliation> {
    let (domain, token) = validate_identity(identity, settings)?;
    let configured = scope(settings, domain)?;
    let mut rpc = Rpc::new(operation, settings, credentials)?;
    Ok(match find_token(&mut rpc, configured, &token).await? {
        Some(id) => Reconciliation::Observed {
            hash: remote_id(id),
        },
        None => Reconciliation::NotObserved,
    })
}
/// Submits a prepared NZB URL; see [`submit_classified`] for the dispatch classification.
pub async fn submit(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
    prepared: PreparedNzb,
) -> Result<AddOutcome> {
    submit_classified(operation, settings, credentials, prepared)
        .await
        .map_err(|e| e.error)
}
/// Never retried: once `append` is dispatched any failure is `MutationUnknown`-class (except an
/// explicit refusal) and the caller must reconcile by marker instead of submitting again.
pub(crate) async fn submit_classified(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
    prepared: PreparedNzb,
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
            client_retry_eligible: !matches!(
                error,
                QbitError::InvalidRequest
                    | QbitError::Http(
                        HttpError::InvalidRequest
                            | HttpError::LocalUnavailable { .. }
                            | HttpError::Busy
                            | HttpError::ResponseTooLarge
                    )
            ),
        })
}
async fn submit_inner(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
    prepared: PreparedNzb,
    add_started: &mut bool,
) -> Result<AddOutcome> {
    let (domain, token) = validate_identity(&prepared.identity, settings)?;
    if prepared.identity.payload_sha256()
        != digest_hex(&ring::digest::SHA256, prepared.url.as_bytes())
    {
        return Err(QbitError::InvalidRequest);
    }
    let configured = scope(settings, domain)?;
    let mut rpc = Rpc::new(operation, settings, credentials)?;
    supported_version(&mut rpc).await?;
    // An unknown category would silently land in the default destination and break domain
    // isolation of completed output.
    if !categories(&rpc.config().await?)
        .iter()
        .any(|c| same_category(&c.name, &configured.category))
    {
        return Err(QbitError::UnsupportedFeature);
    }
    if let Some(id) = find_token(&mut rpc, configured, &token).await? {
        return Ok(AddOutcome::AlreadyPresent {
            hash: remote_id(id),
        });
    }
    let priority = if prepared.recent {
        configured.recent_priority
    } else {
        configured.older_priority
    };
    operation.ensure_active()?;
    *add_started = true;
    let result = rpc
        .call(
            "append",
            json!([
                format!("{}.nzb", prepared.job_name),
                prepared.url,
                configured.category,
                priority.wire(),
                false,
                configured.add_paused,
                "",
                0,
                DUPE_MODE,
            ]),
        )
        .await
        .map_err(|error| match error {
            QbitError::Http(HttpError::Authentication) => error,
            _ => QbitError::MutationUnknown,
        })?;
    let id = match &result {
        // NZBGet answers 0 (or a negative number) when it declined to queue the NZB; no job
        // exists, so this is a definite refusal rather than an unknown mutation.
        Value::Number(n) if n.as_i64().is_some_and(|n| n <= 0) => {
            *add_started = false;
            return Err(QbitError::Rejected);
        }
        Value::Bool(false) => {
            *add_started = false;
            return Err(QbitError::Rejected);
        }
        Value::Number(n) => n
            .as_i64()
            .filter(|id| (1..=MAX_NZB_ID).contains(id))
            .ok_or(QbitError::MutationUnknown)?,
        _ => return Err(QbitError::MutationUnknown),
    };
    // Postcondition: the returned job exists in this domain's category.
    let groups = rpc.groups().await.map_err(|_| QbitError::MutationUnknown)?;
    let history = rpc
        .history()
        .await
        .map_err(|_| QbitError::MutationUnknown)?;
    for (list, category_key, name_key) in [
        (&groups, "Category", "NZBName"),
        (&history, "Category", "Name"),
    ] {
        for entry in list {
            if nzb_id(entry, "NZBID").ok() != Some(id) {
                continue;
            }
            return if optional_text(entry, category_key, 256)
                .is_ok_and(|c| same_category(c, &configured.category))
                && optional_text(entry, name_key, 2048)
                    .is_ok_and(|n| name_has_token(n, &token) || n.contains("://"))
            {
                Ok(AddOutcome::Observed {
                    hash: remote_id(id),
                })
            } else {
                Err(QbitError::MutationUnknown)
            };
        }
    }
    Err(QbitError::MutationUnknown)
}

#[doc(hidden)]
pub async fn submit_classified_for_tests(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
    prepared: PreparedNzb,
) -> std::result::Result<AddOutcome, (NzbgetError, bool)> {
    submit_classified(operation, settings, credentials, prepared)
        .await
        .map_err(|e| (e.error, e.definitely_not_submitted))
}
/// `(remote root, root came from an explicit category directory, removes completed downloads)`.
#[doc(hidden)]
pub async fn client_status_snapshot_for_tests(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
    domain: MediaDomain,
) -> Result<(String, bool, bool)> {
    match client_status_snapshot(operation, settings, credentials, domain).await? {
        ClientStatusSnapshot::Status(status) => Ok((
            status.remote_root,
            status.root_source == RootSource::Category,
            status.removes_completed_downloads,
        )),
        _ => Err(QbitError::InvalidResponse),
    }
}

/// Locates one job by its remote identity within the domain's bounded observation.
pub async fn job(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
    target: &MediaTarget,
    remote: &str,
) -> Result<UsenetJob> {
    let domain = target_domain(target)?;
    let configured = scope(settings, domain)?;
    if !hex_token(remote) {
        return Err(QbitError::InvalidRequest);
    }
    let mut rpc = Rpc::new(operation, settings, credentials)?;
    let (active, finished) = observe(&mut rpc, configured, domain).await?;
    let mut matches = Vec::new();
    for entry in active {
        if entry.item.hash == remote {
            matches.push(UsenetJob {
                item: entry.item,
                storage: None,
            });
        }
    }
    for entry in finished {
        if entry.item.hash != remote {
            continue;
        }
        let storage = if entry.item.completed {
            Some(output_path(&entry.storage, &entry.raw_name)?)
        } else {
            None
        };
        matches.push(UsenetJob {
            item: entry.item,
            storage,
        });
    }
    match matches.len() {
        0 => Err(QbitError::NotFound),
        1 => Ok(matches.remove(0)),
        _ => Err(QbitError::InvalidResponse),
    }
}

/// Private output-root facts for health: the category's destination directory.
pub(crate) async fn client_status_snapshot(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
    domain: MediaDomain,
) -> Result<ClientStatusSnapshot> {
    let configured = scope(settings, domain)?;
    let locality = endpoint_locality(settings.endpoint())?;
    let mut rpc = Rpc::new(operation, settings, credentials)?;
    let config = rpc.config().await?;
    let removes = keep_history_days(&config)? == 0;
    let listed = categories(&config);
    let Some(category) = listed
        .iter()
        .find(|c| same_category(&c.name, &configured.category))
    else {
        return Ok(ClientStatusSnapshot::OutputMissing(locality));
    };
    if category.dest_dir.is_empty() {
        return Ok(ClientStatusSnapshot::OutputMissing(locality));
    }
    let Ok((remote_root, _)) = remote_root(&category.dest_dir, None) else {
        return Ok(ClientStatusSnapshot::OutputInvalid(locality));
    };
    Ok(ClientStatusSnapshot::Status(ClientStatus {
        domain,
        remote_root,
        root_source: if category.explicit_dest {
            RootSource::Category
        } else {
            RootSource::Default
        },
        locality,
        removes_completed_downloads: removes,
    }))
}

/// Explicit queue/state controls. A caller must persist target + identity + intent first.
/// Each is followed by a verifying read; imported categories and file priorities do not exist.
pub async fn control(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
    target: &MediaTarget,
    expected: &str,
    action: Control,
) -> Result<()> {
    let domain = target_domain(target)?;
    let configured = scope(settings, domain)?;
    if !hex_token(expected) {
        return Err(QbitError::InvalidRequest);
    }
    let mut rpc = Rpc::new(operation, settings, credentials)?;
    let (active, finished) = observe(&mut rpc, configured, domain).await?;
    let queued = active.iter().find(|e| e.item.hash == expected);
    let historic = finished.iter().find(|e| e.item.hash == expected);
    let (command, param, id) = match (&action, queued, historic) {
        (_, None, None) => return Err(QbitError::NotFound),
        (Control::Pause, Some(entry), _) => ("GroupPause", "", entry.id),
        (Control::Resume, Some(entry), _) => ("GroupResume", "", entry.id),
        (Control::Priority { first: true }, Some(entry), _) => ("GroupMoveTop", "", entry.id),
        (Control::Priority { first: false }, Some(entry), _) => ("GroupMoveBottom", "", entry.id),
        (Control::Remove { .. }, Some(entry), _) => ("GroupFinalDelete", "", entry.id),
        // NZBGet cannot delete a finished job's files through its API.
        (
            Control::Remove {
                delete_files: false,
            },
            None,
            Some(entry),
        ) => ("HistoryDelete", "", entry.id),
        _ => return Err(QbitError::UnsupportedFeature),
    };
    let confirmation = rpc
        .call("editqueue", json!([command, 0, param, [id]]))
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
    if confirmation != Value::Bool(true) {
        return Err(QbitError::Rejected);
    }
    let (after_active, after_finished) = observe(&mut rpc, configured, domain)
        .await
        .map_err(|_| QbitError::MutationUnknown)?;
    let after = after_active.iter().find(|e| e.id == id);
    let confirmed = match action {
        Control::Remove { .. } => after.is_none() && !after_finished.iter().any(|e| e.id == id),
        Control::Pause => after.is_some_and(|e| e.item.status == DownloadStatus::Paused),
        Control::Resume => after.is_some_and(|e| e.item.status != DownloadStatus::Paused),
        Control::Priority { first: true } => after_active.first().is_some_and(|e| e.id == id),
        Control::Priority { first: false } => after_active.last().is_some_and(|e| e.id == id),
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
    fn tokens_are_deterministic_bounded_and_marked() {
        let provider = Uuid::nil();
        let a = submission_token(provider, "f", &MediaTarget::Episode(1), "https://i/1");
        assert_eq!(
            a,
            submission_token(provider, "f", &MediaTarget::Episode(1), "https://i/1")
        );
        assert_ne!(
            a,
            submission_token(provider, "f", &MediaTarget::Movie(1), "https://i/1")
        );
        assert!(hex_token(&a));
        let name = job_name("Show/S01E01{{pw}}", &a);
        assert!(name.starts_with("Show_S01E01__pw__.hrrdarr-"));
        assert!(name_has_token(&name, &a));
        assert!(job_name(&"x".repeat(1000), &a).len() <= MAX_JOB_TITLE_BYTES + 9 + TOKEN_HEX);
    }
    #[test]
    fn versions_sizes_and_categories_are_bounded() {
        assert_eq!(parse_version("21.1"), Ok(Version::Release(21, 1)));
        assert_eq!(parse_version("20.0-r2312"), Ok(Version::Release(20, 0)));
        assert_eq!(
            parse_version("21.0-testing-r2345"),
            Ok(Version::Release(21, 0))
        );
        assert_eq!(parse_version("testing-r2345"), Ok(Version::Testing));
        assert_eq!(parse_version("13"), Ok(Version::Release(13, 0)));
        assert!(parse_version("four").is_err());
        let size = json!({"FileSizeLo": 0, "FileSizeHi": 1});
        assert_eq!(size64(&size, "FileSize"), Ok(1 << 32));
        assert_eq!(size64(&json!({"FileSizeMB": 2}), "FileSize"), Ok(2_097_152));
        assert!(size64(&json!({"FileSizeLo": u64::MAX}), "FileSize").is_err());
        let config: BTreeMap<String, String> = [
            ("MainDir", "/data"),
            ("DestDir", "${MainDir}/dst"),
            ("Category1.Name", "tv"),
            ("Category2.Name", "Movies"),
            ("Category2.DestDir", "/films"),
            ("KeepHistory", "7"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
        let listed = categories(&config);
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].dest_dir, "/data/dst/tv");
        assert!(!listed[0].explicit_dest);
        assert_eq!(listed[1].dest_dir, "/films");
        assert_eq!(keep_history_days(&config), Ok(7));
        assert!(keep_history_days(&[("KeepHistory".to_owned(), "x".to_owned())].into()).is_err());
    }
}
