# Backup and restore

Migration file `0081_backups.sql` (after `0080_library_root_health.sql`; runner position 81 on published main, 83 on the import-list history, or 87 on the legacy history)
adds the backup policy singleton `backup_settings` and durable `Backup` command table.
The accepted histories are described in [import-lists.md](import-lists.md#migration-numbering).
Archives record the actual database ledger; restore validates that exact history, including when
different supported histories share a version number. Archives live on disk; the database stores policy and command history.
Incompatible older migration histories are rejected before writes; they require a separately
validated upgrade/recovery path, not renumbering recorded migrations or deleting database state.
The UI is System → Backup (`#/system/backup`).

Backups cover the **local** libSQL database only. With remote storage (`TURSO_DATABASE_URL`) every
backup/restore endpoint returns 503 `backup_local_only`; use the provider's own backup tooling.

## What a backup contains

| Included | Not included |
| --- | --- |
| The whole database: TV and movie libraries, files, history, commands, profiles, settings, provider and notification configuration | Media files, subtitles, sidecars, recycle-bin contents |
| Provider/notification credentials **as AEAD ciphertext**, exactly as stored | The credential key (`HRRDARR_PROVIDER_KEY`) or any other key material |
| `manifest.json` describing the archive | Environment configuration (bind address, allowed hosts, trusted networks, database path) |
| | External service state (download clients, indexers, notification targets), browser preferences |

A restore therefore needs the archive **and** the same `HRRDARR_PROVIDER_KEY` in the environment of
the restored instance. Without it the instance starts and the library is intact, but sealed
credentials cannot be used until the key is supplied (or credentials are re-entered).

## Archive format

`hrrdarr_backup_<YYYYMMDDTHHMMSSZ>_<8 hex>.zip` under `<folder>/{manual,scheduled,update}/`. It is
a standard stored (uncompressed) ZIP that ordinary tools list and extract, holding exactly:

- `manifest.json`: `format` (1), `application`, `application_version`, `backup_type`,
  `created_at`, `backend` (`local-libsql`), `sqlite_version`, `schema_version`, the full
  `migrations` prefix (`version`, `name`, SHA-256 `checksum` of each migration's SQL),
  `files` (`database.db` size and SHA-256), `includes`, `excludes`, and `secrets`
  (`encrypted_credentials` count, `key_included: false`, `key_source`).
- `database.db`: a self-contained rollback-journal SQLite file.

The reader accepts only this strict layout: the two allow-listed entry names once each,
contiguous records, CRC-32 and sizes cross-checked between local and central records, no
compression/encryption/ZIP64/extra fields/trailing data. Entry names are compared with constants
and never used as paths, so archive paths cannot escape staging. Without ZIP64 an archive is at
most 4 GiB − 1 byte; larger databases fail with `backup_too_large`. No dependency was added: the
container is ~250 lines in `src/backup/archive.rs` (multipart upload support would have required
a new crate, so uploads use a raw request body, as the snapshot importer does).

## Creating a backup

`POST /api/v1/system/backup-commands` with `{}` enqueues a manual backup (202). A scheduled backup
is enqueued by the command worker once per `interval_days` (1–7, default 7); the first slot is one
interval after the policy is first observed. At most one backup command is active (unique index),
so the command is independent of the shared 1024-row command pool; a request while one is active
returns the active command. `GET /api/v1/system/backup-commands[?limit&offset]` and
`/{id}` read history (latest 100 terminal rows retained).

The worker runs each backup inside one iteration:

1. Resolve the folder and refuse (`backup_folder_unsafe`) a folder that equals, contains or lies
   inside a root folder, series folder, movie folder or recycle bin of either domain (lexical).
2. Create a private (`0700`) unique staging directory `<folder>/.staging/<uuid>/`; stale staging
   directories older than a day are removed.
3. Refuse databases above the size bound, then `VACUUM main INTO` the staging file: one read
   transaction, so the copy is consistent while WAL writers continue. The SQL runs on a blocking
   thread and is interrupted at the run's time budget (35 s, below the worker's 45 s iteration).
4. Convert the copy to `journal_mode=DELETE`, then `integrity_check`, `foreign_key_check` and a
   migration-history check against this build. Any failure discards it (`snapshot_invalid`).
5. Hash it, write the manifest and the archive into staging (`0600`), `fsync`, then re-read the
   archive through the same validator restores use (structure, CRCs, manifest, SHA-256).
6. Publish by hard link (no-clobber; falls back to an existence-checked rename) into the type
   folder and `fsync` the directory. Staging is removed on every path.
7. Only then, and only for `scheduled` and `update` types, remove backups of the **same type**
   older than `retention_days` (1–90, default 28). Manual backups are only deleted explicitly.
   Files whose names are not generated backup names, and symlinks, are never listed or pruned.

`update` backups: when startup applies migrations, the runner's pre-migration snapshot directory
(`.hrrdarr-migration-<uuid>/`, retained as before) is also published as an `update` archive with
the old schema version. Failure is logged (`event=backup_failed`) and does not block startup.

Failed commands record one of `backup_folder_unavailable`, `backup_folder_unsafe`,
`backup_too_large`, `backup_timeout`, `snapshot_failed`, `snapshot_invalid`, `archive_failed`,
`publish_failed`, `storage_error` or `interrupted`. Nothing is published or pruned on failure. An
interrupted command is failed, not retried.

## Listing, download and delete

- `GET /api/v1/system/backups[?limit=1..100&offset&backup_type]` → `ApiPage<BackupFile>` sorted
  newest first over the full inventory (up to 100,000 matching generated regular archives;
  offsets through 100,000 are accepted). Larger inventories return 503
  `backup_inventory_too_large`, with no partial totals.
- `GET /api/v1/system/backups/{id}` → `{backup, manifest}`.
- `GET /api/v1/system/backups/{id}/download` streams the archive (`application/zip`,
  `Content-Disposition: attachment`, `Cache-Control: no-store`).
- `DELETE /api/v1/system/backups/{id}` removes one archive (204); the UI asks for confirmation.

`id` is the first 32 hex digits of SHA-256 over `<type>/<name>`. It must match `^[0-9a-f]{32}$`
(otherwise 400) and is resolved only against generated names of regular files inside the
folder (otherwise 404). Known-ID lookup uses constant memory and works independently of the
materialized listing cap. Clients never supply a path.

Inventory requests share one process-wide scan permit. The ten-second HTTP timeout includes
waiting for admission; each admitted scan has its own ten-second cooperative deadline.
The blocking scan retains the permit until it actually
finishes, even after an HTTP timeout or disconnect; subsequent scans cannot accumulate behind
an abandoned scan. An operating-system filesystem call that blocks cannot be forcibly stopped.
Retention scans also honor the existing backup deadline.

## Restore

- `POST /api/v1/system/backups/{id}/restore` stages a listed backup.
- `POST /api/v1/system/restore/upload` stages an uploaded archive sent as the raw request body
  (`Content-Type: application/zip` or `application/octet-stream`; anything else is 415). A
  declared length over 4 GiB − 1 is refused before reading (413); the streamed body is bounded
  independently.
- `GET /api/v1/system/restore` → `{pending}`; `DELETE /api/v1/system/restore` cancels (204/404).

Staging validates everything before anything is published: archive structure and CRCs, manifest
format/application/backend, `key_included: false`, the migration prefix (each name and checksum
must equal this build's; a newer `schema_version` is `backup_schema_unsupported`, a mismatch
`backup_history_mismatch`), the database's size and SHA-256 against the manifest
(`backup_checksum_mismatch`), then a **read-only** open of the extracted database for
`integrity_check`/`foreign_key_check` (`backup_integrity_failed`) and its own migration history
(newer rows are `backup_schema_unsupported`; its version must equal the manifest's). Other
structural problems are `backup_archive_invalid` (422). Older schemas are accepted and migrate at
the next startup (with the usual pre-migration and `update` backups).

A valid candidate is staged in a private `.<db>.restore-staging-<uuid>/` beside the database and
published by one directory rename to `.<db>.restore-pending/` (`database.db` + `restore.json`).
Only one restore can be pending (409 `restore_already_pending`). Live state is unchanged; the
response says `restart_required: true`.

### Applying at startup

Before the database is opened, startup consumes the pending marker:

1. Acquire the persistent `.<db>.restore-lock` before inspecting pending state, including when
   the live database is absent. A competing restore operation refuses to proceed. The same lock
   protects staging, status reads and cancellation; it must not be removed or renamed.
   Also refuse to replace a database held by a running application through its live database lock.
2. Re-hash the staged database against the marker. A mismatch, an unreadable marker or a missing
   staged database (before anything moved) quarantines the candidate as
   `.<db>.restore-rejected-<stamp>-<rand>/`, logs `event=backup_restore_rejected` and starts on
   the unchanged live database.
3. Record the name of `.<db>.restore-original-<stamp>-<rand>/` in the pending directory, create it, move
   the live database and its `-wal`/`-shm`/`-journal` companions into it, then rename the staged
   database into place, with directory `fsync`s between steps.
4. Write `restore-applied.json` into the original directory and remove the pending directory.
   Logs `event=backup_restore_applied original=<dir>`.

An interruption between steps resumes on the next start using the recorded original directory
(no second original is created); if the candidate was already installed, the live file must match
the recorded checksum or startup stops with instructions instead of opening an empty database.
An interrupted `original.partial` marker is discarded and rewritten before any live files move;
unexpected symlinks or nonregular marker files stop startup. The retention directory is created
only after publishing its marker, so interrupted marker writes do not accumulate unused directories.
The original directory is never removed automatically: it is the recoverable copy of the replaced
state. To roll back, stop every hrrdarr process using that database and keep it stopped throughout
the replacement. Move the current live database **and all its existing `-wal`, `-shm` and
`-journal` companions** to a separate recovery directory first. Then copy the retained original
as one consistent set: `database.db` becomes the configured `HRRDARR_DATABASE_PATH` (default
`hrrdarr.db`), and each retained `database.db-wal`, `database.db-shm` or `database.db-journal`
becomes that configured path plus the matching suffix. For example, `/data/library.db` requires
`database.db-wal` to become `/data/library.db-wal`. Leave absent original companions absent;
never mix them with companions produced by the restored database. Retain both recovery copies
until the rollback is verified, then restart hrrdarr. If an interrupted apply still has a pending
directory, move that entire directory aside while stopped before restarting, so it cannot reapply
the candidate over the rolled-back database. Leave the persistent restore lock file in place.

The restored database contains its own policy rows, so a restored `folder` setting points where the
source instance wrote backups; check it after restoring onto another host. The snapshot also
captures its own `Backup` command as `running`; worker recovery marks it `failed`/`interrupted`.

## Settings

`GET`/`PUT /api/v1/system/backup-settings`: `{revision, folder, interval_days, retention_days}`
(revision compare-and-swap; `folder` is required, `null`/blank means `Backups` beside the database
file). The response adds `effective_folder` and `next_backup_at`. A folder must be absolute and
normalized (no `.`/`..`, control characters or backslashes, not `/`), must not overlap any library
path or recycle bin (409 `backup_folder_overlaps_library`, rechecked under the write lock), and is
created (`0700`) and probed with a temporary file before saving (422 `backup_folder_unwritable`).
Changing the interval moves the next scheduled backup to one new interval from now.

## Verification

`tests/backup_restore.rs` (real HTTP, scratch directories): manual and scheduled backups of a
database whose series/episode/movie and file IDs collide, manifest checksums checked with an
independent ZIP reader, ciphertext present and plaintext/key (hex and raw bytes) absent from the
archive bytes; retention pruning only after success and never of manual or foreign files; a
mid-archive failure publishes and prunes nothing; settings path validation; corrupt, truncated,
tampered, foreign-key-broken, newer-schema, history-mismatched, key-claiming, traversal and
oversized archives refused with no staged or live change; traversal-shaped ids and planted
symlinks refused; restore by id applied at restart with the original retained, interrupted-apply
resume and staged-tamper quarantine; an uploaded archive restored into an **isolated binary
instance** with a fresh data directory, read back over HTTP (series, movie, episode, provider);
and an upgrade that publishes an `update` backup which restores and re-migrates. Unit tests cover
the ZIP container, CRC-32, names, manifest prefix rules and the streamed upload bound.
`frontend/tests/backup.test.mjs` covers pure helpers and client request shapes;
`frontend/tests/backup-browser.mjs` drives the page against the owned UI fixture.

## Not claimed

- Restores were applied by restarting the process in tests, never by a supervisor or service
  manager; hrrdarr does not restart itself after staging.
- Decrypting restored credentials with the environment key was not exercised by a provider call;
  the tests prove the restored ciphertext is byte-identical and that no key material is archived.
- Backup authenticity: checksums detect corruption and inconsistent edits, but an attacker who can
  rewrite both the database and the manifest can produce an archive that validates. Archives are
  not encrypted or signed; protect the backup folder (`0700`/`0600` are applied to what hrrdarr
  creates, not to a pre-existing configured folder).
- Overlap checks are lexical; bind mounts and symlink aliases of library folders are not detected.
  Download uses the listed path and follows a symlink swapped in after listing; the folder is
  assumed to be writable only by hrrdarr.
- Very large databases: the command's 35 s budget and the 4 GiB format limit were not measured on
  real large libraries; such backups fail rather than run in the background.
- Disk-full, `fsync` failure and power loss were reasoned about, not injected; the resume path was
  tested by simulating the on-disk state of one interrupted step.
- Remote/Turso storage has no backup support. Multiple instances sharing one backup folder were not
  tested. Multipart uploads are not accepted (raw body only), unlike upstream.
- No Sonarr/Radarr backup archive can be restored here; their snapshots use the snapshot importer.
