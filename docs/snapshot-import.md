# Importing Sonarr and Radarr SQLite snapshots

`POST /api/v1/migrations?application=sonarr&dry_run=true` accepts the raw bytes of
an exported, consistent SQLite backup (`Content-Type: application/octet-stream`).
`application` is required: `sonarr` or `radarr`. `dry_run` defaults to `true`.
Use `dry_run=false` with the same upload to apply it. This route does not accept
server paths, ZIP archives, WAL sidecars, PostgreSQL dumps or live databases.

Set `HRRDARR_URL` to the URL of your isolated hrrdarr instance. Do not point it at
a running Sonarr, Radarr, Readarr or other media service.

```sh
curl --fail-with-body -X POST \
  -H 'Content-Type: application/octet-stream' \
  --data-binary @sonarr-backup.db \
  "$HRRDARR_URL/api/v1/migrations?application=sonarr&dry_run=true"
```

Use the source application's exported database backup. If that *isolated backup*
is in WAL mode, open the isolated backup with SQLite and set `PRAGMA journal_mode=DELETE`
before uploading it. Do not change journal mode on the live source, copy a live
main database without its committed WAL, or discard a required WAL. The importer
rejects WAL headers; a valid DELETE-mode header alone cannot prove how a backup
was created. The operator must supply a consistent export.

## Supported contracts

- Sonarr schema **233**: `Series`, serialized `Seasons`, `Episodes`, `EpisodeFiles`.
- Radarr schema **206**: inline metadata in `Movies`, plus `MovieFiles`.
- Radarr schema **242**: `MovieMetadata`, `Movies.MovieMetadataId`, `MovieFiles`.

These are exact `VersionInfo` versions, with required columns validated separately.
Other versions are rejected, even if their fields look similar. The layout boundary
is Radarr migration 207; the 242 reader follows the explicit metadata foreign key.
Requirements come from Sonarr `76c684e097f16ac216e6213845e5cac372774995` and Radarr
`c90668a520664ad0c91812cfee57c41928ad2148`. Tests use independent synthetic fixtures;
this is not runtime equivalence testing against either application.

Imported active fields are external identity, title/year, library path, monitoring
(including TV seasons/episodes), file associations/paths, and movie file edition.
One TV file can serve several episodes. Movie catalog metadata without library
membership is retained as catalog metadata. Only absolute POSIX roots and relative
POSIX file paths without traversal are accepted. Windows path conversion and remote
path mappings are not guessed. File ID zero/null means no association; positive IDs
resolve through file records and their correct owner, never into path strings.

## Reconciliation and retention

HTTP 200 returns `application`, `fingerprint` (SHA-256 of upload), `schema_version`,
`dry_run`, `applied`, `mapped`, `duplicates`, `metadata_backfilled`, `conflicts`, `missing_file_records`,
`unsupported` and a `policy` explanation. `mapped` counts new proposed records;
`duplicates` counts matching existing records. With any conflict, `applied=false`
and **the entire destination transaction rolls back**, including otherwise valid rows.
A dry run always rolls back. Existing rows are compared across all imported fields;
no existing library is cleared or overwritten. Reruns validate stored mappings and
conflict if local records were edited or deleted. Numeric source IDs are scoped by
application, upload fingerprint and destination entity; destination IDs are allocated
independently. Uploading a different backup reconciles by domain identities/paths.

`missing_file_records` counts references to absent *source database file records*.
Such references become absent associations and the original raw data remains retained.
It does **not** count absent media on disk: the importer never reads library paths or
media. Mounts, permissions, availability of media, and path mappings are unverified.

`unsupported` lists tables/columns retained but not applied. All source table rows,
including extra fields on mapped rows, are archived as typed JSON. This preserves
SQL nulls and binary values as hex. Profile assignments, custom formats, tags,
collections, history, providers, path mappings, list exclusions, unknown settings,
and richer metadata are archival only except the explicit custom-format, profile, History, provider, root-folder and path-mapping reconstructions below. They have not gained runtime
semantics by being retained. There is no archive-reading API.

