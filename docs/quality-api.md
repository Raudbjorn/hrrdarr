# Quality definitions API

This native API keeps TV (`tv`) and movie (`movies`) catalogs separate. It does not
claim Sonarr/Radarr V3 wire compatibility. IDs are stable catalog quality identities
within a domain, not upstream database row IDs. For example, ID 20 is TV's
`Bluray-1080p Remux` and movies' `Bluray-480p`.

The base path is `/api/v1/{media}/quality-definitions`, where `{media}` is exactly
`tv` or `movies`. No pagination is needed for the fixed catalog: all 22 TV or 30
movie definitions are returned, sorted by immutable weight then ID. No entries
are hidden or truncated. Unknown quality (ID 0) is included; unused/removed IDs
are not manufactured.

| Method/path suffix | Request | Successful response |
| --- | --- | --- |
| GET (base) | None | All current definitions |
| GET `/{id}` | None | One current definition |
| GET `/limits` | None | `{ "min": 0, "max": 1000, "unit": "MiB/minute" }` for TV; max 2000 for movies |
| GET `/defaults` | None | All immutable catalog defaults; never modifies state |
| PUT `/{id}` | One update object | Updated definition |
| PUT `/bulk` | Array of 1–64 update objects | All current definitions after atomic commit |
| POST `/reset` | `{ "reset_titles": false }` (false is default) | All current definitions after atomic reset |

Every success returns HTTP 200. Updates are synchronous; no command ID or fake
queued status is returned. Default startup/migration seeding does not reset edited
settings on restart. Durable reset command delivery and change broadcasts are
separate work.

Example complete update body (the body ID must match the route ID):

```json
{
  "id": 20,
  "title": "My remux label",
  "min_size": 35,
  "max_size": null,
  "preferred_size": 95
}
```

An update replaces title and all three size settings. An omitted size field means
null, not “keep its previous value.” `title` is required: 1–100 characters,
nonblank, without control characters. Unknown fields are rejected, including any
attempt to supply `quality`, `weight`, `media_type`, catalog/default values or
other immutable metadata. To edit a GET response, select its update fields; do
not PUT the entire GET object.

Sizes are nullable finite numbers, in MiB per minute. Null means an unset bound or
preference; it is not zero. Each supplied TV number must be within 0..1000,
each movie number within 0..2000. All supplied pairs must satisfy
`min_size <= preferred_size <= max_size`; additionally `min_size <= max_size`
even when preferred is null. All-null and equal bounds are supported. This
explicit min/max check is stricter than the pinned nullable-validator gap; inverted
bounds are not a supported behavior. Negative values, NaN, infinity, overflowing
JSON numbers, duplicate batch IDs and invalid/unknown fields are rejected. A
missing target or any database failure rolls back the complete batch.

Definition responses include `id`, `media_type`, immutable `quality`
(`id`, `name`, `source`, `resolution`, and movie-only `modifier`), mutable `title`,
immutable `weight` and nullable `group_name`, plus `min_size`, `max_size` and
`preferred_size`. Catalog/defaults and quality identities are enforced as immutable
by both request shape and database trigger.

## Reset differences

The pinned references differ:

- TV preserves every size setting. `reset_titles=true` restores catalog titles;
  false leaves titles unchanged.
- Movies restore all three default size settings regardless of `reset_titles`.
  Titles reset only when `reset_titles=true`.

Both operations affect only the chosen media domain. The default listing is
available separately for clients to inspect values without applying a reset.

## Errors and limits

These routes use `{ "error": { "code": "...", "message": "..." } }`:

| HTTP | Code | Meaning |
| --- | --- | --- |
| 400 | `invalid_request` | Malformed JSON, wrong shape/type, invalid IDs or update values |
| 404 | `media_type_not_found` | Unknown media domain |
| 404 | `quality_not_found` | No quality for this domain and ID; whole batch rolled back |
| 413 | `body_too_large` | Body exceeds 32 KiB |
| 500 | `database_error` | Database operation failed; no SQL/private values in response |

