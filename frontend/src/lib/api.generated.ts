// Generated from the Rust HTTP DTOs by ts-rs. Do not edit.
// Regenerate: cargo run --locked --bin generate-api
// JSON i64/u64 remain numbers on the wire. Reject integers outside Number.isSafeInteger
// at the API boundary before using IDs, sizes or duration ticks; these types are not validators.
// URL-query properties are omittable scalars; nullability below describes JSON bodies/resources.

export type ApiErrorDetail = { code: string, message: string, };

export type ApiErrorEnvelope = { error: ApiErrorDetail, };

export type ApiPage<T> = { items: Array<T>, total: number, limit: number, offset: number, };

export type Capabilities = { max_limit: number, default_limit: number, search: SearchCapability, tv: SearchCapability, movies: SearchCapability, categories: Array<IndexerCategory>, };

export type ClientTest = { api_version: string, application_version: string, domains: Array<MediaDomain>, missing_categories: Array<string>, queueing_enabled: boolean, };

export type DownloadContentLayout = "default" | "original" | "subfolder";

export type DownloadDiagnostic = "error" | "missing_files" | "stalled" | "metadata" | "dht_disabled" | "unknown_state";

export type DownloadFile = { index: number, name: string, size_bytes: number, progress: number, priority: number, };

export type DownloadFiles = { domain: MediaDomain, hash: string, files: Array<DownloadFile>, };

export type DownloadFilesQuery = { domain: MediaDomain, hash: string, };

export type DownloadInitialState = "started" | "stopped" | "forced";

export type DownloadItem = { hash: string, domain: MediaDomain, name: string, category: string, status: DownloadStatus, diagnostic: DownloadDiagnostic | null, progress: number, size_bytes: number, remaining_bytes: number | null, download_bytes_per_second: number, upload_bytes_per_second: number, eta_seconds: number | null, ratio: number, seeding_seconds: number | null, completed: boolean, };

export type DownloadPage = { domain: MediaDomain, offset: number, limit: number, next_offset: number | null, items: Array<DownloadItem>, };

export type DownloadQuery = { domain: MediaDomain, offset: number, limit: number, imported: boolean, };

export type DownloadScope = { category: string, imported_category: string | null, recent_priority: number, older_priority: number, initial_state?: DownloadInitialState, content_layout?: DownloadContentLayout, sequential_order?: boolean, first_last_first?: boolean, add_tags?: boolean, };

export type DownloadStatus = "queued" | "downloading" | "paused" | "completed" | "failed" | "warning" | "stalled" | "unknown";

export type Episode = { id: number, series_id: number, season: number, number: number, title: string, monitored: boolean, episode_file_id: number | null, has_file: boolean, file_path: string | null, tvdb_id: number | null, air_date: string | null, air_date_utc: string | null, last_search_time: string | null, runtime: number | null, finale_type: string | null, overview: string | null, absolute_episode_number: number | null, scene_absolute_episode_number: number | null, scene_episode_number: number | null, scene_season_number: number | null, unverified_scene_numbering: boolean | null, series?: EpisodeSeriesProjection, episode_file?: EpisodeFileProjection, images?: Array<EpisodeCover> | null, };

export type EpisodeCover = { coverType: EpisodeCoverType, url?: string | null, remoteUrl?: string | null, };

export type EpisodeCoverType = "unknown" | "poster" | "banner" | "fanart" | "screenshot" | "headshot" | "clearlogo";

export type EpisodeFileProjection = { id: number, series_id: number, path: string, };

export type EpisodeIncludes = { include_series?: boolean, include_episode_file?: boolean, include_images?: boolean, };

export type EpisodeMonitor = { monitored: boolean, };

export type EpisodeMonitorMany = { episode_ids: Array<number>, monitored: boolean, };

export type EpisodeQuery = { series_id?: number, season?: number, episode_ids?: string, episode_file_id?: number, offset?: number, limit?: number, include_series?: boolean, include_episode_file?: boolean, include_images?: boolean, };

export type EpisodeSeriesProjection = { id: number, tvdb_id: number | null, title: string, year: number | null, path: string, poster: string | null, monitored: boolean, };

export type FileBulk = { files: Array<FileUpdate>, };