Archives can contain credentials. Import requires a local Unix destination database
with no group/other permission bits; remote and non-Unix destinations are refused.
Staging directories/files use 0700/0600 and are removed after reading. Provider
credentials remain inactive: no clients, sessions, jobs, downloads or external
service state are activated or resumed. Reports/errors contain no source field values;
source and destination backups must be protected as credential-bearing material.
No source database or media bytes are modified.

## Limits and errors

Uploads are limited to 32 MiB, 128 tables, 100,000 total rows, 256 columns per table,
1 MiB per text/blob cell and 64 MiB serialized archive payload. One snapshot import
runs at a time per process. A concurrent import is rejected and can be retried.
Source parsing runs on a blocking worker. SQLite opens the staged copy read-only,
with trusted schema disabled. Views, triggers, virtual tables and generated/hidden
columns are rejected. Ordinary, partial and expression indexes are accepted, but
row scans do not use them. `quick_check` validates database structure with CHECK
expression evaluation disabled; index consistency and arbitrary source CHECK
constraints are not verified. Adapter checks and destination constraints independently
validate the imported contract.

Malformed query options or rejected uploads/import attempts return HTTP 400; an
oversized HTTP body returns 413. Errors are static, without SQL/source values. A
conflict is a successful reconciliation report (200, `applied=false`), not a partial
write. A database rollback failure requires operator integrity inspection before
retrying. Read the report's `applied` field, not just the HTTP status.

## Verification limits

`cargo test --locked --test snapshot_import` covers both versions/layouts, combined
ID collisions, dry runs, replay, local conflicts, source file records/sentinels,
paths, inactive raw retention, secret-safe reports/errors, late-write rollback,
source-byte preservation, hostile schema and private-file requirements. Handler
coverage exercises uploaded bytes and preview/application results.

No real exported application backup, browser flow, Windows path, remote database,
external provider, actual media scan, or crash/disk-failure injection is claimed.
Ancillary settings still require semantic adapters before full snapshot parity.

Episode metadata added in schema 5 can be activated by exact replay of a previously
imported snapshot. A one-time fill requires intact mapped core fields and all newly
supported fields still null; it never overwrites local edits. `metadata_backfilled`
counts affected episode records. Dry runs roll back both data and activation marker.
See [episode API](episode-api.md) for date, numbering, cover-field and replay rules.

## Optional provider reconstruction

`import_providers=true` explicitly reconstructs **Torznab, Newznab and qBittorrent**
configuration while importing the same supported Sonarr/Radarr snapshot versions.
The default is `false`, which preserves the previous archival-only behavior and
requires no provider key. `dry_run` still defaults to `true`.

Reconstructed providers are always **disabled and untested**. Review unsupported
fields before enabling them through the native provider API. The importer never
contacts an indexer/client, starts a session, submits a download, resumes a job or
imports external queue/test state. Source enable flags, indexer automation policies,
tags, seed criteria and cleanup policies remain archived and explicitly reported
as unsupported. Unknown provider implementations remain entirely archival.
Unknown JSON property names are reported through a static settings marker rather
than echoing potentially private names or values.

Mapped configuration includes names/priorities; indexer endpoint/API path,
media-specific categories, anime-format/movie-year flags and additional parameters;
qBittorrent endpoint, scoped categories/priorities, initial state, content layout,
sequential/first-last-piece settings and TV tag forwarding. Missing JSON properties
use the pinned source defaults: indexer `/api`, TV categories 5030/5040, movie
categories 2000/2010/2020/2030/2040/2045/2050/2060, qBittorrent localhost:8080 and
`tv-sonarr`/`radarr` categories, started/default-layout and false flags. Explicit
invalid values are rejected, not silently converted to defaults. Bare IPv6 hosts
are bracketed. Additional parameter percent encoding must represent valid UTF-8;
malformed encoding is rejected rather than changing credential bytes.

API keys, additional query parameters and username/password pairs are encrypted
with `HRRDARR_PROVIDER_KEY` using the existing native credential format. A key is
required only when the reconstructed configuration contains credentials; a wrong
key on replay fails closed. Credential-free trusted clients remain credential-free.
Supplying both bearer and username/password credentials is rejected instead of
choosing one silently. Original credential-bearing source rows remain in the private
archive, so the database and its backups still need owner-only protection. Key
custody remains separate from database backups; see [provider configuration](provider-config.md).

