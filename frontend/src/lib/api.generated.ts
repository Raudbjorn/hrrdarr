// Generated from the Rust HTTP DTOs by ts-rs. Do not edit.
// Regenerate: cargo run --locked --bin generate-api
// JSON i64/u64 remain numbers on the wire. Reject integers outside Number.isSafeInteger
// at the API boundary before using IDs, sizes or duration ticks; these types are not validators.
// URL-query properties are omittable scalars; nullability below describes JSON bodies/resources.

export type ApiErrorDetail = { code: string, message: string, };

export type ApiErrorEnvelope = { error: ApiErrorDetail, };

export type ApiPage<T> = { items: Array<T>, total: number, limit: number, offset: number, };

export type AutoTag = { id: number, media_type: MediaDomain, name: string, remove_tags_automatically: boolean, tag_ids: Array<number>, specifications: Array<AutoTagSpecification>, };

export type AutoTagCondition = { "implementation": "genre" } & AutoTagTextValues | { "implementation": "keyword" } & AutoTagTextValues | { "implementation": "network" } & AutoTagTextValues | { "implementation": "studio" } & AutoTagTextValues | { "implementation": "original_country" } & AutoTagTextValues | { "implementation": "year" } & AutoTagRange | { "implementation": "runtime" } & AutoTagRange | { "implementation": "root_folder" } & AutoTagRootFolder | { "implementation": "original_language" } & AutoTagIntegerValues | { "implementation": "quality_profile" } & AutoTagIntegerValues | { "implementation": "series_type" } & AutoTagTextValues | { "implementation": "status" } & AutoTagTextValues | { "implementation": "minimum_availability" } & AutoTagTextValues | { "implementation": "monitored" } & AutoTagFlag | { "implementation": "collection" } & AutoTagIntegerValues;

export type AutoTagFieldKind = "text_list" | "integer_list" | "integer" | "text" | "boolean" | "enum_list";

export type AutoTagFlag = { value: boolean, };

export type AutoTagInput = { name: string, remove_tags_automatically: boolean, tag_ids: Array<number>, specifications: Array<AutoTagSpecificationInput>, };

export type AutoTagIntegerValues = { values: Array<number>, };

export type AutoTagRange = { min?: number | null, max?: number | null, };

export type AutoTagRootFolder = { path: string, };

export type AutoTagSchemaEntry = { implementation: string, label: string, fields: Array<AutoTagSchemaField>, };

export type AutoTagSchemaField = { name: string, label: string, kind: AutoTagFieldKind, required: boolean, options: Array<string>, };

export type AutoTagSpecification = { id: number, name: string, negate: boolean, required: boolean, condition: AutoTagCondition, };

export type AutoTagSpecificationInput = { name: string, negate: boolean, required: boolean, condition: AutoTagCondition, };

export type AutoTagTextValues = { values: Array<string>, };

export type BlocklistClearCommand = { id: string, name: BlocklistClearName, target: BlocklistClearTarget, priority: CommandPriority, status: CommandStatus, attempts: number, next_attempt_at: number, created_at: number, started_at: number | null, completed_at: number | null, error_code: string | null, records_removed: number, };

export type BlocklistClearInput = { target: BlocklistClearTarget, priority: CommandPriority, };

export type BlocklistClearName = "clear_blocklist";

export type BlocklistClearTarget = { media_type: MediaDomain, };

export type BlocklistEntry = { id: BlocklistIdentity, origin: BlocklistOrigin, target: BlocklistTarget, occurred_at: string, published_at: string | null, source_title: string, protocol: BlocklistProtocol | null, size_bytes: number | null, quality: FileQuality | null, languages: Array<number> | null, };

export type BlocklistIdentity = { application: SnapshotApplication, fingerprint: string, source_id: number, };

export type BlocklistOrigin = "source_snapshot";

export type BlocklistProtocol = "unknown" | "usenet" | "torrent";

export type BlocklistQuery = { media_type?: MediaDomain, series_ids?: string, movie_ids?: string, protocols?: string, sort?: BlocklistSort, sort_direction?: BlocklistSortDirection, limit?: number, offset?: number, };

export type BlocklistRemoval = { ids: Array<BlocklistIdentity>, };

export type BlocklistSort = "date" | "source_title";

export type BlocklistSortDirection = "asc" | "desc";

export type BlocklistTarget = { "media_type": "tv", series_id: number, episode_ids: Array<number>, } | { "media_type": "movies", movie_id: number, };

export type CalendarDiagnostics = { invalid_dates: number, date_only_air_dates: number | null, };

export type CalendarFeedQuery = { past_days?: number, future_days?: number, unmonitored?: boolean, tags?: string, specials?: boolean, release_types?: string, };

export type CalendarPage<T> = { media_type: MediaDomain, start: string, end: string, first_date: string, end_date: string, include_unmonitored: boolean, include_specials: boolean, release_types: Array<MovieReleaseType>, tags: Array<number>, items: Array<T>, total: number, limit: number, evaluated_at: number, diagnostics: CalendarDiagnostics, };

export type CalendarQuery = { start: string, end: string, unmonitored?: boolean, tags?: string, specials?: boolean, release_types?: string, };

export type CalendarRuntimeSource = "series" | "episode";

export type CandidateOrigin = { "kind": "rss" } | { "kind": "search", command_id: string, result_id: string, mode: SearchMode, };

export type Capabilities = { max_limit: number, default_limit: number, search: SearchCapability, tv: SearchCapability, movies: SearchCapability, categories: Array<IndexerCategory>, };

export type CategoryDiscoveryError = { code: string, message: string, retry_after_seconds: number | null, };

export type CategoryDiscoveryInput = { media_type: MediaDomain, source: ProviderDraftSource | null, connection: IndexerConnection, };

export type CategoryDiscoveryResult = { source: ProviderDraftSource | null, media_type: MediaDomain, origin: CategoryOrigin, options: Array<CategoryOption>, discovery_error: CategoryDiscoveryError | null, };

export type CategoryOption = { id: number, parent_id: number | null, label: string, };

export type CategoryOrigin = "advertised" | "standard_fallback";

export type ClientAttempt = { ordinal: number, selection_round: number, client_id: string, client_revision: number, state: ClientAttemptState, error_code: string | null, created_at: number, updated_at: number, };

export type ClientAttemptState = "selected" | "prepared" | "dispatching" | "definitely_not_submitted" | "observed" | "unknown";

export type ClientSelection = { "kind": "default", } | { "kind": "explicit", provider_id: string, provider_revision: number, };

export type ClientTest = { api_version: string, application_version: string, domains: Array<MediaDomain>, missing_categories: Array<string>, queueing_enabled: boolean, };

export type CollectionAssociation = { "state": "absent" } | { "state": "clear" } | { "state": "present", "value": CollectionRef };

export type CollectionBulkPatch = { ids: Array<number>, patch: CollectionPatch, };

export type CollectionMovie = { metadata_id: number, tmdb_id: number | null, title: string, year: number | null, 
/**
 * Library movie id when the movie is in the library; null for catalog-only members.
 */
movie_id: number | null, monitored: boolean | null, };

export type CollectionPatch = { monitored?: boolean, search_on_add?: boolean, quality_profile_id?: number | null, root_folder_id?: number | null, minimum_availability?: MinimumAvailability | null, 
/**
 * Replaces the complete tag set applied to movies this collection adds.
 */
tag_ids?: Array<number>, 
/**
 * One-shot bulk action: set monitoring on movies of this collection already in the library.
 */
monitor_movies?: boolean, };

export type CollectionQuery = { limit?: number, offset?: number, monitored?: boolean, q?: string, };

export type CollectionRef = { tmdb_id: number, title: string, };

export type CollectionRefreshCommand = { id: string, name: CollectionRefreshName, origin: CollectionRefreshOrigin, status: CommandStatus, attempts: number, created_at: number, started_at: number | null, completed_at: number | null, error_code: string | null, collections_checked: number, collections_failed: number, movies_added: number, 
/**
 * Added movies whose collection requested search-on-add. Counted, not submitted here.
 */
searches_pending: number, };

export type CollectionRefreshInput = Record<symbol, never>;

export type CollectionRefreshName = "refresh_collections";

export type CollectionRefreshOrigin = "manual" | "automatic";

export type CollectionRefreshQuery = { limit?: number, offset?: number, };

export type ColonReplacement = "delete" | "dash" | "space_dash" | "space_dash_space" | "smart" | "custom";

export type Command = { id: string, name: CommandName, target: RefreshTarget, provider_revision: number, priority: CommandPriority, status: CommandStatus, attempts: number, next_attempt_at: number, created_at: number, started_at: number | null, completed_at: number | null, error_code: string | null, items_observed: number, origin: CommandOrigin, manual_bypass: boolean, suppression_reason: string | null, suppression_until: number | null, };

export type CommandInput = { name: CommandName, target: RefreshTarget, provider_revision: number, priority: CommandPriority, };

export type CommandName = "refresh_downloads";

export type CommandOrigin = "legacy_unknown" | "automatic" | "manual";

export type CommandPriority = "normal" | "high";

export type CommandQuery = { media_type?: MediaDomain, status?: CommandStatus, limit?: number, offset?: number, };

export type CommandStatus = "queued" | "running" | "retry_wait" | "succeeded" | "failed" | "cancelled";

export type CompletedDownloadHandling = { media_type: MediaDomain, enabled: boolean, defined: boolean, revision: number, locally_edited: boolean, reconciliation_pending: boolean, reconciliation_reason: CompletedDownloadHandlingReconciliationReason | null, effective_scopes: number, observation_disabled_scopes: number, };

export type CompletedDownloadHandlingReconciliationReason = "pending" | "schedule_limit";

export type CompletedDownloadHandlingUpdate = { enabled: boolean, revision: number, };

