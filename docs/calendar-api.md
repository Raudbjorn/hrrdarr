# Calendar API, iCalendar feed and UI

Native calendar for both domains: TV episode air times and library-movie release dates, with each release type (in cinemas, digital, physical) as its own entry. Implementation: `src/calendar.rs` (HTTP, selection), `src/calendar/ical.rs` (RFC 5545 writer). UI: `frontend/src/lib/CalendarPanel.svelte`, `frontend/src/lib/calendar-view.ts`, route `#/calendar`.

Sonarr/Radarr calendar controllers, feeds and frontends were used as requirements only (iteration60 discovery/schema contract). This is a native contract; no V3/V5 wire compatibility is claimed. No migration: the query reads existing columns (`episodes.air_date`/`air_date_utc`, `series.runtime`/`network`, `movie_metadata.in_cinemas`/`digital_release`/`physical_release`/`studio`/`status`, tags).

## Routes

| Route | Body |
| --- | --- |
| GET `/api/v1/tv/calendar` | `CalendarPage<TvCalendarEntry>` |
| GET `/api/v1/movies/calendar` | `CalendarPage<MovieCalendarEntry>` |
| GET `/feed/v1/calendar/tv.ics` | `text/calendar; charset=utf-8` |
| GET `/feed/v1/calendar/movies.ics` | `text/calendar; charset=utf-8` |

The domain is part of the path and every entry carries a typed `MediaTarget` (`episode`/`movie`), so equal numeric IDs never meet. Stable event identities are `episode-<id>` and `movie-<id>-<in_cinemas|digital|physical>`; they never contain a date, so a changed release date moves the same event. GET performs no writes, provider requests, filesystem checks or searches: one read-only transaction, one captured clock (`evaluated_at`).

## JSON query (strict)

| Key | Values | Default |
| --- | --- | --- |
| `start`, `end` | **required**; `YYYY-MM-DD` or whole-second RFC 3339 instant (percent-encode `+` as `%2B`; an unencoded `+` decodes to a space and is rejected) | — |
| `unmonitored` | `true`, `false` | `false` |
| `tags` | comma-separated positive tag IDs, ≤ 64, no repeats; OR within the path's domain | none |
| `specials` | TV only: `true`, `false` | `true` |
| `release_types` | movies only: comma-separated `in_cinemas`, `digital`, `physical`, no repeats | all three |

Unknown, repeated, empty or domain-inapplicable keys, non-decimal or out-of-range values, fractional-second bounds, years outside 1–9999 and raw queries over 4096 bytes return 400 `invalid_calendar_query`.

**Window.** Half-open in both forms (the references use an inclusive end; half-open is deliberate so adjacent ranges never duplicate an entry). A plain date means UTC midnight for instants and that floating date for all-day entries.

- TV: an episode is included when its UTC air instant `t` satisfies `start ≤ t < end`.
- Movies: an all-day release date `D` is included when `first_date ≤ D < end_date`, where `first_date` is `start`'s civil date and `end_date` is `end`'s civil date if `end` is local midnight, otherwise the following date — both read **in the offset the client wrote**. So a `+05:00` local-day window selects that civil date; release dates never shift with the viewer's zone.
- `start < end` (instants) and `1 ≤ end_date − first_date ≤ 366` dates are required.

The response echoes both forms (`start`/`end` as UTC RFC 3339, `first_date`/`end_date`) and the applied filters.

## Membership

