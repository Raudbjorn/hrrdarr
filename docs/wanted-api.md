# Wanted API and UI

Native Wanted lists for both domains: **missing** and **cutoff unmet**, plus Wanted search admission. Implementation: `src/wanted.rs` (HTTP, queries), `src/quality_profiles.rs` (`quality_threshold`, pure), `src/search/decision.rs` (`movie_available`, the existing background availability rule, now shared), `src/commands/search.rs` (`admit_wanted`, and `record_dispatch`, the native last-search producer), migration `migrations/0076_wanted_last_search.sql` (schema version 76). UI: `frontend/src/lib/WantedPanel.svelte`, `frontend/src/lib/wanted-view.ts`, routes `#/wanted/missing` and `#/wanted/cutoffunmet`.

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

## Last search

Every row carries `last_search_time` (RFC 3339 UTC, whole seconds) and `last_search_source` (`native`, `imported` or `null`).

- **Native fact** (`episodes.native_last_search_at`, `movies.native_last_search_at`, Unix seconds). A durable search command (`/api/v1/search/commands`, both `automatic` and `interactive`, including every Wanted child) writes it when its indexer dispatch phase is over: after `fetch` returned (all pages fetched, or the first caught, classified indexer failure such as an HTTP error, a malformed page or the per-operation deadline) or the 35 s command deadline expired, **provided at least one eligible indexer request was issued**. Eligible means the provider revision, operation policy, transport and indexer backoff admission all passed (`RefreshClient::raw_search_observed`). The fact and the command's `dispatch_completed_at` progress are written in one Immediate transaction, and only while the command is still `running`, before any release decision, offer publication or grab. Each later attempt that dispatches moves both forward (never backwards).
- **Never written by**: admission (single or Wanted), worker claim, a refusal before any request (target identity changed, provider revision/policy changed, indexer in backoff), cancellation before the dispatch completed (including cancellation while a page is in flight), decisions, offer publication, interactive grabs, RSS sync, or the stateless `/api/v1/release-search` page preview. RSS follows the reference (Sonarr/Radarr update the per-item time only from targeted search dispatch, also when an indexer fails, provided at least one indexer was searched); the stateless preview has no durable command progress to commit with, so it is not counted.
- **Imported fact** (TV only): `episodes.last_search_time`, the Sonarr snapshot text, is kept unchanged as an imported fact and never written natively. Radarr snapshots carry no movie value. The effective TV value is the later of the native and the parseable imported instant; `last_search_source` names the winner (`native` on equal instants). An unparseable imported value counts as never searched.
- Storage enforces the producer contract: `dispatch_completed_at` cannot be set on insert, outside `running`, to NULL or backwards (triggers `search_commands_dispatch_admit`, `search_commands_dispatch`). Search commands expose `dispatch_completed_at` (`null` until dispatched).

## Query (strict)

| Key | Values | Default |
| --- | --- | --- |
| `limit` | 1–100 | 50 |
| `offset` | 0–2147483647 | 0 |
| `monitored` | `monitored`, `unmonitored`, `all` | `monitored` |
| `sort` | TV: `air_date`, `series_title`; movies: `title`, `year`, `in_cinemas`, `digital_release`, `physical_release`; cutoff lists also `quality`; every list `last_search` | TV `air_date`, movies `title` |
| `direction` | `asc`, `desc` | `asc` (native choice; the reference omitted direction defaults to descending) |
| `availability` | movies missing only: `all`, `available` (`available` or `ungated`), `unavailable` | `all` |

Unknown, duplicate, empty or domain-inapplicable keys, non-decimal numbers and raw queries over 4096 bytes return 400 `invalid_wanted_query`. Null sort values sort last in either direction; ties are ascending typed IDs (`series_title` ties: series ID, season, number, episode ID). **Exception — `last_search`**: never searched is ordered as older than every instant, so `asc` is oldest-searched-first with never-searched rows first, and `desc` puts them last; equal instants tie by ascending typed ID in both directions. Title sorts are `lower(title)`; no persisted sort-title authority exists yet (no article stripping).

