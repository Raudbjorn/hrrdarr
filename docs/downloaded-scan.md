# Downloaded-folder scan commands (phase A: discovery)

`src/commands/downloaded_scan.rs` implements the durable producer behind the Sonarr
`DownloadedEpisodesScan` / Radarr `DownloadedMoviesScan` command rows. One `POST` names exactly one
absolute file or folder. The worker captures a bounded inventory of that path, evaluates every
entry against the library, observes the accepted candidates, and publishes the **complete**
inventory in a single transaction. **Phase A performs no transfer**: nothing is moved, copied,
hardlinked, renamed, retired or deleted, and no `import_journal` row is created. The import phase
(arming selected children, history, cleanup) consumes the state described under
[Parent lifecycle](#parent-lifecycle). Storage is migrations 0054, 0056 and 0057; no migration was
added for phase A.

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
| `POST /api/v1/{tv\|movies}/downloaded-scan-commands/{id}/cancel` | Idempotent, monotone cancel. |

Request bodies are limited to 8 KiB and every request has a five-second deadline. Unknown fields,
unknown query parameters, non-canonical (uppercase) path UUIDs and out-of-range paging are `400`
(`invalid_command_request`). Responses over 1 MiB fail with `413 command_response_limit` rather than
returning a truncated page. The generated TypeScript contract is
`DownloadedScanInput`, `DownloadedScanCommand`, `ScanChild` and their enums in
`frontend/src/lib/api.generated.ts`; `api.ts` has thin, non-retrying wrappers.

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
| `awaiting_import` | `running`, `inventory_revision` 1, selected children are `ready`. **Phase A stops here.** |
| `settled` | Terminal. |

Counters follow the 0054 triggers. `queued|retry_wait -> running` increments `attempts` and
`run_generation` together and advances `ready_order_at`; `attempts` is therefore at most 3 and counts
claims, never children. `run_generation` fences every later write: publication and settlement
compare the claim's generation, so a superseded worker cannot publish or settle.

**The parked state.** After a successful publication the schema forbids a terminal parent while any
child is `ready`, so when at least one child was selected the parent stays `running` /
`awaiting_import` with `inventory_revision` 1 and `ready` children. The worker never re-claims it
(claims take only `queued|retry_wait`) and recovery never resets it (recovery only touches
`running` parents with `inventory_revision` 0). The import phase must claim these rows by
`status='running' AND inventory_revision=1`. While parked the parent counts against the 1024 pool
and its selected sources, destinations, inodes, episodes and movie reserve against other scans,
manual imports and rescans (`downloaded_scan_reserved_children`). The only phase A exit is
`cancel`, which marks every `ready` child `cancelled` and settles the parent `cancelled` in one
transaction. If any child already has an operation or is `importing`, cancel records only the
fence (no such child exists in phase A).

If nothing was selected, the parent settles `succeeded` with `outcome: "unsuccessful"` in the same
transaction as publication (`success` requires an imported child, which phase A cannot produce).

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
count `ScanChild` pages. Dispositions in phase A: `ready` (selected), `rejected`,
`already_imported` (the exact source path is in completed import history), and `cancelled`.
`reasons` are static lowercase codes. Selected children carry owner (`series_id` xor `movie_id`),
`anchor_episode_id` plus `member_count`, frozen old movie file, destination, resolved copy/move,
quality, revision and edition. `observation` is `measured|unavailable`; a measured observation has
its retained media info in `downloaded_scan_media_observations`.

## Tracked requests

`tracked` resolves strictly by provider + domain + remote id at discovery. An unknown remote id
falls back to ordinary identification and is reported as `tracking: unknown_fallback` once
published (`pending` before). A receipt that exists for that provider/domain/remote is **not yet
supported** in phase A: reserving the receipt, expected coverage and the processing handback belong
to the import phase, and degrading a known receipt into an untracked scan would be unsafe, so the
parent fails with `scan_tracked_receipt_unsupported` (receipt `observed`) or
`scan_tracked_receipt_unavailable` (any other status). A provider removed after admission fails
with `scan_tracked_provider_unavailable`.

## Worker integration

`worker.rs::claim` adds one arm to the shared union: `status IN ('queued','retry_wait') AND
next_attempt_at<=now`, ordered by the scan's own `priority, ready_order_at, id`. Only this arm
substitutes `ready_order_at` for `created_at`, so an interrupted scan that re-enters the queue sorts
behind same-priority work created before its re-entry (covered by
`scan_continuations_order_by_ready_time_so_same_priority_work_interleaves`). `recover` resets only
unpublished `running` parents: to `cancelled` if a cancel was recorded, otherwise `retry_wait`
(`attempts` < 3) or `failed`. A dropped step future leaves a read-only blocking thread that can no
longer publish: publication and settlement are fenced by generation.

## Not established by phase A

- No transfer, arming, history, replacement retirement, cleanup or coverage handback exists; the
  import phase must consume `awaiting_import` parents.
- The members, resume, resume-cleanup and supersession routes of the queue contract are absent.
- Tracked receipts are rejected (above); only unknown-remote fallback is exercised.
- Discovery is read-only but non-atomic with the filesystem: a file added between
  `verify_before_publish` and commit is not in the inventory; the import phase must revalidate each
  source before arming it.
- Per-request owner resolution scans the title catalog once per distinct normalized title; very
  large libraries combined with very many distinct files can exhaust the 30-second budget, which
  fails visibly (`scan_discovery_timeout`) rather than truncating.
- Covered by [unit tests](../src/commands/downloaded_scan/tests.rs) and
  [HTTP + worker tests](../tests/downloaded_scan_commands.rs) against temporary directories; the
  media probe results depend on the installed `ffprobe` (an unavailable probe is recorded, not
  fatal, for sample-exempt formats).
