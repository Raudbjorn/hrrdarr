# Activity and download-refresh UI

Activity consumes the existing native command, schedule and queue contracts using
Rust-generated DTOs. Refresh commands read client state. Completed-download handling defaults on for each domain and authorizes eligible confirmed owned receipts; client removal is not offered.

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

## Completed-download policies and recovery

Selecting a client and media domain also opens its completed-download policy and paged processing records. The policy inherits its domain setting until explicitly overridden, uses captured revisions for changes, and offers copy or hardlink; both retain torrent source bytes. Eligible imports require a domain/host remote path mapping. Refreshing downloads may admit completed RSS- or Search-owned receipts when the effective policy is enabled. Merely appearing in a client category does not confer an import target.

Each processing record shows its typed target, preflight attempts, operation UUID, phase and errors. A blocked or cancelled preflight supports explicit retry. A linked failed import supports Resume, which continues the same operation rather than submitting or importing again. Pending resume authorization is visible and suppresses another resume button. Cancellation is offered only before an operation is linked. Disabling a policy stops new admissions; linked recovery remains authorized.

Replacement feedback distinguishes pending retirement, a previous file retained in a private recovery artifact, and an original still shared by other TV episodes. Recovery bytes are not automatically deleted. Policy edits preserve their original revision while dirty. Conflicts prompt explicit readback; unknown write responses lock further actions until Check processing status succeeds. Polling never replays a mutation. Changing client/domain asks before discarding dirty drafts and ignores late responses. Activity and Providers retain drafts across workspace navigation. Provider revision changes invalidate captured policy/schedule revisions without replacing dirty values.

## Domain intent and observation

The domain control is usable before any provider exists. It distinguishes default from explicitly defined values and records native intent even when saving an unchanged value. Per-client overrides can be reset to inheritance while retaining the transfer mode. Domain desired imports, effective scope count, and the backend reconciliation reason are displayed separately; counts never infer the reason. No health API or health result is fabricated.

Schedules distinguish inherited and explicit requested values from effective observation. Explicitly disabled observation differs from a deleted/suppressed schedule. Restoring the automatic schedule uses captured CAS and the backend 60-second interval. Master-off does not disable explicitly requested observation or remove existing linked import recovery.

Pending writes and unknown outcomes lock related provider/settings writes through complete readback. Lost provider update/delete outcomes remain attached to their original provider ID. Unknown creation requires explicit adoption of a matching public configuration after full related readback; matching does not prove which request created it. No blind create retry or automatic duplicate deletion occurs.

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
all RSS/download-client behaviors, blocklist/history breadth or full Slice 2 parity.
The UI uses generated backend contracts and adds no frontend dependencies.

The earlier completed-import browser scenario used real RSS commands and the owned mock's actual qBittorrent add/details responses, then domain path mappings and the real import journal. That historical run covered initial TV hardlink import, a movie upgrade from the earlier manual fixture using copy, missing-mapping retry, policy revision conflict, lost accepted policy/resume responses, and retained-original feedback. A scratch-only history INSERT failure proves the movie original survives a failed commit before resuming the same operation. No producer receipt or processing state is fabricated with SQL. This browser run does not kill/restart a process or establish the complete Slice 2/4 gate; backend recovery tests supply separate evidence.

Iteration 52 browser verification passed on 2026-09-29 against the owned loopback fixture: `completed-download-handling-browser.mjs` covered both-domain default/defined intent, overrides and resets, observation suppression, CAS, stale drafts, navigation and unknown-write readback. The full `library-browser.mjs` run passed default inherited RSS and interactive/automatic Search imports in both domains, with source-preserving copy, missing-mapping retry, failed movie replacement preservation and same-operation Resume while the movie master was off. The owned completion endpoint changes only an already-submitted mock torrent; actual periodic refresh/import workers create application state. Both runs joined their fixture and frontend processes. Final Svelte checks reported zero errors/warnings, all 64 frontend tests passed, and the production build passed. These results do not establish live-service behavior, process restart recovery, or completion of the integrated required Rust suite; no parity row is promoted on this evidence alone.
