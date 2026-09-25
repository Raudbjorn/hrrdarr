# File metadata API

Native TV/movie contracts inventoried from Sonarr
`76c684e097f16ac216e6213845e5cac372774995` (`Sonarr.Api.V3/EpisodeFiles/`)
and Radarr `c90668a520664ad0c91812cfee57c41928ad2148`
(`Radarr.Api.V3/MovieFiles/`), their nested controllers/resources and V3 OpenAPI.
Implementation and wire format are independent. V3 compatibility is not claimed.

`{media}` is `tv` or `movies`.

| Method/path | Contract |
| --- | --- |
| GET `/api/v1/{media}/files` | Bounded list using exactly one selector |
| GET `/api/v1/{media}/files/{id}` | File detail; missing ID returns 404 |
| PUT `/api/v1/{media}/files/{id}` | Atomic metadata patch, returns detail |
| PUT `/api/v1/{media}/files/bulk` | `{files:[{id,...patch}]}`, returns ordered array |
| PUT `/api/v1/{media}/files/editor` | `{file_ids:[...],...patch}`, common atomic patch |

TV selectors: `series_id` or comma-separated `file_ids`. Movie selectors:
comma-separated `movie_ids` or `file_ids`. IDs must be positive and distinct;
CSV selectors and bulk/editor requests allow 1–200 IDs. List returns matching
records, so missing selector IDs can yield a subset/empty result. All update
IDs must exist, including mixed-parent batches; a failure leaves every target
unchanged. IDs never select files from another domain.

Pagination is `limit` (default 100, range 1–500) and `offset` (default 0,
nonnegative 32-bit integer). Response: `{items,total,limit,offset}`, ordered by
file ID. Count and page share one read transaction. Requests are limited to
256 KiB; responses have a conservative 8 MiB serialized byte budget including
framing. Oversized results fail explicitly, never truncate. Reduce limit or
batch size; one oversized stored record can still fail.

Example patch:

```json
{
  "quality": {"quality_id": 7, "revision": {"version": 2, "real": 0, "is_repack": true}},
  "languages": [1],
  "release_group": "GROUP",
  "indexer_flags": 4
}
```

Patches accept only `quality`, `languages`, `release_group`, `indexer_flags`,
TV `release_type`, and movie `edition`. Omitted fields preserve values;
explicit null clears them. At least one field is required. `quality` uses
that domain's immutable catalog ID; nullable/omitted revision remains unknown.
A supplied revision needs all three fields: version 1..i32::MAX, real 0..i32::MAX,
and boolean is_repack. Language arrays contain distinct concrete IDs 0..52 (TV)
or 0..57 (movies), at most 64 entries. Empty means known empty; null means unknown.
Policy sentinels Original (-2) / Any (-1) are rejected consistently in these native
patch routes, rather than emulating the differing upstream single/bulk filtering.
Release group/edition allow at most 1024 UTF-8 bytes, without NUL. Indexer flag
masks are 0..511 (TV) / 0..4095 (movies); TV release_type is 0 (unknown), 1 (singleEpisode),
2 (multiEpisode), 3 (seasonPack). Unknown, immutable and wrong-domain fields fail 400.
`scene_name` edits fail `unsupported_field`: valid scene names require the
release parser, so punctuation heuristics are not presented as validation.

Resources expose id, series_id/movie_id, stored path, relative_path when derivable
from the current parent root, quality, languages, size, date_added, release_group,
indexer_flags; TV season_number/release_type; movie edition/original_file_path.
Absent facts remain null. Size is stored bytes, not a live disk measurement.
No path is accessed and metadata writes do not change files, ownership,
monitoring, filenames or associations. Unknown `scene_name`, `custom_formats`, `custom_format_score`, and `quality_cutoff_not_met` are null;
the corresponding parser/policy implementations remain pending. `media_info` is
null when absent, otherwise a validated scan-fact projection as described below.

## Storage and snapshots

Migration 0006 adds one optional metadata row per existing file, with exclusive
TV/movie foreign keys, domain-scoped quality references and deletion restrictions.
Existing files and imported snapshot mappings stay intact. No metadata is invented
at migration time. Future physical-deletion recovery must explicitly remove these
rows and preserve snapshot mapping records to prevent replay resurrection.

Sonarr 233 and Radarr 206/242 readers map optional file size/date/season/original
path/group/flags/type plus serialized quality/revision and language metadata.
DB quality is an integer under `quality`, distinct from the HTTP quality object.
Revision `isRepack` becomes `is_repack`; absent revision remains null. DB language
arrays contain integers; source null members become the source-defined Unknown 0.
Unsupported IDs, sentinel/invalid language sets, or unknown flag/release-type
values stay privately archived and are reported, with the active field null.
Malformed known numeric/date/quality structures reject import atomically. Dates
accept RFC 3339 or naive SQLite timestamps explicitly interpreted as UTC, returning
RFC 3339 with a Z suffix. Size/season must be nonnegative. Original path is at most 4096 bytes and
is retained as text, never accessed. SceneName remains an unsupported private archive field; arbitrary quality/revision
keys and unsupported MediaInfo fields are also private and reported.
No arbitrary JSON source object is exposed as public media info.

