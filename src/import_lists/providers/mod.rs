//! Concrete list sources. Each module owns one implementation: its settings schema and a bounded
//! `fetch` that returns the complete list or fails (never a partial list, never silent truncation).
//! All network I/O goes through [`Transport`]: http(s) only, no redirects, no ambient proxy, bounded
//! time and response size, and no URL, header or body text in errors.
use super::model::{Field, FieldKind, ListItem, MAX_ITEMS, SettingValue};
use crate::{
    api::MediaDomain,
    notifications::transport::{Body, Method, Outgoing, SendError, Transport},
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

pub mod anilist;
pub mod arr;
pub mod custom;
pub mod imdb;
pub mod mal;
pub mod plex;
pub mod rss;
pub mod simkl;
pub mod stevenlu;
pub mod tmdb;
pub mod trakt;

/// Largest list document accepted from a remote service.
pub const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;
/// Most requests one fetch may issue (paginated sources stop here and fail with `resource_limit`).
pub const MAX_REQUESTS: usize = 25;
const MAX_SAFE: i64 = 9_007_199_254_740_991;

/// Stable, secret-free failure class. The string is stored in `last_error` and run `error_code`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchError {
    InvalidSettings,
    Authentication,
    RateLimited,
    Timeout,
    Transport,
    Server,
    Rejected,
    Redirect,
    HostDenied,
    InvalidResponse,
    ResourceLimit,
    Unsupported,
}
impl FetchError {
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidSettings => "invalid_settings",
            Self::Authentication => "authentication",
            Self::RateLimited => "rate_limited",
            Self::Timeout => "timeout",
            Self::Transport => "transport",
            Self::Server => "server_error",
            Self::Rejected => "rejected",
            Self::Redirect => "redirect",
            Self::HostDenied => "host_denied",
            Self::InvalidResponse => "invalid_response",
            Self::ResourceLimit => "resource_limit",
            Self::Unsupported => "unsupported",
        }
    }
    /// Whether another attempt can plausibly succeed without a configuration change.
    pub fn retryable(self) -> bool {
        matches!(
            self,
            Self::RateLimited | Self::Timeout | Self::Transport | Self::Server
        )
    }
}
impl From<SendError> for FetchError {
    fn from(e: SendError) -> Self {
        match e {
            SendError::InvalidRequest => Self::InvalidSettings,
            SendError::PolicyDenied => Self::HostDenied,
            SendError::Timeout => Self::Timeout,
            SendError::Transport => Self::Transport,
            SendError::Authentication => Self::Authentication,
            SendError::RateLimited(_) => Self::RateLimited,
            SendError::Rejected => Self::Rejected,
            SendError::Server => Self::Server,
            SendError::Redirect => Self::Redirect,
            SendError::TooLarge => Self::ResourceLimit,
        }
    }
}
pub type Fetched = Result<Vec<ListItem>, FetchError>;

/// A stored list with decrypted secrets, for fetching only; never serialized or logged.
pub struct ListRuntime {
    pub id: Uuid,
    pub media: MediaDomain,
    pub implementation: String,
    pub settings: BTreeMap<String, SettingValue>,
    pub secrets: BTreeMap<String, String>,
}
impl ListRuntime {
    pub fn text(&self, name: &str) -> Option<&str> {
        match self.settings.get(name) {
            Some(SettingValue::Text(t)) if !t.is_empty() => Some(t),
            _ => None,
        }
    }
    pub fn int(&self, name: &str) -> Option<i64> {
        match self.settings.get(name) {
            Some(SettingValue::Int(n)) => Some(*n),
            _ => None,
        }
    }
    pub fn flag(&self, name: &str) -> bool {
        matches!(self.settings.get(name), Some(SettingValue::Bool(true)))
    }
    pub fn secret(&self, name: &str) -> Option<&str> {
        self.secrets
            .get(name)
            .map(String::as_str)
            .filter(|s| !s.is_empty())
    }
    /// Setting or secret required for the request; absence is a configuration failure.
    pub fn need_text(&self, name: &str) -> Result<&str, FetchError> {
        self.text(name).ok_or(FetchError::InvalidSettings)
    }
    pub fn need_secret(&self, name: &str) -> Result<&str, FetchError> {
        self.secret(name).ok_or(FetchError::InvalidSettings)
    }
}

