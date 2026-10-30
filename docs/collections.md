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
  A collection's assigned quality profile cannot be deleted (`409 profile_in_use`), even without library members;
  clear or reassign the collection's profile first. Migration 0076 also guards direct database deletion and identity changes.
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
(`metadata_unavailable`). Migration `0066_collections.sql`.

## Not claimed

* `search_on_add` is stored and counted as `searches_pending`; no search is submitted by this feature.
* No artwork/poster storage or delivery, no import exclusions (api.014), no snapshot import of collections,
  no TV analogue, no discovery/"popular" collection lists.
* The command table is not part of the shared 1024-row command pool and is not covered by housekeeping; it prunes
  itself to 100 terminal rows. It does not appear in the generic Activity/command views.
* Added movies use a simple portable folder name (not the naming-template engine), and are added monitored.
* Collection graph parts carry identity only (title/year/IMDb); richer facts arrive when the movie is refreshed.
* Verified against mock metadata and an owned fixture only; no live TMDB/SkyHook access.
