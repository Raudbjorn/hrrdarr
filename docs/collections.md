# Movie collections (api.036, ui.037, job.012)

Movies only; there is no TV counterpart. Collections are catalog entities (`movie_collections`) whose members
reference catalog metadata (`movie_collection_members`), so a collection can list movies that are not in the
library.

## Adoption

`MovieDetails` carries a tri-state `collection` association from the movie metadata service: absent preserves the
stored membership, explicit `null` clears it, an object sets it. It is persisted by the shared fact writer (library
add via lookup and metadata refresh) in the same transaction. A movie is in at most one collection. Clearing keeps
the collection row, its policy and the movie.

## API (`/api/v1/movies/collections`)

* `GET ?limit&offset&monitored&q` paged list with `movie_count`, `library_movie_count`, `missing_movie_count`
  (members empty); `GET /{id}` includes members with `movie_id` (null when not in the library).
* `PUT /{id}` and `PUT /` (bulk `{ids, patch}`, at most 200, all-or-nothing): `monitored`, `search_on_add`,
  `quality_profile_id`, `root_folder_id`, `minimum_availability`, `tag_ids` (replace), `monitor_movies` (one-shot
  monitoring toggle for library members). Monitoring requires profile, root folder and availability (409
  `collection_policy_incomplete`; the invariant is also enforced by triggers). Profiles/roots/tags must be movie-domain.
* Monitoring a collection immediately adds its known missing members to the library at
  `<root>/<Title (Year)>` with the collection's profile, availability and tags (then AutoTagging). Existing paths,
  nested paths and existing library movies are skipped, never replaced.

## RefreshCollections (job.012)

`POST /api/v1/movies/collections/refresh-commands` (202) queues one command (one active at a time; a duplicate
returns the active one); `GET` list/detail. The worker also schedules it every 24 hours while collections exist.
Each collection graph is fetched (`movie/collection/{tmdbId}`, bounded, 1000 parts) outside any transaction, then
applied atomically: title/overview/last sync, member set replaced (authoritative), and for monitored collections the
missing members are added. A run spans worker slices through a cursor and survives restarts. Per-collection fetch
failures are counted; if nothing succeeded and the service was unavailable the command fails
(`metadata_unavailable`). Migration `0066_collections.sql`; snapshot ownership in `0069_collection_snapshot.sql`.

## Snapshot import (migration 0069)

A Radarr 206 or 242 backup reconstructs collections and `ImportExclusions` (`src/snapshots/collections.rs`).
Version 206 retains inline collection identity/title without inventing policy; 242 also maps catalog-only
membership, facts and the movie root/profile/tag references the backup can resolve. Unresolved references stay
inactive with diagnostics; unsupported fields remain in the private archive. Preview runs the same transaction and
rolls back; applying twice is idempotent (`snapshot_imports.collection_version`). Import performs no metadata
requests, movie additions, searches or media access.

Policy stays where 0066 put it (columns on `movie_collections`). Ownership markers decide who may write it:

* `local_edit` becomes 1 when the API changes any policy field or the tag set, and never returns to 0. A
  collection with `local_edit=1` or `metadata_revision>0` (a natively fetched graph or a native title change) is
  native: a snapshot keeps it and reports "newer native collection retained".
* `snapshot_owned=1` marks policy a snapshot wrote. A newer backup replaces that policy in place (tags included)
  while `local_edit=0`; policy that is neither snapshot-owned nor empty is native and retained.
* `movie_collection_members.origin` is `legacy`, `snapshot`, `discovery` (metadata adoption) or `graph`
  (RefreshCollections). A newer backup replaces `snapshot` memberships it no longer lists and removes the
  snapshot membership of a movie it maps but lists in no collection; every other origin is native and untouched.
  A movie whose collection the backup cannot resolve keeps its memberships.
* `movie_import_exclusions` holds snapshot exclusions (title/year optional). Omitted snapshot-owned rows are
  removed by a newer backup that carries an `ImportExclusions` table; rows with `local_edit=1` are retained.
  Nothing in this branch consumes exclusions yet.

Deleting a quality profile or root folder that a collection references disables that collection's policy
(`monitored=0`, the reference cleared) instead of failing or leaving a dangling monitored policy. Tags used by a
collection remain assignments: deleting one is a conflict, as before.

## Reconciliation with origin/main's collections checkpoint

Main's published migrations 1–53 remain byte-for-byte in the migration registry. Later migrations
adapt that schema to the inline collection policy used here; filename prefixes identify source files,
while `src/db/mod.rs` defines schema versions and execution order.

* Existing collection policy and tags move from `movie_collection_settings` to `movie_collections`
  and its tag table. The prior settings revision is retained as `checkpoint_settings_revision`;
  catalog metadata, membership provenance and ownership markers are preserved.
* `movie_collection_intents` remains authoritative: explicit removals suppress recreation and local
  edits fence snapshot replacement. Native discovery may revive metadata-missing collections
  while retaining the local-edit marker.
* Download-client scope tags move into `download_client_tags`. Indexer scope tags remain in
  `provider_scope_tags`, survive edits that retain their domain, and prevent tag deletion while used.
  `provider_status` and `provider_selection_cursors` remain stored; neither main nor this branch
  consumes those checkpoint tables at runtime. Current backoff and selection behavior uses the
  existing scoped status tables and selection implementation.
* Pending download-client health wake-ups transfer to `download_client_status.next_health_at`.
* Snapshot provider reconciliation retains conflict reporting and exact-match binding.

Upgrade fixtures in `src/db/published_prefix_tests.rs` cover the published schema and populated
state. These tests use isolated databases; they do not establish production upgrade success.

## Not claimed

* `search_on_add` is stored and counted as `searches_pending`; no search is submitted by this feature.
* No artwork/poster delivery (snapshot images are stored privately, never fetched), no import-exclusion
  consumer or API (api.014; snapshot-imported exclusions are stored only), no TV analogue, no
  discovery/"popular" collection lists.
* The command table is not part of the shared 1024-row command pool and is not covered by housekeeping; it prunes
  itself to 100 terminal rows. It does not appear in the generic Activity/command views.
* Added movies use a simple portable folder name (not the naming-template engine), and are added monitored.
* Collection graph parts carry identity only (title/year/IMDb); richer facts arrive when the movie is refreshed.
* Verified against mock metadata and an owned fixture only; no live TMDB/SkyHook access.
