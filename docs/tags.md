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

## Browser workflows

Tags settings supports both domain catalogs, normalized create-or-return, rename,
usage counts, paged owner links, and explicit delete confirmation. Assigned tags
remain protected by the real API. The library add form offers optional catalog
selection; Library tags applies add, remove, or replace to up to 25 selected owners
atomically. Empty replacement explicitly clears their tags. Assignment requests
contain only the tag patch, preserving unrelated settings and local settings drafts.
Full returned library records provide assignment readback with current tag labels.

Editors remain mounted across workspace navigation. Within each media domain, same-session catalog,
assignment, and creation writes cannot overlap; independent domains can write
concurrently. Unknown outcomes block further related writes in that domain until
explicit catalog/owner readback. Creation reconciles by its
external metadata identity. Catalog changes invalidate older selections; explicit
reload clears stale tag IDs so a reused native ID cannot silently select a new label.
Domain changes reset assignment action to Add. Reads use epochs and domain checks;
usage pages contain at most 25 links and assignment reconciliation reads at most 25
owners. Loading, error, empty, and retained-draft states use native accessible controls.

Requirements were checked against the pinned Sonarr/Radarr Settings/Tags and
Series/Movie editing surfaces (label selection, usage, and deletion intent); their
implementation was not copied. `frontend/tests/tags-browser.mjs` exercises both
native domain APIs through the browser, including metadata creation, atomic bulk
assignment, remove/empty replace, in-use deletion, and controlled lost responses, 26-owner paging, and late reads across domain switches.
The API/helper tests cover scope, payload projection, label bounds and empty semantics.

## Not claimed

Concurrent external clients have no revision/CAS protection. A full browser reload
can discard local pending/uncertain state; this editor does not introduce durable
mutation receipts. Owner links identify native series/movie IDs rather than
upstream provider references. Provider, release profile,
list, notification and autotag tag references are not yet implemented; their
source fields remain archived unsupported and do not silently become active.
No external services or real media are used. Tag reconstruction does not complete
collection/list defaults, exclusions, advanced numbering, or the full parity gate.

Delay profiles additionally contribute typed `delay_profile_ids` to tag details.
The UI displays those references separately and opens the correct scoped delay
editor. Library owner counts/pages retain their original meaning. Delay mutations
invalidate usage reads; a response captured before a mutation cannot clear that
stale state. Tags referenced only by delay profiles remain protected from deletion.
AutoTagging and the other unsupported reference categories above remain absent.
