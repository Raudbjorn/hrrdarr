# Download Station protocol

Synology Download Station is a torrent download client for TV and movies here. It talks to DSM's Web API (`<endpoint>/webapi/auth.cgi`, `/webapi/DownloadStation/info.cgi`, `/webapi/DownloadStation/task.cgi`) through the same bounded provider transport as the other clients, as the sixth wire on the shared label-torrent engine described in [deluge-protocol.md](deluge-protocol.md) (`src/providers/label_client.rs`; this wire is `src/providers/label_client/downloadstation.rs`).

Protocol source: Synology's Download Station Web API guide as understood from its documentation (`SYNO.API.Auth` login/logout with `session=DownloadStation&format=sid`, `SYNO.DownloadStation.Info` `getinfo`/`getconfig`, `SYNO.DownloadStation.Task` `list`/`getinfo`/`create`/`pause`/`resume`/`delete`, the `additional=detail,transfer,file` expansions and the numeric error codes). The mocks in `tests/downloadstation_protocol.rs`, `tests/downloadstation_automation.rs` and `frontend/tests/downloadstation-provider-browser.mjs` are written from that description. None of this is evidence from a live-DSM run.

Requirements were inventoried from `src/NzbDrone.Core/Download/Clients/DownloadStation/` in Sonarr `76c684e097f16ac216e6213845e5cac372774995` and Radarr `c90668a520664ad0c91812cfee57c41928ad2148`: host/port/SSL, user name and password, a destination folder per domain. The implementation is independent of both.

## Why the destination carries the identity

Download Station reports tasks by its own ids (`dbid_...`) and never an info hash, while the engine identifies a download by hash across restarts. A magnet task does carry its hash in `additional.detail.uri`, but an uploaded `.torrent` does not. So each torrent is created with the destination `<default destination>/<label>/<info hash>` (relative to the shared folders, as DSM expects). A task belongs to a domain when its destination has exactly that shape, and its hash is the last component. A task elsewhere whose magnet URI names a hash is reported with no label, so a grab of a hash someone already added by hand is a visible `ScopeConflict` rather than a duplicate. Tasks with neither (plain HTTP downloads, other folders without a magnet) are ignored. The visible consequence is one folder per torrent below the domain folder on the NAS; the importer sees `/<destination>` as the save path and reads files relative to it.

Download Station does not create missing destination folders. This implementation currently sends each release to a new `<label>/<hash>` folder without creating it through File Station; real-NAS grabs can fail with error 403 (destination does not exist). The owned mocks do not establish successful live submission.

## Configuration

`ProviderSettings::Downloadstation { endpoint, tv, movies }`, stored by migration file 0074 (registry row only; the scope triggers and unique label index came with 0069, and 0075 reinstalls the combined trigger set).

- `endpoint` is DSM's base URL, for example `https://nas.example:5001`. Use HTTPS: credentials are sent in the login form.
- Each scope is the shared `TorrentLabelScope`; `category` is the folder name below the default destination (1-64 of lowercase letters, digits, `.`, `_`, `-`; the complete names `.` and `..` are forbidden; TV and movie values must differ). `add_paused` pauses the task after it is created. `recent_priority`/`older_priority` are stored but not applied.
- Credentials are required and are only `{"kind":"username_password"}`; `api_key` is rejected. User name, password and the session id are redacted from projected strings. Accounts that require a second factor (OTP) cannot be used and fail as `authentication`.

## Session and Test

Every operation logs in (`SYNO.API.Auth` version 6) and, whether it succeeded or not, logs out afterwards through the engine's `Wire::finish` hook, so sessions do not accumulate on the NAS. All calls are form POSTs without a query string, so secrets never appear in URLs or access logs. Login error codes 400-410 and session errors (105-107, 119, 120) map to `authentication`; task id 404 is `not_found`.

`POST /api/v1/providers/{id}/test` and `test-draft` log in, read `getinfo` (`version_string` is the reported version) and `getconfig` (`default_destination`, required: without it domains cannot be isolated and the result is `unsupported`), list tasks and each domain's downloads. Test never creates anything.

## Status mapping

`waiting` is `queued`; `paused`; `error`; `hash_checking`/`extracting` are `checking`; `finished`/`seeding`/`finishing` and `downloading` with every byte present are completed; `downloading` with size 0 is a magnet awaiting metadata (`queued` / `metadata`); other statuses are `unknown`. File names come from `getinfo&additional=file` and are relative to the destination; `priority: skip` is unwanted.

## Submission

Same flow as the other label clients (lookup by info hash first; never a second add). `create` with `uri` (magnet) or a multipart `file` upload (fields before the file) and the `destination`; the wire then waits until the task is listed (15 polls, 200 ms apart) and pauses it when the scope adds paused. A pause that fails after the create is `MutationUnknown`. The destination is part of the create request, so a lost reply needs no repair: reconcile finds the task by hash. `adopt` is not supported.

Destination folders must already exist: this wire does not create the per-torrent folder through File Station. A real NAS can reject task creation with error 403 (destination does not exist); owned mocks do not establish successful live grabs.

## Controls

Pause, Resume and Remove (`delete` with `force_complete=false`), each followed by a verifying read. Priority and deleting downloaded data are `unsupported_feature`: Download Station has no API for either. No API route or UI invokes controls yet.

## Not claimed

- Live DSM compatibility was not exercised. Every observation comes from owned mocks on 127.0.0.1 over plain HTTP; no NAS was contacted. API names, parameters, `additional` shapes, status strings (DSM may report numeric statuses on newer versions, which would map to `unknown`), error codes and the destination semantics (relative to shared folders) come from the documentation, not a live run.
- The save path reported to the importer is the shared-folder path with a leading slash (for example `/downloads/tv/<hash>`); mapping it to the host's real volume path is the job of a remote path mapping. The real volume path is not read from DSM.
- Two-factor accounts, queue placement, seeding limits and the health classifier are not implemented. The `SYNO.API.Info` path discovery is not performed; the documented fixed paths are used.
- Migration file `0074` is ordered by the registry in `src/db/mod.rs`; its filename prefix is not the schema version.