export type CreditDetails = { credit_tmdb_id: string, person_tmdb_id: number, person_name: string, department: string | null, job: string | null, character: string | null, order: number, kind: CreditKind, images: Array<CreditImage>, };

export type CreditImage = { cover_type: string, url: string, };

export type CreditKind = "cast" | "crew";

export type CustomFormat = { id: number, media_type: MediaDomain, name: string, include_when_renaming: boolean, specifications: Array<CustomFormatSpecification>, };

export type CustomFormatBulkInput = { ids: Array<number>, include_when_renaming?: boolean | null, };

export type CustomFormatChoice = { value: number, label: string, };

export type CustomFormatCondition = { "kind": "release_title", pattern: string, } | { "kind": "release_group", pattern: string, } | { "kind": "edition", pattern: string, } | { "kind": "language", value: number, except_language: boolean, } | { "kind": "size", min_gib: number, max_gib: number, } | { "kind": "source", value: number, } | { "kind": "resolution", value: number, } | { "kind": "quality_modifier", value: number, } | { "kind": "indexer_flag", value: number, } | { "kind": "release_type", value: number, } | { "kind": "year", min: number, max: number, };

export type CustomFormatInput = { name: string, include_when_renaming: boolean, specifications: Array<CustomFormatSpecification>, };

export type CustomFormatPreset = { label: string, specification: CustomFormatSpecification, };

export type CustomFormatSchema = { version: number, media_type: MediaDomain, conditions: Array<CustomFormatCondition>, presets: { [key in string]: Array<CustomFormatPreset> }, choices: { [key in string]: Array<CustomFormatChoice> }, max_formats: number, max_specifications: number, regex: string, };

export type CustomFormatSpecification = { name: string, negate: boolean, required: boolean, condition: CustomFormatCondition, };

export type DelayProfile = { id: number, media_type: MediaDomain, is_global: boolean, position: number, tag_ids: Array<number>, settings: DelaySettings, };

export type DelayProfileCatalog = { revision: number, configured: boolean, availability_delay_days: number | null, profiles: Array<DelayProfile>, };

export type DelayProfileInput = { settings: DelayProfileSettings, tag_ids: Array<number>, };

export type DelayProfileReorder = { revision: number, ids: Array<number>, };

export type DelayProfileSettings = { torrent_delay_minutes: number, usenet_delay_minutes: number, enable_torrent: boolean, enable_usenet: boolean, preferred_protocol: DelayProtocol, bypass_if_highest_quality: boolean, bypass_if_above_custom_format_score: boolean, minimum_custom_format_score: number, };

export type DelayProfileWrite = { revision: number, profile: DelayProfileInput, };

export type DelayProtocol = "usenet" | "torrent";

export type DelaySettings = { "semantics": "legacy_age_only", torrent_delay_minutes: number | null, usenet_delay_minutes: number | null, } | { "semantics": "profile" } & DelayProfileSettings;

export type DeletionEventType = "file_deleted";

export type DeletionReason = "manual" | "upgrade";

export type Direction = "remote_to_local" | "local_to_remote";

export type Disposition = "accept" | "reject" | "delay";

export type DownloadContentLayout = "default" | "original" | "subfolder";

export type DownloadDiagnostic = "error" | "missing_files" | "stalled" | "metadata" | "dht_disabled" | "unknown_state" | "verifying" | "repairing" | "extracting" | "moving" | "post_processing" | "disk_full";

export type DownloadFile = { index: number, name: string, size_bytes: number, progress: number, priority: number, };

export type DownloadFiles = { domain: MediaDomain, hash: string, files: Array<DownloadFile>, };

export type DownloadFilesQuery = { domain: MediaDomain, hash: string, };

export type DownloadInitialState = "started" | "stopped" | "forced";

export type DownloadItem = { hash: string, domain: MediaDomain, name: string, category: string, status: DownloadStatus, diagnostic: DownloadDiagnostic | null, progress: number, size_bytes: number, remaining_bytes: number | null, download_bytes_per_second: number, upload_bytes_per_second: number, eta_seconds: number | null, ratio: number, seeding_seconds: number | null, completed: boolean, };

export type DownloadPage = { domain: MediaDomain, offset: number, limit: number, next_offset: number | null, items: Array<DownloadItem>, };

export type DownloadProcessing = { receipt_id: string, target: ReleaseTarget, provider_id: string, provider_revision: number, remote_id: string, policy_revision: number, status: ProcessingStatus, preflight_attempts: number, total_preflight_attempts: number, next_attempt_at: number, operation_id: string | null, import_phase: string | null, resume_requested: boolean, retirement_state: string | null, recycle_state: string | null, recovery_bytes_retained: boolean, error_code: string | null, path_failure_code: string | null, recovery_required: boolean, reasons: Array<string>, created_at: number, updated_at: number, scan_command_id: string | null, scan_reserved: boolean, };

export type DownloadQuery = { domain: MediaDomain, offset: number, limit: number, imported: boolean, };

export type DownloadScope = { category: string, imported_category: string | null, recent_priority: number, older_priority: number, initial_state?: DownloadInitialState, content_layout?: DownloadContentLayout, sequential_order?: boolean, first_last_first?: boolean, add_tags?: boolean, tags?: Array<number>, };

export type DownloadStatus = "queued" | "downloading" | "paused" | "completed" | "failed" | "warning" | "stalled" | "unknown";

export type DownloadedScanChildQuery = { disposition?: ScanDisposition, limit?: number, offset?: number, };

export type DownloadedScanCommand = { id: string, media_type: MediaDomain, path: string, mode: ScanMode, priority: CommandPriority, tracked: ScanTracked | null, tracking: ScanTracking, status: CommandStatus, phase: ScanPhase, outcome: ScanOutcome | null, error_code: string | null, attempts: number, run_generation: number, cancel_requested: boolean, inventory_revision: number, source: ScanSource | null, next_attempt_at: number, created_at: number, started_at: number | null, completed_at: number | null, counts: ScanCounts, receipt_id: string | null, predecessor_operation_id: string | null, coverage: ScanCoverage, cleanup: ScanCleanup, supersedes_command_id: string | null, successor_command_id: string | null, successor_allowed: boolean, };

export type DownloadedScanInput = { id: string, path: string, mode: ScanMode, priority: CommandPriority, tracked?: ScanTracked, supersedes_command_id?: string, };

export type DownloadedScanQuery = { status?: CommandStatus, limit?: number, offset?: number, };

export type Episode = { id: number, series_id: number, season: number, number: number, title: string, monitored: boolean, episode_file_id: number | null, has_file: boolean, file_path: string | null, tvdb_id: number | null, air_date: string | null, air_date_utc: string | null, last_search_time: string | null, runtime: number | null, finale_type: string | null, overview: string | null, absolute_episode_number: number | null, scene_absolute_episode_number: number | null, scene_episode_number: number | null, scene_season_number: number | null, unverified_scene_numbering: boolean | null, series?: EpisodeSeriesProjection, episode_file?: EpisodeFileProjection, images?: Array<EpisodeCover> | null, };

export type EpisodeCover = { coverType: EpisodeCoverType, url?: string | null, remoteUrl?: string | null, };

export type EpisodeCoverType = "unknown" | "poster" | "banner" | "fanart" | "screenshot" | "headshot" | "clearlogo";

export type EpisodeDetails = { tvdb_id: number, season: number, number: number, title: string, air_date: string | null, air_date_utc: string | null, absolute_episode_number: number | null, runtime: number | null, overview: string | null, finale_type: string | null, };

export type EpisodeFileProjection = { id: number, series_id: number, path: string, };

export type EpisodeIncludes = { include_series?: boolean, include_episode_file?: boolean, include_images?: boolean, };

export type EpisodeMonitor = { monitored: boolean, };

export type EpisodeMonitorMany = { episode_ids: Array<number>, monitored: boolean, };

export type EpisodeQuery = { series_id?: number, season?: number, episode_ids?: string, episode_file_id?: number, offset?: number, limit?: number, include_series?: boolean, include_episode_file?: boolean, include_images?: boolean, };

export type EpisodeSeriesProjection = { runtime: number | null, id: number, tvdb_id: number | null, title: string, year: number | null, path: string, poster: string | null, monitored: boolean, };

export type EvaluatedRelease = { metadata: ReleaseMetadata, decision: ReleaseDecision, };

export type FeedDomainTest = { media_type: MediaDomain, parsed: number, rejected: number, below_minimum_seeders: number, };

export type FeedScope = { enable_rss?: boolean, minimum_seeders?: number | null, };

export type FeedTest = { domains: Array<FeedDomainTest>, };

export type FileBulk = { files: Array<FileUpdate>, };

export type FileBulkDeleteInput = { file_ids: Array<number>, mode: FileDeletionMode, };

export type FileDeleteInput = { mode: FileDeletionMode, };

export type FileDeletion = { id: string, media_type: MediaDomain, file_id: number, path: string, effect: FileDeletionEffect, recycle_path: string | null, targets: Array<MediaTarget>, };

export type FileDeletionBatch = { batch_id: string, deletions: Array<FileDeletion>, };

export type FileDeletionEffect = "recycled" | "deleted" | "already_missing";

export type FileDeletionMode = "recycle" | "permanent";

export type FileEditor = { file_ids: Array<number>, quality?: FileQualityInput | null, languages?: Array<number> | null, release_group?: string | null, edition?: string | null, indexer_flags?: number | null, release_type?: number | null, };

export type FilePatch = { quality?: FileQualityInput | null, languages?: Array<number> | null, release_group?: string | null, edition?: string | null, indexer_flags?: number | null, release_type?: number | null, };

export type FileQuality = { quality_id: number, revision: FileRevision | null, };

export type FileQualityInput = { quality_id: number, revision?: FileRevision | null, };

