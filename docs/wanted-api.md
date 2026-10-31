# Wanted API and UI

Native Wanted lists for both domains: **missing** and **cutoff unmet**, plus Wanted search admission. Implementation: `src/wanted.rs` (HTTP, queries), `src/quality_profiles.rs` (`quality_threshold`, pure), `src/search/decision.rs` (`movie_available`, the existing background availability rule, now shared), `src/commands/search.rs` (`admit_wanted`). UI: `frontend/src/lib/WantedPanel.svelte`, `frontend/src/lib/wanted-view.ts`, routes `#/wanted/missing` and `#/wanted/cutoffunmet`.

Sonarr/Radarr Wanted controllers and frontends were used as requirements only (see the iteration85 requirements, decisions and search contract). This is a native contract; no V3/V5 wire compatibility is claimed.

## Routes

| Route | Body |
| --- | --- |
| GET `/api/v1/tv/wanted/missing` | `WantedPage<TvWantedItem>` |
| GET `/api/v1/tv/wanted/cutoff` | `WantedPage<TvWantedItem>` |
| GET `/api/v1/movies/wanted/missing` | `WantedPage<MovieWantedItem>` |
| GET `/api/v1/movies/wanted/cutoff` | `WantedPage<MovieWantedItem>` |
| POST `/api/v1/{tv,movies}/wanted/search` | `WantedSearchInput` → 202 `WantedSearchReceipt` |

The domain is part of the path, so equal numeric episode and movie IDs never meet: every row and receipt entry carries a typed `MediaTarget` (`episode` or `movie`).

## Membership

File presence is the active DB association (`media_files::active_file_sql`; a journaled, retired replacement file does not count). No filesystem stat or provider request happens on GET.

| List | TV (episodes) | Movies (library movies) |
| --- | --- | --- |
| Missing | No active file; `unixepoch(air_date_utc) + series.runtime × 60 ≤ now`. Specials (season 0) included. Unknown air time or unknown series runtime is **excluded and counted** (`unknown_air_date`, `unknown_series_runtime`). Episode runtime is never substituted. Future and in-progress episodes are excluded. | No file, `year > 0`. **No availability or release-date gate** (reference behavior): unreleased movies stay listed. Year unknown/0 is excluded and counted (`unknown_year`). Orphan catalog metadata is never listed. |
| Cutoff unmet | Active file, season ≥ 1, current quality rank below the profile threshold. | File, current quality rank below the profile threshold. |

Monitoring is a partition (`monitored` default): TV `monitored` means episode **and** series monitored, `unmonitored` is its complement, `all` disables the filter. Season monitoring is reported (`season_monitored`), not filtered. Movies compare the movie flag.

**Quality-only cutoff** (`quality_threshold`): every quality in the profile (allowed or not) gets its outer item position; grouped qualities share one rank. With upgrades allowed, the threshold is the configured cutoff quality's/group's position; with upgrades disabled it is the first allowed item. A file is below cutoff when its rank is lower. Custom-format scores, revisions, size and language never affect membership. Files whose quality is absent from file metadata or from the profile, or whose profile has no resolvable policy, are excluded and counted as `unknown_cutoff` — never reported as met or below.

Movie rows carry `availability`: `available` / `unavailable` from the existing background rule (`movie_available`, minimum availability + release dates + `release_delay_policies.availability_delay_days`), or `ungated` when no movie policy row exists (background decisions then apply no availability veto). User-invoked Wanted searches bypass availability regardless.

## Query (strict)

| Key | Values | Default |
| --- | --- | --- |
| `limit` | 1–100 | 50 |
| `offset` | 0–100000 | 0 |
| `monitored` | `monitored`, `unmonitored`, `all` | `monitored` |
| `sort` | TV: `air_date`, `series_title`; movies: `title`, `year`, `in_cinemas`, `digital_release`, `physical_release`; cutoff lists also `quality` | TV `air_date`, movies `title` |
| `direction` | `asc`, `desc` | `asc` (native choice; the reference omitted direction defaults to descending) |
| `availability` | movies missing only: `all`, `available` (`available` or `ungated`), `unavailable` | `all` |

