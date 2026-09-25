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
and richer metadata are currently archival only (except the explicit provider reconstruction below). They have not gained runtime
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
