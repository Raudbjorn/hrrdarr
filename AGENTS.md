# Ultrasonic Agents for Codex

This file defines the agent personas available to Codex for the Ultrasonic project. Each agent is a specialized subagent that can be invoked via the `@agent` syntax.

## Agent Registry (12 agents)

### schema-migration

**Role:** Database Schema Migration Specialist
**Description:** Expands the 3-table libSQL schema into the full 40+ table relational model. Handles migrations, foreign keys, sqlx compile-time checks, and libSQL/Turso integration.
**When to invoke:** Any database schema work, migration writing, repository pattern implementation, libSQL capability verification.
**Tools:** Read, Write, Edit, Bash, Glob, Grep

### provider-framework

**Role:** Provider Abstraction Framework Architect
**Description:** Builds the trait-based provider system (Indexer, DownloadClient, Notification, Metadata, ImportList) with factory, config storage, and credential encryption.
**When to invoke:** Adding new provider types, framework design, config/credential handling, health check interfaces.
**Tools:** Read, Write, Edit, Bash, Glob, Grep

### indexer-providers

**Role:** Torznab/Newznab/Jackett Indexer Implementation
**Description:** Implements indexer providers per Torznab spec: RSS sync, search, capability detection, category mapping, release parsing.
**When to invoke:** Indexer protocol work, RSS parsing, search implementation, release normalization.
**Tools:** Read, Write, Edit, Bash, Glob, Grep

### download-client-providers

**Role:** qBittorrent/SABnzbd/Transmission Download Client Implementation
**Description:** Implements download client providers: add torrent/magnet/nzb, status queries, priority, categories, pause/resume, completion detection.
**When to invoke:** Download client protocol work, torrent lifecycle, WebAPI integration.
**Tools:** Read, Write, Edit, Bash, Glob, Grep

### command-queue

**Role:** Command Queue & Scheduler Engineer
**Description:** Persistent command queue, scheduler, TaskManager equivalent. Handles priority, retries, concurrency, scheduled jobs (RSS, Refresh, Housekeeping, Backup).
**When to invoke:** Background job infrastructure, command persistence, scheduling, queue management.
**Tools:** Read, Write, Edit, Bash, Glob, Grep

### search-grab-pipeline

**Role:** Search & Grab Pipeline Engineer
**Description:** Core PVR loop: release search → decision engine (quality profiles, custom formats) → grab → download client. Highest risk: 73KB parser port.
**When to invoke:** Search logic, release parsing, quality profiles, decision specifications, grab handoff.
**Tools:** Read, Write, Edit, Bash, Glob, Grep

### import-pipeline

**Role:** Import Pipeline Engineer
**Description:** Closes the loop: completed download → EpisodeFile creation → file move/copy/hardlink → rename → library update. Fixes the two critical bugs in main.rs.
**When to invoke:** Import logic, EpisodeFile model, file operations, rename, bug fixes for import_execute/migration.
**Tools:** Read, Write, Edit, Bash, Glob, Grep

### ui-features

**Role:** UI Feature Parity Engineer
**Description:** Svelte pages for Calendar, Wanted, Activity, Settings, System. TanStack Query + Svelte stores, real-time via WebSocket, type-safe API client.
**When to invoke:** Frontend pages, state management, real-time updates, API client generation, component library.
**Tools:** Read, Write, Edit, Bash, Glob, Grep

### notification-providers

**Role:** Notification Provider Implementation
**Description:** Implements Discord, Telegram, Webhook, Email providers with Handlebars templating and event routing.
**When to invoke:** Notification delivery, templating, webhook/bot APIs, event routing.
**Tools:** Read, Write, Edit, Bash, Glob, Grep

### metadata-providers

**Role:** Metadata Provider Implementation
**Description:** Implements TVDB, TMDB, TVMaze, NFO generation, artwork download/resize, scheduled refresh.
**When to invoke:** Metadata scraping, NFO generation, artwork handling, Plex/Jellyfin compatibility.
**Tools:** Read, Write, Edit, Bash, Glob, Grep

### import-list-providers

**Role:** Import List Provider Implementation
**Description:** Implements Trakt, Plex, MyAnimeList providers with OAuth, series matching, scheduled sync.
**When to invoke:** External list sync, OAuth flows, series matching, scheduled imports.
**Tools:** Read, Write, Edit, Bash, Glob, Grep

### testing-verification

**Role:** Testing & Verification Specialist
**Description:** Integration tests, contract tests, migration testing, parser test corpus, CI/CD pipeline. Runs parallel to all agents.
**When to invoke:** Test writing, CI/CD setup, contract testing, migration verification, E2E flows.
**Tools:** Read, Write, Edit, Bash, Glob, Grep

## Dependency Graph (Critical Path)

```
Slice 0: schema-migration (2 weeks)           ← START HERE
    ↓
Slice 1: provider-framework (2 weeks)
    ↓         ↙               ↘
    indexer-providers    download-client-providers  (2 weeks, parallel)
    ↓
Slice 2: command-queue (2 weeks)
    ↓
Slice 3: search-grab-pipeline (3 weeks)      ← HIGHEST RISK
    ↓
Slice 4: import-pipeline (1 week)            ← FIRST WORKING AUTOMATION
    ↓
Slice 5+: All parallel agents:
    ├── ui-features (4 weeks)
    ├── notification-providers (2 weeks)
    ├── metadata-providers (2 weeks)
    └── import-list-providers (2 weeks)

Total Critical Path: ~10 weeks to working automation
Parallel After Slice 4: All remaining providers, UI pages, jobs, metadata
```

## Key Files to Know

- `src/main.rs` — Current Axum API (4 endpoints, 2 bugs)
- `src/db/schema.rs` — Rust type definitions (to be created)
- `frontend/src/App.svelte` — Current UI (Series list only)

## License Notice

Sonarr is GPLv3. Treat as requirements document only. Implement from:

- Torznab spec (github.com/Sonarr/Torznab-Spec)
- qBittorrent WebAPI / SABnzbd API documentation
- Observed API behavior
- PVR domain knowledge

Do NOT port C# logic directly.
