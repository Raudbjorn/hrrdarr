# File and folder renaming (cmd.011, cmd.012, api.044)

Renaming plans from stored file facts and the current naming configuration, previews the
result read-only, and executes through a durable command. TV and movies share the machinery
but have separate typed targets: `series_ids` / `movie_ids` are never interchangeable.

## Planning

* TV (`src/naming/plan.rs`): each physical episode file is named from the complete set of linked
  episodes (`render_episode_set`, configured multi-episode style, standard/daily/anime pattern by
  series type). The file is placed in the series folder, in the season folder when the series'
  `season_folder` setting is on and a season/specials format exists. An unknown `season_folder`
  (never assigned) or a missing format keeps the file in its current directory. Files without a
  linked episode are reported `unavailable`.
* Movies: `src/naming/movie_preview.rs::compute` (the same code behind the movie preview) names
  the file; the destination is the movie folder.
* Folders: `series_folder_format` / `movie_folder_format` rendered to one path component in the
  same parent directory.
* Rename disabled: names are cleaned from the original release title or current name, as the
  movie preview already did.
* Collisions: duplicate destinations among selected files and a destination that is another
  tracked file are `blocked` in the plan. TV and folder previews additionally probe the disk
  read-only (missing source, existing destination, symlinked or out-of-root components). The
  movie file preview keeps its verified no-occupancy contract; execution re-checks everything.

## HTTP

* `GET /api/v1/tv/rename-preview?series_id=N[&season=S]` or `?series_ids=1,2,...` (<= 50)
* `GET /api/v1/{tv|movies}/rename-folder-preview?series_ids=` / `?movie_ids=`
* `GET /api/v1/movies/rename-preview?movie_ids=` (existing)
* `POST /api/v1/rename/commands`, `GET /api/v1/rename/commands[/{id}]`, `POST .../{id}/cancel`.
  Names: `rename_files` (one owner, 1..200 file IDs), `rename_series`, `rename_movie`,
  `rename_series_folder`, `rename_movie_folder`. `expected_naming_revision` fences against
  configuration changes after the preview (409 at admission, `failed/naming_revision_changed`
  at run). One active command per owner (409 `command_conflict`).
* `GET /api/v1/rename/events` lists the durable rename events.

## Execution and recovery

The command runs on the single command worker under the process-wide import/deletion permit
(`import_busy` retries). Per file: re-plan from current facts, refuse on destination/source
problems, journal intent (`rename_items`, state `planned`, with device/inode of the media file
and each sidecar), move with `renameat2(RENAME_NOREPLACE)` through descriptor-relative,
no-symlink directory walks (`src/rename/fsops.rs`, built on the recycle-bin/importer
primitives), then commit the new path, journal state, event row and counters in one
transaction. Sidecars (same stem followed by `.`, extensions srt ass ssa sub idx sup vtt nfo
jpg jpeg png tbn, not tracked as media) move with the media file. A failure attempts to roll the
whole group back. If rollback cannot restore the layout, the item stays `planned` with
`rollback_incomplete`, remains protected from history pruning, and stops that command attempt.
After a crash the journal row's true state is read back from disk: all pairs at source or already
moved roll forward; anomalies attempt rollback and become `rolled_back` only once restoration
succeeds. Recovery runs at worker start and before each command, including terminal predecessors
for the same TV/movie owners. Unresolved items are logged and retained for another recovery pass. Cross-device moves are refused
(`EXDEV` -> `cross_device`), never copied. Case-only renames work on case-sensitive filesystems
as ordinary renames and on case-insensitive ones via a plain rename when no distinct entry has
the target name. When both path lookups return the recorded identity, journal recovery uses the
directory entry's exact spelling to distinguish the source from the destination; two distinct
entries sharing an inode remain a conflict. Cancel stops before the next item; finished renames
stay. A run handles at most 1000 items (`bounded`); rerunning continues, since renamed files then
plan as unchanged.

## Historical failed journals (`failed` / `rollback_incomplete`)

Older binaries could settle an unrestorable group as `failed` with `rollback_incomplete` and
count it in `items_failed` while the media sat at its new (or partly moved) path and the
database still named the old path. Current code keeps such an item `planned`, but the historical
rows are not selected by automatic recovery. They stay retained by housekeeping and are repaired
only by an explicit request. The class is exactly `state='failed' AND error_code='rollback_incomplete'`;
any other failed item (`destination_exists`, `target_changed`, ...) is unrelated evidence and
is refused as `not_legacy`.