/// One registered implementation. `available` is false only while a provider is unimplemented, and
/// unavailable implementations are hidden from the schema and rejected on create.
pub struct Implementation {
    pub name: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub domains: &'static [MediaDomain],
    pub default_interval_minutes: i64,
    pub min_interval_minutes: i64,
    pub fields: fn() -> Vec<Field>,
    pub available: bool,
}

pub fn field(
    name: &'static str,
    label: &'static str,
    kind: FieldKind,
    help: &'static str,
) -> Field {
    Field {
        name,
        label,
        kind,
        required: false,
        secret: false,
        help,
        default: None,
        options: Vec::new(),
        min: None,
        max: None,
        max_length: None,
    }
}
impl Field {
    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }
    pub fn secret(mut self) -> Self {
        self.secret = true;
        self.max_length = Some(1024);
        self
    }
    pub fn with_default(mut self, value: SettingValue) -> Self {
        self.default = Some(value);
        self
    }
    pub fn options(mut self, options: &[&'static str]) -> Self {
        self.options = options.to_vec();
        self
    }
    pub fn range(mut self, min: i64, max: i64) -> Self {
        self.min = Some(min);
        self.max = Some(max);
        self
    }
    pub fn max_length(mut self, max: u32) -> Self {
        self.max_length = Some(max);
        self
    }
}
pub fn text_default(value: &str) -> SettingValue {
    SettingValue::Text(value.to_owned())
}

/// Percent-encodes one URL path segment or query component.
pub fn encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