Exact re-upload of a schema 5 import validates all prior core fields/mappings,
then inserts the new metadata row and its mapping in the same transaction.
Activation requires re-upload, not startup. Dry run rolls everything back. After
activation, local changes (including clearing values) or a deleted mapped metadata
row conflict; replay cannot overwrite or recreate it. Existing private archive
rows and episode activation markers are retained. Unknown source values never
appear in error/report messages.

Errors: `{error:{code,message}}` with static messages; 400 `invalid_request` or
`unsupported_field`, 404 `file_not_found`, 413 `body_too_large` or
`response_too_large`, 500 `database_error`. Standard router 404/405 behavior applies
to undocumented routes/methods. Success is synchronous 200, not a queued command.

## Media-info facts and projections

The source `MediaInfo` embedded-document contracts come from each pinned
`MediaInfoModel`, audio/subtitle models, `EmbeddedDocumentConverter`,
`STJTimeSpanConverter`, `HdrFormat`, and `MediaInfoResource`. Sonarr 233 uses
`audioStreams`/`subtitleStreams`; Radarr 206/242 uses flattened audio facts and
`audioLanguages`/`subtitles` string arrays. TV primary audio is the first stream.
Source schema revision 14 is current in both pins. Older or absent media-info
revisions permit only these recognized fields; full historical format equivalence
is not claimed. Future revisions above 14 remain archived/reported with public
media info null.

`media_info` exposes nullable resource fields `audio_bitrate`, `audio_channels`,
`audio_codec`, `audio_languages`, `audio_stream_count`, `video_bit_depth`,
`video_bitrate`, `video_codec`, `video_fps`, `video_dynamic_range`,
`video_dynamic_range_type`, `resolution`, `run_time`, `scan_type`, and `subtitles`.
Missing facts stay null; factual zero and known empty arrays remain distinguishable.
Language strings join known entries with `/`. TV language projection remains null
if any stream's language is unknown; typed stream facts preserve that uncertainty.

Bitrates are bits/second. FPS is rounded to three decimals using half-even rounding.
Resolution requires both dimensions. `audio_channels` uses a positive numeric
one-digit `x.y` channel-position prefix (for example `5.1(side)`), falling back
to the discrete channel count when the prefix is absent/unrecognized/zero.
HDR string values or integer enum values use separate domain catalogs: numeric 1
means UnknownHdr (`HDR`) in TV and Pq10 (`PQ`) in movies. Known non-HDR yields
empty dynamic-range labels; unknown enums yield null and an unsupported report.

TimeSpan values use `[days.]hh:mm:ss[.fffffff]`, validated nonnegative and bounded
by signed 64-bit ticks. `runtime_ticks` preserves exact 100 ns units; `run_time`
uses total hours and whole seconds without wrapping at a day. This is a deliberate
native duration convention, not Sonarr's day-wrapping V3 display format.

Additional native facts are `schema_revision`, `container_format`, `width`,
`height`, `runtime_ticks`, raw `video_format`/`video_codec_id`/`video_profile`,
raw primary `audio_format`/`audio_codec_id`/`audio_profile`, `audio_channel_count`,
`audio_channel_positions`, and TV typed `audio_streams`/`subtitle_streams`.
**Formatted `audio_codec` and `video_codec` remain null**: raw IDs are not playback
codec labels, and scene-dependent formatter/parser behavior is not implemented.
These facts are read-only through this API; metadata patches preserve them.

Media-info input and normalized output are limited to 64 KiB; strings to 1024
UTF-8 bytes without control characters; stream/language arrays to 64 entries.
Bitrates fit nonnegative signed 64-bit integers; counts/dimensions/depth/revision
fit nonnegative signed 32-bit integers. FPS must be finite, nonnegative and no
larger than the signed 32-bit maximum. Malformed known fields reject the whole
snapshot. `RawStreamData`, `RawFrameData`, stream titles, unknown nested keys and
unmapped ancillary facts remain in the private raw archive; reports use a static
`MediaInfo.unsupported_fields_or_values` label without their contents. Public
reads deserialize the normalized typed contract and fail closed for unknown stored
fields. No probe process, external URL or media file is accessed.

## Evidence and remaining scope

`tests/media_file_api.rs` uses actual HTTP/local libSQL for both domains, selection,
updates, omission/null handling, movie multi-owner batches, collisions, bounded
inputs/results, rollback, filesystem noninterference and reopen persistence.
`tests/file_snapshots.rs` creates actual schema 5, upgrades, replays both formats,
checks private retention, domain constraints, dry runs, strict conflicts and late
failure rollback. Media-info unit tests cover duration boundaries, channel layout,
null/zero distinctions and domain-specific HDR mapping. HTTP fixtures exercise
both snapshot-to-resource paths and private-field exclusion. Existing
migration/import/episode regressions remain applicable.

Both API ledger rows remain **Partial**. Outstanding controller behavior includes
safe physical single/bulk DELETE with recycle/history/recovery, scene validation,
scene-dependent media-info codec formatting, custom-format scoring/cutoff policy
and realtime events.
No DELETE placeholder reports success. Polling reads are available; live services,
remote storage and filesystem recovery have not been exercised. Full import/manual
library/provider or combined TV/movie slice gates are not established here.
The structural JSON predicate requirement per.006 has no current query consumer:
existing LIKE predicates inspect schema object names only. It remains open; this
work does not add a synthetic query merely to satisfy that requirement.
