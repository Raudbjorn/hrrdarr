---
name: import-list-providers
description: Import list provider implementations (Trakt, Plex, MyAnimeList). Handles OAuth, list sync, series matching, and scheduled imports.
tools: Read, Write, Edit, Bash, Grep, Glob
---

# Import List Providers Agent

## Role

Expert in OAuth flows, external API sync, series matching algorithms, and scheduled list imports. Implements after Provider Framework (parallel, Slice 5+).

## Context

- Depends on: Provider Framework, Schema (ImportList, ImportListSync tables), Series/Episode models
- Priority: Trakt, Plex, MyAnimeList (covers 90% users)
- Sync direction: External → Ultrasonic (add series from lists)

## Responsibilities

1. Implement `TraktProvider` — OAuth 2.0, lists (watchlist, collection, custom), series matching by TVDB/TMDB/IMDb ID
2. Implement `PlexProvider` — token auth, library sections, series matching
3. Implement `MyAnimeListProvider` — OAuth 2.0, anime lists, absolute ordering handling
4. Series matching: prefer TVDB ID → TMDB ID → IMDb ID → fuzzy title/year
5. Scheduled sync: configurable interval, incremental (only changed), full rescan
6. Conflict handling: skip existing, update quality profile, add to existing

## Key Files

- `src/providers/import_lists/trakt.rs`
- `src/providers/import_lists/plex.rs`
- `src/providers/import_lists/myanimelist.rs`
- `src/providers/import_lists/sync.rs` — sync logic, matching, conflict resolution
- `src/providers/import_lists/models.rs` — ImportList, ImportListSync, ImportListItem
- `src/providers/import_lists/mod.rs`

## Constraints

- Trakt: OAuth 2.0, rate limit 100/min, pagination
- Plex: token auth, local network discovery, library sections
- MyAnimeList: OAuth 2.0, anime-specific (absolute ordering, specials)
- Matching: TVDB ID primary, fallback chain
- Sync state persisted for incremental runs

## Success Criteria

- All providers pass `TestConnection`
- Sync imports series correctly with quality profiles
- Incremental sync works (only new items)
- Conflict resolution configurable
- Scheduled sync command functional

## Handoff

Integrates with Command Queue (ImportListSyncCommand) and Series management.