Standard router 404/405 behavior outside these documented endpoints is unchanged.
Batch limit 64 exceeds either fixed catalog without allowing unbounded writes.
Writes use one immediate transaction and return its committed view. Read endpoints
return a complete bounded catalog. This is last-committed-writer behavior; there is
no ETag or optimistic-concurrency token.

## Provenance and remaining coverage

Catalog facts (names, source/resolution/modifier, identity, order, groups, defaults)
were independently inventoried from `NzbDrone.Core/Qualities/Quality.cs` and size
limits from `QualityDefinitionLimits.cs` at:

- Sonarr `76c684e097f16ac216e6213845e5cac372774995`
- Radarr `c90668a520664ad0c91812cfee57c41928ad2148`

The complete factual catalog is in `migrations/0004_quality_definitions.sql`.
Both `*.Api.V3/Qualities/` controller/resource/validator directories and their V3
OpenAPI `qualitydefinition` list/detail/update/limits paths establish the endpoint
capabilities. `QualityDefinitionService` and the reset command contract establish
the domain-specific reset behavior. Upstream implementation code was not copied
or translated. hrrdarr uses its own scoped relational table, native endpoint
contract and transaction implementation.

`tests/qualities.rs` exercises actual HTTP requests to an isolated loopback Axum
router and local libSQL database: catalog identities/defaults/limits, same-ID domain
isolation, single/bulk writes, bounds/nulls/immutable fields, reset differences,
late-failure rollback, errors, reopening persistence and migration rollback.
The main-handler test also constructs the production merged router to detect route
registration conflicts. Schema migration tests cover fresh and prototype upgrades.
This is implementation evidence, not observed upstream runtime equivalence.

TV definition writes now propagate size settings into persisted profiles in the same
transaction. A definition with **any nonnull** size copies **all three** size fields
(including nulls) to its matching quality item in every TV profile. Direct items,
group children, disabled groups and disallowed items are included. An all-null edit
still updates the definition but leaves profile overrides untouched. Bulk edits
apply this rule independently per definition. TV title resets and all movie
updates/resets do not propagate into profiles. A profile-write failure rolls back
every definition and profile write in the request.

## Persisted profile item/group foundation

The native `/api/v1/{media}/quality-profiles` API supports actual profile editing:

| Method/path | Request | Response |
| --- | --- | --- |
| POST base | Profile input | HTTP 201, complete created profile |
| GET base | `?offset=0&limit=50` | HTTP 200, paged summaries |
| GET `/{id}` | None | HTTP 200, complete profile |
| PUT `/{id}` | Profile input | HTTP 200, complete replaced profile |

Names are unique per media domain, nonblank, 1–100 characters without controls.
Profile inputs contain `name`, `items` and optional `policy`; unknown fields are rejected. Full
responses add `id` and `media_type`. Groups have one level and contain quality
leaves, never nested groups. A quality may appear only once per profile, even
across groups. Catalog identities must exist in the chosen domain. Profiles may
contain a subset of the catalog; omitted qualities are not implicitly created.
A profile and each group must contain at least one quality. At most 64 combined
group/quality nodes are allowed, with the same 32 KiB request limit as definitions.

```json
{
  "name": "TV sizes",
  "items": [
    {
      "kind": "quality", "quality_id": 20, "allowed": true,
      "min_size": 35, "max_size": null, "preferred_size": 95
    },
    {
      "kind": "group", "name": "Web", "allowed": true,
      "items": [
        {
          "quality_id": 3, "allowed": false,
          "min_size": null, "max_size": 130, "preferred_size": 95
        }
      ]
    }
  ]
}
```

Each leaf uses the same domain-specific size validation as definitions; null or
omitted values are unset overrides, not instructions to read a default. Input
array order is preserved for root items/groups and grouped children. Replacing a
profile atomically replaces its name, ordering, groups, allowed flags and sizes;
a failure restores the complete prior profile. Profile reads and list count/page
queries use consistent read transactions during concurrent replacements.

