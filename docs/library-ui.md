# Manual library and import UI

The Svelte workspace now reads the native TV/movie library APIs, with paged lists,
metadata lookup and selected-result add. Details expose series/movie monitoring,
TV season and episode monitoring, and domain-specific library settings. Unknown
facts remain unknown. Changing series monitoring preserves episode flags; changing
a season updates its episodes. A selected unassociated episode or movie is an
explicitly typed manual-import target.

Import preview shows the target, source, destination and transfer mode before the
user authorizes execution. Library and destination directories must already exist.
This is an initial import, not replacement or automatic discovery. Copy, move and
hardlink retain their backend meanings; see [manual-import.md](manual-import.md).

The browser retains one confirmed receipt in localStorage under
`hrrdarr.import.receipt.v1`, including the operation UUID, typed target, paths,
mode and whether this confirmation authorized execution. Reload checks the same
operation. An execution timeout or lost response does not mean cancellation:
readback reconciles the result, and explicit Resume uses the same UUID. No automatic
mutation retry or second preview is submitted. Unexecuted previews can return to
editing after a fresh status check; authorized unresolved receipts remain available
for recovery. A completed receipt can be cleared to prepare the next import.
Storage failures and corrupt receipts are visible; they do not silently discard
uncertain transfers. Browser-local storage is not a server operation history or a
multi-user synchronization mechanism. It contains local filesystem paths, not
provider credentials. Clearing browser data removes this recovery convenience.

Read requests use bounded timeouts and response sizes. Metadata requests allow
35 seconds; ordinary reads and execution requests allow 10 seconds, followed by
explicit status recovery where appropriate. Polling is sequential, stops when
hidden/unmounted, and stops on completion/errors. Stale detail/search responses
are ignored after selection or domain changes. Labels, native buttons, visible
focus and responsive layouts support keyboard and narrow-screen use.

## Development connection

The Vite server proxies `/api` only when `HRRDARR_API_ORIGIN` explicitly supplies
an HTTP(S) origin without credentials, paths, queries or fragments. It binds to
loopback. No local service port is assumed and no broad CORS configuration is added.
For production, serve the bundle and API under the same origin using the intended
host deployment; production static serving is not implemented by this change.

## Runnable isolated browser verification

The opt-in fixture owns a synthetic metadata service, a real native API server,
a temporary database and tiny media files. No library or episode rows are seeded
by SQL. All listeners use `127.0.0.1:0`.

1. Run `cargo test --locked --test library_ui_fixture -- --ignored --nocapture`.
   Its `UI_FIXTURE` JSON prints the owned API origin, scratch directory and shutdown
   file. The fixture stops after 15 minutes or when that shutdown file is created.
2. Set `HRRDARR_API_ORIGIN` to that printed API origin, then run
   `npm --prefix frontend run dev -- --port 0`. Record the printed frontend origin.
3. Set `UI_URL` to that frontend origin, `UI_SCRATCH` to the fixture scratch path,
   `UI_PROVIDER_ORIGIN` to the printed `provider_origin`, and `UI_METADATA_ORIGIN`
   to the printed `metadata` origin, then run `node frontend/tests/library-browser.mjs` against a fresh fixture.
   Set `PLAYWRIGHT_MODULE` to the installed Playwright `index.mjs` if it is not at
   `/usr/lib/node_modules/playwright/index.mjs`; Chromium must be installed.
   Optional `UI_SCREENSHOT` writes a full-page mobile screenshot.
4. Create the printed shutdown file and stop the owned Vite process after the run.

The browser script checks both-domain lookup/add, independent monitoring, real
copy imports, equal numeric episode/movie IDs, receipt reload, delayed stale TV
lookup rejection after switching to movies, lost execution responses, explicit
resume after an execution request was never delivered, byte preservation, library
readback, mobile overflow and browser script errors. Lost responses are deliberately
simulated at the browser network boundary; actual import effects use real handlers.
The transport suite separately checks bounds, structured errors, methods, typed
bodies and no implicit mutation retries. Existing legacy transport assertions remain
unchanged.

Initial library/import verification on 2026-09-25 passed 129 Rust tests (the manual fixture is
ignored in the default suite), 12 frontend tests, generated-contract checks,
Svelte checks with zero errors/warnings, formatting, locked compilation and the
frontend build. The manual fixture and browser regression also passed separately;
the parent inspected the 390px screenshot and stopped both owned servers.

## Existing-library metadata refresh

Selected series and movie details expose their native typed metadata-refresh target.
Refresh metadata creates a durable command; Check metadata status reads scoped,
paged history and command detail. State, attempts, error, timestamps and successful
record counts remain visible. Cancellation affects only that metadata command;
terminal history deletion requires explicit confirmation and leaves library/media
records intact. There are no percentage estimates, automatic POST retries or
metadata schedules in this panel.

An initial successful target-history read is required before submission. Lost write
responses lock further mutations until explicit readback. Leaving/re-entering a
selected title reads persisted command state before enabling another explicit action.
Polling is sequential and stops on hidden documents/unmount. Late responses cannot
replace another selected title. Completion does not reload or overwrite unsaved
library settings; Reload library details explicitly warns that it discards edits.

The extended browser scenario checks both-domain metadata updates, equal library
IDs, source/file and monitoring preservation, a newly discovered episode, lost
accepted command responses without automatic resubmission, cancellation and terminal
history deletion, retry errors, stale selection and reload without command creation.
It uses the fixture's owned catalogue modes; this does not establish live catalogue
behavior or process-restart recovery. Native backend recovery evidence is separate.

## Not claimed

Initial provider setup and the [slice 1 gate](slice-1-gate.md) are now delivered;
full provider/settings breadth remains incomplete. Full add/detail/import routes, profile selection, artwork, discovery,
replacement, bulk import, search-on-add and complete controller parity remain
required later work. This workspace does not introduce a routing framework, query
cache, schema migration or dependency. Browser verification uses the observed
external Playwright installation (1.63.0), not a project-pinned browser toolchain.
No live services, production media, upstream runtime equivalence, full accessibility
audit, multi-tab recovery or production deployment was tested. The browser scenario
uses same-device copy; deeper transfer and recovery evidence remains in Rust tests.
