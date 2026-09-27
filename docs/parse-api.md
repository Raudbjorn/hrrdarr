# Parse diagnostic API

`src/parse.rs` is a read-only "why didn't this release title match my series/movie"
diagnostic. It never writes to the database or the filesystem, never grabs or queues
anything, and has no side effects at all.

Only `Sonarr.Api.V3/Parse/ParseController.cs` and `Radarr.Api.V3/Parse/ParseController.cs`
(both under `.do-not-commit/`, git-ignored, not committed) were opened, for route/action
shape only: a single `GET` action per domain that parses a raw title and tries to find a
matching library entry. No upstream parsing, matching or resource-shape code was read or
translated. This endpoint calls the same clean-room primitives the real pipeline already
uses and is already tested against — `crate::search::parser::parse`/`normalize`
(`src/search/parser.rs`) — but its own matching/disambiguation logic and response DTOs
(`TvParseResult`, `MovieParseResult`, `MatchedSeries`, `MatchedEpisode`, `MatchedMovie`) are
an independent design, not a port of upstream's `ParseResource`.

## Routes

| Method/path | Behavior |
| --- | --- |
| GET `/api/v1/tv/parse?title=<string>` | Parses `title` as a TV release and best-effort matches it against the `series`/`episodes` tables |
| GET `/api/v1/movies/parse?title=<string>` | Parses `title` as a movie release and best-effort matches it against the `movies`/`movie_metadata` tables |

Both are pure `GET`s with no request body; nothing is persisted, queued, or sent to any
provider or download client.

## Response shape

```
TvParseResult    { title: string, parsed: ParsedRelease | null, series: MatchedSeries | null, episodes: MatchedEpisode[] }
MovieParseResult { title: string, parsed: ParsedRelease | null, movie: MatchedMovie | null }
MatchedSeries    { id: number, title: string }
MatchedEpisode   { id: number, season: number, number: number }
MatchedMovie     { id: number, title: string, year: number | null }
```

`title` always echoes the input verbatim, even when it fails to parse or match anything —
that is itself the diagnostic signal an operator is looking for, matching upstream's
"still return a response naming just the title" behavior for a title that fails to parse.
`parsed` is `crate::search::parser::ParsedRelease` (see `src/search/parser.rs`) exactly as
the real decision engine and downloaded-file matcher already serialize it (`revision`,
`quality_name`, `edition`, `year`, `numbering`); it is not redefined here. `parsed` is
`null` whenever `crate::search::parser::parse` returns `Err(..)` for any reason — this
includes an oversized (over 1024 bytes) or control-character-laden title, not just an
ordinary "no numbering found" release title (see "Input bounds" below) — that parse-error
detail itself is not surfaced as a separate field or code; the operator sees `parsed: null`
and can judge from the echoed `title` why.

## Matching rules

No alias/alternate-title matching anywhere: hrrdarr has no alternative-title data for
either domain yet (movie alternative titles are `api.049`, Missing), so both routes only
compare `crate::search::parser::normalize(parsed.title)` against the stored primary title
column. This is a deliberate, documented limitation, not an oversight.