export type FileEditor = { file_ids: Array<number>, quality?: FileQualityInput | null, languages?: Array<number> | null, release_group?: string | null, edition?: string | null, indexer_flags?: number | null, release_type?: number | null, };

export type FilePatch = { quality?: FileQualityInput | null, languages?: Array<number> | null, release_group?: string | null, edition?: string | null, indexer_flags?: number | null, release_type?: number | null, };

export type FileQuality = { quality_id: number, revision: FileRevision | null, };

export type FileQualityInput = { quality_id: number, revision?: FileRevision | null, };

export type FileQuery = { series_id?: number, movie_ids?: string, file_ids?: string, offset?: number, limit?: number, };

export type FileResource = TvFileResource | MovieFileResource;

export type FileRevision = { version: number, real: number, is_repack: boolean, };

export type FileUpdate = { id: number, quality?: FileQualityInput | null, languages?: Array<number> | null, release_group?: string | null, edition?: string | null, indexer_flags?: number | null, release_type?: number | null, };

export type ImportInput = ManualImportRequest | ImportRequest;

export type ImportRequest = { episode_id: number, source: string, mode: string, destination: string, };

export type IndexerCategory = { id: number, parent_id: number | null, };

export type IndexerContinuation = { query_index: number, offset: number, };

export type IndexerItemWarning = { index: number, code: IndexerItemWarningCode, };

export type IndexerItemWarningCode = "invalid_item";

export type IndexerPage = { warnings: Array<IndexerItemWarning>, query_index: number, query_count: number, next_query: IndexerContinuation | null, media_type: MediaDomain, items: Array<ReleaseMetadata>, offset: number, limit: number, total: number | null, next_offset: number | null, };

export type IndexerParameter = { name: string, value: string, };

export type IndexerSearch = { "kind": "rss", media_type: MediaDomain, offset?: number, query_index?: number, limit?: number, } | { "kind": "tv", title: string, aliases?: Array<string>, tvdb_id?: number | null, tvmaze_id?: number | null, rage_id?: number | null, imdb_id?: string | null, tmdb_id?: number | null, numbering: TvNumbering, search_mode?: TvSearchMode, offset?: number, query_index?: number, limit?: number, } | { "kind": "movie", title: string, aliases?: Array<string>, year?: number | null, imdb_id?: string | null, tmdb_id?: number | null, offset?: number, query_index?: number, limit?: number, };

export type IndexerTest = { capabilities: Capabilities, domains: Array<MediaDomain>, };

export type LegacyEpisode = { id: number, series_id: number, season: number, number: number, title: string, file_path: string | null, };

export type LegacyError = { error: string, };

export type LegacySeries = { id: number, title: string, year: number | null, path: string, poster: string | null, };

export type LibraryBulk = { items: Array<LibraryUpdate>, };

export type LibraryCreate = { title?: string | null, year?: number | null, tvdb_id?: number | null, tmdb_id?: number | null, imdb_id?: string | null, metadata_id?: number | null, path: string, settings?: LibraryPatch, };

export type LibraryEditor = { ids: Array<number>, patch: LibraryPatch, };

export type LibraryItem = { id: number, media_type: MediaDomain, metadata_id: number | null, title: string, year: number | null, tvdb_id: number | null, tmdb_id: number | null, imdb_id: string | null, path: string, poster: string | null, monitored: boolean, settings: LibrarySettings, statistics: LibraryStatistics, seasons?: Array<LibrarySeason>, is_available: boolean | null, };

export type LibraryPage = { items: Array<LibraryItem>, total: number, limit: number, offset: number, };

export type LibraryPatch = { monitored?: boolean, quality_profile_id?: number | null, series_type?: SeriesType | null, season_folder?: boolean | null, use_scene_numbering?: boolean | null, monitor_new_items?: MonitorNewItems | null, minimum_availability?: MinimumAvailability | null, seasons?: Array<LibrarySeasonInput> | null, };

export type LibraryQuery = { limit?: number, offset?: number, ids?: string, tvdb_id?: number, tmdb_id?: number, };

export type LibrarySeason = { number: number, monitored: boolean, statistics: LibraryStatistics, };

export type LibrarySeasonInput = { number: number, monitored: boolean, };

