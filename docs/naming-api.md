# Naming configuration API

Only Sonarr's `76c684e097f16ac216e6213845e5cac372774995`
`Sonarr.Api.V3/Config/NamingConfigController.cs` was opened, for controller/route
shape only (singleton `GET`, `PUT` guarded by FluentValidation, and a query-driven
`GET examples`). Radarr's controller was **not** opened; nothing here claims
Radarr route parity, only that migration 0029 (built independently) provides
the movie columns this API serves. The `NamingConfigResource` schema was read
in both pinned Sonarr/Radarr V3 OpenAPI documents for field names only:
Sonarr's list matches the TV columns in migration 0029. Radarr's list has no
`customColonReplacementFormat` at all, and its `ColonReplacementFormat` string
enum at this pin is `delete`/`dash`/`spaceDash`/`spaceDashSpace`/`smart` —
**no `custom`**. Migration 0029 gives movies the same six-value
`colon_replacement` (including `custom`) as TV; that was the schema owner's
decision, not something re-verified against Radarr's reference here — flagged
to the lead as a question for whoever owns the schema, not changed in this
change.

The token vocabulary, grammar, sanitization rules and every line of
implementation below are an independent, clean-room design (see
[`src/naming/render.rs`](../src/naming/render.rs)) — no upstream filename-
builder or validator code (`_filenameSampleService`/`_filenameValidationService`,
in different files, not opened) was read or translated. V3 wire compatibility
is not claimed. Known shape divergences from what the Sonarr controller shows:

- `PUT` here has no `{id}` path segment (this is a domain-scoped singleton, not
  a `RestController<T>` collection) and uses a body `revision` for optimistic
  concurrency instead of FluentValidation-on-save; upstream's `PUT` is
  `RestPutById` with no revision concept.