Unknown, duplicate, empty or domain-inapplicable keys, non-decimal numbers and raw queries over 4096 bytes return 400 `invalid_wanted_query`. Null sort values sort last in either direction; ties are ascending typed IDs (`series_title` ties: series ID, season, number, episode ID). Title sorts are `lower(title)`; no persisted sort-title authority exists yet (no article stripping).

Count, page and diagnostics come from one read transaction with one captured clock (`evaluated_at`). An offset past the end returns no items and the unchanged total. Pages requested at different times are not one immutable snapshot. Errors: 413 `wanted_resource_limit` (response > 8 MiB, > 1000 profiles, or > 10 000 rows scanned by the movie availability filter), 503 `wanted_unavailable` (storage or the 10 s deadline), 500 `wanted_state_invalid` (invalid persisted profile/settings). Bodies are the static `ApiErrorEnvelope`.

## Search admission

`WantedSearchInput`: `request_id` (client UUID), `kind`, `selection`, `indexer_id`/`indexer_revision`, `client_selection` (`default` or `explicit`), `priority`. Mode is always **automatic** user search (best accepted release is grabbed). The existing single search command's provider scope, protocol and key checks apply.

- `{"kind":"ids","ids":[...]}` — explicit targets (deduplicated, 1–1024), searched even if unmonitored, not in the list, or already being searched. Unknown IDs fail the whole request (409 `target_changed`).
- `{"kind":"all","monitored":..., "availability"?: ...}` — every current member under that filter, captured inside the admission transaction (at most 1025 read; more than 1024 is 413 `wanted_search_target_limit`). Targets with an active (queued/running/retry-wait) search command are skipped and counted (`skipped_active`).

All children are inserted in one Immediate transaction as ordinary `search_commands` rows (no batch table, no second worker). If the remaining shared active-command pool (1024) cannot hold them all, the request is refused as a whole (429 `command_history_full`). Child command IDs derive from the request UUID, a SHA-256 digest of the canonical body and the target.

### Durable receipts

Every admission, **including one that matches nothing or whose matches are all already being searched**, writes a receipt in the same Immediate transaction as its children (migration `0091_wanted_search_receipts.sql`, ordinal 80): request UUID, canonical-body digest, domain, `matched` / `skipped_active` / `admitted` and the admitted target identities with their deterministic child command IDs (`wanted_search_receipts`, `wanted_search_receipt_targets`). Provider credentials and payloads are not stored; the digest binds provider, revision, client selection, kind, filter, priority and domain. A receipt and its children commit or roll back together, and duplicate UUIDs are serialized by the transaction.

A retry authenticates against the receipt **before** selecting targets or reading providers:

- Same UUID and body: 202 with `replayed: true`, the original counters and original child identities, and no new commands, regardless of library, active-search or provider state afterwards (editing or deleting the provider, cancelling or pruning the children, or restarting does not matter).
- Same UUID and a different body, including a different priority, filter, kind, provider revision, client selection or media domain: 409 `command_conflict`, no writes.
- A request that is refused (validation, `provider_changed`, `target_changed`, 413, 429) writes no receipt, so correcting it and retrying under the same UUID is a fresh attempt.

Retention and capacity are explicit. A receipt is authoritative for **7 days** from creation (`WANTED_RECEIPT_SECONDS`); after that the UUID is expired and may be admitted again as a new request, so idempotency is time-limited, not indefinite. At most **4096** unexpired receipts and **65 536** unexpired admitted targets are kept (storage trigger and writer pre-check); a request that would exceed either is refused with 429 `wanted_receipt_capacity` and no writes. An unexpired receipt is never discarded to make room. Expired receipts do not count toward capacity and are removed (with their targets) by the next admission (up to 256 per call) and by terminal-history housekeeping, which owns cleanup (`docs/housekeeping.md`). Receipts are not pruned by command-history retention, so child cleanup never weakens an unexpired receipt.

