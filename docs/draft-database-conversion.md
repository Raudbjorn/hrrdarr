# Converting an unpublished PR #31 draft database

A database written by the unpublished PR #31 draft build (draft head `1431d97`, 76 migration rows)
is not accepted at startup. Its ledger matches the main lineage through version 62 and first
differs at version 63 (`notifications`); against the published PR #31 ledger it differs from
version 50. `hrrdarr` rejects it before running a statement and tells you so.

`hrrdarr convert-draft-database` builds a **new** database on the current main lineage from it.
It is offline, never modifies the source, and never overwrites anything.

```sh
systemctl stop hrrdarr                                   # stop all other database writers/readers too
hrrdarr convert-draft-database --from hrrdarr.db         # dry run: converts in scratch space, verifies, discards
hrrdarr convert-draft-database --from hrrdarr.db --to hrrdarr.converted.db
```

Keep the original `hrrdarr.db` and any `hrrdarr.db-wal`, `hrrdarr.db-shm`, or `hrrdarr.db-journal`
sidecars together at their existing
names for recovery; sidecars may contain committed data that is absent from the main file. Do not
move only the main file or put the converted database under a basename with retained draft sidecars.

Instead, change the service's existing `HRRDARR_DATABASE_PATH` configuration to the **absolute path**
of `hrrdarr.converted.db`. Keep its parent directory writable by the service and keep the original
provider encryption key/configuration. If you edited the systemd unit or a drop-in, run
`systemctl daemon-reload`, then start the service:

```sh
systemctl start hrrdarr                                  # opens the converted path; later migrations apply as usual
```

Verify the configured database path and startup result before retiring any original files. This
switch leaves the draft database and its sidecars intact and uses a distinct basename for the
converted database. The output basename must have no existing database or sidecar, including a
rollback journal; the converter refuses those paths. Keep all other processes away from that
output basename until conversion and configuration are complete.

Exit codes: `0` done, `1` error (nothing published), `3` the source is not a convertible draft
database (nothing written but the report).

## What is accepted

Only the complete, unmodified draft database: all 76 ledger rows with the draft's names and
checksums, **and** the draft schema (checked by fingerprint). Anything else is refused with a
reason in the report:

| Reason | Meaning |
| --- | --- |
| `already_supported` | Startup already accepts it (this includes a draft database that stopped before version 63, whose rows are identical to main's). Start `hrrdarr`. |
| `no_migration_history` | No `schema_migrations` table. |
| `not_the_draft_ledger` | A row differs from the draft ledger, or the history is longer. |
| `draft_ledger_incomplete` | The draft build never finished migrating (between versions 63 and 75). Run the draft build once to finish, then convert. |
| `draft_schema_differs` | The draft ledger over a schema that was changed outside the migrations. |

## How it converts

1. Takes the service lock, copies the database file and any `-wal` file to a private scratch
   directory while hashing them, and opens only the copy. The hash is recomputed at the end; a
   difference aborts.
2. Builds an empty target by running the real main migrations 1 through 79, so the target schema,
   constraints and ledger are the supported ones by construction.
3. Copies every table by column name inside one transaction, with triggers suspended (they would
   re-derive events and history) and restored from their own stored text afterwards. A draft column
   with no mapping, or a table that is neither copied nor known to be new, aborts the conversion.
4. Applies the same transformations the main migrations apply:
   - **Collection policy** moves from `movie_collections` to `movie_collection_settings` (migration
     0077). The draft's own `local_edit` / `snapshot_owned` flags are kept; a row with neither flag
     (unknown provenance) is treated as a local edit, as 0077 does. A monitored collection without a
     root, profile and availability is stored unmonitored.
   - **Collection members** are stamped with their collection's `metadata_revision`, which the main
     triggers require; `graph_complete` starts at 0.
   - **Notification deliveries** gain `media_type` (the event's scope, else TV when the notification
     has a TV scope, else movies), `sent_parts = '[]'` and no lease (migration 0083).
   - **Snapshot imports** gain `provider_authority_version = 0`.
5. Verifies: schema equals a freshly migrated reference, `PRAGMA integrity_check`, foreign keys,
   per-table row counts, and the cross-table facts main's triggers only enforce on write (collection
   roots in the movies domain, member revisions, member and tag limits, directory-client categories).
   Data main would reject is refused rather than published.
6. Checks the output with the application's own migration-history reader (79 rows, main lineage) and
   publishes it with a no-clobber hard link, mode `0600`. The destination filesystem must support
   hard links; otherwise conversion fails without publishing a destination. There is no direct-copy
   fallback that could expose a partially copied database.

## What is not preserved

- `schema_migrations` is regenerated: the converted ledger is main's, and `applied_at` is the
  conversion time.
- The draft stored collection `search_on_add` as a non-null column, so "absent" and "explicit false"
  were never distinguished. Values are copied as stored. (`recover-collection-policy` applies only to
  databases that went through the published version-82 conversion.)
- Draft triggers and indexes are replaced by main's. No event is replayed.
- New main tables (provider tags, provider status, selection cursors, collection intents) start
  empty, exactly as a fresh main migration leaves them.

## Audit report

Every run writes `hrrdarr-draft-conversion-<mode>-<time>-<id>.json` (mode `0600`) next to the output
(or the source for a dry run) and one structured line on stderr. It contains hashes, per-table row
counts and decisions. It contains no row values, paths or credentials.
