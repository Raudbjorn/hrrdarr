//! Bounded outbound HTTP for notifications.
//!
//! Rules mirror the provider transport (`providers::http`): http(s) only, no URL userinfo or
//! fragment, redirects never followed, no ambient proxy, bounded time and body, and URLs, headers
//! and bodies never enter errors. Notifications may legitimately target a LAN service (Gotify,
//! ntfy, a Kodi box), so loopback and private ranges stay reachable; the added host policy
//! refuses addresses that can only be an SSRF aim: link-local (cloud metadata), unspecified,
//! multicast and broadcast. The policy runs on literal hosts when a URL is validated and on every
//! resolved address at connect time, which closes DNS-rebinding to a denied range.
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::sync::Semaphore;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_CONCURRENT_SENDS: usize = 8;
pub const MAX_URL_BYTES: usize = 2048;
const MAX_HEADERS: usize = 16;
const MAX_HEADER_VALUE_BYTES: usize = 4096;
const MAX_REQUEST_BYTES: usize = 256 * 1024;
const DEFAULT_RESPONSE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Body {
    None,
    Json(serde_json::Value),
    Form(Vec<(String, String)>),
    Text {
        content_type: &'static str,
        text: String,
    },
}

/// One fully formed HTTP request. Never `Debug`-printed by the dispatcher: it carries secrets.
#[derive(Clone, PartialEq)]
pub struct Outgoing {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Body,
}
impl Outgoing {
    pub fn post_json(url: impl Into<String>, json: serde_json::Value) -> Self {
        Self {
            method: Method::Post,
            url: url.into(),
            headers: Vec::new(),
            body: Body::Json(json),
        }
    }
    pub fn post_form(url: impl Into<String>, form: Vec<(String, String)>) -> Self {
        Self {
            method: Method::Post,
            url: url.into(),
            headers: Vec::new(),
            body: Body::Form(form),
        }
    }
    pub fn get(url: impl Into<String>) -> Self {
        Self {
            method: Method::Get,
            url: url.into(),
            headers: Vec::new(),
            body: Body::None,
        }
    }
    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_owned(), value.into()));
        self
    }
    /// Stable 64-bit identity of the whole request (method, URL, headers, body), recorded once
    /// it is acknowledged so a retried delivery skips it. A digest, so no secret is stored.
    pub fn fingerprint(&self) -> String {
        let mut bytes = Vec::new();
        let mut field = |part: &[u8]| {
            bytes.extend_from_slice(&(part.len() as u64).to_be_bytes());
            bytes.extend_from_slice(part);
        };
        field(match self.method {
            Method::Get => b"GET",
            Method::Post => b"POST",
            Method::Put => b"PUT",
        });
        field(self.url.as_bytes());
        for (name, value) in &self.headers {
            field(name.as_bytes());
            field(value.as_bytes());
        }
        match &self.body {
            Body::None => field(b"none"),
            Body::Json(value) => {
                field(b"json");
                field(&serde_json::to_vec(value).unwrap_or_default());
            }
            Body::Form(pairs) => {
                field(b"form");
                for (name, value) in pairs {
                    field(name.as_bytes());
                    field(value.as_bytes());
                }
            }
            Body::Text { content_type, text } => {
                field(b"text");
                field(content_type.as_bytes());
                field(text.as_bytes());
            }
        }
        let mut hex = crate::providers::qbittorrent::digest_hex(&ring::digest::SHA256, &bytes);
        hex.truncate(16);
        hex
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendError {
    InvalidRequest,
    PolicyDenied,
    Timeout,
    Transport,
    Authentication,
    RateLimited(Option<u32>),
    /// Any other non-success status below 500.
    Rejected,
    Server,
    Redirect,
    TooLarge,
}
impl SendError {
    /// Stable class stored in the delivery journal and returned by test endpoints.
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::PolicyDenied => "host_denied",
            Self::Timeout => "timeout",
            Self::Transport => "transport",
            Self::Authentication => "authentication",
            Self::RateLimited(_) => "rate_limited",
            Self::Rejected => "rejected",
            Self::Server => "server_error",
            Self::Redirect => "redirect",
            Self::TooLarge => "response_too_large",
        }
    }
    /// Whether another attempt can plausibly succeed without a configuration change.
    pub fn retryable(self) -> bool {
        matches!(
            self,
            Self::Timeout | Self::Transport | Self::RateLimited(_) | Self::Server
        )
    }
    pub fn retry_after_seconds(self) -> Option<u32> {
        match self {
            Self::RateLimited(seconds) => seconds,
            _ => None,
        }
    }
}

pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