* `GET /api/v1/rename/legacy-recovery[?media_type=tv|movies&limit=1..25&offset=]` is a read-only
  inventory (one assessment per row, nothing is written).
* `POST /api/v1/rename/legacy-recovery` with `{"dry_run": bool, "item_ids": [1..25 distinct]}`
  assesses the selected rows and, only when `dry_run` is `false` (required, no default), applies the
  admitted ones. An unknown item ID is 404 `rename_item_not_found` before anything is written.

Each assessed item reports its typed owner (`media_type` + `owner_id`; a series and a movie with
the same number stay distinct), the original and new path, the media identity (device/inode),
every sidecar with its identity, the observed `layout` (`at_source`, `moved`, `partial`,
`conflict`, `unreadable`), the owning command's `items_failed`, the `items_failed_delta` the
transition would make, and either an `action` or a `refusal`:

| Layout | Action / outcome |
| --- | --- |
| `moved` or `partial` | `roll_forward`: reopen as `planned` and decrement the command's `items_failed` once. The worker's existing journal recovery then moves any remaining pair and commits the database path, the rename event and `items_renamed` (or, if that forward move fails and rollback restores the layout, settles `rolled_back` and counts the failure again). |
| `at_source` | `settle_restored`: the original layout is verified intact, so the row becomes `rolled_back` / `legacy_rollback_verified` without any media change; `items_failed` already counted it. |
| `conflict`, `unreadable` | Refused (`layout_conflict`, `layout_unreadable` with the filesystem code in `layout_error`): an unrelated file at the old name, changed identity, a symlink or an unreadable path. Nothing is written; remove the obstacle and ask again. |

Other refusals: `journal_invalid` (identity or sidecar JSON does not parse), `owner_missing`,
`database_path_changed` (the file/series/movie row no longer names the old path), `owner_busy`
(a queued, running or retrying rename command targets the owner), `unsettled_journal` (another
planned journal overlaps the same file or folder), `counter_inconsistent` (`items_failed` is
already 0, so it cannot be decremented without going negative).

The transition is one Immediate transaction that is repeated against the database facts inside
that transaction. It touches no media; filesystem effects stay with the single command worker
under the import/deletion permit, which finishes reopened journals at its next start or before
the next rename command for the same owner. Because the row can leave the legacy class only once,
rerunning or retrying after an interruption cannot decrement a counter twice, drop a journal or
duplicate an event. A reopened `planned` journal is protected from history pruning like any
current recovery handle; once it settles as `committed` or `rolled_back` it no longer pins its
command and is pruned normally, while unresolved legacy rows still pin theirs.

## OnRename seam

Each committed rename inserts an immutable `rename_events` row (`event_type='rename'`, `kind`
file/folder, domain-typed owner, old/new path, sidecar count, `dispatched_at` NULL) in the same
transaction, then calls observers registered with
`rename::events::register_observer` (trait `RenameObserver`). A consumer reads
`events::pending` and calls `events::mark_dispatched`. No notification delivery exists here.

## Not claimed

* Historical `failed`/`rollback_incomplete` journals are repaired only by the explicit request
  above ([issue #63](https://github.com/Raudbjorn/hrrdarr/issues/63)); nothing reopens them
  automatically. Verified with owned fixture databases and files built to the old on-disk shape;
  a binary from before the fix was not run to produce them, and evidence that older binaries
  already deleted (pruned failed rows) cannot be reconstructed. The command history status of
  an affected command (`failed`/`items_failed`) is not rewritten when its item is recovered; only the
  counters and the journal row change. No production library or case-insensitive filesystem was
  exercised.
* Cross-device refusal is mapped from `EXDEV` and unit-checked by code mapping only; no real
  second filesystem was exercised. Case-insensitive filesystems were not exercised (the
  alias observations are simulated against real directory listings; file/folder moves and reverse
  moves are tested on a case-sensitive filesystem).
* Journalled moves are atomic renames, not the importer's stage/copy/hash pipeline; file
  bytes are not hashed. Identity is device+inode.
* Empty source folders are not removed; other tables holding paths under a renamed folder (for
  example scan inventories, historical import destinations) are not rewritten.
* Rename events are not in `/api/v1/history`; the notifications lane consumes the outbox.
* One rename failed once in the browser script on first run and passed on three reruns
  (cause not established; observed-once).
* Source-faithful Sonarr/Radarr runtime equivalence, V3 wire shapes, scene/absolute edge cases
  beyond the renderer's tested grammar, and extra-file tracking tables are not claimed.
* The command pool admits `rename_commands`; its schema is preserved from main as migration
  `0065_rename_commands.sql` at ordinal 65.
