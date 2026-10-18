# Command-history housekeeping

`src/commands/housekeeping.rs` prunes terminal rows from the eight shared-capacity command
tables: `commands`, `metadata_refresh_commands`, `blocklist_clear_commands`, `rss_commands`,
`search_commands`, `manual_import_commands`, `quality_reset_commands` and `rescan_commands`.
Since migration 0033 only active rows count toward the 1024-row pool, so history never
exhausts capacity; this job bounds disk use and query cost instead. `health_commands` already
retains 128 terminal rows through its own triggers and is not touched.

## Policy (named constants)

| Constant | Value | Meaning |
| --- | --- | --- |
| `RETENTION_SECONDS` | 14 days | Only rows with `completed_at` strictly older than this are eligible. Must exceed the 1800 s search-offer lifetime (compile-time assertion). |
| `MIN_KEEP_PER_DOMAIN` | 100 | Newest terminal rows (by `created_at,id`, the list order) kept per table and media domain, whatever their age. |
| `BATCH_ROWS` | 100 | Rows per transaction. |
| `MAX_BATCHES_PER_GROUP` | 20 | Batches per table-domain per run. |
| `RUN_BUDGET` | 5 s | Wall-clock budget, checked between batches only. |
| `RUN_INTERVAL_SECONDS` | 6 h | Interval once drained. |
| `BACKLOG_INTERVAL_SECONDS` | 60 s | Follow-up delay when a run stopped on a bound or a group failed. |
| `STARTUP_DELAY_SECONDS` | 10 s | First run after worker start. |

## What is deleted, and what never is

Only each table's actual terminal statuses: `succeeded`, `failed`, `cancelled`, plus `skipped`
for rescans. `queued`, `running` and `retry_wait` rows are never selected, and the status
guard is repeated in the `DELETE` inside the same `BEGIN IMMEDIATE` transaction. Worker claims
use the same kind of transaction, so a batch and a claim serialise and a claimed row is seen as
running by the batch.

The keep floor and the media domain are applied per `(table, media_type)`. `manual_import_commands`
has no domain column, so it is derived from the operation (`episode` is tv, `movie` is movies).
Series/episode/movie numeric ids are never compared, so equal ids in both domains cannot interfere.

Rows protected from deletion (verified against migrations and every reader of these tables):

| Table | Protected while |
| --- | --- |
| `commands` | It is the `command_id` of a `download_refresh_snapshots` row (queue provenance). |
| `rss_commands` | A referencing `rss_candidates` row is `pending`, `prepared`, `submitting`, `reconciling` or `needs_attention`. Settled candidates keep their own dedup/receipt state and tolerate `ON DELETE SET NULL`, the same path as the existing delete endpoint. |
| `search_commands` | Any of its offers is selected, or unexpired. Expired unselected offers are deleted with the command (the `search_results` FK is `RESTRICT`). |
| `manual_import_commands` | The operation's `import_journal` phase is neither `preview` nor `complete`. |
| Others | Nothing references them and their dedup indexes are partial on active statuses, so terminal rows are pure history. |

Terminal rows are not used for replay deduplication anywhere: dedup is the active-scope unique
indexes plus candidate/hash-claim rows, which this job never deletes.

## Scheduling and recovery

No migration and no new table. The job is a worker-owned recurring maintenance step, not a
persisted command row: `Schedule` lives in the worker task, which calls `housekeeping::tick`
once per second. It runs once `STARTUP_DELAY_SECONDS` after start, then every `RUN_INTERVAL_SECONDS`
(or after `BACKLOG_INTERVAL_SECONDS` while work remains). It runs on the existing single worker
loop, so there is no second scheduler and no concurrent run. Errors are logged with fixed codes
and never propagate into the worker's recovery path.

The run is stateless and idempotent. A restart schedules one early run; a batch interrupted by
shutdown or a crash rolls back as a unit; a failing table is reported and skipped without
blocking the others, and the next run retries it. Logs carry only fixed fields
(`event=housekeeping_run|housekeeping_pruned|housekeeping_error`, table name, media type, counts).

Residual: housekeeping has no history of its own, no API and no manual trigger. A durable,
API-visible `HousekeepingCommand` with a persisted next run would need a new command table, its
capacity-pool admit triggers and `COMMAND_CAPACITY_SQL` changes (a schema-migration task).

## Pagination decision

The list endpoints keep rejecting `offset > MAX_COMMANDS` (1024); the contract is unchanged.
A hard row ceiling would delete rows inside the retention window, which this job must not do.
Accepted residual: if a table accumulates more than 1024 terminal rows inside the 14-day window
(or because of the keep floor), the older ones are unpageable until they age out and are pruned.
