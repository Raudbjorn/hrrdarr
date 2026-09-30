//! Deployment-controlled listener and HTTP authority policy; never client authentication.
use axum::{
    Json, Router,
    extract::{Request, State},
    http::{StatusCode, header::HOST, uri::Authority},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Serialize;
use std::{
    collections::BTreeSet,
    net::{IpAddr, SocketAddr},
    sync::Arc,
};
use ts_rs::TS;

const DEFAULT_BIND: &str = "127.0.0.1:8760";
const MAX_BIND_BYTES: usize = 64;
const MAX_HOSTS: usize = 64;
const MAX_LIST_BYTES: usize = 16 * 1024;
const MAX_HOST_BYTES: usize = 253;
const MAX_AUTHORITY_BYTES: usize = MAX_HOST_BYTES + 7;

#[derive(Clone, Copy, Debug, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum HostSettingSource {
    Default,
    Environment,
}
#[derive(Clone, Copy, Debug, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum HostAuthentication {
    None,
}
#[derive(Clone, Copy, Debug, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum HostMutability {
    Deployment,
}
#[derive(Clone, Copy, Debug, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum HostApplyMode {
    ProcessRestart,
}
#[derive(Clone, Debug, Serialize, TS)]
pub struct HostCapabilities {
    pub persisted_edits: bool,
    pub tls: bool,
    pub url_base: bool,
    pub trusted_forwarding: bool,
}
#[derive(Clone, Debug, Serialize, TS)]
pub struct HostSettings {
    pub authentication: HostAuthentication,
    pub configured_bind: String,
    pub bound_address: String,
    pub bind_source: HostSettingSource,
    pub allowed_hosts: Vec<String>,
    pub allowed_hosts_source: HostSettingSource,
    pub filtering_enabled: bool,
    pub mutability: HostMutability,
    pub apply_mode: HostApplyMode,
    pub capabilities: HostCapabilities,
}

#[derive(Clone, Debug)]
pub struct HostConfig {
    bind: SocketAddr,
    bind_source: HostSettingSource,
    allowed_hosts: Vec<String>,
    allowed_hosts_source: HostSettingSource,
}

impl HostConfig {
    pub fn from_env() -> Result<Self, &'static str> {
        let bind = environment_value("HRRDARR_BIND")?;
        let hosts = environment_value("HRRDARR_ALLOWED_HOSTS")?;
        Self::from_values(bind.as_deref(), hosts.as_deref())
    }

    /// Pure startup parser. Missing and explicitly empty lists preserve permissive behavior.
    pub fn from_values(bind: Option<&str>, hosts: Option<&str>) -> Result<Self, &'static str> {
        let bind_value = bind.unwrap_or(DEFAULT_BIND);
        if bind_value.len() > MAX_BIND_BYTES {
            return Err("invalid_bind_address");
        }
        let bind_address = bind_value.parse().map_err(|_| "invalid_bind_address")?;
        let list = hosts.unwrap_or("");
        if list.len() > MAX_LIST_BYTES {
            return Err("allowed_hosts_limit_exceeded");
        }
        if list.bytes().any(|byte| byte.is_ascii_control()) {
            return Err("invalid_allowed_host");
        }
        let mut allowed_hosts = BTreeSet::new();
        if !list.trim_matches(' ').is_empty() {
            for (index, entry) in list.split(',').enumerate() {
                if index >= MAX_HOSTS {
                    return Err("allowed_hosts_limit_exceeded");
                }
                allowed_hosts
                    .insert(canonical_host(entry.trim_matches(' ')).ok_or("invalid_allowed_host")?);
            }
        }
        Ok(Self {
            bind: bind_address,
            bind_source: if bind.is_some() {
                HostSettingSource::Environment
            } else {
                HostSettingSource::Default
            },
            allowed_hosts: allowed_hosts.into_iter().collect(),
            allowed_hosts_source: if hosts.is_some() {
                HostSettingSource::Environment
            } else {
                HostSettingSource::Default
            },
        })
    }

    pub fn bind_address(&self) -> SocketAddr {
        self.bind
    }

    /// Apply last, after every application route, to cover fallback and mutation routes too.
    /// `bound` must be the actual listener.local_addr(), including its assigned ephemeral port.
    pub fn apply(self, app: Router, bound: SocketAddr) -> Router {
        let settings = Arc::new(HostSettings {
            authentication: HostAuthentication::None,
            configured_bind: self.bind.to_string(),
            bound_address: bound.to_string(),
            bind_source: self.bind_source,
            filtering_enabled: !self.allowed_hosts.is_empty(),
            allowed_hosts: self.allowed_hosts,
            allowed_hosts_source: self.allowed_hosts_source,
            mutability: HostMutability::Deployment,
            apply_mode: HostApplyMode::ProcessRestart,
            capabilities: HostCapabilities {
                persisted_edits: false,
                tls: false,
                url_base: false,
                trusted_forwarding: false,
            },
        });
        app.merge(
            Router::new()
                .route("/api/v1/config/host", get(read_settings))
                .with_state(settings.clone()),
        )
        .layer(middleware::from_fn_with_state(settings, enforce_authority))
    }
}

/// Never substitute defaults for an explicitly supplied non-Unicode deployment value.
fn environment_value(name: &str) -> Result<Option<String>, &'static str> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err("invalid_environment_value"),
    }
}

async fn read_settings(State(settings): State<Arc<HostSettings>>) -> Json<HostSettings> {
    Json((*settings).clone())
}

