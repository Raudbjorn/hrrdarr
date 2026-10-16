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
The form edits categories, per-domain RSS/automatic/interactive search switches,
and each indexer scope’s download-client binding. Other options are preserved.
Client choices include enabled qBittorrent clients supporting that domain; use the
provider-list pages to browse choices. An existing binding outside the current page
or no longer available remains visible by ID and is preserved until explicitly
changed or cleared. Disabling a scope removes its settings when saved. Backend validation
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

RSS and download-search forms honor the selected indexer’s domain-specific binding.
They fetch the bound client by ID even when it is outside the current provider page,
lock that selection, and block new requests with a visible explanation when the
client is missing, disabled, or does not support the domain. Unbound indexers retain
manual client selection. RSS and search actions also respect their corresponding
scope switches. Switching RSS domains resets provider selection; the library keys
search components by media target so their selections and pending reads are disposed
when the target changes. Backend revision and binding checks remain authoritative
if a provider changes after the form loads. Reconciliation of previously submitted
work is independent of this selection UI.

## Verification limits

Run `node frontend/tests/indexer-binding-browser.mjs` against a fresh
[owned browser fixture](library-ui.md#runnable-isolated-browser-verification),
with its `UI_URL`, `UI_PROVIDER_ORIGIN` and `UI_SCRATCH`. The scenario checks
both-domain binding and policy save/reload, actual RSS schedule/search admission,
disabled/deleted client handling, restoration of unbound client selection, and
explicit clearing of deleted-client bindings.
Backend binding and interrupted-submission regressions live in
`tests/rss_commands.rs` and `tests/search_commands.rs`.

Frontend contract checks, build and transport tests validate types, request methods,
revision payloads, credential omission/clear/replacement, DELETE 204, and bounded
client behavior. Browser evidence must use the actual UI against isolated native
endpoints and owned mock providers, including authentication failure/success,
scoped setting preservation and stale revisions. Such checks do not establish live
provider equivalence or complete settings parity. Draft testing, presets, discovered
category selection, advanced scope editors, bulk operations, every remaining
provider and full routes `ui.010`/`ui.013` remain required work. No dependency,
backend route or schema change is introduced by this UI.