export type LibrarySettings = { quality_profile_id: number | null, profile_name: string | null, series_type: string | null, season_folder: boolean | null, use_scene_numbering: boolean | null, monitor_new_items: string | null, minimum_availability: string | null, added: string | null, };

export type LibraryStatistics = { total_episode_count: number | null, episode_count: number | null, episode_file_count: number | null, file_count: number, known_size_file_count: number, size_on_disk: number | null, season_count: number | null, };

export type LibraryUpdate = { id: number, patch: LibraryPatch, };

export type ManualImportRequest = { target: MediaTarget, source: string, mode: Mode, destination: string, };

export type MediaDomain = "tv" | "movies";

export type MediaInfo = { schema_revision: number | null, container_format: string | null, audio_bitrate: number | null, audio_channels: number | null, audio_codec: string | null, audio_languages: string | null, audio_stream_count: number | null, video_bit_depth: number | null, video_bitrate: number | null, video_codec: string | null, video_fps: number | null, video_dynamic_range: string | null, video_dynamic_range_type: string | null, resolution: string | null, run_time: string | null, scan_type: string | null, subtitles: string | null, video_format: string | null, video_codec_id: string | null, video_profile: string | null, audio_format: string | null, audio_codec_id: string | null, audio_profile: string | null, audio_channel_count: number | null, audio_channel_positions: string | null, width: number | null, height: number | null, runtime_ticks: number | null, audio_streams: Array<MediaInfoAudioStream> | null, subtitle_streams: Array<MediaInfoSubtitleStream> | null, };

export type MediaInfoAudioStream = { language: string | null, format: string | null, codec_id: string | null, profile: string | null, bitrate: number | null, channels: number | null, channel_positions: string | null, };

export type MediaInfoSubtitleStream = { language: string | null, format: string | null, forced: boolean | null, hearing_impaired: boolean | null, };

export type MediaTarget = { "media_type": "episode", "id": number } | { "media_type": "movie", "id": number };

export type MinimumAvailability = "tba" | "announced" | "in_cinemas" | "released";

export type Mode = "copy" | "move" | "hardlink";

export type MonitorNewItems = "all" | "none";

export type MovieFileResource = { movie_id: number, edition: string | null, original_file_path: string | null, id: number, path: string, relative_path: string | null, quality: FileQuality | null, languages: Array<number> | null, size: number | null, date_added: string | null, release_group: string | null, indexer_flags: number | null, scene_name: null, media_info: MediaInfo | null, custom_formats: null, custom_format_score: null, quality_cutoff_not_met: null, };

export type MovieIndexerScope = { categories: Array<number>, remove_year?: boolean, };

export type Operation = { id: string, target: MediaTarget, status: string, message: string, error_code: string | null, };

export type Provider = { id: string, revision: number, name: string, enabled: boolean, priority: number, settings: ProviderSettings, has_credentials: boolean, test_supported: boolean, test_status: TestStatus, last_test: ProviderTestObservation | null, };

export type ProviderCredentials = { "kind": "api_key", api_key: string, } | { "kind": "username_password", username: string, password: string, } | { "kind": "indexer", api_key?: string | null, tv_parameters?: Array<IndexerParameter>, movie_parameters?: Array<IndexerParameter>, };

export type ProviderDownloadResult = { provider_id: string, revision: number, page: DownloadPage, };

export type ProviderFilesResult = { provider_id: string, revision: number, result: DownloadFiles, };

export type ProviderInput = { name: string, enabled: boolean, priority: number, settings: ProviderSettings, credentials?: ProviderCredentials | null, };

export type ProviderQuery = { limit?: number, offset?: number, media_type?: MediaDomain, };

export type ProviderRevision = { revision: number, };

export type ProviderSearchResult = { provider_id: string, revision: number, page: IndexerPage, };

export type ProviderSettings = { "implementation": "torznab", endpoint: string, tv: TvIndexerScope | null, movies: MovieIndexerScope | null, } | { "implementation": "newznab", endpoint: string, tv: TvIndexerScope | null, movies: MovieIndexerScope | null, } | { "implementation": "qbittorrent", endpoint: string, tv: DownloadScope | null, movies: DownloadScope | null, };

