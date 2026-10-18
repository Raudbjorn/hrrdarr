# Host startup and authority settings

The process has no credential authentication. Its default bind address remains
`127.0.0.1:8760`; port 8760 avoids the Readarr 8787 collision. Deployment bind,
firewall and VPN configuration control network reachability. **Allowed hosts
validate the HTTP authority; they do not restrict client IP addresses.** Anyone
who can reach the listener can send an allowed Host value.

## Deployment configuration

These values are read once before opening or migrating the database and starting
workers. Invalid host configuration exits with sanitized `process_failed` output,
phase `host_configuration`, without echoing the supplied value. Other existing
startup configuration behavior is unchanged.

| Variable | Default | Supported values |
| --- | --- | --- |
| `HRRDARR_BIND` | `127.0.0.1:8760` | IPv4 or bracketed IPv6 socket address with a u16 port; maximum 64 bytes. No hostname resolution. Port zero is supported for owned test listeners. |
| `HRRDARR_ALLOWED_HOSTS` | Absent: filtering disabled | Comma-separated exact hostnames/IP literals. Explicit empty string or ordinary spaces also disables filtering. |
| `HRRDARR_TRUSTED_NETWORKS` | Absent: forwarding disabled | Comma-separated proxy-source IP addresses or CIDRs. Empty string or ordinary spaces disables forwarding; no implicit trusted ranges. |

An absent or empty list preserves the existing permissive behavior: **when forwarding is also disabled, no
application authority validation is applied**, including on otherwise malformed
headers accepted by the HTTP server. The server's HTTP parser may still reject
invalid wire syntax. Binding a public interface does not enable filtering.
There are no automatically appended hostnames or loopback exceptions when a list
is configured; include every authority your client or proxy uses.

Configured lists have these bounds and rules:

- At most 16,384 raw bytes and 64 comma-separated entries, counting duplicates.
- Ordinary ASCII spaces around entries are stripped. ASCII control bytes,
  including tab, CR, LF, NUL and DEL, are rejected before stripping.
- Nonempty hostnames have at most 253 bytes, with ASCII alphanumeric/hyphen labels
  of 1–63 bytes and no leading/trailing hyphen. Case and one trailing DNS dot are
  normalized. ASCII punycode is accepted; raw Unicode/IDNA conversion is unsupported.
- IPv4 uses canonical dotted-decimal parsing. Ambiguous numeric spellings such as
  `127.1` are rejected. IPv6 must be bracketed and is normalized using the standard
  IP parser; zone identifiers are unsupported.
- Empty interior/trailing entries, wildcard/suffix expressions, schemes, paths,
  credentials and list-entry ports are rejected. Duplicate canonical hosts are
  removed and results sorted.
- Explicit non-Unicode values for any of these variables are errors, not absent values.

The configuration is immutable for the life of the process. Change deployment
values and restart the process externally to apply them. There is no host-settings
file, database migration, PUT operation or restart endpoint in this delivery.

## Effective configuration API

`GET /api/v1/config/host` returns the generated Rust `HostSettings` contract:

```json
{
  "authentication": "none",
  "configured_bind": "127.0.0.1:0",
  "bound_address": "127.0.0.1:41235",
  "bind_source": "environment",
  "allowed_hosts": ["example.test"],
  "allowed_hosts_source": "environment",
  "filtering_enabled": true,
  "trusted_networks": ["127.0.0.1/32"],
  "trusted_networks_source": "environment",
  "forwarding_enabled": true,
  "mutability": "deployment",
  "apply_mode": "process_restart",
  "capabilities": {
    "persisted_edits": false,
    "tls": false,
    "url_base": false,
    "trusted_forwarding": true
  }
}
```

`bound_address` comes from the actual bound listener, including the assigned port
when configuration uses zero. It is not a claim of an externally reachable URL.
Each source is `default` or `environment`; an explicitly empty environment list
still has source `environment` and returns `filtering_enabled: false` with an
empty list. This endpoint contains only host settings, not database URLs, provider
keys, inbound API keys or the full environment.

The API has one process-wide policy shared by TV, movies, filesystem, migrations,
providers and all other merged routes. Filtering also applies to this GET,
method-not-allowed responses and the unknown-route fallback.