fn v4_denied(ip: Ipv4Addr) -> bool {
    ip.is_unspecified() || ip.is_multicast() || ip.is_broadcast() || ip.is_link_local()
}
fn v6_denied(ip: Ipv6Addr) -> bool {
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return v4_denied(mapped);
    }
    let segments = ip.segments();
    ip.is_unspecified()
        || ip.is_multicast()
        // fe80::/10 link-local and the fd00:ec2::/32 cloud metadata range.
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] == 0xfd00 && segments[1] == 0x0ec2)
}
/// Addresses a notification can never target.
pub fn ip_denied(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => v4_denied(ip),
        IpAddr::V6(ip) => v6_denied(ip),
    }
}

/// Validates a configured or constructed endpoint. Queries are allowed (webhook tokens commonly
/// live there); credentials in the authority and fragments are not.
pub fn validate_url(raw: &str) -> Result<url::Url, SendError> {
    if raw.trim() != raw
        || raw.is_empty()
        || raw.len() > MAX_URL_BYTES
        || raw.contains('\\')
        || raw.chars().any(char::is_control)
    {
        return Err(SendError::InvalidRequest);
    }
    let url = url::Url::parse(raw).map_err(|_| SendError::InvalidRequest)?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(SendError::InvalidRequest);
    }
    match url.host() {
        None => return Err(SendError::InvalidRequest),
        Some(url::Host::Ipv4(ip)) if ip_denied(IpAddr::V4(ip)) => {
            return Err(SendError::PolicyDenied);
        }
        Some(url::Host::Ipv6(ip)) if ip_denied(IpAddr::V6(ip)) => {
            return Err(SendError::PolicyDenied);
        }
        Some(_) => {}
    }
    Ok(url)
}

