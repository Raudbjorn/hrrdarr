# Collections implementation checkpoint

This branch contains prerequisites, not a complete Collections feature. Collection API, refresh jobs, automatic additions/search, provider-authority consumers and UI remain unfinished. Migrations47/48 add persistent state; no dependencies were added.

## Metadata protocol

`MetadataClient::collection(tmdb_id)` requests `movie/collection/{tmdb_id}` from the fixed Radarr metadata service using the existing bounded HTTP client. Owned fixture injection is restricted to explicit `http://127.0.0.1:PORT/`. The client keeps the existing 10-second transport deadline, 1 MiB body cap, cooldown/operation admission, and no redirects/proxies. A 404 is `MetadataError::NotFound`; invalid identity, malformed graph and resource excess are static `InvalidResponse` errors. Response content and image URLs never enter error messages.

The complete response requires `tmdbId`, nonblank `name`, and a non-null `parts` array. An empty array is a complete empty graph; missing parts are not. Maximum 1000 unique positive member TMDB IDs. Collection names are limited to512 UTF-8 bytes, overviews64 KiB, images32, URL text2048 bytes, image kinds64 bytes, and ratings16 sources. Each private image array must also fit65536 bytes of compact JSON encoded as exact `{cover_type,source_url}` pairs, including escaping. Member movie facts reuse existing runtime/language/date/genre/title validation and date-derived availability. Ratings retain source/count/value, including null source entries; legacy rating lists are retained separately. Unknown image kinds remain private facts. Duplicate recognized JSON fields and duplicate rating-source keys are invalid.

`CollectionDetails` contains collection identity/title/overview, optional private images and a complete vector of `CollectionMember` facts. Each member holds existing `MovieDetails`, original title, overview, ratings and images. Missing optional facts stay absent, including missing images versus an explicitly empty image list. The DTOs carrying image source URLs deliberately do not implement Serialize or TypeScript export. Debug output redacts image URLs. Image facts do not authorize network requests; a future artwork consumer must independently admit origins and apply its own bounds.

`movie_with_collection(id)` returns `MovieWithCollection { movie, collection }`. `CollectionAssociation` distinguishes `Absent`, `Clear` (wire null) and `Present(CollectionSummary {tmdb_id,title})`. Zero is invalid, not an invented removal sentinel. A member summary naming another collection is rejected. Existing `movie(id)` keeps its existing `MovieDetails` response and validates supplied collection summaries; existing constructors and struct literals are unchanged. Library add/refresh consumers must explicitly adopt the companion API before collection associations are persisted.

The metadata protocol layer itself implements no collection API router, database writer, snapshot activation, refresh scheduler, movie adoption, exclusions UI, image download or search-on-add; adoption and snapshot consumers are described below. The collection lifecycle contract remains in the ignored iteration87 requirements report. Tests colocated in `metadata/collections.rs` exercise real owned HTTP routing/status/body bounds and pure graph/presence validation; execution belongs to parent verification, not this source handoff.

## Transactional movie adoption

Selected movie adds and durable metadata refresh now fetch the companion API before opening their writer transaction. Association publication occurs inside the same fenced transaction as movie facts; ordinary sparse responses preserve membership. Explicit null clears only membership, followed by last-library-member policy cleanup. Existing movie files, catalog facts and history are never deleted by collection cleanup. Unidentified legacy collections cause a conflict instead of title-based reconciliation.

Movie selected-add accepts optional `monitor` (`movie_only`, default; `movie_and_collection`; `none`) and `collection_expected_revision`. The collection choice requires complete saved root/profile/availability defaults. Existing policies require the exact captured revision and retain their defaults/tags; new discovery is unmonitored with search-on-add false. Root inheritance uses the longest containing movie root, with path-component containment. Ordinary discovery with incomplete defaults remains unmonitored. Existing policy—including empty tags—wins on later discovery.

Explicit enable records retained local intent as well as live settings. Last-member or association cleanup retains a removal intent before deleting collection rows, protecting against snapshot resurrection. Fresh authoritative discovery may restore lifecycle/metadata_missing removal while retaining local_edit against snapshot replay. Actual user-removal tombstones remain protected. There is no new destructive library-delete endpoint; the reusable cleanup helper must be called within a future actual movie-deletion transaction, never for mere file disappearance.

The adoption repository requires an existing transaction and performs no HTTP/filesystem effects. Callers must roll back on every error. Public transport fields cannot enable search-on-add here; no search command is admitted in this prerequisite. Collection refresh/snapshot/UI/search consumers remain separate required work. The new integration source exercises repository transactions/reopen/stale targets and real selected-add HTTP using an owned metadata peer; test execution remains parent-owned.

## Snapshot reconstruction

Collections and import exclusions can be reconstructed from supported Radarr206/242 backups.206 retains inline collection identity/title without inventing missing policy.242 also maps catalog-only membership, facts, movie root/profile/tag references and available defaults. Unresolved references stay inactive with diagnostics. Unsupported source fields remain in private archives. Preview runs the same transaction and rolls back; applying repeats is idempotent. Native collection intent, settings and exclusion removal tombstones take precedence over old snapshots. Import performs no metadata requests, movie additions, searches or media access; source collections are not treated as complete freshly fetched graphs. Sonarr imports remain separate.

## Checkpoint verification

Parent verification passed84 focused Rust tests (41 integration,38 database,5 metadata),69 frontend tests, Svelte checking with zero errors/warnings, frontend build, formatting and generated API drift checks. Two initial test defects were corrected before this successful run.

## Not claimed

The full Rust suite and browser workflows were not rerun for this checkpoint. Collections API, refresh/add/search automation, provider-authority consumers and UI are incomplete; these tests do not establish full Collections parity. New collection/provider tables and snapshot replay markers are introduced by migrations47/48. No new dependencies or live-service access.
