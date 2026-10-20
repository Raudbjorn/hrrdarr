//! SABnzbd JSON API operations over the bounded provider transport.
//!
//! Requirements were inventoried from the pinned Sonarr/Radarr SABnzbd clients; the wire contract
//! follows the public SABnzbd API reference. No scheduler, automatic mutation retry or local
//! filesystem access happens here: callers own durable intent, path mapping and imports.
use super::{
    ClientRetryKind, Credentials, ProviderSettings, UsenetScope,
    http::{HttpError, HttpOperation, HttpRequestBody},
    qbittorrent::{
        AddOutcome, ClientStatus, ClientStatusSnapshot, DownloadDiagnostic, DownloadItem,
        DownloadPage, DownloadQuery, DownloadStatus, QbitError, Reconciliation, RootSource,
        SubmissionError, SubmissionIdentity, digest_hex, endpoint_locality, remote_root,
        target_domain, validate_remote_path,
    },
};
use crate::{api::MediaDomain, db::MediaTarget};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;
use uuid::Uuid;

/// Shared client error vocabulary (named after its first protocol; not qBittorrent-specific).
pub type SabError = QbitError;
type Result<T> = std::result::Result<T, SabError>;

/// Queue/history pages are bounded; a full queue page is not proof that nothing follows.
const PAGE_SLOTS: usize = 500;
const LOOKUP_SLOTS: usize = 50;
const MAX_REFRESH_BYTES: usize = 1024 * 1024;
const MIN_MAJOR_VERSION: u32 = 3;
const TOKEN_MARKER: &str = ".hrrdarr-";
const TOKEN_HEX: usize = 40;
const MAX_JOB_TITLE_BYTES: usize = 180;
const BYTES_PER_MIB: f64 = 1_048_576.0;
const MAX_REPORTED_BYTES: f64 = 1.0e15;
const MAX_ETA_SECONDS: u64 = 365 * 24 * 60 * 60;
/// The pinned references treat this exact failure message as recoverable, not as a failed download.
const DISK_FULL_MESSAGE: &str = "unpacking failed, write error or disk is full?";

