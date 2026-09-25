# Slice 3 gate — passed 2026-09-25

The combined parity plan's exact acceptance is:

> TV episode and eligible movie reach the client; ambiguous remake/year and wrong media category are rejected; background automation respects availability and configured delay; user-invoked search preserves the availability exception; cutoff/upgrade foundation verified

Iteration 40 adds persistent singleton automatic search and interactive selection for both domains. Both are user-invoked; automatic selection retains the explicit user-search context. RSS remains background automation. The independent testing-verification specialist authored HTTP/browser regressions; a separate read-only review mapped the evidence to this gate. Execution results below are the parent's independent verification.

## Acceptance evidence

| Requirement | Runnable evidence and scope |
| --- | --- |
| TV episode and eligible movie reach the client | `tests/rss_commands.rs::rss_grabs_both_domains_and_replay_does_not_duplicate_submissions` asserts two actual mock submissions with typed receipts. `tests/download_processing.rs::real_add_rss_owned_completed_downloads_import_both_domains_once` begins with metadata Add APIs and imports/upgrades both domains. |
| Ambiguous remake/year and wrong media category are rejected | `tests/release_decisions.rs::both_domain_decisions_and_real_search_consumer` asserts wrong-year, ambiguous same-title/year and wrong-category rejection codes. `tests/search_commands.rs::wrong_category_and_movie_remake_never_reach_download_client` checks zero submissions and specific matching/category failures. Browser `verifySearchGrab` displays wrong-category reasons. |
| Background automation respects availability and configured delay | `tests/rss_commands.rs::rejected_availability_is_reevaluated_and_schedules_are_revision_fenced` rejects an unavailable movie and submits after availability changes. `delayed_payloads_are_private_and_missing_key_rejects_without_starvation` retains both domains pending. The shared decision regression checks the exact TV deadline, movie availability rejection and later acceptance in both domains. |
| User-invoked search preserves the availability exception | `tests/search_commands.rs::automatic_and_interactive_search_preserve_context_targets_and_replay` rejects future-dated targets through RSS, then hands off both domains through automatic and interactive user searches. It checks later-page profile ranking, exact lower-ranked interactive selection, concurrent selection replay and typed targets. The browser adds distinct future-dated TV/movie targets and exercises both modes. |
| Cutoff/upgrade foundation verified | The shared decision regression accepts an upgrade, rejects a met cutoff and treats equivalent quality-group members as one rank. `src/search/downloaded.rs::tests::downloaded_files_require_exact_target_quality_and_size` rejects cutoff replacements in both domains. Its real Add/RSS integration performs both-domain upgrades and preserves original files/associations on a failed commit before explicit recovery. |

The new search regressions also cover provider/identity drift, identity changes during remote preparation, retained offers across database reopen, expiry, selected-receipt replay after expiry and failed continuation without partial grab. An interrupted selected submission is reconciled without a second POST; uncertain presence remains `needs_attention`, not a trusted library association. The schema regression exercises actual version 26 upgrade, rollback, reopen, typed targets, immutable provenance, shared capacity and atomic encrypted-payload transfer.

The complete browser regression in [library-browser.md](library-browser.md) preserves prior library/provider/Activity/RSS/import checks, adds four actual search submissions, verifies read-only interactive listing, lost accepted create/grab responses with explicit readback, retained receipt history and mobile layout. It uses [the isolated fixture instructions](library-ui.md#runnable-isolated-browser-verification).

## Reproduce

```sh
cargo fmt --check
cargo check --locked
cargo test --locked
npm --prefix frontend run check
npm --prefix frontend test
npm --prefix frontend run build
git diff --check
```

Parent independent verification passed: 173 distinct Rust tests (175 reported passes include repeated child-process helpers), 22 frontend transport tests, generated contract drift checks, Svelte checks with zero errors/warnings, formatting, locked compilation, build and diff checks. A fresh complete browser run also passed; its fixture/Vite processes stopped and scratch directories were removed. All service listeners are owned ephemeral loopback mocks; databases, credentials and media are synthetic. Requirements remain pinned to Sonarr `76c684e097f16ac216e6213845e5cac372774995` and Radarr `c90668a520664ad0c91812cfee57c41928ad2148`. Source inspection establishes requirements, not observed upstream equivalence. Broad ledger rows remain Partial.

## Not claimed

- No live provider/client/catalog behavior, production media, remote storage, upstream runtime or V3/V5 wire equivalence. This gate covers the native singleton foundation; bulk/season/series/missing/cutoff search fanout, forced overrides/push, packs and full custom-format/ranking breadth remain required work.
- Delay transitions use supplied time or fixture state changes; expiry uses isolated timestamp fault injection. These are deterministic contract checks, not long wall-clock observations. Some decision tests seed library facts; real metadata Add producers are separately exercised through HTTP and browser workflows.
- No slice 4 or full parity claim. Existing import recovery evidence does not establish every process-kill, power-loss, cross-device, naming or failed-download replacement path. Uncertain submissions remain unresolved rather than being retried automatically.
- New state: migration 27 search commands/results, immutable search-to-candidate provenance and encrypted offers with 30-minute expiry. Selected provenance, receipts and hash claims remain retained and can exhaust bounded capacity; no automatic pruning of selected history. No project dependencies were added. Browser execution uses separately installed Playwright/Chromium.
