# Remote path mappings

Native TV and movie configuration is independent. The same host and remote prefix
can map to different local directories in each domain.

- `GET /api/v1/{media}/remote-path-mappings?limit=50&offset=0` lists mappings (maximum 100).
- `POST /api/v1/{media}/remote-path-mappings` accepts `host`, `remote_path`, and `local_path`.
- `GET /api/v1/{media}/remote-path-mappings/{id}` returns one mapping.
- `PUT /api/v1/{media}/remote-path-mappings/{id}` accepts the full configuration plus its current `revision`.
- `DELETE /api/v1/{media}/remote-path-mappings/{id}?revision=1` deletes configuration only.
- `POST /api/v1/{media}/remote-path-mappings/resolve` accepts `host`, `path`, and `direction` (`remote_to_local` or `local_to_remote`).

Here `{media}` is `tv` or `movies`. Responses include native IDs and revisions.
Updates increment revision; stale updates/deletes and duplicate canonical host/remote
prefixes return 409. Wrong-domain detail returns 404; malformed inputs and unknown
fields/query options return 400. Bodies are capped at 16 KiB and serialized responses
at 1 MiB. This is a native API, not V3 wire compatibility.

Hosts are canonical DNS/IDNA or IP addresses, compared without case; ports, schemes,
credentials and URL paths are rejected. Paths are bounded to 4096 UTF-8 bytes.
Remote paths support absolute POSIX paths, Windows drive roots, UNC roots, and mixed
Windows separators. Relative POSIX/Windows prefixes can be configured for reverse
translation; as in the references, forward containment requires both paths rooted,
so relative forward inputs remain unchanged. Traversal, control characters, drive-relative paths and Windows
alternate-data-stream/device syntax are rejected. Repeated/trailing separators are
normalized. POSIX comparison is case-sensitive; Windows comparison uses componentwise
Unicode lowercase, preserving original suffix bytes. Exact .NET invariant casing
for every Unicode character is not claimed.

Matching requires a complete component prefix, so `/downloads` does not match
`/downloads2`. The **lowest native ID wins**, in both directions; this deliberately
implements first-match ordering rather than longest-prefix ordering. No match returns
the original input unchanged, including empty input. Resolution reads at most 1000
mappings per host/domain and fails explicitly above that bound.

Local paths are dedicated absolute POSIX directories, excluding `/` and Linux system
paths `/bin`, `/boot`, `/lib`, `/sbin`, `/proc`, `/usr/bin` and their descendants.
Native create/update verifies existence using bounded descriptor-relative no-follow
traversal; it does not require writability, enumerate children, or create a probe.
The shared filesystem worker pool retains its permit until a timed-out kernel call
actually ends. Unavailable directories, timeout and worker saturation return 422.
A saved mapping is configuration, not evidence that a mount remains available.

## Provider and import boundary

`POST /api/v1/providers/{id}/path-preview` accepts `media_type` and `remote_path`.
It requires a qBittorrent provider with that scope, derives the host from its validated
endpoint, and resolves mappings in the same read transaction as provider configuration.
It returns provider ID/revision, input/output, matched mapping ID/revision (or null),
and `lexical_only: true`. Disabled configurations can be previewed; no credentials,
network request, client submission or filesystem operation is involved.

The resulting path can be supplied to the existing typed manual-import preview.
That preview still enforces source identity, no-follow traversal, target ownership,
and destination constraints. Translation alone grants no filesystem authority.
Automatic polling/download-to-import consumption remains part of the pending durable
pipeline; this endpoint does not bypass durable client submissions.

## Snapshot reconstruction

Supported Sonarr 233 and Radarr 206/242 snapshots reconstruct `RemotePathMappings`
offline in the existing import transaction. Missing local mounts do not prevent
configuration reconstruction. Unknown columns remain reported and privately archived.
Source IDs are application/fingerprint/table scoped. Rows are processed by ascending
source ID; native IDs preserve that order when first inserted into an empty destination.

Exact replay and exact configuration reconciliation do not overwrite destination
configuration. Local revisions, deleted/recreated mapped IDs, changed configuration,
or conflicting host/prefix identity cause whole-import conflicts. Existing overlapping
records outside the imported set also conflict conservatively. If an existing native
ID order reverses the relative source order of any forward or reverse overlapping
pair, import conflicts instead of silently changing precedence. Nonoverlapping existing
configuration is compatible. Native IDs are never reordered. Dry-run and late errors
roll back configuration, source mappings and archive records together.

## Evidence and limits

Requirements were inspected independently in both pinned `RemotePathMappingController`,
resource/service and `RemotePathMappingServiceFixture` sources (Sonarr `76c684e`,
Radarr `c90668a`). Native implementation is independent; inspection is not runtime
reference equivalence. Focused synthetic tests cover CRUD/CAS/domain isolation,
forward/reverse prefixes, Windows/UNC/Unicode paths, real safe manual previews in
both domains, snapshot precedence/replay/rollback and schema-14 upgrade/reopen.

No live clients, production databases or media were used. Windows local filesystem validation, every platform-specific system-folder policy,
V3 wire compatibility, automatic queue integration, and settings UI are not claimed.
New persistent state is schema-15 mappings and revision enforcement; no dependency
was added. Mapping deletion never deletes media or library rows.
