# Provider setup UI

The Providers workspace uses native `/api/v1/providers` contracts and generated
Rust-to-TypeScript DTOs for Torznab, Newznab and qBittorrent. It provides a paged
list, create/edit, explicit configuration deletion and saved connection tests.
Each provider may serve TV, movies or both; tests cover all saved scopes.
Implementation cannot change during an edit. Saving alone does not contact a
provider; testing does not submit downloads or provision missing client categories.

New scope options come from the existing schema endpoint. Users supply endpoint,
category IDs or download category names. Editing preserves the complete saved scope
object, including indexer search flags and qBittorrent imported categories,
priorities, initial state, content layout, sequential/first-last options and tags.
The minimal form edits active categories only; these additional options are not
editable here. Disabling a scope removes its settings when saved. Backend validation
remains authoritative for categories, endpoint, credentials and domain rules.

Credentials are write-only, never read back or stored by the UI. Preserve omits the
credential member, retaining the entire encrypted bundle including hidden private
TV/movie parameters. Replace sends a new API-key or username/password bundle;
Clear sends null. Both explicitly warn that the entire prior bundle is discarded,
including private parameters absent from the form. Changing a credential-bearing
endpoint requires explicit replace/clear to avoid silently forwarding stored secrets
to another destination. Secret fields clear after a save attempt, provider selection,
credential-mode change, implementation change or leaving this workspace. Browser
password-manager behavior is outside the application's storage guarantee.

Edits and deletion use the saved revision. Conflicts require reloading the provider;
the editor does not silently rebase changes. Network/unreadable mutation responses
lock further saves/deletions because the write may have committed: refresh the list
and select the actual saved record before proceeding. There is no automatic mutation
retry. Tests require saved, unchanged form data, allow a bounded 35-second response,
and read back the provider after both success and failure. Older saved observations
remain timestamped and are not presented as a new successful test. A revision change
during a test requires a reload.

The existing library/import workspace remains available. Switching to Providers
unmounts its import panel, stopping its polling; returning restores its persisted
receipt and checks status without automatically executing another transfer.
Development uses the explicit server-side `HRRDARR_API_ORIGIN` Vite proxy described
in [library-ui.md](library-ui.md); no production static serving, CORS bypass or live
service default is introduced.

## Verification limits

Frontend contract checks, build and transport tests validate types, request methods,
revision payloads, credential omission/clear/replacement, DELETE 204, and bounded
client behavior. Browser evidence must use the actual UI against isolated native
endpoints and owned mock providers, including authentication failure/success,
scoped setting preservation and stale revisions. Such checks do not establish live
provider equivalence or complete settings parity. Draft testing, presets, discovered
category selection, advanced scope editors, bulk operations, every remaining
provider and full routes `ui.010`/`ui.013` remain required work. No dependency,
backend route or schema change is introduced by this UI.