Rollout boundary: admissions made before the migration have no receipt and cannot be reconstructed, so an empty pre-migration response is still not replayable. For those batches (and for a UUID whose receipt expired while child rows remain) replay falls back to the deterministic child commands as before: an exact replay returns the original capture with `replayed: true` and null counts, and pruned child history means a fresh admission. The UI resends only the same request after an uncertain response, and only on explicit user action.

TV search-all enqueues one episode search per episode: the existing search command supports a single target only, so season-grouped (pack-aware) search-all is **not** implemented.

## UI

`#/wanted/missing` and `#/wanted/cutoffunmet` with a TV/Movies switch: accessible table (labelled checkboxes, select-all on page), loading/error/empty states, unknown-fact diagnostics, monitoring/sort/direction/page-size (and movie availability) filters, pagination, monitor/unmonitor selected (existing episode/movie editor writes), search selected, search all behind an inline confirmation, receipt summary, per-command status readback (first 25) and explicit same-request retry after an uncertain admission.

## Evidence

- `cargo test --locked --test wanted_api` — both domains with colliding IDs; monitored partitions (episode vs series); aired-end boundary (one minute either side), future, unknown air date and unknown series runtime; specials in missing, excluded from cutoff; shared multi-episode file rows; grouped-equal, disallowed-lower, above, upgrade-disabled first-allowed and unknown-quality/no-metadata cutoff; movie year/unknown year, orphan exclusion, availability filter and ungated policy; sorts with null-last and tie order; pagination totals; GET leaves `PRAGMA data_version` unchanged; monitoring write moves rows; strict query rejection; selected/all admission, replay/conflict, active skip, typed movie vs episode targets, atomic rejection, 1025-target refusal and pool-capacity refusal.
- Unit tests: `quality_profiles::threshold_tests`, `wanted::tests`, `commands::search::wanted_child_tests`, `frontend/tests/wanted-view.test.mjs`, `frontend/tests/routes.test.mjs`.
- Browser: `frontend/tests/wanted-browser.mjs` (owned `library_ui_fixture`, `/api/fixture/wanted-seed`) drives both domains' missing and cutoff pages, keyboard selection, search selected, confirmed search all (including cancel), active-skip receipt, status readback, availability filter, unmonitor selected, and asserts the durable commands through the search-command API.

## Not claimed

- **Last-search time**: no native producer updates `episodes.last_search_time`, and movies have no last-search column. The TV field shows imported snapshot values only; no `last_search` sort, no oldest-searched-first ordering of search-all.
- **Sort title**: no persisted series/movie sort-title authority; title sorts are case-insensitive literal titles.
- **Grouped TV search-all** (series/season cohorts, season-pack search) and the **eligible tagged indexer union** per execution unit are not implemented; Wanted searches use one explicit indexer and the existing single-target command.
- **Receipts are time-limited and not retroactive**: replay is authoritative for 7 days per receipt (see Durable receipts); an expired UUID is a new request. Pre-migration empty admissions left no record and cannot be replayed; pre-migration non-empty batches still rely on their child commands. Verified with owned fixtures only (`tests/wanted_api.rs`); no restart-during-admission process kill, no live provider.
- **Download-queue exclusion**: search-all skips targets with active search commands only, not targets with an in-flight download.
- **V5 filters** (series IDs, profile IDs, series type, tags, include-specials, quality) and saved custom filters, persisted column/display options, per-row interactive search, detail navigation and manual import from Wanted rows are not implemented.
- Retired/quarantined replacement files are excluded through the shared active-file predicate, but no Wanted test seeds a retired file.
- No V3/V5 wire compatibility, no real indexer/download-client run (the worker is stopped or absent in all Wanted tests), no restart-during-admission process-kill test, no load test beyond 1027 episodes.
- The movie missing default follows the reference (no availability gate); the `availability` filter is a native addition.
