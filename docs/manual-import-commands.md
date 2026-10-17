# Durable manual import commands

`src/commands/manual_import.rs` lets an operator submit a batch of already-previewed manual
imports (`POST /api/v1/imports`, see [manual import recovery](manual-import.md)) for durable,
worker-driven execution, instead of calling `POST /api/v1/imports/{id}/execute` once per operation
synchronously over HTTP. It never accepts or re-specifies a mode: the `Plan`'s mode was fixed at
preview time and this command only authorizes running that existing plan to completion.
See [path diagnostics](path-diagnostics.md) for retained evidence, Health findings, and recovery guidance.

## Routes

- `POST /api/v1/manual-import/commands/reconcile` — read-only body `{"operation_ids":[...]}`, 1–100 distinct operation UUIDs. Returns 200 with exactly one result per requested UUID, in request order: `{"operation_id":"...","operation":Operation|null,"active_command":ManualImportCommand|null}`. Unknown fields and query parameters are rejected. This uses the same 8 KiB request limit, five-second deadline and 1 MiB response bound as the command router.
- `POST /api/v1/manual-import/commands` — body `{"operation_ids":[...],"priority":"normal"|"high"}`, up to 100 unique operation UUIDs. Validates the whole batch in one transaction before inserting anything: every ID must exist, have an `import_journal` row still at phase `preview` with no `error_code` or retained `path_failure_code`, not already have a non-terminal `manual_import_commands` row, and not be claimed by the automated-download path (`rss_candidate_imports`). Returns 202 and the created rows, all sharing one fresh `batch_id` (`rows[0].batch_id`); poll that with the list endpoint.
- `GET /api/v1/manual-import/commands` — list, filterable by `batch_id` and `status`, paginated (`limit` 1-100, `offset` up to 1024).
- `GET /api/v1/manual-import/commands/{id}` — single row detail.
- `POST /api/v1/manual-import/commands/{id}/cancel` — only while `queued`/`retry_wait` **and** the linked `import_journal` is still at `preview`, without retained path evidence or a matching active transfer. Once execution has begun, through this command or a direct `execute` call racing ahead of it, cancellation returns 409: a copy/move/hardlink in progress or completed cannot be undone by cancelling the wrapper.

An operation admitted here can still be executed directly through `POST /api/v1/imports/{id}/execute`; the two paths are not mutually exclusive at the database level (only the automated-download path has that trigger-enforced exclusivity). `cancel` re-checks the journal phase, not just the command's own status column, specifically to catch that race.

## Reconciliation after an uncertain response

Reconciliation reads operation status and the current queued, retry-wait or running command in one
read-only database snapshot. It uses the canonical operation projection, including any recovery
fields exposed by the operation API. Missing operations return both values as null. An existing
operation with no active command returns its status and `active_command:null`; a terminal command
is not substituted for an active one. Known command IDs remain readable through the detail route
while retained. Projection or storage errors fail the entire response rather than omitting entries.

The active-command lookup uses the partial unique index on operation ID. It does not scan global
history, enumerate terminal commands, or depend on the list endpoint's offset limit. Terminal
history may have been pruned, and multiple historical batches for one operation are legal.

This snapshot does **not** settle an earlier HTTP request, reserve admission, recover the exact
original batch identity, or grant recovery authority. An original request may still be pending
after a read reports no active command. An explicit new admission must use the same captured
operation UUIDs and remains subject to transactional validation; a conflict requires fresh reads,
not automatic repeated POSTs or replacement previews. Cancelling one observed command does not
revoke every pending admission request. A complete operation can coexist with an unsettled queued
wrapper after direct execution; reconciliation reports both facts without settling that wrapper.

[Reconciliation integration coverage](../tests/manual_import_reconcile.rs) exercises real TV/movie
previews with colliding numeric target IDs, missing operations, strict request bounds, active and
terminal command states, complete operations with queued wrappers, more than 1024 unrelated terminal
rows, terminal-history pruning, and the active lookup's query plan. It checks database data-version
stability around reconciliation requests, unchanged source bytes/identity, and absence of unrequested
imports. This source coverage does not establish a passing run, request-settlement semantics,
distributed ownership, or filesystem effects beyond the explicitly executed test operation.

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
reconciles this using the durable journal and the matching in-process execution lease:

- **Every call:** complete journals settle succeeded. A matching active operation lease keeps a
  noncomplete command running, including on an in-process worker restart. An abandoned retained
  path marker settles failed with explicit recovery required, even if error-status persistence failed.
  A recorded generic error also settles failed once no matching transfer owns the lease.
- **Only at worker start** (`code="interrupted"`): legacy nonterminal journals without retained path
  evidence or an active matching lease keep the existing attempt-counted retry_wait/failed sweep.

