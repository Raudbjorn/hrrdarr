# Manual library API

Native contracts are inventoried from Sonarr
`76c684e097f16ac216e6213845e5cac372774995` (`Sonarr.Api.V3/Series/`) and Radarr
`c90668a520664ad0c91812cfee57c41928ad2148` (`Radarr.Api.V3/Movies/`), their nested
controllers/resources, monitoring services, statistics repositories and V3 OpenAPI.
Source inspection establishes requirements, not runtime or V3 wire equivalence.
Both full controller rows remain **Partial**.

The TV collection is `/api/v1/tv/series`; the movie collection is `/api/v1/movies`.

| Method/path | Behavior |
| --- | --- |
| GET collection | `{items,total,limit,offset}`, ordered by native ID |
| POST collection | Add a manual library record; HTTP 201 with resource |
| GET collection`/{id}` | Detail; TV includes existing seasons and their statistics |
| PUT collection`/{id}` | Atomic settings/monitoring patch; HTTP 200 with resource |
| PUT collection`/bulk` | `{items:[{id,patch:{...}}]}`, atomic independent patches |
| PUT collection`/editor` | `{ids:[...],patch:{...}}`, atomic common patch |
| GET `/api/v1/series` | Existing frontend array: id/title/year/path/poster |

Lists accept limit 1–500 (default 100), nonnegative 32-bit offset (default 0),
and at most one selector: distinct comma-separated `ids`, TV `tvdb_id`, or movie
`tmdb_id`. Missing selector IDs yield a subset/empty list; a missing detail or
update target is 404. Explicit selectors/batches allow 1–200 distinct positive IDs.
Ambiguous, unknown or wrong-domain query parameters fail 400. Detail, legacy and
write routes accept no query parameters, including move/search effect flags.

A manual TV add:

```json
{
  "title": "Example", "tvdb_id": 123, "year": 2024,
  "path": "/library/tv/Example",
  "settings": {
    "quality_profile_id": 1, "series_type": "standard",
    "seasons": [{"number": 0, "monitored": false}, {"number": 1, "monitored": true}]
  }
}
```

A movie add supplies `tmdb_id`, `title`, optional `year`/`imdb_id`, `path`, and
optional `settings`. Alternatively `{metadata_id,path,settings}` attaches an
existing catalog-only record without changing catalog facts. The two identity
forms cannot be combined. If supplied TMDB facts exactly match an existing catalog
record, that record is reused. Conflicting facts or duplicate library membership
return 409. Catalog-only entries remain outside the movie list until attached.
This endpoint accepts explicit facts; it does not look up or verify external IDs.

Titles require 1–1024 UTF-8 bytes, not whitespace-only, without control characters;
year is null or 1–9999. IDs are positive; IMDb IDs are `tt` followed by digits,
at most 32 bytes. A declared path must be absolute POSIX, at most 4096 bytes,
without traversal, backslashes or control characters. Repeated/trailing slashes
are normalized; `/` is rejected. Textual path equality/ancestor/descendant conflicts
against both domains are rejected, ignoring stored trailing slashes. This does
not resolve symlinks, case aliases, mounts or permissions. No directory is created,
scanned, moved or removed; later physical operations must validate ownership again.

## Settings and monitoring

PUT bodies are settings patches directly; POST nests these fields under `settings`.
Omission preserves a field; explicit null clears nullable settings. Only `monitored`
is nonnullable. Unknown fields, identity/title/path changes and effect options are
rejected, not silently ignored.

- Both domains: `monitored`, nullable `quality_profile_id`. A profile must exist in
  the same domain; the composite foreign key enforces this in storage too.
- TV: nullable `series_type` (`standard`, `daily`, `anime`), `season_folder`,
  `use_scene_numbering`, `monitor_new_items` (`all`, `none`); `seasons` array of
  `{number,monitored}` with at most 1000 distinct nonnegative season numbers.
- Movies: nullable `minimum_availability` (`tba`, `announced`, `in_cinemas`, `released`).
  This stores policy selection; `is_available` remains null until availability
  logic and release-date metadata are implemented.

Creation defaults: monitored true; TV standard type, season folders true, scene
numbering false, monitor-new all; movies minimum availability released. An omitted
profile is unassigned. Explicit null remains unknown for nullable settings. These
are documented native creation defaults, not inferred upgrade/source settings.
Added time records membership creation in UTC. No episode/catalog season records
are fetched automatically; TV creation uses only explicitly supplied seasons.

