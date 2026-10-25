# NZBGet protocol

NZBGet is a usenet download client for TV and movies. It talks to NZBGet's JSON-RPC API (`POST <endpoint>/jsonrpc`) through the same bounded provider transport as qBittorrent, SABnzbd and Transmission. It shares the usenet path with SABnzbd: protocol-aware client selection (Newznab indexers only), the durable submission journal, RSS claims, download refresh snapshots, completed-download processing, remote path mapping and import.

Protocol source: NZBGet's published API reference (`version`, `config`, `status`, `listgroups`, `history`, `append`, `editqueue`). The mocks in `tests/nzbget_protocol.rs`, `tests/nzbget_automation.rs` and `frontend/tests/nzbget-provider-browser.mjs` are written from that reference. None of this is evidence from a live-client run.

Requirements were inventoried from `src/NzbDrone.Core/Download/Clients/Nzbget/` in Sonarr `76c684e097f16ac216e6213845e5cac372774995` and Radarr `c90668a520664ad0c91812cfee57c41928ad2148`: host/port/SSL/URL base and credentials, a category and recent/older priority per domain, add paused, the version floor, the category and `KeepHistory` checks, the status rules for queue and history entries, and the sizes split into 32-bit halves. The implementation is independent of both.

## Configuration

`ProviderSettings::Nzbget { endpoint, tv, movies }`, stored by migration 0064.

