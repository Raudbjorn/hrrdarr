# Downloaded-folder scan commands

`src/commands/downloaded_scan.rs` implements the durable producer behind the Sonarr
`DownloadedEpisodesScan` / Radarr `DownloadedMoviesScan` command rows. One `POST` names exactly one
absolute file or folder. **Phase A (discovery)**: the worker captures a bounded inventory of that
path, evaluates every entry against the library, observes the accepted candidates, and publishes
the **complete** inventory in a single transaction without any filesystem effect. **Phase B
(import)**: the worker then imports the selected children one at a time through the existing
import journal engine (`src/commands/downloaded_scan/continuation.rs` drives
`src/import/scan_owned.rs`); see [Import phase](#import-phase-phase-b). Storage is migrations
0054, 0056 and 0057; no migration was added for either phase.

This is a native contract, not Sonarr/Radarr V3 wire or SignalR compatibility. Progress is bounded
polling of the routes below.

## Routes

Both domains have the same shape; the domain comes from the route and is never inferred from a
numeric owner ID. A command UUID asked for under the other domain is `404`.

| Route | Purpose |
| --- | --- |
| `POST /api/v1/{tv\|movies}/downloaded-scan-commands` | Admit. `202` new, `200` exact replay, `409` identity mismatch. |
| `GET  /api/v1/{tv\|movies}/downloaded-scan-commands` | Newest-first page. Filter `status`; `limit` 1-100 (default 25), `offset` 0-100000. |
| `GET  /api/v1/{tv\|movies}/downloaded-scan-commands/{id}` | One parent with aggregate counts (never embedded children). |
| `GET  /api/v1/{tv\|movies}/downloaded-scan-commands/{id}/children` | Ordinal-ordered child page. Filter `disposition`; same paging. |
| `POST /api/v1/{tv\|movies}/downloaded-scan-commands/{id}/cancel` | Idempotent, monotone stop-after-current cancel. |
| `POST /api/v1/{tv\|movies}/downloaded-scan-commands/{id}/children/{child_id}/resume` | Explicit recovery of one failed child (see [Resume](#explicit-child-resume)). |

Request bodies are limited to 8 KiB and every request has a five-second deadline. Unknown fields,
unknown query parameters, non-canonical (uppercase) path UUIDs and out-of-range paging are `400`
(`invalid_command_request`). Responses over 1 MiB fail with `413 command_response_limit` rather than
returning a truncated page. The generated TypeScript contract is
`DownloadedScanInput`, `DownloadedScanCommand`, `ScanChild`, `ScanChildResumeInput` and their enums
in `frontend/src/lib/api.generated.ts`; `api.ts` has thin, non-retrying wrappers (including
`resumeDownloadedScanChild`).

### Admission

```json
{"id":"<client uuid>","path":"/downloads/Harbor.S01.1080p.WEB-DL","mode":"auto","priority":"normal",
 "tracked":{"provider_id":"<uuid>","remote_id":"<40 or 64 lowercase hex>"}}
```

- `id` is the immutable client-generated identity. It is checked first, inside the `IMMEDIATE`
  transaction: the same canonical request returns the existing command with `200` and consumes no
  capacity; any differing domain, path, mode, priority or tracked identity is `409
  downloaded_scan_conflict`. After a lost response, `GET` the same UUID; never mint a new one.
  `404` is only current absence.
- `path` must be absolute, normalized UTF-8 of at most 4096 bytes: no NUL, no `.`/`..`/empty
  components, no trailing slash, not `/`. Newlines and tabs inside a name are legal. A valid path
  that does not exist is **not** a request error (see [Outcomes](#outcomes)).
- `mode` is `auto|move|copy`. `auto` resolves before any effect: an untracked scan moves.
  A tracked scan whose move capability is unknown never moves implicitly (phase B fact).
- `priority` is `normal|high`.
- `tracked` is optional. The provider must exist and be scoped to the route's domain (`409
  downloaded_scan_provider_unknown`). `supersedes_command_id` is not accepted in phase A and is
  rejected like any unknown field.
- Capacity is the shared 1024 non-terminal pool across all ten command tables (`429
  command_history_full`). The shared pre-check (`command_capacity`) previously omitted the scan
  table, so every other producer would have hit the admission trigger instead of a clean `429`;
  it now counts `downloaded_scan_commands`.

## Parent lifecycle

Public `status` is `queued|running|retry_wait|succeeded|failed|cancelled`. `phase` is derived and
never grants execution permission:

| `phase` | Meaning |
| --- | --- |
| `queued` | `queued` or `retry_wait`; due at `next_attempt_at`. |
| `inventory` | `running`, `inventory_revision` 0: discovery in flight (or abandoned and awaiting recovery). |
| `awaiting_import` | `running`, `inventory_revision` 1, selected children are `ready` (or the parent is settling). |
| `importing` | `running`, one child is `importing`. |
| `recovery_required` | `running`, nothing is ready or importing, and a `failed` child holds an unresolved (non-complete) operation. Only an explicit child resume continues it. |
| `settled` | Terminal. |

Counters follow the 0054 triggers. `queued|retry_wait -> running` increments `attempts` and
`run_generation` together and advances `ready_order_at`; `attempts` is therefore at most 3 and counts
claims, never children. `run_generation` fences every later write: publication and settlement
compare the claim's generation, so a superseded worker cannot publish or settle.

**Published parents.** After a successful publication the schema forbids a terminal parent while
any child is `ready`, so when at least one child was selected the parent stays `running` with
`inventory_revision` 1. Discovery recovery never resets it (recovery only touches `running`
parents with `inventory_revision` 0). The worker claims it as an import continuation (below).
While it is running the parent counts against the 1024 pool and its selected sources,
destinations, inodes, episodes and movie reserve against other scans, manual imports and rescans
(`downloaded_scan_reserved_children`). `counts.recovery_required` counts failed children whose
operation is not complete.

If nothing was selected, the parent settles `succeeded` with `outcome: "unsuccessful"` in the same
transaction as publication (`success` requires an imported child).

### Outcomes

| Situation | Result |
| --- | --- |
| Valid path that does not exist | `succeeded`, `outcome: unsuccessful`, `source: missing`, zero children, `attempts` 1, no retry. |
| Existing empty folder, or nothing accepted | `succeeded`, `outcome: unsuccessful`, `source: found`. |
| At least one accepted file | `running`, `awaiting_import`. |
| Cap, permission or identity failure | No publication (`inventory_revision` 0, zero children). Retryable causes use the three-attempt budget with bounded exponential backoff and jitter; the rest fail at once. |
| Cancelled before claim or during discovery | `cancelled`, no children; a cancel recorded after discovery but before publication fences it. |

Error codes (`error_code`, static and path-free): `scan_inventory_limit` (entries, depth, bytes or
encoding), `scan_source_denied`, `scan_source_invalid`, `scan_source_changed` (retry),
`scan_source_io` (retry), `scan_discovery_timeout` (retry), `scan_storage_error` (retry),
`scan_target_busy` (retry; another scan reservation, an active rescan, an in-flight transfer or an unrelated in-flight download receipt owns a selected episode or movie), `scan_facts_changed`
(retry), `scan_facts_invalid`, `scan_decision_limit`, `scan_identity_unsupported`,
`scan_history_limit`, `scan_file_limit`, `scan_multiple_owners`, and the tracked codes below.

## Discovery

Everything below runs on the worker, never in an HTTP request, with a 30-second discovery budget
(40 seconds hard) inside the worker's 45-second step timeout.

1. `Inventory::capture` on a blocking thread: anchored, no symlink following, depth 32, 20,000
   visited entries (unsupported entries included), 64 MiB serialized. Any limit, permission or
   identity failure aborts; there is no truncation.
2. For every file: the immediate owner is resolved (`resolve_scan_owner`, cached per normalized
   title/year), then `warm_scan_file` and `evaluate_scan_file` decide quality, members and revision.
   Deterministic evaluator outcomes are visible child rejections, not command failures. Storage,
   timeout and cap codes are infrastructure failures. For a folder source the folder name is passed
   as release identity for every nested file; a file source has none.
3. Eligible files are observed on a blocking thread (`scan::observe`: media probe, sibling
   multipart, working folder, free space). Observation rejections become child reasons.
4. Ranking is deterministic: hardlink aliases of one inode keep only the best candidate
   (`duplicate_source_identity`); `select_scan_files` then picks a non-overlapping set
   (`superseded_by_better_file` for the rest). A folder whose selected files name more than one
   library owner fails with `scan_multiple_owners` instead of importing partially.
5. The anchored inventory is re-verified (`verify_before_publish`) before the transaction.
6. One `IMMEDIATE` transaction: re-checks the parent fence (`running`, same `run_generation`,
   `inventory_revision` 0, `cancel_requested` 0), re-evaluates every selected file against its
   stored proof inside the write snapshot, resolves the destination (naming settings; a render
   failure such as `multi_episode_style_required` becomes a visible rejection), inserts every
   child, TV episode membership and media observation, and moves `inventory_revision` 0 to 1 with
   exact `inventory_count`/`inventory_bytes`. Any error rolls back all of it.

After revision 1, input, inventory, selection, membership and destination are immutable (enforced by
the 0054 triggers). Files that appear later are never discovered by this command, and a restart
never re-enumerates. A separate request is required for new files.

### Children

Every inventory entry is a child so the anchored inventory can be restored later: files, plus
directories, symlinks (not followed) and special files as `rejected` children (`kind` tells them
apart). `counts.rejected` therefore includes non-file entries; filter by `kind` client-side or
count `ScanChild` pages. Dispositions at publication: `ready` (selected), `rejected` and
`already_imported` (the exact source path is in completed import history); the import phase moves
selected children through `importing` to `imported`, `failed` or `cancelled`. Each child also
projects its linked `operation_id`, `operation_phase`, `path_failure_code`, `resume_generation`,
`resume_requested` and a derived `recovery_required` (failed, unresolved journal, not active).
`reasons` are static lowercase codes. Selected children carry owner (`series_id` xor `movie_id`),
`anchor_episode_id` plus `member_count`, frozen old movie file, destination, resolved copy/move,
quality, revision and edition. `observation` is `measured|unavailable`; a measured observation has
its retained media info in `downloaded_scan_media_observations`.

## Import phase (phase B)

### Continuation claims and fairness

`worker.rs::claim` admits `status='running' AND inventory_revision=1` parents into the same scan
arm when `next_attempt_at<=now`. A continuation claim increments `run_generation` and advances
`ready_order_at` to the claim time but never touches `attempts` (the transition trigger allows
attempts to change only on `queued|retry_wait -> running`). Same-priority work queued before the
continuation therefore runs first. Each worker turn performs exactly one bounded step, strictly
sequentially (children never run in parallel):

1. **An `importing` child.** While its operation holds the process execution permit
   (`operation_active`) the parent is polled again 5 seconds later. A `complete` journal settles
   the child `imported` exactly once (the trigger requires the import history row). A clean,
   never-armed preview (the start transaction committed but the engine never armed, e.g. a dropped
   worker turn) is started again under the same generation. Anything else (a measured
   `error_code`, or the `unresolved_attempt`/path marker of an abandoned attempt) makes the child
   `failed` with that code (or `import_interrupted`); it is **never retried automatically**.
2. **A `failed` child with `resume_requested`.** Started under that exact resume generation.
3. **`cancel_requested`.** See [Cancellation](#cancellation-stop-after-current).
4. **The next `ready` child (lowest ordinal).** `prepare_scan` then `start_scan` (below). A
   deterministic prepare refusal marks the child `failed` with a static code
   (`hc021_*_prepare` for measured path causes, otherwise the importer's code, e.g.
   `scan_decision_changed`, `scan_naming_changed`, `destination_claimed`, `source_changed`); no
   operation exists, so nothing is uncertain. Storage failures leave it `ready` for the next poll.
5. **Nothing ready or importing.** The parent settles `succeeded` with `outcome: success` iff at
   least one of its children is `imported`, otherwise `unsuccessful`. If a `failed` child holds a
   non-complete journal the 0054 terminal trigger forbids settlement: the parent stays `running`
   in phase `recovery_required` and is re-polled only every 300 seconds (a resume or cancel wakes
   it immediately).

### Importer seam

`src/import/scan_owned.rs` exposes `prepare_scan(db, ScanChildAdmission{command_id, child_id,
inventory_revision, run_generation})` and `start_scan(db, ScanChildStart{claim,
resume_generation})`. The queue passes identities and fences only.

- **prepare** loads the published child, its proof and its episode membership, warms the stored
  decision without a write transaction, captures every distinct original (`OldFile`, recovery name
  `.hrrdarr-replaced-{operation}-{old_file_id}`) and the `Plan` on a blocking thread, and requires
  the source to be the same inode with the same size/mtime as the inventory identity. One
  `IMMEDIATE` transaction then re-checks the parent claim (running, revision, generation, not
  cancelled), re-evaluates the decision against the stored proof, re-resolves the destination
  (naming drift is `scan_naming_changed`), re-checks ownership, every member's frozen association
  and every original's path/metadata, and writes the operation, the preview journal,
  `scan_import_facts`, one `scan_import_replacements` row per distinct original and the
  write-once child link. An already linked child returns the same operation.
- **start** reads the child's linked operation (never an arbitrary ID), takes the existing process
  execution permit, re-checks the claim and the canonical journal in one transaction, consumes the
  start authority (`ready -> importing`, or `failed -> importing` consuming `resume_requested`
  under the exact `resume_generation`) and launches the existing engine with the same permit.
  `Busy`/`Active` are polled; `Complete` is settled by the next turn.
- **Engine.** The physical transfer is the existing journal engine: preview -> staging (arming
  records the unresolved marker in the same transaction) -> staged -> published (or same-path
  exchange) -> one commit transaction -> per-original retirement -> complete. For scan operations
  the commit creates **one** physical episode file for a multi-episode file and compare-and-swaps
  every member episode to it, writes one `import_history` row (anchored on the lowest episode ID)
  plus one `import_history_episodes` row per member, or updates/creates the movie file. Each
  distinct original is independently checkpointed: still referenced by an episode outside the set
  -> `shared_retained`; otherwise moved into its private recovery directory (bytes retained) ->
  `quarantined`. A failed replacement before commit restores a same-path exchange and leaves every
  original's bytes, path and association intact (covered by the forced-failure test).
- **Producer exclusion.** `POST /api/v1/imports/{id}/execute` refuses any scan-owned operation with
  `409 scan_recovery_required` (including completed ones); manual-command admission refuses it with
  `409 manual_import_owned_by_scan`. `GET /api/v1/imports/{id}` stays readable.

### Modes and source cleanup

`copy` leaves the source file in place; `move` verifies the destination, moves the source into the
engine's private `.hrrdarr-move-{operation}` directory beside it and then unlinks it. **Directory
cleanup is not implemented**: `cleanup_state` stays `not_needed` for every parent (the 0054 trigger
permits that even after imported moves), and the source folder, its rejected/unimported entries
and the empty private `.hrrdarr-move-*` directories are left in place. No directory is ever
removed, recursively or otherwise. The `resume-cleanup` route therefore does not exist.

### Cancellation (stop-after-current)

`cancel` sets the monotone `cancel_requested` fence and wakes the parent. In the same transaction
every untouched child (`ready` without an operation, or with a never-armed clean preview) becomes
`cancelled`, and a `failed` child whose journal never left `preview` (arming moves preview to
staging together with the unresolved marker, so no effect happened) has that journal error cleared
and becomes `cancelled`; its child `error_code` is kept. If nothing is importing or unresolved the
parent settles `cancelled` at once (the phase A parked-cancel path). Otherwise only the fence is
recorded: no further child is prepared or started (prepare/start refuse with `scan_cancelled`,
and the 0054 trigger forbids arming any scan journal after cancel), the in-flight child finishes
and settles, and the next turn settles the parent `cancelled`. An unresolved effectful child keeps
a cancelled parent `running` until it is explicitly resumed to completion.

### Explicit child resume

`POST .../children/{child_id}/resume` with
`{"operation_id": "<uuid>", "inventory_revision": 1, "expected_resume_generation": <n>}` (unknown
fields `400`). It takes the non-blocking process resume guard (`409 import_busy` while any import
executes) and holds it through an `IMMEDIATE` transaction that re-reads the parent (domain scoped,
`404` otherwise), the child and its journal:

- `200` and `resume_generation = n + 1`, `resume_requested = true` when the child is `failed`, its
  linked journal is not complete and carries an error or unresolved marker, the parent is running
  at that revision and `n` equals the current generation. The parent is woken immediately.
- `200` without any change when `resume_requested` is already true at generation `n + 1` (exact
  replay after a lost response; never a second authorization). A replay mutates nothing, so it is
  answered even while an import holds the permit. Once the worker consumes the request the same
  body is `409`; read the child to learn the outcome.
- `409 downloaded_scan_resume_conflict` for a different operation, revision or a non-running
  parent; `409 downloaded_scan_resume_generation` for any other generation;
  `409 downloaded_scan_resume_not_required` when nothing is unresolved; `409
  downloaded_scan_cancelled` for a never-armed preview after cancel (cancellation only permits
  finishing an already effectful child).

The worker consumes the request once (step 2). If that attempt fails again the child returns to
`failed`; another recovery needs another generation.

### Restart and recovery

A worker restart does not stop a detached import: the new worker sees the operation active and
waits, or sees a complete journal and settles the child once (never re-imports). After a process
restart the in-memory permit is gone, so an `importing` child whose journal still carries the
unresolved marker becomes `failed` (`import_interrupted`, `recovery_required`) and the parent waits
for an explicit resume; success is never fabricated.

## Tracked requests

`tracked` resolves strictly by provider + domain + remote id at discovery. An unknown remote id
falls back to ordinary identification and is reported as `tracking: unknown_fallback` once
published (`pending` before). A receipt that exists for that provider/domain/remote is **not yet
supported**: reserving the receipt, expected coverage and the processing handback are not
implemented, and degrading a known receipt into an untracked scan would be unsafe, so the
parent fails with `scan_tracked_receipt_unsupported` (receipt `observed`) or
`scan_tracked_receipt_unavailable` (any other status). A provider removed after admission fails
with `scan_tracked_provider_unavailable`.

## Worker integration

`worker.rs::claim` adds one arm to the shared union: `(status IN ('queued','retry_wait') OR
(status='running' AND inventory_revision=1)) AND next_attempt_at<=now`, ordered by the scan's own
`priority, ready_order_at, id`. Only this arm
substitutes `ready_order_at` for `created_at`, so an interrupted scan that re-enters the queue sorts
behind same-priority work created before its re-entry (covered by
`scan_continuations_order_by_ready_time_so_same_priority_work_interleaves`). `recover` resets only
unpublished `running` parents: to `cancelled` if a cancel was recorded, otherwise `retry_wait`
(`attempts` < 3) or `failed`. A dropped step future leaves a read-only blocking thread that can no
longer publish: publication and settlement are fenced by generation.

## Not established

- Source-folder cleanup (`observed -> remove_intent -> settled`), the resume-cleanup route and
  tracked coverage handback are not implemented; `cleanup_state` and `coverage_state` keep their
  defaults. Tracked receipts are still rejected at discovery (above).
- The members and supersession routes of the queue contract are absent.
- Policy is re-evaluated at prepare only; between prepare and commit the importer re-checks
  structural facts (owner, root, member associations, originals) while the 0054 triggers reserve
  the owner's path, settings and members.
- `GET /api/v1/history` filters by the anchor episode only; non-anchor members of a multi-episode
  import are recorded in `import_history_episodes` but not yet surfaced by that reader.
- Health invalidation for a measured prepare refusal (child `error_code`, no journal) is not wired;
  journal-backed failures use the existing engine hooks.
- Discovery is read-only but non-atomic with the filesystem: a file added between
  `verify_before_publish` and commit is not in the inventory; prepare revalidates each source's
  inode, size and mtime before creating its operation.
- No UI panel consumes these routes yet; only the typed `api.ts` wrappers exist.
- Per-request owner resolution scans the title catalog once per distinct normalized title; very
  large libraries combined with very many distinct files can exhaust the 30-second budget, which
  fails visibly (`scan_discovery_timeout`) rather than truncating.
- Covered by [unit tests](../src/commands/downloaded_scan/tests.rs) and
  [HTTP + worker tests](../tests/downloaded_scan_commands.rs) against temporary directories; the
  media probe results depend on the installed `ffprobe` (an unavailable probe is recorded, not
  fatal, for sample-exempt formats).
