# Durable manual import commands

`src/commands/manual_import.rs` lets an operator submit a batch of already-previewed manual
imports (`POST /api/v1/imports`, see [manual import recovery](manual-import.md)) for durable,
worker-driven execution, instead of calling `POST /api/v1/imports/{id}/execute` once per operation
synchronously over HTTP. It never accepts or re-specifies a mode: the `Plan`'s mode was fixed at
preview time and this command only authorizes running that existing plan to completion.

## Routes

- `POST /api/v1/manual-import/commands` — body `{"operation_ids":[...],"priority":"normal"|"high"}`, up to 100 unique operation UUIDs. Validates the whole batch in one transaction before inserting anything: every ID must exist, have an `import_journal` row still at phase `preview` with no `error_code`, not already have a non-terminal `manual_import_commands` row, and not be claimed by the automated-download path (`rss_candidate_imports`). Returns 202 and the created rows, all sharing one fresh `batch_id` (`rows[0].batch_id`); poll that with the list endpoint.
- `GET /api/v1/manual-import/commands` — list, filterable by `batch_id` and `status`, paginated (`limit` 1-100, `offset` up to 1024).
- `GET /api/v1/manual-import/commands/{id}` — single row detail.
- `POST /api/v1/manual-import/commands/{id}/cancel` — only while `queued`/`retry_wait` **and** the linked `import_journal` is still at `preview`. Once execution has begun, through this command or a direct `execute` call racing ahead of it, cancellation returns 409: a copy/move/hardlink in progress or completed cannot be undone by cancelling the wrapper.

An operation admitted here can still be executed directly through `POST /api/v1/imports/{id}/execute`; the two paths are not mutually exclusive at the database level (only the automated-download path has that trigger-enforced exclusivity). `cancel` re-checks the journal phase, not just the command's own status column, specifically to catch that race.

## Why `execute()` settles in one tick, and what happens when it doesn't

`crate::import::execute` acquires a single process-wide execution permit and blocks until the whole
phase pipeline reaches `complete` or errors — `run_inner` falls through every `if rec.phase==X`
block in the same call, so one call always drains to a terminal journal state unless the permit
itself could not be acquired (`import_busy`, returned before touching anything). A command row
therefore normally transitions `running` → `succeeded`/`failed` inside the same worker tick it was
claimed on.

The one exception is the worker's 45-second step timeout. If a transfer runs longer, the step's
future — and the `run()` call awaiting `execute()` inside it — is dropped, but `execute()`'s
detached `tokio::spawn` task keeps running independently, since it (not the caller) owns the
permit. The row is left `running` with nothing left to settle it through the normal `claim()` path,
because `claim()` only ever selects `queued`/`retry_wait` rows.

`recover()` (called at the top of every worker tick, and once more when the worker starts —
`start_with_metadata`, once per `commands::start`/`start_with_metadata` call) is what eventually
reconciles this, and only ever from durable facts:

- **Every call:** a `running` row whose `import_journal` has already reached `phase='complete'` or
  recorded an `error_code` is settled accordingly. This can never fire on a row that is still
  genuinely mid-transfer, since only `checkpoint()`/the error path write those facts, and both are
  terminal by construction — this is the same reasoning as `processing::recover` never touching an
  `importing` row's fast in-flight state.
- **Only at worker start** (`code="interrupted"`): a `running` row whose journal is *not* terminal is
  treated as orphaned by a prior crash and recovered by the same attempt-counted `retry_wait`/`failed`
  sweep every other command table uses at startup. This is sound for the one worker `main.rs` starts
  per OS process, where a fresh process is the only way `start_with_metadata` runs again — but the
  detached `launch()` task (and the execution permit it holds) is not owned by the worker's own task
  and does not stop when `Runtime::shutdown` aborts it, so an **in-process** restart of just the
  worker (calling `start`/`start_with_metadata` again without the process exiting) can still find a
  transfer genuinely in flight and reset it anyway. See "Not claimed" below.

`run()` itself also never calls `execute()` over a journal that already has a persisted
`error_code`: `run_inner` resumes a `staging` transfer before any `checkpoint` call would clear that
error, so re-executing here would blindly retry the operation under this command's own 3-attempt
budget instead of the operation's documented explicit-retry contract (repeat
`POST .../imports/{id}/execute`). Admission enforces the same invariant up front
(`error_code IS NULL` required to submit a command at all); `run()`'s own pre-check exists only to
close the narrow window where a direct `execute` call fails the operation between admission and
claim.

## Error codes

