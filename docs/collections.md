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

origin/main (PR #1) landed an independent collections foundation as its migrations 0052/0053. This branch's
0066 model (policy columns, API, UI, RefreshCollections, AutoTagging coupling) is the base; origin's additional
value was ported onto it and the duplicate parts dropped:

* Ported: snapshot import of collections and exclusions with its replacement rules and regression tests
  (`tests/snapshot_collections.rs`, retargeted to the columns above), the `origin` membership provenance, the
  reference-delete fixes for profiles and roots (`tests/collection_reference_deletes.rs`), and the preference for
  fresh native data (`local_edit`, `metadata_revision`) over old backups.
* Dropped as duplicate: origin's `movie_collection_settings`, `movie_collection_intents`, `graph_complete`/member
  revision triggers, companion `movie_with_collection` metadata API and `CollectionAssociation` types (0066 and
  `src/metadata.rs` already carry the association on `MovieDetails`), its transactional adoption repository, the
  selected-add `monitor`/`collection_expected_revision` options, and `collection_schema.rs`/`collection_adoption.rs`
  tests, which assert that schema. Intent tombstones and the retire-empty-collection cleanup are not ported
  because this branch has no collection deletion and keeps collections as catalog entities.
* Provider authority (origin 0053) is not added: `provider_scope_tags` is covered by `download_client_tags`
  (0052), `provider_status` by the durable `indexer_status`/`download_client_status` backoff, and the selection
  cursor by `src/providers/selection.rs` (volatile per-domain cursors; nothing in either tree consumed origin's
  persistent cursor table). Origin's tag-preserving `write_scopes` fix is already
  implemented here (`retained_tags`); `tests/provider_config_api.rs` keeps the retention test for it.
* The snapshot provider reconciliation (PR #19: `provider_conflicts`, `allow_unmapped_providers`, exact-match
  binding) is merged as-is and works with download-client tags.

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
