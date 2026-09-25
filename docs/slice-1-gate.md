# Slice 1 gate — passed 2026-09-25

The combined parity plan's exact acceptance is:

> User can add one series and one movie, import each safely, and test an indexer/client; existing import/path-corruption bugs fixed

The connected native HTTP path and the minimal library/provider browser workflow
passed independent parent verification in iteration 33. **Slice 1 passed for the
literal acceptance above.** The browser regression uses real native handlers
against isolated metadata/indexer/client mocks, not fetch stubs.

Full scope remains visible in the local
[parity ledger](../.do-not-commit/loop/LEDGER.md): `ui.005` add/new,
`ui.010` download-client settings, `ui.013` indexer settings, `ui.029`/`ui.036`
TV/movie import, and `ui.032`/`ui.038` library detail. The bounded workflow below
does not close their advanced editors, actions or broader controller requirements.

## Connected browser evidence

The opt-in fixture `tests/library_ui_fixture.rs::library_ui_fixture` creates a
private scratch database, media files and three owned loopback listeners. It
prints a `UI_FIXTURE` JSON manifest containing the API/provider origins, paths and
shutdown file. It creates no library or provider records directly. Normal shutdown
joins the servers and removes scratch data; maximum lifetime is 15 minutes.

```sh
cargo test --locked --test library_ui_fixture -- --ignored --nocapture
# In another shell, use the printed api origin:
HRRDARR_API_ORIGIN=http://127.0.0.1:API_PORT npm --prefix frontend run dev -- --port 0
# Use Vite's printed origin and the fixture manifest values:
UI_URL=http://127.0.0.1:VITE_PORT \
UI_PROVIDER_ORIGIN=http://127.0.0.1:PROVIDER_PORT \
UI_SCRATCH=/path/from/fixture/manifest \
node frontend/tests/library-browser.mjs
# Create the manifest's shutdown file after review, and stop the owned Vite process.
```

The standalone runner uses installed Playwright at
`/usr/lib/node_modules/playwright/index.mjs`, or `PLAYWRIGHT_MODULE` pointing to an
installed module. Playwright 1.63.0 was observed for this run; it is not a new pinned
repository dependency. Use a fresh fixture per run because successful adds and
imports intentionally persist.

On 2026-09-25 testing-verification and the parent independently ran the complete
browser runner against separate fresh fixtures; both passed:

- TV/movie lookup, selected add, monitoring, typed equal local IDs, import preview,
  explicit execution and reload/readback against real handlers.
- Stale lookup suppression; a real execute response deliberately dropped *after*
  completion reconciles by status without a second execute. A request dropped
  *before* delivery retains the same preview and requires explicit resume.
- UI creation of Torznab, Newznab and qBittorrent with both TV/movie scopes. Bad
  synthetic credentials produce saved failure; explicit replacement produces
  scoped success. qBittorrent uses actual mock login/SID authentication and separate
  category probes. Indexer mocks accept only the configured category probes.
- An independent actual API update to a UI-created provider causes the stale UI
  save to conflict and lock; explicit reload recovers. A later visible priority
  edit preserves both scopes and hidden TV/movie options. Private indexer
  parameters remain encrypted and are observed only as bounded boolean evidence
  at the mock; they reach their own domain probe without crossing domains.
- Credential clear removes the complete bundle and causes test failure;
  replacement restores success. Saved secrets disappear from editor inputs and
  never appear in rendered content or localStorage. Saved success and edited
  priority survive page reload for every provider.
- No uncaught browser errors and no horizontal overflow at a 390px viewport.

`frontend/tests/api.test.mjs` passes 14 transport tests, including explicit
credential omission versus replacement/null, whole scoped settings, structured
revision conflicts, successful empty delete responses and no automatic retries
on an uncertain test call. These stub tests supplement, rather than replace, the
real browser scenario. See [library UI](library-ui.md) and
[provider UI](provider-ui.md) for delivered scope.

## Connected HTTP evidence

```sh
cargo test --locked --test metadata_lookup
```

`tests/metadata_lookup.rs::lookup_selected_add_and_safe_import_create_both_domain_targets`
uses one temporary database and one native API server. Separate owned loopback
mocks provide metadata and provider protocols. Library targets and provider
configurations are created through actual HTTP handlers; SQL is used only to
inspect results and install/remove intentional failure triggers.