export type FileQuery = { series_id?: number, movie_ids?: string, file_ids?: string, offset?: number, limit?: number, };

export type FileResource = TvFileResource | MovieFileResource;

export type FileRevision = { version: number, real: number, is_repack: boolean, };

export type FileUpdate = { id: number, quality?: FileQualityInput | null, languages?: Array<number> | null, release_group?: string | null, edition?: string | null, indexer_flags?: number | null, release_type?: number | null, };

export type FilesystemContents = { path: string, parent: string | null, directories: Array<FilesystemEntry>, files: Array<FilesystemEntry>, };

export type FilesystemEntry = { type: FilesystemKind, name: string, path: string, extension: string | null, size: number | null, last_modified: string | null, };

export type FilesystemKind = "file" | "folder";

export type FilesystemLookup = { path?: string, include_files?: boolean, allow_folders_without_trailing_slashes?: boolean, };

export type FilesystemMediaFile = { path: string, relative_path: string, name: string, };

export type FilesystemMediaFiles = { path: string, media_type: MediaDomain, files: Array<FilesystemMediaFile>, };

export type FilesystemMediaQuery = { path: string, media_type: MediaDomain, };

export type FilesystemPath = { path: string, };

export type FilesystemType = { type: FilesystemKind, };

export type FolderRenamePreview = { media_type: MediaDomain, naming_revision: number, folder_format: string | null, ids: Array<number>, unchanged_count: number, items: Array<FolderRenamePreviewItem>, };

export type FolderRenamePreviewItem = { series_id: number | null, movie_id: number | null, existing_path: string, new_path: string | null, status: RenameStatus, reasons: Array<RenameReason>, };

export type HealthAdmission = { "outcome": "queued", command_id: string, requested_scope: HealthScope, selection: Array<HealthSelectionToken>, pending: boolean, } | { "outcome": "coalesced_queued", command_id: string, requested_scope: HealthScope, selection: Array<HealthSelectionToken>, pending: boolean, } | { "outcome": "pending", active_command_id: string, requested_scope: HealthScope, selection: Array<HealthSelectionToken>, pending: boolean, };

export type HealthCheckState = { identity: HealthIdentity, startup: boolean, scheduled: boolean, generation: number, observed_generation: number | null, checked_at: number | null, observed_epoch: string | null, evaluation: HealthEvaluation, last_error: string | null, pending_reasons: number, due_at: number | null, };

export type HealthCommand = { id: string, scope: HealthScope, priority: CommandPriority, status: CommandStatus, attempts: number, next_attempt_at: number, created_at: number, started_at: number | null, completed_at: number | null, error_code: string | null, epoch: string, is_grace: boolean, members: Array<HealthMember>, };

export type HealthCommandInput = { scope: HealthScope, priority: CommandPriority, };

export type HealthCoverage = { registered_only: boolean, identities: Array<HealthIdentity>, };

export type HealthEvaluation = "never_run" | "current" | "pending" | "running" | "stale" | "failed";

export type HealthIdentity = { scope: HealthScope, check_key: string, };

export type HealthIssue = { identity: HealthIdentity, severity: HealthSeverity, reason: string, message: string, wiki_url: string, compatibility_type: string, };

export type HealthLifecycle = { epoch: string, started_at: number, grace_due_at: number, grace_phase: string, next_scheduled_at: number, schedule_error: string | null, last_batch_completed_at: number | null, };

export type HealthMember = { identity: HealthIdentity, admitted_generation: number, captured_generation: number | null, captured_reasons: number | null, captured_due_at: number | null, };

export type HealthScope = "tv" | "movies" | "system" | "all";

export type HealthSelectionToken = { identity: HealthIdentity, generation: number, };

export type HealthSeverity = "ok" | "notice" | "warning" | "error";

export type HealthSnapshot = { checks: Array<HealthCheckState>, issues: Array<HealthIssue>, total: number, limit: number, offset: number, lifecycle: HealthLifecycle, active_command: HealthCommand | null, pending_keys: Array<HealthSelectionToken>, summary: HealthSummary, coverage: HealthCoverage, };

export type HealthSummary = { total: number, current: number, never_run: number, stale: number, failed: number, non_ok: number, };

export type HealthTransition = { sequence: number, event_id: string, epoch: string, command_id: string, kind: string, in_grace: boolean, created_at: number, issue: HealthIssue, };

export type HealthTransitions = { oldest_retained_sequence: number | null, latest_sequence: number | null, next_cursor: number, gap: boolean, items: Array<HealthTransition>, };

export type HistoricalFile = { media_type: MediaDomain, id: number, };

export type HistoryEvent = { "origin": "native_import" } & NativeHistoryEvent | { "origin": "source_snapshot" } & SourceHistoryEvent | { "origin": "native_deletion" } & NativeDeletionEvent;

export type HistoryEventType = "file_imported";

export type HistoryQuery = { media_type?: MediaDomain, episode_id?: number, movie_id?: number, series_id?: number, season?: number, from?: string, to?: string, limit?: number, offset?: number, };

export type HostApplyMode = "process_restart";

export type HostAuthentication = "none";

export type HostCapabilities = { persisted_edits: boolean, tls: boolean, url_base: boolean, trusted_forwarding: boolean, };

export type HostMutability = "deployment";

export type HostSettingSource = "default" | "environment";

export type HostSettings = { authentication: HostAuthentication, configured_bind: string, bound_address: string, bind_source: HostSettingSource, allowed_hosts: Array<string>, allowed_hosts_source: HostSettingSource, filtering_enabled: boolean, trusted_networks: Array<string>, trusted_networks_source: HostSettingSource, forwarding_enabled: boolean, mutability: HostMutability, apply_mode: HostApplyMode, capabilities: HostCapabilities, };

export type ImportInput = ManualImportRequest | ImportRequest;

export type ImportPolicy = { media_type: MediaDomain, revision: number, skip_free_space_check_when_importing: boolean, minimum_free_space_mib: number, download_client_working_folders: string, };

export type ImportPolicyUpdate = { revision: number, skip_free_space_check_when_importing: boolean, minimum_free_space_mib: number, download_client_working_folders: string, };

export type ImportRequest = { episode_id: number, source: string, mode: string, destination: string, };

export type IndexerCategory = { id: number, parent_id: number | null, };

export type IndexerConnection = { implementation: IndexerImplementation, endpoint: string, credentials?: ProviderCredentials | null, };

export type IndexerContinuation = { query_index: number, offset: number, };

export type IndexerImplementation = "torznab" | "newznab";

export type IndexerItemWarning = { index: number, code: IndexerItemWarningCode, };

export type IndexerItemWarningCode = "invalid_item";

export type IndexerPage = { warnings: Array<IndexerItemWarning>, query_index: number, query_count: number, next_query: IndexerContinuation | null, media_type: MediaDomain, items: Array<ReleaseMetadata>, offset: number, limit: number, total: number | null, next_offset: number | null, };

export type IndexerParameter = { name: string, value: string, };

export type IndexerSearch = { "kind": "rss", media_type: MediaDomain, offset?: number, query_index?: number, limit?: number, } | { "kind": "tv", title: string, aliases?: Array<string>, tvdb_id?: number | null, tvmaze_id?: number | null, rage_id?: number | null, imdb_id?: string | null, tmdb_id?: number | null, numbering: TvNumbering, search_mode?: TvSearchMode, offset?: number, query_index?: number, limit?: number, } | { "kind": "movie", title: string, aliases?: Array<string>, year?: number | null, imdb_id?: string | null, tmdb_id?: number | null, offset?: number, query_index?: number, limit?: number, };

export type IndexerTest = { capabilities: Capabilities, domains: Array<MediaDomain>, };

export type LabelClientTest = { client_version: string, domains: Array<MediaDomain>, warnings: Array<LabelTestWarning>, };

export type LabelTestWarning = "label_not_created";

export type LegacyEpisode = { id: number, series_id: number, season: number, number: number, title: string, file_path: string | null, };

export type LegacyError = { error: string, };

export type LegacySeries = { id: number, title: string, year: number | null, path: string, poster: string | null, };

export type LibraryBulk = { items: Array<LibraryUpdate>, };

export type LibraryCreate = { title?: string | null, year?: number | null, tvdb_id?: number | null, tmdb_id?: number | null, imdb_id?: string | null, metadata_id?: number | null, path: string, settings?: LibraryPatch, };

export type LibraryEditor = { ids: Array<number>, patch: LibraryPatch, };

export type LibraryItem = { runtime: number | null, tag_ids: Array<number>, id: number, media_type: MediaDomain, metadata_id: number | null, title: string, year: number | null, tvdb_id: number | null, tmdb_id: number | null, imdb_id: string | null, path: string, poster: string | null, monitored: boolean, settings: LibrarySettings, statistics: LibraryStatistics, seasons?: Array<LibrarySeason>, is_available: boolean | null, };

export type LibraryPage = { items: Array<LibraryItem>, total: number, limit: number, offset: number, };

export type LibraryPatch = { tags?: TagAssignment, monitored?: boolean, quality_profile_id?: number | null, series_type?: SeriesType | null, season_folder?: boolean | null, use_scene_numbering?: boolean | null, monitor_new_items?: MonitorNewItems | null, minimum_availability?: MinimumAvailability | null, seasons?: Array<LibrarySeasonInput> | null, };

export type LibraryQuery = { limit?: number, offset?: number, ids?: string, tvdb_id?: number, tmdb_id?: number, };

export type LibrarySeason = { number: number, monitored: boolean, statistics: LibraryStatistics, };

export type LibrarySeasonInput = { number: number, monitored: boolean, };

export type LibrarySettings = { quality_profile_id: number | null, profile_name: string | null, series_type: string | null, season_folder: boolean | null, use_scene_numbering: boolean | null, monitor_new_items: string | null, minimum_availability: string | null, added: string | null, };