`error_code` reuses whatever `import_journal.error_code` or `crate::import::Error::code()` already
produced (all already lowercase-snake-case, matching the column's structural `CHECK`) — for
example `target_changed`, `destination_claimed`, `source_is_library`. Any `execute()` outcome that
left the journal untouched (not just `import_busy` — any error surfaced before `launch()`'s task
ever starts, plus the defensive `import_incomplete` fallback for an `execute()` call that returned
`Ok` without a terminal journal, believed unreachable given `run_inner`'s phase-chaining per the
reasoning above) is retried within the row's own 3-attempt budget before failing, since the
operation itself was never touched. Any code that would violate the column's `[a-z0-9_]{1,64}` check
is normalized to `import_failed` rather than risk a storage error stranding the row `running`.

## Deviation from the original brief

The brief's exact `claim()` UNION branch (`status IN ('queued','retry_wait') AND
next_attempt_at<=?`) is extended with `AND NOT EXISTS(SELECT 1 FROM manual_import_commands r WHERE
r.status='running')`. Without it, once one command's transfer runs past the step timeout (still
holding the global execution permit), the next tick would claim a sibling command in the same
batch, which would immediately get `import_busy`, burn an attempt, and — after three ticks — fail
outright while the first transfer might still be legitimately succeeding in the background. The
gate serializes this table's own claims to match the reality of `crate::import::execute`'s single
global permit; it does not affect any other command table's claim eligibility. It does not gate
against `download_processing`'s `importing` rows (also a permit holder, via `start_owned`) — that
collision is left to this table's existing `import_busy` retry/backoff, reported as a residual
below.

## Not claimed

- **`run()`'s own call into `crate::import::execute()` has no test coverage at all** — neither the
  success settlement (`phase='complete'` → `succeeded`), nor failure from a journal error `execute()`
  itself just wrote, nor the `import_busy`/`import_incomplete` retry branches. A test exercising any
  of these was written and removed: `crate::import::execute()` holds the single process-wide
  execution permit described above, and `src/import/tests.rs`/`src/import/owned_tests.rs`'s own
  tests never guard against a concurrent caller either. Adding this module's tests alongside them
  made an unrelated existing test (`import::tests::restart_recovers_publication_commit_and_cleanup_without_losing_sources`)
  fail on a spurious `import_busy` in 5 of 5 runs under this machine's default `cargo test`
  parallelism — not a rare flake, a reliable collision. Closing this needs a lock shared by every
  real-`execute()`-calling test, which belongs in `src/import/` (outside this module's file
  ownership), not a module-local one. Real coverage of these branches is left to the
  testing-verification pass, and should live in `tests/*.rs` (a separate process per binary) with
  its own internal serialization, in the style of `tests/download_processing.rs`, not in this
  module's `#[cfg(test)]` block.
- **`recover()`'s `phase='complete'` branch has no direct test either**, for the same reason plus one
  more: `import_journal`'s own `import_commit_requires_history` trigger makes that phase impossible
  to fabricate without a real committed `import_history` row, so even a journal-only test can't
  reach it. The closest evidence this module has: `manual_import::recover` runs, without error, on
  every worker tick of the existing real-worker integration tests it now participates in
  (`commands::blocklist::tests`, `tests/commands_api.rs`, `tests/download_processing.rs`,
  `tests/rss_commands.rs`, `tests/search_commands.rs`) — none of which currently create a
  `manual_import_commands` row, so this only shows the added `recover()` call and the extended
  `claim()` UNION branch (including its extra bound parameter) don't break anything for those
  tables, not that the `complete` branch itself is exercised.
- **No test ever claims a row through `worker.rs`'s new `kind=7` branch** — the extended `claim()`
  UNION arm, the self-exclusion gate, or the `Claimed::ManualImport` dispatch in `step()` — for the
  same reason: doing so exercises `run()` → `execute()`.
- `import_busy` collisions against `download_processing`'s owned/RSS-triggered imports (also
  competing for `crate::import`'s single global execution permit via `start_owned`) are not
  specifically gated against; they fall back to this table's ordinary 3-attempt retry/backoff, which
  can still fail a manual-import command solely due to unrelated download-processing activity
  holding the permit.
- If the worker restarts **in-process** (calling `start`/`start_with_metadata` again without the OS
  process exiting) while a transfer is still genuinely running in its detached `launch()` task, the
  `"interrupted"` sweep does not know that and resets the row to `retry_wait` anyway (see the
  worker-start note above) — it can then exhaust its 3 attempts on repeated `import_busy` and end
  `failed` while the underlying operation completes successfully in the background, unobserved by
  the command row.
- A row can also be stranded `running` with a non-terminal, error-free journal and **no live task at
  all** — for example if `launch()`'s own error-status write fails, or the spawned task panics
  outright. Because the self-exclusion gate added to `claim()`'s UNION branch (see "Deviation" below)
  blocks claiming *any* manual-import row while one is `running`, this strands the entire
  manual-import queue, not just the affected row, until the next worker start.
- The 3-attempt cap can mark a command `failed('interrupted')` at worker start while its underlying
  operation is still resumable through a direct `execute` call; the command row does not reflect that
  residual resumability.
- Every manual import longer than 45 seconds logs a spurious
  `event=command_worker_error code=storage_error` from the worker's own step timeout, even when the
  transfer eventually succeeds.
- `src/commands/processing.rs`'s and `src/commands/mod.rs`'s own pre-insert capacity checks still sum
  only five command tables, not the six `migration 0030`'s triggers now enforce; unaffected callers
  can still hit a trigger-level `command capacity reached` abort instead of a clean 429. Not fixed
  here (outside this module's file ownership); flagged for whoever owns those checks.
