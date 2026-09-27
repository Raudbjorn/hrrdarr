# Native settings atomicity

`per.003` concerns the whole read/modify/write boundary, including related rows.
The persistence review's XML test applies here as a **storage-independent
requirement**: independent changes survive; an unsuccessful mutation leaves the
previous complete configuration. hrrdarr has no XML configuration writer.
[Startup](../src/main.rs) reads deployment environment variables (database,
bind address, provider key); it never edits those variables or a `config.xml`.
The currently implemented mutable settings below live in libSQL. Adding an XML
writer solely to reproduce the review's test would introduce a second authority.

## Mutation inventory

This inventory is source-verified, including callers of helpers that receive a
`Connection` rather than owning a transaction. `Immediate` means the write
transaction starts **before** reading mutable stored state. Success commits only
after all related writes and response construction; failure rolls back. An SQL
statement is itself atomic, including its foreign-key actions and triggers.

| Surface and actual entry points | Atomic mechanism and concurrency contract | Regression evidence |
| --- | --- | --- |
| [Provider config](../src/providers/mod.rs): `create`, `update`, `delete`, `write_scopes`, `credentials` | Immediate transaction contains parent, encrypted credentials, both domain scopes and revision. Update reads old secret/config inside it and compares revision; stale replacement is 409. Omitted credentials preserve the stored value; null explicitly clears. Scope replacement is never a separate commit. Delete includes dependent rows. | `settings_atomicity`: simultaneous same-revision replacements have one 200/one 409, coherent TV/movie scopes and exact decrypted winning secret; orderly reopen. `provider_config_api`: late movie-scope failure rolls back parent, ciphertext and revision. |
| [Provider bulk administration](../src/providers/administration.rs): `update`, `delete`, `selected` | Immediate transaction around the entire selection, old values, revision checks and all mutations. Unspecified bulk fields derive from values read inside the transaction. | `provider_administration_api`: stale member and late failure cannot partially mutate a batch. |
| Provider test observations: `record_test` | One `INSERT … SELECT … WHERE revision … ON CONFLICT … UPDATE`; an observation for an obsolete config cannot overwrite the current revision's test state. Draft/category/preset endpoints do not save configuration. | Provider configuration/protocol/administration suites. External test execution is separately revision-checked; the network operation is not a settings transaction. |
| [Quality profiles](../src/quality_profiles.rs): `persist` → `persist_on_connection`, `insert_leaf`, `insert_policy` | Immediate transaction covers old profile/policy validation, parent update, deletion/replacement of groups/items/policy and final read. Full replacement is intentionally last successful writer, not a field patch or a revisioned API; competing requests cannot create hybrid parent/children. | `settings_atomicity`: both successful concurrent replacements, discriminating group/quality/policy values, late policy insertion failure and reopen, separately for TV/movies. `profile_policy`, `qualities` cover existing contracts. |
| [Quality definitions](../src/qualities.rs): `edit`, `reset`, `reset_definitions`; [quality reset command](../src/commands/quality_reset.rs): `run` | Immediate transaction for a definition batch plus TV profile-item propagation; reset callers supply a transaction. Command reset and command completion commit together. Updates are full submitted definition values, not unprotected reads of old definitions. | `qualities`: both-domain edits, late batch rollback, persisted values; `profile_policy`: TV propagation; quality-reset module tests. |
| [Naming](../src/naming/mod.rs): `put_tv`, `put_movies` | Immediate transaction and single full-row `UPDATE … WHERE revision=?`, revision increment and response read; stale request is 409. No config cache is published before commit. | `settings_atomicity`: one complete winning request and one conflict per domain, then reopen; `naming_api`: full replacement, null/omission validation, stale revision. |
| [Root folders](../src/root_folders.rs): `create`, `delete` | Create performs filesystem observation first, then Immediate transaction with unique `(media_type,path)` insertion. Duplicate is a conflict. Delete is a single scoped statement. No read-derived replacement or root-edit endpoint exists. Observations do not promise the mount remains available. | `root_folders`: both-domain create/duplicate/config-only delete; snapshot late rollback. |
| [Remote path mappings](../src/remote_paths.rs): `write`, `delete` | Immediate transaction includes duplicate lookup, normalized mapping insert or revision-CAS replacement and response read. Delete is one revision-CAS statement. Preview/resolve are read-only. | `remote_paths`: both-domain revision/conflict/normalization, snapshot order and rollback. |
| [Library settings](../src/library.rs): `create`/metadata `selected` → `create_record` → `patch`; `update`/`bulk`/`editor` → `persist` → `patch` | Every caller owns an Immediate transaction before identity/profile validation or old season-monitor reads. A patch updates only explicitly supplied columns; missing differs from null. Settings, parent monitoring, season monitoring and episode propagation commit together. Metadata requests finish before the transaction; refresh helpers preserve user settings. | `settings_atomicity`: simultaneous independent field changes both return 200 and survive in each domain; failure on parent monitoring after settings update rolls everything back; reopen matches exact prior response. `library_api`, `library_refresh`, `library_snapshots`. |
| Adjacent user edits: [episode monitoring](../src/episodes.rs) `set_monitoring`, [file quality/language/edition](../src/media_files.rs) `persist` | Immediate transaction around selected IDs, validation and all changed columns; omitted file fields remain untouched. These are library facts rather than global settings but use the same boundary. | `episodes`, `episode_api`, `media_file_api`. |
| [Download refresh schedules](../src/commands/mod.rs): `schedule`, `delete_schedule`; [RSS schedules](../src/commands/rss.rs): `save_schedule`, `delete_schedule` | Immediate transaction reads old schedule/revision and current provider revisions before upsert. Stale revision conflicts; delete is a single revision-CAS statement. Both `schedule_due` helpers are called only by [worker `claim`](../src/commands/worker.rs), whose Immediate transaction includes due reads, enqueue and schedule advancement/disable. They are not unprotected read/modify/write helpers. | `commands_api`, `rss_commands`: domain scopes, stale revisions, provider changes and restart. |
| [Download processing policy](../src/commands/processing.rs): `save_policy` | Immediate transaction reads existing scoped policy and provider revision, compares policy revision, then upserts and reads response. Disabled implicit default is checked inside the same transaction. | `download_processing`: both-domain policies and persisted processing/restart behavior. |
| [Snapshot import](../src/snapshots/mod.rs): `import`/`import_with_providers` → `import_inner` → profile `prepare`, common `write`, profile `finish`, provider `write` → `import_configuration` | One Immediate destination transaction surrounds all reconstructed settings: profiles/items/policies/assignments, library settings/seasons, roots, mappings, providers/scopes/secrets, source archives and provenance. Helpers never independently commit. Dry run or conflict rolls back. No active naming/scheduler settings reconstruction exists; unsupported source rows remain private archives. | `profile_snapshots`, `provider_snapshots`, `library_snapshots`, `root_folders`, `remote_paths`: both references, collision/idempotency and late rollback. |
| [Database migrations](../src/db/mod.rs) | Ordered migration transaction covers seed/default settings and schema versions/checksums. Changes to persistent settings representation belong in this mechanism. | `schema_migrations` and module migration tests. |