export type LibraryStatistics = { total_episode_count: number | null, episode_count: number | null, episode_file_count: number | null, file_count: number, known_size_file_count: number, size_on_disk: number | null, season_count: number | null, };

export type LibraryUpdate = { id: number, patch: LibraryPatch, };

export type LookupResult = { media_type: MediaDomain, external_id: number, title: string, year: number | null, imdb_id: string | null, };

export type ManualImportBatchInput = { operation_ids: Array<string>, priority: CommandPriority, };

export type ManualImportCommand = { id: string, batch_id: string, operation_id: string, priority: CommandPriority, status: CommandStatus, attempts: number, next_attempt_at: number, created_at: number, started_at: number | null, completed_at: number | null, error_code: string | null, };

export type ManualImportCommandQuery = { batch_id?: string, status?: CommandStatus, limit?: number, offset?: number, };

export type ManualImportReconcileInput = { operation_ids: Array<string>, };

export type ManualImportReconciliation = { operation_id: string, operation: Operation | null, active_command: ManualImportCommand | null, };

export type ManualImportRequest = { target: MediaTarget, source: string, mode: Mode, destination: string, };

export type Mapping = { id: number, media_type: MediaDomain, host: string, remote_path: string, local_path: string, revision: number, };

export type MappingInput = { host: string, remote_path: string, local_path: string, };

export type MappingQuery = { limit?: number, offset?: number, };

export type MappingRevision = { revision: number, };

export type MappingUpdate = { revision: number, host: string, remote_path: string, local_path: string, };

export type MatchedEpisode = { id: number, season: number, number: number, };

export type MatchedMovie = { id: number, title: string, year: number | null, };

export type MatchedSeries = { id: number, title: string, };

export type MediaDomain = "tv" | "movies";

export type MediaInfo = { schema_revision: number | null, container_format: string | null, audio_bitrate: number | null, audio_channels: number | null, audio_codec: string | null, audio_languages: string | null, audio_stream_count: number | null, video_bit_depth: number | null, video_bitrate: number | null, video_codec: string | null, video_fps: number | null, video_dynamic_range: string | null, video_dynamic_range_type: string | null, resolution: string | null, run_time: string | null, scan_type: string | null, subtitles: string | null, video_format: string | null, video_codec_id: string | null, video_profile: string | null, audio_format: string | null, audio_codec_id: string | null, audio_profile: string | null, audio_channel_count: number | null, audio_channel_positions: string | null, width: number | null, height: number | null, runtime_ticks: number | null, audio_streams: Array<MediaInfoAudioStream> | null, subtitle_streams: Array<MediaInfoSubtitleStream> | null, };

export type MediaInfoAudioStream = { language: string | null, format: string | null, codec_id: string | null, profile: string | null, bitrate: number | null, channels: number | null, channel_positions: string | null, };

export type MediaInfoSubtitleStream = { language: string | null, format: string | null, forced: boolean | null, hearing_impaired: boolean | null, };

export type MediaTarget = { "media_type": "episode", "id": number } | { "media_type": "movie", "id": number };

export type MetadataCommand = { id: string, name: MetadataCommandName, target: MetadataRefreshTarget, external_id: number, priority: CommandPriority, status: CommandStatus, attempts: number, next_attempt_at: number, created_at: number, started_at: number | null, completed_at: number | null, error_code: string | null, records_updated: number, };

export type MetadataCommandInput = { target: MetadataRefreshTarget, priority: CommandPriority, };

export type MetadataCommandName = "refresh_series" | "refresh_movie";

export type MetadataCommandQuery = { media_type?: MediaDomain, series_id?: number, movie_id?: number, status?: CommandStatus, limit?: number, offset?: number, };

export type MetadataLookupQuery = { term: string, };

export type MetadataRefreshTarget = { "media_type": "tv", series_id: number, } | { "media_type": "movies", movie_id: number, };

export type MinimumAvailability = "tba" | "announced" | "in_cinemas" | "released";

export type Mode = "copy" | "move" | "hardlink";

export type MonitorNewItems = "all" | "none";

export type MovieCalendarEntry = { event_id: string, target: MediaTarget, metadata_id: number, title: string, year: number | null, tmdb_id: number | null, imdb_id: string | null, release_type: MovieReleaseType, date: string, released_at: string, monitored: boolean, has_file: boolean, status: string | null, studio: string | null, runtime_minutes: number | null, };

export type MovieCollection = { id: number, tmdb_id: number | null, title: string, overview: string | null, monitored: boolean, search_on_add: boolean, quality_profile_id: number | null, root_folder_id: number | null, root_folder_path: string | null, minimum_availability: string | null, tag_ids: Array<number>, last_info_sync: string | null, movie_count: number, library_movie_count: number, missing_movie_count: number, 
/**
 * Members are listed by the detail endpoint only; list responses carry an empty array.
 */
movies: Array<CollectionMovie>, };

export type MovieCredit = { id: number, metadata_id: number, credit_tmdb_id: string, person_tmdb_id: number, person_name: string, department: string | null, job: string | null, character: string | null, order: number, type: CreditKind, images: Array<CreditImage>, };

export type MovieCreditQuery = { limit?: number, offset?: number, movie_id?: number, metadata_id?: number, };

export type MovieDetails = { studio: string | null, genres: Array<string> | null, keywords: Array<string> | null, tmdb_id: number, title: string, year: number | null, imdb_id: string | null, runtime: number | null, status: string | null, in_cinemas: string | null, digital_release: string | null, physical_release: string | null, secondary_year: number | null, original_language: number | null, alternative_titles: Array<string> | null, credits: Array<CreditDetails> | null, collection: CollectionAssociation, };

export type MovieFileResource = { movie_id: number, edition: string | null, original_file_path: string | null, id: number, path: string, relative_path: string | null, quality: FileQuality | null, languages: Array<number> | null, size: number | null, date_added: string | null, release_group: string | null, indexer_flags: number | null, scene_name: null, media_info: MediaInfo | null, custom_formats: null, custom_format_score: null, quality_cutoff_not_met: null, };

export type MovieIndexerScope = { enable_rss?: boolean, enable_automatic_search?: boolean, enable_interactive_search?: boolean, download_client_id?: string | null, categories: Array<number>, remove_year?: boolean, };

export type MovieLookupAdd = { tmdb_id: number, path: string, settings?: LibraryPatch, };

export type MovieNamingConfig = { revision: number, rename_enabled: boolean, replace_illegal_characters: boolean, colon_replacement: ColonReplacement, custom_colon_replacement: string | null, standard_movie_format: string | null, movie_folder_format: string | null, };

export type MovieNamingExamples = { standard_movie_format: string | null, movie_folder_format: string | null, };

export type MovieNamingExamplesQuery = { rename_enabled?: boolean, replace_illegal_characters?: boolean, colon_replacement?: ColonReplacement, custom_colon_replacement?: string, standard_movie_format?: string, movie_folder_format?: string, };

export type MovieNamingUpdate = { revision: number, rename_enabled: boolean, replace_illegal_characters: boolean, colon_replacement: ColonReplacement, custom_colon_replacement: string | null, standard_movie_format: string | null, movie_folder_format: string | null, };

export type MovieNotificationScope = { triggers: MovieNotificationTriggers, 
/**
 * Movie tag IDs. Empty means every movie; otherwise a movie must carry one of them.
 */
tag_ids: Array<number>, };

export type MovieNotificationTriggers = { on_grab: boolean, on_import: boolean, on_upgrade: boolean, on_rename: boolean, on_movie_added: boolean, on_movie_delete: boolean, on_movie_file_delete: boolean, on_movie_file_delete_for_upgrade: boolean, on_health_issue: boolean, include_health_warnings: boolean, on_health_restored: boolean, on_application_update: boolean, on_manual_interaction_required: boolean, };

export type MovieParseResult = { title: string, parsed: ParsedRelease | null, movie: MatchedMovie | null, };

export type MovieReleaseProfileInput = { name: string | null, enabled: boolean, required: Array<string>, ignored: Array<string>, tag_ids: Array<number>, indexers: Array<ReleaseProfileIndexer>, };

export type MovieReleaseType = "in_cinemas" | "digital" | "physical";

export type MovieRenamePreview = { naming_revision: number, rename_enabled: boolean, standard_movie_format: string | null, movie_ids: Array<number>, files_considered: number, unchanged_count: number, unavailable_count: number, items: Array<MovieRenamePreviewItem>, };

export type MovieRenamePreviewItem = { movie_id: number, movie_file_id: number, existing_path: string | null, new_path: string | null, status: MovieRenamePreviewStatus, reasons: Array<MovieRenamePreviewReason>, };

export type MovieRenamePreviewReason = "invalid_path" | "missing_extension" | "invalid_file_facts" | "naming_render_failed";

export type MovieRenamePreviewStatus = "change" | "unavailable";

export type MovieWantedItem = { target: MediaTarget, metadata_id: number, title: string, year: number | null, tmdb_id: number | null, imdb_id: string | null, monitored: boolean, status: string | null, minimum_availability: MinimumAvailability | null, in_cinemas: string | null, digital_release: string | null, physical_release: string | null, availability: WantedMovieAvailability, profile_id: number | null, file: WantedFile | null, cutoff: WantedCutoff | null, };

export type MultiEpisodeExampleStatus = "rendered" | "preserved" | "needs_policy" | "invalid";

export type MultiEpisodeNamingExample = { field: string, contiguous: boolean, episodes: Array<number>, status: MultiEpisodeExampleStatus, filename: string | null, reason: string | null, };

export type NamingErrorDetail = { code: string, message: string, };

export type NamingErrorEnvelope = { error: NamingErrorDetail, };

