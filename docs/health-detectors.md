# Download client health observations

## Completed download handling

The TV and movie detectors independently consume the existing per-domain `enabled` and `defined` authority. They never modify download permissions, reconcile clients, or equate readiness suppression with a disabled master switch. Both identify compatibility type `ImportMechanismCheck`; the engine supplies separate domain/check identities and warning severity.

TV first obtains fresh status for every enabled TV-scoped download client, including when the master setting is explicitly true or false. Undefined settings produce migration guidance for non-loopback endpoints, otherwise all-SABnzbd, all-NZBGet, or other/mixed local clients. Empty clients follow vacuous `all`, giving the SABnzbd guidance. An explicit equal-value save resolves undefinedness without changing effective authority. Movies only consult `enabled`; they do not probe clients or warn on undefined default-on settings.

Status reads use the existing qBittorrent transport, credential snapshot, provider revision check, request deadlines and shared operation permits. The read transaction closes before network requests. Unsupported clients, status failures and limit overflow return unevaluated errors, never invented OK. The caller owns the 30-second whole-batch deadline, cancellation and atomic generation-fenced publication. A probe future owns its transport permit and releases it when dropped. Client selection is bounded to 256; excess is an explicit failure. Serial probing may time out before reaching that ceiling; larger deployments require resumable batches before raising the budget.

`Loopback` establishes the native endpoint classification; `Unknown` selects conservative non-local path guidance without asserting that the server is physically remote. This is deliberately not DNS or local-interface discovery. Failed observations remain failed/stale rather than reproducing the upstream TV exception-to-OK behavior. Persisted results, generation fences and timed startup-grace handling belong to the health engine, not the detector.

Reference requirements: Sonarr `76c684e097f16ac216e6213845e5cac372774995` and Radarr `c90668a520664ad0c91812cfee57c41928ad2148`, `src/NzbDrone.Core/HealthCheck/Checks/ImportMechanismCheck.cs` and `HealthCheck.cs`. Sources establish requirements only; implementation is native Rust, not translated upstream code. Domain help links and compatibility type are retained for future notification consumers; no notification delivery is implemented here.

`tests/health_detectors.rs` covers the pure domain/client/status matrix and owned qBittorrent HTTP fixtures for status-first behavior, movie independence, scoped selection, equal-value save, configuration revision fencing and cancellation/permit release. SABnzbd/NZBGet entries are classifier fixtures only: their runtime transports are not implemented by this change. The wider health catalog and notification/script delivery remain incomplete.


## Download client communication

TV and movies independently evaluate enabled download clients in their own scope under `download_client_communication`, with compatibility type `DownloadClientCheck`. No enabled scoped client produces a warning (`download_client_none_available`); indexers, disabled clients and other-domain scopes do not count. Successful scoped item retrieval, including an empty list, produces OK. A current remote failure produces an error (`download_client_communication_failed`), not a no-client warning. The error identifies the failed configured provider by its validated display name (at most 128 bytes, no control characters) and UUID. Messages never include endpoints, credentials, item names or remote response bodies. The no-client message remains static.

The detector reuses `RefreshClient::probe_download_communication` and qBittorrent `refresh_pages`; it does not create a second HTTP client or write queue state. This is a decoded, paginated item observation. The existing `qbittorrent::test_connection` already checks versions, preferences, categories and a scoped `raw_list(limit=1)` authorization read; describing it as a version-only test would be incorrect. That small connection-test read does not replace the bounded full item observation.

Selection reads at most 257 rows to reject more than 256 clients explicitly, closes its read transaction, and probes serially. Each scoped item observation uses 500-item pages, at most 1,000 items and a 1 MiB serialized result; the transport also caps each response body at 1 MiB. Overflow remains unevaluated, never a healthy truncated prefix. Existing transport limits are eight concurrent operations, 256 provider lanes, one active operation per provider, a 100 ms request interval, a 30-second operation deadline, 10-second request timeout, 3-second connect timeout and 5-second read timeout. Requests leave a 5-second operation reserve. Credential/configuration snapshot acquisition has a 5-second timeout; the engine additionally owns the aggregate batch deadline and cancellation. Dropping the probe future releases its operation permit.

Remote authentication, connection refusal, dispatched request timeout, rejected HTTP responses (including a newly observed 429), and malformed protocol responses are evaluated communication failures. Local credential/configuration failures, unsupported implementation/capability, busy lanes, cached rate-limit cooldown, request construction or pacing failure, and response/item/enumeration limits remain unevaluated. Dispatch provenance resets for each request, so an earlier dispatched request cannot turn a later local pacing timeout into remote failure. Both success and failure must pass current provider revision and operation-validity checks; provider deletion or changed scope/configuration invalidates the observation. Engine generation fencing and per-check publication decide whether an observation can replace stored health; unevaluated results cannot clear a prior issue.

