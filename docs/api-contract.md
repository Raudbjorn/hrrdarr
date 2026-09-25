# Generated API contract

Rust handler DTOs own the wire shapes. `ts-rs` 12.0.1 derives their TypeScript declarations; `src/api_contract.rs` registers the shipped roots and follows nested dependencies. The deterministic output is committed at `frontend/src/lib/api.generated.ts`. No build script or test rewrites that file. `cargo run` still starts hrrdarr; generation requires the explicit binary.

```sh
cargo run --locked --bin generate-api
cargo run --locked --bin generate-api -- --check
npm --prefix frontend run generate:api
npm --prefix frontend run check:api
npm --prefix frontend run check
npm --prefix frontend test
cargo test --locked --test api_contract
```

Install the locked frontend dependencies first (`npm --prefix frontend ci`): the Rust contract test compiles real local HTTP response specimens with the installed TypeScript compiler. `--check` fails on a missing or stale artifact without modifying it. An optional path after `--check` supports isolated artifact checks. Sorted declarations contain no timestamps or environment-dependent configuration.

## Shipped surface

All paths below start with `/api/v1`. `{media}` is `tv` or `movies`; library `{base}` is `tv/series` or `movies`. Query DTOs describe URL scalar values, not JSON request bodies. CSV selectors remain strings. IDs in route segments are positive domain-qualified integers; operation IDs are UUID strings.