Every effectful invocation first records a durable `unresolved_attempt` sentinel (or retains earlier
causal evidence) and invalidates scoped Health in one transaction. Preview admission also advances
into staging in that transaction, preserving rescan exclusion. Failed admission performs no transfer
or compensating restoration. Association commit retains evidence; only successful full cleanup and
completion clear it atomically with Health invalidation. A passing Health probe cannot clear it.

Operation responses expose nullable `path_failure_code` and readonly `recovery_required`, independently
of generic errors. Codes distinguish measured source/root/destination/stage/retirement failures and
precommit/cleanup boundaries. Ambiguous cross-role syscall failures retain uncertainty. Committed
Move source absence is not a removed-mid-processing diagnosis. Public error codes/messages stay
stable and never expose private filesystem paths. The recovery boolean is a UI projection, not write
authority; initial preview execution remains separate. Explicit automated resume reserves the existing
import permit through its authorization transaction so it cannot bank permission during a live transfer.

`run()` itself also never calls `execute()` over a journal that already has a persisted
`error_code` or retained path marker: `run_inner` resumes a `staging` transfer before any `checkpoint` call would clear that
error, so re-executing here would blindly retry the operation under this command's own 3-attempt
budget instead of the operation's documented explicit-retry contract (repeat
`POST .../imports/{id}/execute`). Admission enforces the same invariant up front
(`error_code` and `path_failure_code` must both be NULL to submit a command); `run()`'s own pre-check exists only to
close the narrow window where a direct `execute` call fails the operation between admission and
claim.

## Error codes

`error_code` reuses whatever `import_journal.error_code` or `crate::import::Error::code()` already
produced (all already lowercase-snake-case, matching the column's structural `CHECK`) — for
example `target_changed`, `destination_claimed`, `source_is_library`. Only confirmed pre-admission
`import_busy`/`rescan_active` contention is automatically retried within the command's 3-attempt
budget. An admitted invocation whose diagnostic write failed is still a failure requiring explicit
recovery; a NULL generic error does not prove it was untouched. Any code that would violate the column's `[a-z0-9_]{1,64}` check
is normalized to `import_failed` rather than risk a storage error stranding the row `running`.

## Worker eligibility

The worker claims manual-import commands only when no other manual-import command is `running`.
This prevents a detached transfer from consuming its siblings' retry budgets while it retains the
single import permit. A queued or active rescan for the same target also defers the claim.
Automated download imports share the permit but are not covered by the manual-command gate;
contention with them uses the ordinary bounded retry path.

Admission uses the shared `command_capacity()` check, which includes manual imports and the other
command tables and counts nonterminal work against the 1024-row pool. Retained terminal history
does not consume that capacity.

## Verification

The following coverage is defined in the current test sources; test presence does not substitute
for a passing run on the revision being deployed.

- [Manual-import HTTP integration tests](../tests/manual_import_commands.rs) run a real worker
  through the HTTP API. They cover successful TV/movie imports with equal numeric target IDs,
  mixed success/failure batches, batch listing, rejection of automated-download-owned operations,
  and a direct-execute/worker race that must produce exactly one import-history row. These exercise
  the worker's manual-import claim and dispatch path, including `run()` calling `execute()`.
- The same integration suite closes and reopens the database between workers, checks that completed
  imports retain their completion timestamps and destination modification times, and lets a queued
  sibling complete after restart. It also verifies that a running wrapper settles after its real
  journal reaches `complete`; this scenario constructs the running wrapper and does not wait for
  the worker's actual 45-second timeout.
- [Manual-import module tests](../src/commands/manual_import.rs) cover admission, cancellation,
  pre-existing errors, and recovery. The active-transfer regression holds a real SQLite writer
  barrier, observes the real import lease, verifies that interrupted-worker recovery leaves the
  command running, then releases the barrier and verifies successful completion settlement. A
  separate admitted sentinel without an active transfer requires explicit recovery.
- [Worker tests](../src/commands/worker.rs) verify that the manual-import claim is deferred by a
  target rescan and becomes eligible once the rescan completes.

## Not claimed

- These tests do not demonstrate a real transfer running past the 45-second worker timeout,
  power-loss durability, distributed import ownership, or cancellation of a blocked filesystem
  syscall. The execution permit is process-local.
- Manual commands can exhaust their three-attempt contention budget while an unrelated automated
  download import holds the shared permit. The manual-command eligibility gate does not eliminate
  that contention.
- Legacy unmarked operations can exhaust their interrupted-startup retry budget while remaining
  resumable through direct execution. A failed wrapper does not mean its underlying operation is
  permanently unrecoverable.
- A worker step timeout is logged as `command_worker_error code=storage_error`, even when its
  detached transfer later completes successfully; that log alone does not establish transfer failure.
- The recovery tests use local databases, temporary files, and local HTTP fixtures. They do not
  establish production filesystem behavior, remote-client interoperability, or browser interaction.