## Executable concurrency and recovery check

```sh
cargo test --locked --test settings_atomicity
```

The three tests use owned scratch databases and `127.0.0.1:0` listeners, independent
HTTP clients/connections, a bounded start barrier and a four-thread runtime.
They exercise actual API handlers; no private helper is substituted for the
write path. Injected SQLite `RAISE(ABORT)` failures occur after earlier writes,
not merely during request validation. All API/server/database owners are dropped
before reopening the same path. Provider encryption is independently checked
against the persisted v1 authenticated envelope, including exact secret value.
Nothing contacts a live provider or changes deployment state.

Atomicity does not imply merging two full replacements. Provider and naming
clients must reload after 409. Profiles and definitions deliberately accept
complete replacements without revision fields: the later complete value wins.
Library patches preserve unrelated fields because they never rewrite omitted
columns. This distinction is part of the native API contract.

## Durable contracts and limits

Existing migration versions/checksums, provider `settings_version=1`, credential
envelope version/AAD, explicit serialized enums, typed media domains, timestamp
representations and command names remain unchanged. The tests use the supported
wire fields and exact persistent credential format; this work introduces no new
settings representation, dependency, cache, host-config authority or schema.
Future changes to these formats need a migration/compatibility decision, not an
incidental serializer rename. See [API contract](api-contract.md),
[quality API](quality-api.md) and [naming API](naming-api.md).

Observed evidence is local libSQL, controlled SQL failure and orderly reopen.
It does **not** prove abrupt-kill/power-loss durability, every interruption timing,
remote/Turso transaction behavior, filesystem mount stability, or runtime parity
with either upstream application. There is no XML output to validate. Missing
host/general settings and unimplemented provider families remain separate parity
work; this audit covers every existing native settings write surface, not future
ones. Database atomicity also does not make downloads, media moves or notifications
transactional; those have separate durable-operation requirements.
