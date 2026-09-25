# Episode API

These native routes implement episode selection and monitoring using persisted TV
relationships. They do not perform filesystem operations or share numeric targets
with movies. Source requirements: Sonarr commit
`76c684e097f16ac216e6213845e5cac372774995`, `Sonarr.Api.V3/Episodes/EpisodeController.cs`,
`EpisodeControllerWithSignalR.cs`, `EpisodeResource.cs`, and V3 OpenAPI episode
paths. Implementation and wire shapes are independent; V3 compatibility is not claimed.

| Method/path | Behavior |
| --- | --- |
| GET `/api/v1/episodes` | Paged selection; exactly one selector is required |
| GET `/api/v1/episodes/{id}` | Detail with available series, file and image projections |
| PUT `/api/v1/episodes/{id}` | Set only this episode's monitoring flag |
| PUT `/api/v1/episodes/monitor` | Atomically set monitoring for explicit episode IDs |
| GET `/api/v1/series/{id}/episodes` | Existing frontend-compatible array, retaining `season`, `number`, `file_path` |

GET selectors are `series_id` with optional nonnegative `season`, comma-separated
`episode_ids`, or `episode_file_id`. Choose exactly one; ambiguous combinations
are rejected rather than using implicit precedence. For example:

```text
/api/v1/episodes?series_id=7&season=0&limit=50&offset=0
/api/v1/episodes?episode_ids=1,2&include_episode_file=true
/api/v1/episodes?episode_file_id=11&include_series=true&include_images=true
```

List responses are `{items,total,offset,limit}`. Default limit is 100, permitted
range 1–500; offset is a nonnegative 32-bit integer. Ordering is series ID, season,
episode number, then ID. Count and page share one read transaction. An explicit
ID selector accepts 1–200 distinct positive IDs; it returns matching rows, so
missing IDs can produce an empty/subset list. A missing detail ID returns 404.
File selection returns every episode associated with the TV file, including
multi-episode packs; an equal movie-file ID never selects movie data.

Single monitoring body:

```json
{"monitored": false}
```

Bulk monitoring body:

```json
{"episode_ids": [1, 2], "monitored": true}
```

Only those fields are accepted. Lists must contain 1–200 distinct positive IDs.
Every target must exist before writes begin. All monitoring changes and their
response read use one transaction; invalid/missing targets, late database failures,
or oversized results leave the entire batch unchanged. Series and season monitoring,
file associations, metadata and movies are not changed. Successful single updates
return one episode; bulk updates return a deterministically ordered array. Bulk
monitoring accepts the same three `include_*` flags as listing. Success is HTTP 200,
not a deferred command status.

## Resource and source metadata

Each native episode contains:

- Core: `id`, `series_id`, `season`, `number`, `title`, `monitored`.
- File relation: nullable `episode_file_id`, `has_file`, nullable `file_path`.
- Catalog: nullable `tvdb_id`, `air_date`, `air_date_utc`, `last_search_time`,
  `runtime` (minutes), `finale_type`, `overview`.
- Numbering: nullable `absolute_episode_number`, `scene_absolute_episode_number`,
  `scene_episode_number`, `scene_season_number`, `unverified_scene_numbering`.

`has_file` describes a database association, not verified media existence. No media
path is accessed. Absent catalog data stays null on prototype/schema upgrades;
unknown dates, runtime or scene numbering are not given invented values.

List projections default off. Detail enables all three:

- `include_series`: existing series ID/TVDB ID/title/year/path/poster/monitoring.
- `include_episode_file`: existing TV file ID, owning series ID and path; omitted
  when there is no file.
- `include_images`: stored cover array, null if unknown, empty array if known empty.
  It is omitted when not requested. Only coverType/url/remoteUrl are public.

These are honest projections of current data, not synthetic versions of the full
series or file APIs. File quality/media-info/upgrade/custom-format calculations and
richer series resources remain their own contracts. Images and related series
fields are not selected from storage when their projections are not requested.

Sonarr 233 snapshots now map the optional corresponding episode columns. Date-only
values must be exact Gregorian `YYYY-MM-DD`. UTC fields accept RFC3339 instants or
SQLite naive timestamp representations (`YYYY-MM-DD HH:MM:SS[.fraction]` or with
`T`); naive values in these source UTC columns are explicitly interpreted as UTC.
Returned instants are normalized RFC3339 `Z`, while local air-date strings remain
calendar dates. Chrono 0.4.45, already in Cargo.lock, is now a direct dependency
for date validation; no new package was resolved.

TVDB zero means unknown. Runtime/numbering values must be nonnegative; unknown
values are null. Scene flags must be boolean 0/1. Empty source date strings become
null. Finale text is retained (maximum 128 bytes); overview is at most 64 KiB.
Images are at most 32 objects/64 KiB JSON. Supported cover types are unknown, poster,
banner, fanart, screenshot, headshot and clearlogo; source integer 0–6 forms are
normalized to names. Known image URLs must be null or strings of at most 4096 bytes
without control characters. URLs are serialized only, never fetched here. Unknown
cover fields remain in the private raw snapshot archive and are reported as
unsupported; they are not exposed through episode images. Invalid known fields
reject the import without changing destination data.

## Upgrade and replay

Migration 0005 adds nullable episode metadata and a per-snapshot metadata version.
For previously imported schema 4 snapshots, new metadata is **not** activated merely
by startup. Re-uploading the exact application/fingerprint can fill metadata once,
only when its original mapped destination episode still exists, every previously
mapped core field is unchanged and every newly mapped metadata field is null.
A local core or metadata edit conflicts and rolls back the entire import. Successful
fill sets the snapshot marker atomically and reports `metadata_backfilled`; dry
runs and conflicting imports do neither. Once activated, exact replay uses strict
comparison again, so locally clearing a metadata field is not permission to restore
it silently. Sources and their private archives remain unchanged.

## Bounds and errors

All episode request bodies are limited to 16 KiB. Paged/detail/monitor responses have
an 8 MiB conservative byte budget accounting for JSON escaping; oversized responses
return 409 `pagination_required`, never a truncated success. Request a smaller page
or fewer IDs. A single excessively large stored record can still exceed the budget.
The legacy array selects only its original fields, with a 10,000-row cap and the
same byte budget; large libraries must use the paged endpoint. Existing frontend
endpoint/field shapes are retained, without an unbounded response.

Errors use `{error:{code,message}}`, with static messages and no SQL/source values:
400 `invalid_request`, 404 `episode_not_found`, 409 `pagination_required`, 413
`body_too_large`,500 `database_error` or `metadata_invalid`. Standard router
404/405 behavior outside documented methods is unchanged.

## Evidence and limits

`tests/episode_api.rs` exercises real isolated HTTP selectors, includes, ordering,
pagination, file ownership/packs, single/bulk monitoring, failures, movie-ID
isolation and response bounds. `tests/episodes.rs` exercises actual schema 4→5
migration, private optional-metadata import, date/numbering validation, unknown
cover redaction, dry-run/replay/backfill conflict handling and DDL rollback.
Production merged-router construction and existing schema/snapshot tests remain.

No filesystem scan/import/delete/rename, real provider refresh, live media service,
remote database, browser workflow, grabbed transient state or realtime broadcast
is claimed. Polling these resources is supported. The nested RenameEpisodeController
single/bulk preview capability is tracked separately as `api.044`, dependent on
naming policy and file workflows; it has not been omitted from the total parity
scope. This TV-only contract does not establish the combined TV/movie slice gate.