- Upstream's `GET examples` takes one whole config object by query and falls
  back to the persisted config only as an all-or-nothing choice (keyed on
  whether the query's `id` is `0`); this API resolves the fallback **per
  field**, per the brief.
- Upstream renders every format's sample at `PUT`-time and rejects the save if
  any sample fails to validate (`ValidateFormatResult`), turning a bad-at-render
  template into a save-time error and a failing *example* into a silent `null`.
  This API validates only template **syntax** at `PUT`-time (per the brief —
  the renderer isn't reachable from real facts yet) and returns a render-time
  **400** from `examples` instead of nulling a failing sample. Concretely,
  `standard_episode_format = "{Episode Title}"` alone is accepted by `PUT`
  here but will fail to *render* for an episode with no title once real facts
  are wired in — worth remembering when this renderer is connected to
  automated imports.

**Wired into the automated owned-download import path, filenames only.**
`src/naming/destination.rs::resolve_owned_destination` is now the single
place that computes the automated-import destination: it is called
pre-transaction from `src/commands/processing.rs` (building the `OwnedImport`
sent to `prepare_owned`) and again, fresh, inside `prepare_owned`'s
transaction (`src/import/owned.rs`) as a preflight re-check that the
destination a stale `Plan` was built from still matches what the current
facts would produce. When `rename_enabled=0` for the target's domain, or the
applicable format field (below) is `NULL`, it returns exactly
`root.join(basename)` -- byte-identical to the pre-wiring behavior; no
default template is ever invented. When enabled and configured, it fetches
the series/episode or movie/quality facts from the database, renders the
applicable **filename** format through the same parser/renderer described
above, takes the extension from the downloaded file's own basename (the
renderer returns a stem only), and caps the stem so `stem.ext` never exceeds
255 bytes (NAME_MAX) even though the renderer's own cap only accounts for the
stem. TV picks `standard_episode_format`, `daily_episode_format` or
`anime_episode_format` by the series' `library_settings.series_type`
(`NULL`/`standard` uses `standard_episode_format`); movies always use
`standard_movie_format`. **The `daily`/`anime` selection was unreachable from
the automated pipeline before iteration49 (`scn.002`, `f7cb8f8`)**:
`src/search/downloaded.rs::evaluate` rejected any target whose
`library_settings.series_type != "standard"` with `numbering_unsupported`
before naming was ever consulted, and its numbering-match logic only
implemented `parser::Numbering::Episodes`. That gap is now closed --
`evaluate` matches real `Daily` (exact air-date, upstream
`FindOneByAirDate` semantics) and `Absolute`/anime (upstream
`GetAnimeEpisodes` scene-then-plain-column semantics) numbering, so a
matched daily/anime episode reaches this module the same way a standard
one does, using its own real `season`/`number`/`air_date` fields --
`resolve_owned_destination`'s `Some("daily")`/`Some("anime")` arms need no
change themselves, since they already loaded and rendered against exactly
this data; they simply couldn't be reached by anything real before.
**Independently re-verified end to end**, in a follow-up to the note this
replaces: `tests/download_processing.rs::daily_series_completed_download_imports_and_renders_http`
and `::anime_series_completed_download_imports_and_renders_http` each drive a
real Add->RSS->grab->completed-download run for a `daily`- and an
`anime`-typed series respectively, over the same real HTTP/producer pipeline
the rest of that file already exercises (no mocked `evaluate` or
`resolve_owned_destination` call). Both assert a real `episodes.episode_file_id`
row association (not merely an accepted decision) and that the configured
`daily_episode_format`/`anime_episode_format` template actually rendered the
on-disk filename (`"Harbor - 2020-01-01 [WEBDL-1080p].mkv"` from a real air
date, `"Harbor - 01 [WEBDL-1080p].mkv"` from a real absolute episode number),
not the raw downloaded basename. Two residual gaps this run does not cover,
left for whoever owns them next: the fixture's `use_scene_numbering` is
`false`, so this does not exercise the scene-absolute-number-vs-plain-column
fallback in `downloaded::match_absolute` (which has its own dedicated unit
coverage in `src/search/downloaded.rs`) or the equivalent fallback this same
session added to `src/search/decision.rs`'s own `Absolute` arm (which has its
own dedicated coverage in `tests/release_decisions.rs`); and
`src/commands/rescan.rs:958` still reports daily/absolute numbering as
unsupported on its own separate call path, unrelated to and not fixed by
either change described here. A render failure (for
example `standard_episode_format = "{Episode Title}"` for an episode with no
title) is a distinct, propagated error -- the automated path blocks the item
with `error_code="unsupported_download"` and reason token
`naming_render_failed` in `reasons_json` (the `error_code` enum from
migration 0026 predates this feature and has no naming-specific member, and
`reasons_json` only accepts `[a-z0-9_]*` tokens up to 128 bytes, so the full
free-text render-failure detail is logged, not stored -- see
`src/commands/processing.rs`) rather than silently falling back to basename
preservation under a name the operator did not ask for. Once a
`Plan` is journaled (`import_journal`), resuming an in-flight operation reads
`plan.destination` verbatim from the journal and never calls the resolver
again, so a mid-flight settings change cannot alter a destination already
committed to disk.

**Still not wired.** Folder-format templates
(`series_folder_format`/`season_folder_format`/`specials_folder_format`/`movie_folder_format`)
are not rendered anywhere; the automated path's destination parent directory
is always the library root, unchanged, because there is no journaled
directory-creation capability yet. Multi-episode rendering
(`multi_episode_style`) is still not consumed. Manual import
(`src/import/mod.rs`'s `preview`/`ManualImportRequest`, the
caller-supplied-destination path) is untouched by this renderer entirely.

## Routes

TV: `/api/v1/tv/config/naming`. Movies: `/api/v1/movies/config/naming`. Each is
a singleton row seeded by migration 0029 (`naming_settings`); there is no
create or delete.

| Method/path | Behavior |
| --- | --- |
| GET `/api/v1/tv/config/naming` | Current TV naming config |
| PUT `/api/v1/tv/config/naming` | Full replacement; requires `revision` |
| GET `/api/v1/tv/config/naming/examples` | Live preview against a fixed example episode |
| GET `/api/v1/movies/config/naming` | Current movie naming config |
| PUT `/api/v1/movies/config/naming` | Full replacement; requires `revision` |
| GET `/api/v1/movies/config/naming/examples` | Live preview against a fixed example movie |

