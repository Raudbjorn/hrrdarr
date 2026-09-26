# Imported blocklist management

Supported Sonarr and Radarr snapshot Blocklist records become typed, removable local records. This is a real snapshot-to-API management workflow, not an automatic failure producer or release-decision policy. Raw source rows remain private archives.

## API

| Method and path | Contract |
| --- | --- |
| GET `/api/v1/blocklist` | `BlocklistQuery` → `ApiPage<BlocklistEntry>` |
| DELETE `/api/v1/blocklist/{application}/{fingerprint}/{source_id}` | Remove one source identity; 204 |
| DELETE `/api/v1/blocklist/bulk` | `BlocklistRemoval` (`ids` array); atomic removal; 204 |

`BlocklistIdentity` contains the source application (`sonarr` or `radarr`), the 64-character lowercase snapshot SHA-256 fingerprint, and a positive JSON-safe source ID. It is stable across identical reuploads. The identity is neither a library integer nor a native download receipt.

Every response entry has `origin: "source_snapshot"`. Its target is either `{media_type:"tv", series_id, episode_ids:[...]}` or `{media_type:"movies", movie_id}`. TV episodes are a sorted, validated set belonging to that series; an empty set retains the source's series-level record. Movies are never represented as episodes. Current library IDs may collide across domains without selecting the wrong target.

Public facts are `occurred_at`, nullable `published_at`, `source_title`, nullable `protocol`, nullable `size_bytes`, nullable validated `quality`, and nullable `languages`. Timestamps are RFC3339 UTC. Protocol values are `unknown`, `usenet`, and `torrent`; null means the source supplied no protocol. Private Message, Source, Indexer and raw dictionaries are not exposed. Unsupported facts remain archived and are reported; no public values or dynamic custom-format results are invented.

Query filters:

- `media_type`: `tv` or `movies`.
- `series_ids` or `movie_ids`: comma-separated lists of 1–100 distinct positive JSON-safe integers, requiring the matching media domain; the two selectors cannot be combined.
- `protocols`: distinct comma-separated protocol names.
- `sort`: `date` (default) or `source_title`; `sort_direction`: `desc` (default) or `asc`.
- `limit`: 1–100, default 50; `offset`: 0–10000, default zero.

All sorts use source application, fingerprint and descending source ID as stable tie breakers. Count and page share a read transaction. Paging selects compact identities before loading at most 100 full records and their episode sets. Each episode set is bounded to 10000 IDs; the serialized response is capped at 1 MiB and fails rather than truncates. Reads and removals have a five-second deadline. Mutation bodies are capped at 32 KiB. Unknown fields, filters, sort names and malformed identities are rejected.

Bulk removal requires 1–100 unique identities. Every identity is checked before any entry is removed, and all removals share one transaction. Unknown identities return 404 with no changes. An already removed known identity returns 204, including in a mixed bulk request. Repeating a request after an uncertain response is safe: tombstones preserve removal rather than creating another action. Selected removal has no implicit “clear all.” The separate durable domain-clear action below requires explicit domain selection. There is no automatic retry of a user write in the UI or external client mutation.

Errors use static `ApiErrorEnvelope` codes: malformed requests 400, unknown identities 404, inconsistent retained state 409, oversized response 413, deadline 503, storage fault 500. Storage logs include a static event and error discriminant, not private source text.

## Workspace

The Blocklist view uses these native contracts for media, library-ID and protocol filters, date/title sorting, page navigation and source-record details. Removal requires an explicit confirmation of the selected identities. Selection is limited to the current page. A lost removal response locks further writes until explicit readback; the UI never retries a removal automatically.

`frontend/tests/library-browser.mjs` uses the real snapshot importer and API in an owned fixture. It checks pagination, normalized CSV filters, typed ID collisions and TV episode sets, private-field omission, cancelled confirmation, individual and mixed-domain bulk removal, dropped accepted responses, and successful same-snapshot reimports that retain removals. Existing library media bytes remain unchanged. The parent independently ran the complete browser workflow and reviewed its mobile screenshot.

## Durable whole-domain clear

The separate native command consumes the pinned TV/movie ClearBlocklist requirement: purge the chosen application's whole blocklist, with observable completion. In the combined app, the target is explicitly `{media_type:"tv"}` or `{media_type:"movies"}`. It ignores list filters and pagination and evaluates the entire chosen domain when execution begins, including records imported after enqueue. The other domain is untouched.

