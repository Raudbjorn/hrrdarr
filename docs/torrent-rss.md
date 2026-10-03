# Generic torrent RSS feeds (`torrentrss`)

`torrentrss` is a native indexer implementation for a plain RSS 2.0 torrent feed. It is the shared foundation for later feed-style indexers (Nyaa, Fanzub, IPTorrents, Torrentleech): the parser in `src/providers/indexer/feed.rs` is generic over the namespaces in use and carries no tracker-specific rules. It was implemented from RSS 2.0 and common torrent-feed conventions (`enclosure`, `link`, `guid`, `pubDate`, the ezrss `torrent:` and Nyaa `nyaa:` namespaces, Torznab `attr` elements); no reference code was copied or translated.

## Configuration

```json
{"implementation":"torrentrss","endpoint":"https://tracker.example/rss",
 "tv":{"enable_rss":true,"minimum_seeders":5},"movies":null}
```

- `endpoint` follows the Torznab endpoint rules: http(s), no userinfo, **no query string or fragment**. Secrets must never sit in the plaintext endpoint column.
- A scope's presence is the domain opt-in (feeds have no capability list). At least one of `tv`/`movies` is required. Each scope has `enable_rss` (default true) and `minimum_seeders` (nullable, 0 to 1,000,000).
- **Search flags**: there are none. `enable_automatic_search`, `enable_interactive_search`, `download_client_id` and categories are *rejected* as unknown fields on input (`deny_unknown_fields`), stored as 0/NULL, and enforced by storage triggers. Consequently automatic/interactive search eligibility is always false for a feed (`indexer_operation_enabled`), `/search` with a `tv`/`movie` request answers 409 `provider_unavailable` without any network call, and the health `indexer_search` check treats a feed-only library as having no search (honest by construction).
- **Credentials** (`kind: "feed"`, write-only, sealed with the existing AES-GCM envelope, bound to provider id and implementation): `cookie` (visible ASCII only, at most 4096 bytes, sent as a `Cookie` header) and per-domain `tv_parameters`/`movie_parameters` (up to 16 name/value pairs appended to the endpoint query, e.g. a passkey). Reserved Torznab names are allowed because a feed has no Torznab vocabulary. `api_key`, `username_password` and `indexer` credential kinds never validate for a feed; `feed` never validates for another implementation. Secrets are redacted from release titles, never echoed by any response, never stored in plaintext, and never put in error text.
- **Known limitation, cookie scope**: the cookie and private parameters authenticate the *feed fetch* only. The `.torrent` bytes of an enclosure/link release are downloaded later by the download client's transport (`qbittorrent::prepare`), which sends no indexer cookie. Trackers whose download links work with a passkey in the link (the common case) are unaffected; trackers that additionally require the session cookie on the download will sync but fail at grab. Forwarding an indexer secret into another provider's transport needs its own design and is not part of this slice. Magnet-bearing items never need the download.
- **Minimum seeders**: items reporting fewer seeders are skipped; items reporting none are kept (an unknown value is not evidence of too few). Skipped items are counted in the test result as `below_minimum_seeders`.
- **Download-client binding**: not supported. RSS commands and schedules already pair an indexer with an explicit client, so a stored preference adds nothing yet; the 0049 `download_client_id` CHECK (native indexers only) is left unchanged and the importer maps no binding.

## Parsing and failure policy

One bounded fetch (transport redirect policy, 1 MiB body cap, timeouts, per-provider lane/cooldown from `src/providers/http.rs`) of the feed URL plus private parameters and optional cookie.

The feed is **invalid** (typed `invalid_response`, whole fetch fails) when it is not well-formed XML, contains a DTD or entity declaration (no expansion), is over the XML caps (1 MiB, 20,000 nodes, **element depth 32**), is not `rss` with exactly one `channel`, has more than 500 items, or has items but *none* valid. Otherwise each malformed item is **dropped individually** and counted (`rejected`); duplicate `guid`s keep the first. For the RSS command this differs deliberately from native Torznab, where one invalid item fails the whole capture: a generic feed is expected to contain the occasional bad entry.

Per item: `title` (required, at most 2048 bytes), a locator (torrent-typed `enclosure`, else an untyped one, else `torrent:magnetURI`/`magneturl`, else `link`; http(s) or `magnet:` with a BitTorrent hash only; relative references are resolved against the feed URL; NZB enclosures are never selected), `pubDate` or `dc:date` (required; RFC 2822 including obsolete zone names, RFC 3339, zone-less values read as UTC, a bare date), size (Torznab `size`, `torrent:contentLength`, `nyaa:size`, `<size>` or enclosure `length`; plain bytes or a decimal with B/KB/KiB/MB/MiB/GB/GiB/TB/TiB, all 1024-based; zero means "not reported"; malformed rejects the item), optional seeders, leechers, peers and a 40-hex info hash (derived from a magnet when absent). A malformed optional number rejects the item; it is never zeroed.

**Relative dates are not supported**: "2 hours ago" is rejected per item rather than guessed, because a guessed instant would move a release's age between replays. "Relative" in the requirement was interpreted as relative *URLs*, which are supported.

**Categories**: feeds have no category vocabulary, but the decision engine rejects releases without a media-domain category. An item with no numeric Torznab category is therefore given the standard root category of the scope it was fetched for (TV 5000, Movies 2000); explicit numeric categories on the item win, so a wrong-domain item is still rejected. Title parsing and library matching still decide; a mixed feed placed in the TV scope will simply fail to match movie titles.