export type ProviderTestObservation = { revision: number, tested_at: number, status: TestStatus, error_code: string | null, };

export type ProviderTestOutcome = IndexerTest | ClientTest;

export type ProviderTestResult = { provider_id: string, revision: number, tested_at: number, result: ProviderTestOutcome, };

export type ProviderUpdate = { revision: number, name: string, enabled: boolean, priority: number, settings: ProviderSettings, credentials?: ProviderCredentials | null, };

export type Quality = { id: number, name: string, source: string, resolution: number, modifier?: string, };

export type QualityDefinition = { id: number, media_type: MediaDomain, quality: Quality, title: string, weight: number, group_name: string | null, min_size: number | null, max_size: number | null, preferred_size: number | null, };

export type QualityDefinitionLimits = { min: number, max: number, unit: string, };

export type QualityDefinitionReset = { reset_titles?: boolean, };

export type QualityDefinitionUpdate = { id: number, title: string, min_size?: number | null, max_size?: number | null, preferred_size?: number | null, };

export type QualityProfile = { id: number, media_type: MediaDomain, name: string, items: Array<QualityProfileItem>, };

export type QualityProfileInput = { name: string, items: Array<QualityProfileItemInput>, };

export type QualityProfileItem = { "kind": "quality" } & QualityProfileLeaf | { "kind": "group", name: string, allowed: boolean, items: Array<QualityProfileLeaf>, };

export type QualityProfileItemInput = { "kind": "quality" } & QualityProfileLeafInput | { "kind": "group", name: string, allowed: boolean, items: Array<QualityProfileLeafInput>, };

export type QualityProfileLeaf = { quality_id: number, allowed: boolean, min_size: number | null, max_size: number | null, preferred_size: number | null, };

export type QualityProfileLeafInput = { quality_id: number, allowed: boolean, min_size?: number | null, max_size?: number | null, preferred_size?: number | null, };

export type QualityProfilePage = { media_type: MediaDomain, items: Array<QualityProfileSummary>, total: number, offset: number, limit: number, };

export type QualityProfileQuery = { offset?: number, limit?: number, };

export type QualityProfileSummary = { id: number, name: string, item_count: number, group_count: number, };

export type ReleaseMetadata = { title: string | null, size_bytes: number | null, published_at: string, categories: Array<number>, seeders: number | null, leechers: number | null, peers: number | null, languages: Array<string>, };

export type SearchCapability = { available: boolean, parameters: Array<string>, aggregate_ids: boolean, search_engine: SearchEngine, };

export type SearchEngine = "raw" | "sphinx";

export type SeriesType = "standard" | "daily" | "anime";

export type SnapshotApplication = "sonarr" | "radarr";

export type SnapshotOptions = { application: SnapshotApplication, dry_run?: boolean, import_providers?: boolean, };

export type SnapshotReport = { application: SnapshotApplication, fingerprint: string, schema_version: number, dry_run: boolean, applied: boolean, mapped: number, duplicates: number, metadata_backfilled: number, conflicts: number, missing_file_records: number, unsupported: Array<SnapshotUnsupported>, policy: string, };

export type SnapshotUnsupported = { table: string, rows: number, columns: Array<string>, };

export type TestStatus = "never_tested" | "success" | "failure";

export type TvFileResource = { series_id: number, season_number: number | null, release_type: number | null, id: number, path: string, relative_path: string | null, quality: FileQuality | null, languages: Array<number> | null, size: number | null, date_added: string | null, release_group: string | null, indexer_flags: number | null, scene_name: null, media_info: MediaInfo | null, custom_formats: null, custom_format_score: null, quality_cutoff_not_met: null, };

export type TvIndexerScope = { categories: Array<number>, anime_categories: Array<number>, anime_standard_format_search?: boolean, };

export type TvNumbering = { "kind": "episode", season: number, episode: number, } | { "kind": "season", season: number, } | { "kind": "daily", date: string, } | { "kind": "daily_season", year: number, } | { "kind": "special", episode_title: string, } | { "kind": "anime", absolute_episode: number, season?: number | null, episode?: number | null, } | { "kind": "anime_season", season: number, season_aliases?: Array<string>, };

export type TvSearchMode = "default" | "ids" | "titles" | "both";
