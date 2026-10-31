# Collection policy recovery

`hrrdarr recover-collection-policy` restores one specific piece of collection policy that the
published PR #31 history destroyed: whether a collection's "search on add" setting was **absent**
(`NULL`) or an **explicit false** (`0`). It is an explicit, offline, dry-run-by-default command.
It is not run by the server and never runs during startup.

## Affected history

* Older main published `movie_collection_settings.search_on_add` as nullable. `NULL` means
  "no choice recorded"; `0` means "a person chose not to search".
* The published PR #31 history (version 82) replaced that table with inline columns on
  `movie_collections` where `search_on_add` is `NOT NULL DEFAULT 0` (migration
  `collections_checkpoint_supersede`, version 73). A database that went through it holds `0` for
  both cases, and the distinction cannot be reconstructed from live data.
* A database that is still on the old published main history (versions 52 to 72) and is upgraded
  by the current build keeps the distinction: the runner records which rows were `NULL` before
  that step and the version 83 bridge re-applies it. **Only databases that already completed the
  published version-82 conversion need this recovery.** After the bridge (version 83) such a
  database has a separate `movie_collection_settings` row per configured collection, with
  `search_on_add = 0` where `NULL` used to be.

The recovery does not rewrite `schema_migrations`, change monitoring, search behavior, root
folders, quality profiles, availability, tags or removal intents, and it never turns a false
into "absent" on a guess.

## What you need

A **matching pre-upgrade backup**: the directory `.hrrdarr-migration-<uuid>` that hrrdarr wrote
next to the database file (`database.db` and `manifest.json`) when the database was still at a
version before the conversion (the first upgrade from published main writes one with
`"schema_version": 53`). The backup created by the later upgrade to version 83 is taken after
the loss and proves nothing; the command reports it as unrecoverable.

To find the candidate, next to the database:

```sh
grep -l '"schema_version": 53' .hrrdarr-migration-*/manifest.json
```

Without such a backup nothing can be recovered. Keep the live database as it is; every false
value stays exactly as recorded.

## Command

Stop hrrdarr first. The command takes the same exclusive database lock as the server and refuses
to run while the server holds it. The database path comes from `HRRDARR_DATABASE_PATH`
(default `hrrdarr.db`); remote storage (`TURSO_DATABASE_URL`) is refused.

```sh
# 1. Dry run (default): reads only, writes nothing to the database.
HRRDARR_DATABASE_PATH=/data/hrrdarr.db hrrdarr recover-collection-policy --backup /data/.hrrdarr-migration-<uuid>

# 2. Review the report, then apply in one transaction.
HRRDARR_DATABASE_PATH=/data/hrrdarr.db hrrdarr recover-collection-policy --backup /data/.hrrdarr-migration-<uuid> --apply
```

The database must already be at the current schema version (start hrrdarr once, then stop it). The
backup is never modified: it is hash-checked while it is copied to a private scratch directory
next to the database, and only the copy is opened. The scratch directory is removed afterwards.

Exit codes: `0` completed (dry run or applied, including "nothing to restore"), `1` error or bad
usage (no changes), `3` the backup cannot prove anything (nothing applied).

## How a backup is accepted

All of these must hold, otherwise every live false value is reported as
`unrecoverable_no_backup_provenance` and nothing is applied:

1. `manifest.json` and `database.db` exist as regular files (a symlinked backup directory is
   refused), the manifest is a `pre-migration recovery` manifest, and its `sha256` equals the
   SHA-256 of the bytes of `database.db`.
2. The database passes `integrity_check` and `foreign_key_check`.
3. Its `schema_migrations` ledger is an exact prefix of the published PR #31 history (name,
   checksum and SQL of every row) of 52 to 72 rows, so `search_on_add` was still nullable, and
   the manifest's `schema_version` equals the ledger's row count. The live ledger must be that
   same history, complete (87 rows, including the version-83 bridge, search-on-add, Wanted timestamps, root health and backups). A backup from the other (main) lineage, or one taken after
   the conversion, is rejected.

## Decisions

Collections are matched by **both** the internal id and the TMDB collection id. They must agree in
both directions; otherwise the row is `ambiguous` and not touched.

| Decision | Meaning |
| --- | --- |
| `restorable` | Backup `NULL` with `local_edit = 0`, live `0`, and the live settings equal what the version-82 conversion plus the version 83 bridge yield from the backup row: same `settings_revision`, `monitored`, root folder, quality profile, minimum availability, `local_edit`, `snapshot_owned` and tag set. |
| `already_null` | Live is already `NULL`. A repeat run reports this and changes nothing. |
| `explicit_false_preserved` | The backup recorded an explicit false; kept. |
| `explicit_true_preserved` | The backup recorded an explicit true; kept. (An addition to the original list so a true value is not labelled false.) |
| `local_edit_since_backup` | The live row differs from that conversion in a compared field (the `reason` names the first difference), so it was edited after the backup; kept. |
| `ambiguous` | No TMDB identity in the backup, or the id and TMDB id point at different live rows, or a monitored row (a monitored collection cannot hold an absent value), or matching settings whose backup already had `local_edit = 1` (later search-only edits cannot be detected). |
| `missing_in_live` | The backup row has no counterpart collection in the live database. |
| `unrecoverable_no_backup_provenance` | A live false that the backup does not cover (no usable backup, or no settings row for it in the backup). |

## Revision consequence

Each restored collection's `settings_revision` increases by **exactly 1**. The
`collection_settings_revision` trigger requires this for any policy change and it stays enabled,
as do foreign keys. A client holding the previous revision of a restored collection will see a
revision conflict and must reload. `local_edit` and `snapshot_owned` are not changed, so a
restored value that no one edited can still be replaced by a later snapshot import, exactly like
any other snapshot-owned value.

## Apply semantics and report

`--apply` runs one `BEGIN IMMEDIATE` transaction that re-plans from the locked live data,
updates only the `restorable` rows (each conditioned on its revision and the old value), verifies
the result, checks the migration ledger digest and integrity, and commits. Any failure rolls
the whole transaction back and leaves the database unchanged. Repeating the command is safe.

Every run, including dry runs and failures, writes an audit report next to the database (never in
the backup): `hrrdarr-collection-policy-recovery-<dry-run|apply>-<unix>-<uuid>.json`, created
with `create_new` and mode `0600`, and prints one structured line to stderr. It contains the
collection ids, TMDB ids, decisions and reason codes, counts, whether it was applied, the backup's
database hash (as verified), the hash of the manifest file, the backup schema version and the
ledger digest before and after. It contains no tokens, credentials or media paths. A report is
reserved before the database is touched, so an unwritable location fails early.

## Not recoverable

* Anything without a matching backup. The live data alone cannot distinguish absent from false.
* Collections created after the backup, or not in it.
* A `search_on_add` value deliberately set to false after the backup, when the row already had
  `local_edit = 1` in the backup and nothing else about it changed. The compared fields cannot
  show that edit. All otherwise matching rows with `local_edit = 1` in the backup are therefore
  `ambiguous` and remain unchanged, even with `--apply`. No automatic restore is safe for them.
* Rows with a TMDB id missing in the backup, or whose id and TMDB id disagree.
* Policy edits that were later reverted by hand to the same values: they are indistinguishable
  from no edit.