export type NativeDeletionEvent = { id: string, deletion_id: string, event_type: DeletionEventType, reason: DeletionReason, target: MediaTarget, file: HistoricalFile, path: string, recycle_path: string | null, deleted_at: string, };

export type NativeHistoryEvent = { id: string, event_type: HistoryEventType, target: MediaTarget, file: HistoricalFile, source: string, destination: string, size_bytes: number, sha256: string, imported_at: string, };

export type Notification = { id: string, name: string, implementation: string, enabled: boolean, revision: number, settings: { [key in string]: NotificationSettingValue }, 
/**
 * Names of secrets that are configured; the values are never returned.
 */
secrets_configured: Array<string>, tv: TvNotificationScope | null, movies: MovieNotificationScope | null, created_at: number, updated_at: number, };

export type NotificationDelivery = { id: number, event_sequence: number, notification_id: string, event_kind: string, media_type: MediaDomain | null, state: string, attempts: number, next_attempt_at: number, last_error: string | null, created_at: number, completed_at: number | null, };

export type NotificationDeliveryList = { items: Array<NotificationDelivery>, };

export type NotificationDomainTestResult = { media_type: MediaDomain, status: NotificationTestStatus, 
/**
 * Stable machine class; never contains remote bodies, URLs or secrets.
 */
error: string | null, };

export type NotificationField = { name: string, label: string, kind: NotificationFieldKind, required: boolean, 
/**
 * Secret fields are submitted through `secrets`, stored encrypted and never returned.
 */
secret: boolean, help: string, default: NotificationSettingValue | null, options: Array<string>, min: number | null, max: number | null, max_length: number | null, };

export type NotificationFieldKind = "text" | "url" | "number" | "boolean" | "select" | "path";

export type NotificationImplementation = { implementation: string, label: string, description: string, kind: NotificationImplementationKind, domains: Array<MediaDomain>, fields: Array<NotificationField>, };

export type NotificationImplementationKind = "message" | "structured" | "script" | "library_update";

export type NotificationImplementationList = { items: Array<NotificationImplementation>, };

export type NotificationInput = { name: string, implementation: string, enabled: boolean, 
/**
 * Non-secret settings keyed by the implementation schema field name.
 */
settings: { [key in string]: NotificationSettingValue }, 
/**
 * Write-only secrets. On update an absent key keeps the stored value and `null` clears it.
 */
secrets: { [key in string]: string | null }, tv?: TvNotificationScope, movies?: MovieNotificationScope, 
/**
 * Required on update; must equal the stored revision.
 */
revision?: number, };

export type NotificationList = { items: Array<Notification>, };

export type NotificationSettingValue = boolean | number | string;

export type NotificationTestAllResult = { items: Array<NotificationTestResult>, };

export type NotificationTestInput = { 
/**
 * A saved notification whose stored secrets fill any secret not supplied in the draft.
 */
existing_id?: string, draft: NotificationInput, };

export type NotificationTestResult = { id: string | null, name: string, results: Array<NotificationDomainTestResult>, };

export type NotificationTestStatus = "delivered" | "failed" | "skipped";

export type Numbering = { "kind": "episodes", season: number, episodes: Array<number>, } | { "kind": "season", season: number, } | { "kind": "daily", date: string, } | { "kind": "absolute", episode: number, };

export type NzbgetPriority = "very_low" | "low" | "normal" | "high" | "very_high" | "force";

export type NzbgetScope = { category: string, recent_priority?: NzbgetPriority, older_priority?: NzbgetPriority, 
/**
 * The job is added paused and must be resumed in NZBGet.
 */
add_paused?: boolean, tags?: Array<number>, };

export type NzbgetTest = { nzbget_version: string, domains: Array<MediaDomain>, warnings: Array<NzbgetTestWarning>, };

export type NzbgetTestWarning = "testing_version" | "short_history_retention";

export type Operation = { id: string, target: MediaTarget, status: string, message: string, error_code: string | null, path_failure_code: string | null, recovery_required: boolean, };

export type ParseQuery = { title: string, };

export type ParsedRelease = { title: string, year: number | null, numbering: Numbering | null, quality_name: string | null, edition: string | null, revision: FileRevision | null, revision_marker: boolean, technical_start: number | null, };

export type PresetScope = { "media_type": "tv", settings: TvIndexerScope, } | { "media_type": "movies", settings: MovieIndexerScope, };

export type ProcessingInput = { provider_id: string, provider_revision: number, media_type: MediaDomain, receipt_ids: Array<string>, };

export type ProcessingMode = "copy" | "hardlink";

export type ProcessingPolicy = { observation_suppressed: boolean, observation_enabled: boolean, enabled_override: boolean | null, desired_enabled: boolean, provider_id: string, media_type: MediaDomain, provider_revision: number, revision: number | null, enabled: boolean, mode: ProcessingMode, };

export type ProcessingPolicyInput = { provider_revision: number, revision: number | null, enabled: boolean, mode: ProcessingMode, };

export type ProcessingPolicyReset = { provider_revision: number, revision: number | null, mode: ProcessingMode, };

export type ProcessingQuery = { provider_id?: string, media_type?: MediaDomain, status?: ProcessingStatus, receipt_id?: string, limit?: number, offset?: number, };

export type ProcessingStatus = "queued" | "checking" | "importing" | "imported" | "blocked" | "cancelled";

export type Provider = { id: string, revision: number, name: string, enabled: boolean, priority: number, settings: ProviderSettings, has_credentials: boolean, test_supported: boolean, test_status: TestStatus, last_test: ProviderTestObservation | null, };

export type ProviderBatchItem = { provider_id: string, revision: number, outcome: ProviderBatchOutcome, };

export type ProviderBatchOutcome = { "status": "success", tested_at: number, } | { "status": "failure", code: string, message: string, retry_after_seconds: number | null, } | { "status": "changed" } | { "status": "timeout" };

export type ProviderBatchResult = { items: Array<ProviderBatchItem>, };

export type ProviderBulkResult = { items: Array<Provider>, };

export type ProviderBulkUpdate = { changes: ProviderChanges, media_type: MediaDomain, kind: ProviderKind, items: Array<ProviderSelectionItem>, };

export type ProviderChanges = { enabled?: boolean | null, priority?: number | null, };

export type ProviderCredentials = { "kind": "api_key", api_key: string, } | { "kind": "username_password", username: string, password: string, } | { "kind": "indexer", api_key?: string | null, tv_parameters?: Array<IndexerParameter>, movie_parameters?: Array<IndexerParameter>, } | { "kind": "feed", cookie?: string | null, tv_parameters?: Array<IndexerParameter>, movie_parameters?: Array<IndexerParameter>, };

export type ProviderDefaults = { "kind": "indexer", tv: TvIndexerScope, movies: MovieIndexerScope, } | { "kind": "feed", tv: FeedScope, movies: FeedScope, } | { "kind": "download_client", imported_category: string | null, recent_priority: number, older_priority: number, initial_state: DownloadInitialState, content_layout: DownloadContentLayout, sequential_order: boolean, first_last_first: boolean, add_tags: boolean, tags: Array<number>, } | { "kind": "usenet_client", recent_priority: UsenetPriority, older_priority: UsenetPriority, tags: Array<number>, } | { "kind": "nzbget_client", recent_priority: NzbgetPriority, older_priority: NzbgetPriority, add_paused: boolean, tags: Array<number>, endpoint_hint: string, } | { "kind": "transmission_client", recent_priority: TorrentQueuePriority, older_priority: TorrentQueuePriority, add_paused: boolean, tags: Array<number>, endpoint_hint: string, } | { "kind": "label_torrent_client", recent_priority: TorrentQueuePriority, older_priority: TorrentQueuePriority, add_paused: boolean, tags: Array<number>, endpoint_hint: string, };

export type ProviderDownloadResult = { provider_id: string, revision: number, page: DownloadPage, };

export type ProviderDraftInput = { source: ProviderDraftSource | null, config: ProviderInput, };

export type ProviderDraftResult = { source: ProviderDraftSource | null, tested_at: number, result: ProviderTestOutcome, };

export type ProviderDraftSource = { id: string, revision: number, };

export type ProviderFilesResult = { provider_id: string, revision: number, result: DownloadFiles, };

export type ProviderFilter = { media_type: MediaDomain, kind: ProviderKind, };

export type ProviderImplementation = "newznab" | "nzbget" | "qbittorrent" | "sabnzbd" | "torrentrss" | "torznab" | "transmission" | "deluge";

export type ProviderInput = { name: string, enabled: boolean, priority: number, settings: ProviderSettings, credentials?: ProviderCredentials | null, };

export type ProviderKind = "indexer" | "download_client";

export type ProviderPathInput = { media_type: MediaDomain, remote_path: string, };

export type ProviderPathPreview = { provider_id: string, provider_revision: number, resolution: Resolution, };

export type ProviderPreset = { key: string, name: string, implementation: ProviderImplementation, enabled: boolean, endpoint: string | null, endpoint_hint: string | null, defaults: PresetScope, };

export type ProviderQuery = { limit?: number, offset?: number, media_type?: MediaDomain, };

export type ProviderRevision = { revision: number, };

export type ProviderSchema = { templates: Array<ProviderTemplate>, };

export type ProviderSearchResult = { provider_id: string, revision: number, page: IndexerPage, };

export type ProviderSelection = { media_type: MediaDomain, kind: ProviderKind, items: Array<ProviderSelectionItem>, };

export type ProviderSelectionItem = { id: string, revision: number, };