| | TV (episodes) | Movies (library movies only; catalog-only metadata never appears) |
| --- | --- | --- |
| Date | Known `air_date_utc`. Episodes with only a source-local `air_date` inside `[first_date, end_date)` are **not shown** and are counted as `date_only_air_dates`; no midnight is invented. Episodes with neither date are absent. | One entry per selected release type whose date is inside the window. Other release dates of the same movie outside the window are **not** emitted (the reference emits all dates of a selected movie; this strict event window is deliberate). Same-day releases remain separate entries. |
| Monitoring default | Episode **and** series monitored (season flags reported, not filtered). `unmonitored=true` removes the gate. | Movie monitored. |
| `has_file` | Active episode-file association (`media_files::active_file_sql`; journaled retired replacements do not count). | Movie file association. |
| Runtime | Series runtime when > 0 (`runtime_source: series`), else episode runtime when > 0 (`episode`), else unknown. `ends_at` = start + runtime. | `runtime_minutes` as stored. |
| Movie date | — | `date` is the UTC date of the stored release timestamp (existing producer contract; the provider's original offset is not recoverable). `released_at` is that instant. |

Ordering: TV by air instant, then series, season, number, episode ID; movies by date, then release type (in cinemas, digital, physical), then movie ID.

**Stored-date validity.** Both producer encodings are read (`RFC 3339 …Z/±hh:mm` from snapshots, `YYYY-MM-DD HH:MM:SS[.f]` from live metadata). SQL selects with `unixepoch()` and additionally requires the date portion to be a real calendar date (SQLite otherwise normalizes e.g. February 30 to March 2). Every selected value is re-validated strictly in Rust (years 1–9999, ≤ 9 fractional digits). `diagnostics.invalid_dates` counts non-null stored dates under the current filters (any window) that fail these checks; they are never shown or guessed.

## Bounds and errors

| Bound | Behaviour |
| --- | --- |
| > 2000 entries in one JSON response (movie release types count separately; counted on candidate rows) | 413 `calendar_resource_limit` — narrow the window or filters; never truncated |
| > 10 000 events in one feed, feed body > 4 MiB, JSON body > 8 MiB | 413 `calendar_resource_limit` |
| Storage failure or 10 s deadline | 503 `calendar_unavailable` |
| Invalid persisted series type | 500 `calendar_state_invalid` |
| Clock outside supported range | 500 `calendar_clock_error` |

Bodies are the static `ApiErrorEnvelope`.

## iCalendar feed

Query: `past_days`, `future_days` (0–365, defaults 7 and 28; `past_days + future_days + 1 ≤ 366`), plus `unmonitored`, `tags`, `specials` (TV), `release_types` (movies) with the JSON semantics. The window is the UTC dates `[today − past_days, today + future_days]`. Explicit `start`/`end` are not accepted on the feed.

Output (RFC 5545): UTF-8, CRLF line endings, content lines folded at 75 octets (continuation lines begin with one space; UTF-8 sequences are never split), TEXT values escape `\`, `;`, `,` and line breaks (`\n`), other control characters become spaces. Fixed properties only: `VERSION`, `PRODID`, `CALSCALE`, `METHOD:PUBLISH`, `X-WR-CALNAME`; per event `UID` (`<event id>@hrrdarr`), `DTSTAMP` (generation time, UTC), `DTSTART`, `DURATION`/`DTEND`, `SUMMARY`, optional `DESCRIPTION`/`CATEGORIES`, `STATUS`.

- TV: timed `DTSTART:YYYYMMDDTHHMMSSZ` (fractional seconds truncated); `DURATION:PT<n>M` when the runtime above is known and 1–10 080 minutes, otherwise no duration. Summary `Series - SxEE - Title` (`1x02`), or `Series - Title` for daily series. Description = episode overview, category = series network. `STATUS:CONFIRMED` with a file, else `TENTATIVE`.
- Movies: all-day `DTSTART;VALUE=DATE:` with the exclusive next-day `DTEND;VALUE=DATE:`. Summary `Title - In cinemas|Digital release|Physical release`, category = studio, no description (no movie overview is persisted). `TENTATIVE` when the stored status is `tba` or `announced`, otherwise `CONFIRMED` (including unknown status).
- An empty selection is a valid empty `VCALENDAR`.
- **Fail closed:** if `invalid_dates > 0` under the feed's filters the feed returns 500 `calendar_state_invalid` instead of publishing a partial subscription. Responses carry `Cache-Control: private, no-store`.

### Authentication decision

The feed has no separate credential, token or API key and no anonymous bypass. It is merged into the same application router as every `/api/v1` route and therefore passes the deployment host-authority and trusted-forwarding guard applied last by `HostConfig::apply` (see `docs/host-api.md`): a disallowed `Host` gets 403 `host_not_allowed`, exactly like the JSON API. The deployment's `authentication: none` model (bind address, firewall/VPN) is unchanged; a feed link is reachable exactly where the UI is and is not a public link. The ICS body never contains request `Host`/`Forwarded` values. The UI builds subscription links from the browser's current origin plus the fixed relative path and the selected filters. Upstream API-key query parameters were deliberately not reproduced.

## UI

`#/calendar` (workspace nav "Calendar"): TV / Movies / Both scope; Month (Sunday-first grid including adjacent days), Week and Agenda (month list) views; previous / today / next navigation; include-unmonitored, include-specials (TV) and per-release-type (movies) filters sent to the server; legend with text for every color and status (`Downloaded`, `Missing`/`Released, no file`, `On air`, `Not aired yet`, `Not released yet`, `Unmonitored`); loading (`role=status`), per-domain error alerts with retry, empty and diagnostics notes; day details panel; feed links (copy and `webcal:`) for the current filters. TV requests send local-midnight instants with the viewer's offset for each bound (DST-aware), so air times land on the viewer's local day; movie requests send plain dates. Keyboard: roving focus over day buttons — arrows (±1 day / ±1 week), Home/End (week), Page Up/Down (month), Enter/Space opens day details; moving outside the visible range loads the new range. Responses for a superseded range/filter are ignored; the view re-reads every 60 s while visible and idle. The Vite dev server proxies `/feed` as well as `/api`.

## Evidence

- `cargo test --locked --test calendar_api` (real owned HTTP, scratch DBs, colliding episode/movie IDs): TV start-inclusive/end-exclusive boundaries (exact, one second before, fractional `.999`), RFC 3339/naive/offset-stored encodings, local-offset windows, date-only episode diagnostics, impossible (`02-30`) and garbage dates counted invalid, unmonitored episode vs unmonitored series, specials filter, OR tags and a cross-domain tag ID matching nothing, series vs episode runtime source; movie release types as separate entries (three on one day, three on different days), strict event window at both edges, catalog-only exclusion, unmonitored movies, release-type subset and its diagnostics, offset-stored release UTC date, all-day selection under `+05:00` bounds and partial-day windows; domain isolation after deleting an equal-numbered movie; 400 query cases; the 366-date maximum; 2000/2001 TV entries and 667×3 movie entries → 413; GET leaves `PRAGMA data_version` unchanged; feeds: empty calendar, CRLF/no bare LF, every physical line ≤ 75 octets, required properties, escaping of `,`/`;`/`\`/newline in summary/category/description, multibyte folding round-trip, durations, daily summaries, exclusive all-day `DTEND`, `TENTATIVE`/`CONFIRMED`, release-type filter, UID stability across date changes, past/future bounds, fail-closed 500; the real `hrrdarr` binary with `HRRDARR_ALLOWED_HOSTS` serves both feeds and JSON routes for an allowed `Host` and 403s a disallowed one.
- Unit tests: `calendar::tests` (bound parsing, offsets/DST windows, feed window, strict stored instants), `calendar::ical::tests` (golden VCALENDAR, escaping/injection, folding at exactly 75/76 octets, multibyte, empty calendar, duration bound); `frontend/tests/calendar-view.test.mjs` (leap/DST date arithmetic under pinned `TZ`, Sunday-first ranges, local-midnight offsets incl. `+05:30`/`−03:30`, query/feed builders, local-day placement in New York and Tokyo, statuses, grouping, keyboard movement); `frontend/tests/routes.test.mjs`.
- Browser: `frontend/tests/calendar-browser.mjs` (owned `library_ui_fixture`, `/api/fixture/calendar-seed`, browser zone `America/New_York`) reads both domains back through the API, checks the 02:30Z pilot on the local previous day at local time, two same-day movie entries, keyboard roving focus/details/focus return, date-only note, unmonitored toggle, release-type filter and the matching feed link/content, TV feed UIDs/DTSTART/DURATION and the `webcal:` link, scope switches, agenda ordering, week view, a 503 movie read shown as an alert then cleared by retry, strict 400 and a mobile viewport.

- Cross-feature: `frontend/tests/scn007-browser.mjs` (see `docs/activity-ui.md`, scn.007) places API-created TV air times and one movie's cinema/digital/physical dates in the month grid, reads both feeds from the UI links, and shows Wanted bulk unmonitoring reflected in the default Calendar, feeds and the include-unmonitored view.

## Not claimed

- **Indexes and scale:** no date indexes were added (no migration); each request scans the filtered episodes/movies with `unixepoch()`. No query-plan or load evidence beyond 2001 episodes / 2001 movie entries.
- **Timezones:** the server has no named-timezone engine. Feed windows and movie dates are UTC dates; the feed has no `asAllDay`/viewer-zone option, and TV events are timed UTC only. The UI relies on the browser's zone; DST correctness is exercised for New York only in the browser and via `TZ` in unit tests. A zone whose local midnight does not exist sends the first existing local time, which widens the movie overlap rule by one date at an `end` bound (TV/movie requests use different forms, so only TV is affected, and only for diagnostics).
- **Date-only TV episodes** are reported, not rendered as all-day events. Premieres-only, season-premiere/finale icons, cutoff-unmet icons, collapsed multi-episode groups, persisted view/filter preferences, first-day-of-week setting, day and forecast views and saved custom filters are not implemented.
- **Queue/download state and search:** calendar entries carry no queued/downloading status and offer no search action (the discovery notes Sonarr's pinned calendar search callback is empty; no inert button was added).
- **Movie overview** is not persisted, so movie feed events have no description. Movie `status` is often unknown (live metadata leaves it `null`), so most movie events are `CONFIRMED`.
- **Invalid-date detection** relies on SQLite `unixepoch()` plus a date-portion check and strict Rust re-validation of selected rows; out-of-window values SQLite reads but Rust would reject for other reasons (e.g. missing seconds) are not counted. A single invalid stored date blocks the whole feed of that domain until repaired.
- No V3/V5 wire compatibility, no real calendar client subscription test (only the RFC 5545 rules listed above are checked, with a hand-written unfolder — no third-party parser), no live services, no remote/Turso topology test.
- Snapshot-imported movie release dates are not produced by the current Radarr snapshot reader (verified from source: `src/snapshots/readers.rs` maps no release-date columns; its `inCinemas` token is the minimum-availability enum). Movie calendar dates therefore come from live metadata add/refresh only.
