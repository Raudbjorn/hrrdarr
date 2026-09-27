# Targeted search and selected-release grab

Native targeted search supports one episode or movie and one explicitly chosen indexer/client pair. `automatic` searches all bounded continuations and selects the best eligible result; `interactive` retains results without authorizing a client submission until the user selects one. Both routes are explicitly user-invoked. Automatic *selection* does not mean a background trigger: both use the persisted `user_search` decision context, including its availability/delay exception. RSS retains its background availability, monitoring and configured-delay checks.

Identity, requested target, media category, current quality/profile, size, cutoff, blocklist and client capability checks still apply. This accepted-result selection is not a forced override. Upstream release controllers additionally expose target/quality/language overrides and release push; those remain required future work. Episode/season/series/missing/cutoff command fanout, full ranking/custom-format breadth, packs and other clients also remain incomplete. These native routes do not claim V3 wire compatibility or upstream runtime equivalence.

## API

- `POST /api/v1/search/commands`: `request_id` UUID, `mode`, existing typed `target` (`{"media_type":"episode","id":1}` or `{"media_type":"movie","id":1}`), indexer/client UUIDs and revisions, and normal/high priority. Returns 202 with durable command state. The same UUID and exact request returns the existing command before revalidating mutable configuration; a changed request with that UUID conflicts.
- `GET /api/v1/search/commands`: optional `target_type=episode|movie` plus `target_id` together, optional status, limit 1–100 and offset 0–10000. Detail uses `/{id}`. Progress includes fetched count, fetch completion, selected candidate UUID and its current status/error/reasons.
- `GET /api/v1/search/commands/{id}/results`: bounded pagination over retained safe metadata and decision snapshots. No locator, credential or raw feed payload is returned.
- `POST /api/v1/search/results/{id}/grab` with `{}`: explicitly selects one accepted interactive offer. It accepts no replacement URL, target or decision context. Repeating the same selection returns the same receipt even after expiry or provider disablement. Selecting another result from that command conflicts.
- `POST /api/v1/search/commands/{id}/cancel`: cancels queued/running/retry-wait search before handoff. A terminal command cannot revoke an already-selected submission intent.
- `DELETE /api/v1/search/commands/{id}`: removes terminal history and unselected offers only when there is no selected receipt. Selected command/result provenance remains retained. The older RSS candidate deletion endpoint also returns conflict for retained search receipts.

API requests have an 8 KiB body limit, five-second deadline and 1 MiB response limit. A write timeout has an uncertain outcome: read the request UUID or repeat the exact request/selection. No automatic UI retry should manufacture another request UUID.

## Selection and dispatch

The complete capture is limited to 10 pages, 1000 releases, 4 MiB serialized private release data and a 35-second network deadline. Unsupported or malformed feed warnings and an unfinished continuation at those limits fail the capture without a grab. Later pages participate in selection; the first eligible row is not automatically the winner.

Automatic ranking uses the existing profile's ordered quality groups, with group members sharing a rank. Higher rank wins, then seed count, publication time and a stable discovery fingerprint. Numeric quality IDs are not ranks. Only accepted, exact singleton targets with supported torrent facts participate. If the selected release later fails preparation or submission, the receipt reports that failure; the command does not silently choose another result or claim a successful grab.

A successful command means capture and, for automatic mode, selection finished. It does not mean the client accepted the torrent. `selected_candidate_status`, error and reasons reflect the current receipt. Interactive stored decisions are snapshots; current facts are reevaluated when selected and again both before preparation and inside the final intent transaction. Captured catalog/episode identity prevents a numeric target from silently changing meaning.

Selection decrypts the retained offer and re-encrypts it for the existing candidate's AEAD context. Candidate creation, immutable result-to-candidate linkage and removal of the original encrypted payload commit together. The existing submission journal persists identity before POST, retains global hash/target ownership, and reconciles uncertain outcomes read-only. Search receipts are explicitly tagged with their command/result origin rather than appearing as invented RSS sync commands. Completed processing can consume their trusted observed receipts through the existing import workflow.

## Storage and retention

Migration 0027 retains request UUID, server-owned context, captured target facts and provider revisions. Unselected offers expire after 1800 seconds. The shared worker deletes at most 1024 expired, unselected terminal offers per sweep; selected provenance is never expired back into dispatch authority. A command's `fetched` count records capture, while a results page's total counts currently retained offers.

All eight command families (including manual-import, quality-reset and rescan commands) share the existing 1024-command capacity pool, counting only active (queued, running or retry-waiting) rows — a command freeing its slot on completion, failure or cancellation, not requiring deletion. Offers and candidates each have their own, separate 1024-row caps (unrelated to the command pool and not affected by that active-only accounting), with one combined 16 MiB encrypted-payload cap enforced in storage. Selection transfers payload ownership atomically without bypassing that cap. A configured encryption key is required; there is no plaintext fallback. Retained selected receipts and their provenance require future explicit lifecycle work; capacity errors remain visible.

The pinned command services distinguish manual-trigger automatic selection from interactive listing, and the release controllers separately support forced overrides. That requirement inventory governs future breadth; this unit implements the concrete singleton accepted-result workflows described here.

## Runnable evidence

`cargo test --locked --test search_commands` passes eight HTTP tests using scratch databases and owned localhost indexer/client mocks. [The fixtures](../tests/search_commands.rs) cover both domains, quality ranking across continuation pages, exact interactive selection, persisted user-search availability context after prior RSS rejection, request/selection replay, expired offers, provider and target drift, identity changes during remote preparation, wrong categories and movie remakes, failed continuation with no partial grab, and database reopen with retained offers or uncertain submissions. Reopened uncertain submissions reconcile without another add request. `cargo test --locked --test release_decisions` also passes the existing shared evaluator regression after extracting profile ranking.

These tests use seeded local library/profile facts and mock transports; they do not establish live-provider equivalence, full Add-to-Import automation, forced override behavior or the remaining search fanout and ranking breadth.

## Numbering admission

Returned TV releases and downloaded files use the same bounded daily/absolute identity resolver. Daily filenames are accepted for daily and anime series, while Standard rejects them. A sole exact air-date match is valid, including a special; multiple matches exclude season-zero specials and require exactly one regular episode. Only that selected episode contributes monitoring, runtime, airing and existing-file facts. Absolute numbering is not restricted to anime: any configured series type can match one catalog absolute number. Missing/duplicate final matches remain rejected, and import revalidation reads current metadata after grab.

Indexer query families remain selected by configured series type. This change does not enable scene searches, standard-style scene-file imports, season/absolute packs or aliases. Downloaded matching retains the native `use_scene_numbering` switch for unique scene-absolute lookup followed by ordinary-absolute fallback; upstream `sceneSource` and scene-origin mapping are not fully represented. Local library rescans intentionally use ordinary numbering without that scene-first lookup. These distinctions remain Partial, and no schema or dependency is added.
