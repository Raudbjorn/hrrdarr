# Slice 2 gate — passed 2026-09-25

The combined parity plan's exact acceptance is:

> Restart/retry does not duplicate external actions; mixed-domain tasks retain their targets; errors surface in API/UI

Iteration 39 passed this acceptance for the delivered native persistent-work paths in both domains. The parent independently ran the final Rust/frontend checks and the complete browser regression against a fresh owned fixture. A separate read-only testing-verification assessment mapped the tests to the three requirements; its source review was supplementary, not execution evidence.

## Acceptance evidence

| Requirement | Runnable evidence and observed scope |
| --- | --- |
| Restart/retry does not duplicate external actions | `tests/commands_api.rs::process_ownership_rejects_competitor_and_recovers_killed_reads_in_both_domains` kills a child process during active TV and movie reads, rejects a competing owner and recovers the same commands. `tests/rss_commands.rs::interrupted_submission_reopens_with_typed_identity_and_only_reconciles` interrupts submissions in both domains, reopens the database and permits only read-only reconciliation, never another add POST. Uncertain presence remains a visible unresolved receipt. TV also exercises three-read absence exhaustion. |
| Mixed-domain tasks retain their targets | Refresh, metadata and blocklist command tests preserve typed identities across retries/cancellation/reopen. RSS and completed-download tests use a shared client with separate categories and colliding numeric episode/movie IDs. Real metadata Add → RSS → Grab → completed refresh → mapped import creates the correct library associations and history. A subsequent real RSS upgrade preserves original associations/bytes on failed commit, then resumes the same operation after policy disablement. Completed reopen/replay preserves operation IDs, four history facts and four permanent hash claims without another submission. |
| Errors surface in API/UI | `frontend/tests/library-browser.mjs`, using `tests/library_ui_fixture.rs`, exercises scoped refresh failures, bounded retries, prior-success retention, cancellation, schedules and uncertain-response readback. Completed-import scenarios expose missing mappings for both domains and a failed movie replacement. Explicit retry/resume uses the same receipt/operation; dropped accepted policy/resume responses cause readback, not automatic repeat writes. Retained-original feedback, mobile overflow and browser-error checks pass. |

The completed-download HTTP fixture additionally verifies exactly three automatic transient detail reads before a blocked, unlinked result, explicit retry accounting, wrong filename/ambiguous file rejection, nested source mapping, sample exclusion, actual hardlink inode identity, stable movie-file IDs and retained recovery bytes. The browser performs an initial TV hardlink import and upgrades the movie created by its earlier manual-import scenario using copy. It asserts exactly two client add requests after retries/recovery; no receipt or processing status is inserted directly by the browser fixture.

## Reproduce

From the repository root:

```sh
cargo fmt --check
cargo check --locked
cargo test --locked
npm --prefix frontend run check
npm --prefix frontend test
npm --prefix frontend run build
git diff --check
```

The final run passed 164 distinct Rust tests (166 reported passes include repeated child-process helper invocations), 21 frontend transport tests, generated API drift checks, Svelte checks with zero errors/warnings, formatting, compilation, build and diff checks. The ignored browser fixture is exercised separately using the [isolated browser instructions](library-ui.md#runnable-isolated-browser-verification), with the additional [RSS/import scenario](library-browser.md). Use a fresh fixture per complete browser run. All listeners bind owned ephemeral loopback ports; scratch media and synthetic credentials are removed when the fixture shuts down.

Related focused evidence:

- `tests/download_processing.rs`: actual both-domain producers, bounded reads, upgrades, failure, explicit recovery and completed reopen.
- `tests/metadata_commands.rs` and `tests/blocklist_commands.rs`: typed local operations, rollback/cancellation and restart behavior under the shared worker.
- `src/import/tests.rs` and `src/import/owned_tests.rs`: isolated filesystem/database phase recovery, failed commits, shared TV originals, retirement before checkpoint, changed/reappeared originals, and failure-status persistence errors.
- `src/db/processing_tests.rs`: actual schema-25 upgrade/rollback/reopen, typed ownership, retry/retirement guards and retired snapshot replay refusal.
- `tests/indexer_protocol.rs` and the private payload unit: magnet links/enclosures normalize at the shared indexer boundary, including older pending payloads. The completed-download test also uses a movie magnet link without a separate magnet attribute.

Requirements remain pinned to Sonarr `76c684e097f16ac216e6213845e5cac372774995` and Radarr `c90668a520664ad0c91812cfee57c41928ad2148`. Their source inspection establishes inventory, not observed upstream runtime equivalence. Broad command/controller/provider rows remain Partial in the parity ledger; this gate does not close those rows.

## Not claimed

- No live clients/indexers/catalogs, production credentials/media, remote storage, upstream runtime or V3/V5 wire equivalence. The complete command catalog, cron/maintenance breadth and remaining concrete providers are unfinished.
- RSS submission interruption is runtime shutdown/database reopen; killed-process evidence covers reads. The completed-download HTTP reopen happens after imports finish. Separate import tests cover in-flight phase recovery, not an end-to-end killed-process import or power-loss test.
- No slice-3/4 or full automation/parity gate. Manual grab, advanced TV numbering/packs, general naming, failed-download replacement/blocklisting, extras, client removal and remaining UI workflows retain their own requirements.
- Creating a retirement directory before its identity is checkpointed can leave a preserved artifact requiring operator reconciliation. A failure that cannot be written to the database is latched in the running process; after restart it may undergo one idempotent recovery attempt. The latch covers returned errors, not arbitrary task panics. Cross-device and every filesystem failure timing are not established.
- New state includes migration 26's processing policies, processing/receipt-import journals, immutable replacement facts and retained recovery artifacts; a bounded process-local failure set prevents repeated execution when error-status persistence fails. Recovery artifacts and submission/hash claims are not automatically pruned. No project dependencies were added; browser checks use the separately installed Playwright/Chromium.