## API behaviour

- `POST /api/v1/providers/{id}/test` (and `test-draft`) fetches the feed for every configured scope and answers `{"domains":[{"media_type","parsed","rejected","below_minimum_seeders"}]}`; an empty well-formed feed is valid evidence of access. Failures are typed like the other indexers (`authentication`, `invalid_response`, `response_too_large`, `redirect_rejected`, `transport_error`, `timeout`, `rate_limited`) and stored against the tested revision (`provider_tests`); a configuration change invalidates the observation.
- `POST /api/v1/providers/{id}/search` accepts only `kind: "rss"`; offset/limit page locally over the single fetched document.
- The RSS command (`src/commands/rss.rs`) polls feeds per enabled domain scope through the same `raw_search`, with the same provider lane, rate-limit cool-down, revision fencing and fingerprint idempotency as Torznab. Replays do not repeat external grabs; disabled providers, absent scopes, disabled RSS flags and stale revisions are refused before any request.

## Migration 0050

`0050_torrent_rss_indexer.sql` is the only new migration. `providers` is referenced by cascading foreign keys and the runner holds `foreign_keys=ON` inside its transaction, so a rebuild would delete dependent rows. The two CHECK lists (`providers.implementation` and the `provider_scopes` table CHECK) are therefore widened in place with the documented `writable_schema` edit plus `PRAGMA writable_schema=RESET` (verified against this libSQL build; every existing row already satisfies the wider predicate). A guard table makes the migration fail and roll back if either replacement did not apply. Triggers are dropped and recreated from their *live* 0049 definitions. One nullable column, `minimum_seeders`, is added with a feed-only CHECK. The edit is local-engine behaviour; the runner already refuses pending remote migrations, and remote Turso was not exercised.

## Per-site decisions for implementation enumerations

| Site | Decision |
| --- | --- |
| `providers.implementation` CHECK (0009) | Extended in 0050 |
| `provider_scopes` table CHECK (0009) | Extended in 0050 (feed shape: no categories, no client columns) |
| `provider_scopes.download_client_id` CHECK (0049) | Left: native indexers only |
| `provider_scope_options_*` (0011), `provider_client_options_*` (0012) | Extended; feed scopes must have search flags 0 and all native/client options NULL |
| `provider_test_*_owner` (0010/0012) | Extended: revision-bound tests allowed for feeds |
| `rss_commands_admit`, `rss_schedules_admit`, `rss_schedules_update` (0025) | Extended: a feed may be an RSS indexer paired with a qBittorrent client |
| `search_commands_admit` (0027 lineage) | Left: feeds cannot search |
| Older superseded trigger definitions (0027/0030-0033/0042 and others) | Left byte-identical; only the live definitions were rewritten |
| `src/providers/mod.rs` settings enum, `read`, `write_scopes`, probe, health-change capture, importer reconstruction | Extended |
| `src/providers/mod.rs` client-reference lookup and binding validation | Left: only native indexers bind clients |
| `src/providers/indexer.rs` `endpoint()`/`scopes()`/capability code | Left (`Unsupported` for feeds); `raw_search` branches to the feed path first |
| `src/providers/credentials.rs` | New `feed` kind; `api_key` refused for feeds |
| `src/providers/administration.rs` schema/filter/bulk test-all | Extended (template, filters, indexer batch selection) |
| `src/commands/rss.rs` `valid_target`, `capture_feed` | Extended; item warnings tolerated for feeds only |
| `src/health/indexers.rs` COUNTS | Extended; no new `health_checks` rows and therefore no new evaluators |
| `src/health_detectors.rs` and `src/health/download_roots.rs` client selection (`NOT IN ('torznab','newznab')`) | Changed to a positive `='qbittorrent'` filter; otherwise a feed would be mistaken for an unsupported client and make the check unevaluable |
| `src/health_detectors.rs` binding detector catalogue | Extended (feeds never carry a binding) |
| `src/release_profiles.rs`, `src/snapshots/release_profiles.rs` indexer references | Extended: a profile may scope to a feed |
| Download-client command guards (`commands/mod.rs`, `processing.rs`, `worker.rs`) | Left: qBittorrent only |
| Snapshot importer (`src/snapshots/providers.rs`) | Left: Sonarr/Radarr `TorrentRss` rows are reported as unsupported (the source feed URL usually embeds a passkey in its query, so mapping would have to split secrets into sealed credentials) |
| Category discovery (`categories.rs`) | Left: feeds have no capabilities document |
| Frontend `ProviderPanel.svelte` | Lists, shows, tests and deletes feeds; creation/editing are intentionally not offered so a save cannot rewrite scopes or sealed credentials |

## Shared XML hardening change

While adding a nesting test the existing shared XML helper was found to be exposed to stack exhaustion: the parser recurses per element level, and the 20,000-node cap alone still permits far deeper nesting than a 2 MiB worker stack tolerates (a 500-level document overflowed a 2 MiB stack in a debug build). A linear pre-scan now rejects element depth above 32 for **both** the Torznab/Newznab and feed paths (real documents nest fewer than ten levels).

## Not covered

Live trackers and real-world feed variety, cookie/session renewal, Atom feeds, relative-date feeds, binding a feed to a download client, a UI create/edit flow, and remote Turso are not covered.
