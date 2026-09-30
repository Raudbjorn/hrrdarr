//! Native bounded reverse-proxy trust. No client admission or authentication.
use super::{MAX_LIST_BYTES, parse_authority};
use axum::extract::{ConnectInfo, Request};
use ipnet::IpNet;
use serde::Serialize;
use std::{
    collections::BTreeSet,
    net::{IpAddr, SocketAddr},
};

const MAX_NETWORKS: usize = 64;
const MAX_HOPS: usize = 16;
const MAX_HEADER_BYTES: usize = 8192;
const HEADERS: [&str; 3] = ["x-forwarded-for", "x-forwarded-host", "x-forwarded-proto"];

/// Effective attributes for consumers. The socket peer is never overwritten by a header.
#[derive(Clone, Debug, Serialize)]
pub struct EffectiveRequestContext {
    pub transport_peer: Option<SocketAddr>,
    pub client_ip: Option<IpAddr>,
    pub scheme: String,
    pub authority: Option<String>,
    pub forwarded_hops: usize,
}

#[derive(Clone, Debug, Default)]
pub(super) struct TrustedNetworks(Vec<IpNet>);
impl TrustedNetworks {
    pub(super) fn parse(value: Option<&str>) -> Result<Self, &'static str> {
        let value = value.unwrap_or("");
        if value.len() > MAX_LIST_BYTES {
            return Err("trusted_networks_limit_exceeded");
        }
        if value.bytes().any(|b| b.is_ascii_control()) {
            return Err("invalid_trusted_network");
        }
        let mut networks = BTreeSet::new();
        if !value.trim_matches(' ').is_empty() {
            for (index, entry) in value.split(',').enumerate() {
                if index >= MAX_NETWORKS {
                    return Err("trusted_networks_limit_exceeded");
                }
                let entry = entry.trim_matches(' ');
                let network = if entry.contains('/') {
                    entry
                        .parse::<IpNet>()
                        .map_err(|_| "invalid_trusted_network")?
                } else {
                    IpNet::from(
                        entry
                            .parse::<IpAddr>()
                            .map_err(|_| "invalid_trusted_network")?,
                    )
                };
                let network = match network.addr() {
                    IpAddr::V6(address) if address.to_ipv4_mapped().is_some() => {
                        let prefix = network
                            .prefix_len()
                            .checked_sub(96)
                            .ok_or("invalid_trusted_network")?;
                        IpNet::new(normalize_ip(IpAddr::V6(address)), prefix)
                            .map_err(|_| "invalid_trusted_network")?
                    }
                    _ => network,
                };
                networks.insert(network.trunc());
            }
        }
        Ok(Self(networks.into_iter().collect()))
    }
    pub(super) fn enabled(&self) -> bool {
        !self.0.is_empty()
    }
    pub(super) fn strings(&self) -> Vec<String> {
        self.0.iter().map(ToString::to_string).collect()
    }
    fn trusts(&self, ip: IpAddr) -> bool {
        let ip = normalize_ip(ip);
        self.0.iter().any(|network| network.contains(&ip))
    }

    pub(super) fn resolve(
        &self,
        request: &mut Request,
        authority: Option<String>,
    ) -> Result<EffectiveRequestContext, &'static str> {
        let peer = request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|peer| peer.0);
        let mut context = EffectiveRequestContext {
            transport_peer: peer,
            client_ip: peer.map(|peer| normalize_ip(peer.ip())),
            // The native listener is HTTP. Only a trusted proxy can report upstream HTTPS.
            scheme: "http".into(),
            authority,
            forwarded_hops: 0,
        };
        if !self.enabled() {
            return Ok(context);
        }
        let peer = peer.ok_or("transport_peer_unavailable")?;
        if self.trusts(peer.ip()) {
            self.apply_headers(request, &mut context)?;
        }
        // Consumers use the typed context, never an attacker-controlled unconsumed prefix.
        // Original-* headers are not a source of authority and are not synthesized here.
        let untrusted_headers: Vec<_> = request
            .headers()
            .keys()
            .filter(|name| {
                let name = name.as_str();
                name == "forwarded"
                    || name.starts_with("x-forwarded-")
                    || name.starts_with("x-original-")
            })
            .cloned()
            .collect();
        for name in untrusted_headers {
            request.headers_mut().remove(name);
        }
        Ok(context)
    }

    fn apply_headers(
        &self,
        request: &Request,
        context: &mut EffectiveRequestContext,
    ) -> Result<(), &'static str> {
        let invalid = "invalid_forwarded_headers";
        if request.headers().contains_key("forwarded") {
            return Err(invalid);
        }
        let mut byte_count = 0usize;
        let mut lists = Vec::with_capacity(3);
        for name in HEADERS {
            let mut headers = request.headers().get_all(name).iter();
            let value = headers.next();
            if headers.next().is_some() {
                return Err(invalid);
            }
            let values = match value {
                None => None,
                Some(value) => {
                    byte_count += value.as_bytes().len();
                    if byte_count > MAX_HEADER_BYTES {
                        return Err(invalid);
                    }
                    let value = value.to_str().map_err(|_| invalid)?;
                    if value.bytes().any(|b| b.is_ascii_control()) {
                        return Err(invalid);
                    }
                    let mut values = Vec::new();
                    for (index, item) in value.split(',').enumerate() {
                        if index >= MAX_HOPS {
                            return Err(invalid);
                        }
                        let item = item.trim_matches(' ');
                        if item.is_empty() {
                            return Err(invalid);
                        }
                        values.push(item);
                    }
                    Some(values)
                }
            };
            lists.push(values);
        }
        let Some(addresses) = &lists[0] else {
            return if lists[1].is_some() || lists[2].is_some() {
                Err(invalid)
            } else {
                Ok(())
            };
        };
        if lists[1..]
            .iter()
            .flatten()
            .any(|list| list.len() != addresses.len())
        {
            return Err(invalid);
        }
        let addresses: Vec<_> = addresses
            .iter()
            .map(|value| parse_address(value).ok_or(invalid))
            .collect::<Result<_, _>>()?;
        let hosts = lists[1]
            .as_ref()
            .map(|list| {
                list.iter()
                    .map(|value| parse_authority(value).map(format_authority).ok_or(invalid))
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;
        let schemes = lists[2]
            .as_ref()
            .map(|list| {
                list.iter()
                    .map(|value| match *value {
                        "http" | "https" => Ok(*value),
                        _ => Err(invalid),
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;
        for index in (0..addresses.len()).rev() {
            let Some(current) = context.client_ip else {
                return Err(invalid);
            };
            if !self.trusts(current) {
                break;
            }
            context.client_ip = Some(addresses[index]);
            if let Some(hosts) = &hosts {
                context.authority = Some(hosts[index].clone());
            }
            if let Some(schemes) = &schemes {
                context.scheme = schemes[index].into();
            }
            context.forwarded_hops += 1;
        }
        Ok(())
    }
}

pub(super) fn format_authority((host, port): (String, Option<u16>)) -> String {
    match port {
        Some(port) => format!("{host}:{port}"),
        None => host,
    }
}
fn normalize_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(ip) => ip
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(ip)),
        _ => ip,
    }
}
fn parse_address(value: &str) -> Option<IpAddr> {
    if value.len() > 64 || value.contains('%') {
        return None;
    }
    value
        .parse::<IpAddr>()
        .ok()
        .or_else(|| {
            value
                .parse::<SocketAddr>()
                .ok()
                .map(|endpoint| endpoint.ip())
        })
        .or_else(|| {
            value
                .strip_prefix('[')?
                .strip_suffix(']')?
                .parse::<std::net::Ipv6Addr>()
                .ok()
                .map(IpAddr::V6)
        })
        .map(normalize_ip)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn network_bounds_and_mapped_identity() {
        let nets =
            TrustedNetworks::parse(Some("192.0.2.5/24, ::ffff:192.0.2.0/120, 2001:db8::9/64"))
                .unwrap();
        assert_eq!(nets.strings(), ["192.0.2.0/24", "2001:db8::/64"]);
        assert!(nets.trusts("::ffff:192.0.2.1".parse().unwrap()));
        assert!(!nets.trusts("192.0.3.1".parse().unwrap()));
        assert!(
            !TrustedNetworks::parse(Some("::/0"))
                .unwrap()
                .trusts("::ffff:192.0.2.1".parse().unwrap())
        );
        assert!(
            !TrustedNetworks::parse(None)
                .unwrap()
                .trusts("127.0.0.1".parse().unwrap())
        );
        for invalid in [
            "::ffff:192.0.2.1/95",
            "127.1",
            "localhost",
            "127.0.0.1,",
            "127.0.0.1\n",
            "127.0.0.1/33",
            "::1/129",
            "[::1]",
            "fe80::1%eth0",
        ] {
            assert!(TrustedNetworks::parse(Some(invalid)).is_err(), "{invalid}");
        }
        assert!(
            TrustedNetworks::parse(Some(&vec!["127.0.0.1"; MAX_NETWORKS + 1].join(","))).is_err()
        );
        assert_eq!(
            parse_address("[::ffff:192.0.2.1]:1234"),
            Some("192.0.2.1".parse().unwrap())
        );
    }
}
