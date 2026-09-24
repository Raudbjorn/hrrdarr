# Ultrasonic Agents for Claude Code

This directory contains agent definitions for the Ultrasonic PVR project. Each agent is a specialized subagent that can be invoked via the agent system.

## Agent Registry (13 agents)

### schema-migration

**Role:** Database Schema Migration Specialist  
**Description:** Expands the 3-table libSQL schema into the full 40+ table relational model. Handles migrations, foreign keys, sqlx compile-time checks, and libSQL/Turso integration.  
**Slice:** 0 (Critical Path - Foundation)  
**File:** `schema-migration.md`

### provider-framework

**Role:** Provider Abstraction Framework Architect  
**Description:** Builds the trait-based provider system (Indexer, DownloadClient, Notification, Metadata, ImportList) with factory, config storage, and credential encryption.  
**Slice:** 1 (Critical Path)  
**Depends on:** schema-migration  
**File:** `provider-framework.md`

### indexer-providers

**Role:** Torznab/Newznab/Jackett Indexer Implementation  
**Description:** Implements indexer providers per Torznab spec: RSS sync, search, capability detection, category mapping, release parsing.  
**Slice:** 1 (Parallel after provider-framework)  
**Depends on:** provider-framework  
**File:** `indexer-providers.md`

### download-client-providers

**Role:** qBittorrent/SABnzbd/Transmission Download Client Implementation  
**Description:** Implements download client providers: add torrent/magnet/nzb, status queries, priority, categories, pause/resume, completion detection.  
**Slice:** 1 (Parallel after provider-framework)  
**Depends on:** provider-framework  
**File:** `download-client-providers.md`

### command-queue

**Role:** Command Queue & Scheduler Engineer  
**Description:** Persistent command queue, scheduler, TaskManager equivalent. Handles priority, retries, concurrency, scheduled jobs (RSS, Refresh, Housekeeping, Backup).  
**Slice:** 2 (Critical Path)  
**Depends on:** schema-migration, provider-framework  
**File:** `command-queue.md`

### search-grab-pipeline

**Role:** Search & Grab Pipeline Engineer  
**Description:** Core PVR loop: release search → decision engine (quality profiles, custom formats) → grab → download client. Highest risk: 73KB parser port.  
**Slice:** 3 (Critical Path)  
**Depends on:** schema-migration, indexer-providers, download-client-providers, command-queue  
**File:** `search-grab-pipeline.md`

### import-pipeline

**Role:** Import Pipeline Engineer  
**Description:** Closes the loop: completed download → EpisodeFile creation → file move/copy/hardlink → rename → library update. Fixes the two critical bugs in main.rs.  
**Slice:** 4 (Critical Path - First Working Automation)  
**Depends on:** schema-migration, search-grab-pipeline, command-queue  
**File:** `import-pipeline.md`

### ui-features

**Role:** UI Feature Parity Engineer  
**Description:** Svelte pages for Calendar, Wanted, Activity, Settings, System. TanStack Query + Svelte stores, real-time via WebSocket, type-safe API client.  
**Slice:** 5 (Parallel after API stable)  
**Depends on:** search-grab-pipeline, import-pipeline, command-queue  
**File:** `ui-features.md`

### notification-providers

**Role:** Notification Provider Implementation  
**Description:** Implements Discord, Telegram, Webhook, Email providers with Handlebars templating and event routing.  
**Slice:** 5 (Parallel)  
**Depends on:** provider-framework  
**File:** `notification-providers.md`

### metadata-providers

**Role:** Metadata Provider Implementation  
**Description:** Implements TVDB, TMDB, TVMaze, NFO generation, artwork download/resize, scheduled refresh.  
**Slice:** 5 (Parallel)  
**Depends on:** provider-framework, import-pipeline  
**File:** `metadata-providers.md`

### import-list-providers

**Role:** Import List Provider Implementation  
**Description:** Implements Trakt, Plex, MyAnimeList providers with OAuth, series matching, scheduled sync.  
**Slice:** 5 (Parallel)  
**Depends on:** provider-framework, schema-migration  
**File:** `import-list-providers.md`

### testing-verification

**Role:** Testing & Verification Specialist  
**Description:** Integration tests, contract tests, migration testing, parser test corpus, CI/CD pipeline. Runs parallel to all agents.  
**Slice:** All (Continuous)  
**File:** `testing-verification.md`

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
