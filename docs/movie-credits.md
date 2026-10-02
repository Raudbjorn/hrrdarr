# Movie credits (ledger api.037, slice 5, movie-only)

Native, read-only credits for movie catalog metadata. This is a minimal vertical slice; it does not claim Radarr V3 wire compatibility, and the ledger row stays below Verified.

## Storage

Migration `0047_movie_credits.sql` adds `movie_credits`:

- `metadata_id` references `movie_metadata(id)` with `ON DELETE CASCADE`. Credits belong to catalog metadata, so catalog-only entries without library membership can have credits. No TV table is referenced, and the only routes are under `/api/v1/movies`, so a series, season or episode integer can never select a credit.
- `UNIQUE(metadata_id, credit_tmdb_id)`; `credit_tmdb_id` is the provider's string credit id (`[A-Za-z0-9_-]`, 1 to 64 bytes).
- Bounds are enforced twice, by the metadata validator and by CHECK constraints and a trigger: at most 500 credits per movie, name 512 bytes, department/job 256, character 1024, order 0 to 100000, person id 1 to 2147483647, at most 8 images of at most 2048 bytes each.
- `images_json` holds validated remote references only.

## Write path

Credits are written only inside the existing metadata add/refresh transaction (`library::refresh::movie_facts`), after the provider fetch completes and before commit.

- `MovieDetails.credits`: absent (`None`) preserves stored credits; an explicit `[]` clears them; a list replaces the stored set.
- Rows keep their `id` per `(metadata, credit_tmdb_id)`: changed rows are updated, missing rows deleted, new rows inserted.
- Refresh touches nothing in `movies`, library settings, `movie_files` or tags.
- A failed or invalid provider response is rejected before any transaction opens, so existing credits stay. A storage failure rolls back with the rest of the transaction.
- Adding a movie onto catalog metadata that already holds different credits is a conflict, as for the other catalog facts.

Provider wire shape (fixture-defined, not verified against a live service): `credits: [{creditId, personTmdbId, name, type: "cast"|"crew", order, department, job, character, images: [{coverType, url}]}]`.

## API

All routes are GET only. Paths are under `/api/v1/movies`, and the static `credits` segment takes precedence over `/{id}`.

| Route | Result |
| --- | --- |
| `/credits?limit&offset&movie_id&metadata_id` | `ApiPage<MovieCredit>`; limit 1 to 500 (default 100); `movie_id` and `metadata_id` are mutually exclusive; no filter lists all credits |
| `/credits/{id}` | `MovieCredit` |

Ordering is `metadata_id, type (cast first), order, id`. Errors use the shared envelope: `invalid_request` (400) for unknown or out-of-range parameters, `credit_not_found` (404) for an unknown credit, movie or metadata id. A filter naming a missing movie or metadata returns 404, not an empty page.

`MovieCredit` fields: `id, metadata_id, credit_tmdb_id, person_tmdb_id, person_name, department, job, character, order, type, images[{cover_type, url}]`. Types are in the generated contract (`frontend/src/lib/api.generated.ts`).

## Images and known limits

Image URLs pass through only if they are https, have a DNS host name containing a dot (no IP literals, `localhost` or single-label hosts), and carry no credentials, port or fragment. They are remote provider references that the server never fetches. Cover type is one of `poster, banner, fanart, screenshot, headshot, clearlogo`.

**No local cover mapping is implemented.** api.019 MediaCovers is Blocked, so there is no cached local URL.

Not covered: a live TMDB or Radarr service, the real provider payload shape, importing a Radarr `Credits` table from a snapshot, a scheduled credits refresh (credits update only when a metadata refresh runs), and any frontend.