fn canonical_host(value: &str) -> Option<String> {
    if value.is_empty() || value.len() > MAX_HOST_BYTES || !value.is_ascii() {
        return None;
    }
    if let Some(inner) = value.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
        return inner
            .parse::<std::net::Ipv6Addr>()
            .ok()
            .map(|ip| format!("[{ip}]"));
    }
    if let Ok(ip) = value.parse::<IpAddr>() {
        return match ip {
            IpAddr::V4(ip) => Some(ip.to_string()),
            IpAddr::V6(_) => None,
        };
    }
    let dns = value.strip_suffix('.').unwrap_or(value);
    // Reject ambiguous noncanonical numeric IP spellings rather than resolving them as DNS.
    if dns.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return None;
    }
    if !dns.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label.as_bytes()[0].is_ascii_alphanumeric()
            && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    }) {
        return None;
    }
    Some(dns.to_ascii_lowercase())
}

fn parse_authority(value: &str) -> Option<(String, Option<u16>)> {
    if value.len() > MAX_AUTHORITY_BYTES || value.contains('@') {
        return None;
    }
    let authority = value.parse::<Authority>().ok()?;
    let host = authority.host();
    let suffix = value.strip_prefix(host)?;
    let port = if suffix.is_empty() {
        None
    } else {
        let digits = suffix.strip_prefix(':')?;
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        Some(digits.parse::<u16>().ok()?)
    };
    Some((canonical_host(host)?, port))
}

fn request_host(request: &Request) -> Option<String> {
    let mut values = request.headers().get_all(HOST).iter();
    let header = values
        .next()
        .map(|v| v.to_str().ok().and_then(parse_authority));
    if values.next().is_some() {
        return None;
    }
    let header = match header {
        Some(Some(value)) => Some(value),
        Some(None) => return None,
        None => None,
    };
    let uri = match request.uri().authority() {
        Some(value) => Some(parse_authority(value.as_str())?),
        None => None,
    };
    match (uri, header) {
        (Some(uri), Some(header)) => {
            let default_port = match request.uri().scheme_str() {
                Some("http") => Some(80),
                Some("https") => Some(443),
                _ => None,
            };
            if uri.0 != header.0 || uri.1.or(default_port) != header.1.or(default_port) {
                return None;
            }
            Some(uri.0)
        }
        (Some(uri), None) => Some(uri.0),
        (None, Some(header)) => Some(header.0),
        (None, None) => None,
    }
}

async fn enforce_authority(
    State(settings): State<Arc<HostSettings>>,
    request: Request,
    next: Next,
) -> Response {
    if settings.filtering_enabled {
        let Some(host) = request_host(&request) else {
            return (
                StatusCode::BAD_REQUEST,
                Json(crate::api::ApiErrorEnvelope::new(
                    "invalid_host_authority",
                    "A single valid HTTP host authority is required",
                )),
            )
                .into_response();
        };
        if !settings.allowed_hosts.contains(&host) {
            return (
                StatusCode::FORBIDDEN,
                Json(crate::api::ApiErrorEnvelope::new(
                    "host_not_allowed",
                    "HTTP host authority is not allowed",
                )),
            )
                .into_response();
        }
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn startup_and_authority_contract() {
        let default = HostConfig::from_values(None, None).unwrap();
        assert_eq!(default.bind_address().to_string(), DEFAULT_BIND);
        assert!(default.allowed_hosts.is_empty());
        assert!(
            HostConfig::from_values(None, Some("  "))
                .unwrap()
                .allowed_hosts
                .is_empty()
        );
        let configured = HostConfig::from_values(
            Some("[::1]:0"),
            Some("EXAMPLE.test.,[0:0:0:0:0:0:0:1],example.test,127.0.0.1"),
        )
        .unwrap();
        assert_eq!(
            configured.allowed_hosts,
            ["127.0.0.1", "[::1]", "example.test"]
        );
        for host in [
            "*",
            "*.example.test",
            ".example.test",
            "https://example.test",
            "example.test:80",
            "user@example.test",
            "example.test/path",
            "é.test",
            "bad_name",
            "::1",
            "127.1",
            "a,,b",
            "a,",
            "-host",
            "host-",
            "a\n.test",
            "a\n",
            "\r\n",
            "a\t",
            "\0",
            "\u{a0}a",
        ] {
            assert!(HostConfig::from_values(None, Some(host)).is_err(), "{host}");
        }
        assert!(HostConfig::from_values(Some("bad"), None).is_err());
        assert!(HostConfig::from_values(None, Some(&"a".repeat(MAX_LIST_BYTES + 1))).is_err());
        assert!(HostConfig::from_values(None, Some(&vec!["a"; MAX_HOSTS + 1].join(","))).is_err());
        assert_eq!(
            parse_authority("[::1]:8760"),
            Some(("[::1]".into(), Some(8760)))
        );
        for bad in [
            "a:", "a:65536", "a:bad", "a,b", "a:80:90", "a@b", " a", "a ",
        ] {
            assert!(parse_authority(bad).is_none(), "{bad}");
        }
        let make = |uri: &str, host: Option<&str>| {
            let mut request = Request::builder().uri(uri);
            if let Some(host) = host {
                request = request.header(HOST, host);
            }
            request.body(axum::body::Body::empty()).unwrap()
        };
        assert_eq!(
            request_host(&make("http://example.test/", Some("EXAMPLE.test:80"))),
            Some("example.test".into())
        );
        assert_eq!(
            request_host(&make("http://example.test:81/", Some("example.test:80"))),
            None
        );
        assert_eq!(
            request_host(&make("http://example.test/", Some("other.test"))),
            None
        );
        assert_eq!(
            request_host(&make("https://example.test/", None)),
            Some("example.test".into())
        );
        assert_eq!(request_host(&make("/", None)), None);
        let mut duplicate = make("/", Some("example.test"));
        duplicate
            .headers_mut()
            .append(HOST, "example.test".parse().unwrap());
        assert_eq!(request_host(&duplicate), None);
    }
}