## Authority enforcement when enabled

When host filtering or forwarding is enabled, the middleware first validates
one original Host header and/or URI authority (including HTTP/2
`:authority`) before allowing the handler to execute. Duplicate Host fields are
rejected, even with identical values. Missing authority, invalid syntax and
conflicting URI/Host authorities are rejected. Every authority is bounded to 260
bytes, with a canonical host as above and an optional decimal u16 port. A trailing
colon, invalid port, user information, whitespace or comma list is invalid.

When URI authority and Host are both present, their canonical hostnames and ports
must agree; absent ports use the URI scheme's default (HTTP 80, HTTPS 443). The
matched allowed-host list excludes ports, so a request's port does not change
which hostname is allowed. Matching is exact, never a suffix or substring match.

| Status | Shared API error code | Meaning |
| --- | --- | --- |
| 400 | `invalid_host_authority` | Missing, duplicate, malformed, oversized or conflicting authority. |
| 403 | `host_not_allowed` | Well-formed canonical host absent from configured list. |

Errors use `ApiErrorEnvelope` and static messages without reflecting request
headers. The HTTP server itself may reject malformed wire requests before this
middleware and therefore return a parser-level 400 without an API envelope.

## Trusted reverse proxies

Trusted networks describe proxy sources, not allowed clients. Configuration is
bounded to 16,384 bytes and 64 entries (including duplicates), with only ordinary
ASCII spaces trimmed and all ASCII control bytes rejected. Use standard bare IP
or CIDR syntax: IPv6 configuration is unbracketed; endpoints, zone identifiers,
DNS names, empty entries and invalid prefixes are rejected. IP literals become
`/32` or `/128`, CIDR host bits are truncated, and canonical duplicates are removed.
The reported list is sorted by address family/address/prefix using `ipnet`.

IPv4-mapped IPv6 addresses are normalized to IPv4 for trust comparison and effective
client identity. Mapped CIDRs require prefix >=96 and become IPv4 CIDRs with 96
subtracted; smaller mapped prefixes are rejected. An IPv4/mapped client matches
only IPv4 rules, never a broad native IPv6 rule such as `::/0`. The actual transport
socket address is preserved separately, including its original family and port.

`forwarding_enabled` means a nonempty trusted list is configured.
`capabilities.trusted_forwarding` means the feature is supported, even when it is
not enabled. The runtime uses `ConnectInfo<SocketAddr>` from the accepted TCP
connection. If forwarding is enabled but peer information is missing, requests
fail with 503 `transport_peer_unavailable`; header values cannot supply that peer.
No network is implicitly trusted, including loopback, private ranges or CGNAT.

Only a trusted transport peer can supply `X-Forwarded-For`, `X-Forwarded-Host` and
`X-Forwarded-Proto`. Untrusted peers' forwarding headers are ignored, including
malformed values, and cannot alter the effective authority or client. A trusted
peer sending no forwarding fields uses the ordinary request attributes.

The native forwarded-header contract is deliberately strict:

1. Each supported header has at most one field occurrence. Comma-separated lists
   have at most 16 entries; the combined three field values have at most 8,192
   bytes. Duplicate fields, controls, empty entries and malformed values fail.
2. `X-Forwarded-For` is required if either host or proto is supplied. Optional host
   and proto lists must have exactly the same number of entries as the for list.
   Proxies must append corresponding entries, not a single unaligned host/proto.
3. For entries accept plain IPv4/IPv6, bracketed IPv6, or socket endpoints; ports
   are discarded for effective client identity. Each entry is at most 64 bytes,
   and zone identifiers are unsupported. Host entries use the existing bounded
   authority parser. Proto entries are exactly lowercase `http` or `https`.
4. All list syntax is validated within the limits. Evaluation starts at the
   rightmost entry with the actual socket peer. An entry is consumed only if the
   current peer is trusted; its for/host/proto attributes are consumed together.
   The new client IP is checked before consuming the next entry to the left.
   Evaluation stops at the first untrusted hop. Unconsumed prefixes never choose
   the client, scheme or authority, even if they contain allowed hostnames.
