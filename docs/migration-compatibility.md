# Migration compatibility for the merged indexer branch

The selected upgrade baseline is main at
`fff552ad8516270091378807491cbc2a13601bbc`. Its first 78 migration identities and
SQL are preserved byte for byte. The nine-tracker migration file
`0090_indexer_breadth.sql` is appended at ordinal 79. File numbering and persisted
ordinal are distinct; the runner requires a contiguous history.

## Supported baseline and historical blockers

Fresh databases and databases carrying the selected main history can apply the
appended tracker migration. This does not reconcile other historical ledgers:

| Historical source | First divergence from the merged registry | Status |
| --- | --- | --- |
| Original main `6298f0e0d1403c68c1b4e7e61c2ba5d44eba2c50` | Version 50: `torrent_rss_indexer` versus `indexer_backoff` | Rejected before migration writes; conversion remains tracked in [#35](https://github.com/Raudbjorn/hrrdarr/issues/35). |
| Pre-merge PR #32 `eb08c2ba9adf0df14dd7be2fb6d933f26ffb3889` | Version 63: different `notifications` SQL/checksum; later ordinals also differ | Rejected at startup before migration writes. Convert explicitly offline with `hrrdarr convert-premerge-pr32` ([#47](https://github.com/Raudbjorn/hrrdarr/issues/47), below). |

Do not delete or rewrite migration-history rows to bypass these checks. The
underlying schemas and persisted policy differ, not only their version numbers.
The original PR #32 analysis remains available at
[its reviewed commit](https://github.com/Raudbjorn/hrrdarr/blob/eb08c2ba9adf0df14dd7be2fb6d933f26ffb3889/docs/migration-compatibility.md).

Current main's migrations 77 and 78 introduce collection and provider authority.
Collection policy lives in `movie_collection_settings`, removal intent in
`movie_collection_intents`, and provider authority has dedicated tags, status and
selection-cursor tables. The pre-merge PR instead added collection snapshot
fields at ordinal 69 and used a different notification delivery shape. Retaining
unused legacy tables would not preserve the meaning of their settings.

## Converting a pre-merge PR #32 database

Startup keeps refusing the pre-merge ledger. Conversion is a separate, explicit,
offline step that builds a **new** database; the original is never opened for
writing and stays your recovery path.

```sh
# Stop the pre-merge instance cleanly first (its -wal file must be empty or absent).
hrrdarr convert-premerge-pr32 /path/to/old.db /path/to/new.db
```

The command prints a JSON report (row counts, unmonitored collections, pending
deliveries). It refuses, writes nothing and leaves no partial file when:

- the destination exists (it never overwrites);
- the source has an uncheckpointed write-ahead log;
- the source ledger is not exactly the pinned pre-merge ledger (63–76 entries,
  name, checksum and SQL byte for byte), including a tampered or main-lineage ledger;
- the source has a table the destination has no home for, or a table whose shape
  differs without an explicit mapping;
- a notification delivery belongs to a notification with no scope, so its domain
  cannot be determined.

How it works: the source is copied to a private work directory and verified. A
ledger shorter than 76 is first brought to the pre-merge head by replaying the
pinned pre-merge entries on that copy, as the pre-merge runner would. A fresh
database is then created by the normal runner (so its ledger, schema and triggers
are exactly a new install's), the data is moved inside one transaction with
`PRAGMA foreign_key_check`/`integrity_check` before commit, and the result is
renamed into place only after it reopens cleanly under the normal runner.

Mapping decisions:

| Source | Converted |
| --- | --- |
| Tables with identical shape (libraries, files, history, providers and tracker options such as `minimum_seeders`, tags, backoff, commands, download receipts) | Copied as is. Rows seeded by migrations the source never ran are kept. |
| `movie_collections` inline policy (monitored, root, profile, availability, search-on-add) | Moved to `movie_collection_settings`, the same rule as main's migration 0077: a monitored collection missing a root, profile or availability is converted unmonitored (counted in `collections_unmonitored`). The retired columns on `movie_collections` are left at their defaults. |
| `local_edit` / `snapshot_owned` | Carried. A policy written before those markers existed (source ledger 67–68) has unknown provenance and is protected as a local edit. |
| `movie_collection_tags`, members, exclusions | Copied; member revisions are set to their collection's revision. |
| `notification_deliveries` | Gain a domain: a domain event keeps its scope; a system event takes the matching scope, TV first. `sent_parts` stays empty and the claim token is unset: the pre-merge schema never recorded acknowledged request parts, so none are invented. Pending deliveries stay pending (their claim is released), failed and delivered ones keep their state. No event is generated by the conversion. |
| `snapshot_imports` | Copied; `provider_authority_version` is 0, so a later snapshot import repopulates provider authority. |
| Provider tags, selection cursors, provider status, Wanted receipts | New in the converted schema and empty. No failure state is invented. |

AUTOINCREMENT counters never move backwards past ids the source issued.

### Not claimed

- Exercised against ledgers rebuilt from the pinned SQL and populated with
  representative rows, not against a production database from an actual
  pre-merge instance or its historical runner binary.
- A multi-request notification that was partly sent before conversion may be
  re-sent in full; the source holds no evidence of which parts succeeded.
- Other lineages (original main, [#35](https://github.com/Raudbjorn/hrrdarr/issues/35))
  are not converted.
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

`db::indexer_backoff_tests::published_torrent_rss_history_is_rejected_without_changing_data`
constructs the unchanged first 49 entries plus the exact original-main migration
50, then checks repeated startup refusal and unchanged schema/history/provider/
TV/movie rows. Its SQL fixture is copied verbatim from the original commit into
`tests/fixtures/migrations/6298f0e_0050_torrent_rss_indexer.sql`.

```sh
cargo test --locked --lib published_torrent_rss_history_is_rejected_without_changing_data
```

That test protects refusal behavior. It does not repair historical upgrades or
exercise the complete original-main version-53 schema. The tracker schema tests
separately cover the selected main-to-tracker upgrade. Neither establishes live
provider compatibility or filesystem/media recovery.