`PUT` is a full replacement, not a patch: every field must be present (formats
may be explicit `null`). It follows the same optimistic-concurrency shape as
`PUT /api/v1/providers/{id}` (`src/providers/mod.rs`): the body's `revision`
must equal the currently stored value or the request fails `409
naming_revision_conflict` without writing anything; a successful write bumps
`revision` by exactly 1 (enforced by the `naming_settings_revision_step`
trigger from migration 0029).

TV fields: `rename_enabled`, `replace_illegal_characters`, `colon_replacement`,
`custom_colon_replacement`, `standard_episode_format`, `daily_episode_format`,
`anime_episode_format`, `series_folder_format`, `season_folder_format`,
`specials_folder_format`, `multi_episode_style`. Movie fields: the same common
four plus `standard_movie_format`, `movie_folder_format`. `colon_replacement`
is one of `delete`, `dash`, `space_dash`, `space_dash_space`, `smart`,
`custom`. `custom_colon_replacement` must be present iff `colon_replacement`
is `custom`, 1–16 bytes, and free of control characters, `/`, `\`, and `:`.
`multi_episode_style` (TV only) is `0`–`5` if present; the migration reserves
the column but no multi-episode rendering exists yet (single episode/movie
only — see Renderer below).

On a fresh install `GET` returns `rename_enabled: false`,
`replace_illegal_characters: true`, `colon_replacement: "smart"`,
`revision: 1`, and every format field `null`, matching migration 0029's seed
rows exactly. No default template is ever invented.

### Examples query semantics

`GET .../examples` accepts one optional query parameter per config field
(`rename_enabled`, `replace_illegal_characters`, `colon_replacement`,
`custom_colon_replacement`, and the domain's format fields). A present
parameter overrides the stored value for this preview only; nothing is
persisted. An absent parameter falls back to the stored config. **A query
string cannot express an explicit override to `null`** — there is no way to
force a field to null through this endpoint; omit the parameter to use
whatever is currently stored. Unknown or wrong-domain query parameters (for
example a movie field on the TV route) fail 400. Overrides are validated with
the same parser/validator as `PUT`, returning the same error codes on failure.
The preview renders even when `rename_enabled` (stored or overridden) is
`false` — that flag only gates whether the *automated* import path would
apply renaming, which this endpoint does not touch. `custom_colon_replacement`
only falls back to the stored value when the **effective** `colon_replacement`
(after applying any override) is `custom`; overriding `colon_replacement` away
from `custom` never inherits a stored custom string it would otherwise fail
`validate_common` against.

Each non-null (after override) format field is rendered against one fixed,
hardcoded example and returned under its own key; a field that resolves to
`null` is returned as `null`, never an invented default. Fixed examples (see
`src/naming/mod.rs::example_episode_facts`/`example_movie_facts`): TV is
series "Halcyon Vale", season 3, episode 7, episode title "The Long Dark",
quality "WEBDL-1080p", air date "2024-05-14"; movie is "The Wandering Harbor"
(2023), edition "Director's Cut", quality "Bluray-1080p".

## Token vocabulary

Templates use `{Token Name}` bracket syntax, a widely observed naming
convention in this space, not upstream parsing logic. The vocabulary is
closed and domain-scoped: an unrecognized token is a hard validation error
naming the offending `{...}` text (`unknown_naming_token`), and a token from
the other domain is a distinct, separately coded error
(`cross_domain_naming_token`). `{Quality Title}` is the only token shared by
both domains.

TV tokens: `{Series Title}`, `{season:00}`, `{episode:00}`, `{Episode Title}`,
`{Air-Date}`, `{Quality Title}`. Movie tokens: `{Movie Title}`,
`{Release Year}`, `{Edition Tags}`, `{Quality Title}`.

`{season:00}`/`{episode:00}` zero-pad the number to the number of `0`
characters after the colon (1–4 digits; anything else, including `0` digits
or a non-`0` character, is `invalid_naming_template`). `{season:0}` pads to 1
digit, `{season:0000}` to 4.

A field also only accepts the tokens that make sense for it
(`naming_token_not_allowed_in_field` for the rest, so e.g. a `{Movie Title}`
mistake in a TV template still surfaces the more specific
`cross_domain_naming_token` first):

| Field | Allowed tokens |
| --- | --- |
| `standard_episode_format`, `daily_episode_format`, `anime_episode_format` | `{Series Title}`, `{season:00}`, `{episode:00}`, `{Episode Title}`, `{Air-Date}`, `{Quality Title}` |
| `series_folder_format` | `{Series Title}` |
| `season_folder_format`, `specials_folder_format` | `{Series Title}`, `{season:00}` |
| `standard_movie_format` | `{Movie Title}`, `{Release Year}`, `{Edition Tags}`, `{Quality Title}` |
| `movie_folder_format` | `{Movie Title}`, `{Release Year}`, `{Edition Tags}` |

Rationale: a series/season folder is not per-episode, so episode/quality
tokens don't belong there; a movie folder is not per-file, so quality doesn't
belong there either. This keeps one series/movie from scattering files across
folders that vary by a per-file fact.

An absent optional fact (for example a missing `{Episode Title}` or
`{Air-Date}`) substitutes an empty string, not an error or a placeholder.

## Grammar and structural validation (parse time, no facts needed)

Both `PUT` and `examples` run every non-null template through the same
parser before touching any fact:

- Template must be non-empty (use `null` to unset) and at most 1024 bytes.
- `{`/`}` must be balanced; unknown/cross-domain/not-allowed tokens are
  rejected as above.
- Literal (non-token) text in the template itself must not contain control
  characters (including NUL) — the operator-authored template is validated up
  front rather than letting a stray control character reach a rendered path
  unchanged.
- `standard_episode_format`, `daily_episode_format`, `anime_episode_format`,
  `standard_movie_format` are **filename** formats: a literal `/` anywhere is
  a hard parse error (`invalid_naming_template`) — these always render to
  exactly one path component.
- `series_folder_format`, `season_folder_format`, `specials_folder_format`,
  `movie_folder_format` are **folder** formats: a literal `/` splits the
  template into multiple path components (nested folders). A leading `/`, a
  trailing `/`, a doubled `//`, or a component whose *literal* text alone
  (after trimming leading/trailing `.`/space) is empty is rejected as
  `invalid_naming_template` before any token is substituted — a component
  built entirely from token output is instead checked at render time (below),
  since its content isn't known until then.