Provider mappings use application + snapshot fingerprint + source table + numeric
source ID, with a UUID destination and captured revision. Equal Indexers and
DownloadClients IDs remain distinct, as do TV/movie imports. Existing archived
snapshots can be replayed with the option to add these mappings. Exact replay
requires the mapped provider to exist with unchanged revision, complete configuration,
plaintext credential content, disabled state and no test observation. Local changes,
deletions or testing cause conflicts; nothing is overwritten or silently recreated.

For a different backup, implementation + exact endpoint + media domain identify
candidate configurations. No candidate creates a new disabled configuration. If a
candidate exists, exactly one must match every reconstructed field and decrypted
credential; otherwise the import reports a conflict. Independent
TV/movie configurations are not automatically merged. Multiple configurations at
the same endpoint within a domain may therefore require explicit reconciliation,
even when they have different names or credentials. This conservative rule prevents
ambiguous ownership and duplicate configuration creation.

All core records, providers, scopes, archives and mappings share the import
transaction. Any conflict or late error rolls everything back; dry runs persist
nothing. At most 256 total source indexer/client rows can be reconstructed per upload,
including unsupported implementations. No source database or media is modified.

Evidence: `tests/provider_snapshots.rs`, provider adapter unit tests and
`provider_snapshot_mapping_upgrade_rollback_and_reopen` in `tests/schema_migrations.rs`
cover synthetic both-app/version imports, source-table collisions, defaults,
credential privacy/key failures, disabled state, exact/new-backup replay, local
edits/deletions, late rollback and reopening. These do not establish real exported
backup compatibility, external-service equivalence or full ancillary snapshot parity.

Configured `RootFolders.Id/Path` are also mapped for both applications, scoped by
media domain and reconciled by normalized path. This performs no filesystem access:
missing paths remain unobserved declarations. See [root folders](root-folders.md)
for supported path boundaries, replay/deletion conflicts and verification limits.

`RemotePathMappings` is reconstructed offline for both applications using canonical
host/path identity and revision-aware replay protection. Source IDs determine initial
first-match order; incompatible existing forward/reverse overlap precedence causes an
atomic conflict. See [remote path mappings](remote-path-mappings.md) for the exact
normalization, reconciliation, supported path syntax and native preview boundary.

Source History is activated independently from native import receipts for Sonarr 233
and Radarr 206/242. A fact retains `(application, fingerprint, source_id)`, its resolved
TV episode or movie target, original numeric event code, semantic event name, UTC
date, and supported title/download ID/quality/language fields. It never invents a
local operation, file association, path, transfer size or hash. Historical file IDs
are not resolved to today's files. `GET /history` tags these facts as
`source_snapshot`; native association commits remain `native_import`.

Sonarr event codes 1–7 map respectively to grabbed, series-folder import,
download-folder import, failed download, file deletion, file rename and ignored
download. Radarr supports 1, 3, 4, 6, 7, 8, 9 respectively as grabbed,
download-folder import, failed download, file deletion, movie-folder import, file
rename and ignored download. Code zero and unknown/deprecated codes remain archived
and reported, with no active fact. Missing targets are likewise archived/reported;
a TV History row whose SeriesId contradicts its source episode fails validation.

Dates accept source SQLite UTC values or explicit RFC3339 offsets. Canonical storage
uses `YYYY-MM-DD HH:MM:SS` plus an optional fraction of at most nine digits, with
trailing zeros removed. Invalid dates, leap seconds and excess precision fail the
whole import. Missing/null optional fields remain absent; language `[]` remains an
empty list, while null elements normalize to the source unknown language ID zero.
Known quality/language catalogs are validated using the existing native validators.
Unsupported catalog IDs remain private and are reported without their values;
malformed JSON and duplicate language IDs fail validation. Quality catalog membership
is checked on replay too; these catalogs are currently fixed reference data.

Arbitrary `Data`, unknown columns and unsafe download identifiers remain private raw
archives and appear only as static unsupported field/count reports. Public download
IDs require 1–256 ASCII letters, digits, dots, underscores, colons or hyphens; URL-like
values are omitted. Source titles are bounded to 1024 bytes. No raw Data or source
credential values are returned through History.