Count, page and diagnostics come from one read transaction with one captured clock (`evaluated_at`). An offset past the end returns no items and the unchanged total. Pages requested at different times are not one immutable snapshot. Errors: 413 `wanted_resource_limit` (response > 8 MiB, > 1000 profiles, or > 10 000 rows scanned by the movie availability filter), 503 `wanted_unavailable` (storage or the 10 s deadline), 500 `wanted_state_invalid` (invalid persisted profile/settings). Bodies are the static `ApiErrorEnvelope`.

## Search admission

`WantedSearchInput`: `request_id` (client UUID), `kind`, `selection`, `indexer_id`/`indexer_revision`, `client_selection` (`default` or `explicit`), `priority`. Mode is always **automatic** user search (best accepted release is grabbed). The existing single search command's provider scope, protocol and key checks apply.

- `{"kind":"ids","ids":[...]}` — explicit targets (deduplicated, 1–1024), searched even if unmonitored, not in the list, or already being searched. Unknown IDs fail the whole request (409 `target_changed`).
- `{"kind":"all","monitored":..., "availability"?: ...}` — every current member under that filter, captured inside the admission transaction oldest-searched-first (the `last_search` ascending order: never searched, then oldest instant, then typed ID), at most 1025 read; more than 1024 is 413 `wanted_search_target_limit` (the whole request is refused, never truncated; the total order makes the bounded read deterministic). Targets with an active (queued/running/retry-wait) search command are skipped and counted (`skipped_active`).

Both selections are admitted in that oldest-searched-first order (explicit IDs are ordered the same way). The receipt lists children in admission order, and each child ID carries its admission ordinal after the request prefix, so the worker's `priority, created_at, id` claim order runs one request's children oldest-searched-first; an exact replay returns the same order. Children admitted before ordered IDs existed (version-1 IDs) are still read back on replay, ordered by target ID.

All children are inserted in one Immediate transaction as ordinary `search_commands` rows (no batch table, no second worker). If the remaining shared active-command pool (1024) cannot hold them all, the request is refused as a whole (429 `command_history_full`). Child command IDs derive from the request UUID, a SHA-256 digest of the canonical body, the target and its admission ordinal: an exact replay returns the original capture (`replayed: true`, counts `null`), a different body under the same UUID is 409 `command_conflict`. The UI resends only the same request after an uncertain response, and only on explicit user action.

TV search-all enqueues one episode search per episode: the existing search command supports a single target only, so season-grouped (pack-aware) search-all is **not** implemented.

## UI

`#/wanted/missing` and `#/wanted/cutoffunmet` with a TV/Movies switch: accessible table (labelled checkboxes, select-all on page, a "Last searched" column rendered as a `<time datetime>` in minute-precision UTC, "Never searched" for none and "(imported)" for a snapshot value), a "Last searched" sort choice in every list, loading/error/empty states, unknown-fact diagnostics, monitoring/sort/direction/page-size (and movie availability) filters, pagination, monitor/unmonitor selected (existing episode/movie editor writes), search selected, search all behind an inline confirmation, receipt summary, per-command status readback (first 25) and explicit same-request retry after an uncertain admission.

## Evidence

