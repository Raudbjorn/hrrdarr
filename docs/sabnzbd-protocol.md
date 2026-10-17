# SABnzbd protocol

This provider is a usenet download client for TV and movies. It talks to the SABnzbd JSON API over HTTP polling, through the same bounded provider transport as qBittorrent.

Protocol source: [SABnzbd API reference (4.5)](https://sabnzbd.org/wiki/configuration/4.5/api), inspected 2026-10-17. The 4.3 page redirects there. The recorded samples in `tests/sabnzbd_protocol.rs` are taken from it: queue, history, `addurl`, `get_cats` and the API-key error text. The `get_config` fixtures use the documented section and key names, but the reference has no full response sample for them. None of this is evidence from a live-client run.

Requirements were inventoried from `src/NzbDrone.Core/Download/Clients/Sabnzbd/` in Sonarr `76c684e097f16ac216e6213845e5cac372774995` and Radarr `c90668a520664ad0c91812cfee57c41928ad2148`, plus `RemoteEpisode.IsRecentEpisode` and `MovieMetadata.IsRecentMovie`. The implementation is independent of both.

## Configuration

- `endpoint` is the base URL, including scheme, host, port and any URL base (for example `http://host:8080/sabnzbd`). The client adds `/api` to it.
- Each scope has:
  - `category`: required. It must be 1–64 bytes and trimmed, must not be `*`, and must not contain control characters or any of `, * / \ { }`.
  - `recent_priority` and `older_priority`: one of `default` (-100), `paused` (-2), `low` (-1), `normal` (0), `high` (1) or `force` (2).
  - `tags`: optional eligibility tags, with the same semantics as qBittorrent.
- TV and movie categories must differ, compared case-insensitively. SABnzbd compares categories case-insensitively, so a shared category would let either domain see the other's jobs.
- Credentials are only `{"kind":"api_key"}`. The key is stored in the encrypted credential envelope, sent as the `apikey` query parameter, and redacted from projected strings. Username/password and the NZB-only key are not supported. The NZB key is rejected by `get_config`.
- "Recent" means one of:
  - an episode that aired within 14 days;
  - a movie with a physical or digital release within 21 days;
  - a movie that reached cinemas within 120 days.

  These thresholds are from the requirement references, not from the SABnzbd API.

## Test

`POST /api/v1/providers/{id}/test` and `test-draft` run these steps in order:

1. `version`. This call is unauthenticated. Releases older than 3.x are unsupported. A `develop` build is accepted with the `develop_version` warning.
2. `get_config`.
3. For each configured scope:
   - The category must exist. Otherwise the test fails with 422 `sabnzbd_category_missing`.
   - The category folder must not end in `*`, which disables job folders. Otherwise 422 `sabnzbd_job_folders_disabled`.
   - No TV, movie or date sorting, legacy (`misc.enable_*_sorting`) or 4.x `sorters`, may apply to the category. An empty category list counts as all categories. Otherwise 422 `sabnzbd_sorting_enabled`.
4. A scoped `queue` read, to prove that the key can read jobs.
5. History retention that may delete completed jobs within 14 days adds the `removes_completed_downloads` warning.

A wrong or missing key returns HTTP 502 with error code `authentication`, the same as other providers, and is stored as the failed observation. The client recognizes a wrong key from the plain-text `error: API Key Incorrect/Required` reply or from `{"status":false,"error":…}`.

## Submission identity and idempotency

SABnzbd assigns `nzo_id` only after `addurl` succeeds, so a lost response leaves no identifier to reconcile with. Preparation therefore derives a deterministic 40-hex token. The token is the first 40 hex characters of a SHA-256 over the provider UUID, the settings fingerprint (tags excluded), the typed `MediaTarget` and the NZB URL.

- The token is the journaled submission hash and the `rss_hash_claims` key.
- The job is submitted as `nzbname = "<sanitized title ≤180 B>.hrrdarr-<token>"`.
- The marker is removed from projected job names. It does remain in SABnzbd's on-disk job folder, and therefore in `original_file_path`.
- The remote identity is `sha1("sabnzbd-nzo:" + nzo_id)`, a fixed 40-hex value stored as `observed_hash`. The raw `nzo_id` is never stored.

Submission runs these steps in order:

1. `version`.
2. `get_cats`. The category must exist, or the error is `UnsupportedFeature`. No category is ever created, and a job is never sent to Default.
3. A marker lookup across queue and history. A match returns `AlreadyPresent`.
4. `addurl` with `name`, `nzbname`, `cat` and `priority`.
5. Exactly one `nzo_id` is required. Its queue or history entry must show the configured category.

Any failure after dispatch is `MutationUnknown`, except a definite authentication failure. It is never retried automatically. Reconciliation searches by marker. If the job turns up in the other domain's category, that is `ScopeConflict`, never "absent". Because the durable journal does not allow `reconciling → observed`, a lost response whose job is later found becomes `needs_attention` / `presence_unconfirmed`, and is not resubmitted.

## Refresh and completed handoff

- The client reads `queue` and `history` filtered by `cat`, each limited to 500 slots. A full queue page fails closed with `ResponseTooLarge`. Only the 500 most recent history entries in each category are observed, and older history is invisible. A slot whose category differs from the filter is `ScopeConflict`.
- Queue status mapping:
  - A global pause, unless the job's priority is Force, or a job's own `Paused` status gives `paused`.
  - `queued`, `grabbing` and `propagating` give `queued`.
  - `downloading`, `fetching`, `checking` and `quickcheck` give `downloading`.
  - `verifying`, `repairing`, `extracting`, `moving` and `running` give `downloading` with the matching diagnostic.
  - Anything else gives `unknown`.
- History status mapping:
  - `Completed` gives `completed`.
  - `Failed` with the exact message "Unpacking failed, write error or disk is full?" gives `warning` / `disk_full`. Other failures give `failed`.
  - Post-processing states give `downloading`.
  - `Deleted` is skipped.
- A job that appears in both queue and history while it moves between them is reported once, from the queue.
- Names that look like URLs, which SABnzbd shows while it fetches an NZB and may carry an indexer key, are projected as `(fetching NZB)`.
- The output of a completed job is its history `storage` path, widened to the enclosing folder named after the job. Only rooted POSIX, UNC or drive paths are accepted.
- SABnzbd does not list the files of completed jobs, so the output must resolve through an explicit remote path mapping for the endpoint host. Without one, processing is blocked with `path_mapping_missing` (health `hc021_mapping_missing`). The mapped local output is enumerated on a blocking thread, bounded to 2048 files and depth 6, with symlinks and special files skipped. CDH/import then proceeds as for qBittorrent, for both domains.
- The health and status snapshot uses `misc.complete_dir`, falling back to `fullstatus.completedir` when the configured value is relative, joined with the category folder.

## Protocol routing

Migration 0058 adds `provider_implementations(implementation, kind, protocol)`, a write-once table:

| Implementation | Kind | Protocol |
| --- | --- | --- |
| torznab | indexer | torrent |
| newznab | indexer | usenet |
| qbittorrent | download_client | torrent |
| sabnzbd | download_client | usenet |

`providers.implementation` now references this table. Every trigger or view that previously named qBittorrent now selects `kind='download_client'` from it.

A grab or binding must use a client of the indexer's protocol:

- RSS and search admission triggers, `/api/v1/rss/commands`, `/api/v1/search/commands` and the RSS schedules reject an explicit mismatch with 409 `protocol_mismatch`.
- An indexer's stored client preference for the other protocol is kept rather than erased, like a disabled client preference. The `IndexerDownloadClient` health check, like the references', checks only presence and enablement, so it does not report the mismatch. Default selection rejects it at dispatch. Durable candidate error codes are a closed vocabulary, so the stored code is `unsupported_target`.
- Default selection considers only clients of the matching protocol.

To add a client:

1. Insert a registry row.
2. Replace the `provider_scope_shape_*` and options triggers.
3. Add a `ProviderSettings` variant and the dispatch arms in `providers::download` and `RefreshClient`.

No table rebuild is needed.

## Not supported

- The `files` endpoint, imported categories and remote removal.
- Snapshot import of SABnzbd clients.
- Downloaded-scan tracked receipts, which are still qBittorrent-only.
- Username/password authentication.
- Live SABnzbd compatibility, which was not exercised.