| Route and method | Request | Success body |
| --- | --- | --- |
| GET `/blocklist` | `BlocklistQuery` | `ApiPage<BlocklistEntry>` (typed imported source facts) |
| DELETE `/blocklist/{application}/{fingerprint}/{source_id}` | none | empty (204; durable provenance tombstone) |
| DELETE `/blocklist/bulk` | `BlocklistRemoval` | empty (204; atomic, max 100 explicit identities) |
| POST `/blocklist/clear-commands` | `BlocklistClearInput` | `BlocklistClearCommand` (202; active domain deduplication) |
| GET `/blocklist/clear-commands` | `CommandQuery` | `ApiPage<BlocklistClearCommand>` |
| GET `/blocklist/clear-commands/{id}` | none | `BlocklistClearCommand` |
| POST `/blocklist/clear-commands/{id}/cancel` | none | `BlocklistClearCommand` |
| DELETE `/blocklist/clear-commands/{id}` | none | empty (204; terminal command history only) |
| GET `/history` | `HistoryQuery` | `ApiPage<HistoryEvent>` (tagged native receipts and source snapshot facts) |
| POST `/commands` | `CommandInput` | `Command` (202; active scope deduplication) |
| GET `/commands` | `CommandQuery` | `ApiPage<Command>` |
| GET `/commands/{id}` | none | `Command` |
| POST `/commands/{id}/cancel` | none | `Command` |
| DELETE `/commands/{id}` | none | empty (204; terminal history only) |
| POST `/metadata-refresh/commands` | `MetadataCommandInput` | `MetadataCommand` (202; active typed-target deduplication) |
| GET `/metadata-refresh/commands` | `MetadataCommandQuery` | `ApiPage<MetadataCommand>` |
| GET `/metadata-refresh/commands/{id}` | none | `MetadataCommand` |
| POST `/metadata-refresh/commands/{id}/cancel` | none | `MetadataCommand` |
| DELETE `/metadata-refresh/commands/{id}` | none | empty (204; terminal history only) |
| GET `/download-refresh/schedules` | none | `RefreshSchedule[]` |
| PUT `/download-refresh/schedules` | `RefreshScheduleInput` | `RefreshSchedule` |
| DELETE `/download-refresh/schedules` | `RefreshScheduleDelete` | empty (204) |
| GET `/queue` | `QueueQuery` | `QueueSnapshot` (explicitly unassociated client observations) |
| GET `/series` | none | `LegacySeries[]` |
| GET `/series/{id}/episodes` | none | `LegacyEpisode[]` |
| GET `/filesystem` | `FilesystemLookup` query | `FilesystemContents` |
| GET `/filesystem/type` | `FilesystemPath` query | `FilesystemType` (UI classification hint) |
| GET `/filesystem/media-files` | `FilesystemMediaQuery` query | `FilesystemMediaFiles` |
| POST `/imports` | `ImportInput` (legacy episode or typed manual request) | `Operation` (202 preview) |
| GET `/imports/{id}` | none | `Operation` |
| POST `/imports/{id}/execute` | none | `Operation` (200 after execution or completed replay) |
| POST `/migrations` | `SnapshotOptions` query; raw SQLite backup bytes | `SnapshotReport` |
| GET `/{base}` | `LibraryQuery` | `LibraryPage` |
| POST `/{base}` | `LibraryCreate` | `LibraryItem` (201) |
| GET `/{base}/{id}` | none | `LibraryItem` |
| PUT `/{base}/{id}` | `LibraryPatch` | `LibraryItem` |
| PUT `/{base}/bulk` | `LibraryBulk` (`items` with `id` and `patch`) | `LibraryItem[]` |
| PUT `/{base}/editor` | `LibraryEditor` | `LibraryItem[]` |
| GET `/{media}/root-folders` | `RootQuery` | `ApiPage<RootFolder>` |
| POST `/{media}/root-folders` | `RootInput` | `RootFolder` (201) |
| GET `/{media}/root-folders/{id}` | none | `RootFolder` |
| DELETE `/{media}/root-folders/{id}` | none | empty (204; configuration only) |
| GET `/{media}/remote-path-mappings` | `MappingQuery` | `ApiPage<Mapping>` |
| POST `/{media}/remote-path-mappings` | `MappingInput` | `Mapping` (201) |
| GET `/{media}/remote-path-mappings/{id}` | none | `Mapping` |
| PUT `/{media}/remote-path-mappings/{id}` | `MappingUpdate` | `Mapping` |
| DELETE `/{media}/remote-path-mappings/{id}` | `MappingRevision` query | empty (204; configuration only) |
| POST `/{media}/remote-path-mappings/resolve` | `ResolveInput` | `Resolution` |
| GET `/providers/schema` | `ProviderFilter` query | `ProviderSchema` |
| PUT `/providers/bulk` | `ProviderBulkUpdate` | `ProviderBulkResult` |
| DELETE `/providers/bulk` | `ProviderSelection` | 204, no body |
| POST `/providers/testall` | `ProviderFilter` body | `ProviderBatchResult` |
| POST `/providers/test-draft` | `ProviderDraftInput` (required nullable source; no query flags) | `ProviderDraftResult` (unsaved probe; no persisted observation) |
| POST `/providers/indexer-categories` | `CategoryDiscoveryInput` (minimal connection; required nullable source; no query flags) | `CategoryDiscoveryResult` (advertised or explicit standard fallback; no writes) |
| POST `/providers/{id}/path-preview` | `ProviderPathInput` | `ProviderPathPreview` |
| GET `/episodes` | `EpisodeQuery` | `ApiPage<Episode>` |
| GET `/episodes/{id}` | none (all projections included) | `Episode` |
| PUT `/episodes/{id}` | `EpisodeMonitor` | `Episode` |
| PUT `/episodes/monitor` | `EpisodeMonitorMany` body; `EpisodeIncludes` query | `Episode[]` |
| GET `/{media}/files` | `FileQuery` | `ApiPage<FileResource>` |
| GET `/{media}/files/{id}` | none | `TvFileResource` or `MovieFileResource` |
| PUT `/{media}/files/{id}` | `FilePatch` | matching file resource |
| PUT `/{media}/files/bulk` | `FileBulk` (`files`, each a flattened `FileUpdate`) | `FileResource[]` |
| PUT `/{media}/files/editor` | `FileEditor` (flattened patch) | `FileResource[]` |
| GET `/{media}/quality-definitions` or `/defaults` | none | `QualityDefinition[]` |
| GET `/{media}/quality-definitions/limits` | none | `QualityDefinitionLimits` |
| GET `/{media}/quality-definitions/{id}` | none | `QualityDefinition` |
| PUT `/{media}/quality-definitions/{id}` | `QualityDefinitionUpdate` (body ID matches route) | `QualityDefinition` |
| PUT `/{media}/quality-definitions/bulk` | raw `QualityDefinitionUpdate[]` | `QualityDefinition[]` |
| POST `/{media}/quality-definitions/reset` | `QualityDefinitionReset` | `QualityDefinition[]` |
| GET `/{media}/quality-profiles` | `QualityProfileQuery` | `QualityProfilePage` |
| POST `/{media}/quality-profiles` | `QualityProfileInput` | `QualityProfile` (201) |
| GET `/{media}/quality-profiles/{id}` | none | `QualityProfile` |
| PUT `/{media}/quality-profiles/{id}` | `QualityProfileInput` | `QualityProfile` |