Patching the series monitoring flag does not flatten season/episode flags. Patching
an **existing changed season flag** updates that season and all its episodes.
Submitting an unchanged season preserves individual episode overrides. Missing
season targets fail the entire request; patches do not add/remove seasons. Movie
monitoring changes only the movie. Bulk/editor failures, including late database
errors and oversized responses, roll back settings, profiles, seasons and episodes
together. Files, catalog facts and unrelated media remain intact. Bulk responses
are ID-ordered arrays; a one-target response includes TV season details.

Typed Rust request/response DTOs live in `src/library.rs`. Patch serialization
preserves native value/null/omission semantics; generated TypeScript remains a
separate required contract task.

## Resources and statistics

Resources expose core identity/title/year/path/monitoring, TV poster/TVDB ID,
movie metadata/TMDB/IMDb IDs, nullable domain settings/profile name/added date,
statistics, and TV seasons on detail. Rich provider catalog fields are not
fabricated. Reads use one database snapshot; aggregate queries are batched across
the selected parents.

TV statistics include specials in totals. `total_episode_count` counts stored
episodes. `episode_count` counts episodes with a file association, plus monitored
episodes whose known UTC air time has passed. Unknown air times do not invent
eligibility. `episode_file_count` counts episode associations: two episodes sharing
one file count twice here. `file_count` counts distinct file records and byte totals
count each file once. `season_count` excludes season 0. Season detail applies the
same episode rules and deduplicates files within that season; a shared cross-season
file can appear in both season subtotals but only once in the series total.

Movie statistics use its actual movie-file relationship; TV-only counts are null.
`known_size_file_count` counts files with stored byte metadata. `size_on_disk` is
null if any associated file has unknown size; no files yields zero. These are
cached facts, not a fresh disk existence/size check. Monitoring never manufactures
file presence. Equal numeric movie/series/episode/file IDs remain domain-scoped.

## Upgrade, snapshot replay and limits

Migration 0007 adds optional typed `library_settings` sidecars. Existing core rows
are unchanged; startup does not invent policy assignments. Sonarr 233 readers map
optional SeriesType, SeasonFolder, UseSceneNumbering, MonitorNewItems and Added.
Radarr 206/242 readers map MinimumAvailability and Added. Known numeric/string enum
values are normalized; unknown enums remain private/reported with active null.
Malformed known booleans/dates fail import atomically. Added dates accept RFC3339
or naive SQLite UTC timestamps and normalize to UTC.

Source profile IDs are **not** destination profile IDs. They remain private and
reported until profile adapters can preserve their identity and semantics; native
profile assignment is supported now. Tags and other unimplemented source settings
likewise remain in the private archive/report. Exact re-upload of a schema 6
snapshot can insert the new sidecar only after its original core mapping reconciles.
After activation, local settings changes (including clears) or deleted mapped
sidecars conflict; replay never overwrites/recreates them. Dry runs and late
failures leave no partial settings or new mappings. Sources remain untouched.

Bodies are limited to 256 KiB. Responses use a conservative 8 MiB JSON budget;
oversized responses fail explicitly. Legacy listing has an additional 10,000-row
cap, and detail has a 1000-season cap. Errors are static `{error:{code,message}}`:
400 `invalid_request`, 404 `library_not_found`, 409 `library_conflict` or
`pagination_required`, 413 `body_too_large`, 500 `database_error`. Standard router
404/405 behavior applies outside documented routes/methods.

`tests/library_api.rs` covers real isolated HTTP/native creates and catalog adoption,
profile isolation, monitoring propagation, statistics, atomic failures, bounds and
reopen. `tests/library_snapshots.rs` creates actual schema 6, verifies DDL rollback,
upgrade/replay/dry runs, edited/deleted mapping conflicts, private/unknown settings,
constraints and late-failure rollback for both applications. Existing older upgrade,
file/episode snapshot and frontend compatibility checks remain applicable.

Not claimed: external lookup/refresh/search-on-add, bulk disk/library discovery,
provider-derived rich catalog data/translations/alternative titles/tags, folder
naming/rename previews, root-policy services, physical move/delete/list exclusions,
availability/scoring policy, realtime events, browser flows or live services.
Nested lookup/import/folder/editor deletion and movie alternatives/rename remain
required work, whether tracked in these rows or separately. This manual foundation
does not complete either full controller or the combined slice 0/1 gates.