**TV** (`series.title`, `episodes.season`/`episodes.number`): every `series` row (bounded
scan, `LIMIT 10001`, matching `search::decision`'s own scan-limit idiom) is normalized and
compared against the parsed title. If exactly one row matches, that series is returned; if
zero or more than one row share the same normalized title, `series` is `null` — this
endpoint never picks an arbitrary candidate among duplicates, the same principle
`search::decision::evaluate` already applies to real grab decisions
(`ambiguous_library_match`). There is no `year`/`tvdb_id` field on a TV release to
disambiguate with (the parser never produces a TV `year`), so an ambiguous TV title match
has no fallback here at all; flagged to the lead as a real gap if duplicate series titles
turn out to matter in practice.

When a series match is found and `parsed.numbering` is `Episodes{season,episodes}` or
`Season{season}`, matching episode rows are resolved by `series_id`/`season`/`number`
(bounded scan, `LIMIT 1001` per series/season). `Episodes` numbering returns only the
requested episode numbers that actually exist; `Season` numbering returns every episode in
that season. **`Daily` and `Absolute` numbering are never resolved to episodes here
(`episodes: []`), by explicit brief scope, not because the schema lacks the columns** —
`search::decision::evaluate` already does resolve both of these for real grab decisions,
joining on `episodes.air_date` (`Daily`) and `episodes.absolute_episode_number` (`Absolute`),
gated on `library_settings.series_type`. This endpoint intentionally does not replicate
that gating/join here; it is a scope limit worth revisiting, not a technical constraint.
This endpoint also always reads `episodes.season`/`episodes.number` directly and never
consults `library_settings.use_scene_numbering`/the `scene_*` columns
`search::decision::evaluate` switches to when scene numbering is active — another
documented simplification versus the real decision engine.

**Movies** (`movies`/`movie_metadata`, joined `d.title,d.year` the same way
`src/naming/destination.rs`'s destination resolver does): every `movies`/`movie_metadata`
row (bounded scan, `LIMIT 10001`) is normalized and compared against the parsed title.
- Zero title matches: `movie` is `null`.
- Exactly one title match: that movie is returned **regardless of year** — a deliberate
  simplification versus `search::decision::evaluate`/`search::downloaded::evaluate`, both of
  which require `parsed.year` to agree with `d.year` **or** `d.secondary_year` even for a
  single candidate. This diagnostic endpoint does not consult `secondary_year` at all and
  does not re-check year on an already-unique title match; flagged to the lead as a
  documented divergence, not an oversight.
- More than one title match (e.g. two movies that share a normalized title with different
  years): `parsed.year` (always present on a successful movie parse — `parser::parse`
  returns `Err("missing_movie_year")` otherwise) disambiguates **only if exactly one**
  remaining candidate has that year. If none or more than one still match after filtering
  by year, the match stays ambiguous and `movie` is `null` — never an arbitrary pick among
  the duplicates.

## Errors

`{error:{code,message}}`, same envelope as `qualities.rs`/`library.rs`
(`crate::api::ApiErrorEnvelope`, `code: &'static str`).

400 `invalid_request` — `title` query parameter is missing or empty. This is the **only**
route-level validation; it exists specifically so an empty/absent `title` gets a normal 400
instead of upstream's own "return null" (an empty `200` body) behavior, which does not fit
this codebase's error-handling convention. Every other input, however hostile — a
1024+-byte title, embedded control characters, a title with no numbering at all — is a
**200** with `parsed: null` (see "Input bounds" below), because a title that fails to parse
is itself diagnostic information the operator is looking for, not an API error. 500
`database_error` (storage failure, no partial state possible since nothing is written),
`library_match_limit` (a normalized-title/episode scan exceeded its bounded-scan cap; see
"Not claimed").

## Input bounds

`crate::search::parser::parse` already rejects an empty, over-1024-byte, or
control-character-containing title internally (`Err("invalid_release_title")`), cheaply —
the length check runs before any tokenizing, so an oversized title never reaches the O(n)
token-splitting logic. This route does not duplicate that check; it only adds the one 400
case `parse()` itself cannot express through its `Result` (empty is diagnosed as `Err`
inside `parse()`, but the brief for this endpoint requires empty specifically to be an HTTP
400, not a `200` with `parsed: null`). No additional query-string length cap is imposed at
the route layer beyond whatever the HTTP server already bounds a request URI to.

## Evidence

`src/parse.rs` carries in-module `#[tokio::test]`s directly against the handlers (both
domains: success-with-match including episode resolution, success-with-no-match, ambiguous
duplicate-title series/movie resolving to no match, `Daily`/`Absolute` numbering never
resolving episodes, parse-failure returns title-only, empty-title 400, missing-title-param
400, movie year disambiguation across three cases — resolved-by-year, wrong-year-stays-null,
same-title-same-year-stays-ambiguous — and an oversized/control-character hostile title
parsing cleanly to `parsed: null`). `tests/parse_api.rs` exercises the same core scenarios
(match with episode resolution, no-match/parse-failure, movie year disambiguation, empty
title on both routes including a request with the parameter omitted entirely, an oversized
title) over a real `axum::serve` listener via `reqwest`, the same pattern
`tests/remote_paths.rs`/`tests/naming_api.rs` use.

## Not claimed

- No alias/alternate-title matching for either domain (see "Matching rules" above); a
  release whose title only matches a stored alternate title, not the primary one, reports
  no match here even though the real decision engine (which does consult
  `movie_alternative_titles`) might accept it.
- Ambiguous TV series titles have no disambiguation path at all (no TV-side equivalent of
  the movie `year` filter); this is a real gap versus the movie route, not by design so much
  as by the data available, and is called out to the lead above.
- Movie matching does not consult `secondary_year`, and does not re-check year on an
  already-unique title match, both unlike `search::decision`/`search::downloaded`; a movie
  could be reported as matched here even though the real decision engine would reject it on
  year, or vice versa.
- `Daily`/`Absolute` TV numbering never resolves to episode rows, unlike the real decision
  engine; `use_scene_numbering` is never consulted (always reads plain `season`/`number`).
- Bounded scans (`LIMIT 10001` for series/movies, `LIMIT 1001` for one series/season's
  episodes) return `library_match_limit` past their cap rather than silently truncating;
  this has no test because it requires seeding thousands of rows and is not expected to
  occur in a real single-instance library.
- No custom-format fields anywhere in the response; hrrdarr has no custom-formats
  implementation yet (`api.008`, Missing).
- No path-vs-title-vs-season-title parsing mode distinction upstream's `ParseController`
  offers — every input is parsed as one release title string through the same
  `parser::parse` call the rest of the pipeline uses, a deliberate simplification for a
  single diagnostic endpoint.
- This endpoint is not wired into any UI; it is an HTTP contract only.

`ParsedRelease.revision` is a structured `FileRevision` (`version`, `real`, `is_repack`) or null for a decoded historical lossy numeric revision; `revision_marker` records explicit recognition. Fresh recognized release titles without markers have baseline version1/real0/non-repack. The bounded grammar handles PROPER, REPACK/RERIP numbered forms, repeated REAL and TV version suffixes. Revision-like text before identity or in a hyphenated release group is not revision evidence. Ambiguous trailing bracketed revision-like groups are rejected explicitly; exhaustive upstream parser equivalence is not claimed. File-import provenance applies stricter rules to unmarked filenames than title inspection.