The paged response is `{media_type, items, total, offset, limit}`; each summary has
`id`, `name`, `item_count` (all leaves) and `group_count`. The default limit is 50,
maximum 100, minimum 1; offset is a nonnegative 32-bit integer. One bounded page
query loads summaries without fetching each profile individually. Complete items
come from the detail endpoint. Unknown domain/profile returns 404; unknown quality,
duplicate leaves, malformed pagination or invalid profile data returns 400. The
same JSON error envelope applies, with `profile_not_found`, `profile_name_conflict`
(409), `profile_integrity` (500), or the shared error codes. No source SQL or
private row values are exposed. Conflicting stored root positions fail explicitly
instead of silently hiding a quality item.

Storage uses scoped foreign keys for profiles and quality items and owner-checked
group references. The definition/profile propagation, profile replacement and
catalog migration are transactional. Tests cover multiple direct/grouped/disabled
profiles, nullable single/bulk propagation, all-null exclusion, movie isolation,
late-profile-write rollback, replacement rollback, API round trips and reopened
persistence. The existing quality contract tests continue to cover reset differences.

This supports profile structure and stored policy, not complete quality-profile parity (`api.023`).
Custom-format definitions/relationships and scoring, default profile generation,
profile deletion and search decisions remain separate work. Native library assignments
exist separately; snapshot-to-profile assignments remain unsupported.
No durable `ResetQualityDefinitionsCommand`, SignalR/change events, browser
settings flow, remote database verification, snapshot-to-active-profile mapping,
or release size-decision enforcement is claimed here.

## Explicit stored profile policy

Detail reads return `policy: null` for an unconfigured profile. Migration 19 leaves
all existing profiles unconfigured, retaining items, groups and library assignments
without inventing defaults. A new profile may omit policy or supply null. Once
configured, every PUT must supply a complete nonnull policy alongside the complete
item graph; omitted/null policy is rejected without changing the profile. Clearing
a configured policy is not provided by this contract.

```json
{
  "upgrade_allowed": false,
  "cutoff": { "kind": "group", "position": 1 },
  "min_format_score": 0,
  "cutoff_format_score": 0,
  "min_upgrade_format_score": 1,
  "language_id": null,
  "format_items": []
}
```

The cutoff is either `{ "kind": "quality", "quality_id": 20 }` for an allowed
root quality, or `{ "kind": "group", "position": 1 }` for an allowed group at
that zero-based index in the supplied root `items` array. A grouped child quality
cannot be selected directly. The same validation applies when upgrades are disabled;
toggling `upgrade_allowed` preserves the explicitly supplied cutoff. Group positions
refer to the request, not upstream or database IDs. The transaction allocates groups
and resolves the cutoff to a profile-owned relational foreign key. Replacing the
graph and policy is atomic; an invalid cutoff or late write error preserves both.

All score fields are signed 32-bit integers. `min_upgrade_format_score` must be at
least 1. No score ordering is imposed. With the currently empty custom-format
catalog, `min_format_score` must be at most zero. `format_items` is a closed empty
array (`[]`), not a generic JSON extension point; nonempty arrays are unsupported
and rejected with HTTP 400. No format definitions, condition evaluation or phantom
foreign-key targets are created. A positive cutoff-format threshold can be retained
as configuration, but no current worker evaluates it.

TV policy requires `language_id` absent or null. Movie policy requires an explicit
ID in -2..57: -2 means Original, -1 Any, 0 Unknown, and 1..57 are the pinned concrete
language identities. This differs from file language arrays; a missing movie
language never becomes a guessed language. Unknown policy/cutoff fields, malformed
scalars, overflow and a cutoff outside the supplied allowed roots are rejected.

The fields are stored and editable; they are **not evaluated** by search, upgrade,
import or cutoff-unmet logic in this unit. Existing native subset-catalog profiles
and single-child groups remain supported; the stricter upstream whole-catalog and
multiple-child editor rules are not claimed as V3 equivalence. Source requirements
were checked against both pinned `Profiles/Qualities/QualityProfile.cs`, V3
`Profiles/Quality/QualityCutoffValidator.cs`, and Radarr `Languages/Language.cs`.
`tests/profile_policy.rs` covers both-domain HTTP round trips and rejection/rollback;
`src/db/profile_policy_tests.rs` covers real schema18 upgrade, rollback, constraints,
preservation and reopen using the installed libSQL engine and scratch databases.
No snapshot profile reconstruction or assignment is introduced by migration 19.
