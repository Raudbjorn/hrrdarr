# File deletion and recycle bin

Migration 0059 adds per-domain recycle bin settings, a file-deletion journal, deletion history and
the `CleanUpRecycleBin` command. TV and movies are configured, recycled, cleaned and health-checked
independently; equal numeric file IDs in the two domains never select each other.

## Settings

`GET`/`PUT /api/v1/{tv|movies}/recycle-bin` reads and writes
`{media_type, revision, path, cleanup_days, next_cleanup_at}`. A write must send the current
`revision` (compare-and-swap; a stale revision returns 409 `recycle_bin_conflict`) and an explicit
`path` (`null` clears the bin). `cleanup_days` is 0–36500; 0 disables automatic cleanup.

A path must be absolute, normalized (no `.`/`..`, repeated or trailing separators, control
characters), at most 4096 bytes and not `/`. It must not equal, contain or lie inside any root
folder, series folder or movie folder of either domain. Both directions are enforced: the settings
write checks Rust and SQL, and root-folder/series/movie insert and update triggers refuse a library
path that would overlap a configured bin. TV and movies may share one bin but cannot nest. Overlap
is lexical; bind mounts or symlinked aliases of the same directory are not detected. Saving does
not create the folder: Health reports a missing or unwritable bin.

The first scheduled cleanup is one day after choosing or changing the path. The UI is under
Settings → Media management (`/settings/mediamanagement`).

## Deleting files

`DELETE /api/v1/{tv|movies}/files/{id}` with `{"mode":"recycle"|"permanent"}` and
`DELETE /api/v1/{tv|movies}/files/bulk` with `{"file_ids":[...],"mode":...}` (1–50 distinct IDs)
are the only deletion entry points. The mode is the user's confirmed intent, not a hint:

- `recycle` requires a configured bin (otherwise 409 `recycle_bin_not_configured`).
- `permanent` requires that no bin is configured (otherwise 409 `recycle_bin_configured`).
  There is no "skip the bin" override.

The whole request is validated before any effect: every ID must exist in that domain (404
otherwise), each owner folder's root must exist and contain subfolders (missing/unmounted roots
return 409 `library_root_missing`/`library_root_empty`), and each file must lie inside its owner
folder without symlinked components. Files still referenced by import or scan journals return
409 `file_reserved_by_import`; owners with in-flight imports, rescans or downloaded scans return
409 `library_busy`. Deletion holds the same global effect permit as imports and scans (409
`import_busy` while another file operation runs) and triggers prevent new import/scan references
to a file while its deletion is active.

A TV file that serves several episodes is one deletion: every episode association is detached and
one history row per episode records it. A movie loses its file association. File metadata rows are
removed with the file record.

### Recycle layout and transfer

The file moves to `<bin>/<series or movie folder name>/<path relative to that folder>`. An
existing name gets a `name_N.ext` suffix; nothing in the bin is overwritten. Same-device bins use
a single no-replace rename (inode kept). Cross-device bins copy into a private partial file, fsync,
re-read and compare SHA-256, publish with no-replace, commit the database, then remove the original
only after re-verifying its device/inode/size/mtime. A permanent deletion unlinks the verified file
and then commits. A file already missing from disk is recorded as `already_missing` and its record
is removed without touching the bin.

### Journal and recovery

Each file gets a `file_deletions` row before any filesystem effect, with the source identity,
planned destination and mode. Phases: `admitted` → (`copied`) → `committed` → `complete`, or
`aborted`. No database transaction spans a file transfer. Recovery runs at worker startup (bounded
wait for the effect permit) and before every new deletion batch:

- `admitted`, rename: if the file is at the destination and gone from the source, roll forward
  (commit); if still at the source, abort.
- `admitted`/`copied`, copy: a verified published copy plus an unchanged source rolls forward;
  otherwise partial/unverified copies are removed and the row aborts. The source is never removed
  before commit.
- `committed`: remove the original only if it still matches the recorded identity, then complete.
  A changed or replaced original is kept and reported (`recycle_source_cleanup_pending`).
- Permanent `admitted`: a still-present source aborts; an absent source rolls forward.

Deletion history (`file_deletion_history`, reason `manual`) is written in the same transaction as
the association changes and appears in `GET /api/v1/history` as `origin: native_deletion`.

## CleanUpRecycleBin

`POST /api/v1/recycle-bin/cleanup-commands` with `{"target":{"media_type":...},"priority":...}`
enqueues a manual cleanup; the worker also enqueues an `automatic` one per domain when
`next_cleanup_at` is due, the path is set and `cleanup_days > 0`, then advances it by one day.
`GET` lists (filter `media_type`, `limit`, `offset`), `GET /{id}` reads and `POST /{id}/cancel`
cancels queued or retry-waiting work. One active cleanup per domain; the table shares the 1024 active command pool
and is pruned by housekeeping.

Cleanup opens the bin without following symlinks and walks descriptor-relative. It removes regular
files whose mtime is at least `cleanup_days` old, then empty subfolders. It never follows or removes
symlinks, special files, entries on another mount or in-progress `.partial` files, and never touches
anything outside the bin. Everything already in the bin counts, not only files hrrdarr moved there.
Each run is bounded (depth 32, 100 000 entries, 10 000 removals, 60 s); a bounded run reports
`bounded` and leaves the rest for the next run. Outcomes: `cleaned`, `not_configured`, `disabled`;
a missing bin fails with `recycle_bin_unavailable`. Cleanup waits (retry) while a file operation
holds the effect permit.

## Health

Health check `recycle_bin` (compatibility `RecyclingBinCheck`, scope `tv` and `movies`) is healthy
when no bin is configured. A configured bin that is missing reports `recycle_bin_missing`; one that
is not a writable directory or traverses a symlink reports `recycle_bin_unwritable` (error
severity). Saving settings marks the check for re-evaluation.

## Not implemented

Deleting a series/movie folder with its files, emptying the bin on demand, recycling replaced files
during upgrades, extras/metadata sidecar retirement, importing source `RecycleBin` settings from
snapshots, and bind-mount alias detection. Files outside their library folder are refused rather
than deleted. Deletion history stores the historical file ID; SQLite may reuse that ID for a later
file in the same domain.
