# Native tags and library assignments

The TV and movie catalogs are separate: `/api/v1/tv/tags` and
`/api/v1/movies/tags`. GET lists labels alphabetically; POST `{ "label": "Sci-Fi" }`
creates or returns the existing normalized label. GET/PUT/DELETE `/{id}` reads,
renames, or explicitly deletes it. Labels accept 1–128 ASCII letters, digits and
hyphens, normalized to lowercase. Each domain supports at most 1,024 tags.
Renaming to an occupied label returns 409; deleting an assigned tag returns 409
`tag_in_use`. IDs are positive JavaScript-safe integers and scoped to the route.

GET `/detail` and `/detail/{id}` return `{tag, in_use, owner_count}`. GET
`/{id}/owners?limit=100&offset=0` returns `{ids,total,limit,offset}`, where IDs
identify series or movies according to the route. Limits are 1–200. Only actual
implemented library references contribute to usage today.

Library create `settings` and the existing update, bulk, and editor patches accept:

```json
{"tags":{"mode":"replace","ids":[1,2]}}
```

Modes are `add`, `remove`, and `replace`; omission preserves assignments. Explicit
null is invalid. An empty replacement intentionally clears the set. The same patch
on the existing editor endpoint applies atomically to all selected owners; a bad
owner or foreign-domain tag rolls back the entire batch, including other settings.
Each owner supports at most 200 distinct tags. Library reads expose `tag_ids`.
Manual creation and metadata-selected creation use the same settings writer.
Native creation owns its initial empty set, preventing old archives from assigning
unexpected tags to a subsequently recreated library record.

Migration 0037 adds domain-constrained tag catalogs, separate series/movie joins,
a local assignment edit marker, and an independent snapshot activation marker.
Native tag allocation reserves historical snapshot destination IDs, so deleting
and recreating an equal label cannot impersonate the old imported tag.

Sonarr 233 and Radarr 206/242 snapshots validate and remap `Tags(Id,Label)` and
owner `Tags` JSON arrays. Catalog and owner IDs may collide across applications.
Dry runs use the same transaction and roll it back; late failures roll back tag,
assignment, library, archive and mapping changes together. Exact replay compares
current labels/assignments; local changes produce conflicts instead of overwrite.
Previously archived tags activate only on an explicit repeat upload, while native
assignment edits—including an intentional empty set—block backfill.

Malformed JSON, duplicate identities/references, and references missing from a
present catalog fail validation. Non-text/null/non-array assignments and unknown
catalog fields remain unsupported, privately archived and inactive; they are not
silently converted into empty sets. Missing catalogs with nonempty assignments
are reported unsupported. These limits and interpretations are native contracts,
not a claim of V3 wire compatibility or unlimited upstream capacity.

## Not claimed

No tag management UI ships in this backend unit. Provider, delay/release profile,
list, notification and autotag tag references are not yet implemented; their
source fields remain archived unsupported and do not silently become active.
No external services or real media are used. Tag reconstruction does not complete
collection/list defaults, exclusions, advanced numbering, or the full parity gate.
