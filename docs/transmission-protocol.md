# Transmission protocol

This provider is a torrent download client for TV and movies. It talks to Transmission's JSON RPC over HTTP polling, through the same bounded provider transport as qBittorrent and SABnzbd. It shares the torrent path with qBittorrent: `.torrent`/magnet resolution and info-hash identity, the durable submission journal, RSS hash claims, download refresh snapshots, completed-download processing and import.

Protocol source: Transmission's published RPC specification (`docs/rpc-spec.md` in the Transmission repository): the request envelope, the 409 session handshake, `session-get`, `torrent-get`, `torrent-add`, `torrent-set`, `torrent-start`/`torrent-stop`, `torrent-remove` and `queue-move-*`, and the status and error codes. The mocks in `tests/transmission_protocol.rs`, `tests/transmission_automation.rs` and `frontend/tests/transmission-provider-browser.mjs` are written from that specification. None of this is evidence from a live-client run.

Requirements were inventoried from `src/NzbDrone.Core/Download/Clients/Transmission/` in Sonarr `76c684e097f16ac216e6213845e5cac372774995` and Radarr `c90668a520664ad0c91812cfee57c41928ad2148`: which settings exist (host, port, SSL, URL base, credentials, category or directory, recent/older priority, add paused), the version floor and the use of labels from Transmission 4.0. The implementation is independent of both.

## Configuration

`ProviderSettings::Transmission { endpoint, tv, movies }`, stored by migration 0062.

- `endpoint` is one URL: scheme (`https` when SSL is on), host, port and URL base, for example `http://transmission.example:9091/transmission`. The client appends `/rpc`. The editor shows Host, Port (default 9091), Use SSL and URL base (default `/transmission`) as separate fields, composes the URL and displays the resulting RPC URL; editing a saved provider parses the stored URL back into those fields.
- Each scope (`TransmissionScope`) has:
  - `placement`, one of:
    - `{"strategy":"category","category":"…"}`: 1–64 characters of `[A-Za-z0-9._-]`, not `.` or `..`. Torrents go to `<session download-dir>/<category>`; on RPC 17+ the category is also the torrent's label.
    - `{"strategy":"directory","directory":"…"}`: an absolute POSIX or drive path on the Transmission host, at most 1024 bytes, without empty, `.` or `..` segments, `:` after the drive or control characters. No label is applied.
  - `recent_priority`, `older_priority`: `last` (queue order) or `first` (`queue-move-top` after the add). "Recent" uses the same thresholds as SABnzbd.
  - `add_paused`: the torrent is added stopped (`paused: true`).
  - `tags`: optional eligibility tags, as for the other clients.
- TV and movie placements must not collide: two categories must differ case-insensitively (enforced by the unique index `provider_torrent_labels`), and two directories must not be equal or nested (case- and separator-insensitive, enforced by the `provider_scope_directories` insert trigger and by settings validation). A category whose effective folder nests in the other domain's directory depends on the server's download-dir, so only Test can detect it (422 `transmission_domains_overlap`).
- Storage: the category, priorities (0/1) and add-paused (`initial_state` `started`/`stopped`) live in `provider_scopes`; a directory placement lives in `provider_scope_directories`, keyed by provider and domain and deleted with the scope. Directory rows are immutable and a scope cannot hold both a category and a directory. qBittorrent-only scope columns must be NULL.
- Credentials are only `{"kind":"username_password"}`, sent as HTTP basic authentication. The username must not contain `:`. `api_key` is rejected. Credentials stay write-only and both values are redacted from projected strings (names, version).

## RPC envelope and handshake

Every call is `POST <endpoint>/rpc` with `{"method","arguments","tag"}`, `Content-Type: application/json`. The reply must echo the tag and carry a non-empty `result`; anything other than `"success"` is `Rejected`, without reflecting the server's text.

- A 409 must carry `X-Transmission-Session-Id` (captured as at most 128 visible ASCII bytes). The call is resent once with that id. The specification states that the server did not run the method when it answers 409, so resending is safe even for `torrent-add`. A second 409 within the same call, a repeated identical id or more than 8 rotations per operation is `InvalidResponse`. Each operation starts without a session id.
- 401/403 is `authentication` (HTTP 502 from the API), as for the other providers.
- 404 (wrong URL base) and 5xx are transport failures.
- Version gate: `rpc-version` below 14 (Transmission 2.40), or an `rpc-version-minimum` above 17, is `UnsupportedVersion`. The `labels` field is requested from RPC 16; labels decide ownership from RPC 17 (Transmission 4.0).

## Test

`POST /api/v1/providers/{id}/test` and `test-draft`:

1. `session-get` (version gate, `download-dir`, `dht-enabled`, `download-queue-enabled`).
2. Ownership is resolved for each scope. A category scope needs a usable absolute `download-dir`; otherwise 422 `transmission_download_dir_invalid`. Effective locations that nest across domains give 422 `transmission_domains_overlap`.
3. `torrent-get` of every torrent's `id`, `hashString`, `downloadDir` (and `labels`) proves the credentials can read torrents. A torrent claimed by both domains is also `transmission_domains_overlap`.

The result is `TransmissionTest { transmission_version, rpc_version, domains, labels_supported, warnings }`. Warnings: `labels_unsupported` (a category scope on a server older than 4.0, isolated by subfolder only) and `queue_disabled` (a scope uses `first` but the download queue is off).

## Ownership

For each domain the client computes an owner: a category scope owns `<download-dir>/<category>` and the label `<category>`; a directory scope owns its directory. A torrent belongs to a domain when, on RPC 17+ and if it has labels, one of its labels matches the category case-insensitively; otherwise when its `downloadDir` is inside the owned directory. A torrent that belongs to both domains is `ScopeConflict`, never silently assigned.