| Method and path | Contract |
| --- | --- |
| POST `/api/v1/blocklist/clear-commands` | `BlocklistClearInput`: `target` and `priority` (`normal`/`high`); 202 `BlocklistClearCommand` |
| GET `/api/v1/blocklist/clear-commands` | Existing `CommandQuery` domain/status/limit/offset filters; `ApiPage<BlocklistClearCommand>` |
| GET `/api/v1/blocklist/clear-commands/{uuid}` | Retained command |
| POST `/api/v1/blocklist/clear-commands/{uuid}/cancel` | Current command after cancellation attempt |
| DELETE `/api/v1/blocklist/clear-commands/{uuid}` | Terminal command history only; 204 |

One active command per domain is deduplicated. All eight command tables (commands, metadata-refresh, blocklist-clear, RSS, search, manual-import, quality-reset and rescan) share the existing 1024-row capacity pool and one worker, ordered by priority then creation time/UUID; only active (queued, running or retry-waiting) rows count toward that cap, so a command frees its own slot on completion, failure or cancellation without needing deletion. Requests have an 8 KiB body limit and five-second deadline; command pages have the existing 1 MiB response cap, limit 1–100/default 50 and offset 0–1024. A deadline or lost response can leave an uncertain request outcome; read command state before taking another action. `blocklist_clear_timeout` is a static 503 response.

Claiming persists the typed domain and increments the attempt count. A single immediate transaction rechecks running state, deletes all active entries in that domain (which records the existing tombstones and removes episode memberships), and writes command success and `records_removed` together. No entry IDs are loaded into an unbounded application collection, and no transaction spans network or media work. The existing 45-second worker deadline bounds each attempt. Progress is status, attempts and timestamps; `records_removed` is the final successful count, zero before success. An empty domain succeeds with zero removals.

Cancellation that wins before this transaction prevents the purge. If success commits first, cancellation returns the completed command; cancellation does not undo a committed clear. Storage failure rolls back deletion, tombstones and settlement together. Interrupted or failed attempts recover without resetting attempts, with a maximum of three; errors surface as `interrupted` or `storage_error`. Restart cannot separate committed deletion from command success. Reading, cancelling or deleting completed command history never re-executes a clear against later arrivals. This local atomic policy does not establish exactly-once external mutations.

`tests/blocklist_commands.rs` exercises real snapshot-produced records through the HTTP command consumer: execution-time arrivals, domain isolation, shared worker priority, active deduplication, cancellation, queued reopen, a settlement failure that rolls back movie entries/tombstones before retry, empty-domain success, non-resurrection, terminal replay behavior, and shared-cap admission across all three command APIs. A focused private-worker test uses the production claim function, then cancels or closes/reopens before execution in both domains; it verifies exact UUID/domain/attempt retention without fabricating running rows. Migration tests separately cover state constraints and atomic relational rollback. No new scheduler or independent worker is introduced. The browser regression additionally covers confirmation while filtering the other domain, queued cancellation, a lost accepted response with explicit readback, successful TV and movie clears, terminal-history deletion, exact snapshot replay and preserved library media bytes.

## Persistence and scope

Migration 0022 adds immutable source provenance with a removal timestamp, active blocklist facts, and relational TV episode membership. Deleting an active entry records its tombstone and removes its episode links atomically. Identical snapshot reupload validates the original facts and does not reactivate removed provenance. A different fingerprint is a different source snapshot and may contain new records; this is not a global release fingerprint or a promise to suppress every later snapshot containing similar text.

Library target deletion removes the whole active entry while preserving its tombstone. Individual TV episode deletion cannot silently shrink an active episode set. Snapshot import is transactional: malformed or conflicting supported facts do not partially activate records. No source database or media file is changed.

`tests/blocklist_api.rs` imports synthetic Sonarr 233, Radarr 206 and Radarr 242 backups through an owned HTTP endpoint. It covers dry runs, source/private-field boundaries, equal TV/movie IDs, TV episode sets, protocol and multi-target validation, stable pagination, sorting, invalid requests, atomic unknown-ID bulk rejection, idempotent individual/bulk deletion, reopen, and accepted identical reuploads that preserve removals. Schema/importer tests cover migration, relational integrity, archive reporting, backfill and target deletion.

No live service or production library was used. Automatic failed-download production, release rejection/enforcement, download retry and dynamic custom-format calculation remain incomplete. Reference indexer/library-title/quality/language sorts and richer resource projections remain incomplete. The native API does not claim V3 wire compatibility or full Blocklist/Slice 2 parity. New state is the three migration-0022 tables/indexes and bounded migration-0023 clear-command history; no dependency is added.
