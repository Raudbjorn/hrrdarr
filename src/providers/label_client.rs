//! Shared engine for torrent clients that isolate TV from movies by a per-domain label
//! (category) and identify torrents by their info hash: Deluge, rTorrent, uTorrent and similar.
//!
//! Each protocol implements only [`Wire`]: connect/authenticate and version-gate, list and look up
//! torrents in a normalized shape, add, label and control. Ownership, status mapping, the
//! hash-reconciled never-double-submit flow, completed-download facts and output-root facts live
//! here once, mirroring the Transmission client. Requirements (settings that exist, label as
//! category, recent/older priority, add paused, completion detection) were inventoried from the
//! pinned Sonarr/Radarr references; nothing here translates their code.
//!
//! No scheduler, automatic mutation retry or local filesystem access happens here.
use super::{
    ClientRetryKind, Credentials, ProviderSettings, TorrentLabelScope, TorrentQueuePriority,
    download::DownloadDetails,
    http::{HttpError, HttpOperation},
    qbittorrent::{
        self, AddOutcome, AddSource, ClientStatus, ClientStatusSnapshot, Control,
        DownloadDiagnostic, DownloadItem, DownloadPage, DownloadQuery, DownloadStatus, QbitError,
        QbitOptions, Reconciliation, RootSource, SubmissionError, SubmissionIdentity, TorrentFile,
        digest_hex, endpoint_locality, target_domain, validate_remote_path,
    },
};
use crate::{api::MediaDomain, db::MediaTarget};
use serde::Serialize;
use std::{collections::BTreeSet, future::Future};

pub mod deluge;
pub mod rtorrent;

pub type LabelError = QbitError;
type Result<T> = std::result::Result<T, LabelError>;

pub(crate) const MAX_LISTED_TORRENTS: usize = 8192;
pub(crate) const MAX_FILES: usize = 4096;
const MAX_DOMAIN_ITEMS: usize = 1000;
const MAX_REFRESH_BYTES: usize = 1024 * 1024;
const MAX_ETA_SECONDS: u64 = 365 * 24 * 60 * 60;
const MAX_RATIO: f64 = 1_000_000.0;

/// Normalized torrent state; each wire maps its client's own state vocabulary onto this.
#[allow(dead_code)] // variants a given wire does not produce are used by other clients
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RawState {
    /// Metadata of a magnet has not been fetched yet.
    Metadata,
    Checking,
    Queued,
    Downloading,
    Stalled,
    Seeding,
    Paused,
    Error,
    Unknown,
}
/// One torrent as read from a client. `label` is empty when unlabelled.
#[derive(Clone, Debug)]
pub(crate) struct RawTorrent {
    /// Lowercase 40-hex v1 info hash.
    pub hash: String,
    pub name: String,
    pub label: String,
    pub save_path: String,
    pub size_bytes: u64,
    pub downloaded_bytes: u64,
    pub state: RawState,
    pub download_rate: u64,
    pub upload_rate: u64,
    pub eta_seconds: Option<u64>,
    pub ratio: f64,
    pub seeding_seconds: Option<u64>,
}
pub(crate) struct RawFile {
    /// Relative path inside the torrent's save path.
    pub path: String,
    pub size_bytes: u64,
    pub downloaded_bytes: u64,
    pub wanted: bool,
}
pub(crate) struct WireInfo {
    pub version: String,
    /// The client's default download directory; empty when the client does not report one.
    pub default_dir: String,
    /// Whether the client can hold labels at all (e.g. Deluge's Label plugin is enabled).
    pub labels_supported: bool,
}
pub(crate) struct AddRequest<'a> {
    pub source: &'a AddSource,
    pub hash: &'a str,
    pub label: &'a str,
    pub paused: bool,
    /// Move to the top of the queue after adding.
    pub first: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AddReply {
    Added,
    /// The client already had the torrent.
    Duplicate,
}

