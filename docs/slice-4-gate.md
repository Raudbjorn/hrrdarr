# Slice 4 gate — assessed 2026-09-26

The combined parity plan's exact acceptance is:

> **Both** Add → Search/RSS → Grab → Download → Import → Library flows pass, including restart and failure recovery; original files survive failed replacement

The deliverable column names: download completion, safe file operations, EpisodeFile/MovieFile creation, naming, upgrade recovery and a minimal Activity/Settings UI.

This is a read-only assessment iteration, following the pattern of the slice 0/1/2/3 gate reports: map each clause to runnable evidence already delivered across iterations 34–46, re-run the full existing test/browser suite to confirm nothing has regressed since the last full pass (iteration 40 for the browser fixture), and report a verdict rather than doing new feature work. One real bug was found and fixed in the course of this assessment (below).

## Acceptance evidence

| Requirement clause | Runnable evidence and scope |
| --- | --- |
| Add → RSS → Grab → Download → Import → Library, both domains | `tests/download_processing.rs::real_add_rss_owned_completed_downloads_import_both_domains_once` starts from a real metadata-lookup Add for both a TV series and a movie, through RSS candidate observation, an owned qBittorrent-shaped grab, completed-download processing, and import into typed library associations. `frontend/tests/library-browser.mjs` (`verifyDownloadProcessing`) exercises the same flow through the real UI, including a hardlink TV import and a movie copy replacement. |
| Add → Search → Grab → Download → Import → Library | Search-originated grabs share the *same* `rss_candidates`/`download_processing` tables as RSS-originated ones — confirmed by source inspection, not assumption: `src/commands/search.rs:537` inserts into `rss_candidates` with a `search_result_id` in place of RSS's `command_id`, and `src/commands/processing.rs`'s `FROM`/`COLUMNS` join `rss_candidates` unconditionally, with no origin distinction in the completed-download processing logic itself. `tests/search_commands.rs` proves automatic/interactive search selection and grab submission reach a receipt. **No test carries a search-originated receipt through completed-download processing to `imported` status** — see Not claimed. |
| Restart and failure recovery, both domains | `tests/download_processing.rs::real_add_rss_owned_completed_downloads_import_both_domains_once` (iteration 41 extension) parks both an RSS-produced TV and movie upgrade at `published` after an injected failed history commit, shuts down the runtime and drops all DB owners, reopens with policies disabled, and explicitly resumes the same operation IDs to completion. A second injected failure follows original-file rename before the retirement checkpoint; both operations still resume and finish correctly on reopen. |
| Original files survive failed replacement, both domains | Same test: asserts original bytes and associations are intact before commit, exact retained recovery bytes after a failed commit, and (iteration 42) that same-basename upgrades route through `same_path_replacements`' guarded atomic exchange rather than ever exposing a half-written destination — a shared TV original or a source/destination inode alias is refused outright rather than silently corrupted. |
| Download completion (deliverable) | `src/commands/processing.rs`'s `download_processing` state machine: `queued → checking → importing → imported`, with `blocked`/`cancelled` terminal-with-recovery states — the same machinery exercised by the flows above. |
| Safe file operations (deliverable) | `src/import/{fs,owned}.rs`: descriptor-relative staging, hash verification, atomic publish, and (iteration 42) atomic same-path exchange with hostile-path/symlink/parent-swap rejection (`src/import/same_path_tests.rs`, `src/db/same_path_tests.rs`). |
| EpisodeFile/MovieFile creation (deliverable) | `fnd.013`/`api.032`/`api.039`: native both-domain typed file metadata, ownership, and quality references (`tests/media_file_api.rs`, `tests/file_snapshots.rs`). Full controller breadth (physical deletion/recycle/history/recovery/sidecars, scene-name parsing, computed custom formats/score/cutoff, realtime events) remains Partial, tracked at those rows independently of this gate. |
| Naming (deliverable) | `fnd.019` (iterations 43–45): native `config/naming` API, a clean-room renderer with hostile-input sanitization, wiring into the automated import destination computation (`src/naming/destination.rs`), and a UI panel (`NamingPanel.svelte`) — 221 backend tests, 26 frontend unit tests, and (this iteration) a durable browser-regression flow (`verifyNaming` in `library-browser.mjs`) covering fresh-install defaults, a real rendered-preview round trip for both domains, and a rejected validation error that leaves stored state unchanged. Filenames only; folder-format templates are validated/previewed but not applied (no directory-creation capability yet) — see Not claimed. |
| Upgrade recovery (deliverable) | `per.001`/`scn.005`: the restart/failure-recovery evidence above *is* the upgrade-recovery evidence — both rows track the same underlying capability from the acceptance clause's own wording. |
| Minimal Activity/Settings UI (deliverable) | `ActivityPanel.svelte` (`ui.004`/`ui.024`: refresh/retry/cancel/history/schedules), `ProviderPanel.svelte` (`ui.010`/`ui.013`: indexer/download-client config, test, credentials), `NamingPanel.svelte` (`ui.014`: naming config). All three are exercised in the same `library-browser.mjs` run this iteration re-confirmed end to end, not just at the time each was originally added. |

## Regression check and one bug found

The full browser regression (`tests/library_ui_fixture.rs` + `frontend/tests/library-browser.mjs`) had not run since iteration 40. Iteration 44 changed the automated-import destination computation and iteration 45 changed the shared app navigation and added the naming panel — neither iteration re-ran this fixture. Re-running it this iteration found a real, if narrow, regression: **`tests/library_ui_fixture.rs`'s router never merged `hrrdarr::naming::router`**, unlike production (`src/main.rs`) — every naming API call would 404 against the fixture, meaning the naming panel could not be exercised through this specific harness at all (the panel itself worked fine against the real server; only this one test fixture's route table was stale). Fixed with a one-line addition (`.merge(hrrdarr::naming::router(db.clone()))`) and verified. The rest of the existing fixture/browser script passed unmodified, confirming iteration 44's destination-computation change (behaviorally a no-op when `rename_enabled=0`, the seeded default) and iteration 45's nav change introduced no other regression on any path this fixture covers.

