//! Owned real socket context: no proxy header is allowed to manufacture transport trust.
use axum::{Json, Router, extract::Extension, http::HeaderMap, routing::get};
use hrrdarr::host::{EffectiveRequestContext, HostConfig};
use serde_json::{Value, json};
use std::{net::SocketAddr, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
struct Server {
    origin: String,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn echo(
    Extension(context): Extension<EffectiveRequestContext>,
    headers: HeaderMap,
) -> Json<Value> {
    let forwarding_headers_present = [
        "forwarded",
        "x-forwarded-for",
        "x-forwarded-host",
        "x-forwarded-proto",
        "x-original-host",
        "x-forwarded-custom",
    ]
    .iter()
    .any(|name| headers.contains_key(*name));
    Json(json!({"context": context, "forwarding_headers_present": forwarding_headers_present}))
}
async fn server(trust: Option<&str>, capture_peer: bool) -> Server {
    server_with_hosts(
        trust,
        capture_peer,
        Some("external.example,allowed.example,[::1]"),
    )
    .await
}
async fn server_with_hosts(trust: Option<&str>, capture_peer: bool, hosts: Option<&str>) -> Server {
    let config =
        HostConfig::from_values_with_trusted_networks(Some("127.0.0.1:0"), hosts, trust).unwrap();
    let listener = tokio::net::TcpListener::bind(config.bind_address())
        .await
        .unwrap();
    let bound = listener.local_addr().unwrap();
    let app = config.apply(Router::new().route("/echo", get(echo)), bound);
    let task = tokio::spawn(async move {
        if capture_peer {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        } else {
            axum::serve(listener, app).await.unwrap();
        }
    });
    Server {
        origin: format!("http://{bound}"),
        task,
    }
}
async fn request(server: &Server, host: &str, headers: &[(&str, &str)]) -> (u16, Value) {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let mut request = client
        .get(format!("{}/echo", server.origin))
        .header("Host", host);
    for (key, value) in headers {
        request = request.header(*key, *value);
    }
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    let text = response.text().await.unwrap();
    (
        status,
        serde_json::from_str(&text).unwrap_or(Value::String(text)),
    )
}
async fn raw(server: &Server, headers: &str) -> u16 {
    tokio::time::timeout(Duration::from_secs(5),async {
        let mut socket=tokio::net::TcpStream::connect(server.origin.strip_prefix("http://").unwrap()).await.unwrap();
        socket.write_all(format!("GET /echo HTTP/1.1\r\nHost: allowed.example\r\n{headers}Connection: close\r\n\r\n").as_bytes()).await.unwrap();
        let mut bytes=Vec::new();socket.take(65537).read_to_end(&mut bytes).await.unwrap();assert!(bytes.len()<=65536);
        String::from_utf8_lossy(&bytes).split_whitespace().nth(1).unwrap().parse().unwrap()
    }).await.unwrap()
}
#[tokio::test]
async fn real_peer_trust_consumes_only_the_verified_right_hand_chain() {
    let server = server(Some("127.0.0.1/32,10.0.0.0/8"), true).await;
    let (code, value) = request(
        &server,
        "internal.example",
        &[
            ("x-forwarded-for", "198.51.100.9, 203.0.113.7, 10.1.2.3"),
            (
                "x-forwarded-host",
                "evil.example, external.example:443, allowed.example",
            ),
            ("x-forwarded-proto", "http, https, http"),
        ],
    )
    .await;
    assert_eq!(code, 200, "{value}");
    let context = &value["context"];
    assert_eq!(context["client_ip"], "203.0.113.7");
    assert_eq!(context["scheme"], "https");
    assert_eq!(context["authority"], "external.example:443");
    assert_eq!(context["forwarded_hops"], 2);
    let peer: SocketAddr = context["transport_peer"].as_str().unwrap().parse().unwrap();
    assert!(peer.ip().is_loopback());
    assert_ne!(peer.port(), 0);
    assert_eq!(value["forwarding_headers_present"], false);
    // The left prefix is not trusted merely because a later hop asserted it.
    let (code, value) = request(
        &server,
        "allowed.example",
        &[
            ("x-forwarded-for", "198.51.100.9, 203.0.113.7"),
            ("x-forwarded-host", "external.example, evil.example"),
        ],
    )
    .await;
    assert_eq!(code, 403);
    assert_eq!(value["error"]["code"], "host_not_allowed");
    let (code, value) = request(&server, "allowed.example", &[]).await;
    assert_eq!(code, 200);
    assert_eq!(value["context"]["forwarded_hops"], 0);
    assert_eq!(value["context"]["client_ip"], "127.0.0.1");
    assert_eq!(value["context"]["scheme"], "http");
}
#[tokio::test]
async fn untrusted_or_unconfigured_peers_cannot_spoof_authority_client_or_scheme() {
    for trust in [None, Some(""), Some("192.0.2.0/24")] {
        let server = server(trust, true).await;
        let malicious = [
            ("x-forwarded-for", "garbage"),
            ("x-forwarded-host", "external.example"),
            ("x-forwarded-proto", "https"),
            ("forwarded", "for=127.0.0.1;host=external.example"),
        ];
        assert_eq!(request(&server, "evil.example", &malicious).await.0, 403);
        let (code, value) = request(&server, "allowed.example", &malicious).await;
        assert_eq!(code, 200, "{value}");
        assert_eq!(value["context"]["client_ip"], "127.0.0.1");
        assert_eq!(value["context"]["scheme"], "http");
        assert_eq!(value["context"]["authority"], "allowed.example");
        assert_eq!(value["context"]["forwarded_hops"], 0);
        if trust == Some("192.0.2.0/24") {
            assert_eq!(value["forwarding_headers_present"], false);
        }
    }
}
#[tokio::test]
async fn trusted_malformed_or_ambiguous_forwarding_fails_without_reflecting_input() {
    let server = server(Some("127.0.0.1"), true).await;
    for headers in [
        vec![("x-forwarded-host", "external.example")],
        vec![("x-forwarded-proto", "https")],
        vec![("x-forwarded-for", "unknown")],
        vec![("x-forwarded-for", "_hidden")],
        vec![("x-forwarded-for", "203.0.113.1,")],
        vec![
            ("x-forwarded-for", "203.0.113.1,10.0.0.1"),
            ("x-forwarded-host", "external.example"),
        ],
        vec![
            ("x-forwarded-for", "203.0.113.1"),
            ("x-forwarded-proto", "ftp"),
        ],
        vec![
            ("x-forwarded-for", "203.0.113.1"),
            ("x-forwarded-host", "sentinel-secret@external.example"),
        ],
        vec![("forwarded", "for=203.0.113.1;host=external.example")],
        vec![
            ("forwarded", "for=203.0.113.1"),
            ("x-forwarded-for", "203.0.113.1"),
        ],
        // Even an unconsumed prefix must have valid bounded syntax.
        vec![("x-forwarded-for", "sentinel-invalid,203.0.113.1")],
    ] {
        let (code, value) = request(&server, "allowed.example", &headers).await;
        assert_eq!(code, 400, "{headers:?}: {value}");
        assert_eq!(value["error"]["code"], "invalid_forwarded_headers");
        assert!(!value.to_string().contains("sentinel"));
    }
    for name in ["X-Forwarded-For", "X-Forwarded-Host", "X-Forwarded-Proto"] {
        let value = match name {
            "X-Forwarded-For" => "203.0.113.1",
            "X-Forwarded-Host" => "external.example",
            _ => "https",
        };
        let base = if name == "X-Forwarded-For" {
            ""
        } else {
            "X-Forwarded-For: 203.0.113.1\r\n"
        };
        assert_eq!(
            raw(
                &server,
                &format!("{base}{name}: {value}\r\n{name}: {value}\r\n")
            )
            .await,
            400
        );
    }
    // Boundary uses internal whitespace (not HTTP-trimmed outer OWS), keeping valid IPs.
    let boundary = format!("127.0.0.1,{}127.0.0.1", " ".repeat(8173));
    assert_eq!(boundary.len(), 8192);
    assert_eq!(
        request(
            &server,
            "allowed.example",
            &[("x-forwarded-for", &boundary)]
        )
        .await
        .0,
        200
    );
    let overflow = format!("127.0.0.1,{}127.0.0.1", " ".repeat(8174));
    assert_eq!(
        request(
            &server,
            "allowed.example",
            &[("x-forwarded-for", &overflow)]
        )
        .await
        .0,
        400
    );
    assert_eq!(
        request(
            &server,
            "allowed.example",
            &[
                ("x-forwarded-for", &boundary),
                ("x-forwarded-proto", "http,http")
            ]
        )
        .await
        .0,
        400
    );
    let chain16 = vec!["127.0.0.1"; 16].join(",");
    let chain17 = vec!["127.0.0.1"; 17].join(",");
    let (code, value) = request(&server, "allowed.example", &[("x-forwarded-for", &chain16)]).await;
    assert_eq!(code, 200);
    assert_eq!(value["context"]["forwarded_hops"], 16);
    assert_eq!(
        request(&server, "allowed.example", &[("x-forwarded-for", &chain17)])
            .await
            .0,
        400
    );
    assert_eq!(
        request(
            &server,
            "allowed.example",
            &[("x-forwarded-for", &"x".repeat(8193))]
        )
        .await
        .0,
        400
    );
}
#[tokio::test]
async fn ipv6_and_mapped_addresses_follow_the_same_trust_boundary() {
    for trust in [
        "127.0.0.1/32,2001:db8::/32",
        "::ffff:127.0.0.1/128,2001:db8::/32",
    ] {
        let server = server(Some(trust), true).await;
        let (code, value) = request(
            &server,
            "allowed.example",
            &[
                ("x-forwarded-for", "[2001:db9::1]:123, [2001:db8::2]:456"),
                ("x-forwarded-host", "[::1]:443, allowed.example"),
                ("x-forwarded-proto", "https, http"),
            ],
        )
        .await;
        assert_eq!(code, 200, "{value}");
        assert_eq!(value["context"]["client_ip"], "2001:db9::1");
        assert_eq!(value["context"]["authority"], "[::1]:443");
        assert_eq!(value["context"]["scheme"], "https");
        assert_eq!(value["context"]["forwarded_hops"], 2);
        let (code, value) = request(
            &server,
            "allowed.example",
            &[("x-forwarded-for", "::ffff:203.0.113.1")],
        )
        .await;
        assert_eq!(code, 200);
        assert_eq!(value["context"]["client_ip"], "203.0.113.1");
    }
}
#[tokio::test]
async fn mapped_ipv4_does_not_inherit_broad_native_ipv6_trust() {
    let server = server(Some("127.0.0.1/32,::/0"), true).await;
    let (code, value) = request(
        &server,
        "allowed.example",
        &[("x-forwarded-for", "198.51.100.8, ::ffff:203.0.113.9")],
    )
    .await;
    assert_eq!(code, 200, "{value}");
    assert_eq!(value["context"]["client_ip"], "203.0.113.9");
    assert_eq!(value["context"]["forwarded_hops"], 1);
}

#[tokio::test]
async fn configured_trust_requires_actual_transport_metadata() {
    let missing = server(Some("127.0.0.1"), false).await;
    let (code, value) = request(
        &missing,
        "allowed.example",
        &[("x-forwarded-for", "127.0.0.1")],
    )
    .await;
    assert_eq!(code, 503);
    assert_eq!(value["error"]["code"], "transport_peer_unavailable");
    let disabled = server(None, false).await;
    let (code, value) = request(&disabled, "allowed.example", &[]).await;
    assert_eq!(code, 200);
    assert_eq!(value["context"]["transport_peer"], Value::Null);
    assert_eq!(value["context"]["client_ip"], Value::Null);
    // Legacy permissiveness survives when both policies are disabled: comma authority
    // is invalid to our host parser but legal enough for the HTTP transport to deliver.
    let permissive = server_with_hosts(None, true, None).await;
    let (code, value) = request(
        &permissive,
        "allowed.example,evil.example",
        &[("x-forwarded-for", "malformed")],
    )
    .await;
    assert_eq!(code, 200, "{value}");
    assert_eq!(value["context"]["authority"], Value::Null);
    assert_eq!(value["context"]["forwarded_hops"], 0);
}

#[test]
fn trusted_network_configuration_is_bounded_literal_only_and_not_implicitly_loopback() {
    let parse =
        |value: &str| HostConfig::from_values_with_trusted_networks(None, None, Some(value));
    for valid in [
        "",
        " ",
        "127.0.0.1",
        "2001:db8::1",
        "192.0.2.129/24",
        "::ffff:192.0.2.129/120",
    ] {
        assert!(parse(valid).is_ok(), "{valid}");
    }
    for invalid in [
        "localhost",
        "https://127.0.0.1",
        "127.0.0.1:80",
        "127.0.0.1,",
        "127.1",
        "127.0.0.1/33",
        "2001:db8::/129",
        "[::1]",
        "::ffff:127.0.0.1/95",
        "fe80::1%lo",
        "127.0.0.1\t",
        "éxample.invalid",
    ] {
        assert!(parse(invalid).is_err(), "{invalid}");
    }
    assert!(parse(&vec!["127.0.0.1"; 64].join(",")).is_ok());
    assert!(parse(&vec!["127.0.0.1"; 65].join(",")).is_err());
    assert!(parse(&" ".repeat(16384)).is_ok());
    assert!(parse(&" ".repeat(16385)).is_err());
}

#[tokio::test]
async fn network_snapshot_canonicalizes_literals_hostbits_and_mapped_duplicates() {
    let server = server(
        Some("192.0.2.129/24, ::ffff:192.0.2.129/120, 2001:db8::9/64,127.0.0.1"),
        true,
    )
    .await;
    let response = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
        .get(format!("{}/api/v1/config/host", server.origin))
        .header("Host", "allowed.example")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let value: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
    assert_eq!(
        value["trusted_networks"],
        json!(["127.0.0.1/32", "192.0.2.0/24", "2001:db8::/64"])
    );
    // IPv4 socket syntax and mapped IPv6 must normalize before the next-hop trust check.
    let (code, value) = request(
        &server,
        "allowed.example",
        &[
            (
                "x-forwarded-for",
                "198.51.100.1:123, [::ffff:192.0.2.1]:456",
            ),
            ("x-original-host", "evil.example"),
            ("x-forwarded-custom", "sentinel"),
        ],
    )
    .await;
    assert_eq!(code, 200, "{value}");
    assert_eq!(value["context"]["client_ip"], "198.51.100.1");
    assert_eq!(value["context"]["forwarded_hops"], 2);
    assert_eq!(value["forwarding_headers_present"], false);
}
