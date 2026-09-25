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

Bulk removal requires 1–100 unique identities. Every identity is checked before any entry is removed, and all removals share one transaction. Unknown identities return 404 with no changes. An already removed known identity returns 204, including in a mixed bulk request. Repeating a request after an uncertain response is safe: tombstones preserve removal rather than creating another action. There is no implicit “clear all,” no automatic retry of a user write in the UI, and no external client mutation.

Errors use static `ApiErrorEnvelope` codes: malformed requests 400, unknown identities 404, inconsistent retained state 409, oversized response 413, deadline 503, storage fault 500. Storage logs include a static event and error discriminant, not private source text.

## Workspace

The Blocklist view uses these native contracts for media, library-ID and protocol filters, date/title sorting, page navigation and source-record details. Removal requires an explicit confirmation of the selected identities. Selection is limited to the current page. A lost removal response locks further writes until explicit readback; the UI never retries a removal automatically.

`frontend/tests/library-browser.mjs` uses the real snapshot importer and API in an owned fixture. It checks pagination, normalized CSV filters, typed ID collisions and TV episode sets, private-field omission, cancelled confirmation, individual and mixed-domain bulk removal, dropped accepted responses, and successful same-snapshot reimports that retain removals. Existing library media bytes remain unchanged. The parent independently ran the complete browser workflow and reviewed its mobile screenshot.

## Persistence and scope

Migration 0022 adds immutable source provenance with a removal timestamp, active blocklist facts, and relational TV episode membership. Deleting an active entry records its tombstone and removes its episode links atomically. Identical snapshot reupload validates the original facts and does not reactivate removed provenance. A different fingerprint is a different source snapshot and may contain new records; this is not a global release fingerprint or a promise to suppress every later snapshot containing similar text.

Library target deletion removes the whole active entry while preserving its tombstone. Individual TV episode deletion cannot silently shrink an active episode set. Snapshot import is transactional: malformed or conflicting supported facts do not partially activate records. No source database or media file is changed.

`tests/blocklist_api.rs` imports synthetic Sonarr 233, Radarr 206 and Radarr 242 backups through an owned HTTP endpoint. It covers dry runs, source/private-field boundaries, equal TV/movie IDs, TV episode sets, protocol and multi-target validation, stable pagination, sorting, invalid requests, atomic unknown-ID bulk rejection, idempotent individual/bulk deletion, reopen, and accepted identical reuploads that preserve removals. Schema/importer tests cover migration, relational integrity, archive reporting, backfill and target deletion.

No live service, production library, automatic failed-download producer, release rejection/enforcement, download retry, dynamic custom-format calculation, or durable clear-all command is delivered here. Reference indexer/library-title/quality/language sorts and richer resource projections remain incomplete. The native API does not claim V3 wire compatibility or full Blocklist/Slice 2 parity. New state is the three migration-0022 tables and indexes; no dependency is added.