/// Protocol side of a label client. Futures are `Send` so handlers can run them on a runtime.
pub(crate) trait Wire {
    /// Replaces configured secrets in a string that may be projected.
    fn redact(&self, value: String) -> String;
    /// Authenticates, enforces the supported version floor and reads static facts.
    fn connect(&mut self) -> impl Future<Output = Result<WireInfo>> + Send;
    /// Torrents carrying `label` (case-insensitive) when given, otherwise every torrent. A wire
    /// may over-return; the engine filters again.
    fn list(&mut self, label: Option<&str>)
    -> impl Future<Output = Result<Vec<RawTorrent>>> + Send;
    fn get(&mut self, hash: &str) -> impl Future<Output = Result<Option<RawTorrent>>> + Send;
    fn files(&mut self, hash: &str) -> impl Future<Output = Result<Vec<RawFile>>> + Send;
    /// Creates `label` if the client needs it declared first.
    fn ensure_label(&mut self, label: &str) -> impl Future<Output = Result<()>> + Send;
    /// Adds the torrent, then applies label, paused state and queue position.
    fn add(&mut self, request: &AddRequest<'_>) -> impl Future<Output = Result<AddReply>> + Send;
    fn control(&mut self, hash: &str, action: &Control) -> impl Future<Output = Result<()>> + Send;
    /// Labels and places a torrent that is already in the client without a label.
    fn adopt(
        &mut self,
        hash: &str,
        label: &str,
        first: bool,
    ) -> impl Future<Output = Result<()>> + Send;
}

#[derive(Serialize, ts_rs::TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LabelTestWarning {
    /// A configured label is not declared in the client yet; the first grab creates it.
    LabelNotCreated,
}
#[derive(Serialize, ts_rs::TS, Debug)]
pub struct LabelClientTest {
    pub client_version: String,
    pub domains: Vec<MediaDomain>,
    pub warnings: Vec<LabelTestWarning>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LabelProblem {
    /// The client cannot hold labels (for example a disabled plugin), so domains cannot be isolated.
    LabelsUnavailable,
}
#[derive(Debug)]
pub struct LabelTestFailure {
    pub error: LabelError,
    pub problem: Option<LabelProblem>,
}
impl From<LabelError> for LabelTestFailure {
    fn from(error: LabelError) -> Self {
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
    #[allow(dead_code)]
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
pub struct LabelDetails {
    pub item: DownloadItem,
    pub save_path: String,
    pub files: Vec<TorrentFile>,
}
impl From<LabelDetails> for DownloadDetails {
    fn from(value: LabelDetails) -> Self {
        Self {
            item: value.item,
            save_path: value.save_path,
            files: value.files,
        }
    }
}

fn scope(settings: &ProviderSettings, domain: MediaDomain) -> Result<&TorrentLabelScope> {
    settings.validate().map_err(|_| QbitError::InvalidRequest)?;
    settings.label_scope(domain).ok_or(QbitError::ScopeConflict)
}
/// Eligibility tags do not change remote submission behaviour; exclude them like the others.
pub(crate) fn settings_fingerprint(settings: &ProviderSettings) -> Result<String> {
    let stripped = settings
        .without_label_tags()
        .ok_or(QbitError::ScopeConflict)?;
    let bytes = serde_json::to_vec(&stripped).map_err(|_| QbitError::InvalidRequest)?;
    Ok(digest_hex(&ring::digest::SHA256, &bytes))
}
fn same_label(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}
fn rooted(path: &str) -> bool {
    path.starts_with('/')
        || path.starts_with("\\\\")
        || (path.len() >= 3
            && path.as_bytes()[0].is_ascii_alphabetic()
            && path.as_bytes()[1] == b':'
            && matches!(path.as_bytes()[2], b'/' | b'\\'))
}
pub(crate) fn valid_hash(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Maps one torrent into the shared download model. Names can carry tracker URLs with passkeys
/// in some clients, so they pass through the wire's redaction.
fn item<W: Wire>(wire: &W, torrent: &RawTorrent, domain: MediaDomain) -> Result<DownloadItem> {
    let size = torrent.size_bytes;
    let left = size.saturating_sub(torrent.downloaded_bytes);
    let data_complete = size > 0 && left == 0;
    let (status, diagnostic) = match torrent.state {
        RawState::Metadata => (DownloadStatus::Queued, Some(DownloadDiagnostic::Metadata)),
        RawState::Error => (DownloadStatus::Warning, Some(DownloadDiagnostic::Error)),
        RawState::Checking | RawState::Queued => (DownloadStatus::Queued, None),
        RawState::Stalled if !data_complete => {
            (DownloadStatus::Warning, Some(DownloadDiagnostic::Stalled))
        }
        RawState::Paused if data_complete => (DownloadStatus::Completed, None),
        RawState::Seeding | RawState::Stalled if data_complete => (DownloadStatus::Completed, None),
        RawState::Paused => (DownloadStatus::Paused, None),
        RawState::Downloading | RawState::Seeding | RawState::Stalled => {
            (DownloadStatus::Downloading, None)
        }
        RawState::Unknown => (
            DownloadStatus::Unknown,
            Some(DownloadDiagnostic::UnknownState),
        ),
    };
    let completed = status == DownloadStatus::Completed;
    let progress = if size == 0 {
        0.0
    } else {
        (torrent.downloaded_bytes.min(size)) as f64 / size as f64
    };
    Ok(DownloadItem {
        hash: torrent.hash.clone(),
        domain,
        name: wire.redact(torrent.name.clone()),
        category: torrent.label.clone(),
        status,
        diagnostic,
        progress,
        size_bytes: size,
        remaining_bytes: Some(left),
        download_bytes_per_second: torrent.download_rate,
        upload_bytes_per_second: torrent.upload_rate,
        eta_seconds: if completed {
            Some(0)
        } else {
            torrent.eta_seconds.filter(|n| *n <= MAX_ETA_SECONDS)
        },
        ratio: if torrent.ratio.is_finite() && torrent.ratio > 0.0 {
            torrent.ratio.min(MAX_RATIO)
        } else {
            0.0
        },
        seeding_seconds: torrent.seeding_seconds,
        completed,
    })
}
fn checked(torrents: Vec<RawTorrent>) -> Result<Vec<RawTorrent>> {
    if torrents.len() > MAX_LISTED_TORRENTS {
        return Err(QbitError::InvalidResponse);
    }
    let mut seen = BTreeSet::new();
    for torrent in &torrents {
        if !valid_hash(&torrent.hash) || !seen.insert(torrent.hash.clone()) {
            return Err(QbitError::InvalidResponse);
        }
    }
    Ok(torrents)
}

async fn observe<W: Wire>(
    wire: &mut W,
    settings: &ProviderSettings,
    domain: MediaDomain,
) -> Result<Vec<DownloadItem>> {
    let configured = scope(settings, domain)?;
    wire.connect().await?;
    let mut items = Vec::new();
    for torrent in checked(wire.list(Some(&configured.category)).await?)? {
        if !same_label(&torrent.label, &configured.category) {
            continue;
        }
        if items.len() == MAX_DOMAIN_ITEMS {
            return Err(HttpError::ResponseTooLarge.into());
        }
        items.push(item(wire, &torrent, domain)?);
    }
    Ok(items)
}
pub(crate) async fn refresh_pages_with<W: Wire>(
    operation: &HttpOperation<'_>,
    wire: &mut W,
    settings: &ProviderSettings,
    domain: MediaDomain,
) -> Result<Vec<DownloadItem>> {
    let items = observe(wire, settings, domain).await?;
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
pub(crate) async fn query_with<W: Wire>(
    operation: &HttpOperation<'_>,
    wire: &mut W,
    settings: &ProviderSettings,
    request: &DownloadQuery,
) -> Result<DownloadPage> {
    if !(1..=500).contains(&request.limit) || request.offset > i32::MAX as u32 {
        return Err(QbitError::InvalidRequest);
    }
    scope(settings, request.domain)?;
    if request.imported {
        return Err(QbitError::ScopeConflict);
    }
    let mut all = refresh_pages_with(operation, wire, settings, request.domain).await?;
    all.sort_by(|a, b| a.hash.cmp(&b.hash));
    let start = (request.offset as usize).min(all.len());
    let end = start.saturating_add(request.limit as usize).min(all.len());
    Ok(DownloadPage {
        domain: request.domain,
        offset: request.offset,
        limit: request.limit,
        next_offset: (end < all.len()).then_some(end as u32),
        items: all.into_iter().skip(start).take(end - start).collect(),
    })
}

/// Looks up one info hash. Present in this domain → `Some`; present under another label (or
/// unlabelled) is a visible conflict, never absence that could authorize another submission.
async fn find<W: Wire>(
    wire: &mut W,
    configured: &TorrentLabelScope,
    hash: &str,
) -> Result<Option<RawTorrent>> {
    match wire.get(hash).await? {
        None => Ok(None),
        Some(torrent) => {
            if torrent.hash != hash {
                return Err(QbitError::InvalidResponse);
            }
            if !same_label(&torrent.label, &configured.category) {
                return Err(QbitError::ScopeConflict);
            }
            Ok(Some(torrent))
        }
    }
}

pub(crate) async fn test_with<W: Wire>(
    wire: &mut W,
    settings: &ProviderSettings,
) -> std::result::Result<LabelClientTest, LabelTestFailure> {
    settings.validate().map_err(|_| QbitError::InvalidRequest)?;
    let info = wire.connect().await?;
    let mut domains = Vec::new();
    let mut warnings = Vec::new();
    for domain in [MediaDomain::Tv, MediaDomain::Movies] {
        let Some(configured) = settings.label_scope(domain) else {
            continue;
        };
        if !info.labels_supported {
            return Err(LabelTestFailure {
                error: QbitError::UnsupportedFeature,
                problem: Some(LabelProblem::LabelsUnavailable),
            });
        }
        // Authentication must also authorize torrent reads within this domain's label.
        let torrents = checked(wire.list(Some(&configured.category)).await?)?;
        if torrents
            .iter()
            .any(|t| same_label(&t.label, &configured.category) && t.name.is_empty())
        {
            return Err(QbitError::InvalidResponse.into());
        }
        domains.push(domain);
    }
    // Static label creation is deferred to the first grab; Test never mutates the client.
    let _ = &mut warnings;
    Ok(LabelClientTest {
        client_version: wire.redact(info.version),
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
    let (source, identities) = qbittorrent::resolve_source(operation, source).await?;
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
    if identity.version() != 1
        || !valid_hash(hash)
        || identity.payload_sha256().len() != 64
        || !identity
            .payload_sha256()
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(QbitError::InvalidRequest);
    }
    if identity.settings_fingerprint() != settings_fingerprint(settings)? {
        return Err(QbitError::ScopeConflict);
    }
    Ok((domain, hash.clone()))
}
pub(crate) async fn reconcile_with<W: Wire>(
    wire: &mut W,
    settings: &ProviderSettings,
    identity: &SubmissionIdentity,
) -> Result<Reconciliation> {
    let (domain, hash) = validate_identity(identity, settings)?;
    let configured = scope(settings, domain)?;
    wire.connect().await?;
    Ok(match find(wire, configured, &hash).await? {
        Some(_) => Reconciliation::Observed { hash },
        None => Reconciliation::NotObserved,
    })
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
/// Never retried: once the add is dispatched any failure is `MutationUnknown`-class and the
/// caller must reconcile by info hash instead of submitting again.
pub(crate) async fn submit_with<W: Wire>(
    operation: &HttpOperation<'_>,
    wire: &mut W,
    settings: &ProviderSettings,
    prepared: PreparedTorrent,
) -> std::result::Result<AddOutcome, SubmissionError> {
    let mut add_started = false;
    submit_inner(operation, wire, settings, prepared, &mut add_started)
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
async fn submit_inner<W: Wire>(
    operation: &HttpOperation<'_>,
    wire: &mut W,
    settings: &ProviderSettings,
    prepared: PreparedTorrent,
    add_started: &mut bool,
) -> Result<AddOutcome> {
    let (domain, hash) = validate_identity(&prepared.identity, settings)?;
    let PreparedTorrent {
        identity,
        source,
        recent,
        ..
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
    let info = wire.connect().await?;
    if !info.labels_supported {
        return Err(QbitError::UnsupportedFeature);
    }
    if find(wire, configured, &hash).await?.is_some() {
        return Ok(AddOutcome::AlreadyPresent { hash });
    }
    // Declaring a label is idempotent and precedes the add, so a failure here is a definite
    // non-submission.
    wire.ensure_label(&configured.category).await?;
    let first = if recent {
        configured.recent_priority
    } else {
        configured.older_priority
    } == TorrentQueuePriority::First;
    operation.ensure_active()?;
    // Once add begins, even rejection-shaped failures cannot authorize another client.
    *add_started = true;
    let reply = wire
        .add(&AddRequest {
            source: &source,
            hash: &hash,
            label: &configured.category,
            paused: configured.add_paused,
            first,
        })
        .await;
    let reply = match reply {
        Ok(reply) => reply,
        Err(error @ QbitError::Http(HttpError::Authentication)) => return Err(error),
        Err(QbitError::Rejected) => {
            // The client may have refused because someone else already holds this hash under
            // another label: that is a visible conflict, anything else must be reconciled.
            return Err(match find(wire, configured, &hash).await {
                Err(QbitError::ScopeConflict) => QbitError::ScopeConflict,
                _ => QbitError::MutationUnknown,
            });
        }
        Err(_) => {
            // The add may have landed with its reply (or the labelling that follows it) lost.
            // An unlabelled torrent with exactly this hash right after our own attempt is ours:
            // finish labelling it so a later reconcile observes it in this domain.
            if let Ok(Some(torrent)) = wire.get(&hash).await {
                if torrent.label.is_empty() {
                    let _ = wire.adopt(&hash, &configured.category, first).await;
                }
            }
            return Err(QbitError::MutationUnknown);
        }
    };
    let after = match find(wire, configured, &hash).await {
        Ok(Some(torrent)) => torrent,
        Ok(None) => return Err(QbitError::MutationUnknown),
        Err(QbitError::ScopeConflict) if reply == AddReply::Duplicate => {
            return Err(QbitError::ScopeConflict);
        }
        Err(_) => return Err(QbitError::MutationUnknown),
    };
    if reply == AddReply::Duplicate {
        // Someone else added it between the lookup and add: never claim a torrent this call did
        // not create, but it is ours to track since it sits in this domain.
        return Ok(AddOutcome::AlreadyPresent { hash });
    }
    if configured.add_paused != (after.state == RawState::Paused) {
        return Err(QbitError::MutationUnknown);
    }
    Ok(AddOutcome::Observed { hash })
}

/// Completed-download facts for one torrent of this domain. Paths are remote; file names are
/// relative to `save_path`.
pub(crate) async fn details_with<W: Wire>(
    wire: &mut W,
    settings: &ProviderSettings,
    target: &MediaTarget,
    expected: &str,
) -> Result<LabelDetails> {
    let domain = target_domain(target)?;
    let configured = scope(settings, domain)?;
    let expected = qbittorrent::hash(expected)?;
    if !valid_hash(&expected) {
        return Err(QbitError::InvalidRequest);
    }
    wire.connect().await?;
    let torrent = find(wire, configured, &expected)
        .await?
        .ok_or(QbitError::NotFound)?;
    let item = item(wire, &torrent, domain)?;
    validate_remote_path(&torrent.save_path)?;
    if !rooted(&torrent.save_path) {
        return Err(QbitError::InvalidResponse);
    }
    let files = wire.files(&expected).await?;
    if files.len() > MAX_FILES {
        return Err(QbitError::InvalidResponse);
    }
    let mut result = Vec::with_capacity(files.len());
    for (index, file) in files.into_iter().enumerate() {
        let name = file.path.as_str();
        // Remote names remain relative; a later path mapper decides filesystem ownership.
        if name.is_empty()
            || name.len() > 4096
            || name.chars().any(char::is_control)
            || name.starts_with(['/', '\\'])
            || name.contains(':')
            || name
                .split(['/', '\\'])
                .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err(QbitError::InvalidResponse);
        }
        let done = file.downloaded_bytes.min(file.size_bytes);
        result.push(TorrentFile {
            index: index as u32,
            name: wire.redact(file.path),
            size_bytes: file.size_bytes,
            progress: if file.size_bytes == 0 {
                1.0
            } else {
                done as f64 / file.size_bytes as f64
            },
            priority: u8::from(file.wanted),
        });
    }
    Ok(LabelDetails {
        item,
        save_path: torrent.save_path,
        files: result,
    })
}

/// Private output-root facts for health: the client's default download directory. Label clients
/// keep finished torrents until removed, so retention is always safe.
pub(crate) async fn client_status_snapshot_with<W: Wire>(
    wire: &mut W,
    settings: &ProviderSettings,
    domain: MediaDomain,
) -> Result<ClientStatusSnapshot> {
    scope(settings, domain)?;
    let locality = endpoint_locality(settings.endpoint())?;
    let info = wire.connect().await?;
    if info.default_dir.is_empty() {
        return Ok(ClientStatusSnapshot::OutputMissing(locality));
    }
    if validate_remote_path(&info.default_dir).is_err() || !rooted(&info.default_dir) {
        return Ok(ClientStatusSnapshot::OutputInvalid(locality));
    }
    Ok(ClientStatusSnapshot::Status(ClientStatus {
        domain,
        remote_root: info.default_dir.trim_end_matches(['/', '\\']).to_owned(),
        root_source: RootSource::Default,
        locality,
        removes_completed_downloads: false,
    }))
}

/// Explicit queue/state controls. A caller must persist target + identity + intent first.
pub(crate) async fn control_with<W: Wire>(
    wire: &mut W,
    settings: &ProviderSettings,
    target: &MediaTarget,
    expected: &str,
    action: Control,
) -> Result<()> {
    let domain = target_domain(target)?;
    let configured = scope(settings, domain)?;
    let expected = qbittorrent::hash(expected)?;
    if !valid_hash(&expected) {
        return Err(QbitError::InvalidRequest);
    }
    if !matches!(
        action,
        Control::Pause | Control::Resume | Control::Remove { .. } | Control::Priority { .. }
    ) {
        return Err(QbitError::UnsupportedFeature);
    }
    wire.connect().await?;
    find(wire, configured, &expected)
        .await?
        .ok_or(QbitError::NotFound)?;
    wire.control(&expected, &action)
        .await
        .map_err(|error| match error {
            QbitError::UnsupportedFeature | QbitError::InvalidRequest => error,
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
    let after = find(wire, configured, &expected)
        .await
        .map_err(|_| QbitError::MutationUnknown)?;
    let confirmed = match action {
        Control::Remove { .. } => after.is_none(),
        Control::Pause => after.is_some_and(|t| t.state == RawState::Paused),
        Control::Resume => after.is_some_and(|t| t.state != RawState::Paused),
        Control::Priority { .. } => after.is_some(),
        _ => false,
    };
    if confirmed {
        Ok(())
    } else {
        Err(QbitError::MutationUnknown)
    }
}

/// Builds the wire for the configured implementation and runs the body against it. One arm per
/// protocol; the engine functions above never name a concrete client.
macro_rules! with_wire {
    ($operation:expr, $settings:expr, $credentials:expr, |$wire:ident| $body:expr) => {
        match $settings {
            ProviderSettings::Deluge { endpoint, .. } => {
                let mut $wire = $crate::providers::label_client::deluge::DelugeWire::new(
                    $operation,
                    endpoint,
                    $credentials,
                )?;
                $body
            }
            ProviderSettings::Rtorrent { endpoint, .. } => {
                let mut $wire = $crate::providers::label_client::rtorrent::RtorrentWire::new(
                    $operation,
                    endpoint,
                    $credentials,
                )?;
                $body
            }
            _ => Err(QbitError::ScopeConflict),
        }
    };
}

pub async fn refresh_pages(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
    domain: MediaDomain,
) -> Result<Vec<DownloadItem>> {
    with_wire!(operation, settings, credentials, |wire| {
        refresh_pages_with(operation, &mut wire, settings, domain).await
    })
}
pub async fn query(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
    request: &DownloadQuery,
) -> Result<DownloadPage> {
    with_wire!(operation, settings, credentials, |wire| {
        query_with(operation, &mut wire, settings, request).await
    })
}
pub async fn test_connection(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
) -> std::result::Result<LabelClientTest, LabelTestFailure> {
    let result: Result<std::result::Result<LabelClientTest, LabelTestFailure>> =
        with_wire!(operation, settings, credentials, |wire| {
            Ok(test_with(&mut wire, settings).await)
        });
    result?
}
pub(crate) async fn submit_classified(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
    prepared: PreparedTorrent,
) -> std::result::Result<AddOutcome, SubmissionError> {
    let result: Result<std::result::Result<AddOutcome, SubmissionError>> = async {
        with_wire!(operation, settings, credentials, |wire| {
            Ok(submit_with(operation, &mut wire, settings, prepared).await)
        })
    }
    .await;
    result.unwrap_or_else(|error| {
        Err(SubmissionError {
            error,
            definitely_not_submitted: true,
            client_retry_kind: ClientRetryKind::Other,
            client_retry_eligible: !local_submission_failure(error),
        })
    })
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
pub async fn reconcile(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
    identity: &SubmissionIdentity,
) -> Result<Reconciliation> {
    with_wire!(operation, settings, credentials, |wire| {
        reconcile_with(&mut wire, settings, identity).await
    })
}
pub async fn details(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
    target: &MediaTarget,
    expected: &str,
) -> Result<LabelDetails> {
    with_wire!(operation, settings, credentials, |wire| {
        details_with(&mut wire, settings, target, expected).await
    })
}
pub(crate) async fn client_status_snapshot(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
    domain: MediaDomain,
) -> Result<ClientStatusSnapshot> {
    with_wire!(operation, settings, credentials, |wire| {
        client_status_snapshot_with(&mut wire, settings, domain).await
    })
}
pub async fn control(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
    target: &MediaTarget,
    expected: &str,
    action: Control,
) -> Result<()> {
    with_wire!(operation, settings, credentials, |wire| {
        control_with(&mut wire, settings, target, expected, action).await
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hashes_and_roots_are_strict() {
        assert!(valid_hash(&"a".repeat(40)));
        assert!(!valid_hash(&"A".repeat(40)));
        assert!(!valid_hash("abc"));
        assert!(rooted("/data") && rooted("C:\\x") && rooted("\\\\srv\\share"));
        assert!(!rooted("data") && !rooted("C:"));
    }
}
