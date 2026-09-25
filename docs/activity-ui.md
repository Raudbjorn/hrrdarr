# Activity and download-refresh UI

Activity consumes the existing native command, schedule and queue contracts using
Rust-generated DTOs. It does not submit or remove downloads, match them to library
targets or import media. Only the implemented `refresh_downloads` command is offered.

The command history is paged and filtered by media type and status. Details show
the immutable provider/domain target, provider revision, priority, attempts, state,
recorded timestamps and static error code. Successful item counts are not displayed
as percentage progress. Cancel command cancels refresh work, not a client download.
Only terminal history offers deletion, with explicit confirmation that client
items and media remain. No terminal work is automatically resubmitted by the UI.

A paged provider picker exposes qBittorrent clients while retaining disabled clients
for inspection. Refresh downloads requires an enabled provider and the chosen TV or
movie scope. Queue reads show the latest successful snapshot's observation time,
provider revision and optional originating command ID. A missing snapshot (404) is
unknown state, separate from a successful empty page. Old successes may survive a
failed refresh and are always dated. Client-reported download progress is distinct
from command progress; null associations are visibly unknown/unassociated.

Schedules list all bounded persisted rows, including disabled and `provider_changed`
errors, revision, next due and last accepted run. Selecting a schedule loads its
provider directly even when outside the current provider page. The editor captures
its revision only on selection or explicit reload; polling never silently rebases
an in-progress edit. New schedules send the required explicit `revision: null`;
updates/deletions send the captured revision. Conflicts require reloading schedule
settings. Enabled schedules become due immediately. Last accepted run is enqueue
or coalescing time, not successful completion. Deleting a schedule is explicitly
confirmed and does not cancel existing commands or delete snapshots.

Visible Activity uses one sequential read cycle at a time, with a five-second pause
between cycles. Polling stops when the document is hidden or the component unmounts;
responses cannot repopulate disposed or changed selections. Actions are disabled
while a cycle or mutation runs. All requests reuse the existing bounded transport.
An unknown mutation outcome locks further mutations, clears history filters for
readback, and requires an explicit successful Reload activity before another action.
No request is automatically retried. API commands may independently use their
existing durable read-only retry policy.

## Verification limits

Frontend checks/build and transport tests verify native request shapes, explicit
nullable schedule revision, query filters, cancellation and body-bearing schedule
DELETE. The standalone `frontend/tests/library-browser.mjs`, using
`tests/library_ui_fixture.rs` and the setup in [slice-1-gate.md](slice-1-gate.md),
passed through actual controls and native endpoints with owned providers on
2026-09-25. It proves both scoped refreshes with the same remote hash, unknown
associations, retained success after three failed attempts, visible retry/errors,
cancellation and terminal-history deletion, a lost accepted response without
automatic replay, schedule creation/reload/disable/deletion, command filtering
and a 390px layout without page overflow or uncaught browser errors. A separate
desktop inspection displayed the failed command and its recorded attempts/error.
The fixture has a live worker and bounded test-only delay/failure controls; no
command status or snapshot is inserted directly by these browser tests. This UI does not establish live client
behavior, process restart recovery, all Activity/settings routes, all command types,
RSS, tracked-download matching, blocklist/history breadth or full Slice 2 parity.
No dependency, database migration or backend contract is introduced here.
