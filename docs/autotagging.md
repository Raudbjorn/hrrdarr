# AutoTagging (api.002)

Per-domain rules that add tags to library items from stored catalog facts. TV and movie rules are separate
(`/api/v1/tv/autotagging`, `/api/v1/movies/autotagging`); a rule can only reference tags of its own domain.

## API

| Route | Behavior |
| --- | --- |
| `GET /api/v1/{tv,movies}/autotagging` | All rules of the domain (at most 256), ordered by name. |
| `POST` | Create. 201 with the stored rule. Duplicate names in a domain are 409. |
| `GET/PUT/DELETE /{id}` | Read, replace the whole rule, delete (204). Another domain's id is 404. |
| `GET /schema` | Implementations and fields available for the domain, for editors. |

A rule is `{name, remove_tags_automatically, tag_ids[1..64], specifications[1..64]}`. A specification is
`{name, negate, required, condition}` where `condition` is a tagged object such as
`{"implementation":"genre","values":["Drama"]}`.

Implementations. Both domains: `genre`, `year`, `runtime`, `root_folder`, `original_language`, `quality_profile`,
`status`, `monitored`. TV only: `network`, `original_country`, `series_type`. Movies only: `keyword`, `studio`,
`minimum_availability`, `collection` (TMDB collection ids). Text matches are case-insensitive; `root_folder`
matches the longest configured root folder that contains the item path.

## Semantics

* A rule matches when every `required` specification matches and, if non-required specifications exist, at
  least one of them matches. A rule needs at least one specification.
* `negate` flips a known fact. An unknown fact (NULL, not yet fetched) never matches, even when negated.
* Matching rules add their tags. When `remove_tags_automatically` is set, a rule that does not match removes its
  own tags, unless another matching rule adds the same tag. Other tags are never touched.
* Application runs inside the writing transaction on library add (manual and metadata lookup), on metadata
  refresh (both commands and direct refresh) and on library updates (`PUT` single, bulk and editor). A request
  that itself assigns tags is respected for that request and not re-tagged.
* Automatic assignments are not recorded as user edits. Tags referenced by a rule (or by a collection) cannot be
  deleted (409 `tag_in_use`); `GET /tags/detail` lists `auto_tagging_ids` and `collection_ids`.

Storage: migration `0065_autotagging_rules.sql` (rules, tag links with restrictive tag FK, ordered
specifications with validated JSON conditions). Facts come from migration 0045 columns and library settings.

## Not claimed

* Certification rules: certification is not stored, so no such implementation exists.
* `collection` conditions only match movies with a stored collection membership; a movie without one has an unknown
  collection fact (so even a negated collection condition does not match it).
* No bulk "re-apply all rules to the existing library" operation: existing items are re-evaluated on their next
  add/refresh/update only. Creating or editing a rule does not retroactively tag or untag anything.
* No snapshot (Sonarr/Radarr backup) import of AutoTagging rules.
* Upstream V3 wire compatibility is not claimed; the schema and field names are hrrdarr's own.
* A rule whose application would push an item past 200 tags makes that write fail (400) instead of truncating.
* Tag application was exercised against mock metadata and an owned fixture only; no live services.