5. The original Host/URI authority must be valid and unambiguous before any
   forwarding replacement. Its syntax validation compares original URI/Host ports
   using the original URI scheme. The selected forwarded authority is independent
   of that original authority and paired only with the same consumed proto entry;
   its explicit port is preserved, never compared to an original-scheme default.
   The allowed-host list matches the selected effective canonical hostname without
   its port. Cookies, Authorization and API keys cannot bypass this check.

RFC `Forwarded` is unsupported and causes 400 `invalid_forwarded_headers` when
sent by a trusted peer; it is ignored from untrusted peers. `X-Original-*` is never
trusted. Other `X-Forwarded-*` extensions, such as prefix, are unsupported and
ignored. When forwarding is enabled, all `Forwarded`, `X-Forwarded-*` and
`X-Original-*` fields are removed before the handler runs, including unconsumed
prefixes. With forwarding disabled, they remain ignored by this policy and are
left unchanged, preserving existing behavior.

Malformed trusted forwarding returns a sanitized shared envelope with status 400
and code `invalid_forwarded_headers`. No request-provided value is reflected.
The HTTP server may reject malformed wire syntax earlier without an API envelope.

### Effective request context

Handlers consume the `hrrdarr::host::EffectiveRequestContext` request extension:

```text
transport_peer: Option<SocketAddr> // actual accepted socket endpoint
client_ip: Option<IpAddr>          // normalized effective client
scheme: String                    // http, or trusted reported http/https
authority: Option<String>          // canonical effective host[:explicit-port]
forwarded_hops: usize              // consumed trusted suffix length
```

This is actual middleware state, not a diagnostic endpoint. The global host
allowlist consumes its effective authority. Consumers must use this context for
effective attributes: the request URI and original Host header are not rewritten.
The direct scheme is `http` because the native listener is plaintext, even when
a client sends an absolute-form `https` URI. Trusted proto can describe TLS
terminated upstream but does not establish built-in TLS support. The authority
may be absent only in the legacy permissive path where both features are disabled.
A router served without ConnectInfo and with forwarding disabled reports no peer
or client rather than fabricating an address.

### Reference requirements and deliberate native differences

Both pinned `NzbDrone.Host/ForwardedHeadersConfigurator.cs` sources enable the
three X headers, remove the hop limit and append configured networks to framework
known-source collections. Their `Startup.cs` applies forwarding before host
filtering (Sonarr lines329–330; Radarr260–261). They do not clear inherited trusted
sources. Their host projects target .NET10 (Sonarr) and .NET8 (Radarr).

Official framework defaults include IPv6 loopback `::1` and IPv4 loopback
`127.0.0.0/8`, and do not require header symmetry: [ASP.NET Core8 options](https://github.com/dotnet/aspnetcore/blob/v8.0.0/src/Middleware/HttpOverrides/src/ForwardedHeadersOptions.cs)
and [ASP.NET Core10 options](https://github.com/dotnet/aspnetcore/blob/v10.0.0/src/Middleware/HttpOverrides/src/ForwardedHeadersOptions.cs)
with its [default network list](https://github.com/dotnet/aspnetcore/blob/v10.0.0/src/Middleware/HttpOverrides/src/DualIPNetworkList.cs).
The native implementation instead has explicit-only trust, a 16-entry bound,
strict aligned optional lists and typed effective attributes. These are deliberate
native restrictions, not a claim of complete upstream runtime equivalence. Source
was used to establish requirements only; no upstream implementation was ported.

## Verification scope and remaining work

Unit checks in `src/host.rs` cover startup parsing and request authority handling.
Independent `tests/host_api.rs` and `tests/host_forwarding.rs` cover the real
HTTP/startup and effective-context contracts. Report actual
check results separately; existence of tests is not evidence they passed.

This backend prerequisite does not complete `sys.001`, `api.006` or `ui.011`.
The plan retains “System parity includes authentication/API keys and host settings”.
Credential authentication remains an explicit operator-selected deviation.
Persisted editable settings, TLS, URL base, backup/update,
restart and general log/proxy settings remain unsupported. No General UI is
claimed. Its eventual acceptance remains “All workflows are usable through real
APIs, with accessible loading/error/empty states”.