export type ProviderSettings = { "implementation": "torznab", endpoint: string, tv: TvIndexerScope | null, movies: MovieIndexerScope | null, } | { "implementation": "newznab", endpoint: string, tv: TvIndexerScope | null, movies: MovieIndexerScope | null, } | { "implementation": "qbittorrent", endpoint: string, tv: DownloadScope | null, movies: DownloadScope | null, } | { "implementation": "sabnzbd", endpoint: string, tv: UsenetScope | null, movies: UsenetScope | null, } | { "implementation": "nzbget", endpoint: string, tv: NzbgetScope | null, movies: NzbgetScope | null, } | { "implementation": "transmission", endpoint: string, tv: TransmissionScope | null, movies: TransmissionScope | null, } | { "implementation": "torrentrss", endpoint: string, tv: FeedScope | null, movies: FeedScope | null, } | { "implementation": "deluge", endpoint: string, tv: TorrentLabelScope | null, movies: TorrentLabelScope | null, };

export type ProviderTemplate = { implementation: ProviderImplementation, supported_media: Array<MediaDomain>, enabled: boolean, priority: number, defaults: ProviderDefaults, presets: Array<ProviderPreset>, };

export type ProviderTestObservation = { revision: number, tested_at: number, status: TestStatus, error_code: string | null, };

export type ProviderTestOutcome = IndexerTest | FeedTest | ClientTest | SabnzbdTest | NzbgetTest | TransmissionTest | LabelClientTest;

export type ProviderTestResult = { provider_id: string, revision: number, tested_at: number, result: ProviderTestOutcome, };

export type ProviderUpdate = { revision: number, name: string, enabled: boolean, priority: number, settings: ProviderSettings, credentials?: ProviderCredentials | null, };

export type Quality = { id: number, name: string, source: string, resolution: number, modifier?: string, };

export type QualityDefinition = { id: number, media_type: MediaDomain, quality: Quality, title: string, weight: number, group_name: string | null, min_size: number | null, max_size: number | null, preferred_size: number | null, };

export type QualityDefinitionLimits = { min: number, max: number, unit: string, };

export type QualityDefinitionReset = { reset_titles?: boolean, };

export type QualityDefinitionUpdate = { id: number, title: string, min_size?: number | null, max_size?: number | null, preferred_size?: number | null, };

export type QualityProfile = { id: number, media_type: MediaDomain, name: string, items: Array<QualityProfileItem>, policy: QualityProfilePolicy | null, };

export type QualityProfileCutoff = { "kind": "quality", quality_id: number, } | { "kind": "group", position: number, };

export type QualityProfileFormatScore = { format_id: number, score: number, };

export type QualityProfileInput = { name: string, items: Array<QualityProfileItemInput>, policy?: QualityProfilePolicy | null, };

export type QualityProfileItem = { "kind": "quality" } & QualityProfileLeaf | { "kind": "group", name: string, allowed: boolean, items: Array<QualityProfileLeaf>, };

export type QualityProfileItemInput = { "kind": "quality" } & QualityProfileLeafInput | { "kind": "group", name: string, allowed: boolean, items: Array<QualityProfileLeafInput>, };

export type QualityProfileLeaf = { quality_id: number, allowed: boolean, min_size: number | null, max_size: number | null, preferred_size: number | null, };

export type QualityProfileLeafInput = { quality_id: number, allowed: boolean, min_size?: number | null, max_size?: number | null, preferred_size?: number | null, };

export type QualityProfilePage = { media_type: MediaDomain, items: Array<QualityProfileSummary>, total: number, offset: number, limit: number, };

export type QualityProfilePolicy = { upgrade_allowed: boolean, cutoff: QualityProfileCutoff, min_format_score: number, cutoff_format_score: number, min_upgrade_format_score: number, language_id?: number | null, format_items: Array<QualityProfileFormatScore>, };

export type QualityProfileQuery = { offset?: number, limit?: number, };

export type QualityProfileSummary = { id: number, name: string, item_count: number, group_count: number, };

export type QualityResetCommand = { id: string, media_type: MediaDomain, reset_titles: boolean, priority: CommandPriority, status: CommandStatus, attempts: number, next_attempt_at: number, created_at: number, started_at: number | null, completed_at: number | null, error_code: string | null, definitions_reset: number | null, };

export type QualityResetInput = { media_type: MediaDomain, reset_titles: boolean, priority: CommandPriority, };

export type QueueObservation = { association: MediaTarget | null, download: DownloadItem, };

export type QueueQuery = { provider_id: string, media_type: MediaDomain, limit?: number, offset?: number, };

export type QueueSnapshot = { target: RefreshTarget, provider_revision: number, observed_at: number, command_id: string | null, items: Array<QueueObservation>, total: number, limit: number, offset: number, };

export type RecycleBinCleanupCommand = { id: string, name: RecycleBinCleanupName, target: RecycleBinCleanupTarget, origin: RecycleBinCleanupOrigin, priority: CommandPriority, status: CommandStatus, attempts: number, next_attempt_at: number, created_at: number, started_at: number | null, completed_at: number | null, error_code: string | null, outcome: RecycleBinCleanupOutcome | null, bin_path: string | null, cleanup_days: number | null, files_removed: number, folders_removed: number, entries_skipped: number, entries_failed: number, bounded: boolean, };

export type RecycleBinCleanupInput = { target: RecycleBinCleanupTarget, priority: CommandPriority, };

export type RecycleBinCleanupName = "clean_up_recycle_bin";

export type RecycleBinCleanupOrigin = "manual" | "automatic";

export type RecycleBinCleanupOutcome = "cleaned" | "not_configured" | "disabled";

export type RecycleBinCleanupTarget = { media_type: MediaDomain, };

export type RecycleBinSettings = { media_type: MediaDomain, revision: number, path: string | null, cleanup_days: number, };

export type RecycleBinSettingsUpdate = { revision: number, path: string | null, cleanup_days: number, };

export type RefreshSchedule = { intent: RefreshScheduleIntent, requested_enabled: boolean | null, target: RefreshTarget, revision: number, provider_revision: number, enabled: boolean, interval_seconds: number, next_run_at: number, last_run_at: number | null, error_code: string | null, };

export type RefreshScheduleDelete = { target: RefreshTarget, revision: number, };

export type RefreshScheduleInput = { target: RefreshTarget, revision: number | null, provider_revision: number, enabled: boolean, interval_seconds: number, };

export type RefreshScheduleIntent = "inherited" | "explicit";

export type RefreshScheduleReset = { target: RefreshTarget, revision: number | null, provider_revision: number, };

export type RefreshTarget = { provider_id: string, media_type: MediaDomain, };

export type ReleaseDecision = { target: ReleaseTarget | null, disposition: Disposition, reasons: Array<string>, not_before: number | null, quality_id: number | null, parsed: ParsedRelease | null, custom_formats: Score | null, };

export type ReleaseMetadata = { title: string | null, size_bytes: number | null, published_at: string, categories: Array<number>, seeders: number | null, leechers: number | null, peers: number | null, languages: Array<string>, };

export type ReleasePolicy = { torrent_delay_minutes: number, usenet_delay_minutes: number, availability_delay_days: number, };

export type ReleaseProfile = { "media_type": "tv", id: number, name: string | null, enabled: boolean, required: Array<string>, ignored: Array<string>, tag_ids: Array<number>, indexers: Array<ReleaseProfileIndexer>, excluded_tag_ids: Array<number>, air_date_restriction: boolean, air_date_grace_period_days: number, allow_season_pack_without_all_episodes_aired: boolean, } | { "media_type": "movies", id: number, name: string | null, enabled: boolean, required: Array<string>, ignored: Array<string>, tag_ids: Array<number>, indexers: Array<ReleaseProfileIndexer>, };

export type ReleaseProfileCatalog = { media_type: MediaDomain, revision: number, profiles: Array<ReleaseProfile>, };

export type ReleaseProfileDetail = { revision: number, profile: ReleaseProfile, };

export type ReleaseProfileIndexer = { "kind": "provider", id: string, } | { "kind": "unresolved_source", application: ReleaseProfileSourceApplication, fingerprint: string, source_id: number, };

export type ReleaseProfileSourceApplication = "sonarr" | "radarr";

export type ReleaseProfileWrite<T> = { revision: number, profile: T, };

export type ReleaseSearchInput = { provider_id: string, provider_revision: number, target: MediaTarget, offset: number, query_index: number, limit: number, };

export type ReleaseSearchPage = { items: Array<EvaluatedRelease>, next_query: IndexerContinuation | null, };

export type ReleaseTarget = { "media_type": "tv", series_id: number, episode_ids: Array<number>, } | { "media_type": "movies", movie_id: number, };

export type RenameCommand = { id: string, name: RenameName, target: RenameTarget, expected_naming_revision: number | null, priority: CommandPriority, status: CommandStatus, attempts: number, next_attempt_at: number, created_at: number, started_at: number | null, completed_at: number | null, error_code: string | null, cancel_requested: boolean, items_renamed: number, items_unchanged: number, items_failed: number, 
/**
 * More changes existed than one run handles; run the command again to continue.
 */
bounded: boolean, };

export type RenameCommandDetail = { items: Array<RenameItem>, id: string, name: RenameName, target: RenameTarget, expected_naming_revision: number | null, priority: CommandPriority, status: CommandStatus, attempts: number, next_attempt_at: number, created_at: number, started_at: number | null, completed_at: number | null, error_code: string | null, cancel_requested: boolean, items_renamed: number, items_unchanged: number, items_failed: number, 
/**
 * More changes existed than one run handles; run the command again to continue.
 */
bounded: boolean, };

export type RenameEvent = { id: string, event_type: string, kind: RenameEventKind, owner: RenameOwner, file_id: number | null, command_id: string, old_path: string, new_path: string, sidecar_count: number, occurred_at: string, dispatched: boolean, };

export type RenameEventKind = "file" | "folder";

export type RenameEventQuery = { media_type?: MediaDomain, limit?: number, offset?: number, };

export type RenameInput = { name: RenameName, target: RenameTarget, 
/**
 * Fence against configuration changes after the preview the user looked at.
 */
expected_naming_revision?: number, priority: CommandPriority, };