| Acceptance boundary | Observed assertions |
| --- | --- |
| Add a series and movie | Scoped metadata searches return explicit TV/movie identities. Selected add refetches canonical detail and creates the TV seasons/episodes and movie. Both use external ID101 without domain confusion; the imported episode and movie also share local ID1. Missing/contradictory metadata and duplicate episode identities fail without partial library creation; a valid empty upcoming catalog is distinct from missing arrays. |
| Import each safely | Real preview/execute endpoints copy separate scratch files to the API-created episode/movie targets. Source and destination bytes match. File associations, history, library statistics and monitoring remain correct after duplicate-add rejection and database reopen. |
| Test indexers | Torznab and Newznab are each created with TV5030/movie2000 scopes and an encrypted synthetic credential. Each test first encounters an authenticated feed error after successful capabilities, surfaces a redacted HTTP failure and persists failure status, then succeeds. Recorded requests prove both scoped feed probes occurred. |
| Test shared client | A qBittorrent configuration has distinct TV/movie categories. Authentication failure surfaces through HTTP and saved status; a subsequent test succeeds for both domains. Recorded requests include preferences/categories and separate `torrents/info` probes for `tv` and `movies`. No add/delete/control request is made. |
| Persistence | All three provider responses, including revision and last-test result, are identical after reopening the same database and serving fresh provider routes. Library file counts, flags, history rows and imported bytes also survive. Secrets are absent from API responses and errors. |
| Atomicity and local preservation | Late SQL failures roll back selected-add graph/settings writes for both domains. Duplicate adds preserve existing local settings and imported files. Provider failure/success observations do not change config revision. |

The focused run on 2026-09-25 passed. Parent review of the actual diff and an
independent full run in iteration 33 passed 129 Rust tests, 14 frontend tests, formatting, locked
compilation, generated API drift checks, Svelte checks (zero errors/warnings),
the frontend build and diff checks. The client mock explicitly serves scoped
empty torrent lists; successful version/preferences responses alone are not treated
as authentication proof. No provider protocol submission or automatic download is
part of this scenario.

## Supplemental regression evidence

- `tests/import_api.rs::initial_import_http_both_domains_modes_and_preservation`
  covers initial copy/move/hardlink imports, retry and preservation on refused
  replacements. `src/import/tests.rs` covers journal recovery/failure boundaries.
- `tests/provider_indexer_api.rs::indexer_api_transport_status_staleness_and_redaction`
  and `tests/provider_qbittorrent_api.rs::shared_client_api_scopes_defaults_tests_reads_and_stale_results`
  cover deeper transport, scoping and stale-result boundaries. The connected test
  intentionally does not duplicate their complete matrices.
- [Slice-0 evidence](slice-0-gate.md), especially the snapshot integration tests,
  covers the old file-ID-as-path corruption and prototype data preservation.
  The import tests cover the former execute-without-association bug (`fnd.001`).

Protocol/source inventories are pinned to Sonarr
`76c684e097f16ac216e6213845e5cac372774995` and Radarr
`c90668a520664ad0c91812cfee57c41928ad2148`. Their inspection establishes requirements;
this local run observes the native implementation against synthetic wire fixtures.
It does not establish upstream runtime or V3 wire equivalence.

## Not claimed

- No full settings/library UI parity or complete accessibility audit. Initial
  provider and library/import browser workflows pass; advanced editors, bulk
  actions and remaining providers stay incomplete. Existing UI rows remain Partial.
- No live metadata/indexer/client, real credentials, production database/media,
  download submission, automatic polling/search/grab/import or remote storage.
- Copy evidence here does not prove failed replacement recovery, cross-device
  behavior, media validity, power-loss durability or every filesystem failure.
  Those broader contracts retain their separate evidence and remaining scope.
- Provider test success proves the configured mock's scoped read probes, not future
  availability, authorization for destructive operations or a completed download.
- Full library/provider controller parity, ancillary snapshot activation and policy
  evaluation are not delivered by this change.
- The verification adds no backend state, schema or dependencies. The UI adds
  frontend behavior over existing APIs; the tests create temporary databases,
  files, synthetic encrypted credentials and owned loopback servers, then remove
  or stop them. The fixture and Vite process must be shut down after manual review.
