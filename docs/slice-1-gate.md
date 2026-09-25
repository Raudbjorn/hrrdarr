# Slice 1 evidence — gate not passed

The combined parity plan's exact acceptance is:

> User can add one series and one movie, import each safely, and test an indexer/client; existing import/path-corruption bugs fixed

**The whole slice-1 gate is not passed.** The connected native HTTP path below is
exercised, and the library/import browser workflow now has [runnable evidence](library-ui.md).
The frontend still lacks provider configuration and testing, so the complete
user-facing workflow remains unfinished. API evidence does not substitute for that
workflow or its error/reconciliation feedback.

The pending scope remains visible in the local
[parity ledger](../.do-not-commit/loop/LEDGER.md): `ui.005` add/new,
`ui.010` download-client settings, `ui.013` indexer settings, `ui.029`/`ui.036`
TV/movie import, and `ui.032`/`ui.038` library detail. Those rows are not closed by
this report. Minimal user-flow delivery must also connect API transport and handle
uncertain/timeout outcomes without implying an import never happened. Full
controller breadth remains in its existing rows, not a new foundation row.

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
independent full run passed 129 Rust tests, 8 frontend tests, formatting, locked
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

- No complete slice-1 browser workflow or gate. Library/import interactions have
  separate browser evidence; provider UI and full accessibility validation remain
  incomplete. Existing UI rows are not closed by this report.
- No live metadata/indexer/client, real credentials, production database/media,
  download submission, automatic polling/search/grab/import or remote storage.
- Copy evidence here does not prove failed replacement recovery, cross-device
  behavior, media validity, power-loss durability or every filesystem failure.
  Those broader contracts retain their separate evidence and remaining scope.
- Provider test success proves the configured mock's scoped read probes, not future
  availability, authorization for destructive operations or a completed download.
- Full library/provider controller parity, ancillary snapshot activation and policy
  evaluation are not delivered by this test-only change.
- No production state, schema, dependencies or behavior are introduced. The test
  creates temporary databases, files, synthetic encrypted credentials and owned
  loopback servers, then removes/stops them.
