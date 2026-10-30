# Movie collections (api.036, ui.037, job.012)

Movies only; there is no TV counterpart. Collections are catalog entities (`movie_collections`) whose members
reference catalog metadata (`movie_collection_members`), so a collection can list movies that are not in the
library.

## Adoption

`MovieDetails` carries a tri-state `collection` association from the movie metadata service: absent preserves the
stored membership, explicit `null` clears it, an object sets it. It is persisted by the collection adoption repository during library
add via lookup and metadata refresh, in the same transaction. New associations replace identified prior memberships; unidentified legacy memberships are preserved.
Collection adoption retains local removal intent and protects it from stale snapshot replay.

## API (`/api/v1/movies/collections`)

* `GET ?limit&offset&monitored&q` paged list with `movie_count`, `library_movie_count`, `missing_movie_count`
  (members empty); `GET /{id}` includes members with `movie_id` (null when not in the library).
* `PUT /{id}` and `PUT /` (bulk `{ids, patch}`, at most 200, all-or-nothing): `monitored`, `search_on_add`,
  `quality_profile_id`, `root_folder_id`, `minimum_availability`, `tag_ids` (replace), `monitor_movies` (one-shot
  monitoring toggle for library members). Monitoring requires profile, root folder and availability (409
  `collection_policy_incomplete`; the invariant is also enforced by triggers). Profiles/roots/tags must be movie-domain.
  A collection's assigned quality profile cannot be deleted (`409 profile_in_use`), even without library members;
  clear or reassign the collection's profile first. Migration 0077 preserves that guard on the shared collection settings table.
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
(`metadata_unavailable`). Migration `0067_collections.sql`.

## Schema integration

Migrations 0001–0076 retain the PR branch history and checksums. Migration 0077 moves
existing collection policy into `movie_collection_settings`, preserving meaningful
local settings and tags, and adds metadata provenance, retained removal intent,
exclusions and snapshot replay state. Migration 0078 adds provider authority state.
Databases from the divergent main migration history are not an accepted upgrade baseline.

## Snapshot reconstruction

Collections and import exclusions can be reconstructed from supported Radarr 206 and 242 backups. Version 206 retains inline collection identity/title without inventing missing policy. Version 242 also maps catalog-only membership, facts, movie root/profile/tag references and available defaults. Unresolved references stay inactive with diagnostics. Unsupported source fields remain in private archives. Preview runs the same transaction and rolls back; applying repeats is idempotent. Native collection intent, settings and exclusion removal tombstones take precedence over old snapshots. A title that differs from an existing collection with the same TMDB id is retained with a diagnostic rather than failing the import. Snapshot-origin memberships the newer backup no longer lists are replaced; only native associations are protected, and a movie whose collection reference the backup cannot resolve keeps its memberships. Collection settings a snapshot wrote (`snapshot_owned=1`) and nobody edited since (`local_edit=0`) are replaced in place by a newer backup's policy, with the settings revision advanced; discovery-created or locally edited settings are retained. When a backup carries an ImportExclusions table, snapshot-owned exclusions it omits are removed; native exclusion tombstones (`local_edit=1`) are retained. Import performs no metadata requests, movie additions, searches or media access; source collections are not treated as complete freshly fetched graphs. Sonarr imports remain separate.

## Not claimed

* `search_on_add` is stored and counted as `searches_pending`; no search is submitted by this feature.
* No artwork/poster delivery, no import-exclusion editor, no TV analogue, and no discovery/"popular" collection lists.
  Private image facts and supported snapshot exclusions are retained; they do not authorize artwork downloads.
* The command table is not part of the shared 1024-row command pool and is not covered by housekeeping; it prunes
  itself to 100 terminal rows. It does not appear in the generic Activity/command views.
* Added movies use a simple portable folder name (not the naming-template engine), and are added monitored.
* Collection graph parts carry identity only (title/year/IMDb); richer facts arrive when the movie is refreshed.
* Verified against mock metadata and an owned fixture only; no live TMDB/SkyHook access.
