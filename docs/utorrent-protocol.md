# uTorrent protocol

uTorrent is a torrent download client for TV and movies. It talks to the Web UI (`<endpoint>/token.html`, `<endpoint>/?token=...&list=1`, `<endpoint>/?token=...&action=...`) through the same bounded provider transport as the other clients, as the third wire on the shared label-torrent engine described in [deluge-protocol.md](deluge-protocol.md) (`src/providers/label_client.rs`; this wire is `src/providers/label_client/utorrent.rs`).

Protocol source: uTorrent's published Web UI API (the `token.html` handshake, the `list=1` torrent rows with their fixed positions, permille progress and the status bit field, and `action=getsettings|getfiles|add-url|add-file|setprops|stop|start|pause|unpause|remove|removedata|queuetop|queuebottom`). The mocks in `tests/utorrent_protocol.rs`, `tests/utorrent_automation.rs` and `frontend/tests/utorrent-provider-browser.mjs` are written from that description. None of this is evidence from a live-client run.

Requirements were inventoried from `src/NzbDrone.Core/Download/Clients/UTorrent/` in Sonarr `76c684e097f16ac216e6213845e5cac372774995` and Radarr `c90668a520664ad0c91812cfee57c41928ad2148`: host/port/URL base/SSL and user name and password, a label and recent/older priority per domain, "start" versus "stop" on add, the token handshake and the status bit rules. The implementation is independent of both.

## Configuration

`ProviderSettings::Utorrent { endpoint, tv, movies }`, stored by migration 0071 (registry row only; the scope triggers and unique label index came with 0069, and 0075 reinstalls the trigger set with every lane's implementations).

- `endpoint` is the Web UI base including `/gui`, for example `http://utorrent.example:8080/gui`.
- Each scope is the shared `TorrentLabelScope`: `category` (the label: 1-64 of lowercase letters, digits, `.`, `_`, `-`), `recent_priority`/`older_priority` (`last` or `first`; `first` moves a new torrent to the queue top), `add_paused` and optional eligibility `tags`. TV and movie labels must differ.
- Credentials are required and are only `{"kind":"username_password"}` (HTTP basic authentication); `api_key` is rejected and a provider without credentials fails with `invalid_request`. The user name must not contain `:`. Both values and the session token are redacted from projected strings.

## Session

Each operation fetches `token.html` with basic credentials, takes the token from the `id='token'` element (letters, digits, `-`, `_`, at most 128 bytes; anything else is `InvalidResponse`) and the `GUID` cookie, and sends both on every request. A 400/401/403 answer to a GUI request is `authentication` (an expired token or cookie). A wrong URL base is a connection failure. The `build` reported by `list=1` must be present; versions are shown as `build N`.

## Test

`POST /api/v1/providers/{id}/test` and `test-draft` perform the handshake, `list=1`, `getsettings` (default and completed download directories) and list each configured domain's label. The result is `LabelClientTest { client_version, domains, warnings }`. Test never adds or labels anything.

## Status mapping

Rows are read by position (hash, status bits, name, size, progress in permille, ratio in permille, rates, ETA, label, ..., root download path at index 26). Status bits: checking is `queued`; size 0 is a magnet awaiting metadata (`queued` / `metadata`); error is `warning` / `error`; paused (32), or not started and not queued (stopped), is `paused` (or `completed` once progress is 1000); queued without start is `queued`; started and progress 1000 is `completed`; otherwise `downloading`. The label is the row's label; ownership compares it case-insensitively.

The save path is the row's root download path; when it ends with the torrent's own name (a multi-file torrent's folder, or a single file) the parent is used. Names from `getfiles` have backslashes normalized; for a multi-file torrent they are relative to its own folder and are prefixed with the torrent name, a single-file torrent's name is used as is. Which of the two shapes a real uTorrent reports for the root path is taken from the API description, not observed.

## Submission

Same flow as the other label clients (lookup by hash first; `ScopeConflict` for a hash present unlabelled or under another label; never a second add). `add-url` (magnet) or a multipart `add-file` upload (part `torrent_file`) starts the torrent; the client then waits for it to be listed (15 polls, 200 ms apart), sets the label (`setprops s=label`), stops it when the scope adds paused, and moves it to the top for a `first` placement. A failure after the add is `MutationUnknown`. If the add succeeded but its reply (or the labelling) was lost, the engine labels an unlabelled torrent with exactly this hash right after the attempt, so a later reconcile observes it.

## Controls

Pause (`pause`), Resume (`unpause` then `start`), Priority (`queuetop`/`queuebottom`) and Remove (`remove`, or `removedata` to delete files), each followed by a verifying read. No API route or UI invokes controls yet.

## Not claimed

- Live uTorrent compatibility was not exercised. Every observation comes from owned mocks on 127.0.0.1; no installed uTorrent or BitTorrent Web UI was contacted. Row positions, status bits and action names come from the API description, not a live run, and no minimum build is enforced beyond the field being present.
- The newer "uTorrent Web" (non-`/gui`) API and the relocation of completed torrents are not handled; the root download path is used as reported.
- Seeding limits, torrent-level priorities and the health classifier are not implemented.
- Migration 0071 is the 71st entry (0065-0068 come from the rename, auto-tagging, collections and torrent RSS lanes; 0075 later reinstalls the provider scope triggers with every lane's implementations).