Configuration writes select communication and completed-download checks for the old/new domain union. Authoritative provider-test writes compare normalized `(status, error_code)` at the current configuration revision: the first or changed result marks communication dirty for its configured domains in the same transaction, while repeated identical results only update the test timestamp. Indexer tests do not mark download health. Marker due times are not postponed by repeated changes. Ordinary marker failure rolls back the test write; the engine helper handles exact generation exhaustion by retaining the authoritative outcome and recording `health_invariant`. Direct health probes do not write provider-test outcomes or recursively mark themselves dirty. Repeated download-client failure/backoff health (`hc.018`) is outside this change.

Reference requirements are the same pinned Sonarr/Radarr revisions above, `HealthCheck/Checks/DownloadClientCheck.cs`; native implementation retains both no-client and communication-failure outcomes. `tests/health_client_communication.rs` contains owned-loopback fixtures for domain selection, empty/nonempty items, actual remote failures versus cached cooldown, configuration revision changes/deletion, cancellation and permit release, normalized status-marker deduplication/rollback/exhaustion, local credential/configuration failures, enumeration/item/body limits, and safe attribution among multiple clients. Transport unit tests separately distinguish pre-dispatch pacing from dispatched timeout. These test cases describe the verification scope, not a claim that an unrecorded run passed. Engine tests own aggregate deadlines, mixed evaluated/unevaluated publication and generation rejection. No live client, SABnzbd/NZBGet transport, root-folder/path detector, full health catalog, notification delivery or failure-backoff closure is established here.

## Recycling bin

Scoped `recycle_bin` (`RecyclingBinCheck`, migration 0060) is healthy when the domain has no
recycle bin. A configured bin that does not exist reports error `recycle_bin_missing`; a bin that
traverses a symlink, is not a directory or fails a create/remove write probe reports error
`recycle_bin_unwritable`. Saving recycle bin settings marks the check for re-evaluation. File
deletion and cleanup refuse a missing bin independently of Health. See
[recycle-bin.md](recycle-bin.md).

## Library root folders (hc.034)

Migration 0077 (runner position 77) registers three observation-only
identities, all startup + six-hourly scheduled + manual:

| Scope | Check key | Compatibility type | Help link |
| --- | --- | --- | --- |
| `tv` | `root_folder` | `RootFolderCheck` | `https://wiki.servarr.com/sonarr/system#missing-root-folder` |
| `movies` | `root_folder` | `RootFolderCheck` | `https://wiki.servarr.com/radarr/system#missing-root-folder` |
| `movies` | `collection_root_folder` | `MovieCollectionRootFolderCheck` | `https://wiki.servarr.com/radarr/system#movie-collection-missing-root-folder` |

