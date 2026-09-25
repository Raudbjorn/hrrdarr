# Manual and owned-download imports

`POST /api/v1/imports` previews one episode or one movie without transferring bytes:

```json
{"target":{"media_type":"episode","id":1},"source":"/downloads/episode.mkv","mode":"copy","destination":"/tv/Series/Season 01/episode.mkv"}
```

Use `{"media_type":"movie","id":1}` for movies. `copy`, `move` and `hardlink` are distinct modes. The previous `episode_id` request shape remains accepted. Unknown JSON properties are rejected. A preview returns HTTP 202 and an operation UUID. `POST /api/v1/imports/{uuid}/execute` explicitly authorizes the transfer; repeat that request after interruption to resume. `GET /api/v1/imports/{uuid}` exposes the persisted phase and error code. Preview alone never authorizes execution. Old previews without identity journals require a new preview (409).

Execution returns 200 when complete. Invalid input returns 400, missing targets/operations 404, ownership/transfer conflicts 409, local database failures 500, and a remote database 503. Errors use `{ "error": { "code": "...", "message": "..." } }`. Execution serializes imports process-wide; concurrent requests may receive `import_busy` and retry. Disconnecting the HTTP client does not cancel authorized work. The execution permit and database ownership survive a cancelled waiter while blocking filesystem work remains active.

## Delivered boundary

The public manual-preview API remains an initial import into an unassociated target and an absent destination. Existing associations, existing destinations and paths already claimed by either media domain are rejected. Rejection preserves the original; it does not implement an upgrade. One selected episode per request is supported. Existing multi-episode associations are preserved, but creating one file for several episodes is not yet supported.

The service creates the domain's file row, observed size/date metadata, episode association where applicable, and typed import history in one transaction. The movie original source path is retained. For manual previews, quality, languages and media analysis remain unknown unless independently populated; that API does not infer them or inspect codecs. The legacy episode `file_path` projection now resolves through the created file association.

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

An error after DB commit remains visible as `committed` plus an error code. Retry verifies the current association, target root, destination identity/content and source ownership before cleanup. Manual recovery is explicitly invoked through the execute endpoint. Receipt-bound download processing also resumes the same linked journal after worker restart; transfer tasks retain execution ownership while their caller polls persisted status.

## Owned download replacements

An enabled client/domain processing policy authorizes copy or hardlink of a completed, owned download. The processor validates the actual filename against the receipt's single typed episode/movie target and current quality policy. It preserves the validated basename under the existing library root; configurable naming, folder creation and multi-episode imports are not supplied by this contract. Existing destination paths still fail closed. The receipt, operation UUID and immutable quality/revision/edition facts are linked atomically before any transfer. Retrying a linked receipt always resumes that operation; provider or policy changes do not create another transfer.

Owned imports may replace the target's existing association. Their journal archives the original file ID, path, metadata and descriptor identity before staging. The new destination is installed without overwriting, then the association, factual quality metadata and required history commit atomically. Movies preserve the existing movie-file ID while replacing its path and metadata; historical paths/metadata remain in the immutable archive. TV creates a new file and swaps only the selected episode. An original TV file still serving other episodes remains in place.

After commit, an unshared original is moved without overwriting into a journal-owned 0700 `.hrrdarr-replaced-{operation}/original` recovery artifact beside its former path. Its bytes are retained, never pruned. Directory ownership is checkpointed before rename; restart after rename reconciles the recorded artifact and never touches a newly created file at the old pathname. Failure before DB commit leaves the original association and bytes intact. Failure after commit remains a visible incomplete retirement, with the new library file and original/recovery bytes retained. Completion requires successful quarantine or explicit shared-file retention. Retired TV records retain their IDs for snapshot mappings and are excluded from active file APIs/statistics; ordinary unassociated snapshot files remain visible.

Original-file ownership uses device/inode/size/mtime/ctime captured without hashing the old movie. Immediately before rename it requires unchanged metadata; after rename it allows the expected ctime change while checking content identity. New imported bytes still require SHA-256. These checks assume exclusive management of the filesystem and do not defeat a malicious same-privilege concurrent writer. The directory-creation-before-checkpoint gap remains an explicit retained-artifact conflict. Torrent sources stay available for seeding; this path never removes a client item, marks it imported remotely, or unlinks source bytes.

Owned task errors also enter a bounded process-local failure set before error-status persistence. A failed DB error write therefore cannot cause an automatic retry loop while that process lives. An explicit worker resume or manual execute consumes retry authorization and clears prior errors atomically under the execution lease. On process restart, a failure that could not be persisted may undergo one idempotent recovery attempt; durable failure evidence cannot be promised while storage is unavailable. This latch handles returned errors, not arbitrary task panics or process crashes.

## Verification limits

`tests/import_api.rs` exercises the real HTTP router for both domains and all transfer modes, concurrency/retry, managed-source refusal and normalized-path validation. `src/import/tests.rs` reopens isolated databases at publication/commit/cleanup boundaries, injects a transactional history failure, preserves changed/recreated sources, checks known partial stages and descriptor identities, and checks cancellation ownership. `src/import/owned_tests.rs` seeds submission receipts and exercises the real import preparation/execution code: failed replacement commits, database reopen after original quarantine but before checkpoint, reappeared originals, shared TV files and mounted active-file endpoints. `tests/schema_migrations.rs` covers the journal/history constraints.

Cross-device transfers are not exercised by these fixtures; same-device hardlink identity is observed. These checks do not establish full import or automation parity. General manual replacements, multi-episode selection, broader filename matching, naming configuration, codec validation, extras, notifications, permission configuration, configurable recycling/deletion and download-client removal remain outstanding. Owned-download replacement is limited to the validated singleton TV/movie contract above. The full slice-1 gate still requires its other library/provider/UI acceptance criteria; slice-4 upgrade/recovery requirements remain unchanged.