## Renderer and sanitization (render time, `examples` only this iteration)

The renderer (`src/naming/render.rs`) is a pure function — facts in, a
sanitized string out, no I/O — deliberately structured so a later iteration
can call it from the automated import path without change. It only renders a
single episode or a single movie; `multi_episode_style` is not yet consumed.

Per substituted token value, in order:

1. **Colon replacement** always runs first, per `colon_replacement`:
   `delete` removes the colon; `dash` replaces it with `-`; `space_dash` with
   ` -`; `space_dash_space` with ` - `; `custom` with the operator's
   `custom_colon_replacement` text. `smart` deletes the colon if it is
   immediately followed by a space in the source value (the existing space
   already separates the words) and otherwise inserts `-` (so words don't run
   together). Example: `"Alien: Resurrection"` → `smart` → `"Alien
   Resurrection"`; `"A:B"` → `smart` → `"A-B"`.
2. **Illegal characters** — control characters (including NUL), `/`, `\`,
   and `< > " | ? *` — are handled next, governed by
   `replace_illegal_characters`. **This governs every illegal character,
   including the hard path separators `/` and `\`; there is no way to make
   `/`/`\`/NUL pass through unmodified.** If `true`: control characters and
   `< > " | ? *` are deleted; `/` and `\` are replaced with `-` (so a
   fact like an episode title containing `Face/Off` becomes `Face-Off` — it
   stays inside the one component the token occupies, never splitting or
   escaping it). If `false`: encountering any of these characters fails the
   render explicitly with `invalid_naming_examples` — the config opted out of
   silently rewriting values, so a value it can't render safely is refused
   rather than rendered unsafely.
