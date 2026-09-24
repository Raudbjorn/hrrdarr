# hrrdarr development guide

## Scope and source of truth

Deliver the union of Sonarr's TV and Radarr's movie feature sets: library management, automation, every concrete provider, UI, settings and system operations. Deferring a feature changes its delivery slice, not the parity requirement. Both domains must pass the first automation gate.

Before planning or changing domain behavior, read the [combined parity plan](.do-not-commit/research/hrrdarr-parity-plan.md). Its scope, slice numbering and acceptance criteria supersede older TV-only estimates, fixed table counts, skip lists and conflicting agent instructions.

Read the relevant research when working on:

| Task | Reference |
| --- | --- |
| Feature coverage, missing workflows and movie requirements | [Feature inventory](.do-not-commit/research/2026-09-24-sonarr-feature-parity.md) |
| Workstream dependencies and integration risks | [TV decomposition](.do-not-commit/research/sonarr-parity-decomposition.md), subject to the combined plan |
| Schema, snapshot import, filesystem consistency, backups or storage topology | [Persistence review](.do-not-commit/research/sonarr-persistence-review.md) |
| UI state, API types, routing or live updates | [Frontend review](.do-not-commit/research/sonarr-frontend-architecture-review.md) |

Research and upstream checkouts under `.do-not-commit/` are Git-ignored local references; do not commit them. If unavailable, identify the missing evidence and use this guide's constraints; do not invent reference behavior or claim parity verification.

Use upstream trees only to establish requirements. Implement independently from protocol specifications, naming conventions and observed behavior; do not copy or translate upstream C# or frontend code. The research pins Sonarr to `76c684e097f16ac216e6213845e5cac372774995` and Radarr to `c90668a520664ad0c91812cfee57c41928ad2148`. Source inspection establishes inventory, not runtime equivalence; Sonarr-specific findings do not automatically apply to Radarr.

## Current implementation and blockers

Baseline checked on 2026-09-24; inspect current code before relying on these observations:

- [src/main.rs](src/main.rs) contains the Axum API, three-table initialization (`series`, `episodes`, `operations`), five routes, import execution and the Sonarr snapshot importer.
- [frontend/src/App.svelte](frontend/src/App.svelte) lists series/episodes and creates import previews. The UI does not execute imports. Movie support and the automated search/download loop are still missing.
- `import_execute` changes the filesystem and marks the operation complete without updating episode/file associations.
- `migrate` stores `EpisodeFileId` as a path string and deletes destination series/episodes before inserting source rows, without a transaction. Use isolated copies for importer work; the current endpoint is not safe for existing libraries.

Keep Rust/Axum, libSQL and Svelte/Vite. Read [Cargo.toml](Cargo.toml), [Cargo.lock](Cargo.lock) and [frontend/package.json](frontend/package.json) for actual dependencies. Planned modules and libraries in research or agent prompts are not installed infrastructure. Add modules and dependencies as delivered behavior needs them; pin the toolchain/frontend versions before establishing reproducible parity tests.

## Delivery order

| Slice | Deliverable and exit condition |
| --- | --- |
| 0 | Schema and API contracts for both domains. Fresh/upgrade paths preserve data; equal numeric movie/episode IDs remain distinct; both snapshot formats are mapped and unsupported data reported. |
| 1 | Manual libraries and initial providers. Add/edit/monitor one series and movie, import both safely, test an indexer/client, and fix the existing import/path bugs. Start with Torznab/Newznab and qBittorrent. |
| 2 | Persistent commands, scheduler, RSS and download polling. Retries/restarts preserve targets and avoid duplicate external actions; errors and progress surface through APIs/UI. |
| 3 | TV/movie search, decisions and grab. Verify matching, rejection reasons, availability/delay, the explicit user-invoked-search exception, and cutoff/upgrade behavior. |
| 4 | Complete automation and minimal Activity/Settings UI. Both Add → Search/RSS → Grab → Download → Import → Library flows pass restart/failure recovery; failed replacement preserves original files. |
| 5 | Domain breadth: collections/discovery/credits, advanced TV numbering/packs, profiles/custom formats/delays, lists, metadata, notifications and full shared UI. |
| 6 | Full parity closure: evidence for every remaining provider, command, API/UI workflow and system/recovery requirement in both references. |

Build complete vertical slices. Concrete indexer/client work can proceed in parallel once contracts are stable; UI, metadata, lists and notifications can proceed against stable APIs. Coordinate shared migrations and API contracts before concurrent edits. Use the 12 specialist definitions in [.codex/agents/](.codex/agents/) for matching work; give each delegated task explicit file ownership and acceptance criteria.

## Domain and API boundaries

