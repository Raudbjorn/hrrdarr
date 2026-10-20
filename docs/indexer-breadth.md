# Tracker indexers (indexer breadth)

Nine tracker-specific indexers share one registry family, one settings shape (`TrackerScope`), one side table and one parser module (`src/providers/indexer/tracker.rs`). They sit beside the generic `torrentrss` feed (see [torrent-rss.md](torrent-rss.md)) and the native Torznab/Newznab protocol ([indexer-protocol.md](indexer-protocol.md)).

| Implementation | Protocol | Domains | RSS | Search | Credential kind |
| --- | --- | --- | --- | --- | --- |
| `nyaa` | torrent, RSS 2.0 with the Nyaa namespace | tv, movies | yes | text | `feed` (optional cookie and private parameters) |
| `fanzub` | **usenet**, RSS with NZB enclosures | tv | yes | text | `feed` |
| `iptorrents` | torrent, RSS | tv, movies | yes | none | `feed` (parameters incl. digit-leading category selectors) |
| `torrentleech` | torrent, RSS | tv | yes | none | `api_key` (the RSS key, placed in a URL path segment) |
| `broadcasthenet` | torrent, JSON-RPC | tv | yes | TVDB/TVRage/name | `api_key` |
| `filelist` | torrent, JSON | tv, movies | yes | IMDb or name | `username_password` (HTTP Basic) |
| `hdbits` | torrent, JSON (POST) | tv, movies | yes | IMDb, TVDB or text | `username_password` (in the JSON body) |
| `passthepopcorn` | torrent, JSON | movies | yes | IMDb or text | `username_password` (ApiUser/ApiKey headers) |
| `torrentpotato` | torrent, JSON | movies | yes | IMDb or text | `username_password` (query user/passkey) |

## Configuration

```json
{"implementation":"hdbits","endpoint":"https://tracker.example/api/torrents",
 "tv":{"enable_rss":true,"enable_automatic_search":true,"enable_interactive_search":true,"minimum_seeders":3,"categories":[]},
 "movies":null}
```

- A scope's presence is the domain opt-in. Only the domains a tracker serves are accepted; at least one scope is required.
- Search flags are resolved per capability: RSS-only trackers (`iptorrents`, `torrentleech`) have both flags forced to 0 by a trigger. Download-client binding is never stored; RSS commands and schedules pair an indexer with an explicit client, and `fanzub` pairs only with usenet clients through the registry protocol match.
- `categories` are tracker-native ids (FileList, HDBits). They live in the side table `tracker_scope_options` together with `minimum_seeders`, because the existing `provider_scopes.minimum_seeders` column is check-restricted to `torrentrss`.
- Secrets are write-only, sealed with the existing envelope and bound to provider id and implementation; passkeys never appear in an endpoint, a plaintext column, an error or a public release. A missing account fails with `authentication` before any request.

## Behaviour shared by all nine

- One request per operation through the shared transport (SSRF policy, 1 MiB body cap, timeouts, redirects). Each tracker answers a single document that is paged locally; the fetch size is `offset+limit+1` clamped to 2..100.
- Durable back-off: saved-provider search and RSS call `indexer_backoff::admit`; a 429 `Retry-After` is stored as `disabled_until` in `indexer_status`. The explicit test probe does not honour the cool-down but records its outcome.
- Releases keep the passkey-carrying download locator private; the guid is `"{kind}:{id}"`. Categories map to the 5000 (TV) or 2000 (movie) root, 5070 for anime (Nyaa, Fanzub); BroadcastheNet resolution maps to 5030/5040/5045.
- Malformed rows are dropped and counted as item warnings; an unusable document fails typed (`invalid_response`, `authentication`, `rate_limited`, ...).
- Test, test-draft, bulk test, administration filters, health counts, release-profile indexer references and the `ProviderPanel` all treat trackers as indexers.

## Migration

`migrations/0090_indexer_breadth.sql` registers the nine implementations, creates `tracker_scope_options` (cascade on the scope row, owner and identity triggers) and replaces the triggers that enumerate indexer implementations: scope shape and options, RSS schedule/command admission, search command admission and the two `indexer_status` triggers. The file is numbered 0090 but sits at array position 76 of `MIGRATIONS` because the runner needs a contiguous history; the integrating branch renumbers it and must re-derive the replaced triggers if another branch changed them.

## Not claimed

- **No real tracker was contacted.** Every request and response format was written from memory of the public API and exercised only against in-process fake servers; field names, status codes, error bodies and date formats may differ from the live services. Treat each tracker as "protocol-tested against a fixture", not "known to work".
- FileList timestamps are converted from Romanian local time with a hand-written EU DST rule; the repeated autumn hour is read as summer time.
- The indexer cookie or parameters authenticate the feed fetch only. They are not forwarded to the download client's `.torrent` fetch, so trackers that require a cookie for the download itself will fail at grab time (same limitation as `torrentrss`). Passkey-in-URL trackers are unaffected.
- Fanzub was tested for protocol, usenet-only client pairing and admission, but no end-to-end grab through a usenet client was run. The end-to-end RSS-to-grab path was run for Nyaa (TV) and HDBits (movies) against a mock qBittorrent.
- Category discovery is not supported for trackers; categories must be entered by hand. Snapshot import of Sonarr/Radarr tracker rows is not supported.
- Live rate-limit behaviour, authentication renewal, pagination beyond one document, Atom feeds, relative dates and remote Turso are not covered.
- The browser script (`frontend/tests/tracker-provider-browser.mjs`) covers HDBits and Nyaa only; the other seven trackers are covered by API tests and frontend helper unit tests, not by the browser.