#[derive(Serialize, ts_rs::TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SabnzbdTestWarning {
    /// `develop` builds do not report a comparable version.
    DevelopVersion,
    /// History retention may remove completed jobs before they can be imported.
    RemovesCompletedDownloads,
}
#[derive(Serialize, ts_rs::TS, Debug)]
pub struct SabnzbdTest {
    pub sabnzbd_version: String,
    pub domains: Vec<MediaDomain>,
    pub warnings: Vec<SabnzbdTestWarning>,
}
/// A static, user-actionable configuration problem detected by a connection test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SabnzbdProblem {
    CategoryMissing,
    JobFoldersDisabled,
    SortingEnabled,
}
#[derive(Debug)]
pub struct SabTestFailure {
    pub error: SabError,
    pub problem: Option<SabnzbdProblem>,
}
impl From<SabError> for SabTestFailure {
    fn from(error: SabError) -> Self {
        Self {
            error,
            problem: None,
        }
    }
}
fn problem(problem: SabnzbdProblem) -> SabTestFailure {
    SabTestFailure {
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

/// A located SABnzbd job; `storage` is the remote final destination of a completed job.
pub struct UsenetJob {
    pub item: DownloadItem,
    pub storage: Option<String>,
}

fn scope(settings: &ProviderSettings, domain: MediaDomain) -> Result<&UsenetScope> {
    settings.validate().map_err(|_| QbitError::InvalidRequest)?;
    match settings {
        ProviderSettings::Sabnzbd { tv, movies, .. } => match domain {
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
/// Remote identity encoding: SABnzbd `nzo_id` values are opaque, so the journal stores a fixed
/// 40-hex digest of the exact identifier rather than the identifier itself.
pub fn remote_id(nzo_id: &str) -> String {
    digest_hex(
        &ring::digest::SHA1_FOR_LEGACY_USE_ONLY,
        format!("sabnzbd-nzo:{nzo_id}").as_bytes(),
    )
}
/// Eligibility tags do not change remote submission behaviour; exclude them like qBittorrent.
pub(crate) fn settings_fingerprint(settings: &ProviderSettings) -> Result<String> {
    let mut settings = settings.clone();
    if let ProviderSettings::Sabnzbd { tv, movies, .. } = &mut settings {
        for scope in tv.iter_mut().chain(movies.iter_mut()) {
            scope.tags = None;
        }
    } else {
        return Err(QbitError::ScopeConflict);
    }
    let bytes = serde_json::to_vec(&settings).map_err(|_| QbitError::InvalidRequest)?;
    Ok(digest_hex(&ring::digest::SHA256, &bytes))
}
/// Deterministic per client, configuration, typed target and release locator. The same release
/// for the same target on the same client yields the same job marker, so a retried preparation
/// reproduces its journal identity and a lost add response can be reconciled by name.
fn submission_token(provider: Uuid, fingerprint: &str, target: &MediaTarget, url: &str) -> String {
    let target = match target {
        MediaTarget::Episode(id) => format!("episode:{id}"),
        MediaTarget::Movie(id) => format!("movie:{id}"),
    };
    let mut token = digest_hex(
        &ring::digest::SHA256,
        format!("hrrdarr/sabnzbd/v1\0{provider}\0{fingerprint}\0{target}\0{url}").as_bytes(),
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

struct Api<'o, 'a> {
    operation: &'o HttpOperation<'a>,
    url: String,
    key: Option<&'o str>,
}
impl<'o, 'a> Api<'o, 'a> {
    fn new(
        operation: &'o HttpOperation<'a>,
        settings: &ProviderSettings,
        credentials: Option<&'o Credentials>,
    ) -> Result<Self> {
        let ProviderSettings::Sabnzbd { endpoint, .. } = settings else {
            return Err(QbitError::ScopeConflict);
        };
        let key = match credentials {
            None => None,
            Some(Credentials::ApiKey { api_key }) => Some(api_key.as_str()),
            Some(_) => return Err(QbitError::InvalidRequest),
        };
        let mut url = url::Url::parse(endpoint).map_err(|_| QbitError::InvalidRequest)?;
        let path = format!("{}/api", url.path().trim_end_matches('/'));
        url.set_path(&path);
        Ok(Self {
            operation,
            url: url.into(),
            key,
        })
    }
    fn redact(&self, mut value: String) -> String {
        if let Some(key) = self.key.filter(|k| !k.is_empty()) {
            value = value.replace(key, "[redacted]");
        }
        value
    }
    /// One API call. `status:false` responses and plain `error:` bodies are classified without
    /// reflecting their text; authentication failures are distinct from other rejections.
    async fn call(
        &self,
        mode: &str,
        parameters: &[(&str, String)],
        authenticated: bool,
    ) -> Result<Value> {
        let mut query = vec![("mode".to_owned(), mode.to_owned())];
        query.extend(parameters.iter().map(|(k, v)| ((*k).to_owned(), v.clone())));
        query.push(("output".into(), "json".into()));
        if authenticated {
            if let Some(key) = self.key {
                query.push(("apikey".into(), key.to_owned()));
            }
        }
        let response = self
            .operation
            .request(&self.url, &query, &[], HttpRequestBody::Empty)
            .await?;
        if !(200..300).contains(&response.status) {
            return Err(QbitError::InvalidResponse);
        }
        let text = std::str::from_utf8(&response.body).map_err(|_| QbitError::InvalidResponse)?;
        let trimmed = text.trim();
        if trimmed.len() >= 6 && trimmed[..6].eq_ignore_ascii_case("error:") {
            return Err(classify_error(&trimmed[6..]));
        }
        let value: Value = serde_json::from_str(trimmed).map_err(|_| QbitError::InvalidResponse)?;
        if value
            .get("status")
            .is_some_and(|v| v.as_bool() == Some(false) || v.as_str() == Some("false"))
        {
            return Err(classify_error(
                value.get("error").and_then(Value::as_str).unwrap_or(""),
            ));
        }
        Ok(value)
    }
}
fn classify_error(message: &str) -> SabError {
    let lower = message.to_ascii_lowercase();
    if lower.contains("api key incorrect") || lower.contains("api key required") {
        QbitError::Http(HttpError::Authentication)
    } else {
        QbitError::Rejected
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Version {
    Release(u32, u32),
    Develop,
}
fn parse_version(value: &Value) -> Result<(Version, String)> {
    let raw = value
        .get("version")
        .and_then(Value::as_str)
        .ok_or(QbitError::InvalidResponse)?
        .trim();
    if raw.is_empty() || raw.len() > 32 || !raw.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(QbitError::InvalidResponse);
    }
    if raw.eq_ignore_ascii_case("develop") {
        return Ok((Version::Develop, raw.to_owned()));
    }
    let mut parts = raw.split('.');
    let number = |part: Option<&str>| -> Result<u32> {
        let digits: String = part
            .ok_or(QbitError::InvalidResponse)?
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse().map_err(|_| QbitError::InvalidResponse)
    };
    let major = number(parts.next())?;
    let minor = number(parts.next())?;
    Ok((Version::Release(major, minor), raw.to_owned()))
}
async fn supported_version(api: &Api<'_, '_>) -> Result<(Version, String)> {
    let (version, raw) = parse_version(&api.call("version", &[], false).await?)?;
    match version {
        Version::Release(major, _) if major < MIN_MAJOR_VERSION => {
            Err(QbitError::UnsupportedVersion)
        }
        version => Ok((version, raw)),
    }
}
async fn categories(api: &Api<'_, '_>) -> Result<Vec<String>> {
    let value = api.call("get_cats", &[], true).await?;
    let list = value
        .get("categories")
        .and_then(Value::as_array)
        .ok_or(QbitError::InvalidResponse)?;
    if list.len() > 1024 {
        return Err(QbitError::InvalidResponse);
    }
    list.iter()
        .map(|v| {
            v.as_str()
                .filter(|s| s.len() <= 256 && !s.chars().any(char::is_control))
                .map(str::to_owned)
                .ok_or(QbitError::InvalidResponse)
        })
        .collect()
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
/// SABnzbd reports some numbers as decimal strings ("1277.65") and others as JSON numbers.
fn decimal(value: &Value, key: &str) -> Result<f64> {
    let number = match value.get(key) {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) if s.len() <= 32 => s.trim().parse::<f64>().ok(),
        _ => None,
    }
    .ok_or(QbitError::InvalidResponse)?;
    if !number.is_finite() || !(0.0..=MAX_REPORTED_BYTES).contains(&number) {
        return Err(QbitError::InvalidResponse);
    }
    Ok(number)
}
fn mib_bytes(value: &Value, key: &str) -> Result<u64> {
    let bytes = (decimal(value, key)? * BYTES_PER_MIB).round();
    if bytes > MAX_REPORTED_BYTES {
        return Err(QbitError::InvalidResponse);
    }
    Ok(bytes as u64)
}
fn nzo_id(slot: &Value) -> Result<String> {
    let id = text(slot, "nzo_id", 128)?;
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(QbitError::InvalidResponse);
    }
    Ok(id.to_owned())
}
/// `H:MM:SS` or `D:HH:MM:SS`; malformed or implausible estimates are unknown.
fn time_left(value: &str) -> Option<u64> {
    let parts: Vec<u64> = value
        .split(':')
        .map(|p| p.trim().parse::<u64>().ok())
        .collect::<Option<_>>()?;
    let seconds = match parts.as_slice() {
        [h, m, s] if *m < 60 && *s < 60 => h.checked_mul(3600)?.checked_add(m * 60 + s)?,
        [d, h, m, s] if *h < 24 && *m < 60 && *s < 60 => {
            d.checked_mul(86400)?.checked_add(h * 3600 + m * 60 + s)?
        }
        _ => return None,
    };
    (seconds <= MAX_ETA_SECONDS).then_some(seconds)
}
/// A job name shown while a URL is still being fetched can be the locator, which may carry an
/// indexer credential. Never project it. The trailing reconciliation marker is internal and is
/// removed from the projected name so it is not mistaken for release facts (e.g. a group).
fn display_name(api: &Api<'_, '_>, raw: &str) -> String {
    if raw.contains("://") {
        return "(fetching NZB)".into();
    }
    let shown = match raw.rfind(TOKEN_MARKER) {
        Some(at) if at > 0 && hex_token(&raw[at + TOKEN_MARKER.len()..]) => &raw[..at],
        _ => raw,
    };
    api.redact(shown.to_owned())
}
fn queue_item(
    api: &Api<'_, '_>,
    slot: &Value,
    domain: MediaDomain,
    paused_all: bool,
) -> Result<(String, String, String, DownloadItem)> {
    let nzo = nzo_id(slot)?;
    let raw_name = text(slot, "filename", 2048)?.to_owned();
    let category = text(slot, "cat", 256)?.to_owned();
    let state = text(slot, "status", 64)?;
    let forced = match slot.get("priority") {
        Some(Value::String(p)) => p.eq_ignore_ascii_case("force"),
        Some(Value::Number(n)) => n.as_i64() == Some(2),
        _ => false,
    };
    let size = mib_bytes(slot, "mb")?;
    let left = mib_bytes(slot, "mbleft")?.min(size);
    let (status, diagnostic) = if (paused_all && !forced) || state.eq_ignore_ascii_case("paused") {
        (DownloadStatus::Paused, None)
    } else {
        match state.to_ascii_lowercase().as_str() {
            "queued" | "grabbing" | "propagating" => (DownloadStatus::Queued, None),
            "downloading" | "fetching" | "checking" | "quickcheck" => {
                (DownloadStatus::Downloading, None)
            }
            "verifying" => (
                DownloadStatus::Downloading,
                Some(DownloadDiagnostic::Verifying),
            ),
            "repairing" => (
                DownloadStatus::Downloading,
                Some(DownloadDiagnostic::Repairing),
            ),
            "extracting" => (
                DownloadStatus::Downloading,
                Some(DownloadDiagnostic::Extracting),
            ),
            "moving" => (
                DownloadStatus::Downloading,
                Some(DownloadDiagnostic::Moving),
            ),
            "running" => (
                DownloadStatus::Downloading,
                Some(DownloadDiagnostic::PostProcessing),
            ),
            _ => (
                DownloadStatus::Unknown,
                Some(DownloadDiagnostic::UnknownState),
            ),
        }
    };
    let eta = if matches!(status, DownloadStatus::Paused) {
        None
    } else {
        optional_text(slot, "timeleft", 32).ok().and_then(time_left)
    };
    let progress = if size == 0 {
        0.0
    } else {
        (size - left) as f64 / size as f64
    };
    let item = DownloadItem {
        hash: remote_id(&nzo),
        domain,
        name: display_name(api, &raw_name),
        category: category.clone(),
        status,
        diagnostic,
        progress,
        size_bytes: size,
        remaining_bytes: Some(left),
        download_bytes_per_second: 0,
        upload_bytes_per_second: 0,
        eta_seconds: eta,
        ratio: 0.0,
        seeding_seconds: None,
        completed: false,
    };
    Ok((nzo, raw_name, category, item))
}
struct HistorySlot {
    nzo: String,
    name: String,
    category: String,
    item: DownloadItem,
    storage: String,
    deleted: bool,
}
fn history_item(api: &Api<'_, '_>, slot: &Value, domain: MediaDomain) -> Result<HistorySlot> {
    let nzo = nzo_id(slot)?;
    let name = text(slot, "name", 2048)?.to_owned();
    let category = text(slot, "category", 256)?.to_owned();
    let state = text(slot, "status", 64)?.to_ascii_lowercase();
    let size = match slot.get("bytes") {
        Some(Value::Number(n)) => n.as_u64().filter(|b| (*b as f64) <= MAX_REPORTED_BYTES),
        Some(Value::String(s)) if s.len() <= 32 => s.trim().parse::<u64>().ok(),
        _ => None,
    }
    .ok_or(QbitError::InvalidResponse)?;
    let failure = optional_text(slot, "fail_message", 4096)?
        .trim()
        .to_ascii_lowercase();
    let storage = optional_text(slot, "storage", 4096)?.to_owned();
    let (status, diagnostic, completed) = match state.as_str() {
        "completed" => (DownloadStatus::Completed, None, true),
        "failed" if failure == DISK_FULL_MESSAGE => (
            DownloadStatus::Warning,
            Some(DownloadDiagnostic::DiskFull),
            false,
        ),
        "failed" => (
            DownloadStatus::Failed,
            Some(DownloadDiagnostic::Error),
            false,
        ),
        "queued" | "fetching" | "running" => (
            DownloadStatus::Downloading,
            Some(DownloadDiagnostic::PostProcessing),
            false,
        ),
        "quickcheck" | "verifying" => (
            DownloadStatus::Downloading,
            Some(DownloadDiagnostic::Verifying),
            false,
        ),
        "repairing" => (
            DownloadStatus::Downloading,
            Some(DownloadDiagnostic::Repairing),
            false,
        ),
        "extracting" => (
            DownloadStatus::Downloading,
            Some(DownloadDiagnostic::Extracting),
            false,
        ),
        "moving" => (
            DownloadStatus::Downloading,
            Some(DownloadDiagnostic::Moving),
            false,
        ),
        "deleted" => (DownloadStatus::Unknown, None, false),
        _ => (
            DownloadStatus::Unknown,
            Some(DownloadDiagnostic::UnknownState),
            false,
        ),
    };
    let item = DownloadItem {
        hash: remote_id(&nzo),
        domain,
        name: display_name(api, &name),
        category: category.clone(),
        status,
        diagnostic,
        progress: if completed || state != "failed" {
            1.0
        } else {
            0.0
        },
        size_bytes: size,
        remaining_bytes: Some(0),
        download_bytes_per_second: 0,
        upload_bytes_per_second: 0,
        eta_seconds: if completed { Some(0) } else { None },
        ratio: 0.0,
        seeding_seconds: None,
        completed,
    };
    Ok(HistorySlot {
        nzo,
        name,
        category,
        item,
        storage,
        deleted: state == "deleted",
    })
}
fn slots<'v>(value: &'v Value, list: &str) -> Result<&'v Vec<Value>> {
    value
        .get(list)
        .and_then(|v| v.get("slots"))
        .and_then(Value::as_array)
        .ok_or(QbitError::InvalidResponse)
}

/// Read-only domain-isolated observation of up to 500 active jobs and the most recent 500
/// history entries in the configured category. Older history is not observed.
async fn observe(
    api: &Api<'_, '_>,
    configured: &UsenetScope,
    domain: MediaDomain,
    extra: &[(&str, String)],
) -> Result<(Vec<(String, String, DownloadItem)>, Vec<HistorySlot>)> {
    let mut parameters = vec![
        ("cat", configured.category.clone()),
        ("start", "0".to_owned()),
        ("limit", PAGE_SLOTS.to_string()),
    ];
    parameters.extend(extra.iter().cloned());
    let queue = api.call("queue", &parameters, true).await?;
    let paused_all = queue
        .get("queue")
        .and_then(|q| q.get("paused"))
        .map(|v| v.as_bool().ok_or(QbitError::InvalidResponse))
        .transpose()?
        .unwrap_or(false);
    let queue_slots = slots(&queue, "queue")?;
    if queue_slots.len() >= PAGE_SLOTS {
        return Err(HttpError::ResponseTooLarge.into());
    }
    let mut active = Vec::new();
    for slot in queue_slots {
        let (nzo, raw, category, item) = queue_item(api, slot, domain, paused_all)?;
        if !same_category(&category, &configured.category) {
            return Err(QbitError::ScopeConflict);
        }
        active.push((nzo, raw, item));
    }
    let history = api.call("history", &parameters, true).await?;
    let history_slots = slots(&history, "history")?;
    if history_slots.len() > PAGE_SLOTS {
        return Err(QbitError::InvalidResponse);
    }
    let mut finished = Vec::new();
    for slot in history_slots {
        let entry = history_item(api, slot, domain)?;
        if !same_category(&entry.category, &configured.category) {
            return Err(QbitError::ScopeConflict);
        }
        if !entry.deleted {
            finished.push(entry);
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
    let api = Api::new(operation, settings, credentials)?;
    let (active, finished) = observe(&api, configured, domain, &[]).await?;
    let mut seen = BTreeSet::new();
    let mut items = Vec::new();
    for (nzo, _, item) in active {
        if !seen.insert(nzo) {
            return Err(QbitError::InvalidResponse);
        }
        items.push(item);
    }
    let mut history_seen = BTreeSet::new();
    for entry in finished {
        if !history_seen.insert(entry.nzo.clone()) {
            return Err(QbitError::InvalidResponse);
        }
        // A job moving from queue to history between the two reads is reported once, as active.
        if seen.insert(entry.nzo) {
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

/// Search both lists by job-name marker. Category is not filtered so a job moved to another
/// category is a visible scope conflict rather than an absent job.
async fn find_token(
    api: &Api<'_, '_>,
    configured: &UsenetScope,
    token: &str,
) -> Result<Option<String>> {
    let mut found = BTreeSet::new();
    let mut foreign = false;
    for (mode, name_key, category_key) in [
        ("queue", "filename", "cat"),
        ("history", "name", "category"),
    ] {
        let value = api
            .call(
                mode,
                &[
                    ("search", format!("{TOKEN_MARKER}{token}")),
                    ("start", "0".into()),
                    ("limit", LOOKUP_SLOTS.to_string()),
                ],
                true,
            )
            .await?;
        let list = slots(&value, mode)?;
        if list.len() > LOOKUP_SLOTS {
            return Err(QbitError::InvalidResponse);
        }
        for slot in list {
            let name = text(slot, name_key, 2048)?;
            if !name_has_token(name, token) {
                continue;
            }
            if mode == "history"
                && optional_text(slot, "status", 64)?.eq_ignore_ascii_case("deleted")
            {
                continue;
            }
            if same_category(text(slot, category_key, 256)?, &configured.category) {
                found.insert(nzo_id(slot)?);
            } else {
                foreign = true;
            }
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
) -> std::result::Result<SabnzbdTest, SabTestFailure> {
    settings.validate().map_err(|_| QbitError::InvalidRequest)?;
    let api = Api::new(operation, settings, credentials)?;
    let (version, raw_version) = supported_version(&api).await?;
    let mut warnings = Vec::new();
    if version == Version::Develop {
        warnings.push(SabnzbdTestWarning::DevelopVersion);
    }
    // The full configuration requires the API key (an NZB key is rejected here).
    let config = api.call("get_config", &[], true).await?;
    let config = config.get("config").ok_or(QbitError::InvalidResponse)?;
    let misc = config.get("misc").ok_or(QbitError::InvalidResponse)?;
    let listed = config
        .get("categories")
        .and_then(Value::as_array)
        .ok_or(QbitError::InvalidResponse)?;
    let mut domains = Vec::new();
    for domain in [MediaDomain::Tv, MediaDomain::Movies] {
        let Ok(configured) = scope(settings, domain) else {
            continue;
        };
        let Some(category) = listed.iter().find(|c| {
            c.get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| same_category(name, &configured.category))
        }) else {
            return Err(problem(SabnzbdProblem::CategoryMissing));
        };
        if optional_text(category, "dir", 4096)?.ends_with('*') {
            return Err(problem(SabnzbdProblem::JobFoldersDisabled));
        }
        if sorting_applies(misc, config, &configured.category)? {
            return Err(problem(SabnzbdProblem::SortingEnabled));
        }
        // Authentication must also authorize scoped job reads.
        let queue = api
            .call(
                "queue",
                &[
                    ("cat", configured.category.clone()),
                    ("start", "0".into()),
                    ("limit", "1".into()),
                ],
                true,
            )
            .await?;
        for slot in slots(&queue, "queue")? {
            if !same_category(text(slot, "cat", 256)?, &configured.category) {
                return Err(QbitError::ScopeConflict.into());
            }
        }
        domains.push(domain);
    }
    if removes_completed_downloads(misc)? {
        warnings.push(SabnzbdTestWarning::RemovesCompletedDownloads);
    }
    Ok(SabnzbdTest {
        sabnzbd_version: api.redact(raw_version),
        domains,
        warnings,
    })
}
fn flag(value: &Value, key: &str) -> Result<bool> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(Value::Number(n)) => Ok(n.as_i64().ok_or(QbitError::InvalidResponse)? != 0),
        Some(Value::String(s)) => Ok(matches!(s.trim(), "1" | "true" | "True")),
        _ => Err(QbitError::InvalidResponse),
    }
}
/// Empty category lists apply sorting to every category, as observed in both references.
fn listed_category(value: Option<&Value>, category: &str) -> Result<bool> {
    let names: Vec<&str> = match value {
        None | Some(Value::Null) => return Ok(true),
        Some(Value::Array(list)) => list
            .iter()
            .map(|v| v.as_str().ok_or(QbitError::InvalidResponse))
            .collect::<Result<_>>()?,
        Some(Value::String(s)) => s
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect(),
        _ => return Err(QbitError::InvalidResponse),
    };
    Ok(names.is_empty() || names.iter().any(|name| same_category(name, category)))
}
fn sorting_applies(misc: &Value, config: &Value, category: &str) -> Result<bool> {
    for (enabled, categories) in [
        ("enable_tv_sorting", "tv_categories"),
        ("enable_movie_sorting", "movie_categories"),
        ("enable_date_sorting", "date_categories"),
    ] {
        if flag(misc, enabled)? && listed_category(misc.get(categories), category)? {
            return Ok(true);
        }
    }
    if let Some(sorters) = config.get("sorters") {
        let sorters = sorters.as_array().ok_or(QbitError::InvalidResponse)?;
        for sorter in sorters {
            if flag(sorter, "is_active")? && listed_category(sorter.get("sort_cats"), category)? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}
/// History retention that can remove completed jobs within two weeks.
fn removes_completed_downloads(misc: &Value) -> Result<bool> {
    const MINIMUM_RETENTION_DAYS: i64 = 14;
    let number = || -> Result<i64> {
        match misc.get("history_retention_number") {
            Some(Value::Number(n)) => n.as_i64().ok_or(QbitError::InvalidResponse),
            Some(Value::String(s)) => s.trim().parse().map_err(|_| QbitError::InvalidResponse),
            _ => Err(QbitError::InvalidResponse),
        }
    };
    if let Some(option) = misc.get("history_retention_option").and_then(Value::as_str) {
        return Ok(match option {
            "all" => false,
            "number-archive" | "number-delete" | "all-archive" | "all-delete" => true,
            "days-archive" | "days-delete" => number()? < MINIMUM_RETENTION_DAYS,
            _ => return Err(QbitError::InvalidResponse),
        });
    }
    let retention = optional_text(misc, "history_retention", 64)?.trim();
    if retention.is_empty() || retention == "0" {
        return Ok(false);
    }
    if let Some(days) = retention.strip_suffix('d') {
        return Ok(days
            .parse::<i64>()
            .map_err(|_| QbitError::InvalidResponse)?
            < MINIMUM_RETENTION_DAYS);
    }
    Ok(true)
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
    let api = Api::new(operation, settings, credentials)?;
    Ok(match find_token(&api, configured, &token).await? {
        Some(nzo) => Reconciliation::Observed {
            hash: remote_id(&nzo),
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
/// Never retried: once `addurl` is dispatched any failure is `MutationUnknown`-class and the
/// caller must reconcile by marker instead of submitting again.
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
    let api = Api::new(operation, settings, credentials)?;
    supported_version(&api).await?;
    // An unknown category would silently land in Default and break domain isolation.
    if !categories(&api)
        .await?
        .iter()
        .any(|c| same_category(c, &configured.category))
    {
        return Err(QbitError::UnsupportedFeature);
    }
    if let Some(nzo) = find_token(&api, configured, &token).await? {
        return Ok(AddOutcome::AlreadyPresent {
            hash: remote_id(&nzo),
        });
    }
    let priority = if prepared.recent {
        configured.recent_priority
    } else {
        configured.older_priority
    };
    operation.ensure_active()?;
    *add_started = true;
    let response = api
        .call(
            "addurl",
            &[
                ("name", prepared.url),
                ("nzbname", prepared.job_name),
                ("cat", configured.category.clone()),
                ("priority", priority.wire().to_string()),
            ],
            true,
        )
        .await
        .map_err(|error| match error {
            QbitError::Http(HttpError::Authentication) => error,
            _ => QbitError::MutationUnknown,
        })?;
    let ids = response
        .get("nzo_ids")
        .and_then(Value::as_array)
        .ok_or(QbitError::MutationUnknown)?;
    let [id] = ids.as_slice() else {
        return Err(QbitError::MutationUnknown);
    };
    let nzo = id
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 128 && s.bytes().all(|b| b.is_ascii_graphic()))
        .ok_or(QbitError::MutationUnknown)?
        .to_owned();
    // Postcondition: the returned job exists in this domain's category.
    for (mode, category_key) in [("queue", "cat"), ("history", "category")] {
        let value = api
            .call(
                mode,
                &[
                    ("nzo_ids", nzo.clone()),
                    ("start", "0".into()),
                    ("limit", "2".into()),
                ],
                true,
            )
            .await
            .map_err(|_| QbitError::MutationUnknown)?;
        let list = slots(&value, mode).map_err(|_| QbitError::MutationUnknown)?;
        for slot in list {
            if nzo_id(slot).ok().as_deref() != Some(nzo.as_str()) {
                continue;
            }
            return if text(slot, category_key, 256)
                .is_ok_and(|c| same_category(c, &configured.category))
            {
                Ok(AddOutcome::Observed {
                    hash: remote_id(&nzo),
                })
            } else {
                Err(QbitError::MutationUnknown)
            };
        }
    }
    Err(QbitError::MutationUnknown)
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
    let api = Api::new(operation, settings, credentials)?;
    let (active, finished) = observe(&api, configured, domain, &[]).await?;
    let mut matches = Vec::new();
    for (_, _, item) in active {
        if item.hash == remote {
            matches.push(UsenetJob {
                item,
                storage: None,
            });
        }
    }
    for entry in finished {
        if entry.item.hash != remote {
            continue;
        }
        let storage = if entry.item.completed {
            Some(output_path(&entry.storage, &entry.name)?)
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
/// Completed output: the final destination, widened to an enclosing folder named after the job
/// (sorting or nested unpacking can report a file or subfolder beneath the job folder).
pub(super) fn output_path(storage: &str, name: &str) -> Result<String> {
    validate_remote_path(storage)?;
    let rooted = storage.starts_with('/')
        || storage.starts_with("\\\\")
        || (storage.len() >= 3 && storage.as_bytes()[1] == b':');
    if !rooted {
        return Err(QbitError::InvalidResponse);
    }
    let separator = if storage.contains('\\') && !storage.starts_with('/') {
        '\\'
    } else {
        '/'
    };
    let trimmed = storage.trim_end_matches(['/', '\\']);
    let parts: Vec<&str> = trimmed.split(separator).collect();
    let mut chosen = parts.len();
    for (index, part) in parts.iter().enumerate().skip(1) {
        if *part == name {
            chosen = index + 1;
            break;
        }
    }
    let path = parts[..chosen].join(&separator.to_string());
    validate_remote_path(&path)?;
    Ok(path)
}

/// Private output-root facts for health: the category folder beneath the complete directory.
pub(crate) async fn client_status_snapshot(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
    domain: MediaDomain,
) -> Result<ClientStatusSnapshot> {
    let configured = scope(settings, domain)?;
    let locality = endpoint_locality(settings.endpoint())?;
    let api = Api::new(operation, settings, credentials)?;
    let config = api.call("get_config", &[], true).await?;
    let config = config.get("config").ok_or(QbitError::InvalidResponse)?;
    let misc = config.get("misc").ok_or(QbitError::InvalidResponse)?;
    let removes = removes_completed_downloads(misc)?;
    let complete = optional_text(misc, "complete_dir", 4096)?.to_owned();
    if complete.is_empty() {
        return Ok(ClientStatusSnapshot::OutputMissing(locality));
    }
    let rooted = |p: &str| {
        p.starts_with('/') || p.starts_with("\\\\") || (p.len() >= 3 && p.as_bytes()[1] == b':')
    };
    let complete = if rooted(&complete) {
        complete
    } else {
        // A relative complete folder is resolved by the client; ask it for the absolute value.
        let status = api
            .call("fullstatus", &[("skip_dashboard", "1".into())], true)
            .await?;
        let absolute = status
            .get("status")
            .map(|s| optional_text(s, "completedir", 4096))
            .transpose()?
            .unwrap_or("")
            .to_owned();
        if absolute.is_empty() {
            return Ok(ClientStatusSnapshot::OutputMissing(locality));
        }
        absolute
    };
    let listed = config
        .get("categories")
        .and_then(Value::as_array)
        .ok_or(QbitError::InvalidResponse)?;
    let Some(category) = listed.iter().find(|c| {
        c.get("name")
            .and_then(Value::as_str)
            .is_some_and(|n| same_category(n, &configured.category))
    }) else {
        return Ok(ClientStatusSnapshot::OutputMissing(locality));
    };
    let dir = optional_text(category, "dir", 4096)?
        .trim_end_matches('*')
        .to_owned();
    let Ok((remote_root, root_source)) = remote_root(&complete, Some(&dir)) else {
        return Ok(ClientStatusSnapshot::OutputInvalid(locality));
    };
    Ok(ClientStatusSnapshot::Status(ClientStatus {
        domain,
        remote_root,
        root_source: if dir.trim().is_empty() {
            RootSource::Default
        } else {
            root_source
        },
        locality,
        removes_completed_downloads: removes,
    }))
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
        assert_ne!(
            a,
            submission_token(Uuid::max(), "f", &MediaTarget::Episode(1), "https://i/1")
        );
        assert!(hex_token(&a));
        let name = job_name("Show/S01E01{{pw}}", &a);
        assert!(name.starts_with("Show_S01E01__pw__.hrrdarr-"));
        assert!(name_has_token(&name, &a));
        assert_eq!(job_name("...", &a), format!("release.hrrdarr-{a}"));
        assert!(job_name(&"x".repeat(1000), &a).len() <= MAX_JOB_TITLE_BYTES + 9 + TOKEN_HEX);
    }
    #[test]
    fn times_versions_and_retention_are_bounded() {
        assert_eq!(time_left("0:16:44"), Some(1004));
        assert_eq!(time_left("1:02:00:01"), Some(93601));
        assert_eq!(time_left("0:61:00"), None);
        assert_eq!(time_left("9999:00:00"), None);
        assert_eq!(time_left("soon"), None);
        let v = |s: &str| parse_version(&serde_json::json!({ "version": s })).map(|v| v.0);
        assert_eq!(v("4.3.2"), Ok(Version::Release(4, 3)));
        assert_eq!(v("4.4.0RC1"), Ok(Version::Release(4, 4)));
        assert_eq!(v("develop"), Ok(Version::Develop));
        assert!(v("four").is_err());
        let r = |m: Value| removes_completed_downloads(&m);
        assert_eq!(
            r(serde_json::json!({"history_retention_option":"all"})),
            Ok(false)
        );
        assert_eq!(
            r(
                serde_json::json!({"history_retention_option":"days-delete","history_retention_number":13})
            ),
            Ok(true)
        );
        assert_eq!(
            r(
                serde_json::json!({"history_retention_option":"days-archive","history_retention_number":"14"})
            ),
            Ok(false)
        );
        assert_eq!(r(serde_json::json!({"history_retention":"30d"})), Ok(false));
        assert_eq!(r(serde_json::json!({"history_retention":"7d"})), Ok(true));
        assert_eq!(r(serde_json::json!({"history_retention":""})), Ok(false));
    }
    #[test]
    fn output_paths_widen_to_job_folder_and_reject_relative() {
        assert_eq!(
            output_path("/c/tv/Job.hrrdarr-x", "Job.hrrdarr-x").unwrap(),
            "/c/tv/Job.hrrdarr-x"
        );
        assert_eq!(
            output_path("/c/tv/Job/inner/file.mkv", "Job").unwrap(),
            "/c/tv/Job"
        );
        assert_eq!(output_path("C:\\c\\Job\\sub", "Job").unwrap(), "C:\\c\\Job");
        assert!(output_path("relative/Job", "Job").is_err());
        assert!(output_path("/c/../Job", "Job").is_err());
    }
}
