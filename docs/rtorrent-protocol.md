# rTorrent protocol

rTorrent is a torrent download client for TV and movies. It talks XML-RPC over HTTP (`POST <endpoint>` with `Content-Type: text/xml`) to the web server that fronts rTorrent's SCGI socket (an nginx `scgi_pass` or ruTorrent's `/RPC2` mount) through the same bounded provider transport as the other clients. It is the second client on the shared label-torrent engine described in [deluge-protocol.md](deluge-protocol.md): ownership by label, the info-hash-reconciled never-double-submit flow, status mapping, completed-download facts and controls live in `src/providers/label_client.rs`; this client is only the `Wire` in `src/providers/label_client/rtorrent.rs`. No XML-RPC crate was added: requests are serialized by hand and responses parsed with the already-present `roxmltree` (DTDs refused, node count bounded, depth bounded).

Protocol source: rTorrent's published command reference (`system.client_version`, `directory.default`, `d.multicall2`, `f.multicall`, `d.name`, `d.is_multi_file`, `load.normal`/`load.start`/`load.raw`/`load.raw_start`, `d.start`, `d.stop`, `d.erase`, `d.priority.set`, `d.custom1.set`). The mocks in `tests/rtorrent_protocol.rs`, `tests/rtorrent_automation.rs` and `frontend/tests/rtorrent-provider-browser.mjs` are written from that reference. None of this is evidence from a live-client run.

Requirements were inventoried from `src/NzbDrone.Core/Download/Clients/RTorrent/` in Sonarr `76c684e097f16ac216e6213845e5cac372774995` and Radarr `c90668a520664ad0c91812cfee57c41928ad2148`: host/port/URL path/SSL and optional credentials, the label in `d.custom1`, recent/older priority, start or stopped on add, and the status rules. The implementation is independent of both.

## Configuration

`ProviderSettings::Rtorrent { endpoint, tv, movies }`, stored by migration 0070 (registry row only; the scope triggers and unique label index came with 0069, and 0075 reinstalls the trigger set with every lane's implementations).

- `endpoint` is the full XML-RPC URL, for example `http://rtorrent.example/RPC2`; nothing is appended.
- Each scope is the shared `TorrentLabelScope`: `category` (the label: 1-64 of lowercase letters, digits, `.`, `_`, `-`), `recent_priority`/`older_priority` (`last` or `first`; `first` loads the torrent with `d.priority.set=3`, high), `add_paused` and optional eligibility `tags`. TV and movie labels must differ.
- Credentials are HTTP basic `{"kind":"username_password"}` for the web server in front of rTorrent, or none; `api_key` is rejected. Both values are redacted from projected strings and the username must not contain `:`.

## Test

`POST /api/v1/providers/{id}/test` and `test-draft` call `system.client_version` (0.9 or later; `unsupported` otherwise), read `directory.default` and list the torrents of each configured domain's label. The result is `LabelClientTest { client_version, domains, warnings }`. Labels are `d.custom1` values and need no declaration, so Test has nothing to create. A wrong path (404) is a connection failure, an HTML page or an XML document with a DTD is `InvalidResponse`, a fault is a rejection that is not reflected.

## Status mapping

`d.multicall2` over the `main` view (all torrents; the engine filters by the URL-decoded `d.custom1`). Hashing is `queued`; size 0 is a magnet awaiting metadata (`queued` / `metadata`); a non-empty `d.message` is `warning` / `error` unless it begins `Tracker:` (tracker notes are not torrent errors); stopped is `paused` (or `completed` once complete); complete and started is `completed`; active is `downloading`; started but inactive is `queued`. `d.ratio` is in thousandths. rTorrent reports no ETA or seeding time, so those are absent.

The save path is `d.directory`, except that a multi-file torrent's directory already ends with its own name, so the parent is used; file paths from `f.multicall` are then prefixed with the torrent name for multi-file torrents so they are relative to the save path (a single-file torrent's path is its name).

## Submission

Same flow as Deluge (lookup by hash, `ScopeConflict` for a hash that is present unlabelled or under another label, never a second load). The label and priority are load-time commands (`d.custom1.set=<label>`, `d.priority.set=3`), so a torrent never exists unlabelled: magnets use `load.start`/`load.normal` and uploaded `.torrent` bytes (XML-RPC `base64`) use `load.raw_start`/`load.raw`, with the normal forms when added paused. rTorrent loads asynchronously, so after the load the client polls the listing (15 times, 200 ms apart) for the hash; if it is still not listed the engine's verification yields `MutationUnknown` and reconciliation follows. rTorrent silently ignores a load for a hash it already holds, so a racing duplicate surfaces as `MutationUnknown`/conflict rather than `AlreadyPresent`.

## Controls

Pause (`d.stop`), Resume (`d.start`), Priority (`d.priority.set` 3 or 2) and Remove without deleting data (`d.erase`), each followed by a verifying read. Deleting data is `UnsupportedFeature` (rTorrent cannot do it over XML-RPC); nothing is erased in that case. No API route or UI invokes controls yet.

## Not claimed

- Live rTorrent compatibility was not exercised. Every observation comes from owned mocks written from the command reference on 127.0.0.1; no installed rTorrent, SCGI socket or ruTorrent was contacted. Method availability by version (`d.multicall2`, `load.raw_start`) is taken from the reference, not observed.
- Direct SCGI (without an HTTP front end) is not supported.
- Seeding limits, ruTorrent label semantics beyond `d.custom1`, a configured download directory per label and the health classifier are not implemented.
- The label alphabet is the shared lowercase family rule even though ruTorrent allows more; existing labels with other characters cannot be configured.
- Migration 0070 is the 70th entry (0065-0068 come from the rename, auto-tagging, collections and torrent RSS lanes; 0075 later reinstalls the provider scope triggers with every lane's implementations).
