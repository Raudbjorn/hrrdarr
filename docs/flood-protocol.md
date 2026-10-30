# Flood protocol

Flood is a web front end for rTorrent, qBittorrent, Transmission or Deluge. It is a torrent download client for TV and movies here and talks to Flood's REST API (`<endpoint>/api/...`) through the same bounded provider transport as the other clients, as the fifth wire on the shared label-torrent engine described in [deluge-protocol.md](deluge-protocol.md) (`src/providers/label_client.rs`; this wire is `src/providers/label_client/flood.rs`).

Protocol source: Flood's REST API as understood from its documentation and route layout (`POST /api/auth/authenticate` issuing a `jwt` cookie, `GET /api/client/connection-test`, `GET /api/client/settings`, `GET /api/torrents`, `GET /api/torrents/{hash}/contents`, `POST /api/torrents/add-urls|add-files|start|stop|delete`). The mocks in `tests/flood_protocol.rs`, `tests/flood_automation.rs` and `frontend/tests/flood-provider-browser.mjs` are written from that description. None of this is evidence from a live-client run.

Requirements were inventoried from `src/NzbDrone.Core/Download/Clients/Flood/` in Sonarr `76c684e097f16ac216e6213845e5cac372774995` and Radarr `c90668a520664ad0c91812cfee57c41928ad2148`: host/port/URL base/SSL, user name and password, tags, add-paused and per-domain priorities. The implementation is independent of both.

## Configuration

`ProviderSettings::Flood { endpoint, tv, movies }`, stored by migration file 0074 (registry row only; the scope triggers and unique label index came with 0070).

- `endpoint` is Flood's base URL including any URL base, for example `http://flood.example:3000`.
- Each scope is the shared `TorrentLabelScope`; `category` is a Flood tag (1-64 of lowercase letters, digits, `.`, `_`, `-`; TV and movie tags must differ). `add_paused` adds the torrent stopped. `recent_priority`/`older_priority` are accepted and stored but not applied (see Controls).
- Credentials are optional and are only `{"kind":"username_password"}`; `api_key` is rejected. Without credentials Flood must run with authentication disabled. The user name, password and the issued session token are redacted from projected strings.

## Session and Test

With credentials, each operation posts them to `/api/auth/authenticate` and sends the returned `jwt` cookie on every request; 400/401/403 is `authentication`. `POST /api/v1/providers/{id}/test` and `POST /api/v1/providers/test-draft` authenticate, require `isConnected: true` from `connection-test` (Flood up but disconnected from its client is a connection failure), read `directoryDefault` from `client/settings` and list the torrents. The reported version is the fixed text `Flood REST API` because Flood publishes none. Test never adds anything.

## Ownership and status

A torrent belongs to a domain when one of its tags equals the domain tag (case-insensitive); of several tags the configured one wins. Status flags map: `checking`, `error`, size 0 is a magnet awaiting metadata, `stopped` is paused, `complete`/`seeding` or all bytes present is completed, `downloading` is downloading, anything else stalled. The save path is `directory`; when it ends with the torrent's own name (a multi-file folder) the parent is used, and file paths from `/contents` are prefixed with the torrent name for multi-file torrents.

## Submission

Same flow as the other label clients (lookup by info hash first; `ScopeConflict` for a hash present untagged or under another tag; never a second add). `add-urls` (magnet) or `add-files` (base64 `.torrent`) carries `tags: [<tag>]` and `start: false` for add-paused, so the tag is part of the add and a lost reply needs no repair: reconcile finds the torrent by hash. After the add the wire waits until the torrent is listed (15 polls, 200 ms apart). `adopt` is not supported.

## Controls

Pause (`stop`), Resume (`start`) and Remove (`delete`, with `deleteData`), each followed by a verifying read. Priority is `unsupported_feature`: Flood changes queue order only through PATCH requests, which the provider transport does not send. No API route or UI invokes controls yet.

## Not claimed

- Live Flood compatibility was not exercised. Every observation comes from owned mocks on 127.0.0.1; no installed Flood was contacted. Paths, JSON shapes (including the `directory` and `contents` path semantics for multi-file torrents) and the cookie name come from the API description, not a live run. The behaviour of the different backends behind Flood is not distinguished.
- Queue placement (`first`), tag edits on existing torrents (PATCH), seeding limits and the health classifier are not implemented.
- Migration file `0074` is ordered by the registry in `src/db/mod.rs`; its filename prefix is not the schema version.