export type RenameItem = { kind: RenameEventKind, series_id: number | null, movie_id: number | null, file_id: number | null, old_path: string, new_path: string, state: RenameItemState, error_code: string | null, sidecar_count: number, };

export type RenameItemState = "planned" | "committed" | "failed" | "rolled_back";

export type RenameName = "rename_files" | "rename_series" | "rename_movie" | "rename_series_folder" | "rename_movie_folder";

export type RenameOwner = { "media_type": "tv", "id": number } | { "media_type": "movies", "id": number };

export type RenameReason = "invalid_path" | "outside_owner_folder" | "missing_extension" | "invalid_file_facts" | "naming_render_failed" | "multi_episode_style_required" | "no_linked_episodes" | "invalid_episode_set" | "folder_format_unavailable" | "destination_in_use" | "duplicate_destination" | "destination_exists" | "source_missing" | "path_unsafe";

export type RenameStatus = "change" | "unavailable" | "blocked";

export type RenameTarget = { media_type: MediaDomain, series_ids: Array<number>, movie_ids: Array<number>, file_ids: Array<number>, };

export type RescanBatch = { commands: Array<RescanCommand>, busy_target_ids: Array<number>, };

export type RescanCommand = { id: string, media_type: MediaDomain, series_id: number | null, movie_id: number | null, priority: CommandPriority, status: RescanStatus, attempts: number, next_attempt_at: number, created_at: number, started_at: number | null, completed_at: number | null, error_code: string | null, skip_reason: string | null, files_adopted: number | null, files_removed: number | null, };

export type RescanInput = { target_id?: number, priority: CommandPriority, };

export type RescanQuery = { status?: RescanStatus, limit?: number, offset?: number, };

export type RescanStatus = "queued" | "running" | "retry_wait" | "succeeded" | "skipped" | "failed" | "cancelled";

export type Resolution = { input: string, output: string, mapping_id: number | null, mapping_revision: number | null, lexical_only: boolean, };

export type ResolveInput = { host: string, path: string, direction: Direction, };

export type RevisionPolicy = { media_type: MediaDomain, mode: RevisionPolicyMode, revision: number, };

export type RevisionPolicyMode = "prefer_and_upgrade" | "do_not_upgrade" | "do_not_prefer";

export type RevisionPolicyUpdate = { mode: RevisionPolicyMode, revision: number, };

export type RootFolder = { id: number, media_type: MediaDomain, path: string, observation: RootObservationStatus, accessible: boolean | null, writable: boolean | null, free_space: number | null, total_space: number | null, unmapped_folders: Array<UnmappedFolder> | null, };

export type RootInput = { path: string, };

export type RootObservationStatus = "available" | "inaccessible" | "timeout" | "busy" | "limited";

export type RootQuery = { limit?: number, offset?: number, };

export type RssCandidate = { client_attempts: Array<ClientAttempt>, origin: CandidateOrigin, id: string, command_id: string | null, source: RssTarget, title: string, target: ReleaseTarget | null, status: string, reasons: Array<string>, not_before: number | null, attempts: number, created_at: number, updated_at: number, error_code: string | null, };

export type RssCandidateQuery = { media_type?: MediaDomain, command_id?: string, status?: string, limit?: number, offset?: number, };

export type RssCommand = { id: string, target: RssTarget, priority: CommandPriority, status: CommandStatus, attempts: number, next_attempt_at: number, created_at: number, started_at: number | null, completed_at: number | null, error_code: string | null, fetched: number, evaluated: number, rejected: number, pending: number, observed: number, uncertain: number, fetch_complete: boolean, };

export type RssInput = { target: RssTargetInput, priority: CommandPriority, };

export type RssSchedule = { id: string, target: RssTarget, revision: number, interval_seconds: number, enabled: boolean, next_run_at: number, last_run_at: number | null, error_code: string | null, created_at: number, };

export type RssScheduleDelete = { revision: number, };

export type RssScheduleInput = { revision?: number, target: RssTargetInput, interval_seconds: number, enabled: boolean, };

export type RssTarget = { media_type: MediaDomain, indexer_id: string, indexer_revision: number, selection_kind: SelectionKind, client_id: string | null, client_revision: number | null, };

export type RssTargetInput = { media_type: MediaDomain, indexer_id: string, indexer_revision: number, client_selection?: ClientSelection, client_id?: string, client_revision?: number, };

export type SabnzbdTest = { sabnzbd_version: string, domains: Array<MediaDomain>, warnings: Array<SabnzbdTestWarning>, };

export type SabnzbdTestWarning = "develop_version" | "removes_completed_downloads";

export type ScanChild = { id: string, ordinal: number, relative_path: string, source_path: string, kind: ScanEntryKind, disposition: ScanDisposition, selected: boolean, reasons: Array<string>, series_id: number | null, movie_id: number | null, anchor_episode_id: number | null, member_count: number, expected_old_movie_file_id: number | null, destination: string | null, resolved_mode: ScanResolvedMode | null, quality_id: number | null, revision: FileRevision | null, edition: string | null, size: number | null, observation: ScanObservationState | null, error_code: string | null, operation_id: string | null, operation_phase: string | null, path_failure_code: string | null, resume_generation: number, resume_requested: boolean, recovery_required: boolean, };

export type ScanChildMember = { episode_id: number, series_id: number, season_number: number, episode_number: number, expected_old_episode_file_id: number | null, };

export type ScanChildResumeInput = { operation_id: string, inventory_revision: number, expected_resume_generation: number, };

export type ScanCleanup = { state: ScanCleanupState, phase: ScanCleanupPhase | null, reason: string | null, removed: number, error_code: string | null, resume_generation: number, resume_requested: boolean, recovery_required: boolean, };

export type ScanCleanupPhase = "observed" | "remove_intent" | "settled";

export type ScanCleanupResumeInput = { inventory_revision: number, expected_cleanup_resume_generation: number, };

export type ScanCleanupState = "not_needed" | "pending" | "complete" | "retained";

export type ScanCounts = { entries: number, ready: number, importing: number, imported: number, already_imported: number, rejected: number, failed: number, cancelled: number, recovery_required: number, };

export type ScanCoverage = { state: ScanCoverageState, expected: number, covered: number, missing: number, };

export type ScanCoverageState = "untracked" | "pending" | "complete" | "incomplete";

export type ScanDisposition = "ready" | "importing" | "imported" | "already_imported" | "rejected" | "failed" | "cancelled";

export type ScanEntryKind = "file" | "directory" | "symlink" | "other";

export type ScanMemberQuery = { limit?: number, offset?: number, };

export type ScanMode = "auto" | "move" | "copy";

export type ScanObservationState = "measured" | "unavailable";

export type ScanOutcome = "success" | "unsuccessful";

export type ScanPhase = "queued" | "inventory" | "waiting_predecessor" | "awaiting_import" | "importing" | "recovery_required" | "cleanup" | "settled";

export type ScanResolvedMode = "copy" | "move";

export type ScanSource = "found" | "missing";

export type ScanTracked = { provider_id: string, remote_id: string, };

export type ScanTracking = "untracked" | "pending" | "unknown_fallback" | "receipt";

export type Score = { format_ids: Array<number>, score: number, };

export type SearchCapability = { available: boolean, parameters: Array<string>, aggregate_ids: boolean, search_engine: SearchEngine, };

export type SearchCommand = { id: string, mode: SearchMode, target: MediaTarget, source: RssTarget, priority: CommandPriority, status: CommandStatus, attempts: number, next_attempt_at: number, created_at: number, started_at: number | null, completed_at: number | null, error_code: string | null, fetched: number, fetch_complete: boolean, selected_client_id: string | null, selected_client_attempts: Array<ClientAttempt>, selected_candidate_id: string | null, selected_candidate_status: string | null, selected_candidate_error_code: string | null, selected_candidate_reasons: Array<string>, };

export type SearchCommandInput = { request_id: string, mode: SearchMode, target: MediaTarget, indexer_id: string, indexer_revision: number, client_selection?: ClientSelection, client_id?: string, client_revision?: number, priority: CommandPriority, };

export type SearchCommandQuery = { target_type?: string, target_id?: number, status?: CommandStatus, limit?: number, offset?: number, };

export type SearchEngine = "raw" | "sphinx";

export type SearchMode = "automatic" | "interactive";

export type SearchResult = { id: string, command_id: string, metadata: ReleaseMetadata, decision: ReleaseDecision, expires_at: number, selected_candidate_id: string | null, };

export type SelectionKind = "legacy_fixed" | "default" | "explicit";

export type SeriesDetails = { runtime: number | null, network: string | null, original_country: string | null, status: string | null, genres: Array<string> | null, original_language: number | null, tvdb_id: number, title: string, year: number | null, imdb_id: string | null, seasons: Array<number>, episodes: Array<EpisodeDetails>, };

export type SeriesLookupAdd = { tvdb_id: number, path: string, settings?: LibraryPatch, };

export type SeriesType = "standard" | "daily" | "anime";

export type SnapshotApplication = "sonarr" | "radarr";

export type SnapshotOptions = { application: SnapshotApplication, dry_run?: boolean, import_providers?: boolean, allow_unmapped_providers?: boolean, };

export type SnapshotProviderConflict = { table: string, source_id: number, reason: SnapshotProviderConflictReason, candidates: number, blocking: boolean, };

export type SnapshotProviderConflictReason = "ambiguous" | "differs" | "mapping_changed" | "client_unmapped";

export type SnapshotReport = { application: SnapshotApplication, fingerprint: string, schema_version: number, dry_run: boolean, applied: boolean, mapped: number, duplicates: number, metadata_backfilled: number, conflicts: number, missing_file_records: number, unsupported: Array<SnapshotUnsupported>, provider_conflicts: Array<SnapshotProviderConflict>, policy: string, };

