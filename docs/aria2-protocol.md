# Aria2 protocol

Aria2 is a torrent download client for TV and movies. It talks to the JSON-RPC endpoint (`<endpoint>`, typically `http://host:6800/jsonrpc`) through the same bounded provider transport as the other clients, as the fourth wire on the shared label-torrent engine described in [deluge-protocol.md](deluge-protocol.md) (`src/providers/label_client.rs`; this wire is `src/providers/label_client/aria2.rs`).

Protocol source: aria2's published RPC manual (`aria2.getVersion`, `getGlobalOption`, `tellActive|tellWaiting|tellStopped|tellStatus`, `addUri`, `addTorrent`, `pause`, `unpause`, `remove`, `removeDownloadResult`, `changePosition`, the `token:<secret>` leading parameter and string-typed numbers). The mocks in `tests/aria2_protocol.rs`, `tests/aria2_automation.rs` and `frontend/tests/aria2-provider-browser.mjs` are written from that description. None of this is evidence from a live-client run.

Requirements were inventoried from `src/NzbDrone.Core/Download/Clients/Aria2/` in Sonarr `76c684e097f16ac216e6213845e5cac372774995` and Radarr `c90668a520664ad0c91812cfee57c41928ad2148`: host/port/RPC path/SSL and an RPC secret, plus a recent/older priority per domain. The implementation is independent of both.

## Configuration

`ProviderSettings::Aria2 { endpoint, tv, movies }`, stored by migration 0072 (registry row only; the scope triggers and unique label index came with 0069, and 0075 reinstalls the trigger set with every lane's implementations).

- `endpoint` is the full JSON-RPC URL; nothing is appended.
- Each scope is the shared `TorrentLabelScope`. aria2 has no labels, so `category` names a subfolder of aria2's own download directory (1-64 of lowercase letters, digits, `.`, `_`, `-`; TV and movie values must differ). `recent_priority`/`older_priority` (`first` moves a new download to the queue front), `add_paused` and `tags` behave as for the other clients.
- Credentials are optional and are only `{"kind":"api_key"}`, the RPC secret sent as the leading `token:<secret>` parameter. `username_password` is rejected. The secret is redacted from projected strings.

## Ownership

A download belongs to a domain when its `dir` is `<aria2 default dir>/<label>` (default dir from `aria2.getGlobalOption`). Downloads elsewhere, in a deeper subfolder, without an info hash (HTTP/FTP) or that are metadata stubs with a non-empty `followedBy` are not listed. A hash that exists outside the domain's folder is a `ScopeConflict`; aria2's own duplicate refusal is never worked around.

## Test

`POST /api/v1/providers/{id}/test` and `test-draft` call `aria2.getVersion` (floor 1.18) and `aria2.getGlobalOption` and list each configured domain. Test never adds anything. `Unauthorized` (aria2 answers HTTP 400) is `authentication`.

## Status mapping

`active` with all bytes present is `completed` (seeding), otherwise `downloading`; `waiting` is `queued`; `paused` is `paused`; `complete` is `completed`; `error` is `warning` / `error`; an active download with no known size is `queued` / `metadata`. File paths are made relative to `dir`; the save path is `dir`.

## Submission

Same flow as the other label clients (lookup by info hash first; never a second add). `addUri` (magnet) or `addTorrent` (base64 `.torrent`) with options `dir=<default>/<label>` and `pause=true` when the scope adds paused, then a best-effort `changePosition` to 0 for a `first` placement. The domain is part of the add, so a lost reply needs no repair: reconcile finds the download in the label folder by hash. `adopt` is not supported.

## Controls

Pause, Resume, Priority (`changePosition`) and Remove (`remove`, falling back to `removeDownloadResult` for finished downloads). Deleting files is `unsupported_feature` because aria2 has no RPC for it. No API route or UI invokes controls yet.

## Not claimed

- Live aria2 compatibility was not exercised. Every observation comes from owned mocks on 127.0.0.1; no installed aria2c was contacted. Field names, string typing and the `Unauthorized` error come from the RPC manual, not a live run.
- WebSocket and XML-RPC transports, per-file selection, seeding limits (`seed-ratio`, `seed-time`) and the health classifier are not implemented.
- Migration 0072 is the 72nd entry (0065-0068 come from the rename, auto-tagging, collections and torrent RSS lanes; 0075 later reinstalls the provider scope triggers with every lane's implementations).