Migration 18 adds immutable source facts and a History activation version to each
snapshot provenance. It does not activate old archives by itself. Re-uploading the
exact original backup validates and backfills a schema-17 archive atomically; later
exact replays compare every supported fact and create nothing. A missing previously
activated fact or changed mapped target causes a conflict, never silent repair.
Different backup fingerprints retain separate historical provenance, even for the
same upstream History ID; cross-backup event deduplication is not claimed. All source
facts, native core mappings, archives and optional provider reconstruction share the
same transaction, including dry-run rollback.

Requirements were checked against the pinned `History/EpisodeHistory.cs` (Sonarr),
`History/History.cs` (Radarr), and both `Datastore/Converters/UtcConverter.cs` contracts.
Independent implementation evidence is `tests/snapshot_history.rs`, the schema-17
backfill/rollback/reopen test and mixed-page query-plan test in
`src/db/history_tests.rs`, and `tests/history_api.rs`. These use synthetic snapshots;
real exported backup compatibility, unsupported History payload semantics, source
History mutation endpoints and complete ancillary snapshot parity remain unverified.


## Quality profile reconstruction

Whole representable quality profiles and their library assignments are reconstructed
in the same snapshot transaction. Sonarr 233 reads `QualityProfiles` and
`Series.QualityProfileId`; Radarr 206 reads `Profiles` and `Movies.ProfileId`, while
242 reads `QualityProfiles` and `Movies.QualityProfileId`. Radarr migration 230
renamed these fields. Version 206 predates `MinUpgradeFormatScore`; its value is
normalized to 1 using the explicit default introduced by migration 239. Sonarr 233
and Radarr 242 require their stored value. No other policy defaults are invented.

Eligible profiles must have fully representable names, ordered quality/group items,
allowed flags, TV size overrides, upgrade/cutoff/score policy and movie language.
Serialized items accept source camelCase field names case-insensitively, numeric
quality identities, and omitted or zero leaf IDs. Source group IDs identify only
that profile's root groups. Cutoff must identify exactly one allowed root quality
or group; ambiguous identities are unsupported. The writer resolves groups through
native request positions and allocates destination IDs inside the import transaction.

The source must include a `CustomFormats` table. Definitions use stored `{type, body}`
wrappers and profile scores use numeric `{format, score}` references. Supported whole
definitions are validated by the native domain/regex validators before the writer
transaction; source IDs are remapped before complete profile persistence. Every
reference, including score zero, must resolve. Unknown conditions/policy fields,
unsupported regex, invalid values or unresolved references leave the whole affected
definition/profile archived and reported. Malformed serialized JSON and duplicate
source identities abort the import. Reports contain static reasons, never source
field values. Worker admission/timeouts abort with a retryable explanation rather
than permanently classifying a valid definition as unsupported.

Current-domain definitions absent from a profile contribute implicit zero scores.
Canonical comparison ignores zero scores only after validating all source references;
nonzero local additions or changes still conflict. Native catalog/specification bounds
apply, including 128 definitions per domain and the aggregate byte budget.

Profile names are candidate keys only within a media domain. An existing candidate
must match the complete canonical ordered graph and policy, including sizes,
language and thresholds. An unconfigured native `policy: null` is not equal to an
imported policy. Same-name differences conflict without overwriting either profile;
matching defaults are never inferred from their labels. TV/movie profile IDs remain
independent even when source IDs and names are equal.

Migration 20 adds a profile activation version to snapshot provenance. It leaves
existing archives inactive until an exact re-upload. Backfill first requires intact
library-settings mappings and existing null assignments, then verifies core/settings
through normal reconciliation before attaching the resolved profiles. A missing
mapping/row or a local nonnull assignment conflicts and survives unchanged. Later
exact replays require intact mapped profiles with equal graph/policy and the expected
assignment; edits, deletions or missing mappings are not silently repaired. A new
fingerprint can reuse an exactly equal profile; inconsistent existing library
settings cause a conflict. Unsupported profiles never clear local assignments.