export type SnapshotUnsupported = { table: string, rows: number, columns: Array<string>, };

export type SourceHistoryEvent = { id: SourceHistoryIdentity, event_type: SourceHistoryEventType, source_event_type: number, target: MediaTarget, occurred_at: string, source_title: string | null, download_id: string | null, quality: FileQuality | null, languages: Array<number> | null, };

export type SourceHistoryEventType = "grabbed" | "series_folder_imported" | "download_folder_imported" | "download_failed" | "file_deleted" | "file_renamed" | "download_ignored" | "movie_folder_imported";

export type SourceHistoryIdentity = { application: SnapshotApplication, fingerprint: string, source_id: number, };

export type Tag = { id: number, media_type: MediaDomain, label: string, };

export type TagAssignment = { mode: TagAssignmentMode, ids: Array<number>, };

export type TagAssignmentMode = "add" | "remove" | "replace";

export type TagDetail = { tag: Tag, in_use: boolean, owner_count: number, delay_profile_ids: Array<number>, release_profile_ids: Array<number>, excluded_release_profile_ids: Array<number>, download_client_ids: Array<string>, auto_tagging_ids: Array<number>, collection_ids: Array<number>, };

export type TagInput = { label: string, };

export type TagOwners = { ids: Array<number>, total: number, limit: number, offset: number, };

export type TagOwnersQuery = { limit?: number, offset?: number, };

export type TestStatus = "never_tested" | "success" | "failure";

export type TorrentLabelScope = { category: string, recent_priority?: TorrentQueuePriority, older_priority?: TorrentQueuePriority, add_paused?: boolean, tags?: Array<number>, };

export type TorrentPlacement = { "strategy": "category", category: string, } | { "strategy": "directory", directory: string, };

export type TorrentQueuePriority = "last" | "first";

export type TransmissionScope = { placement: TorrentPlacement, recent_priority?: TorrentQueuePriority, older_priority?: TorrentQueuePriority, add_paused?: boolean, tags?: Array<number>, };

export type TransmissionTest = { transmission_version: string, rpc_version: number, domains: Array<MediaDomain>, labels_supported: boolean, warnings: Array<TransmissionTestWarning>, };

export type TransmissionTestWarning = "labels_unsupported" | "queue_disabled";

export type TvCalendarEntry = { event_id: string, target: MediaTarget, series_id: number, series_title: string, series_type: SeriesType | null, series_monitored: boolean, network: string | null, season: number, season_monitored: boolean | null, number: number, absolute_episode_number: number | null, title: string, overview: string | null, finale_type: string | null, monitored: boolean, effective_monitored: boolean, air_date: string | null, starts_at: string, runtime_minutes: number | null, runtime_source: CalendarRuntimeSource | null, ends_at: string | null, has_file: boolean, };

export type TvFileResource = { series_id: number, season_number: number | null, release_type: number | null, id: number, path: string, relative_path: string | null, quality: FileQuality | null, languages: Array<number> | null, size: number | null, date_added: string | null, release_group: string | null, indexer_flags: number | null, scene_name: null, media_info: MediaInfo | null, custom_formats: null, custom_format_score: null, quality_cutoff_not_met: null, };

export type TvIndexerScope = { enable_rss?: boolean, enable_automatic_search?: boolean, enable_interactive_search?: boolean, download_client_id?: string | null, categories: Array<number>, anime_categories: Array<number>, anime_standard_format_search?: boolean, };

export type TvNamingConfig = { revision: number, rename_enabled: boolean, replace_illegal_characters: boolean, colon_replacement: ColonReplacement, custom_colon_replacement: string | null, standard_episode_format: string | null, daily_episode_format: string | null, anime_episode_format: string | null, series_folder_format: string | null, season_folder_format: string | null, specials_folder_format: string | null, multi_episode_style: number | null, };

export type TvNamingExamples = { standard_episode_format: string | null, daily_episode_format: string | null, anime_episode_format: string | null, series_folder_format: string | null, season_folder_format: string | null, specials_folder_format: string | null, multi_episode_examples: Array<MultiEpisodeNamingExample>, };

export type TvNamingExamplesQuery = { rename_enabled?: boolean, replace_illegal_characters?: boolean, colon_replacement?: ColonReplacement, custom_colon_replacement?: string, standard_episode_format?: string, daily_episode_format?: string, anime_episode_format?: string, series_folder_format?: string, season_folder_format?: string, specials_folder_format?: string, multi_episode_style?: number, multi_episode_draft?: boolean, };

export type TvNamingUpdate = { revision: number, rename_enabled: boolean, replace_illegal_characters: boolean, colon_replacement: ColonReplacement, custom_colon_replacement: string | null, standard_episode_format: string | null, daily_episode_format: string | null, anime_episode_format: string | null, series_folder_format: string | null, season_folder_format: string | null, specials_folder_format: string | null, multi_episode_style: number | null, };

export type TvNotificationScope = { triggers: TvNotificationTriggers, 
/**
 * TV tag IDs. Empty means every series; otherwise a series must carry one of them.
 */
tag_ids: Array<number>, };

export type TvNotificationTriggers = { on_grab: boolean, on_import: boolean, on_upgrade: boolean, on_import_complete: boolean, on_rename: boolean, on_series_add: boolean, on_series_delete: boolean, on_episode_file_delete: boolean, on_episode_file_delete_for_upgrade: boolean, on_health_issue: boolean, include_health_warnings: boolean, on_health_restored: boolean, on_application_update: boolean, on_manual_interaction_required: boolean, };

export type TvNumbering = { "kind": "episode", season: number, episode: number, } | { "kind": "season", season: number, } | { "kind": "daily", date: string, } | { "kind": "daily_season", year: number, } | { "kind": "special", episode_title: string, } | { "kind": "anime", absolute_episode: number, season?: number | null, episode?: number | null, } | { "kind": "anime_season", season: number, season_aliases?: Array<string>, };

export type TvParseResult = { title: string, parsed: ParsedRelease | null, series: MatchedSeries | null, episodes: Array<MatchedEpisode>, };

export type TvReleaseProfileInput = { name: string | null, enabled: boolean, required: Array<string>, ignored: Array<string>, tag_ids: Array<number>, indexers: Array<ReleaseProfileIndexer>, excluded_tag_ids: Array<number>, air_date_restriction: boolean, air_date_grace_period_days: number, allow_season_pack_without_all_episodes_aired: boolean, };

export type TvRenamePreview = { naming_revision: number, rename_enabled: boolean, series_ids: Array<number>, season: number | null, files_considered: number, unchanged_count: number, unavailable_count: number, blocked_count: number, items: Array<TvRenamePreviewItem>, };

export type TvRenamePreviewItem = { series_id: number, episode_file_id: number, season_number: number | null, episode_ids: Array<number>, episode_numbers: Array<number>, existing_path: string | null, new_path: string | null, status: RenameStatus, reasons: Array<RenameReason>, };

export type TvSearchMode = "default" | "ids" | "titles" | "both";

export type TvWantedItem = { target: MediaTarget, series_id: number, series_title: string, series_monitored: boolean, series_type: SeriesType | null, season: number, season_monitored: boolean | null, number: number, absolute_episode_number: number | null, scene_season_number: number | null, scene_episode_number: number | null, scene_absolute_episode_number: number | null, title: string, monitored: boolean, effective_monitored: boolean, air_date: string | null, air_date_utc: string | null, aired_end_at: number | null, last_search_time: string | null, profile_id: number | null, file: WantedFile | null, cutoff: WantedCutoff | null, };

export type UnmappedFolder = { name: string, path: string, relative_path: string, };

export type UsenetPriority = "default" | "paused" | "low" | "normal" | "high" | "force";

export type UsenetScope = { category: string, recent_priority?: UsenetPriority, older_priority?: UsenetPriority, tags?: Array<number>, };

export type WantedAvailability = "all" | "available" | "unavailable";

export type WantedCutoff = { profile_id: number, quality_rank: number, threshold_rank: number, basis: WantedCutoffBasis, };

export type WantedCutoffBasis = "configured_cutoff" | "first_allowed";

export type WantedDiagnostics = { unknown_air_date: number | null, unknown_series_runtime: number | null, unknown_year: number | null, unknown_cutoff: number | null, };

export type WantedDirection = "asc" | "desc";

export type WantedFile = { id: number, path: string, quality_id: number | null, quality_name: string | null, };

export type WantedKind = "missing" | "cutoff";

export type WantedMonitored = "monitored" | "unmonitored" | "all";

export type WantedMovieAvailability = "available" | "unavailable" | "ungated";

export type WantedPage<T> = { media_type: MediaDomain, kind: WantedKind, items: Array<T>, total: number, limit: number, offset: number, sort: WantedSort, direction: WantedDirection, monitored: WantedMonitored, evaluated_at: number, diagnostics: WantedDiagnostics, };

export type WantedQuery = { limit?: number, offset?: number, monitored?: WantedMonitored, sort?: WantedSort, direction?: WantedDirection, availability?: WantedAvailability, };

export type WantedSearchCommand = { target: MediaTarget, command_id: string, };

export type WantedSearchInput = { request_id: string, kind: WantedKind, selection: WantedSelection, indexer_id: string, indexer_revision: number, client_selection: ClientSelection, priority: CommandPriority, };

export type WantedSearchReceipt = { request_id: string, media_type: MediaDomain, kind: WantedKind, replayed: boolean, matched: number | null, skipped_active: number | null, admitted: number, commands: Array<WantedSearchCommand>, };

export type WantedSelection = { "kind": "ids", ids: Array<number>, } | { "kind": "all", monitored: WantedMonitored, availability?: WantedAvailability, };

export type WantedSort = "air_date" | "series_title" | "title" | "year" | "in_cinemas" | "digital_release" | "physical_release" | "quality";