## Submission identity and idempotency

Transmission identifies torrents by info-hash, so submissions use the same identity as qBittorrent: `prepare` resolves a `.torrent` URL to bytes (or reads the magnet) and derives the v1 info-hash; a source without a v1 hash is `UnsupportedFeature`. The journaled `SubmissionIdentity` is tokenized over target, hash, settings fingerprint and payload SHA-256, and the hash is the `rss_hash_claims` key.

Submission runs these steps in order:

1. Check the prepared payload's SHA-256.
2. `session-get`, then look up the hash. If it is present in this domain the result is `AlreadyPresent`; if it is present elsewhere, `ScopeConflict`. Nothing is added.
3. A trackerless magnet while DHT is disabled is `Rejected` before dispatch.
4. `torrent-add` with `filename` (magnet) or `metainfo` (base64), `download-dir`, `paused` and, for category placements on RPC 17+, `labels`. A `torrent-duplicate` reply (or the legacy "duplicate torrent" failure) returns `AlreadyPresent` or `ScopeConflict` with no follow-up mutations.
5. Follow-ups: `torrent-set` for seed ratio (`seedRatioLimit`, mode 1) and seeding time (`seedIdleLimit`, mode 1) when the indexer sets them, and `queue-move-top` when the applicable priority is `first`.
6. Verify the torrent by hash: placement, paused state, seed values, queue position 0 when moved, and the label.

An authentication failure before `torrent-add` is a definite non-submission. Any failure after `torrent-add` was dispatched is `MutationUnknown` and is never retried automatically. Reconciliation looks the hash up again; it never resubmits. Because the durable journal does not allow `reconciling → observed`, a lost response whose torrent is later found becomes `needs_attention` / `presence_unconfirmed`, and repeated RSS for the same release is suppressed by the hash claim.

## Refresh and completed handoff

- Refresh is two bounded reads per domain: list every torrent's `id`, `hashString`, `downloadDir` (and `labels`), at most 8192 torrents and 1 MiB; then read 24 state fields (plus `labels`) only for this domain's hashes, at most 1000. A torrent that moves between the two reads is reported by whichever domain owns it at the second read.
- Status mapping:
  - Metadata incomplete or size 0: `queued` with diagnostic `metadata` (`paused` when stopped).
  - Local error (3), or tracker error (2) while data is incomplete: `warning` / `error`. `errorString` is never projected. Tracker warnings (1), and tracker errors once data is complete, do not change the status.
  - Check wait/check (1, 2): `queued`.
  - Stopped, seed wait or seeding (0, 5, 6) with complete data: `completed`.
  - Stopped otherwise: `paused`. Download wait (3): `queued`.
  - Downloading (4) and stalled: `warning` / `stalled`; otherwise `downloading`. Seed wait/seeding with incomplete data: `downloading`.
  - Any other code: `unknown` / `unknown_state`.
- A negative ETA is absent and a completed item reports 0. A negative ratio (`-1` unavailable, `-2` infinite) is 0. Category is the label for category placements and empty for directory placements.
- Completed handoff: `details` reads the torrent's `downloadDir`, `files` and `fileStats` (at most 4096 files, relative names only) and produces the same `DownloadDetails` as qBittorrent, so completed-download processing and import, remote path mappings and failure handling are shared for both domains.
- The health/status snapshot reports the domain's effective directory (`download-dir/category` or the explicit directory) and `removes_completed_downloads: false`: Transmission stops, but never removes, torrents that reach seed limits.

## Controls

`transmission::control` implements Pause (`torrent-stop`), Resume (`torrent-start`), Remove (`torrent-remove` with `delete-local-data`) and Priority (`queue-move-top`/`queue-move-bottom`), each followed by a verifying read. Imported categories and per-file priorities are `UnsupportedFeature`. As for qBittorrent, no API route or UI invokes these controls yet; they are exercised by the protocol tests only.

## Protocol routing

Migration 0062 registers `transmission | download_client | torrent` in `provider_implementations` and replaces the scope shape, options and client-options triggers with a Transmission branch. Torznab indexers may bind to it; Newznab indexers are rejected with `protocol_mismatch`. Default selection, RSS/search dispatch, refresh schedules, download refresh, Activity, completed-download processing and the download-root/path health checks treat it like any other torrent client.

## Not claimed

- Live Transmission compatibility was not exercised. Every observation comes from owned mocks written from the RPC specification on 127.0.0.1; no installed or default-port (9091) Transmission was contacted.
- Transmission 4.1's JSON-RPC 2.0 envelope and `rpc-version` 18+ semantics are not implemented; such a server is accepted only if it still serves the legacy envelope and reports `rpc-version-minimum` of 17 or lower. That compatibility is assumed from the specification, not observed.
- Imported (post-import) category or label changes, `MarkImported`, per-file priority and remote removal after import are not implemented. Controls exist at the protocol layer but no route invokes them.
- Snapshot import of Sonarr/Radarr Transmission clients is not implemented.
- Downloaded-scan tracked receipts (`src/search/downloaded.rs`) still accept qBittorrent only.
- The health classifier still reports the Transmission kind as Other, as it does for SABnzbd.
- Ownership on servers older than 4.0 relies only on download directories; a torrent moved by hand into an owned folder is treated as that domain's.
- Status mapping deliberately differs from the references for tracker warnings and for tracker errors on complete data; this is a design choice, not observed parity.
- The browser test does not exercise grabs: the mock only answers `session-get` and `torrent-get`. Grab, duplicate, lost-response and import behaviour is covered by the Rust automation and protocol tests against mocks.
