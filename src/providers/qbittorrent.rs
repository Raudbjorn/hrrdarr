//! qBittorrent protocol operations. No scheduler, automatic retry, or local filesystem access.
use super::{
    Credentials, DownloadScope, ProviderSettings,
    http::{HttpError, HttpOperation, HttpRequestBody, HttpResponse},
};
use crate::{api::MediaDomain, db::MediaTarget};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QbitError {
    Http(HttpError),
    InvalidRequest,
    InvalidResponse,
    UnsupportedVersion,
    UnsupportedFeature,
    ScopeConflict,
    NotFound,
    Rejected,
    UnsafeRetention,
    MutationUnknown,
}
impl From<HttpError> for QbitError {
    fn from(value: HttpError) -> Self {
        Self::Http(value)
    }
}
type Result<T> = std::result::Result<T, QbitError>;
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Version(u16, u16, u16);
impl Version {
    fn parse(raw: &[u8], legacy: bool) -> Result<Self> {
        let text = std::str::from_utf8(raw)
            .map_err(|_| QbitError::InvalidResponse)?
            .trim();
        if text.len() > 32 {
            return Err(QbitError::InvalidResponse);
        }
        if legacy && !text.contains('.') {
            let api = text
                .parse::<u16>()
                .map_err(|_| QbitError::InvalidResponse)?;
            return Ok(Self(1, api, 0));
        }
        let parts = text
            .split('.')
            .map(str::parse::<u16>)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| QbitError::InvalidResponse)?;
        if !(2..=3).contains(&parts.len()) {
            return Err(QbitError::InvalidResponse);
        }
        Ok(Self(parts[0], parts[1], *parts.get(2).unwrap_or(&0)))
    }
    fn text(self) -> String {
        format!("{}.{}.{}", self.0, self.1, self.2)
    }
}
#[derive(Serialize, ts_rs::TS)]
pub struct ClientTest {
    pub api_version: String,
    pub application_version: String,
    pub domains: Vec<MediaDomain>,
    pub missing_categories: Vec<String>,
    pub queueing_enabled: bool,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct DownloadQuery {
    pub domain: MediaDomain,
    pub offset: u32,
    pub limit: u32,
    pub imported: bool,
}
#[derive(Serialize, ts_rs::TS)]
pub struct DownloadPage {
    pub domain: MediaDomain,
    pub offset: u32,
    pub limit: u32,
    pub next_offset: Option<u32>,
    pub items: Vec<DownloadItem>,
}
#[derive(Serialize, ts_rs::TS, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DownloadStatus {
    Queued,
    Downloading,
    Paused,
    Completed,
    Failed,
    Warning,
    Stalled,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum DownloadDiagnostic {
    Error,
    MissingFiles,
    Stalled,
    Metadata,
    DhtDisabled,
    UnknownState,
}
#[derive(Serialize, ts_rs::TS)]
pub struct DownloadItem {
    pub hash: String,
    pub domain: MediaDomain,
    pub name: String,
    pub category: String,
    pub status: DownloadStatus,
    pub diagnostic: Option<DownloadDiagnostic>,
    pub progress: f64,
    pub size_bytes: u64,
    pub remaining_bytes: Option<u64>,
    pub download_bytes_per_second: u64,
    pub upload_bytes_per_second: u64,
    pub eta_seconds: Option<u64>,
    pub ratio: f64,
    pub seeding_seconds: Option<u64>,
    pub completed: bool,
}
/// Remote paths and remote preferences are private protocol facts, never a filesystem authority.
pub struct TorrentDetails {
    pub item: DownloadItem,
    pub save_path: String,
    pub content_path: Option<String>,
    pub files: Vec<TorrentFile>,
    pub properties: Value,
    pub seed_policy: SeedPolicy,
}
/// Internal facts only: reaching a limit never authorizes removal or an import move.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SeedLimit<T> {
    Unavailable,
    Unknown,
    Inherit,
    Unlimited,
    Limited(T),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeedProvenance {
    Unknown,
    ObservedRaw,
    ObservedEffective,
    Unavailable,
}
fn legacy_seed_profile(version: Version) -> bool {
    version.0 == 1 && (6..=17).contains(&version.1)
}
fn unavailable_fields(value: &Value, fields: &[&str]) -> Result<()> {
    if fields.iter().any(|field| value.get(*field).is_some()) {
        return Err(QbitError::InvalidResponse);
    }
    Ok(())
}
fn unavailable_axis<T>() -> SeedAxis<T> {
    SeedAxis {
        provenance: SeedProvenance::Unavailable,
        raw: SeedLimit::Unavailable,
        global_enabled: None,
        global: SeedLimit::Unavailable,
        effective: SeedLimit::Unavailable,
        reached: Some(false),
    }
}
#[derive(Debug, PartialEq)]
pub struct SeedAxis<T> {
    pub provenance: SeedProvenance,
    pub raw: SeedLimit<T>,
    pub global_enabled: Option<bool>,
    pub global: SeedLimit<T>,
    pub effective: SeedLimit<T>,
    pub reached: Option<bool>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeedAction {
    Stop,
    Remove,
    SuperSeed,
    DeleteFiles,
}
#[derive(Debug, PartialEq)]
pub struct SeedPolicy {
    pub ratio: SeedAxis<f64>,
    pub seeding_minutes: SeedAxis<u64>,
    pub inactive_minutes: SeedAxis<u64>,
    pub elapsed_seeding_seconds: Option<u64>,
    pub last_activity_unix_seconds: Option<u64>,
    pub limit_reached: Option<bool>,
    pub stopped_complete: bool,
    pub ready: Option<bool>,
    pub client_action: Option<SeedAction>,
}
impl SeedPolicy {
    /// Configured action only; this does not establish an enabled removal policy.
    pub fn client_removes_on_limit(&self) -> Option<bool> {
        self.client_action
            .map(|action| matches!(action, SeedAction::Remove | SeedAction::DeleteFiles))
    }
}
fn optional_bool(value: &Value, key: &str) -> Result<Option<bool>> {
    value
        .get(key)
        .map(|v| v.as_bool().ok_or(QbitError::InvalidResponse))
        .transpose()
}
// Both reference clients require completed downloads to remain available for import.
fn removes_completed_downloads(version: Version, preferences: &Value) -> Result<bool> {
    const MINIMUM_RETENTION_MINUTES: u64 = 14 * 24 * 60;
    if legacy_seed_profile(version) {
        unavailable_fields(
            preferences,
            &[
                "max_inactive_seeding_time_enabled",
                "max_inactive_seeding_time",
            ],
        )?;
    }
    let ratio_enabled =
        optional_bool(preferences, "max_ratio_enabled")?.ok_or(QbitError::InvalidResponse)?;
    let (time_enabled, short_time) = if legacy_seed_profile(version) && version < Version(1, 16, 0)
    {
        unavailable_fields(
            preferences,
            &[
                "max_seeding_time_enabled",
                "max_seeding_time",
                "max_inactive_seeding_time_enabled",
                "max_inactive_seeding_time",
            ],
        )?;
        (false, false)
    } else {
        let time_enabled = optional_bool(preferences, "max_seeding_time_enabled")?
            .ok_or(QbitError::InvalidResponse)?;
        let time = minutes_limit(preferences, "max_seeding_time", false)?;
        let short_time = match time {
            SeedLimit::Limited(minutes) => minutes < MINIMUM_RETENTION_MINUTES,
            // The reference retention guard uses the raw signed threshold.
            SeedLimit::Unlimited => true,
            _ if !time_enabled => false,
            _ => return Err(QbitError::InvalidResponse),
        };
        (time_enabled, short_time)
    };
    let removes = match preferences.get("max_ratio_act").and_then(Value::as_u64) {
        Some(0 | 2) => false,
        Some(1 | 3) => true,
        _ => return Err(QbitError::InvalidResponse),
    };
    Ok(removes && (ratio_enabled || (time_enabled && short_time)))
}
fn validate_retention(version: Version, preferences: &Value) -> Result<()> {
    if removes_completed_downloads(version, preferences)? {
        Err(QbitError::UnsafeRetention)
    } else {
        Ok(())
    }
}
fn ratio_limit(value: &Value, key: &str, inherit: bool) -> Result<SeedLimit<f64>> {
    let Some(value) = value.get(key) else {
        return Ok(SeedLimit::Unknown);
    };
    let value = value
        .as_f64()
        .filter(|n| n.is_finite())
        .ok_or(QbitError::InvalidResponse)?;
    match value {
        -2.0 if inherit => Ok(SeedLimit::Inherit),
        -1.0 => Ok(SeedLimit::Unlimited),
        n if (0.0..=1_000_000.0).contains(&n) => Ok(SeedLimit::Limited(n)),
        _ => Err(QbitError::InvalidResponse),
    }
}
fn minutes_limit(value: &Value, key: &str, inherit: bool) -> Result<SeedLimit<u64>> {
    let Some(value) = value.get(key) else {
        return Ok(SeedLimit::Unknown);
    };
    match value.as_i64().ok_or(QbitError::InvalidResponse)? {
        -2 if inherit => Ok(SeedLimit::Inherit),
        -1 => Ok(SeedLimit::Unlimited),
        n if (0..=i32::MAX as i64).contains(&n) => Ok(SeedLimit::Limited(n as u64)),
        _ => Err(QbitError::InvalidResponse),
    }
}
fn seed_axis<T: Copy>(
    raw: SeedLimit<T>,
    global_enabled: Option<bool>,
    global: SeedLimit<T>,
    evaluate: impl FnOnce(T) -> Option<bool>,
) -> SeedAxis<T> {
    let effective = match raw {
        SeedLimit::Inherit => match global_enabled {
            Some(true) => global,
            Some(false) => SeedLimit::Unlimited,
            None => SeedLimit::Unknown,
        },
        value => value,
    };
    let reached = match effective {
        SeedLimit::Limited(n) => evaluate(n),
        SeedLimit::Unlimited => Some(false),
        _ => None,
    };
    SeedAxis {
        provenance: if matches!(raw, SeedLimit::Unknown) {
            SeedProvenance::Unknown
        } else {
            SeedProvenance::ObservedRaw
        },
        raw,
        global_enabled,
        global,
        effective,
        reached,
    }
}
fn seed_seconds(value: &Value, key: &str) -> Result<Option<u64>> {
    match value.get(key) {
        None => Ok(None),
        Some(v) if v.as_i64() == Some(-1) => Ok(None),
        Some(_) => number(value, key).map(Some),
    }
}
fn seed_policy(
    version: Version,
    torrent: &Value,
    properties: &Value,
    preferences: &Value,
    completed: bool,
    now_seconds: u64,
) -> Result<SeedPolicy> {
    if !torrent.is_object() || !properties.is_object() || !preferences.is_object() {
        return Err(QbitError::InvalidResponse);
    }
    let elapsed_seeding_seconds = if torrent.get("seeding_time").is_some() {
        seed_seconds(torrent, "seeding_time")?
    } else {
        seed_seconds(properties, "seeding_time")?
    };
    let last_activity_unix_seconds = seed_seconds(torrent, "last_activity")?;
    let observed_ratio = fraction(torrent, "ratio", 1_000_000.0)?;
    let mut ratio = seed_axis(
        ratio_limit(torrent, "ratio_limit", true)?,
        optional_bool(preferences, "max_ratio_enabled")?,
        ratio_limit(preferences, "max_ratio", false)?,
        |limit| Some(observed_ratio + 0.001 >= limit),
    );
    let mut seeding_minutes = seed_axis(
        minutes_limit(torrent, "seeding_time_limit", true)?,
        optional_bool(preferences, "max_seeding_time_enabled")?,
        minutes_limit(preferences, "max_seeding_time", false)?,
        |minutes| {
            minutes
                .checked_mul(60)
                .and_then(|limit| elapsed_seeding_seconds.map(|elapsed| elapsed >= limit))
        },
    );
    let mut inactive_minutes = seed_axis(
        minutes_limit(torrent, "inactive_seeding_time_limit", true)?,
        optional_bool(preferences, "max_inactive_seeding_time_enabled")?,
        minutes_limit(preferences, "max_inactive_seeding_time", false)?,
        |minutes| {
            let last = last_activity_unix_seconds.filter(|last| *last > 0)?;
            let elapsed = now_seconds.checked_sub(last)?;
            Some(elapsed > minutes.checked_mul(60)?)
        },
    );
    if legacy_seed_profile(version) {
        unavailable_fields(
            torrent,
            &["seeding_time_limit", "inactive_seeding_time_limit"],
        )?;
        unavailable_fields(
            preferences,
            &[
                "max_inactive_seeding_time_enabled",
                "max_inactive_seeding_time",
            ],
        )?;
        let effective = ratio_limit(torrent, "ratio_limit", false)?;
        ratio.raw = SeedLimit::Unknown;
        ratio.effective = effective;
        ratio.provenance = if matches!(effective, SeedLimit::Unknown) {
            SeedProvenance::Unknown
        } else {
            SeedProvenance::ObservedEffective
        };
        ratio.reached = match effective {
            SeedLimit::Limited(limit) => Some(observed_ratio + 0.001 >= limit),
            SeedLimit::Unlimited => Some(false),
            _ => None,
        };
        inactive_minutes = unavailable_axis();
        if version < Version(1, 16, 0) {
            unavailable_fields(
                preferences,
                &["max_seeding_time_enabled", "max_seeding_time"],
            )?;
            seeding_minutes = unavailable_axis();
        }
    }
    let axes = [
        ratio.reached,
        seeding_minutes.reached,
        inactive_minutes.reached,
    ];
    let limit_reached = if axes.contains(&Some(true)) {
        Some(true)
    } else if axes.contains(&None) {
        None
    } else {
        Some(false)
    };
    let stopped_complete = completed
        && matches!(
            text(torrent, "state", 64)?.as_str(),
            "pausedUP" | "stoppedUP"
        );
    let ready = if stopped_complete {
        limit_reached
    } else {
        Some(false)
    };
    let client_action = preferences
        .get("max_ratio_act")
        .map(|value| match value.as_u64() {
            Some(0) => Ok(SeedAction::Stop),
            Some(1) => Ok(SeedAction::Remove),
            Some(2) => Ok(SeedAction::SuperSeed),
            Some(3) => Ok(SeedAction::DeleteFiles),
            _ => Err(QbitError::InvalidResponse),
        })
        .transpose()?;
    Ok(SeedPolicy {
        ratio,
        seeding_minutes,
        inactive_minutes,
        elapsed_seeding_seconds,
        last_activity_unix_seconds,
        limit_reached,
        stopped_complete,
        ready,
        client_action,
    })
}
pub struct TorrentFile {
    pub index: u32,
    pub name: String,
    pub size_bytes: u64,
    pub progress: f64,
    pub priority: u8,
}
fn scope(settings: &ProviderSettings, domain: MediaDomain) -> Result<&DownloadScope> {
    settings.validate().map_err(|_| QbitError::InvalidRequest)?;
    match settings {
        ProviderSettings::Qbittorrent { tv, movies, .. } => match domain {
            MediaDomain::Tv => tv.as_ref(),
            MediaDomain::Movies => movies.as_ref(),
        }
        .ok_or(QbitError::ScopeConflict),
        _ => Err(QbitError::InvalidRequest),
    }
}
fn target_domain(target: &MediaTarget) -> Result<MediaDomain> {
    match target {
        MediaTarget::Episode(id) if *id > 0 => Ok(MediaDomain::Tv),
        MediaTarget::Movie(id) if *id > 0 => Ok(MediaDomain::Movies),
        _ => Err(QbitError::InvalidRequest),
    }
}
fn hash(value: &str) -> Result<String> {
    if ![40, 64].contains(&value.len()) || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(QbitError::InvalidRequest);
    }
    Ok(value.to_ascii_lowercase())
}
fn text(value: &Value, key: &str, max: usize) -> Result<String> {
    let s = value
        .get(key)
        .and_then(Value::as_str)
        .ok_or(QbitError::InvalidResponse)?;
    if s.len() > max || s.chars().any(char::is_control) {
        return Err(QbitError::InvalidResponse);
    }
    Ok(s.to_owned())
}
fn number(value: &Value, key: &str) -> Result<u64> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .filter(|v| *v <= 9_007_199_254_740_991)
        .ok_or(QbitError::InvalidResponse)
}
fn fraction(value: &Value, key: &str, max: f64) -> Result<f64> {
    value
        .get(key)
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite() && *n >= 0.0 && *n <= max)
        .ok_or(QbitError::InvalidResponse)
}
fn observed_running(state: &str) -> bool {
    matches!(
        state,
        "downloading"
            | "forcedDL"
            | "metaDL"
            | "forcedMetaDL"
            | "queuedDL"
            | "stalledDL"
            | "uploading"
            | "forcedUP"
            | "queuedUP"
            | "stalledUP"
    )
}
fn state(value: &str) -> DownloadStatus {
    match value {
        "pausedDL" | "stoppedDL" => DownloadStatus::Paused,
        "queuedDL" | "checkingDL" | "checkingUP" | "checkingResumeData" | "metaDL"
        | "forcedMetaDL" => DownloadStatus::Queued,
        "uploading" | "stalledUP" | "queuedUP" | "forcedUP" | "pausedUP" | "stoppedUP" => {
            DownloadStatus::Completed
        }
        "stalledDL" | "error" | "missingFiles" => DownloadStatus::Warning,
        "moving" | "downloading" | "forcedDL" => DownloadStatus::Downloading,
        _ => DownloadStatus::Unknown,
    }
}
fn apply_metadata_preferences(item: &mut DownloadItem, preferences: &Value) -> Result<()> {
    if item.diagnostic != Some(DownloadDiagnostic::Metadata) {
        return Ok(());
    }
    if !preferences.is_object() {
        return Err(QbitError::InvalidResponse);
    }
    let enabled = preferences
        .get("dht")
        .map(|value| value.as_bool().ok_or(QbitError::InvalidResponse))
        .transpose()?;
    if enabled == Some(false) {
        item.status = DownloadStatus::Warning;
        item.diagnostic = Some(DownloadDiagnostic::DhtDisabled);
    }
    Ok(())
}
struct Session<'a, 'b> {
    operation: &'a HttpOperation<'b>,
    settings: &'a ProviderSettings,
    credentials: Option<&'a Credentials>,
    headers: Vec<(String, String)>,
    observed_sids: Vec<String>,
    version: Version,
    legacy: bool,
}
impl<'a, 'b> Session<'a, 'b> {
    async fn connect(
        operation: &'a HttpOperation<'b>,
        settings: &'a ProviderSettings,
        credentials: Option<&'a Credentials>,
    ) -> Result<Self> {
        settings.validate().map_err(|_| QbitError::InvalidRequest)?;
        if !matches!(settings, ProviderSettings::Qbittorrent { .. }) {
            return Err(QbitError::InvalidRequest);
        }
        let origin = url::Url::parse(settings.endpoint())
            .map_err(|_| QbitError::InvalidRequest)?
            .origin()
            .ascii_serialization();
        let mut session = Self {
            operation,
            settings,
            credentials,
            headers: vec![
                ("Referer".into(), origin.clone()),
                ("Origin".into(), origin),
            ],
            observed_sids: Vec::new(),
            version: Version(2, 0, 0),
            legacy: false,
        };
        if let Some(Credentials::ApiKey { api_key }) = credentials {
            if api_key.is_empty() || api_key.len() > 4096 || api_key.chars().any(char::is_control) {
                return Err(QbitError::InvalidRequest);
            }
            session
                .headers
                .push(("Authorization".into(), format!("Bearer {api_key}")));
        } else if matches!(credentials, Some(Credentials::Indexer { .. })) {
            return Err(QbitError::InvalidRequest);
        }
        // Only an unsupported endpoint permits protocol fallback, never authentication or network failure.
        let probe = session.get_raw("api/v2/app/webapiVersion", &[]).await;
        let response = match probe {
            Err(QbitError::Http(HttpError::Authentication))
                if matches!(credentials, Some(Credentials::UsernamePassword { .. })) =>
            {
                session.login().await?;
                session.get_raw("api/v2/app/webapiVersion", &[]).await?
            }
            other => other?,
        };
        let response = if response.status == 404 {
            session.legacy = true;
            session.headers.retain(|(k, _)| k != "Cookie");
            match session.get_raw("version/api", &[]).await {
                Err(QbitError::Http(HttpError::Authentication))
                    if matches!(credentials, Some(Credentials::UsernamePassword { .. })) =>
                {
                    session.login().await?;
                    session.get_raw("version/api", &[]).await?
                }
                other => other?,
            }
        } else {
            response
        };
        check_status(response.status)?;
        session.version = Version::parse(&response.body, session.legacy)?;
        if (session.legacy && (session.version.0 != 1 || session.version < Version(1, 5, 0)))
            || (!session.legacy && session.version.0 != 2)
        {
            return Err(QbitError::UnsupportedVersion);
        }
        // Independently configured categories are mandatory for this shared client.
        if session.legacy && session.version < Version(1, 6, 0) {
            return Err(QbitError::UnsupportedFeature);
        }
        Ok(session)
    }
    fn endpoint(&self, path: &str) -> String {
        format!(
            "{}/{}",
            self.settings.endpoint().trim_end_matches('/'),
            path
        )
    }
    async fn get_raw(&self, path: &str, query: &[(String, String)]) -> Result<HttpResponse> {
        Ok(self
            .operation
            .request(
                &self.endpoint(path),
                query,
                &self.headers,
                HttpRequestBody::Empty,
            )
            .await?)
    }
    async fn login(&mut self) -> Result<()> {
        let Some(Credentials::UsernamePassword { username, password }) = self.credentials else {
            return Err(QbitError::Http(HttpError::Authentication));
        };
        if username.len() > 4096
            || password.len() > 4096
            || username.chars().any(char::is_control)
            || password.chars().any(char::is_control)
        {
            return Err(QbitError::InvalidRequest);
        }
        self.headers.retain(|(k, _)| k != "Cookie");
        let path = if self.legacy {
            "login"
        } else {
            "api/v2/auth/login"
        };
        let response = self
            .operation
            .request(
                &self.endpoint(path),
                &[],
                &self.headers,
                HttpRequestBody::Form(vec![
                    ("username".into(), username.clone()),
                    ("password".into(), password.clone()),
                ]),
            )
            .await?;
        check_status(response.status)?;
        if response.body != b"Ok." {
            return Err(QbitError::Http(HttpError::Authentication));
        }
        let mut cookies = response
            .set_cookies
            .iter()
            .filter_map(|c| c.split(';').next())
            .filter_map(|c| c.strip_prefix("SID="));
        let sid = cookies
            .next()
            .ok_or(QbitError::Http(HttpError::Authentication))?;
        if cookies.next().is_some()
            || sid.is_empty()
            || sid.len() > 1024
            || !sid
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
        {
            return Err(QbitError::InvalidResponse);
        }
        if !self.observed_sids.iter().any(|known| known == sid) {
            // ponytail: eight session rotations per bounded operation; fail closed beyond this ceiling.
            if self.observed_sids.len() == 8 {
                return Err(QbitError::InvalidResponse);
            }
            self.observed_sids.push(sid.to_owned());
        }
        self.headers.push(("Cookie".into(), format!("SID={sid}")));
        Ok(())
    }
    async fn get(
        &mut self,
        modern: &str,
        legacy: &str,
        query: &[(String, String)],
    ) -> Result<Value> {
        let path = if self.legacy { legacy } else { modern };
        let response = match self.get_raw(path, query).await {
            Err(QbitError::Http(HttpError::Authentication))
                if matches!(self.credentials, Some(Credentials::UsernamePassword { .. })) =>
            {
                self.login().await?;
                self.get_raw(path, query).await?
            }
            other => other?,
        };
        check_status(response.status)?;
        serde_json::from_slice(&response.body).map_err(|_| QbitError::InvalidResponse)
    }
    async fn mutate(&self, modern: &str, legacy: &str, body: HttpRequestBody) -> Result<()> {
        let path = if self.legacy { legacy } else { modern };
        // No reauthentication/replay of a mutation: transport failure does not prove it was not applied.
        let response = self
            .operation
            .request(&self.endpoint(path), &[], &self.headers, body)
            .await
            .map_err(|e| match e {
                HttpError::InvalidRequest => QbitError::InvalidRequest,
                HttpError::Authentication => QbitError::Http(e),
                HttpError::RateLimited { .. } => QbitError::Http(e),
                _ => QbitError::MutationUnknown,
            })?;
        if response.status >= 500 {
            return Err(QbitError::MutationUnknown);
        }
        check_status(response.status)?;
        match response.body.as_slice() {
            b"" | b"Ok." => Ok(()),
            b"Fails." | b"Fail." => Err(QbitError::Rejected),
            _ => Err(QbitError::MutationUnknown),
        }
    }
    fn scope_field(&self) -> &'static str {
        if self.legacy && self.version < Version(1, 10, 0) {
            "label"
        } else {
            "category"
        }
    }
    fn category(&self, item: &Value) -> Result<String> {
        let field = self.scope_field();
        let category = text(item, field, 64)?;
        let other = if field == "label" {
            "category"
        } else {
            "label"
        };
        if item.get(other).is_some() && text(item, other, 64)? != category {
            return Err(QbitError::InvalidResponse);
        }
        Ok(category)
    }
    fn validate_scope_name(&self, name: &str) -> Result<()> {
        if self.scope_field() == "label"
            && (name.trim() != name
                || name == "."
                || name == ".."
                || name.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|']))
        {
            return Err(QbitError::UnsupportedFeature);
        }
        Ok(())
    }
    async fn categories(&mut self) -> Result<BTreeMap<String, String>> {
        let value = if self.version >= Version(2, 1, 1) {
            self.get("api/v2/torrents/categories", "", &[]).await?
        } else {
            self.get(
                "api/v2/sync/maindata",
                "sync/maindata",
                &[("rid".into(), "0".into())],
            )
            .await?
            .get(if self.scope_field() == "label" {
                "labels"
            } else {
                "categories"
            })
            .cloned()
            .ok_or(QbitError::InvalidResponse)?
        };
        let mut result = BTreeMap::new();
        match value {
            Value::Object(items) => {
                for (key, value) in items {
                    if key.len() > 64 || key.chars().any(char::is_control) {
                        return Err(QbitError::InvalidResponse);
                    }
                    if !value.is_object() {
                        return Err(QbitError::InvalidResponse);
                    }
                    let path = if value.get("savePath").is_some() {
                        text(&value, "savePath", 4096)?
                    } else {
                        String::new()
                    };
                    result.insert(key, path);
                }
            }
            Value::Array(items) => {
                for item in items {
                    let Some(name) = item
                        .as_str()
                        .filter(|s| s.len() <= 64 && !s.chars().any(char::is_control))
                    else {
                        return Err(QbitError::InvalidResponse);
                    };
                    result.insert(name.to_owned(), String::new());
                }
            }
            _ => return Err(QbitError::InvalidResponse),
        }
        if result.len() > 4096 {
            return Err(QbitError::InvalidResponse);
        }
        Ok(result)
    }
    async fn raw_list(&mut self, query: &[(String, String)]) -> Result<Vec<Value>> {
        let value = self
            .get("api/v2/torrents/info", "query/torrents", query)
            .await?;
        let Value::Array(items) = value else {
            return Err(QbitError::InvalidResponse);
        };
        if items.len() > 500 {
            return Err(QbitError::InvalidResponse);
        }
        Ok(items)
    }
    async fn find(&mut self, domain: MediaDomain, expected: &str) -> Result<Option<Value>> {
        let expected = hash(expected)?;
        let args = if self.version < Version(2, 0, 1) {
            vec![("limit".into(), "500".into())]
        } else {
            vec![
                ("hashes".into(), expected.clone()),
                ("limit".into(), "2".into()),
            ]
        };
        let items = self.raw_list(&args).await?;
        if self.version < Version(2, 0, 1) && items.len() == 500 {
            return Err(QbitError::UnsupportedFeature);
        }
        let mut found = None;
        for item in items {
            let actual = hash(&text(&item, "hash", 64)?).map_err(|_| QbitError::InvalidResponse)?;
            if actual != expected {
                if self.version >= Version(2, 0, 1) {
                    return Err(QbitError::InvalidResponse);
                }
                continue;
            }
            let configured = scope(self.settings, domain)?;
            let category = self.category(&item)?;
            if category != configured.category
                && configured.imported_category.as_ref() != Some(&category)
            {
                return Err(QbitError::ScopeConflict);
            }
            if found.replace(item).is_some() {
                return Err(QbitError::InvalidResponse);
            }
        }
        Ok(found)
    }
    fn redact(&self, mut value: String) -> String {
        match self.credentials {
            Some(Credentials::ApiKey { api_key }) => {
                if !api_key.is_empty() {
                    value = value.replace(api_key, "[redacted]")
                }
            }
            Some(Credentials::UsernamePassword { username, password }) => {
                for secret in [username, password] {
                    if !secret.is_empty() {
                        value = value.replace(secret, "[redacted]")
                    }
                }
            }
            _ => {}
        }
        for sid in &self.observed_sids {
            value = value.replace(sid, "[redacted]");
        }
        value
    }
    fn item(&self, value: &Value, domain: MediaDomain) -> Result<DownloadItem> {
        let remote_state = text(value, "state", 64)?;
        let status = state(&remote_state);
        let diagnostic = match remote_state.as_str() {
            "error" => Some(DownloadDiagnostic::Error),
            "missingFiles" => Some(DownloadDiagnostic::MissingFiles),
            "stalledDL" => Some(DownloadDiagnostic::Stalled),
            "metaDL" | "forcedMetaDL" => Some(DownloadDiagnostic::Metadata),
            _ if status == DownloadStatus::Unknown => Some(DownloadDiagnostic::UnknownState),
            _ => None,
        };
        let completed = status == DownloadStatus::Completed
            && fraction(value, "progress", 1.0)? == 1.0
            && value
                .get("amount_left")
                .map(|_| number(value, "amount_left"))
                .transpose()?
                .is_none_or(|n| n == 0);
        const UNKNOWN_ETA_SECONDS: u64 = 8_640_000;
        const MAX_ETA_SECONDS: u64 = 365 * 24 * 60 * 60;
        let eta = if completed {
            Some(0)
        } else {
            value
                .get("eta")
                .and_then(Value::as_u64)
                .filter(|n| *n != UNKNOWN_ETA_SECONDS && *n <= MAX_ETA_SECONDS)
        };
        Ok(DownloadItem {
            hash: hash(&text(value, "hash", 64)?).map_err(|_| QbitError::InvalidResponse)?,
            domain,
            name: self.redact(text(value, "name", 1024)?),
            category: self.category(value)?,
            status,
            diagnostic,
            progress: fraction(value, "progress", 1.0)?,
            size_bytes: number(value, "size")?,
            remaining_bytes: value
                .get("amount_left")
                .map(|_| number(value, "amount_left"))
                .transpose()?,
            download_bytes_per_second: number(value, "dlspeed")?,
            upload_bytes_per_second: number(value, "upspeed")?,
            eta_seconds: eta,
            ratio: fraction(value, "ratio", 1_000_000.0)?,
            seeding_seconds: value
                .get("seeding_time")
                .map(|_| number(value, "seeding_time"))
                .transpose()?,
            completed,
        })
    }
}
fn check_status(status: u16) -> Result<()> {
    match status {
        200..=299 => Ok(()),
        404 => Err(QbitError::NotFound),
        400 | 409 | 415 | 422 => Err(QbitError::Rejected),
        401 | 403 => Err(QbitError::Http(HttpError::Authentication)),
        _ => Err(QbitError::Http(HttpError::Transport)),
    }
}

