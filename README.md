# hrrdarr

hrrdarr is a from-scratch, clean-room implementation of the combined feature sets of
**Sonarr** (TV) and **Radarr** (movies) in a single service: library management,
search/grab/import automation, providers, settings and system operations, with TV and
movies modelled as separate domains rather than movies-as-episodes.

Stack: Rust (Axum, tokio), libSQL (embedded or Turso), Svelte 5 + Vite frontend.

> **Status: work in progress, not usable as a Sonarr/Radarr replacement.**
> The parity ledger tracked locally (not published) had 278 rows on 2026-10-03:
> 45 Verified, 68 Partial, 136 Missing, 29 Blocked. A passing build is not parity;
> see "What is and is not claimed" below.

## What exists

- Separate TV (series, season, episode, episode file) and movie (movie, metadata,
  movie file, credits) schemas with typed media targets, so a movie and an episode with
  the same numeric ID never collide. 49 ordered, checksummed migrations.
- Sonarr and Radarr snapshot importers (separate readers, namespaced source IDs,
  dry-run reconciliation, idempotent replay).
- Providers: Torznab/Newznab indexers and qBittorrent, with encrypted credentials.
- Persistent command queue, scheduler, RSS sync and download polling with restart recovery.
- Search, decision engine and grab for both domains, including the user-invoked-search
  availability exception; quality/release/delay profiles, custom formats, tags.
- Import pipeline: copy/move/hardlink, naming, same-path replacement with recovery.
- Health checks, a read-only movie credits API, and a Svelte UI covering library,
  activity, provider, profile, tag and health screens.

Many areas are partial (for example collections, notifications, remaining indexer and
download-client providers, metadata consumers, wanted/calendar, system/backup) or blocked
on verification; those are tracked as work, not hidden.

## Build and run

Requires the pinned toolchain in `rust-toolchain.toml` (Rust 1.98.1) and Node for the frontend.

```sh
HRRDARR_DATABASE_PATH=/tmp/hrrdarr.db cargo run
```

| Variable | Purpose |
| --- | --- |
| `HRRDARR_DATABASE_PATH` | Embedded database file (default `hrrdarr.db`). |
| `TURSO_DATABASE_URL`, `TURSO_AUTH_TOKEN` | Use a Turso/libSQL endpoint instead. |
| `HRRDARR_BIND` | Listen address (default `127.0.0.1:8760`). |
| `HRRDARR_ALLOWED_HOSTS`, `HRRDARR_TRUSTED_NETWORKS` | Host and network allow-lists. |
| `HRRDARR_PROVIDER_KEY` | Hex key used to encrypt provider credentials. |

Frontend: `cd frontend && npm install && npm run dev`.

## Verify

```sh
cargo fmt --check
cargo check --locked
cargo test --locked
npm --prefix frontend run check
npm --prefix frontend test
npm --prefix frontend run build
git diff --check
```

Some test suites are timing-sensitive; run them serially (`-- --test-threads=1`) on a
loaded machine. Browser workflows use Playwright against a fixture server.

## Repository layout

- `src/` Rust service (API, commands, providers, search, import, health, snapshots)
- `migrations/` ordered SQL migrations
- `frontend/` Svelte UI and its tests
- `tests/` integration tests; `docs/` per-feature contracts and gate reports
- `AGENTS.md` development guide; `.claude/` and `.codex/` specialist agent definitions

## Branches

`main` is the accepted line of work (the `parity/loop` branch). The other `parity/*`
branches are per-iteration worktree branches pushed for provenance: some are integrated
into `main`, some are superseded, and some (for example `parity/iteration87-collections`)
are stopped or blocked attempts preserved as-is. Do not assume any of them is mergeable.

## What is and is not claimed

Behaviour is verified against synthetic fixtures, mock HTTP servers and temporary
databases only. Not claimed: compatibility with real Sonarr/Radarr instances, real
indexers, download clients or metadata services, power-loss durability, multi-node
operation, or wire compatibility with the Sonarr/Radarr V3 APIs. Upstream projects are
requirements references only; no upstream code is copied or translated.

## License

MIT. See `LICENSE`.