struct PolicyResolver;
impl reqwest::dns::Resolve for PolicyResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        Box::pin(async move {
            let host = name.as_str().to_owned();
            let found = tokio::time::timeout(RESOLVE_TIMEOUT, tokio::net::lookup_host((host, 0)))
                .await
                .map_err(|_| -> Box<dyn std::error::Error + Send + Sync> {
                    "resolution timed out".into()
                })??;
            let allowed: Vec<SocketAddr> = found.filter(|a| !ip_denied(a.ip())).collect();
            if allowed.is_empty() {
                return Err("no permitted address".into());
            }
            Ok(Box::new(allowed.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

pub struct Transport {
    client: reqwest::Client,
    gate: Arc<Semaphore>,
    /// (production origin prefix, replacement). Empty in production; lets protocol tests point
    /// fixed-endpoint providers at an in-process fake without a code path that skips policy.
    origin_map: Vec<(String, String)>,
}
impl Transport {
    pub fn new() -> Result<Self, SendError> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .referer(false)
            .dns_resolver(Arc::new(PolicyResolver))
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(Duration::from_secs(5))
            .pool_max_idle_per_host(2)
            .pool_idle_timeout(Duration::from_secs(30))
            .user_agent("hrrdarr/0.1")
            .build()
            .map_err(|_| SendError::Transport)?;
        Ok(Self {
            client,
            gate: Arc::new(Semaphore::new(MAX_CONCURRENT_SENDS)),
            origin_map: Vec::new(),
        })
    }
    /// Test seam: rewrite fixed production origins to a local fake. Not reachable from HTTP.
    #[doc(hidden)]
    pub fn with_origin_map(mut self, map: Vec<(String, String)>) -> Self {
        self.origin_map = map;
        self
    }
    fn mapped(&self, url: &str) -> String {
        for (from, to) in &self.origin_map {
            if let Some(rest) = url.strip_prefix(from.as_str()) {
                return format!("{to}{rest}");
            }
        }
        url.to_owned()
    }
    pub async fn send(&self, out: &Outgoing) -> Result<Response, SendError> {
        self.send_limited(out, DEFAULT_RESPONSE_BYTES).await
    }
    pub async fn send_limited(
        &self,
        out: &Outgoing,
        max_response: usize,
    ) -> Result<Response, SendError> {
        let url = validate_url(&self.mapped(&out.url))?;
        if out.headers.len() > MAX_HEADERS {
            return Err(SendError::InvalidRequest);
        }
        let mut headers = reqwest::header::HeaderMap::new();
        for (name, value) in &out.headers {
            let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| SendError::InvalidRequest)?;
            if matches!(
                name.as_str(),
                "host"
                    | "content-length"
                    | "content-type"
                    | "transfer-encoding"
                    | "connection"
                    | "te"
                    | "upgrade"
                    | "proxy-authorization"
                    | "proxy-connection"
                    | "expect"
                    | "accept-encoding"
            ) || name.as_str().starts_with("proxy-")
                || headers.contains_key(&name)
                || value.len() > MAX_HEADER_VALUE_BYTES
            {
                return Err(SendError::InvalidRequest);
            }
            let mut value = reqwest::header::HeaderValue::from_str(value)
                .map_err(|_| SendError::InvalidRequest)?;
            value.set_sensitive(true);
            headers.insert(name, value);
        }
        let builder = match out.method {
            Method::Get => self.client.get(url),
            Method::Post => self.client.post(url),
            Method::Put => self.client.put(url),
        };
        let builder = match &out.body {
            Body::None => builder,
            Body::Json(value) => {
                let bytes = serde_json::to_vec(value).map_err(|_| SendError::InvalidRequest)?;
                if bytes.len() > MAX_REQUEST_BYTES {
                    return Err(SendError::InvalidRequest);
                }
                builder
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(bytes)
            }
            Body::Form(fields) => {
                if fields.len() > 64 {
                    return Err(SendError::InvalidRequest);
                }
                let encoded = url::form_urlencoded::Serializer::new(String::new())
                    .extend_pairs(fields)
                    .finish();
                if encoded.len() > MAX_REQUEST_BYTES {
                    return Err(SendError::InvalidRequest);
                }
                builder
                    .header(
                        reqwest::header::CONTENT_TYPE,
                        "application/x-www-form-urlencoded",
                    )
                    .body(encoded)
            }
            Body::Text { content_type, text } => {
                if text.len() > MAX_REQUEST_BYTES {
                    return Err(SendError::InvalidRequest);
                }
                builder
                    .header(reqwest::header::CONTENT_TYPE, *content_type)
                    .body(text.clone())
            }
        };
        let request = builder
            .headers(headers)
            .header(reqwest::header::ACCEPT_ENCODING, "identity")
            .build()
            .map_err(|_| SendError::InvalidRequest)?;
        let _permit = self
            .gate
            .acquire()
            .await
            .map_err(|_| SendError::Transport)?;
        let mut response = self.client.execute(request).await.map_err(classify)?;
        let status = response.status();
        if status.is_redirection() {
            return Err(SendError::Redirect);
        }
        if status.as_u16() == 429 {
            let seconds = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u32>().ok())
                .map(|s| s.clamp(1, 86_400));
            return Err(SendError::RateLimited(seconds));
        }
        if matches!(status.as_u16(), 401 | 403) {
            return Err(SendError::Authentication);
        }
        if status.is_server_error() {
            return Err(SendError::Server);
        }
        if !status.is_success() {
            return Err(SendError::Rejected);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(classify)? {
            if chunk.len() > max_response - body.len() {
                // A notification only needs the status; an oversized body is simply discarded.
                if max_response > DEFAULT_RESPONSE_BYTES {
                    return Err(SendError::TooLarge);
                }
                break;
            }
            body.extend_from_slice(&chunk);
        }
        Ok(Response {
            status: status.as_u16(),
            body,
        })
    }
}
fn classify(error: reqwest::Error) -> SendError {
    if error.is_timeout() {
        SendError::Timeout
    } else {
        SendError::Transport
    }
}

pub use crate::providers::http::basic_auth;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fingerprint_separates_requests_and_is_stable() {
        let a = Outgoing::post_json("https://ntfy.sh", serde_json::json!({"topic":"a"}));
        let b = Outgoing::post_json("https://ntfy.sh", serde_json::json!({"topic":"b"}));
        assert_eq!(a.fingerprint(), a.clone().fingerprint());
        assert_eq!(a.fingerprint().len(), 16);
        assert_ne!(a.fingerprint(), b.fingerprint());
        assert_ne!(
            a.fingerprint(),
            a.clone().header("Authorization", "Bearer x").fingerprint()
        );
        // Length-prefixed fields: moving bytes between URL and header cannot collide.
        assert_ne!(
            Outgoing::get("http://h/ab").header("c", "d").fingerprint(),
            Outgoing::get("http://h/a").header("bc", "d").fingerprint()
        );
    }
    #[test]
    fn basic_auth_matches_rfc7617_example() {
        assert_eq!(
            basic_auth("Aladdin", "open sesame"),
            "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ=="
        );
    }
    #[test]
    fn policy_denies_only_unreachable_targets() {
        for denied in [
            "http://169.254.169.254/latest",
            "http://0.0.0.0/",
            "http://224.0.0.1/",
            "http://255.255.255.255/",
            "http://[fe80::1]/",
            "http://[::ffff:169.254.169.254]/",
            "http://[fd00:ec2::254]/",
            "http://[::]/",
        ] {
            assert_eq!(
                validate_url(denied).err(),
                Some(SendError::PolicyDenied),
                "{denied}"
            );
        }
        for allowed in [
            "http://127.0.0.1:8080/hook",
            "http://192.168.1.10/",
            "https://example.com/a?token=1",
            "http://[::1]/",
            "http://10.0.0.1/",
        ] {
            assert!(validate_url(allowed).is_ok(), "{allowed}");
        }
        for invalid in [
            "ftp://example.com/",
            "http://user:pw@example.com/",
            "http://example.com/#frag",
            " http://example.com/",
            "http://example.com/\n",
            "not a url",
            "",
        ] {
            assert_eq!(
                validate_url(invalid).err(),
                Some(SendError::InvalidRequest),
                "{invalid:?}"
            );
        }
    }
}