- Model TV (`Series`, `Season`, `Episode`, `EpisodeFile`) and movies (`Movie`, catalog metadata, `MovieFile`, collections) separately. Movies are not synthetic episodes. Catalog/discovery entries can exist without library membership; one TV file can serve several episodes.
- Give shared commands, operations, queue/history/blocklist entries and events typed media targets. Enforce valid relationships and domain-specific uniqueness in storage; an unqualified integer must never choose between a movie and episode.
- Scope profiles, naming, roots, provider categories, monitoring and availability policies to their media domain. Share transports where behavior matches. Preserve TV standard/daily/anime/special/pack semantics and movie title/year/ID/edition/release-date semantics.
- Distinguish background movie availability/delay rules from the explicit user-invoked-search exception. Edition recognition alone does not establish support for multiple simultaneous movie editions.
- Specify pagination, bulk actions, errors and command progress before UI work. Shared Calendar distinguishes TV air dates from cinema/digital/physical movie releases. Sonarr/Radarr V3 wire compatibility requires a separate explicit decision and contract tests.

## Persistence, imports and recovery

- Use ordered, transactional migrations with version/checksum tracking, one migration owner, integrity checks and recovery tests. Validate SQL against the actual libSQL engine/topology. Keep topology changes separate from domain expansion; remote or synchronized storage does not provide filesystem ownership or atomic side effects.
- Use separate Sonarr/Radarr snapshot readers and a validated destination writer. Read immutable backup copies, detect supported schema versions, namespace source IDs by application/snapshot and remap them without clearing existing libraries. Resolve file IDs through file records and library roots; convert no-file sentinels to absent associations.
- Preserve monitoring, profiles/custom formats, tags, collections, history, providers, mappings and exclusions. Retain/report unknown settings and produce a dry-run reconciliation report for mapped, conflicting, unsupported and missing-file records. Reruns must be idempotent; source databases/media stay intact.
- Treat serialized settings, enums, timestamps and job names as versioned contracts. Keep queried relationships relational and provider-specific settings structured; query JSON structurally rather than matching its formatting.
- Journal import intent and progress, stage/validate transfers, commit file associations and required history together, then safely retire old files. Recover both disk-before-DB and DB-before-cleanup failures. A database transaction cannot roll back a file move or download submission; a timeout/cancellation does not prove an operation never happened.
- Keep one active worker per library until distributed ownership is explicitly designed and tested. Bound scans/imports/retries, keep blocking work off async executors, and avoid holding DB transactions across network calls or media transfers. Reuse durable operations/commands for retryable effects where sufficient.
- Validate external paths, path mappings, permissions and copy/move/hardlink behavior, including cross-device and missing-mount cases. Destructive list cleanup and media deletion require explicit user intent.
- Publish backups only after validating a consistent snapshot and manifest; prune old backups after success. Validate restores before replacing live state, retain a recoverable original and test restoration into an isolated instance. Record included DB/config/key material; database backups do not include media. Protect private state and redact secrets at their source.

## Frontend implementation

Keep Svelte and build from real API contracts. Establish one generated Rust/API-to-TypeScript contract as the API grows; avoid independently maintained response shapes. Separate server data/cache from local UI state, and colocate feature components and data access. TanStack Query and WebSockets are planned options, not existing dependencies; polling is sufficient for initial live status.

Ship accessible loading, error and empty states, keyboard interactions and meaningful failure/progress feedback. Verify user workflows through real endpoints; a rendered page or successful bundle is not evidence that import, monitoring or search works.

## Verification and completion

Run checks appropriate to changed code, from the repository root:

```sh
cargo fmt --check
cargo check --locked
cargo test --locked
npm --prefix frontend run build
git diff --check
```

Frontend dependencies must be installed before building. The current frontend has no test/type-check script; the Vite build does not establish either. Documentation-only changes need link/content checks and `git diff --check`, not application builds.

For behavior changes, leave focused runnable regression evidence for the affected contract. At the corresponding slice gates, cover:

- Fresh/upgraded DBs; Sonarr-only, Radarr-only and combined imports; ID collisions, unknown settings, absent files, rollback and reruns.
- Separate TV/movie parser and decision cases, including ambiguous years, numbering/packs, availability transitions and manual-search exceptions.
- A shared client processing both domains without cross-imports; restart/retry, path mapping, failed upgrades and interruption between filesystem/DB steps.
- Collections/list defaults and exclusions, shared UI media filters, secret redaction and isolated backup/restore.

Track each workflow/controller, command, concrete provider and route with reference path/commit, implementation owner/API/UI, slice, status (`Missing`, `Partial`, `Verified`, `Blocked`) and test evidence. A shared implementation satisfies both domains only after both contracts pass. External-service blockers remain incomplete; a passing build alone never establishes parity.

Every PR body must end with a **Not claimed** section stating what was not tested, what verification does not prove, residual risks and newly introduced state/dependencies. Distinguish verified-from-source findings from observed-once behavior, and record reasoning beside changed test assertions.