Upstream requirements (Sonarr/Radarr `HealthCheck/Checks/RootFolderCheck.cs`, Radarr
`MovieCollectionRootFolderCheck.cs`): each series/movie path maps to its best root folder (the
longest configured root that strictly contains it, otherwise the item's parent folder); a root
that is an invalid path or does not exist is an **Error**. One root reports reason
`RootFolderMissing` with "Missing root folder: {path}"; several report `RootFolderMultipleMissing`
with "Multiple root folders are missing: {paths}" joined by ` | `. Radarr's collection variant
uses each collection's own root folder and reports "Missing root folder for movie collection:
{path} ({titles})" / "Multiple root folders are missing for movie collections: …". hrrdarr keeps
these message prefixes, separators, severities and help anchors; the implementation is native.

Native behaviour (`src/health/library_roots.rs`):

- TV and movies are evaluated independently from their own `series`/`movies` rows and
  same-domain `root_folders`; numeric IDs never cross domains. Collections are evaluated only in
  the movies scope and only when `movie_collections.root_folder_id` is set (deleting a root clears
  it and disables collection monitoring, so there is no dangling reference to report).
- Items are aggregated per root: each listed root shows its state, affected item count and up to
  three titles, e.g. `Missing root folder: /media/tv (not found; 3 series, e.g. A, B, C). …`. At
  most five roots are listed; the rest are counted (`N more root folders not listed`). Paths are
  shown up to 256 bytes and titles up to 48 bytes with control characters replaced; the message
  never exceeds the stored 4096-byte bound.
- Each root is probed with the same anchored, read-only walk the importer uses (every component
  opened `O_DIRECTORY|O_NOFOLLOW`). Nothing is created, written, enumerated or probe-written.
  States are reported distinctly where the kernel distinguishes them, with reason
  `{root_folder|collection_root_folder}_{state}` for a single root and `…_multiple_missing` for
  several: `missing` (ENOENT; an unmounted mount point whose stub folder was removed is
  indistinguishable from this), `not_directory`, `symlink` (a component is a symbolic link, which
  hrrdarr refuses to traverse — upstream follows links and would call such a root present),
  `permission_denied` (EACCES/EPERM), `unavailable` (stale/disconnected network or FUSE mount,
  missing device, I/O error and other errnos), `invalid` (a stored item path that fails path
  validation; grouped as `(invalid library paths)`) and `unresponsive` (the probe did not return
  within the budget).
- Bounds: at most 100,000 library rows scanned, 512 distinct roots probed, and a 2-second probe
  budget on one blocking thread (`spawn_blocking`), plus 250 ms slack. A probe stuck in the kernel
  keeps its thread and one of two permits; the evaluation reports that root as `unresponsive` and
  every later root as not evaluated, and further evaluations return `check_timeout` (unevaluated,
  retaining the previous observation) while both permits are held. Any bounded-out item is
  reported: appended to an Error ("Evaluation was bounded: N series were not evaluated."), or, when
  nothing missing was found, a **Notice** `…_check_truncated` — never a healthy result.
- Root folder create/delete and snapshot imports mark the root checks of both domains in the
  same transaction (`health::library_roots_changed`), with the usual five-second debounce. Library
  item add/move/delete do not mark them; the six-hour schedule and manual runs cover those.

## Read-only library mounts (hc.035)

Scoped `mount_read_only` identities in `tv` and `movies` (migration 0077), compatibility type
`MountCheck`, help links `https://wiki.servarr.com/sonarr/system#series-mount-ro` and
`https://wiki.servarr.com/radarr/system#movies-mount-ro`.

Upstream requirement (Sonarr/Radarr `HealthCheck/Checks/MountCheck.cs`): resolve the mount of
every series/movie path; distinct mounts whose options are read-only produce one **Error**
(reasons `MountSeries` / `MountMovies`) with "Mount containing a series path is mounted
read-only: " (movies: "a movie path") followed by `name (path)` entries.

Native behaviour (`src/health/mounts.rs`):

- Linux reads only the process's own `/proc/self/mountinfo` (at most 4 MiB and 65,536 entries).
  Mount points are unescaped from proc(5) octal (`\040`, `\011`, `\012`, `\134`). A mount is
  read-only when its per-mount options contain `ro` (covers read-only bind mounts over a writable
  superblock) or its superblock options do. An item resolves to the mount with the longest
  mount-point prefix on component boundaries; several mounts on the same point resolve to the last
  (topmost) entry.
- Only existing item folders count. Each item is resolved with read-only metadata calls
  (`lstat`, cached parent `realpath`, and `realpath` for a symlinked item folder), so a symlink
  from a writable location into a read-only mount is attributed to the read-only mount. Nothing is
  written and no probe file is created. The mount table source is injectable for tests.
- The issue lists up to five read-only mount points with filesystem type, affected item count and
  up to three titles: `Mount containing a series path is mounted read-only: /mnt/tv (nfs4; 3
  series, e.g. A, B, C). …`. Mount sources (which can name servers) are never shown.
- Bounds: 100,000 rows and 16 MiB of path/title text, 2-second resolution budget, 512 itemized
  mounts (excess counted). Any bounded-out item makes a no-finding result a **Notice**
  `mount_check_truncated`, or is appended to the Error.
- An unreadable, oversized or unparsable table, or a non-Linux platform, produces an explicit
  **Notice** `mount_table_unavailable` ("Read-only mount detection is not supported on this
  platform" / "is unavailable: …"), never a healthy observation and never a failed batch.

Evidence: `src/health/library_roots.rs` and `src/health/mounts.rs` unit tests (best-root
derivation and component boundaries, errno classification, bounded runner budget/hung/permit
behaviour, real-format mountinfo samples with `\040`, read-only bind mounts, superblock `ro`,
stacked and nested mounts, malformed/oversized tables, and parsing the host's own
`/proc/self/mountinfo`), and `src/health/library_tests.rs` with owned temporary folders: missing
root issue → restored → cleared in both domains with colliding IDs, aggregation of several items
under one root, multiple-missing aggregation, file/symlink/permission/invalid states, the
collection identity separate from the movie root, item/root/time bounds and the 4096-byte
message bound, injected read-only/writable mount tables for both domains including a symlinked
item, unavailable/unsupported/malformed tables, publication and restoration through the engine,
root-mutation marking, and before/after tree comparisons proving evaluation created and wrote
nothing. `frontend/tests/health-browser.mjs` asserts a real missing TV root (owned
`/owned-collections/tv` seed) renders with its message and help link.

### Not claimed

- Sonarr's empty-root-folder Warning (`RootFolderEmpty`, `#empty-root-folder`) is not
  implemented; it would require enumerating library roots.
- An unmounted mount point whose empty stub folder still exists is reported healthy (it is a
  present folder); one whose stub is missing is reported `missing`, not `unmounted`. Read-only
  detection after an unmount only sees the parent filesystem.
- Upstream's collection checks for blank roots or roots no longer configured are not reproduced:
  the schema's root reference and root deletion make those states unrepresentable here.
- Symlinked roots are reported as unusable by design; this deliberately differs from upstream.
- Mount resolution is lexical on the resolved path against `mountinfo`; overmounting a parent
  after a child mount, mount namespaces other than the process's own, and per-file `chattr +i`
  or ACL write denial are not modelled. Writable mounts that are full or have quota limits are
  not reported. Network filesystems that report `rw` but reject writes on the server are not
  detected (no write probe is made, by design).
- Library item add/move/delete do not invalidate these checks immediately; detection latency is
  up to the six-hour schedule unless a manual run or a root-folder mutation occurs.
- A probe thread stuck on a hung mount cannot be cancelled; at most two such threads per detector
  family are tolerated before evaluations become explicitly unevaluated.
- No notification delivery is implemented for these observations.
