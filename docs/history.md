# Native import history

`GET /api/v1/history` reads real `file_imported` events written atomically with the initial import's file association. Each `HistoryEvent` contains its operation UUID, a typed episode/movie target, a separately domain-qualified historical file ID, source/destination paths, byte size, SHA256 and UTC import timestamp. File IDs remain historical facts after file-record retirement. There is no live-file join or invented release, quality, language, download or custom-format data.

The event timestamp marks association commit. It does not prove move-source retirement or other cleanup completed; no cleanup status is inferred from this immutable event. Paths describe the committed event and do not grant filesystem authority or assert present file existence.

## Query contract

The response is `ApiPage<HistoryEvent>` with `items`, matching `total`, `limit` and `offset`. Count and items share a read transaction. Results sort by stored import time descending, then operation UUID descending. This makes ties deterministic; offset pagination across separate requests is not a frozen snapshot when new imports arrive.

Supported optional query fields:

- `media_type=tv|movies`, `episode_id`, `movie_id`, `series_id`, and `season`. Season requires series; TV selectors may be combined as intersections. Movie selectors cannot be combined with TV selectors or the opposite media domain. A valid selector with no matches returns an empty page. Series and season filters use the episode's current relationship/numbering because event-time numbering is not stored.
- `from` inclusive and `to` exclusive, as RFC3339 timestamps with explicit timezone. Up to nine fractional digits are accepted; longer fractions are rejected rather than rounded. Inputs normalize to UTC, must remain within four-digit years and require `from < to` when both are provided. Fractional bounds compare correctly against the producer's second-precision timestamps. Encode `+` in timezone offsets as `%2B` in URLs.
- `limit` 1–100, default 50; `offset` 0–10000, default zero. IDs must be positive JSON-safe integers; season may be zero. Larger histories can be narrowed with media/library/date bounds.

Unknown fields, incompatible selectors, malformed dates or invalid bounds return 400 `invalid_history_query`. Serialization above 1 MiB returns 413 `history_response_limit`; the read has a five-second deadline returning 503 `history_timeout`. Storage failures or invalid stored facts return a static 500 error, without logging paths or query values. There are no mutating history routes or implicit retention/pruning policies.

Migration 0017 adds only the global `(imported_at DESC, operation_id DESC)` ordering index. Bound predicates compare normalized values directly to the indexed column. Existing episode/movie indices remain available. Rust DTOs generate the committed TypeScript contract.

## Evidence and remaining scope

`tests/history_api.rs` uses actual owned HTTP imports into a scratch database for both domains, colliding target and file IDs, SHA256/size/path facts, pagination/order, domain/library/season filters, actual immutable timestamp boundaries including offsets/fractions, malformed queries, failed commit exclusion and file-record retirement. Reopening the database preserves the same history. The injected failed-history insert proves the association transaction rolls back without a successful event. Schema tests separately cover timestamp/UUID ties, actual version-16 upgrade preservation/rollback and the libSQL query plan's use of the new index.

The pinned Sonarr and Radarr History controllers also require other event producers, download/event/quality/language filtering, expanded library resources, custom-format/cutoff calculations and mark-failed behavior. Those are still incomplete, as is source-history reconstruction. The native route does not claim V3 wire compatibility, cleanup completion, full History parity or a browser workflow. No live services, production files/databases, remote topology or process-kill recovery were tested. No dependency or history mutation policy was added.
