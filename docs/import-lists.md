# Import lists

Sonarr's "Import Lists" and Radarr's "Lists", delivered as one domain-aware subsystem: a registry of
external list sources, a per-domain exclusion list, a bounded resumable sync engine and provenance
for every item a list adds. Code lives in `src/import_lists/`; migration file
`0080_import_lists.sql` (see "Migration numbering"). The settings panel is described in
[import-lists-ui section](#ui).

## API

All routes are under `/api/v1/importlist` (generated types: `ImportList*` in
`frontend/src/lib/api.generated.ts`). One resource serves both domains; a list's `media_type` is
`tv` (Sonarr lists) or `movies` (Radarr lists) and cannot change.

| Route | Purpose |
| --- | --- |
| `GET /`, `POST /` | List / create |
| `GET /{id}`, `PUT /{id}`, `DELETE /{id}` | Read / update (revision fenced) / delete |
| `GET /schema` | Implementations with field schemas, domains, default and minimum interval |
| `POST /test`, `POST /{id}/test` | Fetch exactly as a sync does, write nothing; returns `{ok,item_count,error}` |
| `POST /{id}/sync`, `POST /sync` | "Sync now" for one list / every enabled list; `202` with the run(s) |
| `GET /{id}/runs?limit=` | Recent sync runs (at most 32 retained per list) |
| `GET /{id}/items?state=&limit=&offset=` | The list's membership snapshot and what happened to each item |
| `GET/POST /exclusions`, `GET/DELETE /exclusions/{id}`, `DELETE /exclusions/bulk` | Exclusions |

Naming: upstream names the exclusion sub-resource differently per application
(`importlistexclusion` / `exclusions`); hrrdarr uses `/importlist/exclusions` for both domains with a
`media_type` column.

### Definition

A list has an `implementation`, non-secret `settings`, write-only `secrets`, and these defaults
for the items it adds: `quality_profile_id`, `root_folder_id`, `tag_ids`, `monitor`
(`all | none | first_season | last_season`; movies accept `all | none`), `search_on_add`,
`enable_automatic_add`, TV-only `series_type` / `season_folder`, movie-only `minimum_availability`,
plus `enabled` and `interval_minutes` (not below the implementation's minimum, at most 30 days).
Profile, root folder and tags must exist in the list's own domain. Automatic add requires a
profile and a root folder. At most 64 lists, 200 tags per list, unique case-insensitive names per
domain, 64 KiB bodies. Unknown settings, secrets sent as settings, control characters and
loopback-unsafe URLs (link-local/metadata ranges, URL credentials, fragments) are rejected.

Deleting a quality profile or root folder a list uses does not leave a dangling reference: the
list loses that default and automatic add is switched off (the same policy collection defaults
use). Deleting a tag removes it from the default set.

### Secrets

Secret fields (API tokens, client ids) are stored in one AES-256-GCM envelope bound to the list id
and implementation (a distinct AEAD purpose from provider and notification credentials, so an
envelope cannot be replayed across them), using the independently stored `HRRDARR_PROVIDER_KEY`.
Responses list only `secrets_configured` names; on update an omitted secret is kept and `null`
clears it. Without the key, creating a list that has secrets fails with `503`. Secrets are sent only
in request headers; errors, logs, run journals and test results carry stable class codes, never
URLs, tokens or response bodies.

## Synchronisation

A sync is a durable run (`import_list_runs`): `queued -> running -> succeeded | failed`. Scheduled
runs are queued by the background worker (`import_lists::start`, a 5 s tick) when
`last_sync_at + interval` has elapsed (after a failed run a list is retried after at most 30
minutes); "Sync now" queues a manual run and is idempotent while a run is active (the active run is
returned). A disabled list is not scheduled and a manual sync of it is refused (`409`).

A claimed run does this, with no database transaction held across any network call:

1. **Fetch** the complete list through the provider with the shared bounded transport (below). A
   fetch is all-or-failed: a corrupt document, a failed page, or more than 5000 items fails the run
   (`invalid_response`, `resource_limit`, ...) and the previous snapshot is kept. A valid empty list
   is a complete, successful, empty list. Removal from a list never deletes a library item.
2. **Record the snapshot** (`import_list_items`, at most 5000 rows per list) atomically: duplicates
   (by the list's canonical identity) collapse, items that left the list are dropped, finished
   items keep their state, failed items are retried after one hour and unresolved after one day.
3. **Process pending items**, at most 25 per claim, then re-queue the run if more remain. Continuations advance their persisted ready time so other queued lists receive a pass. Per item:
   resolve the library identity (the list's own TVDB/TMDb id, else for movies the IMDb id through
   the metadata catalog, else an *exact* normalised-title match, with the year when both sides
   know it; an ambiguous or missing match is left `unresolved`, never guessed), skip it when it is
   excluded or already in the library (`existing`), otherwise add it.
4. **Finish**: counters on the run, `last_sync_at` / `last_success_at` / `last_error` on the list.

Crash recovery: a claim sets a lease; a `running` run whose lease expired is reclaimed and resumes
from the stored snapshot and item states without refetching (tested with the source taken away). An
add that committed just before a crash may be recorded as `existing` instead of `added` on resume;
the provenance row is written in the add transaction and is never lost. Fetch failures that can
succeed later (`timeout`, `transport`, `server_error`, `rate_limited`) are retried with 1/4/16 min
backoff for three attempts; configuration, authentication, redirect and malformed-document
failures are not retried.

With automatic add off (or the list disabled/unconfigured) the list is still fetched and its
membership recorded, but nothing is added.

### The add path and provenance

Items are added through `library::add_from_catalog`, the same code the manual "add selected
result" endpoint (`POST /api/v1/tv/series/lookup`, `/movies/lookup`) uses: metadata is fetched
first, then one write transaction creates the series/movie, its episodes/facts, settings, tags and
auto-tagging. For a list add the **exclusion check runs inside that transaction** (so an exclusion
added concurrently cannot be raced past) and so does the **provenance row**
(`import_list_additions`: list id and name, run id, external id, whether search-on-add was
requested). Deleting a list keeps the library records and nulls only the link (the name stays).
The folder is `<root>/<Title> (<Year>)` built from the list's own title/year with path separators,
control characters and a leading dot neutralised and bounded to 200 bytes; no directory is created
(library paths are declarations). The TV season monitor mode is applied to the seasons the catalog
reports (specials are never monitored by `first_season` / `last_season`).

Exclusions are keyed by TVDB id (TV) or TMDb id (movies) and are domain-scoped: TV 100 and movie
100 are different rows.

## Providers

Every provider uses `notifications::transport::Transport`: http(s) only, no redirects (a redirect
is the class `redirect`; credentials are never forwarded), no ambient proxy, 10 s request / 3 s
connect / 5 s read timeouts, responses capped at 4 MiB (`resource_limit`), link-local, metadata and
multicast address ranges refused at validation and again for every resolved address (DNS
rebinding), at most 8 concurrent requests, 25 requests per fetch and a 90 s deadline for a whole
fetch. Failure classes: `invalid_settings`, `authentication`, `rate_limited`, `timeout`,
`transport`, `server_error`, `rejected`, `redirect`, `host_denied`, `invalid_response`,
`resource_limit`, `unsupported`.

| Implementation | Domains | Source |
| --- | --- | --- |
| `custom` | TV, movies | Any URL returning a JSON array of objects (`tvdb_id`/`tmdb_id`/`imdb_id`/`title`/`year`, snake_case or camelCase); optional bearer token. A non-array document or a present-but-malformed id fails the fetch. |
| `rss` | TV, movies | RSS 2.0 / Atom feed. Release-style titles (`Show.Name.S01E01…`, `Movie.2020.1080p…`) are parsed to name+year; `Name (2020)` is split; an IMDb id in the link/guid/description becomes the identity. DTDs are refused; size, nesting and node counts are bounded. |
| `trakt` | TV, movies | `popular`, `trending`, `anticipated`, `watched`, user watchlist and user list. `client_id` (header `trakt-api-key`) plus an optional **static** access token (`Authorization: Bearer`). |
| `tmdb` | movies | `popular`, `top_rated`, `upcoming`, `now_playing`, a TMDb list, and discover by person, company or keyword. v4 read token as bearer; at most 500 items (20 per page, 25 requests). |
| `imdb` | movies | The public CSV export of an `ls…` list (bounded RFC 4180 parser; TV rows skipped). Resolved to TMDb through the metadata catalog by IMDb id. |
| `stevenlu` | movies | The popular-movies JSON array (default URL overridable); IMDb ids resolved through the catalog. |
| `plex` | TV, movies | The account watchlist (`X-Plex-Token` header, paged by container start/size); GUIDs `tvdb://`, `tmdb://`, `imdb://`. |
| `mal` | TV | A user's anime list by status (`X-MAL-CLIENT-ID`); title and start year only — MAL ids are not mapped to TVDB ids, so items resolve by exact title. |
| `anilist` | TV | A public user's anime list (GraphQL, no auth); movies/music formats skipped; resolved by exact title. |
| `simkl` | TV, movies | `sync/all-items` by status (`simkl-api-key` + bearer token); TVDB/TMDb ids carried. |
| `sonarr`, `radarr` | TV / movies | Another instance's `GET /api/v3/series` / `movie` with `X-Api-Key`, URL-base preserved; optional quality-profile, tag and root-folder-substring filters (AND across filters, OR within one). The remote row id is never an identity. |

## Migration numbering

The registry is migration **file** `0080_import_lists.sql`. Filename prefixes are historical
names, not execution ordinals. Fresh databases and the current main history through version 78
finish at version **79**, which adds import lists. The earlier published main checkpoint through
versions 50–53 follows the preserved PR #31 history through version 82, then the compatibility
bridge at **83**. Existing PR #31 version-82 databases take that same bridge to the current
separate collection settings and shared provider authority model. Common prefixes through 49
can follow the current main path.

`src/db/mod.rs` and `src/db/legacy_migrations.rs` define the two accepted execution histories.
Every recorded name, SQL body and checksum must match one complete prefix; mixed or tampered
histories are rejected. Historical SQL variants remain under `migrations/pr31/`.

A database created by the unpublished PR #31 draft head `1431d97` has a different ledger from
the published one (from version 50) and from main (from version 63, `notifications`). Startup
rejects it before any statement runs, with a message that says so. Convert it into a new database
with the explicit, offline `hrrdarr convert-draft-database --from <draft database> --to <new
database>`; the source is never modified. See [draft database conversion](draft-database-conversion.md).

A database that already completed the published PR #31 version-82 conversion lost the difference
between an absent and an explicit-false collection "search on add" setting. See
[collection policy recovery](collection-policy-recovery.md) for the explicit, dry-run-first
command that restores it from a matching pre-upgrade backup.

The registry creates `import_lists`, `import_list_tags`, `import_list_exclusions`,
`import_list_runs`, `import_list_items`, `import_list_additions`, and triggers for capacity,
identity, revision, run retention and profile/root deletion.

## UI

The "Import lists" settings panel (`#/settings/lists`, `frontend/src/lib/ImportListsPanel.svelte`,
pure logic in `import-list-draft.ts`) is reachable from the settings index and the "Import lists"
nav button and is mounted lazily.

* Each list row shows name, implementation, a TV/Movies badge, enabled state, "Automatic add" or
  "Record only", interval, tag count and the last sync result as a plain-language message (the
  stable error classes above are mapped to sentences). Buttons: Sync now, Edit, Test, Recent syncs,
  Items, Delete (confirm). "Sync all" queues every enabled list.
* Adding: choose an implementation and a media domain (only domains it supports), then a
  schema-driven form. Secrets are password inputs showing "Configured (leave blank to keep)" with a
  "Clear" checkbox for optional ones; values are never rendered and leave component state after
  save. Defaults: quality profile, root folder, tags, monitor, series type / season folder (TV),
  minimum availability (movies), search on add, automatic add, interval, all scoped to the chosen
  domain. Save is blocked client-side (mirroring the server) when automatic add lacks a profile or
  root folder. Test fetches the unsaved draft and writes nothing.
* Sync status: after "Sync now" the panel polls the runs endpoint sequentially (1 s backing off to
  5 s, at most 600 polls, paused while the tab is hidden, stopped on unmount or a terminal run) and
  shows Queued / Running / Succeeded / Failed with the fetched, added, existing, excluded,
  unresolved, failed and pending counters. "Recent syncs" shows the last 10 runs with their origin;
  "Items" shows the snapshot (state filter, 25 per page).
* Exclusions: domain filter, paging, add form (domain, TVDB/TMDb id, title, optional year), single
  remove and select-many bulk delete, both with confirmation.
* Messages use `role=status` / `role=alert`; no `innerHTML`; no horizontal overflow at 390 px
  (browser-verified).

Verification: `frontend/tests/import-lists.test.mjs` (draft building per domain, secret handling,
error-message map, API URLs/methods/validation, route) and the real-browser script
`frontend/tests/import-lists-browser.mjs` against the owned `library_ui_fixture` (which merges the
import-list router and runs the real scheduler; see [library-ui.md](library-ui.md)). The script
creates profiles/roots/tags through the public APIs, saves a TV Custom list with a bearer token,
tests it, runs "Sync now" to success, checks via the library API that the series exist with the
list's profile and tag and via `/items` that they are `added`, runs a second sync (items stay
`added`, nothing duplicated), excludes a movie and checks a movie list skips it, points the list at
a broken feed (plain-language `invalid_response`, previous state intact), clears a secret, uses
Sync all, adds/removes/bulk-deletes exclusions, checks mobile layout and that deleting a list keeps
what it added. Only the Custom provider is exercised in the browser.

## Not claimed

* **No real third-party service was contacted.** Every provider is tested against an in-process
  fake that implements the request/response shape this code expects from each service's public
  API as understood by the implementer (not captured from the live service). Real-service drift
  (field names, paging, rate limits, anti-bot measures — IMDb in particular may refuse the CSV
  export to non-browser clients) is unobserved.
* **Trakt OAuth is not implemented.** Only a static, user-supplied access token (or none, for
  public lists) is supported; the device-code flow and token refresh are not built, so an expired
  token must be replaced by hand. MyAnimeList / AniList / Plex / Simkl likewise use static
  credentials; no OAuth flow of any provider is exercised.
* **Search-on-add records intent only.** The flag is stored on the list and on the provenance row
  (`search_requested`), and the added item is monitored and therefore appears under Wanted, but no
  search command is admitted: search commands are bound to one explicit indexer and client chosen by
  a person, and there is no "search all indexers" command to reuse. Nothing claims an automatic
  search ran.
* **Not the whole Sonarr/Radarr surface.** Not built: CouchPotato (`lst.009`), RadarrList /
  RadarrList2 curated lists (`lst.011`), the Radarr discovery feed `GET /importlist/movie`
  (`api.050`), per-season monitoring copied from a source Sonarr, "tag existing items", list
  cleanup levels (unmonitor/delete removed items) and any destructive cleanup, snapshot import of
  existing lists/exclusions, a health check for failing lists, and a Sonarr-V3-wire-compatible
  route. Folder names do not use the naming templates.
* **Anime title resolution is exact-title only.** MAL/AniList/Simkl-without-TVDB items that the
  metadata catalog does not match by exact normalised title (and year when known) stay
  `unresolved`; there is no anime-id mapping table. Season-specific anime entries commonly will not
  match.
* **Not part of the shared command pool.** Runs are their own durable journal with their own
  bounds (one active run per list, 32 retained, 5000 snapshot rows), not rows in the shared
  1024-slot command capacity; they do not appear in `/api/v1/commands`. A crash-looping process
  that dies inside the same add repeatedly has no attempt cap on lease reclaim.
* **Metadata outages** during processing mark the affected items `failed` (retried after an hour)
  rather than failing the run; the metadata client's own cool-downs are not re-tested here.
* **Exclusion cleanup interplay:** there is no library delete API yet, so "add to exclusions on
  delete" is not wired; exclusions are managed explicitly.
* Verification observed once: the passing counts quoted in the delivering change are single runs
  on one host; the full-suite run is recorded with the change, not here.