Other listed successes return 200. Native errors use `ApiErrorEnvelope` (`error.code`, `error.message`); main import/snapshot errors use `LegacyError` (`error` string). Framework extractor failures, unknown routes and unsupported methods can have non-JSON bodies. Status/range/selector/size constraints remain in handlers and the feature API documents: [library](library-api.md), [episodes](episode-api.md), [files](file-api.md), [qualities](quality-api.md), [snapshots](snapshot-import.md). The table documents routing; the generator exports DTOs, not a generated route client.

## Nulls, omission and numeric safety

Response `Option` fields are normally required and nullable: legacy year/poster/file path and profile leaf size fields must be present even when unknown. Explicit serialization omissions stay optional, including quality modifiers and selectable episode projections. Episode images can be omitted, requested-but-unknown `null`, or an observed array (including empty). Cover URL fields preserve omission separately from explicit `null`.

Library and file patches preserve omitted values; supported explicit nulls clear them. Monitoring null is rejected by library validation. Quality-definition updates instead treat omitted sizes as null. Profile input size fields accept omission; profile responses always emit their nullable fields. Domain restrictions, immutable identities, allowed ranges and unsupported effect fields remain server validations. A common file patch type does not authorize movie-only fields on TV routes or vice versa.

JSON keeps its existing numeric wire representation. Generated `number` is not an assertion that all Rust `i64` values are precisely representable in JavaScript. The frontend boundary rejects non-finite numbers and integers outside `Number.isSafeInteger` on responses and outgoing bodies/IDs. Thus integers beyond ±9,007,199,254,740,991 (including large sizes or duration ticks) fail visibly instead of being used after rounding. This is a client limitation, not a server storage restriction or lossless-number protocol. Other clients must apply the same policy or use a lossless decoder.

## Evidence and limits

`tests/api_contract.rs` checks committed drift, deterministic output, stale CLI failure, actual serde patch/flatten/unknown-field behavior, required-null and omitted fields, and actual merged-router responses against generated types using TypeScript `satisfies`. Negative compiler specimens check required nullable fields and closed domains. Existing endpoint regressions continue checking validation and transactions; frontend tests cover mocked transport, errors and numeric bounds.

The `no-serde-warnings` feature suppresses unsupported-attribute warnings after review: custom presence deserializers have explicit TS field annotations and serialization tests; `deny_unknown_fields` is enforced by serde, not fully expressible in structural TypeScript. UUIDs serialize as strings. The registry uses real handler types; separate profile input/output types represent their different omission rules.

These declarations are compile-time contracts, not runtime response validators. The frontend checks transport status, JSON, byte/time bounds and numeric safety, then trusts its own backend's structural contract. The specimen test samples shipped shapes; it does not exhaust every possible field value or establish Sonarr/Radarr V3 compatibility. No migrations or physical import/deletion behavior are introduced. Code generation adds the pinned ts-rs dependency and its locked transitive packages.

Durable command and queue semantics, ownership, limits and verification scope are documented in [download refresh](download-refresh.md).

Native history filtering, event semantics and evidence limits are documented in [history](history.md).

Durable single-target catalog refresh has a separate additive command surface; its typed targets, shared worker limits and atomic reconciliation contract are documented in [metadata refresh](metadata-refresh.md). Existing download-command DTOs remain unchanged.

Imported source blocklist read/removal contracts and replay behavior are documented in [blocklist management](blocklist.md). No automatic release-decision enforcement or failed-download producer is implied.
