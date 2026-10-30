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
| Pre-merge PR #32 `eb08c2ba9adf0df14dd7be2fb6d933f26ffb3889` | Version 63: different `notifications` SQL/checksum; later ordinals also differ | Rejected before migration writes; conversion is tracked in [#47](https://github.com/Raudbjorn/hrrdarr/issues/47). |

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
