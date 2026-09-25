# Configured root folders

Native endpoints are scoped to `tv` or `movies`:

- `GET /api/v1/{media}/root-folders?limit=50&offset=0` returns a page (maximum 100).
- `POST /api/v1/{media}/root-folders` accepts only `{"path":"/absolute/directory"}`.
- `GET /api/v1/{media}/root-folders/{id}` reads one configured root and observations.
- `DELETE /api/v1/{media}/root-folders/{id}` deletes configuration only, returning 204.

There is no path-edit endpoint. Deletion does not delete/move files, library records
or snapshot mappings. A missing or wrong-domain ID returns 404. Malformed paths,
IDs, bodies and unknown query options return 400; duplicate normalized paths within
a domain return 409. The same path can be configured independently for both domains.
This is not a Sonarr/Radarr wire-compatible route.

Paths are absolute POSIX UTF-8 paths of at most 4096 bytes. Repeated/trailing slashes
are normalized; traversal, control characters, backslashes and `/` itself are
rejected. Native creation requires an existing writable directory. Every component
is opened relative to the preceding directory with symlink following disabled.
Symlink aliases are rejected; this does not detect bind-mount aliases or establish
distributed filesystem ownership. Existing library paths remain independent; adding
a root does not retroactively move or constrain them.

Creation checks writability by creating and removing one uniquely named private
0600 empty probe file through the opened directory descriptor. No existing file is
modified. Failed cleanup is an explicit error. A process crash between creation and
unlink can leave the uniquely named empty probe file; no automatic cleanup is claimed. Request cancellation/timeout cannot
interrupt a blocked kernel filesystem call; the worker may subsequently finish its
probe cleanup even though the caller did not receive success. Creation does not
persist configuration unless validation and response bounds succeed.

## Observations and limits

Only the configured path and domain are stored. Accessibility, write-access checks,
free/total bytes and unmapped folders are fresh, transient observations of this
process's filesystem view. They are not reservations or guarantees for later imports.
`accessible`/`writable` and disk sizes are nullable: unknown is distinct from false.
A missing, inaccessible or symlink-containing path is reported as `inaccessible`;
the API does not guess a more specific cause from the failed safe open.

`observation` is `available`, `inaccessible`, `busy`, `timeout` or `limited`.
`unmapped_folders: null` means no complete scan result; `[]` means a completed scan
found no unmapped directories. A scan failure cannot be mistaken for an empty root.
Disk-space failures leave the corresponding numbers null. Returned integer byte
counts fit JavaScript's safe integer range.

Observations run on blocking workers, with two workers process-wide, a five-second
request budget and cooperative deadline checks. Timed-out/cancelled workers retain
their semaphore permits until they actually finish. A busy observation returns
unknown fields; it does not queue unbounded work. Each root inspects at most 10,000
entries and bounds accumulated names; complete serialized responses are capped at
1 MiB. Same-domain library-path lookup is capped at 10,000 items. If this lookup
cannot complete within its bound, the API returns `root_library_limit` rather than
misclassifying folders. Deletion does not need filesystem or library enumeration.

The scan includes immediate child directories only. Same-domain library paths are
excluded after lexical normalization. Child symlinks are not followed. Known special
directories (including `.grab`, `lost+found`, recycle/system folders and common NAS
metadata directories) are excluded; movie scans also exclude dot-hidden folders.
A concurrent external directory change may invalidate a scan; no filesystem snapshot
or external-actor lock is claimed.

## Snapshot mapping

For supported Sonarr 233 and Radarr 206/242 snapshots, `RootFolders.Id/Path` are mapped
transactionally into scoped configuration using existing application/fingerprint/entity
mappings. No source or destination root is accessed during this import: unavailable
mounts may be configured without pretending they were observed. Extra source fields
are retained privately and reported unsupported. At most 1000 source root rows are
accepted per upload. Existing archived snapshots can be replayed to add roots.

Dry runs and late failures roll back roots, core records, archives and mappings
together. Exact replay must find the mapped ID with matching domain/path; local
edits or deletion cause conflicts. Recreating the same path after deletion gets a
new ID and does not silently repair the old mapping. New snapshots reconcile by
domain plus normalized path. Snapshot imports retain their existing private-database
requirements and never modify source bytes or media.

## Evidence and remaining scope

`tests/root_folders.rs` covers real HTTP CRUD for both domains, normalization,
observations, unmapped exclusions, unavailable roots, symlink rejection, configuration-only
deletion, offline snapshot mapping, replay conflicts and rollback. Module tests cover
busy/deadline/scan-bound semantics. The schema regression covers actual 13→14 migration,
transactional DDL rollback, existing data preservation, constraints and reopen.

This delivers the configured-root prerequisite. Full `RootFolders` parity remains
incomplete: naming-dependent deeper traversal, configured recycle-bin exclusions,
platform-specific system/hidden/network-drive policies, Windows/UNC paths and live UI
updates are not delivered. Tests use temporary Linux directories; no network mounts,
kernel-stall cancellation, power loss, real exported backups or live media services
were exercised. Future consumers must still validate mounts, permissions and file
ownership at their own operation boundaries.
