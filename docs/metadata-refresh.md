# Durable metadata refresh

This native consumer refreshes the catalog facts currently supported by `MetadataClient` for one existing TV series or movie. It uses the same locally owned worker as download polling, with no second queue runtime. It does not perform a disk scan, import, search, grab, or remote mutation.

## API

All routes below start with `/api/v1/metadata-refresh/commands` and use the generated Rust DTOs.

| Method and suffix | Contract |
| --- | --- |
| POST | `MetadataCommandInput`: typed `target` and `priority` (`normal` or `high`); returns 202 `MetadataCommand` |
| GET | `MetadataCommandQuery`; returns `ApiPage<MetadataCommand>` |
| GET `/{uuid}` | One retained command |
| POST `/{uuid}/cancel` | Cancels queued, waiting or running work; returns current command |
| DELETE `/{uuid}` | Deletes terminal history only; returns 204 |

Targets are `{ "media_type": "tv", "series_id": 1 }` or `{ "media_type": "movies", "movie_id": 1 }`. Their integer IDs may be equal without identifying the same library entity. The server derives `refresh_series` or `refresh_movie` and captures the current external TVDB/TMDB identity. Movie commands also retain the internal metadata membership used to resolve that identity. A client cannot supply another command name or override catalog facts.

List filters are optional `media_type`, `status`, `series_id`, and `movie_id`. A target ID requires its matching media domain; both ID selectors together are invalid. All target IDs must be positive JSON-safe integers. `limit` defaults to 50 and is bounded to 1–100; `offset` defaults to zero and is bounded to 1024. Ordering is creation time descending, then command UUID descending. Count and page share a read transaction. Bodies are bounded to 8 KiB; responses to 1 MiB. Unknown request fields fail validation.

An active command for the same target and captured identity is returned on repeated admission. It is not reprioritized. A different captured identity conflicts until the stale command is terminalized by the worker or explicitly cancelled. Download and metadata commands share a total retained-history cap of 1024. There is no implicit pruning. Explicit terminal-history deletion frees capacity; download refresh APIs retain their existing shapes and show download commands only.

Validation fails with 400; missing command UUIDs with 404; active deletion, changed targets or reconciliation conflicts with 409; full history with 429; missing local ownership with 503; storage faults with 500. Responses use static error codes, never upstream response bodies or transport URLs. Persisted command failures include `metadata_not_found`, `metadata_unavailable`, `metadata_busy`, `metadata_rate_limited`, `invalid_metadata_response`, `target_changed`, `metadata_conflict`, `interrupted`, and `storage_error`.

## Execution and reconciliation

One shared worker chooses across the two concrete command tables by priority descending, then creation time and UUID ascending. Claims increment attempts exactly once. It fetches validated catalog detail through the same metadata client used by lookup, sharing service admission lanes and cooldowns. No database transaction spans HTTP. The worker has a 45-second step deadline; the existing HTTP transport bounds its own operation and response. Cancellation is checked every 250 milliseconds while the read is pending.

The final immediate transaction rechecks running status and the captured identity, reconciles metadata, and marks the command succeeded together. The writer preserves existing local IDs, monitoring, paths, profiles, settings and file associations. It updates supported catalog and episode facts and adds unambiguous new seasons/episodes. Ambiguous renumbering, removed existing episodes or identity conflicts fail the whole reconciliation instead of deleting or reassigning existing library records. Current graph state is read after HTTP, so concurrent local policy edits are preserved.

Any writer or settlement failure rolls back all metadata changes. Deterministic conflicts terminalize visibly; transient service failures retry with exponential backoff and jitter, respecting a supplied Retry-After up to one day. There are at most three attempts. Startup recovers interrupted running work without resetting its target or attempt count. A successful metadata update and success state cannot be separated by a restart; only uncommitted reconciliation can repeat. This policy depends on external reads and a local atomic transaction and must not be applied implicitly to future client submissions or filesystem effects.

Queued or waiting commands whose captured identity is obsolete fail independently of their next retry time. Cancellation observed before final publication prevents metadata changes. If publication commits first, a subsequent cancel returns the already terminal success. Cancel followed by terminal-history deletion is a normal outcome even while the worker is observing cancellation; it does not produce a storage error. `records_updated` counts actually changed or inserted catalog, season and episode rows (0–11001). It is zero for an unchanged successful refresh and is not a progress percentage. Status, attempts, retry time, timestamps and static error codes describe progress.

## Evidence and limits

`tests/metadata_commands.rs` uses owned loopback services and scratch databases. It exercises real selected adds, colliding TV/movie IDs, target-filtered pagination validation, active deduplication, priority across metadata and download work, both-domain successful/no-op refresh, retry after HTTP failure, malformed responses, cancellation and history deletion, identity changes during a blocked read, Retry-After cooldowns with immediate stale-future invalidation, and both-domain close/reopen recovery of actual running commands. A scratch trigger rejects command success after metadata writes to prove that settlement failure rolls back the facts before a retry. Migration and writer tests separately cover admission/state constraints and reconciliation/file-policy preservation.

Parent verification passed 136 distinct Rust tests (plus repeated child-helper executions), 16 frontend tests, generated-contract and Svelte checks, locked compilation, formatting, build and diff checks. The full owned browser workflow passed independently. Review corrected its late-completion assertion to wait for the exact newly accepted command UUID; the fresh rerun passed after that correction.

No live metadata service, production library, remote worker topology, power-loss durability, or external mutating action was tested. This adds the bounded migration-0021 metadata command table without a dependency. Manual single-target refresh is delivered; metadata schedules, full-library refresh, reference refresh/rescan policies, richer movie metadata/credits/artwork, and full upstream job behavior remain incomplete. Sonarr and Radarr source inspection establishes those requirements, not runtime equivalence. Full command and Slice 2 parity remain Partial.