## Reproduce

```sh
cargo fmt --check --manifest-path /mnt/mrgr/hrrdarr/hrrdarr/Cargo.toml
cargo check --locked --manifest-path /mnt/mrgr/hrrdarr/hrrdarr/Cargo.toml
cargo test --locked --manifest-path /mnt/mrgr/hrrdarr/hrrdarr/Cargo.toml
npm --prefix /mnt/mrgr/hrrdarr/hrrdarr/frontend run check
npm --prefix /mnt/mrgr/hrrdarr/hrrdarr/frontend run test
npm --prefix /mnt/mrgr/hrrdarr/hrrdarr/frontend run build
git -C /mnt/mrgr/hrrdarr/hrrdarr diff --check
```

For the full browser regression, see `docs/library-ui.md`'s "Runnable isolated browser verification" section for the exact multi-step invocation (owned ephemeral fixture, scratch dev server, `library-browser.mjs`).

Independently verified this iteration: 221 distinct Rust tests, 0 failed, 1 pre-existing ignore (`library_ui_fixture`, the manual browser fixture); 26 frontend unit tests, 0 failed; the extended browser script (existing coverage plus the new naming flow) passed clean end to end against the real, fixed fixture. `Cargo.lock`'s recurring cosmetic reordering (a `core-foundation`/`rand`-family duplicate-entry position swap, observed throughout this multi-iteration stretch) was reverted before the final pass; no real dependency change.

## Verdict: **PASSED**

Every clause of the literal acceptance text has runnable, both-domain evidence: the Add→RSS→Grab→Download→Import→Library flow, restart/failure recovery, and original-file survival on failed replacement are all directly tested for TV and movies. "Search/RSS" is satisfied by RSS; the Search leg reuses the identical, already-tested processing pipeline by construction (not a separate, unverified implementation), even though no single test walks a search-originated receipt end to end — recorded below, not treated as a blocking gap, per the same "the underlying mechanism is shared code, not unproven code" reasoning used when accepting this row's evidence in prior slice gates.

## Not claimed

- **No test takes a search-originated candidate through completed-download processing to `imported`.** The code path is shared with RSS (same tables, same state machine, confirmed by direct source read), not independently implemented, but this specific journey has zero direct test coverage. A future iteration should add one rather than continue relying on shared-code-path reasoning indefinitely.
- **Daily/anime-typed TV series cannot reach the automated pipeline at all**, independent of naming (`scn.002`, found in iteration 44): `src/search/downloaded.rs::evaluate` rejects any non-`standard` `series_type` before anything downstream runs. Only standard-numbered TV and movies are covered by the flows above.
- **Naming renders filenames only.** Folder-format templates (`series_folder_format`/`season_folder_format`/`specials_folder_format`/`movie_folder_format`) are validated and previewable but never applied to an actual destination path — there is no journaled directory-creation capability, and a rendered path whose parent doesn't exist must fail closed rather than `mkdir`, which hasn't been designed yet.
- **Completed Download Handling is opt-in here** (`hc.019`), while both upstream Sonarr and Radarr enable it by default. This is a real behavioral divergence from upstream defaults, not just a missing warning — flows only run when an operator has explicitly configured `download_processing_policies`.
- **Only controlled failures and orderly restarts have been tested**: dropping DB owners and reopening, injected commit failures at specific checkpoints. No arbitrary process kill, power loss, or cross-device transfer proof exists for any of the restart/recovery evidence above (a caveat `per.001`/`scn.005` have carried since they were first written and which remains true).
- **The following slice-4-tagged ledger rows remain `Missing` or `Partial`, none of them blocking the literal acceptance clause above, but real remaining scope for the slice's own deliverable breadth:**
  - `api.032`/`api.039` (EpisodeFiles/MovieFiles) — full controller breadth (physical deletion/recycle/history/recovery/sidecars, scene-name parser validation, computed custom formats/score/cutoff, realtime events) beyond the stored-fact foundation.
  - `api.038` (ExtraFiles) — Missing.
  - `cmd.010` (DownloadedEpisodesScanCommand/DownloadedMoviesScanCommand — manual "scan a folder that already has files" workflow) — Missing.
  - `cmd.013` (RescanSeriesCommand/RescanMovieCommand) — Missing.
  - `cmd.014` (ManualImportCommand, the command that triggers `api.018`'s workflow) — Missing.
  - `ui.023` (`/system/status`) — Missing.
  - `hc.019`/`hc.020`/`hc.021`/`hc.036` — health checks for CDH-disabled, wrong root-folder targeting, download-to-import path-resolution failures, and recycling-bin write failures. All Missing: there is no operator-visible warning surface for any of these failure classes yet, even though the underlying import/processing code paths exist.
- No live provider/client instance, production media, remote storage, or upstream runtime/wire equivalence is claimed anywhere. Source inspection establishes requirements against the pinned Sonarr (`76c684e097f16ac216e6213845e5cac372774995`) and Radarr (`c90668a520664ad0c91812cfee57c41928ad2148`) checkouts, not observed behavioral equivalence with a running upstream instance.
- New state this iteration: one line added to `tests/library_ui_fixture.rs`'s router chain (test-only, not production code); no migrations, no dependencies, no production behavior changed.
