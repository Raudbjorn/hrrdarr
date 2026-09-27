# Storage engine startup contract

`Database::open_local` and `Database::open_remote` inspect the connected engine before schema inspection, backups or migration. The five-second read-only preflight records `sqlite_version()` and sanitized `sqlite_source_id()` through `Database::storage_engine()`. A failed probe returns a downcastable `StorageCompatibilityError` with a fixed capability name and optional validated identity; raw remote errors/URLs/tokens are excluded. Modified amalgamation `alt1` identities are accepted; unrecognized source IDs become `unavailable` without rejecting an otherwise supported engine.

The SQL floor is SQLite 3.38.0 because persisted commands use `unixepoch()` ([3.38 release notes](https://www.sqlite.org/releaselog/3_38_0.html)). This includes the earlier `RETURNING` and `ALTER TABLE DROP COLUMN` syntax ([3.35 release notes](https://www.sqlite.org/releaselog/3_35_0.html)). A version alone is insufficient: startup executes structural JSON and UTC timestamp expressions used by application queries and migrations. Foreign-key enforcement and integrity checks remain mandatory through the existing connection/migration path. The guard does not create test tables or write data before the migration backup.

The resolved libsql-ffi 0.9.30 builds its bundled engine (`build.rs` and `bundled/src/sqlite3.c`, SQLite 3.45.1 with a modified source ID). Updating a system SQLite package does not change that engine. The pinned Radarr native-Linux 3.9.0 package alarm therefore is not the hrrdarr mechanism; compatibility belongs to the actual engine reached by the driver.

Evidence:

- `tests/libsql_capabilities.rs`: actual bundled engine foreign keys, `RETURNING`, add/rename/drop DDL, transactional rollback and integrity.
- `tests/schema_migrations.rs`: actual application fresh/upgrade, migration rollback and ownership contracts.
- `tests/storage_engine.rs`: local startup identity and a recorded, owned loopback Hrana fixture exercised through **real `open_remote`**. It checks accepted SQL floor/modified identity, unsupported version, malformed identity, JSON failure with secret-bearing driver error, rejection before migration statements, and remote private-snapshot/media-ownership restrictions. The fixture executes ordinary SQL against an isolated migrated local DB and overrides only explicit engine-probe cases.

Run `cargo test --locked --test libsql_capabilities --test schema_migrations --test storage_engine`.

## Not claimed

The loopback protocol fixture is not a deployed remote libSQL/Turso engine or proof of its transaction, migration, backup, replication, filesystem-ownership or failure-recovery behavior. Version/JSON probes do not prove every SQL statement or every engine build correct; the bundled conformance tests cover their named operations. No HTTP health API, scheduled health task or health event delivery is added. Remote pending migrations remain refused without a verified backup/recovery strategy; remote storage still grants no local media ownership or private-snapshot permission. No dependencies, schema versions or persisted application state are introduced; opening a new local path may still create the database file before compatibility rejection.