3. Each finished **path component** (all literal text and substituted values
   in one filename, or one folder segment) then has leading/trailing `.` and
   space characters stripped, is truncated to 255 bytes at a UTF-8 character
   boundary (then re-stripped, since truncation can leave a new trailing `.`
   or space), and — if what remains is empty — fails the render with
   `invalid_naming_examples`. This is what catches a component that is
   `..` alone (strips to empty) or entirely a Windows-reserved run of dots.
   **The 255-byte cap applies to the rendered stem only — a filename
   format's output does not include an extension; a future caller appending
   one must budget for it separately.**

Tested hostile inputs (`src/naming/render.rs` unit tests): an episode title of
`Face/Off` (stays in one component; also asserted to fail explicitly when
`replace_illegal_characters=false`), a title containing a literal colon under
every `colon_replacement` value, a title of `..` alone (rejected), a title
with an embedded NUL and control characters (deleted vs. explicit failure per
the flag), and a 500-byte title (truncated to exactly 255 bytes).

## Errors

`{error:{code,message}}`, message is a `String` (not `&'static str` like most
other route modules) because these specific errors legitimately need to name
the offending token or field:

400 `invalid_naming_config` (malformed body/query), `invalid_naming_template`
(structural template problems), `unknown_naming_token`,
`cross_domain_naming_token`, `naming_token_not_allowed_in_field`,
`invalid_naming_examples` (render-time sanitization failure in a preview); 409
`naming_revision_conflict`; 500 `naming_database_error`,
`invalid_stored_naming_settings` (corrupt row — should not occur; both domain
rows are seeded by migration 0029 and never deleted).

## Evidence and remaining scope

`src/naming/render.rs` and `src/naming/mod.rs` carry in-module unit/`#[tokio::test]`
tests covering token substitution for both domains, unknown/cross-domain/wrong-
field/wrong-domain-literal-character token and template rejection (asserting
the specific token text and error code), all listed hostile-sanitization cases
including a multi-byte truncation case, every `colon_replacement` value,
structural grammar rejections (unbalanced braces, empty/oversized templates,
path separator in filename formats, empty/leading/trailing/doubled folder
components), `multi_episode_style` bounds, the fresh-install default snapshot,
PUT round-trip/revision-bump/stale-revision-conflict for both domains called
directly against the handlers, the examples-endpoint colon-mode-override fix,
real query-string parsing via `axum::extract::Query::try_from_uri` (enum/bool
values, cross-domain-field rejection), and that omitting any nullable PUT field
is a hard error rather than a silent null. **No HTTP contract-test file exists
yet** — a separate reviewer is expected to add `tests/naming_api.rs` exercising
the routes end to end over a real listener, the way `tests/provider_config_api.rs`
exercises `src/providers/mod.rs`; that file was intentionally not created here.

`src/naming/destination.rs` carries its own in-module unit tests for the
pure stem/extension-capping logic (`finish_render`): a short stem passed
through unchanged, a 500-byte title capped so `stem.ext` still fits inside
255 bytes, and a template that renders empty surfacing as a `Render` error
rather than a silent fallback. Fetching facts from the database and the
`rename_enabled=0`/unconfigured-format basename-preservation path are
exercised only indirectly today, through whichever integration tests cover
`src/commands/processing.rs` and `src/import/owned.rs`; no dedicated
`resolve_owned_destination` integration test exists yet.

Outstanding: folder-format rendering
(`series_folder_format`/`season_folder_format`/`specials_folder_format`/`movie_folder_format`)
and the directory-creation it would require; multi-episode rendering; any UI.
No live services, network calls, or fixed ports are used anywhere in
this module.