Native profile validation and persistence are shared through connection-aware
helpers, so the snapshot writer never opens a nested transaction or calls an API
handler. Profiles, assignments, core rows, private archives, History and optional
provider reconstruction commit or roll back together. Dry runs persist nothing.
Uploads allow at most 256 profile rows and each profile at most 64 total group/quality
nodes; oversized row counts fail explicitly and unsupported graphs stay archived.
Quality catalog membership is loaded once per import domain for adapter validation.

Evidence: `tests/profile_snapshots.rs` covers synthetic Sonarr 233/Radarr 206/242
activation, both-domain policy readback, unsupported/privacy cases, replay, local
edits and late-write rollback. `src/db/snapshot_profile_tests.rs` constructs actual
schema-19 archives through the prior core writer, checks migration rollback, performs
both-domain exact-upload backfill and tests missing mappings/rows, local assignments
and reopen. These do not establish real-backup equivalence or the complete snapshot parity gate.

## Managed source Blocklist

Sonarr 233 and Radarr 206/242 `Blocklist` records can now be reconstructed for
native blocklist reads and removal. This is source-backed management, not evidence
of failed-download production, matching or decision enforcement. TV records retain
the complete remapped episode set within their series (including an explicitly
empty set); movies reference library membership separately. Unresolved or cross-series
episode references archive/report the whole record, never a narrower target set.

Only validated title, canonical UTC dates, size, protocol, domain quality/revision
and languages enter the public projection. Message, indexer, source, torrent hash,
flags, release type and unknown columns remain in the private raw archive and are
reported unsupported. Unsupported protocol/quality/language semantics leave the
whole record inactive. Malformed known JSON, duplicate identities, invalid dates or
negative sizes fail the import transaction. Missing optional facts remain absent.
The adapter permits at most 10,000 entries and 100,000 total episode links per upload,
with at most 10,000 episodes per entry; existing snapshot byte/row limits also apply.

Identity is `(application, snapshot fingerprint, source ID)`. Immutable normalized
facts and exact destination links are checked on replay. Explicit single/bulk removal
removes the active entry and retains a provenance tombstone; reuploading those same
bytes never reactivates it. Removing library membership likewise tombstones its active
entries. Removing an individual referenced episode is restricted so an entry cannot
silently lose part of its target set. A different fingerprint is a new source attestation
and may introduce another record even with the same source ID or title.

Migration 22 preserves existing archives and does not activate their contents by
itself. Reuploading the validated original backup backfills schema-21 archives once;
missing or changed activated facts conflict rather than being silently repaired.
Dry runs roll back the entire reconciliation, and late failures roll back core mappings,
archives and blocklist activation together. Source databases and media remain untouched.
`tests/blocklist_snapshots.rs` and `src/db/blocklist_tests.rs` cover synthetic both-domain
mapping, schema-21 backfill, rollback, complete target ownership, deletion/replay and
reopen. Live services, real operator backups and full Blocklist controller parity are
not established by these fixtures.

An episode-file row retained as a quarantined replacement artifact is no longer an active snapshot reconciliation candidate. Exact reuploads and matching source records report a conflict rather than restoring its old path or episode association. Its original provenance remains stored. This exclusion is specific to explicitly quarantined replacement rows; ordinary unassociated snapshot files and shared files remain supported.


## Custom-format activation

Migration 36 adds an independent custom-format activation marker without changing
existing profile activation. Opening an old destination does not activate archives.
Reuploading identical schema-35 source bytes can reconstruct definitions and profiles
previously rejected because the source CF catalog was nonempty, including profiles
with empty score arrays. This exception does not recreate deleted profiles that were
already supported with an empty source catalog. Backfill requires intact core mappings
and still-null assignments; definition/profile changes, deleted mapped rows and changed
assignments conflict. Dry-run, conflicts and late failures roll back definitions,
scores, mappings, assignments and activation markers together.

`cargo test --locked --lib snapshot_profile_tests` covers synthetic three-layout graph
mapping, schema35 upgrade/backfill/reopen, rollback, zero-score equality, local edits,
unsupported definitions and unresolved references. Regex compatibility remains the
bounded native subset. No real backups, filesystem/media validation or full ancillary
snapshot preservation is claimed. New state is one activation column; no dependency
is added.