/// Joins a configured base URL and a path, preserving a URL-base prefix and trimming the trailing
/// slash, so `http://host/sonarr/` + `api/v3/series` never drops `/sonarr`.
pub fn join(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

/// One bounded GET. Returns the body only for a success status.
pub async fn get(
    transport: &Transport,
    url: String,
    headers: &[(&str, &str)],
) -> Result<Vec<u8>, FetchError> {
    let mut out = Outgoing::get(url).header(
        "accept",
        "application/json, application/xml, text/xml, text/csv, */*",
    );
    for (name, value) in headers {
        out = out.header(name, *value);
    }
    Ok(transport.send_limited(&out, MAX_BODY_BYTES).await?.body)
}
pub async fn get_json(
    transport: &Transport,
    url: String,
    headers: &[(&str, &str)],
) -> Result<Value, FetchError> {
    let mut out = Outgoing::get(url).header("accept", "application/json");
    for (name, value) in headers {
        out = out.header(name, *value);
    }
    let body = transport.send_limited(&out, MAX_BODY_BYTES).await?.body;
    serde_json::from_slice(&body).map_err(|_| FetchError::InvalidResponse)
}
pub async fn post_json(
    transport: &Transport,
    url: String,
    headers: &[(&str, &str)],
    body: Value,
) -> Result<Value, FetchError> {
    let mut out = Outgoing {
        method: Method::Post,
        url,
        headers: vec![("accept".into(), "application/json".into())],
        body: Body::Json(body),
    };
    for (name, value) in headers {
        out = out.header(name, *value);
    }
    let body = transport.send_limited(&out, MAX_BODY_BYTES).await?.body;
    serde_json::from_slice(&body).map_err(|_| FetchError::InvalidResponse)
}

/// A JSON integer identity, accepting only positive safe integers; anything else is a corrupt
/// document rather than a missing value.
pub fn id_of(value: Option<&Value>) -> Result<Option<i64>, FetchError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => match n.as_i64() {
            Some(id) if (1..=MAX_SAFE).contains(&id) => Ok(Some(id)),
            _ => Err(FetchError::InvalidResponse),
        },
        // Some services (IMDb-style exports, Plex GUIDs) carry numeric ids as strings.
        Some(Value::String(s)) => match s.trim().parse::<i64>() {
            Ok(id) if (1..=MAX_SAFE).contains(&id) => Ok(Some(id)),
            _ if s.trim().is_empty() => Ok(None),
            _ => Err(FetchError::InvalidResponse),
        },
        _ => Err(FetchError::InvalidResponse),
    }
}
pub fn imdb_of(value: Option<&Value>) -> Result<Option<String>, FetchError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.is_empty() => Ok(None),
        Some(Value::String(s)) => valid_imdb(s)
            .map(|s| Some(s.to_owned()))
            .ok_or(FetchError::InvalidResponse),
        _ => Err(FetchError::InvalidResponse),
    }
}
pub fn valid_imdb(s: &str) -> Option<&str> {
    (s.len() >= 3
        && s.len() <= 32
        && s.starts_with("tt")
        && s[2..].bytes().all(|b| b.is_ascii_digit()))
    .then_some(s)
}
pub fn year_of(value: Option<&Value>) -> Result<Option<i64>, FetchError> {
    match id_of(value)? {
        Some(y) if (1..=9999).contains(&y) => Ok(Some(y)),
        Some(_) => Err(FetchError::InvalidResponse),
        None => Ok(None),
    }
}
pub fn title_of(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => clean_title(s),
        _ => String::new(),
    }
}
/// Control characters removed, whitespace collapsed, bounded to 1024 bytes on a char boundary.
pub fn clean_title(raw: &str) -> String {
    let mut out = raw
        .chars()
        .filter(|c| !c.is_control() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    while out.len() > 1024 {
        out.pop();
    }
    out
}

/// Normalises a provider's items: drops entries that carry no usable identity at all, removes
/// duplicates by canonical key (first occurrence wins), and enforces the item cap. Exceeding the cap
/// fails the whole fetch.
pub fn finish(media: MediaDomain, items: Vec<ListItem>) -> Fetched {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for item in items {
        if item.year.is_some_and(|year| !(1..=9999).contains(&year)) {
            return Err(FetchError::InvalidResponse);
        }
        let usable = item.tvdb_id.is_some()
            || item.tmdb_id.is_some()
            || item.imdb_id.is_some()
            || !item.title.is_empty();
        if !usable {
            continue;
        }
        let mut item = item;
        if item.title.is_empty() {
            item.title = item
                .imdb_id
                .clone()
                .unwrap_or_else(|| format!("untitled {}", out.len() + 1));
        }
        if !seen.insert(item_key(media, &item)) {
            continue;
        }
        if out.len() >= MAX_ITEMS {
            return Err(FetchError::ResourceLimit);
        }
        out.push(item);
    }
    Ok(out)
}

/// Canonical identity of an item inside its list. A TV list prefers the TVDB id and a movie list the
/// TMDb id; ids that belong to the other domain are not an identity for this one.
pub fn item_key(media: MediaDomain, item: &ListItem) -> String {
    let primary = match media {
        MediaDomain::Tv => item.tvdb_id.map(|id| format!("tvdb:{id}")),
        MediaDomain::Movies => item.tmdb_id.map(|id| format!("tmdb:{id}")),
    };
    primary
        .or_else(|| item.imdb_id.as_ref().map(|id| format!("imdb:{id}")))
        .unwrap_or_else(|| {
            let title: String = item
                .title
                .to_lowercase()
                .chars()
                .filter(|c| c.is_alphanumeric())
                .collect();
            format!(
                "title:{}:{}",
                title.chars().take(200).collect::<String>(),
                item.year.map_or(String::new(), |y| y.to_string())
            )
        })
}

/// Splits a comma/space separated list of positive integers (profile ids, tag ids).
pub fn int_list(raw: Option<&str>) -> Result<Vec<i64>, FetchError> {
    let Some(raw) = raw else { return Ok(vec![]) };
    raw.split([',', ' '])
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse::<i64>()
                .ok()
                .filter(|n| *n >= 0)
                .ok_or(FetchError::InvalidSettings)
        })
        .collect()
}
