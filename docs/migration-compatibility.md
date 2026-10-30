# Migration compatibility blocker in PR #32

PR #32 does **not** upgrade databases created by its base revision,
`6298f0e0d1403c68c1b4e7e61c2ba5d44eba2c50`, once migration 50 has been applied.
Startup refuses them with `migration history mismatch at version 50` before
applying migrations. Do not delete or rewrite migration-history rows to bypass
this check: the underlying schemas and persisted authority also differ.

The first 49 registered migrations match the base byte for byte. The first
divergent entries are:

| Version | Published base | PR #32 |
| --- | --- | --- |
| 50 | `torrent_rss_indexer` | `indexer_backoff` |
| 51 | `download_client_backoff`, health expiry on `health_checks.next_expiry_at` | Same name, changed SQL/checksum, expiry on `download_client_status.next_health_at` |
| 52 | `collections` | `download_client_selection` |
| 53 | `provider_authority` | `path_diagnostics` |

This is more than a numbering conflict. The base stores collection policy in
`movie_collection_settings`, removal intent in `movie_collection_intents`, and
metadata revisions on collection memberships. This branch uses inline policy
on `movie_collections` and does not read those former tables. Likewise, the
base's `provider_scope_tags`, `provider_status`, and `provider_selection_cursors`
are not read by this branch. Its replacement indexer backoff is scoped per
domain and resets with configuration changes; the base status is provider-wide
and retains versioned observations. Preserving unused tables alone would not
preserve the meaning of existing settings, exclusions, or backoff.

A complete reconciliation must:

- Preserve published ordinal/name/SQL/checksum history, or recognize the exact
  historical lineage through an explicit validated transition without accepting
  arbitrary altered history.
- Define and test mappings for collection monitoring, roots, profiles, tags,
  removal intent, ownership and revisions; provider tag restrictions, selection
  cursors, backoff observations and pending health deadlines; and torrent RSS
  configuration, including `minimum_seeders`.
- Preserve existing TV/movie libraries and active commands/download receipts;
  reject unsupported state before mutation rather than silently dropping it.
- Cover populated base databases through versions 50–53, existing branch
  databases, fresh creation, repeat opens, transaction failure, backup recovery
  and foreign-key integrity using the actual libSQL driver.

The focused test
`db::indexer_backoff_tests::published_torrent_rss_history_is_rejected_without_changing_data`
constructs the unchanged 1–49 prefix plus the exact base migration 50, then
checks repeated startup rejection and unchanged schema/history/provider/TV/movie
rows. Its SQL fixture is copied verbatim from the base commit's
`migrations/0050_torrent_rss_indexer.sql` into
`tests/fixtures/migrations/6298f0e_0050_torrent_rss_indexer.sql`.

Run it with:

```sh
cargo test --locked --lib published_torrent_rss_history_is_rejected_without_changing_data
```

This evidence protects the current refusal behavior; it does not repair the
upgrade path or exercise a complete version-53 database. Both migration review
threads remain open until reconciliation is implemented and verified.
