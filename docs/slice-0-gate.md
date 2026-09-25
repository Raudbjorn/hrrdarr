# Slice 0 gate evidence

The slice-0 gate passed on 2026-09-25 against the native synthetic contracts below.
This advances delivery to slice 1; it does not establish full product parity.
The combined plan's exact slice-0 acceptance is:

> Fresh DB and upgraded prototype preserve TV data; a movie and episode with the same numeric ID remain distinct; both snapshot formats are mapped and unsupported data reported

The associated `scn.001` scenario is fresh setup and upgrade; Sonarr-only,
Radarr-only and combined snapshot imports; ID collisions, unknown settings,
absent files, reruns and rollback.

## Runnable integration evidence

Run from the repository root:

```sh
cargo test --locked --test snapshot_import
```

The focused run on 2026-09-25 passed both tests. Parent review of the actual diff
and an independent full run passed 127 Rust tests, 8 frontend tests, formatting,
locked compilation, generated API drift checks, Svelte checks (zero errors/warnings),
the frontend build and diff checks. All databases and source backups
are synthetic temporary files. No network listeners, clients or media operations
are involved. The two tests serialize snapshot imports within their binary because
the production importer has a process-wide single-import guard.

`slice0_fresh_isolated_and_combined_imports_follow_actual_prototype_upgrade`
uses five destinations: fresh Sonarr233 only, fresh Radarr206 only, fresh
Radarr242 only, fresh combined, and actual three-table prototype upgraded through
`Database::open_local` before combined imports. The upgraded prototype is created
from migration0001 without migration history. Its archive permissions are explicitly
0600, satisfying the existing private-state precondition. The test requires a
migration backup on upgrade and no new backup on reopen.

| Acceptance clause / scenario | Assertions |
| --- | --- |
| Fresh DB and upgraded prototype preserve TV data | Fresh isolated destinations contain no opposite-domain library rows. The upgraded case checks original series ID/title/year/path/poster, all three episodes’ IDs/owners/seasons/numbers/titles/resolved file paths (including the shared file and null association) and operation ID/typed target/source/mode/destination/status/message against explicit expected values, after migration, each import and reopen. |
| Equal numeric movie and episode IDs remain distinct | Both combined destinations contain episode1 and movie1 simultaneously; three app/snapshot-scoped mappings for source ID1 identify the TV episode and both movie-version records. Four movie library entries survive together. |
| Both snapshot formats mapped | Sonarr233, Radarr206 inline movie metadata and Radarr242 split metadata are exercised separately and together. Radarr242 uses `QualityProfileId`; the older `ProfileId` name must not substitute for that version's source column. |
| Unsupported data reported and retained privately | Unresolved TV provider records and movie collections appear in reports. A credential sentinel is absent from public reports and retained in the private Config archive. Unsupported retention does not activate those semantics. |
| Absent files | Missing source file records are counted for TV and movie242; no-file sentinels become absent associations. Two TV episodes share one resolved pack path; three imported TV episodes have no file association. This is database-record evidence, not a disk-existence check. |
| Dry runs, rollback and reruns | Each source first runs dry, then fails at a trigger on episode/movie insertion after preceding core writes, then succeeds after trigger removal. Before/after equality covers core TV/movie/file/settings tables, operations and snapshot provenance/archive tables. Exact replay changes none of that state. Failure text omits the private trigger sentinel. |
| Persistence and immutable inputs | Logical state is identical after database reopen. All three source byte arrays equal their original on-disk fixture bytes after every destination scenario. |

`both_snapshot_adapters_reconcile_without_data_loss_or_secret_disclosure` adds
local-edit and changed-source conflicts without overwriting TV data, archive count
stability, absent mapped-file replay refusal, path traversal/ownership rejection,
WAL upload refusal, unsupported schema/app rejection and hostile SQLite schema
checks. It checks catalog-only movie metadata and edition/monitoring preservation.

## Supplemental evidence

These existing tests support individual boundaries; they are not substitutes for
the connected scenario above:

- `tests/schema_migrations.rs`:
  `fresh_relations_enforce_domain_ownership_and_catalog_membership`,
  `prototype_upgrade_preserves_data_backups_restore_and_rerun_is_noop`,
  `each_migration_ddl_and_writes_rollback_on_failure`.
- `tests/profile_snapshots.rs`:
  `complete_profiles_activate_with_scoped_assignments_and_exact_replay`,
  `unsupported_profiles_are_never_truncated_and_conflicts_roll_back`.
- `src/db/snapshot_profile_tests.rs`:
  `snapshot_profile_schema19_backfill_rollback_reopen_and_intact_mapping_guards`.

Source-format requirements come from the pinned Sonarr
`76c684e097f16ac216e6213845e5cac372774995` and Radarr
`c90668a520664ad0c91812cfee57c41928ad2148` inventories documented in
[snapshot-import.md](snapshot-import.md). Source inspection establishes those
requirements; the test run observes this native implementation against synthetic
fixtures. Neither establishes upstream runtime or V3 wire equivalence.

## Not claimed

- No real exported backups, production databases, live services, browser workflow,
  remote database topology or upstream runtime were tested.
- Source file-record absence is not missing-mount, filesystem permission or media
  existence verification. Literal fixture paths are never inspected as media.
- Injected SQL failures establish transaction rollback, not power-loss durability,
  process-crash recovery or disk-failure recovery. Backup/restore has separate
  focused migration evidence; this scenario does not restore the migration backup.
- Complete ancillary reconstruction, custom-format support, policy evaluation,
  search/download/import automation, future shared command/event targets and the full
  `fnd.009`, `fnd.010` or API rows remain
  separate requirements. Reporting unsupported data satisfies only that reporting
  boundary; it does not deliver the unsupported feature.
- This change introduces only test fixtures and documentation, no production state,
  migrations, API behavior or dependencies. Temporary fixture databases and an
  isolated migration backup are created and removed by the tests.
