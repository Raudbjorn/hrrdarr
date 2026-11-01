# Migration compatibility for the merged indexer branch

The selected main baseline is `023d355` (PR #31 import lists). Its accepted
migration histories remain byte-for-byte prefixes of the merged histories.
The runner recognizes every recorded ordinal, name, SQL body and checksum;
a version number alone does not select a history. Existing records are never
renumbered or rewritten. Unknown or mixed histories are refused.

## Supported histories

| Source history | Preserved entries and appended work | Final version |
| --- | --- | --- |
| Fresh/current import-list history | Main's 83 entries; tracker breadth at 84 and Wanted receipts at 85 | 85 |
| Main with Wanted/root-health/backups at 79–81 | Existing main entries through 83, including import lists and search-on-add; tracker/receipts at 84–85 | 85 |
| Original published main `6298f0e` through 50–53, or the accepted legacy PR #31 history | Historical entries retained through the semantic collection/provider bridge and main additions at 87; tracker/receipts at 88–89 | 89 |
| PR #32 `3987479`, tracker/receipts at 79–80 | Existing entries retained; Wanted timestamps, root health, backups, import lists and search-on-add appended through 85 | 85 |
| PR #32 `8032213`, tracker/receipts at 82–83 | Existing entries retained; import lists and search-on-add appended at 84–85 | 85 |

The supported histories converge to compatible application schemas while keeping
their own ledger identities. The normal transactional runner validates a backup
before applying pending migrations. The two PR #32 histories now upgrade normally;
there is no need to rewrite their ledger or use the older offline converter
([#68](https://github.com/Raudbjorn/hrrdarr/issues/68)).

Before the legacy notification alignment runs, pending system deliveries without a
recorded historical domain cause an explicit refusal. The migration transaction
rolls back and its validated backup remains available; published SQL is unchanged.
Databases already past that alignment can still contain domains inferred by older
code, which need a separate audit rather than another migration-ledger rewrite
([#82](https://github.com/Raudbjorn/hrrdarr/issues/82)).

Original-main compatibility uses the explicit reconciliation introduced by main,
including collection settings, removal intent, provider authority and nullable
search-on-add preservation. Populated SQL fixtures verify these paths; execution
of the pinned historical application binaries is still a separate evidence
requirement in [#35](https://github.com/Raudbjorn/hrrdarr/issues/35). Previously
normalized NULL search-on-add values cannot be inferred; see
[collection policy recovery](collection-policy-recovery.md) and
[#50](https://github.com/Raudbjorn/hrrdarr/issues/50).

The older pre-merge PR #32 `eb08c2b` ledger differs at notifications version 63
and later entries. It remains refused at startup and requires the explicit
converter below. The separate unpublished PR #31 `1431d97` draft uses the
[other offline converter](draft-database-conversion.md).

## Converting a pre-merge PR #32 database

Startup keeps refusing the pre-merge ledger. Conversion is a separate, explicit,
offline step that builds a **new** database; the original is never opened for
writing and stays your recovery path.

```sh
# Stop the pre-merge instance cleanly first (-wal and -journal must be empty or absent).
hrrdarr convert-premerge-pr32 /path/to/old.db /path/to/new.db
```

The command prints a JSON report (row counts, unmonitored collections, pending
deliveries). It refuses, writes nothing and leaves no partial file when:

- the destination exists (it never overwrites);
- the source ownership lock is held by another application instance or cannot be acquired;
- the source has a nonempty write-ahead log or rollback journal, or either sidecar cannot be safely inspected;
- the source ledger is not exactly the pinned pre-merge ledger (63–76 entries,
  name, checksum and SQL byte for byte), including a tampered or main-lineage ledger;
- the source has a table the destination has no home for, or a table whose shape
  differs without an explicit mapping;
- a pending system notification delivery has no recorded fan-out domain (current
  scopes cannot establish the historical domain, even if only one remains);
- a terminal system notification delivery has no current scope for its display domain.

How it works: the converter holds the same exclusive ownership lock as normal
application startup through copying, verification and publication. It copies from
the locked read-only descriptor into a private work directory; it never opens the
original through SQLite or runs journal recovery on it. A
ledger shorter than 76 is first brought to the pre-merge head by replaying the
pinned pre-merge entries on that copy, as the pre-merge runner would. A fresh
database is then created by the normal runner (so its ledger, schema and triggers
are exactly a new install's), the data is moved inside one transaction with
`PRAGMA foreign_key_check`/`integrity_check` before commit, and the result is
published with an atomic no-replace rename only after it reopens cleanly under the
normal runner. A destination created during conversion is preserved.

Mapping decisions:

| Source | Converted |
| --- | --- |
| Tables with identical shape (libraries, files, history, providers and tracker options such as `minimum_seeders`, tags, backoff, commands, download receipts) | Copied as is. Rows seeded by migrations the source never ran are kept. |
| `episodes`, `movies`, `search_commands` | Existing columns are preserved. Main’s new native search timestamps and dispatch completion facts start `NULL`, since the historical runner never recorded them. |
| `movie_collections` inline policy (monitored, root, profile, availability, search-on-add) | Moved to `movie_collection_settings`, the same rule as main's migration 0077: a monitored collection missing a root, profile or availability is converted unmonitored (counted in `collections_unmonitored`). The retired columns on `movie_collections` are left at their defaults. |
| `local_edit` / `snapshot_owned` | Carried. A policy written before those markers existed (source ledger 67–68) has unknown provenance and is protected as a local edit. |
| `movie_collection_tags`, members, exclusions | Copied; member revisions are set to their collection's revision. |
| `notification_deliveries` | Gain a domain: a domain event keeps its recorded scope. Pending system events are refused because the source never recorded their fan-out domain. Terminal system events use current matching scopes (TV first) only as a display projection, not as evidence of historical routing. `sent_parts` stays empty and the claim token is unset: the pre-merge schema never recorded acknowledged request parts, so none are invented. Pending deliveries stay pending (their claim is released), failed and delivered ones keep their state. No event is generated by the conversion. |
| `snapshot_imports` | Copied; `provider_authority_version` is 0, so a later snapshot import repopulates provider authority. |
| Provider tags, selection cursors, provider status, Wanted receipts, backup commands | New in the converted schema and empty. No failure or completed-backup state is invented. |

AUTOINCREMENT counters never move backwards past ids the source issued.

### Not claimed

- Exercised against ledgers rebuilt from the pinned SQL and populated with
  representative rows, not against a production database from an actual
  pre-merge instance or its historical runner binary.
- Ownership exclusion is advisory and applies to cooperating application opens;
  arbitrary external file writers must also remain stopped.
- Non-UTF-8 source and destination filenames are supported, but libSQL requires
  the destination parent directory path used for the working database to be UTF-8.
  Broader path support is tracked in [#66](https://github.com/Raudbjorn/hrrdarr/issues/66).
- A multi-request notification that was partly sent before conversion may be
  re-sent in full; the source holds no evidence of which parts succeeded.
- This command accepts only its pinned pre-merge PR #32 source. Other supported
  histories upgrade through the normal runner; unknown histories remain refused.
- The converter does not verify filesystem or media state, and was not run
  against a remote (Turso) database; it is local-file only.

## Requirements for a historical conversion

- Preserve published ordinal/name/SQL/checksum history, or recognize the exact
  lineage through an explicit validated transition without accepting arbitrary
  altered history. Preserve the selected current-main upgrade path.
- Map collection monitoring, roots, profiles, tags, removal intent, ownership
  and revisions; provider tag restrictions, selection cursors, backoff and
  pending health deadlines; and Torrent RSS settings such as `minimum_seeders`.
- For pre-merge PR databases, also reconcile notification delivery domain,
  multi-request progress and trigger changes without inventing delivery success.
- Preserve TV/movie libraries, file relationships, active commands and download
  receipts. Report unsupported/conflicting state before mutation rather than
  silently dropping it.
- Validate a recoverable backup before any conversion writes. Test transaction
  failure, restore, repeat opens/reruns and foreign-key integrity with libSQL.
- Cover populated original-main versions 50–53, pre-merge PR histories, selected
  current-main databases and fresh creation. Historical-runner fixtures must
  supplement SQL-only fixtures before claiming production upgrade support.

## Focused evidence

Pre-merge conversion (#47): `db::convert_tests` pins the ten divergent entries to
the checksums recorded from `eb08c2b`, converts populated databases at ledger
lengths 63, 64, 66–70, 75 and 76, compares the converted schema, triggers and
ledger with a fresh install, checks the source is byte-identical, and exercises
each refusal. `tests/premerge_conversion_cli.rs` runs the real binary.
The divergent SQL is `src/db/premerge_pr32/`.

```sh
cargo test --locked --lib convert_tests
cargo test --locked --test premerge_conversion_cli
```

Original-main compatibility is covered by `db::published_prefix_tests` and the
pinned version-50 fixture in `db::indexer_backoff_tests`. The PR #32 history tests
in `db::indexer_breadth_tests` cover both earlier migration orders, populated
tracker scopes, equal TV/movie IDs, empty and nonempty receipts, receipt replay,
changed-body refusal, reopen and tampered-history rejection.

```sh
cargo test --locked --lib published_prefix_tests
cargo test --locked --lib indexer_backoff_tests
cargo test --locked --lib indexer_breadth_tests
```

These are isolated SQL-reconstructed databases. They do not establish production
upgrade success, historical-runner equivalence, or filesystem/media recovery.