- `endpoint` is the NZBGet base URL, including any URL base, for example `http://nzbget.example:6789`. The client appends `/jsonrpc`.
- Each scope (`NzbgetScope`) has `category` (1-64 characters, trimmed, no control characters, `,`, `*`, `/`, `\`, `{` or `}`), `recent_priority` and `older_priority` (`very_low`, `low`, `normal`, `high`, `very_high`, `force`, sent as NZBGet's -100, -50, 0, 50, 100, 900), `add_paused` and optional eligibility `tags`. "Recent" uses the same thresholds as SABnzbd.
- TV and movie categories must differ case-insensitively (validation and the unique index `provider_nzbget_categories`).
- Storage: category, priorities and add-paused (`initial_state` `started`/`stopped`) live in `provider_scopes`. That table's existing CHECK limits priority columns to -100..2 and could not be altered without rebuilding the table, so priorities are stored as codes (very_low -100, low -1, normal 0, high 1, very_high 2, force -2) and mapped back by `NzbgetPriority::code`/`stored`. The API never exposes codes.
- Credentials are only `{"kind":"username_password"}` (HTTP basic authentication, NZBGet's ControlUsername/ControlPassword) or none; `api_key` is rejected. The username must not contain `:`. Credentials stay write-only and both values are redacted from projected strings.

## Calls and errors

Each call is `{"jsonrpc":"2.0","method","params","id"}`; a response `id`, when present, must echo the request's. An `error` object is `Rejected` without reflecting its text. HTTP 401/403 is `authentication` (502 from the API), 404 and 5xx are transport failures. Responses are bounded to 1 MiB; NZBGet has no paging, so a queue over 4096 jobs or a history over 8192 entries (or 1 MiB) fails closed rather than truncating.

## Test

`POST /api/v1/providers/{id}/test` and `test-draft`:

1. `version`: the number before the first `-` must be 13 or later (`UnsupportedVersion` otherwise; `append`'s nine-argument form exists from 13). A `testing-...` build warns `testing_version`.
2. `config`: `KeepHistory` 0 is 422 `nzbget_keep_history_zero` and more than 25000 is `nzbget_keep_history_over_max`; each configured category must exist among `Category<N>.Name` (case-insensitive, N up to 99), else 422 `nzbget_category_missing`. Fewer than 2 days of history warns `short_history_retention`.
3. `status`, `listgroups` and `history` prove the credentials can read jobs within the size bounds.

The result is `NzbgetTest { nzbget_version, domains, warnings }`.

## Status mapping

Queue (`listgroups`, only jobs in this domain's category are returned; other categories still
contribute to queue-time estimates):

- Queue results are filtered to this domain's category, but other queued categories remain in the server-wide ETA calculation.
- Paused: global `DownloadPaused`, `Status` PAUSED, or all remaining bytes paused.
- QUEUED, or DOWNLOADING with no active downloads and bytes remaining: `queued`. DOWNLOADING and FETCHING: `downloading`.
- LOADING_PARS, VERIFYING_SOURCES, VERIFYING_REPAIRED: `downloading` / `verifying`; REPAIRING: `repairing`; UNPACKING: `extracting`; MOVING: `moving`; PP_QUEUED, RENAMING, EXECUTING_SCRIPT, PP_FINISHED: `post_processing`. Any other status is `unknown`.
- ETA is the unpaused bytes of every group ahead in server queue order, including other categories, plus this job's, divided by `DownloadRate`; none when paused, when the rate is 0 or beyond a year. Sizes combine `...Lo`/`...Hi` into 64 bits (MB fields are a fallback). A URL still being fetched is shown as "(fetching NZB)", never as the locator.

History (`history`, category-filtered, `Kind` DUP records skipped):

- URL-kind history items are failed URL fetches: `UrlStatus` FAILURE/SCAN_FAILURE is `failed`, and SCAN_SKIPPED is `warning`; they are never importable.
- A user deletion (`DeleteStatus` MANUAL) is omitted unless `MarkStatus` is BAD, which is `failed`.
- Otherwise the detail statuses are graded as NZBGet itself grades them (`NzbInfo::MakeTextStatus`), most severe first: `failed` for `ParStatus` FAILURE, `UnpackStatus` FAILURE, `MoveStatus` FAILURE, `MarkStatus` BAD or `DeleteStatus` HEALTH, DUPE, SCAN, COPY or BAD; `warning` for `ParStatus` MANUAL (damaged) or REPAIR_POSSIBLE (repairable), `UnpackStatus` SPACE (diagnostic `disk_full`) or PASSWORD (encrypted), `ScriptStatus` FAILURE (only the post-processing script failed) or another `DeleteStatus`; else `completed`. This differs from the references in that a later warning cannot hide an earlier failure and that the individual values carry NZBGet's own severities. Entries with no detail fields use the combined `Status` (`SUCCESS/...`, `WARNING/...`, `FAILURE/...`, `DELETED/...`).
- Only `completed` is importable. The output folder is `FinalDir` (else `DestDir`), widened to a folder named after the job, and goes through the shared remote path mapping and directory listing as for SABnzbd.

Remote identity is the SHA-1 of `nzbget-nzb:<NZBID>`; the NZBID is the same in queue and history.

## Submission identity and idempotency

`prepare` derives a deterministic token from provider, settings fingerprint, typed target and release URL (no network I/O) and names the job `<release title>.hrrdarr-<token>`. Submission:

1. Verify version, then that the category exists (an unknown category would land in the default destination).
2. Search queue and history for the token. Present in this domain: `AlreadyPresent`; present in another category: `ScopeConflict`. Nothing is added.
3. `append(<name>.nzb, <url>, category, priority, false, add_paused, "", 0, "FORCE")`: NZBGet downloads the NZB itself from the URL, so no NZB bytes pass through hrrdarr and no 1 MiB body limit applies. `FORCE` disables NZBGet's own duplicate handling; duplicates are prevented by the token.
4. A result of 0 or false is a definite refusal (`Rejected`, nothing submitted). Any other failure after dispatch is `MutationUnknown` and never retried. A positive id is verified in queue or history for category and token.

Reconciliation searches by token and never resubmits; as for the other clients a lost response whose job is found becomes `needs_attention` / `presence_unconfirmed`, and repeated RSS for the release does not add it again.

## Controls

`nzbget::control` implements Pause (`GroupPause`), Resume (`GroupResume`), Priority (`GroupMoveTop`/`GroupMoveBottom`) and Remove (`GroupFinalDelete`, or `HistoryDelete` for a finished job without deleting files) through `editqueue(Command, 0, "", [id])`, each followed by a verifying read. Deleting files of a finished job, imported categories and file priorities are `UnsupportedFeature`. No API route or UI invokes controls yet.

## Protocol routing

Migration 0064 registers `nzbget | download_client | usenet` and replaces the scope shape, options and client-options triggers with an NZBGet branch. Newznab indexers may bind to it; Torznab indexers are rejected with `protocol_mismatch`. Default selection, RSS/search dispatch, refresh schedules, Activity, completed-download processing and the download-root/path health checks treat it like SABnzbd.

## Not claimed

- Live NZBGet compatibility was not exercised. Every observation comes from owned mocks written from the API reference on 127.0.0.1; no installed or default-port (6789) NZBGet was contacted.
- Adding by URL assumes NZBGet keeps a job's NZBID and name when it finishes fetching the NZB. The references upload NZB bytes instead; that path is not implemented. If a real server changes the id, tracked receipts for URL-added jobs would not match.
- The `editqueue` four-argument form and the nine-argument `append` were taken from the API reference, not observed. Versions before 13 are refused.
- Large histories (over about 1 MiB or 8192 entries) fail refresh closed; there is no paging in NZBGet.
- Controls exist at the protocol layer only. Snapshot import of Sonarr/Radarr NZBGet clients, failed-download redownload (`HistoryRedownload`) and the health classifier (still reports kind Other) are not implemented.
- Migration 0064 follows 0063 (notifications); the autotag lane renumbers after it.
