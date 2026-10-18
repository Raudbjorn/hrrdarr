# Provider setup UI

The Providers workspace uses native `/api/v1/providers` contracts and generated
Rust-to-TypeScript DTOs for Torznab, Newznab, qBittorrent, SABnzbd and Transmission. It provides a paged
list, create/edit, explicit configuration deletion and saved connection tests.
Each provider may serve TV, movies or both; tests cover all saved scopes.
Implementation cannot change during an edit. Saving alone does not contact a
provider; testing does not submit downloads or provision missing client categories.

New scope options come from the existing schema endpoint. Users supply endpoint,
category IDs or download category names. Editing preserves the complete saved scope
object, including indexer search flags and qBittorrent imported categories,
priorities, initial state, content layout, sequential/first-last options and tags.
The form edits categories, per-domain RSS/automatic/interactive search switches,
each indexer scope’s download-client preference, and qBittorrent eligibility tags for each domain. Eligibility tags come from the local tag catalog and are separate from tags added to remote torrents. Other options are preserved.
Client choices include enabled download clients supporting that domain and the indexer's protocol (Torznab with qBittorrent or Transmission, Newznab with SABnzbd); SABnzbd scopes add a category plus recent/older priority, and its test result shows version and warnings; Transmission composes its endpoint from Host, Port, Use SSL and URL base (showing the RPC URL), and each scope chooses a category (subfolder and label) or download directory, recent/older queue priority and add paused, with username/password credentials only; use the
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

RSS and download-search forms start with default client selection and allow an explicit override, including when the indexer has a preferred client. The preference remains visible even when unavailable; actual default resolution happens against the matched library owner's tags on the server. Requests persist their selection intent. RSS schedules retain default intent, and release/search details display the current client and ordered attempts with their selection rounds, including safe failure and uncertainty. Scope switches, provider revisions and backend validation still apply. Switching RSS domains resets selection; the library keys search components by target so pending reads are disposed when that target changes.

Provider tag editing requires a current scoped tag catalog. Provider/library/profile writes invalidate related tag usage, and a missing tag remains visible until the draft is reconciled. Tags used by clients appear in tag usage and cannot be deleted while referenced.

## Verification limits

Run `node frontend/tests/indexer-binding-browser.mjs` against a fresh
[owned browser fixture](library-ui.md#runnable-isolated-browser-verification),
with its `UI_URL`, `UI_PROVIDER_ORIGIN` and `UI_SCRATCH`. The scenario checks
both-domain binding and policy save/reload, actual RSS schedule/search admission,
disabled/deleted preference retention, explicit override availability, and
explicit clearing of deleted-client bindings.
`client-selection-browser.mjs` additionally requires `UI_ALTERNATE_PROVIDER_ORIGIN` from the same owned fixture. It exercises two independent mock clients, scoped tag editing, TV fallback, movie single-client failure, an explicit override, safe exhaustion and same-client round recovery on actual retry deadlines, and uncertain submission reconciliation. These scripts require recorded successful runs before being treated as verification evidence.
Backend binding and interrupted-submission regressions live in
`tests/rss_commands.rs` and `tests/search_commands.rs`.

Frontend contract checks, build and transport tests validate types, request methods,
revision payloads, credential omission/clear/replacement, DELETE 204, and bounded
client behavior. Browser evidence must use the actual UI against isolated native
endpoints and owned mock providers, including authentication failure/success,
scoped setting preservation and stale revisions. Such checks do not establish live
provider equivalence or complete settings parity. Draft testing, presets, discovered
category selection, advanced scope editors, bulk operations, every remaining
provider and full routes `ui.010`/`ui.013` remain required work. No new backend route is introduced. Migration 0052 supplies scoped eligibility tags and durable selection attempts and rounds. The provider transport directly references the already-locked Rust TLS dependency to distinguish certificate-trust failures without exposing private error text.