pub async fn test_connection(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
) -> Result<ClientTest> {
    let mut session = Session::connect(operation, settings, credentials).await?;
    let version_path = if session.legacy {
        "version/qbittorrent"
    } else {
        "api/v2/app/version"
    };
    let response = session.get_raw(version_path, &[]).await?;
    check_status(response.status)?;
    let application_version = std::str::from_utf8(&response.body)
        .ok()
        .filter(|v| {
            v.len() <= 64
                && v.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b))
        })
        .ok_or(QbitError::InvalidResponse)?
        .to_owned();
    let preferences = session
        .get("api/v2/app/preferences", "query/preferences", &[])
        .await?;
    validate_retention(session.version, &preferences)?;
    let queueing_enabled = preferences
        .get("queueing_enabled")
        .and_then(Value::as_bool)
        .ok_or(QbitError::InvalidResponse)?;
    let categories = session.categories().await?;
    let mut domains = Vec::new();
    let mut missing_categories = Vec::new();
    for domain in [MediaDomain::Tv, MediaDomain::Movies] {
        let Ok(configured) = scope(settings, domain) else {
            continue;
        };
        if !queueing_enabled && (configured.recent_priority == 1 || configured.older_priority == 1)
        {
            return Err(QbitError::UnsupportedFeature);
        }
        for category in
            std::iter::once(&configured.category).chain(configured.imported_category.iter())
        {
            if !categories.contains_key(category) {
                missing_categories.push(category.clone())
            }
        }
        // Authentication must also authorize scoped torrent reads; caps/version alone is insufficient.
        let items = session
            .raw_list(&[
                (session.scope_field().into(), configured.category.clone()),
                ("limit".into(), "1".into()),
            ])
            .await?;
        if items
            .iter()
            .any(|v| session.category(v).as_ref() != Ok(&configured.category))
        {
            return Err(QbitError::ScopeConflict);
        }
        domains.push(domain);
    }
    Ok(ClientTest {
        api_version: session.version.text(),
        application_version: session.redact(application_version),
        domains,
        missing_categories,
        queueing_enabled,
    })
}
pub async fn query(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
    request: &DownloadQuery,
) -> Result<DownloadPage> {
    if !(1..=500).contains(&request.limit) || request.offset > i32::MAX as u32 {
        return Err(QbitError::InvalidRequest);
    }
    let configured = scope(settings, request.domain)?;
    let selected = if request.imported {
        configured
            .imported_category
            .as_ref()
            .ok_or(QbitError::ScopeConflict)?
    } else {
        &configured.category
    };
    let mut session = Session::connect(operation, settings, credentials).await?;
    let raw = session
        .raw_list(&[
            (session.scope_field().into(), selected.clone()),
            ("offset".into(), request.offset.to_string()),
            ("limit".into(), request.limit.to_string()),
            ("sort".into(), "hash".into()),
        ])
        .await?;
    if raw.len() > request.limit as usize {
        return Err(QbitError::InvalidResponse);
    }
    let preferences = if raw.iter().any(|value| {
        matches!(
            value.get("state").and_then(Value::as_str),
            Some("metaDL" | "forcedMetaDL")
        )
    }) {
        Some(
            session
                .get("api/v2/app/preferences", "query/preferences", &[])
                .await?,
        )
    } else {
        None
    };
    let mut items = Vec::new();
    let mut hashes = BTreeSet::new();
    for value in raw {
        if session.category(&value)? != *selected {
            return Err(QbitError::ScopeConflict);
        }
        let mut item = session.item(&value, request.domain)?;
        if let Some(preferences) = &preferences {
            apply_metadata_preferences(&mut item, preferences)?;
        }
        if !hashes.insert(item.hash.clone()) {
            return Err(QbitError::InvalidResponse);
        }
        items.push(item);
    }
    let next_offset = if items.len() == request.limit as usize {
        Some(
            request
                .offset
                .checked_add(request.limit)
                .ok_or(QbitError::InvalidRequest)?,
        )
    } else {
        None
    };
    Ok(DownloadPage {
        domain: request.domain,
        offset: request.offset,
        limit: request.limit,
        next_offset,
        items,
    })
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct DownloadFilesQuery {
    pub domain: MediaDomain,
    pub hash: String,
}
#[derive(Serialize, ts_rs::TS)]
pub struct DownloadFiles {
    pub domain: MediaDomain,
    pub hash: String,
    pub files: Vec<DownloadFile>,
}
#[derive(Serialize, ts_rs::TS)]
pub struct DownloadFile {
    pub index: u32,
    pub name: String,
    pub size_bytes: u64,
    pub progress: f64,
    pub priority: u8,
}
impl Session<'_, '_> {
    async fn files(&mut self, domain: MediaDomain, expected: &str) -> Result<Vec<TorrentFile>> {
        self.find(domain, expected)
            .await?
            .ok_or(QbitError::NotFound)?;
        let path = format!("query/propertiesFiles/{}", hash(expected)?);
        let value = self
            .get(
                "api/v2/torrents/files",
                &path,
                &if self.legacy {
                    vec![]
                } else {
                    vec![("hash".into(), expected.into())]
                },
            )
            .await?;
        let Value::Array(values) = value else {
            return Err(QbitError::InvalidResponse);
        };
        if values.len() > 4096 {
            return Err(QbitError::InvalidResponse);
        }
        let mut indexes = BTreeSet::new();
        let mut files = Vec::new();
        for (position, value) in values.iter().enumerate() {
            let index = if self.version >= Version(2, 8, 2) {
                u32::try_from(number(value, "index")?).map_err(|_| QbitError::InvalidResponse)?
            } else {
                position as u32
            };
            if !indexes.insert(index) {
                return Err(QbitError::InvalidResponse);
            }
            let name = text(value, "name", 4096)?;
            // Remote names remain relative; a later path mapper decides filesystem ownership.
            if name.starts_with('/')
                || name.starts_with('\\')
                || name.contains(':')
                || name
                    .split(['/', '\\'])
                    .any(|part| part == ".." || part.is_empty())
            {
                return Err(QbitError::InvalidResponse);
            }
            let priority =
                u8::try_from(number(value, "priority")?).map_err(|_| QbitError::InvalidResponse)?;
            if ![0, 1, 4, 6, 7].contains(&priority) {
                return Err(QbitError::InvalidResponse);
            }
            files.push(TorrentFile {
                index,
                name,
                size_bytes: number(value, "size")?,
                progress: fraction(value, "progress", 1.0)?,
                priority,
            });
        }
        Ok(files)
    }
}
pub async fn files(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
    request: &DownloadFilesQuery,
) -> Result<DownloadFiles> {
    scope(settings, request.domain)?;
    hash(&request.hash)?;
    let mut session = Session::connect(operation, settings, credentials).await?;
    let files = session
        .files(request.domain, &request.hash)
        .await?
        .into_iter()
        .map(|file| DownloadFile {
            index: file.index,
            name: session.redact(file.name),
            size_bytes: file.size_bytes,
            progress: file.progress,
            priority: file.priority,
        })
        .collect();
    Ok(DownloadFiles {
        domain: request.domain,
        hash: request.hash.to_ascii_lowercase(),
        files,
    })
}
pub async fn details(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
    target: &MediaTarget,
    expected: &str,
) -> Result<TorrentDetails> {
    let domain = target_domain(target)?;
    scope(settings, domain)?;
    hash(expected)?;
    let mut session = Session::connect(operation, settings, credentials).await?;
    let value = session
        .find(domain, expected)
        .await?
        .ok_or(QbitError::NotFound)?;
    let mut item = session.item(&value, domain)?;
    let files = session.files(domain, expected).await?;
    let path = format!("query/propertiesGeneral/{}", hash(expected)?);
    let properties = session
        .get(
            "api/v2/torrents/properties",
            &path,
            &if session.legacy {
                vec![]
            } else {
                vec![("hash".into(), expected.into())]
            },
        )
        .await?;
    let save_path = if value.get("save_path").is_some() {
        text(&value, "save_path", 4096)?
    } else {
        text(&properties, "save_path", 4096)?
    };
    let content_path = if session.version >= Version(2, 6, 1) {
        let path = text(&value, "content_path", 4096)?;
        if item.completed
            && (path.is_empty()
                || path.trim_end_matches(['/', '\\']) == save_path.trim_end_matches(['/', '\\']))
        {
            return Err(QbitError::InvalidResponse);
        }
        (!path.is_empty()).then_some(path)
    } else if files.is_empty() {
        None
    } else {
        let roots = files
            .iter()
            .map(|f| f.name.split(['/', '\\']).next().unwrap_or(""))
            .collect::<BTreeSet<_>>();
        if roots.len() == 1 {
            Some(format!(
                "{}/{}",
                save_path.trim_end_matches(['/', '\\']),
                roots.first().unwrap()
            ))
        } else {
            None
        }
    };
    let preferences = session
        .get("api/v2/app/preferences", "query/preferences", &[])
        .await?;
    apply_metadata_preferences(&mut item, &preferences)?;
    let now_seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| QbitError::InvalidResponse)?
        .as_secs();
    let seed_policy = seed_policy(
        session.version,
        &value,
        &properties,
        &preferences,
        item.completed,
        now_seconds,
    )?;
    Ok(TorrentDetails {
        item,
        save_path,
        content_path,
        files,
        properties,
        seed_policy,
    })
}
/// Private protocol facts: no serialization/debugging of remote filesystem paths.
pub struct ClientStatus {
    pub domain: MediaDomain,
    pub remote_root: String,
    pub root_source: RootSource,
    pub locality: EndpointLocality,
    pub removes_completed_downloads: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootSource {
    Default,
    Category,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndpointLocality {
    Loopback,
    Unknown,
}
fn endpoint_locality(endpoint: &str) -> Result<EndpointLocality> {
    let url = url::Url::parse(endpoint).map_err(|_| QbitError::InvalidRequest)?;
    let loopback = match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(name)) => {
            name.trim_end_matches('.').eq_ignore_ascii_case("localhost")
        }
        _ => false,
    };
    Ok(if loopback {
        EndpointLocality::Loopback
    } else {
        EndpointLocality::Unknown
    })
}
fn validate_remote_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.len() > 4096
        || path.chars().any(char::is_control)
        || path
            .split(['/', '\\'])
            .any(|part| part == "." || part == "..")
    {
        return Err(QbitError::InvalidResponse);
    }
    // Drive-relative paths and device namespaces cannot be resolved without remote OS state.
    if path.contains(':')
        && !(path.len() >= 3
            && path.as_bytes()[0].is_ascii_alphabetic()
            && path.as_bytes()[1] == b':'
            && matches!(path.as_bytes()[2], b'/' | b'\\')
            && !path[2..].contains(':'))
    {
        return Err(QbitError::InvalidResponse);
    }
    if path.starts_with("\\\\") || path.starts_with("//") {
        let parts = path[2..]
            .split(['/', '\\'])
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>();
        if parts.len() < 2 || matches!(parts[0], "?" | ".") {
            return Err(QbitError::InvalidResponse);
        }
    } else if path.starts_with('\\') {
        return Err(QbitError::InvalidResponse);
    }
    Ok(())
}
fn remote_root(default: &str, category: Option<&str>) -> Result<(String, RootSource)> {
    validate_remote_path(default)?;
    let rooted = |p: &str| {
        p.starts_with('/') || p.starts_with("\\\\") || (p.len() >= 3 && p.as_bytes()[1] == b':')
    };
    if !rooted(default) {
        return Err(QbitError::InvalidResponse);
    }
    let Some(category) = category.filter(|p| !p.trim().is_empty()) else {
        return Ok((default.to_owned(), RootSource::Default));
    };
    validate_remote_path(category)?;
    let path = if rooted(category) {
        category.to_owned()
    } else {
        let separator = if default.contains('\\') { '\\' } else { '/' };
        let relative = category.replace(['/', '\\'], &separator.to_string());
        format!(
            "{}{separator}{relative}",
            default.trim_end_matches(['/', '\\'])
        )
    };
    validate_remote_path(&path)?;
    Ok((path, RootSource::Category))
}
pub async fn client_status(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
    domain: MediaDomain,
) -> Result<ClientStatus> {
    let configured = scope(settings, domain)?;
    let mut session = Session::connect(operation, settings, credentials).await?;
    let preferences = session
        .get("api/v2/app/preferences", "query/preferences", &[])
        .await?;
    let removes_completed_downloads = removes_completed_downloads(session.version, &preferences)?;
    let default = text(&preferences, "save_path", 4096)?;
    let categories = if session.legacy {
        BTreeMap::new()
    } else {
        session.categories().await?
    };
    let (remote_root, root_source) = remote_root(
        &default,
        categories.get(&configured.category).map(String::as_str),
    )?;
    Ok(ClientStatus {
        domain,
        remote_root,
        root_source,
        locality: endpoint_locality(settings.endpoint())?,
        removes_completed_downloads,
    })
}
fn verify_tags(torrent: &Value, requested: &[String]) -> Result<()> {
    let raw = text(torrent, "tags", 16_384)?;
    let tags = if raw.is_empty() {
        Vec::new()
    } else {
        raw.split(',').map(str::trim).collect::<Vec<_>>()
    };
    if tags.len() > 256
        || tags.iter().any(|tag| tag.is_empty() || tag.len() > 256)
        || requested.iter().any(|tag| !tags.contains(&tag.as_str()))
    {
        return Err(QbitError::InvalidResponse);
    }
    Ok(())
}
pub use super::{DownloadContentLayout as ContentLayout, DownloadInitialState as InitialState};
#[derive(Default)]
pub struct QbitOptions {
    pub tags: Vec<String>,
    pub ratio_limit: Option<f64>,
    pub seeding_minutes: Option<i64>,
    pub inactive_seeding_minutes: Option<i64>,
}
/// A caller must persist target + identity + intent before invoking these external mutations.
/// Category membership proves domain, not association with a particular episode/movie.
pub enum Control {
    Pause,
    Resume,
    Remove {
        delete_files: bool,
    },
    Priority {
        first: bool,
    },
    MarkImported,
    ShareLimits {
        ratio: f64,
        seeding_minutes: i64,
        inactive_seeding_minutes: Option<i64>,
    },
    FilePriority {
        indexes: Vec<u32>,
        priority: u8,
    },
    ForceStart {
        enabled: bool,
    },
}
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
    let expected = hash(expected)?;
    let mut session = Session::connect(operation, settings, credentials).await?;
    let item = session
        .find(domain, &expected)
        .await?
        .ok_or(QbitError::NotFound)?;
    let action = match action {
        Control::ShareLimits {
            ratio,
            seeding_minutes,
            inactive_seeding_minutes: None,
        } if session.version >= Version(2, 9, 2) => {
            let preserved = item
                .get("inactive_seeding_time_limit")
                .and_then(Value::as_i64)
                .filter(|n| (-2..=i32::MAX as i64).contains(n))
                .ok_or(QbitError::InvalidResponse)?;
            Control::ShareLimits {
                ratio,
                seeding_minutes,
                inactive_seeding_minutes: Some(preserved),
            }
        }
        action => action,
    };
    let mut args = vec![("hashes".into(), expected.clone())];
    let (modern, legacy) = match &action {
        Control::Pause => {
            if session.legacy {
                args[0].0 = "hash".into()
            }
            (
                if session.version >= Version(2, 11, 0) {
                    "api/v2/torrents/stop"
                } else {
                    "api/v2/torrents/pause"
                },
                "command/pause",
            )
        }
        Control::Resume => {
            if session.legacy {
                args[0].0 = "hash".into()
            }
            (
                if session.version >= Version(2, 11, 0) {
                    "api/v2/torrents/start"
                } else {
                    "api/v2/torrents/resume"
                },
                "command/resume",
            )
        }
        Control::Remove { delete_files } => {
            if !session.legacy {
                args.push(("deleteFiles".into(), delete_files.to_string()))
            }
            (
                "api/v2/torrents/delete",
                if *delete_files {
                    "command/deletePerm"
                } else {
                    "command/delete"
                },
            )
        }
        Control::Priority { first } => {
            let preferences = session
                .get("api/v2/app/preferences", "query/preferences", &[])
                .await?;
            if preferences.get("queueing_enabled").and_then(Value::as_bool) != Some(true) {
                return Err(QbitError::UnsupportedFeature);
            }
            if *first {
                ("api/v2/torrents/topPrio", "command/topPrio")
            } else {
                ("api/v2/torrents/bottomPrio", "command/bottomPrio")
            }
        }
        Control::MarkImported => {
            let category = configured
                .imported_category
                .as_ref()
                .ok_or(QbitError::UnsupportedFeature)?;
            if !session.item(&item, domain)?.completed {
                return Err(QbitError::Rejected);
            }
            // Category changes can move files under automatic torrent management; caller intent is explicit.
            session.ensure_category(category).await?;
            args.push((session.scope_field().into(), category.clone()));
            (
                "api/v2/torrents/setCategory",
                if session.scope_field() == "label" {
                    "command/setLabel"
                } else {
                    "command/setCategory"
                },
            )
        }
        Control::ShareLimits {
            ratio,
            seeding_minutes,
            inactive_seeding_minutes,
        } => {
            if session.version < Version(2, 0, 1)
                || !ratio.is_finite()
                || (*ratio < 0.0 && *ratio != -1.0 && *ratio != -2.0)
                || *ratio > 1_000_000.0
                || *seeding_minutes < -2
                || *seeding_minutes > i32::MAX as i64
            {
                return Err(QbitError::UnsupportedFeature);
            }
            args.push(("ratioLimit".into(), ratio.to_string()));
            args.push(("seedingTimeLimit".into(), seeding_minutes.to_string()));
            if let Some(minutes) = inactive_seeding_minutes {
                if session.version < Version(2, 9, 2) || !(-2..=i32::MAX as i64).contains(minutes) {
                    return Err(QbitError::UnsupportedFeature);
                }
                args.push(("inactiveSeedingTimeLimit".into(), minutes.to_string()));
            }
            ("api/v2/torrents/setShareLimits", "")
        }
        Control::FilePriority { indexes, priority } => {
            if indexes.is_empty() || indexes.len() > 4096 || ![0, 1, 6, 7].contains(priority) {
                return Err(QbitError::InvalidRequest);
            }
            if indexes.len() > 1 && session.version < Version(2, 2, 0) {
                return Err(QbitError::UnsupportedFeature);
            }
            let valid = session
                .files(domain, &expected)
                .await?
                .into_iter()
                .map(|file| file.index)
                .collect::<BTreeSet<_>>();
            if indexes.iter().any(|i| !valid.contains(i))
                || indexes.iter().collect::<BTreeSet<_>>().len() != indexes.len()
            {
                return Err(QbitError::InvalidRequest);
            }
            args[0].0 = "hash".into();
            args.push((
                "id".into(),
                indexes
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join("|"),
            ));
            args.push(("priority".into(), priority.to_string()));
            ("api/v2/torrents/filePrio", "command/setFilePrio")
        }
        Control::ForceStart { enabled } => {
            args.push(("value".into(), enabled.to_string()));
            ("api/v2/torrents/setForceStart", "command/setForceStart")
        }
    };
    // Check again after any read needed to build the mutation; never trust category from an earlier page.
    let current = session
        .find(domain, &expected)
        .await?
        .ok_or(QbitError::NotFound)?;
    if matches!(action, Control::MarkImported) && !session.item(&current, domain)?.completed {
        return Err(QbitError::Rejected);
    }
    session
        .mutate(modern, legacy, HttpRequestBody::Form(args))
        .await?;
    let after = session
        .find(domain, &expected)
        .await
        .map_err(|_| QbitError::MutationUnknown)?;
    match action {
        Control::Remove { .. } => {
            if after.is_some() {
                return Err(QbitError::MutationUnknown);
            }
        }
        Control::MarkImported => {
            if after
                .as_ref()
                .map(|value| session.category(value))
                .transpose()
                .map_err(|_| QbitError::MutationUnknown)?
                .as_ref()
                != configured.imported_category.as_ref()
            {
                return Err(QbitError::MutationUnknown);
            }
        }
        Control::Pause => {
            if !after
                .as_ref()
                .and_then(|v| v.get("state"))
                .and_then(Value::as_str)
                .is_some_and(|s| s.starts_with("paused") || s.starts_with("stopped"))
            {
                return Err(QbitError::MutationUnknown);
            }
        }
        Control::Resume => {
            if !after
                .as_ref()
                .and_then(|v| v.get("state"))
                .and_then(Value::as_str)
                .is_some_and(observed_running)
            {
                return Err(QbitError::MutationUnknown);
            }
        }
        action => {
            let after = after.as_ref().ok_or(QbitError::MutationUnknown)?;
            session
                .verify_control(domain, &expected, &action, after)
                .await
                .map_err(|_| QbitError::MutationUnknown)?;
        }
    }
    Ok(())
}
impl Session<'_, '_> {
    async fn verify_control(
        &mut self,
        domain: MediaDomain,
        expected: &str,
        action: &Control,
        after: &Value,
    ) -> Result<()> {
        let observed = match action {
            Control::ForceStart { enabled } => {
                after.get("force_start").and_then(Value::as_bool) == Some(*enabled)
            }
            Control::ShareLimits {
                ratio,
                seeding_minutes,
                inactive_seeding_minutes,
            } => {
                after.get("ratio_limit").and_then(Value::as_f64) == Some(*ratio)
                    && after.get("seeding_time_limit").and_then(Value::as_i64)
                        == Some(*seeding_minutes)
                    && inactive_seeding_minutes.is_none_or(|n| {
                        after
                            .get("inactive_seeding_time_limit")
                            .and_then(Value::as_i64)
                            == Some(n)
                    })
            }
            Control::FilePriority { indexes, priority } => {
                let files = self.files(domain, expected).await?;
                indexes.iter().all(|index| {
                    files.iter().any(|file| {
                        file.index == *index
                            && (file.priority == *priority
                                || (*priority == 1 && file.priority == 4))
                    })
                })
            }
            Control::Priority { first } => {
                let priority = number(after, "priority")?;
                if *first {
                    priority == 1
                } else {
                    let last = self
                        .raw_list(&[
                            ("sort".into(), "priority".into()),
                            ("reverse".into(), "true".into()),
                            ("limit".into(), "1".into()),
                        ])
                        .await?;
                    last.len() == 1 && priority > 0 && number(&last[0], "priority")? == priority
                }
            }
            _ => false,
        };
        if observed {
            Ok(())
        } else {
            Err(QbitError::MutationUnknown)
        }
    }
}
/// Input is deliberately not Debug/Serialize: URLs and torrent metadata may contain passkeys.
pub enum AddSource {
    Magnet(String),
    Url(String),
    Torrent { filename: String, bytes: Vec<u8> },
}
#[derive(Debug, PartialEq, Eq)]
pub enum AddOutcome {
    AlreadyPresent {
        hash: String,
    },
    /// Identity/domain and explicitly checked options were observed, not filesystem layout.
    Observed {
        hash: String,
    },
    Pending {
        hashes: Vec<String>,
    },
}
fn digest_hex(algorithm: &'static ring::digest::Algorithm, data: &[u8]) -> String {
    ring::digest::digest(algorithm, data)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn magnet_hashes(value: &str) -> Result<Vec<String>> {
    if value.len() > 16384 || value.chars().any(char::is_control) {
        return Err(QbitError::InvalidRequest);
    }
    let url = url::Url::parse(value).map_err(|_| QbitError::InvalidRequest)?;
    if url.scheme() != "magnet" || url.host_str().is_some() || url.fragment().is_some() {
        return Err(QbitError::InvalidRequest);
    }
    let mut identities = BTreeSet::new();
    for (_, xt) in url.query_pairs().filter(|(key, _)| key == "xt") {
        if let Some(v1) = xt.strip_prefix("urn:btih:") {
            if v1.len() == 40 {
                identities.insert(hash(v1)?);
            } else if v1.len() == 32 {
                let mut bits = 0u32;
                let mut count = 0;
                let mut bytes = Vec::new();
                for c in v1.bytes() {
                    let value = match c.to_ascii_uppercase() {
                        b'A'..=b'Z' => c.to_ascii_uppercase() - b'A',
                        b'2'..=b'7' => c - b'2' + 26,
                        _ => return Err(QbitError::InvalidRequest),
                    };
                    bits = (bits << 5) | u32::from(value);
                    count += 5;
                    if count >= 8 {
                        count -= 8;
                        bytes.push((bits >> count) as u8);
                        bits &= (1 << count) - 1;
                    }
                }
                identities.insert(bytes.iter().map(|b| format!("{b:02x}")).collect());
            } else {
                return Err(QbitError::InvalidRequest);
            }
        } else if let Some(v2) = xt.strip_prefix("urn:btmh:1220") {
            if v2.len() != 64 {
                return Err(QbitError::InvalidRequest);
            }
            identities.insert(hash(v2)?);
        } else {
            return Err(QbitError::InvalidRequest);
        }
    }
    if identities.is_empty()
        || identities.iter().filter(|id| id.len() == 40).count() > 1
        || identities.iter().filter(|id| id.len() == 64).count() > 1
    {
        return Err(QbitError::InvalidRequest);
    }
    Ok(identities.into_iter().collect())
}
/// Walk bencode without reserializing: info hashes cover the original byte slice.
fn torrent_hashes(bytes: &[u8]) -> Result<Vec<String>> {
    struct Decoder<'a> {
        bytes: &'a [u8],
        position: usize,
        nodes: usize,
    }
    impl Decoder<'_> {
        fn string(&mut self) -> Result<&[u8]> {
            let start = self.position;
            while self
                .bytes
                .get(self.position)
                .is_some_and(u8::is_ascii_digit)
            {
                self.position += 1;
            }
            if self.position == start
                || self.position - start > 8
                || self.bytes.get(self.position) != Some(&b':')
            {
                return Err(QbitError::InvalidRequest);
            }
            let digits = &self.bytes[start..self.position];
            if digits.len() > 1 && digits[0] == b'0' {
                return Err(QbitError::InvalidRequest);
            }
            let length = std::str::from_utf8(digits)
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .ok_or(QbitError::InvalidRequest)?;
            self.position += 1;
            let end = self
                .position
                .checked_add(length)
                .filter(|n| *n <= self.bytes.len())
                .ok_or(QbitError::InvalidRequest)?;
            let slice = &self.bytes[self.position..end];
            self.position = end;
            Ok(slice)
        }
        fn value(&mut self, depth: usize) -> Result<()> {
            self.nodes += 1;
            if depth > 32 || self.nodes > 20000 {
                return Err(QbitError::InvalidRequest);
            }
            match self.bytes.get(self.position).copied() {
                Some(b'0'..=b'9') => {
                    self.string()?;
                }
                Some(b'i') => {
                    self.position += 1;
                    let start = self.position;
                    while self.bytes.get(self.position).is_some_and(|b| *b != b'e') {
                        self.position += 1;
                        if self.position - start > 20 {
                            return Err(QbitError::InvalidRequest);
                        }
                    }
                    if self.bytes.get(self.position) != Some(&b'e') {
                        return Err(QbitError::InvalidRequest);
                    }
                    let raw = std::str::from_utf8(&self.bytes[start..self.position])
                        .map_err(|_| QbitError::InvalidRequest)?;
                    if raw.parse::<i64>().is_err()
                        || raw == "-0"
                        || raw.starts_with('+')
                        || (raw.len() > 1 && raw.starts_with('0'))
                        || raw.starts_with("-0")
                    {
                        return Err(QbitError::InvalidRequest);
                    }
                    self.position += 1;
                }
                Some(b'l') => {
                    self.position += 1;
                    while self.bytes.get(self.position) != Some(&b'e') {
                        self.value(depth + 1)?;
                    }
                    self.position += 1;
                }
                Some(b'd') => {
                    self.position += 1;
                    let mut previous: Option<Vec<u8>> = None;
                    while self.bytes.get(self.position) != Some(&b'e') {
                        let key = self.string()?.to_vec();
                        if previous.as_ref().is_some_and(|p| p >= &key) {
                            return Err(QbitError::InvalidRequest);
                        }
                        previous = Some(key);
                        self.value(depth + 1)?;
                    }
                    self.position += 1;
                }
                _ => return Err(QbitError::InvalidRequest),
            }
            Ok(())
        }
    }
    if bytes.is_empty() || bytes.len() > super::http::MAX_BODY_BYTES || bytes[0] != b'd' {
        return Err(QbitError::InvalidRequest);
    }
    let mut decoder = Decoder {
        bytes,
        position: 1,
        nodes: 0,
    };
    let mut info = None;
    let mut previous: Option<Vec<u8>> = None;
    while decoder.bytes.get(decoder.position) != Some(&b'e') {
        let key = decoder.string()?.to_vec();
        if previous.as_ref().is_some_and(|p| p >= &key) {
            return Err(QbitError::InvalidRequest);
        }
        previous = Some(key.clone());
        let start = decoder.position;
        decoder.value(1)?;
        if key == b"info" {
            if bytes.get(start) != Some(&b'd') {
                return Err(QbitError::InvalidRequest);
            }
            info = Some(&bytes[start..decoder.position]);
        }
    }
    if decoder.position + 1 != bytes.len() {
        return Err(QbitError::InvalidRequest);
    }
    let info = info.ok_or(QbitError::InvalidRequest)?;
    let mut fields = Decoder {
        bytes: info,
        position: 1,
        nodes: 0,
    };
    let mut v1 = false;
    let mut v2 = false;
    while fields.bytes.get(fields.position) != Some(&b'e') {
        let key = fields.string()?.to_vec();
        let start = fields.position;
        fields.value(1)?;
        if key == b"pieces" {
            v1 = true;
        }
        if key == b"meta version" {
            if &info[start..fields.position] != b"i2e" {
                return Err(QbitError::InvalidRequest);
            }
            v2 = true;
        }
    }
    let mut result = Vec::new();
    if v1 {
        result.push(digest_hex(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, info));
    }
    if v2 {
        result.push(digest_hex(&ring::digest::SHA256, info));
    }
    if result.is_empty() {
        return Err(QbitError::InvalidRequest);
    }
    Ok(result)
}
// None denotes an omitted field; Some("") explicitly advertises no such hash kind.
fn remote_alias(item: &Value, key: &str, length: usize) -> Result<Option<String>> {
    let Some(value) = item.get(key) else {
        return Ok(None);
    };
    let value = value.as_str().ok_or(QbitError::InvalidResponse)?;
    if value.is_empty() {
        return Ok(Some(String::new()));
    }
    if value.len() != length {
        return Err(QbitError::InvalidResponse);
    }
    hash(value)
        .map(Some)
        .map_err(|_| QbitError::InvalidResponse)
}
impl Session<'_, '_> {
    async fn find_identities(
        &mut self,
        domain: MediaDomain,
        identities: &[String],
    ) -> Result<Option<String>> {
        let requires_v2 = identities.iter().any(|id| id.len() == 64);
        if requires_v2 && self.version < Version(2, 8, 4) {
            return Err(QbitError::UnsupportedFeature);
        }
        // A v1 alias may belong to a hybrid whose API ID differs. Never filter by
        // an infohash on a v2-capable client: the wire filter accepts torrent IDs.
        let aliases_supported = self.version >= Version(2, 8, 4);
        let scan = self.version < Version(2, 0, 1) || aliases_supported;
        let args = if scan {
            vec![("limit".into(), "500".into())]
        } else {
            vec![
                ("hashes".into(), identities.join("|")),
                ("limit".into(), "3".into()),
            ]
        };
        let items = self.raw_list(&args).await?;
        if scan && items.len() == 500 {
            return Err(QbitError::UnsupportedFeature);
        }
        let mut found = None;
        let mut seen_ids = BTreeSet::new();
        let mut seen_aliases = BTreeSet::new();
        for item in items {
            let remote = hash(&text(&item, "hash", 64)?).map_err(|_| QbitError::InvalidResponse)?;
            if !seen_ids.insert(remote.clone()) {
                return Err(QbitError::InvalidResponse);
            }
            let v1 = remote_alias(&item, "infohash_v1", 40)?;
            let v2 = remote_alias(&item, "infohash_v2", 64)?;
            if aliases_supported && (v1.is_none() || v2.is_none()) {
                // An omitted field cannot establish a complete search for aliases.
                return Err(QbitError::UnsupportedFeature);
            }
            if aliases_supported && v1.as_deref() == Some("") && v2.as_deref() == Some("") {
                return Err(QbitError::InvalidResponse);
            }
            for alias in [v1.as_deref(), v2.as_deref()]
                .into_iter()
                .flatten()
                .filter(|s| !s.is_empty())
            {
                if !seen_aliases.insert(alias.to_owned()) {
                    return Err(QbitError::InvalidResponse);
                }
            }
            let any_match = identities
                .iter()
                .any(|id| v1.as_ref() == Some(id) || v2.as_ref() == Some(id) || *id == remote);
            if !any_match {
                if !scan {
                    return Err(QbitError::InvalidResponse);
                }
                continue;
            }
            let configured = scope(self.settings, domain)?;
            let category = self.category(&item)?;
            if category != configured.category
                && configured.imported_category.as_ref() != Some(&category)
            {
                return Err(QbitError::ScopeConflict);
            }
            for expected in identities {
                let alias = if expected.len() == 40 { &v1 } else { &v2 };
                if let Some(alias) = alias {
                    if alias != expected {
                        return Err(QbitError::InvalidResponse);
                    }
                } else if expected != &remote {
                    return Err(QbitError::UnsupportedFeature);
                }
            }
            if found.replace(remote).is_some() {
                return Err(QbitError::InvalidResponse);
            }
        }
        Ok(found)
    }
    async fn ensure_category(&mut self, name: &str) -> Result<()> {
        self.validate_scope_name(name)?;
        // Old labels are assigned by add/setLabel; no standalone create-label endpoint exists.
        if self.scope_field() == "label" {
            return Ok(());
        }
        if self.categories().await?.contains_key(name) {
            return Ok(());
        }
        self.mutate(
            "api/v2/torrents/createCategory",
            "command/addCategory",
            HttpRequestBody::Form(vec![("category".into(), name.into())]),
        )
        .await?;
        if !self
            .categories()
            .await
            .map_err(|_| QbitError::MutationUnknown)?
            .contains_key(name)
        {
            return Err(QbitError::MutationUnknown);
        }
        Ok(())
    }
}
/// Read-only preparation. The caller must persist the identity and provider UUID/revision
/// before handing this non-cloneable payload to `submit`.
pub struct PreparedDownload {
    identity: SubmissionIdentity,
    source: AddSource,
    recent: bool,
    options: QbitOptions,
}
impl PreparedDownload {
    pub fn identity(&self) -> &SubmissionIdentity {
        &self.identity
    }
}
/// Safe journal data: excludes URLs, torrent contents and credentials. This is not a durable journal.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmissionIdentity {
    version: u8,
    target: MediaTarget,
    hashes: Vec<String>,
    settings_fingerprint: String,
    payload_sha256: String,
}
impl SubmissionIdentity {
    pub fn target(&self) -> &MediaTarget {
        &self.target
    }
    pub fn hashes(&self) -> &[String] {
        &self.hashes
    }
    pub fn payload_sha256(&self) -> &str {
        &self.payload_sha256
    }
    fn validate(&self, settings: &ProviderSettings) -> Result<MediaDomain> {
        let domain = target_domain(&self.target)?;
        scope(settings, domain)?;
        if self.version != 1
            || self.hashes.is_empty()
            || self.hashes.len() > 2
            || self.hashes.iter().any(|h| hash(h).as_ref() != Ok(h))
            || self.hashes.iter().filter(|h| h.len() == 40).count() > 1
            || self.hashes.iter().filter(|h| h.len() == 64).count() > 1
            || self.payload_sha256.len() != 64
            || hash(&self.payload_sha256).as_ref() != Ok(&self.payload_sha256)
        {
            return Err(QbitError::InvalidRequest);
        }
        if self.settings_fingerprint != settings_fingerprint(settings)? {
            return Err(QbitError::ScopeConflict);
        }
        Ok(domain)
    }
}
fn settings_fingerprint(settings: &ProviderSettings) -> Result<String> {
    let bytes = serde_json::to_vec(settings).map_err(|_| QbitError::InvalidRequest)?;
    Ok(digest_hex(&ring::digest::SHA256, &bytes))
}
/// Presence only: neither variant proves option follow-ups completed or permits retrying add.
#[derive(Debug, PartialEq, Eq)]
pub enum Reconciliation {
    Observed { hash: String },
    NotObserved,
}
pub async fn reconcile(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
    identity: &SubmissionIdentity,
) -> Result<Reconciliation> {
    let domain = identity.validate(settings)?;
    let mut session = Session::connect(operation, settings, credentials).await?;
    Ok(
        match session.find_identities(domain, &identity.hashes).await? {
            Some(hash) => Reconciliation::Observed { hash },
            None => Reconciliation::NotObserved,
        },
    )
}
pub async fn prepare(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    target: MediaTarget,
    source: AddSource,
    recent: bool,
    options: QbitOptions,
) -> Result<PreparedDownload> {
    let domain = target_domain(&target)?;
    scope(settings, domain)?;
    if options.tags.len() > 64
        || options.tags.iter().any(|tag| {
            tag.is_empty()
                || tag.trim() != tag
                || tag.len() > 64
                || tag.contains(',')
                || tag.chars().any(char::is_control)
        })
        || options.tags.iter().collect::<BTreeSet<_>>().len() != options.tags.len()
    {
        return Err(QbitError::InvalidRequest);
    }
    if options
        .ratio_limit
        .is_some_and(|n| !n.is_finite() || n > 1_000_000.0 || (n < 0.0 && n != -1.0 && n != -2.0))
        || [options.seeding_minutes, options.inactive_seeding_minutes]
            .into_iter()
            .flatten()
            .any(|n| !(-2..=i32::MAX as i64).contains(&n))
    {
        return Err(QbitError::InvalidRequest);
    }
    let source = match source {
        AddSource::Url(value) => {
            if value.len() > 16384 || value.chars().any(char::is_control) {
                return Err(QbitError::InvalidRequest);
            }
            let mut url = url::Url::parse(&value).map_err(|_| QbitError::InvalidRequest)?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.fragment().is_some()
            {
                return Err(QbitError::InvalidRequest);
            }
            let query = url
                .query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect::<Vec<_>>();
            url.set_query(None);
            let bytes = operation.get(url.as_str(), &query).await?;
            AddSource::Torrent {
                filename: "download.torrent".into(),
                bytes,
            }
        }
        source => source,
    };
    let identities = match &source {
        AddSource::Magnet(value) => magnet_hashes(value)?,
        AddSource::Torrent { filename, bytes } => {
            if filename.is_empty()
                || filename.len() > 128
                || filename.contains(['/', '\\', '"'])
                || filename.chars().any(char::is_control)
            {
                return Err(QbitError::InvalidRequest);
            }
            torrent_hashes(bytes)?
        }
        AddSource::Url(_) => return Err(QbitError::InvalidRequest),
    };
    let payload = match &source {
        AddSource::Magnet(value) => value.as_bytes(),
        AddSource::Torrent { bytes, .. } => bytes,
        AddSource::Url(_) => return Err(QbitError::InvalidRequest),
    };
    let identity = SubmissionIdentity {
        version: 1,
        target,
        hashes: identities,
        settings_fingerprint: settings_fingerprint(settings)?,
        payload_sha256: digest_hex(&ring::digest::SHA256, payload),
    };
    Ok(PreparedDownload {
        identity,
        source,
        recent,
        options,
    })
}
/// Consumes one prepared value. This does not prove the caller committed its journal,
/// and callers must not prepare again to retry an ambiguous submission.
pub async fn submit(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    credentials: Option<&Credentials>,
    prepared: PreparedDownload,
) -> Result<AddOutcome> {
    let domain = prepared.identity.validate(settings)?;
    let configured = scope(settings, domain)?;
    let PreparedDownload {
        identity,
        source,
        recent,
        options,
    } = prepared;
    let identities = identity.hashes;
    let mut session = Session::connect(operation, settings, credentials).await?;
    let first = if recent {
        configured.recent_priority
    } else {
        configured.older_priority
    } == 1;
    if (configured.add_tags && !options.tags.is_empty() && session.version < Version(2, 3, 0))
        || (session.legacy && session.version < Version(1, 7, 0))
        || (session.legacy
            && session.version < Version(1, 14, 0)
            && matches!(configured.initial_state, InitialState::Stopped))
        || (session.legacy
            && session.version < Version(1, 16, 0)
            && (!matches!(configured.content_layout, ContentLayout::Default)
                || configured.sequential_order
                || configured.first_last_first))
        || (matches!(configured.content_layout, ContentLayout::Original)
            && session.version < Version(2, 7, 0))
        || ((options.ratio_limit.is_some() || options.seeding_minutes.is_some())
            && session.version < Version(2, 0, 1))
        || (options.inactive_seeding_minutes.is_some() && session.version < Version(2, 9, 2))
        || (identities.iter().any(|id| id.len() == 64) && session.version < Version(2, 8, 4))
    {
        return Err(QbitError::UnsupportedFeature);
    }
    session.validate_scope_name(&configured.category)?;
    if let Some(remote) = session.find_identities(domain, &identities).await? {
        return Ok(AddOutcome::AlreadyPresent { hash: remote });
    }
    let preferences = session
        .get("api/v2/app/preferences", "query/preferences", &[])
        .await?;
    if first && preferences.get("queueing_enabled").and_then(Value::as_bool) != Some(true) {
        return Err(QbitError::UnsupportedFeature);
    }
    if let AddSource::Magnet(value) = &source {
        let url = url::Url::parse(value).map_err(|_| QbitError::InvalidRequest)?;
        if !url.query_pairs().any(|(k, v)| k == "tr" && !v.is_empty())
            && preferences.get("dht").and_then(Value::as_bool) != Some(true)
        {
            return Err(QbitError::Rejected);
        }
    }
    session.ensure_category(&configured.category).await?;
    let mut fields = vec![(session.scope_field().into(), configured.category.clone())];
    if !session.legacy || session.version >= Version(1, 14, 0) {
        fields.push((
            if session.version >= Version(2, 11, 0) {
                "stopped"
            } else {
                "paused"
            }
            .into(),
            matches!(configured.initial_state, InitialState::Stopped).to_string(),
        ));
    }
    if !session.legacy || session.version >= Version(1, 16, 0) {
        fields.push((
            "sequentialDownload".into(),
            configured.sequential_order.to_string(),
        ));
        fields.push((
            "firstLastPiecePrio".into(),
            configured.first_last_first.to_string(),
        ));
    }
    if !matches!(configured.content_layout, ContentLayout::Default) {
        if session.version < Version(2, 7, 0) {
            fields.push(("root_folder".into(), "true".into()));
        } else {
            fields.push((
                "contentLayout".into(),
                if matches!(configured.content_layout, ContentLayout::Original) {
                    "Original"
                } else {
                    "Subfolder"
                }
                .into(),
            ));
        }
    }
    if configured.add_tags && !options.tags.is_empty() && session.version >= Version(2, 6, 2) {
        fields.push(("tags".into(), options.tags.join(",")))
    }
    if session.version >= Version(2, 8, 1) {
        if let Some(ratio) = options.ratio_limit {
            fields.push(("ratioLimit".into(), ratio.to_string()))
        }
        if let Some(minutes) = options.seeding_minutes {
            fields.push(("seedingTimeLimit".into(), minutes.to_string()))
        }
    }
    let (legacy, body) = match source {
        AddSource::Magnet(value) => {
            fields.push(("urls".into(), value));
            ("command/download", HttpRequestBody::Form(fields))
        }
        AddSource::Torrent { filename, bytes } => (
            "command/upload",
            HttpRequestBody::Multipart {
                fields,
                field_name: "torrents".into(),
                file_name: filename,
                file: bytes,
            },
        ),
        AddSource::Url(_) => return Err(QbitError::InvalidRequest),
    };
    // Category creation precedes this final collision check. An observed foreign hash is never reclassified.
    if let Some(remote) = session.find_identities(domain, &identities).await? {
        return Ok(AddOutcome::AlreadyPresent { hash: remote });
    }
    session.mutate("api/v2/torrents/add", legacy, body).await?;
    let Some(remote) = session
        .find_identities(domain, &identities)
        .await
        .map_err(|_| QbitError::MutationUnknown)?
    else {
        return Ok(AddOutcome::Pending { hashes: identities });
    };
    if configured.add_tags && !options.tags.is_empty() && session.version < Version(2, 6, 2) {
        session
            .find(domain, &remote)
            .await
            .map_err(|_| QbitError::MutationUnknown)?
            .ok_or(QbitError::MutationUnknown)?;
        session
            .mutate(
                "api/v2/torrents/addTags",
                "",
                HttpRequestBody::Form(vec![
                    ("hashes".into(), remote.clone()),
                    ("tags".into(), options.tags.join(",")),
                ]),
            )
            .await
            .map_err(|_| QbitError::MutationUnknown)?;
    }
    // All follow-up steps are explicit, bounded and non-retried. Failure after add requires reconciliation.
    let mut followups: Vec<(&str, &str, Vec<(String, String)>)> = Vec::new();
    if first {
        followups.push((
            "api/v2/torrents/topPrio",
            "command/topPrio",
            vec![("hashes".into(), remote.clone())],
        ))
    }
    if matches!(configured.initial_state, InitialState::Forced) {
        followups.push((
            "api/v2/torrents/setForceStart",
            "command/setForceStart",
            vec![
                ("hashes".into(), remote.clone()),
                ("value".into(), "true".into()),
            ],
        ))
    }
    if (session.version < Version(2, 8, 1)
        && (options.ratio_limit.is_some() || options.seeding_minutes.is_some()))
        || options.inactive_seeding_minutes.is_some()
    {
        let mut args = vec![
            ("hashes".into(), remote.clone()),
            (
                "ratioLimit".into(),
                options.ratio_limit.unwrap_or(-2.0).to_string(),
            ),
            (
                "seedingTimeLimit".into(),
                options.seeding_minutes.unwrap_or(-2).to_string(),
            ),
        ];
        if let Some(minutes) = options.inactive_seeding_minutes {
            args.push(("inactiveSeedingTimeLimit".into(), minutes.to_string()))
        }
        followups.push(("api/v2/torrents/setShareLimits", "", args));
    }
    for (modern, legacy, args) in followups {
        session
            .find(domain, &remote)
            .await
            .map_err(|_| QbitError::MutationUnknown)?
            .ok_or(QbitError::MutationUnknown)?;
        let action = match modern {
            "api/v2/torrents/topPrio" => Control::Priority { first: true },
            "api/v2/torrents/setForceStart" => Control::ForceStart { enabled: true },
            _ => Control::ShareLimits {
                ratio: options.ratio_limit.unwrap_or(-2.0),
                seeding_minutes: options.seeding_minutes.unwrap_or(-2),
                inactive_seeding_minutes: options.inactive_seeding_minutes,
            },
        };
        session
            .mutate(modern, legacy, HttpRequestBody::Form(args))
            .await
            .map_err(|_| QbitError::MutationUnknown)?;
        let after = session
            .find(domain, &remote)
            .await
            .map_err(|_| QbitError::MutationUnknown)?
            .ok_or(QbitError::MutationUnknown)?;
        session
            .verify_control(domain, &remote, &action, &after)
            .await
            .map_err(|_| QbitError::MutationUnknown)?;
    }
    let after = session
        .find(domain, &remote)
        .await
        .map_err(|_| QbitError::MutationUnknown)?
        .ok_or(QbitError::MutationUnknown)?;
    if configured.add_tags && !options.tags.is_empty() {
        verify_tags(&after, &options.tags).map_err(|_| QbitError::MutationUnknown)?;
    }
    if matches!(configured.initial_state, InitialState::Started)
        && !after
            .get("state")
            .and_then(Value::as_str)
            .is_some_and(observed_running)
    {
        return Err(QbitError::MutationUnknown);
    }
    if after.get("seq_dl").and_then(Value::as_bool) != Some(configured.sequential_order)
        || after.get("f_l_piece_prio").and_then(Value::as_bool) != Some(configured.first_last_first)
        || options
            .ratio_limit
            .is_some_and(|n| after.get("ratio_limit").and_then(Value::as_f64) != Some(n))
        || options
            .seeding_minutes
            .is_some_and(|n| after.get("seeding_time_limit").and_then(Value::as_i64) != Some(n))
        || options.inactive_seeding_minutes.is_some_and(|n| {
            after
                .get("inactive_seeding_time_limit")
                .and_then(Value::as_i64)
                != Some(n)
        })
        || (matches!(configured.initial_state, InitialState::Stopped)
            && !after
                .get("state")
                .and_then(Value::as_str)
                .is_some_and(|s| matches!(s, "pausedDL" | "pausedUP" | "stoppedDL" | "stoppedUP")))
    {
        return Err(QbitError::MutationUnknown);
    }
    Ok(AddOutcome::Observed { hash: remote })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_seed_provenance_keeps_absence_distinct_from_unavailable_and_effective() {
        let torrent = serde_json::json!({"state":"pausedUP","ratio":1.6,"ratio_limit":1.5});
        let prefs = serde_json::json!({"max_ratio_enabled":true,"max_ratio":2.0,"max_ratio_act":0});
        for api in 6..=17 {
            let version = Version(1, api, 0);
            let mut prefs = prefs.clone();
            if api >= 16 {
                prefs["max_seeding_time_enabled"] = serde_json::json!(false);
                prefs["max_seeding_time"] = serde_json::json!(-1);
            }
            let result =
                seed_policy(version, &torrent, &serde_json::json!({}), &prefs, true, 100).unwrap();
            assert_eq!(result.ratio.raw, SeedLimit::Unknown);
            assert_eq!(result.ratio.provenance, SeedProvenance::ObservedEffective);
            assert_eq!(result.ready, Some(true));
            let mut missing = torrent.clone();
            missing.as_object_mut().unwrap().remove("ratio_limit");
            let result =
                seed_policy(version, &missing, &serde_json::json!({}), &prefs, true, 100).unwrap();
            assert_eq!(result.ratio.provenance, SeedProvenance::Unknown);
            assert_eq!(result.ready, None);
            for invalid in [
                serde_json::Value::Null,
                serde_json::json!(-2),
                serde_json::json!("1.5"),
            ] {
                missing["ratio_limit"] = invalid;
                assert!(matches!(
                    seed_policy(version, &missing, &serde_json::json!({}), &prefs, true, 100),
                    Err(QbitError::InvalidResponse)
                ));
            }
            prefs["max_inactive_seeding_time_enabled"] = serde_json::json!(false);
            assert_eq!(
                removes_completed_downloads(version, &prefs),
                Err(QbitError::InvalidResponse)
            );
        }
        assert!(!legacy_seed_profile(Version(1, 18, 0)));
        assert_eq!(
            validate_retention(Version(2, 0, 0), &prefs),
            Err(QbitError::InvalidResponse)
        );
    }

    #[test]
    fn remote_roots_are_host_independent_and_locality_is_only_endpoint_evidence() {
        for (base, category, expected) in [
            ("/root", "child", "/root/child"),
            ("/", "child", "/child"),
            (r"C:\downloads", r"movies\new", r"C:\downloads\movies\new"),
            ("C:/downloads", "movies/new", "C:/downloads/movies/new"),
            ("/root", r"D:\films", r"D:\films"),
            ("/root", "//server/share", "//server/share"),
            ("/root", r"\\server\share", r"\\server\share"),
        ] {
            assert_eq!(
                remote_root(base, Some(category)).unwrap(),
                (expected.into(), RootSource::Category)
            );
        }
        for invalid in [
            "../escape",
            "C:relative",
            "//server",
            "\\relative",
            "bad\0path",
            "./child",
        ] {
            assert!(remote_root("/root", Some(invalid)).is_err());
        }
        assert!(remote_root("relative", None).is_err());
        assert!(remote_root(&format!("/{}", "x".repeat(4096)), None).is_err());
        assert!(remote_root(&format!("/{}", "x".repeat(4090)), Some("123456")).is_err());
        assert!(remote_root("bad\0default", None).is_err());
        assert!(remote_root("/root", Some(&"x".repeat(4097))).is_err());
        for endpoint in [
            "http://127.0.0.1:1",
            "http://[::1]:1",
            "http://localhost:1",
            "http://localhost.:1",
        ] {
            assert_eq!(
                endpoint_locality(endpoint).unwrap(),
                EndpointLocality::Loopback
            );
        }
        for endpoint in ["http://192.0.2.1:1", "http://example.invalid"] {
            assert_eq!(
                endpoint_locality(endpoint).unwrap(),
                EndpointLocality::Unknown
            );
        }
    }

    #[test]
    fn retention_validation_preserves_threshold_and_required_facts() {
        for action in 0..=3 {
            for (ratio, enabled, minutes, risk) in [
                (true, false, -1, true),
                (false, true, 20159, true),
                (false, true, 20160, false),
                (false, false, 0, false),
                (false, true, -1, true),
            ] {
                let prefs = serde_json::json!({"max_ratio_enabled":ratio,
                    "max_seeding_time_enabled":enabled,"max_seeding_time":minutes,
                    "max_ratio_act":action});
                assert_eq!(
                    validate_retention(Version(2, 11, 0), &prefs),
                    if risk && matches!(action, 1 | 3) {
                        Err(QbitError::UnsafeRetention)
                    } else {
                        Ok(())
                    }
                );
            }
        }
        let valid = serde_json::json!({"max_ratio_enabled":false,
            "max_seeding_time_enabled":true,"max_seeding_time":20160,"max_ratio_act":1});
        for key in [
            "max_ratio_enabled",
            "max_seeding_time_enabled",
            "max_seeding_time",
            "max_ratio_act",
        ] {
            let mut invalid = valid.clone();
            invalid.as_object_mut().unwrap().remove(key);
            assert_eq!(
                validate_retention(Version(2, 11, 0), &invalid),
                Err(QbitError::InvalidResponse)
            );
            invalid[key] = serde_json::Value::Null;
            assert_eq!(
                validate_retention(Version(2, 11, 0), &invalid),
                Err(QbitError::InvalidResponse)
            );
        }
    }

    #[test]
    fn seed_limits_preserve_unknowns_units_boundaries_and_policy_separation() {
        let base = serde_json::json!({"state":"pausedUP","ratio":1.9995,"ratio_limit":2.0,"seeding_time_limit":-1,"inactive_seeding_time_limit":-1,"seeding_time":3599,"last_activity":1000});
        let prefs = serde_json::json!({"max_ratio_enabled":true,"max_ratio":3.0,"max_seeding_time_enabled":true,"max_seeding_time":60,"max_inactive_seeding_time_enabled":false,"max_inactive_seeding_time":-1,"max_ratio_act":3});
        let evaluate = |torrent: &Value, global: &Value, now| {
            seed_policy(
                Version(2, 11, 0),
                torrent,
                &serde_json::json!({}),
                global,
                true,
                now,
            )
            .unwrap()
        };
        let result = evaluate(&base, &prefs, 4600);
        assert_eq!(result.ready, Some(true));
        assert_eq!(result.client_removes_on_limit(), Some(true));
        let mut torrent = base.clone();
        torrent["ratio"] = serde_json::json!(1.998);
        assert_eq!(evaluate(&torrent, &prefs, 4600).ready, Some(false));
        torrent["ratio_limit"] = serde_json::json!(-1);
        torrent["seeding_time_limit"] = serde_json::json!(-2);
        assert_eq!(
            evaluate(&torrent, &prefs, 4600).seeding_minutes.effective,
            SeedLimit::Limited(60)
        );
        assert_eq!(evaluate(&torrent, &prefs, 4600).ready, Some(false));
        torrent["seeding_time"] = serde_json::json!(3600);
        assert_eq!(evaluate(&torrent, &prefs, 4600).ready, Some(true));
        // An explicit unlimited axis overrides enabled global policy.
        torrent["seeding_time_limit"] = serde_json::json!(-1);
        assert_eq!(evaluate(&torrent, &prefs, 4600).ready, Some(false));
        torrent["inactive_seeding_time_limit"] = serde_json::json!(60);
        assert_eq!(evaluate(&torrent, &prefs, 4600).ready, Some(false));
        assert_eq!(evaluate(&torrent, &prefs, 4601).ready, Some(true));
        for timestamp in [serde_json::json!(0), serde_json::json!(5000)] {
            torrent["last_activity"] = timestamp;
            assert_eq!(evaluate(&torrent, &prefs, 4600).ready, None);
        }
        torrent.as_object_mut().unwrap().remove("last_activity");
        assert_eq!(evaluate(&torrent, &prefs, 4600).ready, None);
        // Any known reached axis wins OR even when another axis is unknown; zero is active.
        torrent["ratio_limit"] = serde_json::json!(0);
        assert_eq!(evaluate(&torrent, &prefs, 4600).ready, Some(true));
        for state in ["uploading", "forcedUP", "checkingUP", "downloading"] {
            torrent["state"] = serde_json::json!(state);
            assert_eq!(evaluate(&torrent, &prefs, 4600).ready, Some(false));
        }
        torrent["state"] = serde_json::json!("stoppedUP");
        assert_eq!(
            seed_policy(
                Version(2, 11, 0),
                &torrent,
                &serde_json::json!({}),
                &prefs,
                false,
                4600
            )
            .unwrap()
            .ready,
            Some(false)
        );
        torrent["ratio_limit"] = serde_json::json!(-2);
        torrent["inactive_seeding_time_limit"] = serde_json::json!(-1);
        let mut global = prefs.clone();
        global.as_object_mut().unwrap().remove("max_ratio_enabled");
        assert_eq!(evaluate(&torrent, &global, 4600).ready, None);
        global["max_ratio_enabled"] = serde_json::json!(false);
        global.as_object_mut().unwrap().remove("max_ratio");
        assert_eq!(evaluate(&torrent, &global, 4600).ready, Some(false));
        global["max_ratio_enabled"] = serde_json::json!(true);
        assert_eq!(evaluate(&torrent, &global, 4600).ready, None);
        torrent["ratio_limit"] = serde_json::json!(-1);
        torrent["seeding_time_limit"] = serde_json::json!(60);
        torrent.as_object_mut().unwrap().remove("seeding_time");
        assert_eq!(
            seed_policy(
                Version(2, 11, 0),
                &torrent,
                &serde_json::json!({"seeding_time":3600}),
                &prefs,
                true,
                4600
            )
            .unwrap()
            .ready,
            Some(true)
        );
        assert_eq!(
            seed_policy(
                Version(2, 11, 0),
                &torrent,
                &serde_json::json!({"seeding_time":-1}),
                &prefs,
                true,
                4600
            )
            .unwrap()
            .ready,
            None
        );
        for bad in [
            serde_json::json!(-3),
            serde_json::json!(1.5),
            Value::Null,
            serde_json::json!("60"),
            serde_json::json!(i64::MAX),
        ] {
            torrent["seeding_time_limit"] = bad;
            assert!(matches!(
                seed_policy(
                    Version(2, 11, 0),
                    &torrent,
                    &serde_json::json!({}),
                    &prefs,
                    true,
                    4600
                ),
                Err(QbitError::InvalidResponse)
            ));
        }
        torrent = base.clone();
        let mut bad = prefs.clone();
        bad["max_ratio_enabled"] = serde_json::json!(1);
        assert!(
            seed_policy(
                Version(2, 11, 0),
                &torrent,
                &serde_json::json!({}),
                &bad,
                true,
                4600
            )
            .is_err()
        );
    }
    #[test]
    fn optional_seeding_time_preserves_safe_integer_contract() {
        let client = super::super::http::HttpClient::new().unwrap();
        let operation = client.operation(uuid::Uuid::new_v4()).unwrap();
        let settings = ProviderSettings::Qbittorrent {
            endpoint: String::new(),
            tv: None,
            movies: None,
        };
        let session = Session {
            operation: &operation,
            settings: &settings,
            credentials: None,
            headers: Vec::new(),
            observed_sids: Vec::new(),
            version: Version(2, 11, 0),
            legacy: false,
        };
        let mut value = serde_json::json!({"hash":"1111111111111111111111111111111111111111","category":"tv","name":"item","state":"downloading","progress":0.5,"size":100,"amount_left":50,"dlspeed":1,"upspeed":0,"ratio":0.0});
        assert_eq!(
            session
                .item(&value, MediaDomain::Tv)
                .unwrap()
                .seeding_seconds,
            None
        );
        value["seeding_time"] = serde_json::json!(9_007_199_254_740_991u64);
        assert_eq!(
            session
                .item(&value, MediaDomain::Tv)
                .unwrap()
                .seeding_seconds,
            Some(9_007_199_254_740_991)
        );
        for invalid in [
            serde_json::json!(9_007_199_254_740_992u64),
            serde_json::json!(-1),
            serde_json::json!(1.5),
            serde_json::json!("60"),
            Value::Null,
        ] {
            value["seeding_time"] = invalid;
            assert!(matches!(
                session.item(&value, MediaDomain::Tv),
                Err(QbitError::InvalidResponse)
            ));
        }
    }
    #[test]
    fn torrent_and_magnet_identity_are_original_bytes_not_guessed_names() {
        let info = b"d6:lengthi1e4:name1:x12:piece lengthi16384e6:pieces20:12345678901234567890e";
        let mut torrent = b"d4:info".to_vec();
        torrent.extend(info);
        torrent.push(b'e');
        assert_eq!(
            torrent_hashes(&torrent).unwrap(),
            vec![digest_hex(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, info)]
        );
        let v2 = b"d9:file treede12:meta versioni2e4:name1:x12:piece lengthi16384ee";
        let mut torrent = b"d4:info".to_vec();
        torrent.extend(v2);
        torrent.push(b'e');
        assert_eq!(
            torrent_hashes(&torrent).unwrap(),
            vec![digest_hex(&ring::digest::SHA256, v2)]
        );
        assert_eq!(
            magnet_hashes("magnet:?xt=urn:btih:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
            vec!["0".repeat(40)]
        );
        let hybrid = format!(
            "magnet:?xt=urn:btih:{}&xt=urn:btmh:1220{}",
            "1".repeat(40),
            "2".repeat(64)
        );
        assert_eq!(magnet_hashes(&hybrid).unwrap().len(), 2);
        assert!(
            magnet_hashes(&format!(
                "magnet:?xt=urn:btih:{}&xt=urn:btih:{}",
                "1".repeat(40),
                "2".repeat(40)
            ))
            .is_err()
        );
        for bad in [
            b"d4:infode4:infodee".as_slice(),
            b"d4:infoi1ee",
            b"d4:infod6:pieces0:eejunk",
            b"d4:infod6:pieces03:abcee",
        ] {
            assert!(torrent_hashes(bad).is_err())
        }
        let mut deep = b"d4:info".to_vec();
        deep.extend(vec![b'l'; 40]);
        deep.extend(vec![b'e'; 41]);
        assert!(torrent_hashes(&deep).is_err());
    }
}
