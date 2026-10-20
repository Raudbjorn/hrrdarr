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
jpg jpeg png tbn, not tracked as media) move with the media file. Any failure rolls the whole
group back. After a crash the journal row's true state is read back from the disk: all pairs at
source or already moved roll forward; any anomaly rolls moved pairs back and records the item as
failed. This runs at worker start and at the start of each run. Cross-device moves are refused
(`EXDEV` -> `cross_device`), never copied. Case-only renames work on case-sensitive filesystems
as ordinary renames and on case-insensitive ones via a plain rename when no distinct entry has
the target name. Cancel stops before the next item; finished renames stay. A run handles at most
1000 items (`bounded`); rerunning continues, since renamed files then plan as unchanged.

## OnRename seam

Each committed rename inserts an immutable `rename_events` row (`event_type='rename'`, `kind`
file/folder, domain-typed owner, old/new path, sidecar count, `dispatched_at` NULL) in the same
transaction, then calls observers registered with
`rename::events::register_observer` (trait `RenameObserver`). A consumer reads
`events::pending` and calls `events::mark_dispatched`. No notification delivery exists here.

## Not claimed

* Cross-device refusal is mapped from `EXDEV` and unit-checked by code mapping only; no real
  second filesystem was exercised. Case-insensitive filesystems were not exercised (the
  classifier logic is unit-tested on a case-sensitive one).
* Journalled moves are atomic renames, not the importer's stage/copy/hash pipeline; file
  bytes are not hashed. Identity is device+inode.
* Empty source folders are not removed; other tables holding paths under a renamed folder (for
  example scan inventories, historical import destinations) are not rewritten.
* Rename events are not in `/api/v1/history`; the notifications lane consumes the outbox.
* One rename failed once in the browser script on first run and passed on three reruns
  (cause not established; observed-once).
* Source-faithful Sonarr/Radarr runtime equivalence, V3 wire shapes, scene/absolute edge cases
  beyond the renderer's tested grammar, and extra-file tracking tables are not claimed.
* The command pool admits `rename_commands`; the schema migration is file 0067 but sits at
  position 63 on this branch, so merging needs renumbering with the other lanes.
