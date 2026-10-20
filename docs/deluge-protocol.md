# Deluge protocol

Deluge is a torrent download client for TV and movies. It talks to the Deluge Web UI's JSON-RPC (`POST <endpoint>/json`) through the same bounded provider transport as the other clients and shares the torrent path with qBittorrent and Transmission: protocol-aware client selection (Torznab indexers only), the durable submission journal, info-hash reconciliation, download refresh snapshots, completed-download processing, remote path mapping and import.

Deluge is the first client on the shared **label-torrent engine** (`src/providers/label_client.rs`). The engine owns ownership-by-label, the never-double-submit flow, status mapping, completed-download facts, output-root facts and controls; a protocol implements only the `Wire` trait (`src/providers/label_client/deluge.rs`). Migration 0068 prepares the scope triggers and the label unique index for the whole family (`deluge`, `rtorrent`, `utorrent`, `flood`, `aria2`, `downloadstation`), so later members add only a registry row.

Protocol source: Deluge's published Web UI / core / Label plugin RPC (`auth.login`, `web.connected`, `web.get_hosts`, `web.connect`, `daemon.get_version`, `core.get_config_value`, `core.get_torrents_status`, `core.get_torrent_status`, `core.add_torrent_magnet`, `core.add_torrent_file`, `core.pause_torrent`, `core.resume_torrent`, `core.remove_torrent`, `core.queue_top`, `core.queue_bottom`, `label.get_labels`, `label.add`, `label.set_torrent`). The mocks in `tests/deluge_protocol.rs`, `tests/deluge_automation.rs` and `frontend/tests/deluge-provider-browser.mjs` are written from that interface. None of this is evidence from a live-client run.

Requirements were inventoried from `src/NzbDrone.Core/Download/Clients/Deluge/` in Sonarr `76c684e097f16ac216e6213845e5cac372774995` and Radarr `c90668a520664ad0c91812cfee57c41928ad2148`: host/port/SSL/URL base and password, a label (category) and recent/older queue priority per domain, add paused, the Label plugin requirement and the status rules. The implementation is independent of both.

## Configuration

`ProviderSettings::Deluge { endpoint, tv, movies }`, stored by migration 0070.

- `endpoint` is the Web UI base URL, including any URL base, for example `http://deluge.example:8112`. The client appends `/json`.
- Each scope (`TorrentLabelScope`) has `category` (the Deluge label: 1-64 of lowercase letters, digits, `.`, `_`, `-`, the only characters the Label plugin accepts), `recent_priority` and `older_priority` (`last` or `first`; `first` moves a new torrent to the top of the queue), `add_paused` and optional eligibility `tags`.
- TV and movie labels must differ (validation and the unique index `provider_label_categories`).
- Storage reuses `provider_scopes`: label in `category`, queue placement in the priority columns (0 last, 1 first), add paused in `initial_state` (`started`/`stopped`).
- The credential is `{"kind":"api_key","api_key":"<Web UI password>"}`; Deluge's Web UI has no username and always requires a password, so a provider without credentials fails with `invalid_request`. `username_password` is rejected. The password and the session cookie are never projected.

## Session

Each operation logs in (`auth.login`), keeps only the `_session_id` cookie pair and replays it. A `false` login result is `authentication`; a JSON-RPC error with code 1 (expired session) is also `authentication`. The Web UI proxies one daemon: when `web.connected` is false the first host from `web.get_hosts` is attached with `web.connect`, and no known host is a connection failure. `daemon.get_version` (falling back to `daemon.info`) must report 1.3 or later (`UnsupportedVersion` otherwise). A missing `label.get_labels` method means the Label plugin is disabled.

## Test

`POST /api/v1/providers/{id}/test` and `test-draft` log in, check attachment and version, read the default download directory and the declared labels, and list each configured domain's label. The result is `LabelClientTest { client_version, domains, warnings }`. A disabled Label plugin is 422 `label_plugin_unavailable`. Test never creates a label or adds anything; missing labels are created by the first grab.

## Status mapping

`core.get_torrents_status` filtered by label. Ownership is the exact configured label (Deluge labels are lowercase), compared case-insensitively by the engine. States: Downloading with no known size is a magnet awaiting metadata (`queued` / `metadata`), Downloading is `downloading`, Allocating, Checking and Moving are `queued`, Queued is `queued`, Error is `warning` / `error`, Paused is `paused` (or `completed` once all wanted data is present), Seeding with all wanted data present is `completed`. Sizes use `total_wanted`/`total_done`; `progress` of 100 forces complete. Unlabelled torrents and other labels are invisible to both domains. The save path and the per-file list (`core.get_torrent_status` `files`, `file_progress`, `file_priorities`) feed completed-download handling through the shared remote path mapping.

## Submission identity and idempotency

`prepare` resolves a magnet or fetches the `.torrent` and derives the v1 info hash (v2-only releases are `UnsupportedFeature`); the journal stores a tokenized identity (target, hash, settings fingerprint without eligibility tags, payload SHA-256), never the locator. Submission:

1. Connect and require the Label plugin.
2. Look the hash up. Present with this domain's label: `AlreadyPresent`; present under another label or unlabelled: `ScopeConflict`. Nothing is added.
3. Declare the label if absent (`label.add`), a definite non-submission on failure.
4. Add (`core.add_torrent_magnet` / `core.add_torrent_file` with `add_paused`), then `label.set_torrent` and, for a `first` placement, `core.queue_top`. From the add onward a failure is `MutationUnknown` and never retried. A rejected add (for example "already in session") is re-checked: the hash under another label or unlabelled is a visible `ScopeConflict`. A lost reply that leaves an unlabelled torrent with exactly this hash right after our own attempt is labelled and placed (`adopt`) so a later reconcile observes it; the add is not repeated.
5. The result is verified by re-reading the torrent: label and paused state must match.

Seeding limits from the release (`QbitOptions`) are not applied; Deluge's per-torrent ratio settings are not driven by this client.

## Controls

`label_client::control` implements Pause, Resume, Priority (`core.queue_top` / `core.queue_bottom`) and Remove (`core.remove_torrent` with or without data), each followed by a verifying read. Other controls are `UnsupportedFeature`. No API route or UI invokes controls yet.

## Not claimed

- Live Deluge compatibility was not exercised. Every observation comes from owned mocks written from the RPC interface on 127.0.0.1; no installed or default-port (8112, 58846) Deluge was contacted.
- Deluge 1.3 and 2.x differences (for example `daemon.info`, the null reply for a duplicate magnet) were taken from the interface description, not observed.
- The Label plugin's own move-completed and auto-add rules are not read or managed; completed-download handling relies on the torrent's reported save path.
- Seeding limits, import-complete labels, imported-category handling and the health classifier (still reports kind Other) are not implemented.
- Migration 0070 is the 70th entry (the file number equals its array position). It replaces the scope shape, options and client-options triggers for the whole label family and keeps the 0068 `torrentrss` branches (shape, options with `enable_*_search = 0`, client options) intact.
