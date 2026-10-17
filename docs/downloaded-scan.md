# Downloaded-folder scan commands

`src/commands/downloaded_scan.rs` implements the durable producer behind the Sonarr
`DownloadedEpisodesScan` / Radarr `DownloadedMoviesScan` command rows. One `POST` names exactly one
absolute file or folder. **Phase A (discovery)**: the worker captures a bounded inventory of that
path, evaluates every entry against the library, observes the accepted candidates, and publishes
the **complete** inventory in a single transaction without any filesystem effect. **Phase B
(import)**: the worker then imports the selected children one at a time through the existing
import journal engine (`src/commands/downloaded_scan/continuation.rs` drives
`src/import/scan_owned.rs`); see [Import phase](#import-phase-phase-b). **Phase C** reserves a
tracked download receipt and hands it back with typed coverage
([Tracked requests](#tracked-requests)), and removes source directories emptied by move imports
([Source cleanup](#source-cleanup-phase-c)). Storage is migrations 0054, 0056 and 0057; no
migration was added for any phase.

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
| `GET  /api/v1/{tv\|movies}/downloaded-scan-commands/{id}/children/{child_id}/members` | Complete episode membership of one child, ordered by episode ID; same paging. Movie children have none. |
| `POST /api/v1/{tv\|movies}/downloaded-scan-commands/{id}/resume-cleanup` | Explicit recovery of an uncertain source cleanup (see [Source cleanup](#source-cleanup-phase-c)). |

Request bodies are limited to 8 KiB and every request has a five-second deadline. Unknown fields,
unknown query parameters, non-canonical (uppercase) path UUIDs and out-of-range paging are `400`
(`invalid_command_request`). Responses over 1 MiB fail with `413 command_response_limit` rather than
returning a truncated page. The generated TypeScript contract is
`DownloadedScanInput`, `DownloadedScanCommand` (with `ScanCoverage` and `ScanCleanup`), `ScanChild`,
`ScanChildMember`, `ScanChildResumeInput`, `ScanCleanupResumeInput` and their enums in
`frontend/src/lib/api.generated.ts`; `api.ts` has thin, non-retrying wrappers (including
`resumeDownloadedScanChild`, `listDownloadedScanMembers` and `resumeDownloadedScanCleanup`).

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
- `mode` is `auto|move|copy`. `auto` resolves before any effect: an untracked scan moves; a
  tracked scan copies, because the client's move capability is unknown and unknown capability
  never implies a move. Explicit `move`/`copy` stay explicit.
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
| `waiting_predecessor` | `running`, `inventory_revision` 0, a tracked receipt is captured and its linked singleton import has not completed. |
| `awaiting_import` | `running`, `inventory_revision` 1, selected children are `ready` (or the parent is settling). |
| `importing` | `running`, one child is `importing`. |
| `recovery_required` | `running`, nothing is ready or importing, and a `failed` child holds an unresolved (non-complete) operation, or the source cleanup recorded its removal intent and then failed uncertainly (`cleanup.recovery_required`). Only an explicit child or cleanup resume continues it. |
| `cleanup` | `running`, source cleanup is checkpointed (`observed`, or `remove_intent` being executed or explicitly resumed). |
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
`scan_history_limit`, `scan_file_limit`, `scan_multiple_owners`, and the tracked codes
`scan_tracked_provider_unavailable`, `scan_tracked_receipt_domain`,
`scan_tracked_receipt_unavailable`, `scan_tracked_receipt_reserved` and
`scan_tracked_receipt_busy` (retry) described below.

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

### Modes

`copy` leaves the source file in place; `move` verifies the destination, moves the source into the
engine's private `.hrrdarr-move-{operation}` directory beside it and then unlinks it. The empty
private directory and any directories the moves emptied are handled by
[source cleanup](#source-cleanup-phase-c).

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
a cancelled parent `running` until it is explicitly resumed to completion. A source cleanup that
has only been `observed` settles `retained` (`cancelled`); one that recorded its removal intent
must settle (or be explicitly resumed) first. A captured receipt is handed back with its actual
coverage: complete only if every expected member was already credited.

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

## Source cleanup (phase C)

`src/commands/downloaded_scan/cleanup.rs` (orchestration) and `src/import/scan_cleanup.rs`
(anchored filesystem walk). Cleanup applies only to a **folder** source with at least one child
imported by `move`; copy mode, a file source (its private move directory sits beside the file,
outside the scanned source) and a parent without an imported move keep `cleanup.state:
not_needed`. It runs after every child has settled (no ready/importing child and no unresolved
failed child), one checkpoint per worker turn, before the parent settles:

1. **observed** (`not_needed -> pending`): on a blocking thread the scanned folder must still be the
   inventory root (same device/inode, a directory, no symlink); its parent's identity is recorded.
   No effect. A root that is gone, replaced, denied or not a directory settles at once as
   `retained` (`identity_changed`, `path_denied`, `path_invalid`, `cleanup_io`) with nothing
   removed and the parent identity recorded as the unmeasured sentinel 0/0.
2. **remove_intent**: under the single global effect permit (the import permit, owner
   `scan-cleanup-{id}`; `import_busy` polls again in 5 s) the intent is committed together with a
   Health generation bump, then the walk runs (20 s budget, depth 32, 20,000 visited entries):
   parent and root identities are re-checked; only directories whose device/inode is a published
   inventory directory, or an imported child's `.hrrdarr-move-{operation}` directory matched by
   name **and** its journal-recorded identity, are entered; each is removed with `rmdir`
   semantics (`AT_REMOVEDIR`) only after its own contents were pruned, so the kernel refuses any
   non-empty directory. No file is ever removed, nothing unknown is entered, nothing above the
   scanned folder is touched. The scanned folder itself is removed last if it became empty.
3. **settled**: `complete` (the folder is gone) or `retained` with one static reason, in priority
   order `path_denied`, `unimported_media` (an inventory media file that this scan did not import,
   including samples and rejected media), `archive_leftovers` (`rar`, `rNN`, `zip`, `7z`, ...),
   `unknown_leftovers` (anything else, including entries that appeared later); or `cancelled`
   for an `observed` cleanup of a cancelled parent. `cleanup.removed` counts removed
   directories. Retained leftovers are a settled outcome, not a failed import.

An I/O failure after the intent whose effect is unknown, or a lost settlement (process stop,
storage failure), leaves `pending`/`remove_intent` with `cleanup.error_code` and
`cleanup.recovery_required: true`; the parent stays `running` (phase `recovery_required`, polled
every 300 s), counts against the 1024 pool, and cannot settle or be cancelled terminally. Only

```json
POST .../{id}/resume-cleanup
{"inventory_revision": 1, "expected_cleanup_resume_generation": <cleanup.resume_generation>}
```

authorizes one more attempt: under the non-blocking global permit (`409 import_busy`) and an
`IMMEDIATE` transaction it increments the monotone `cleanup_resume_generation` and sets
`cleanup_resume_requested`. An exact replay of the authorized, unconsumed generation returns `200`
without change; any other generation is `409 downloaded_scan_cleanup_resume_generation`; a
different revision or a non-running parent is `409 downloaded_scan_cleanup_resume_conflict`;
nothing to recover is `409 downloaded_scan_cleanup_resume_not_required`; unknown fields `400`;
the other domain `404`. The worker consumes the request once, before cancellation handling, and
re-runs the same anchored walk. An absent scanned folder under the **same** parent with no
replacement is then reconciled as `complete` (the earlier attempt removed it); on a first attempt
an absent folder is `identity_changed`. A second uncertain failure needs another generation.

Every cleanup transition that is effectful or settles (intent, settlement, uncertain failure)
invalidates the domain's `remote_path_mapping` Health generation in the same transaction. Health
has no dedicated reader for cleanup evidence yet (see [Not established](#not-established)).

## Tracked requests

`src/commands/downloaded_scan/receipt.rs`. `tracked` resolves strictly by provider + remote id
(unique per client across domains) at discovery, read-only:

| Situation | Result |
| --- | --- |
| Provider no longer scoped to the route's domain | `failed`, `scan_tracked_provider_unavailable`. |
| Unknown remote id | Ordinary identification, `tracking: unknown_fallback` once published. |
| Receipt of the other domain (same client and hash) | `failed`, `scan_tracked_receipt_domain`; it never authorizes this parent. |
| Receipt not `observed`, or its client disabled/reconfigured | `failed`, `scan_tracked_receipt_unavailable`. |
| Another scan holds or settled this receipt | `failed`, `scan_tracked_receipt_reserved` (a retry needs explicit successor lineage, not implemented). |
| Download processing is `checking` (its preflight owner) | `retry_wait`, `scan_tracked_receipt_busy`; never stolen. |
| Linked singleton import (`rss_candidate_imports`) not complete | Captured now; phase `waiting_predecessor`. |
| Otherwise | Captured in the publication transaction. |

**Capture** writes `candidate_id`, `tracked_json` `{version, provider_id, provider_revision,
remote_id}`, the exact processing fence (all eleven lifetime fields, or `{version:1,absent:true}`
when no processing row exists; none is ever created), the linked predecessor operation, and
`coverage_state: pending`; the 0054 triggers re-verify each value. While captured, the singleton
path cannot touch the receipt: the worker's processing arm, `processing::claim`, `recover`,
`observe` (no new processing row) and `POST /api/v1/download-processing` / `.../cancel` skip or
refuse it (`409 processing_reserved_by_scan`), mirroring the 0054 legacy guard. A linked
predecessor is the exception: it keeps its own authority (resume, completion) and the scan only
reads it. `DownloadProcessing.scan_command_id` names the scan that reserved or settled a receipt
(or the latest scan that read its predecessor).

**Waiting.** A parent waiting for its predecessor stays `running` at revision 0 and is re-claimed
as a new scheduling generation without consuming attempts (5 s while the predecessor executes,
30 s while it is idle, for example awaiting its own explicit resume); startup/per-tick recovery
never resets it. Cancellation settles it `cancelled` with `coverage_state: incomplete`. Once the
predecessor's journal is complete with history, discovery runs and publishes.

**Discovery and decisions** use the receipt authority (`ScanAuthority::Receipt`): the receipt fixes
the owner and the expected members; every filename, folder and receipt title identity must agree,
and files for other episodes are visible rejections (`release_target_mismatch`,
`folder_target_mismatch`). `prepare_scan` re-evaluates with the same captured authority.

**Coverage.** Publication seals the receipt's expected coverage (its episode for TV, its movie)
in `downloaded_scan_coverage`. Credits are write-once proofs against the **current** association:
the completed predecessor's history (credited at publication) and this parent's completed
children (credited with the child's settlement). Final settlement re-credits, then records
`coverage_state` `complete` (every expected member credited) or `incomplete`, then hands the
receipt back in the **same** transaction:

- processing absent: stays absent (the complete-coverage view releases the RSS target);
- linked predecessor: processing stays with that owner;
- otherwise `complete` -> `imported`, `error_code` null, reasons cleared via
  `path_reasons(..., Complete)`; `incomplete` (including cancelled/zero-import) -> `blocked`,
  `import_failed`, reasons `path_reasons(previous, ["downloaded_scan_incomplete"], Preserve)`, so
  a leading `hc021_*` marker is retained. Policy revision, both preflight counters,
  `next_attempt_at` and `created_at` are unchanged; `scan_command_id` is set; the Health
  generation is invalidated in the same transaction.

The receipt stays scan-owned after the handback (the 0054 guard), so a singleton retry cannot
import missing coverage behind the scan's proof. A parent that fails before publication never
captured (capture is in the publication transaction) unless it was waiting for a predecessor,
whose owner keeps the receipt.

## Worker integration

`worker.rs::claim` adds one arm to the shared union: `(status IN ('queued','retry_wait') OR
(status='running' AND (inventory_revision=1 OR candidate_id IS NOT NULL))) AND
next_attempt_at<=now`, ordered by the scan's own `priority, ready_order_at, id`. The processing
arm (kind 5) excludes scan-reserved queued receipts. Only this arm
substitutes `ready_order_at` for `created_at`, so an interrupted scan that re-enters the queue sorts
behind same-priority work created before its re-entry (covered by
`scan_continuations_order_by_ready_time_so_same_priority_work_interleaves`). `recover` resets only
unpublished, uncaptured `running` parents: to `cancelled` if a cancel was recorded, otherwise `retry_wait`
(`attempts` < 3) or `failed`. A dropped step future leaves a read-only blocking thread that can no
longer publish: publication and settlement are fenced by generation.

## Not established

- `supersedes_command_id` (an explicit coverage-aware successor for an incomplete tracked scan)
  is not implemented; an incomplete receipt stays blocked and scan-owned with no in-product
  retry. The UI form does not offer tracked requests.
- Download receipts are singletons (migration 0025 admits one episode per candidate), so typed
  coverage is exercised only for single-episode and movie receipts; a predecessor and a scan
  child are alternatives for that one member, not a split pack.
- Health has no reader for cleanup evidence; cleanup transitions only invalidate the existing
  `remote_path_mapping` generation. The processing panel does not yet link `scan_command_id`.
- Cleanup removes directories by name after an identity check on an open descriptor; a
  directory swapped for another **empty** directory between that check and `rmdir` could be
  removed instead (never a non-empty one, never a file).
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
- `frontend/src/lib/DownloadedScanPanel.svelte` (library view, below manual import) is the UI consumer: explicit
  admission with the request identity saved to `localStorage` before the POST and reconciled by `GET` of the same ID,
  5-second visible-only polling with backoff, child pages, cancel and explicit child resume. Covered by
  `frontend/tests/downloaded-scan-state.test.mjs` and `frontend/tests/downloaded-scan-browser.mjs` (real fixture
  backend). The detail shows tracked coverage and source-cleanup state and offers the explicit cleanup resume.
  Tracked requests are not offered in the form, and lost resume responses are retained only in memory.
- Per-request owner resolution scans the title catalog once per distinct normalized title; very
  large libraries combined with very many distinct files can exhaust the 30-second budget, which
  fails visibly (`scan_discovery_timeout`) rather than truncating.
- Covered by [unit tests](../src/commands/downloaded_scan/tests.rs),
  [HTTP + worker tests](../tests/downloaded_scan_commands.rs) and
  [tracked/cleanup tests](../tests/downloaded_scan_tracked.rs) against temporary directories (the
  tracked predecessor's completion is simulated with the rows its commit writes, not by running
  the singleton engine); the
  media probe results depend on the installed `ffprobe` (an unavailable probe is recorded, not
  fatal, for sample-exempt formats).
