# Initial manual imports

`POST /api/v1/imports` previews one episode or one movie without transferring bytes:

```json
{"target":{"media_type":"episode","id":1},"source":"/downloads/episode.mkv","mode":"copy","destination":"/tv/Series/Season 01/episode.mkv"}
```

Use `{"media_type":"movie","id":1}` for movies. `copy`, `move` and `hardlink` are distinct modes. The previous `episode_id` request shape remains accepted. Unknown JSON properties are rejected. A preview returns HTTP 202 and an operation UUID. `POST /api/v1/imports/{uuid}/execute` explicitly authorizes the transfer; repeat that request after interruption to resume. `GET /api/v1/imports/{uuid}` exposes the persisted phase and error code. Preview alone never authorizes execution. Old previews without identity journals require a new preview (409).

Execution returns 200 when complete. Invalid input returns 400, missing targets/operations 404, ownership/transfer conflicts 409, local database failures 500, and a remote database 503. Errors use `{ "error": { "code": "...", "message": "..." } }`. Execution serializes imports process-wide; concurrent requests may receive `import_busy` and retry. Disconnecting the HTTP client does not cancel authorized work. The execution permit and database ownership survive a cancelled waiter while blocking filesystem work remains active.

## Delivered boundary

This is an initial import into an unassociated target and an absent destination. Existing associations, existing destinations and paths already claimed by either media domain are rejected. Rejection preserves the original; it does not implement an upgrade. One selected episode per request is supported. Existing multi-episode associations are preserved, but creating one file for several episodes is not yet supported.

The service creates the domain's file row, observed size/date metadata, episode association where applicable, and typed import history in one transaction. The movie original source path is retained. Quality, languages and media analysis remain unknown unless independently populated; this service does not infer them or inspect codecs. The legacy episode `file_path` projection now resolves through the created file association.

## Filesystem and recovery contract

The initial implementation targets Linux and a locally owned database. Paths must be absolute, normalized UTF-8 strings, at most 4096 bytes, with no symlink components. Duplicate separators, `.` and `..` spellings are rejected. The library root and destination parent must already exist; the importer does not create missing library/season directories. It captures root, parent and source identities during preview and verifies them through descriptor-relative, no-follow operations. Root/device changes after preview fail closed. This does not establish that a declared root was mounted correctly before the preview.

Sources must be outside every managed library root and must not be another tracked media path. Ownership comparisons use path components and are repeated before execution and move cleanup. This initial bounded check supports at most 10,000 combined root/file records; larger inventories return `library_guard_limit`. External hardlink aliases are not a general ownership-discovery mechanism.

Files must be regular, nonempty and no larger than 256 GiB. Streaming uses a 1 MiB buffer and a cooperative 30-minute elapsed budget per pass. This budget is checked between reads/writes; it cannot interrupt a blocked kernel filesystem call. Network-filesystem and hostile concurrent-writer guarantees are not claimed.

The journal phases are:

1. `preview`: immutable operation, target, mode, paths and identities.
2. `staging`: a private 0700 operation directory and staging file, followed by recorded identities. A known partial copy can be overwritten only within that owned stage. An artifact created before its identity was journaled is preserved and reported as `unowned_artifact` for inspection. This same narrow gap applies to creation of the source-side quarantine directory.
3. `staged`: completed bytes, observed identity and SHA-256 are persisted. Copy/move stream the source into staging; hardlink preserves inode identity and refuses cross-device fallback.
4. `published`: an atomic, non-overwriting hardlink installs the staged file at the destination. Replays require the same inode and content, never merely an existing path.
5. `committed`: file, metadata, association and history are committed together. Failed transactions leave source and staged/published data available for retry.
6. `complete`: required source/stage cleanup finished. For move, cleanup first journals a private source-side quarantine directory, moves the original there without overwriting, verifies it, and persists retirement before unlinking. A recreated source path is never blindly deleted. Copy and hardlink retain their sources.

Directory/file synchronization follows creation, publication, quarantine and unlink. Empty private directories remain as ownership receipts; automatic receipt retention/cleanup is not implemented. Copy/move staging files are created with private 0600 permissions; configurable media permissions are not yet implemented. Hardlinks naturally share source permissions and subsequent source mutations. Hash checks compare metadata before and after reads to detect ordinary concurrent changes, but require exclusive management of media during an import; they are not protection against a malicious process with the same filesystem privileges.

An error after DB commit remains visible as `committed` plus an error code. Retry verifies the current association, target root, destination identity/content and source ownership before cleanup. Recovery is explicitly invoked through the execute endpoint; no startup scheduler or automatic retry queue is introduced.

## Verification limits

`tests/import_api.rs` exercises the real HTTP router for both domains and all transfer modes, concurrency/retry, managed-source refusal and normalized-path validation. `src/import/tests.rs` reopens isolated databases at publication/commit/cleanup boundaries, injects a transactional history failure, preserves changed/recreated sources, checks known partial stages and descriptor identities, and checks cancellation ownership. `tests/schema_migrations.rs` covers the journal/history constraints.

Cross-device transfers are not exercised by these fixtures; same-device hardlink identity is observed. These checks do not establish full import or automation parity. Replacement/upgrade retirement, multi-episode selection, scanning/parsing, naming configuration, codec validation, extras, notifications, permission configuration, recycle/delete, automatic restart scheduling and download-client handoff remain outstanding. The full slice-1 gate still requires its other library/provider/UI acceptance criteria; slice-4 upgrade/recovery requirements remain unchanged.
