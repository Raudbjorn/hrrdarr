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

An absent or empty list preserves the existing permissive behavior: **no
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
- Explicit non-Unicode values for either variable are errors, not absent values.

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
  "mutability": "deployment",
  "apply_mode": "process_restart",
  "capabilities": {
    "persisted_edits": false,
    "tls": false,
    "url_base": false,
    "trusted_forwarding": false
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

The middleware validates one Host header and/or URI authority (including HTTP/2
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

`Forwarded`, `X-Forwarded-For`, `X-Forwarded-Host` and `X-Forwarded-Proto` are ignored
by this policy. They cannot rescue a denied authority or change client identity.
Reverse proxies must send the normal Host expected by the configured list.
Cookies, Authorization and API-key headers cannot bypass host filtering.

In both pinned Sonarr/Radarr sources, `TrustedNetworks` configures trusted
reverse-proxy sources for forwarded headers, not client admission. This delivery
does not implement that capability or expose an inert networks setting. A future
implementation needs peer capture, bounded trust-chain handling and authority
integration before claiming proxy trust support.

## Verification scope and remaining work

Unit checks in `src/host.rs` cover startup parsing and request authority handling.
Independent `tests/host_api.rs` covers the real HTTP/startup contract. Report actual
check results separately; existence of tests is not evidence they passed.

This backend prerequisite does not complete `sys.001`, `api.006` or `ui.011`.
Credential authentication remains an explicit operator-selected deviation.
Persisted editable settings, TLS, URL base, trusted forwarding, backup/update,
restart and general log/proxy settings remain unsupported. No General UI is
claimed. Its eventual acceptance remains “All workflows are usable through real
APIs, with accessible loading/error/empty states”.