- `cargo test --locked --test wanted_api` — `last_search` sort in all four lists (never searched first ascending/last descending, equal-instant ID ties, native vs imported winner and label, unparseable import, colliding episode/movie IDs with different facts, pagination); search-all and explicit admission oldest-searched-first with sorted child IDs, replay order and an availability-filtered capture; ordering under the 1024 bound (1023 admitted: never searched first by ID, then oldest instant); the worker producer against an owned peer answering 404 (caught `refresh_failed`) records both typed targets with fact = `dispatch_completed_at`, while admission alone and a search cancelled before any claim record nothing; plus the earlier coverage: both domains with colliding IDs; monitored partitions (episode vs series); aired-end boundary (one minute either side), future, unknown air date and unknown series runtime; specials in missing, excluded from cutoff; shared multi-episode file rows; grouped-equal, disallowed-lower, above, upgrade-disabled first-allowed and unknown-quality/no-metadata cutoff; movie year/unknown year, orphan exclusion, availability filter and ungated policy; sorts with null-last and tie order; pagination totals; GET leaves `PRAGMA data_version` unchanged; monitoring write moves rows; strict query rejection; selected/all admission, replay/conflict, active skip, typed movie vs episode targets, atomic rejection, 1025-target refusal and pool-capacity refusal.
- `cargo test --locked --test search_commands` — `native_last_search_follows_dispatch_not_admission_rss_decisions_or_grab` (both domains: RSS sync records nothing; admission and claim with the first page held in flight record nothing; completion records fact = `dispatch_completed_at` within the command window; the equal-numbered target in the other domain is untouched; an interactive grab leaves a sentinel fact unchanged; an automatic search whose decisions reject every release still records) and `caught_indexer_failure_records_but_refusal_or_cancel_before_completion_does_not` (both domains: cancel before claim and a stale provider revision issue no request and record nothing; a continuation HTTP 401 records; cancellation while page zero is in flight records nothing; storage triggers refuse a dispatch record on terminal commands or backwards).
- `cargo test --locked --test schema_migrations wanted_last_search_upgrade_preserves_rows_and_imported_facts` — actual version-75 database upgrades to 76 with rows, imported TV text and NULL native facts preserved, CHECK constraints enforced, backup published and reopen a no-op.
- Unit tests: `quality_profiles::threshold_tests`, `wanted::tests` (including `last_search_orders_never_searched_as_oldest_with_typed_ties`), `commands::search::wanted_child_tests` (including ordered child IDs), `frontend/tests/wanted-view.test.mjs`, `frontend/tests/routes.test.mjs`.
- Browser: `frontend/tests/wanted-browser.mjs` (owned `library_ui_fixture`, `/api/fixture/wanted-seed`, which seeds one native TV fact, one imported TV value and one native movie fact in SQL) drives the Last searched column and sort in both domains (ascending/descending order, never searched, imported label, colliding IDs), both domains' missing and cutoff pages, keyboard selection, search selected, confirmed search all (including cancel), active-skip receipt, status readback, availability filter, unmonitor selected, and asserts the durable commands through the search-command API.

## Not claimed

- **Last search**: the producer covers durable search commands only; the stateless `/api/v1/release-search` preview does not record. The episode resource (`/api/v1/episodes`) still shows only the imported value. Radarr snapshot `LastSearchTime` is not imported. Execution order follows admission order only among one request's children (same priority and `created_at`); other queued commands interleave by the shared claim order. The browser check seeds facts in SQL; the producer itself is exercised only by the Rust worker tests, against owned peers.
- **Sort title**: no persisted series/movie sort-title authority; title sorts are case-insensitive literal titles.
- **Grouped TV search-all** (series/season cohorts, season-pack search) and the **eligible tagged indexer union** per execution unit are not implemented; Wanted searches use one explicit indexer and the existing single-target command.
- **No durable batch receipt**: replay is reconstructed from child commands. Deleting a child's terminal history weakens replay; an empty capture is not durable, so replaying it re-evaluates the list.
- **Download-queue exclusion**: search-all skips targets with active search commands only, not targets with an in-flight download.
- **V5 filters** (series IDs, profile IDs, series type, tags, include-specials, quality) and saved custom filters, persisted column/display options, per-row interactive search, detail navigation and manual import from Wanted rows are not implemented.
- Retired/quarantined replacement files are excluded through the shared active-file predicate, but no Wanted test seeds a retired file.
- No V3/V5 wire compatibility, no real indexer/download-client run (the worker is stopped or absent in all Wanted tests), no restart-during-admission test, no load test beyond 1027 episodes.
- The movie missing default follows the reference (no availability gate); the `availability` filter is a native addition.
