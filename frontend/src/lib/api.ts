import type { Tag, TagDetail, TagOwners, TagAssignment } from './api.generated';
import type { ManualImportBatchInput, ManualImportCommand, ManualImportReconciliation } from './api.generated';
import type { DownloadedScanInput, DownloadedScanCommand, DownloadedScanQuery, DownloadedScanChildQuery, ScanChild, ScanChildResumeInput, ScanCleanupResumeInput, ScanChildMember, ScanMemberQuery } from './api.generated';
import type { QualityProfile, QualityProfileInput, QualityDefinition } from './api.generated';
import type { SearchCommandInput, SearchCommand, SearchResult, MediaTarget, ProcessingPolicy, ProcessingPolicyInput, ProcessingInput, DownloadProcessing, RssTargetInput, RssInput, RssCommand, RssCandidate, RssScheduleInput, RssSchedule, ReleasePolicy, ReleaseSearchInput, ReleaseSearchPage, QualityProfilePage, BlocklistClearCommand, BlocklistClearInput, BlocklistEntry, BlocklistIdentity, BlocklistQuery, BlocklistRemoval, MetadataCommand, MetadataCommandInput, MetadataCommandQuery, MetadataRefreshTarget, Command, CommandInput, CommandQuery, CommandPriority, RefreshTarget, RefreshSchedule, RefreshScheduleInput, RefreshScheduleDelete, QueueSnapshot, Provider, ProviderInput, ProviderUpdate, ProviderSchema, ProviderKind, ProviderTestResult, ApiPage, MovieCredit, Episode, LibraryItem, LibraryPage, LibraryPatch, LookupResult, ManualImportRequest, MediaDomain, ApiErrorEnvelope, ImportRequest, LegacyEpisode, LegacyError, LegacySeries, Operation, TvNamingConfig, TvNamingUpdate, TvNamingExamples, TvNamingExamplesQuery, MovieNamingConfig, MovieNamingUpdate, MovieNamingExamples, MovieNamingExamplesQuery, RootFolder, RescanBatch, RescanCommand } from './api.generated';

export type SettingsWriteState = 'idle' | 'busy' | 'uncertain';
export type Result<T> = { ok: true; data: T } | { ok: false; error: string; code?: string; status?: number };
const MAX_RESPONSE_BYTES = 8 * 1024 * 1024;
const REQUEST_TIMEOUT_MS = 10_000;

function safeNumbers(value: unknown): boolean {
  const pending: unknown[] = [value];
  while (pending.length) {
    const current = pending.pop();
    if (typeof current === 'number' && (!Number.isFinite(current) ||
        (Number.isInteger(current) && !Number.isSafeInteger(current)))) return false;
    if (current && typeof current === 'object') {
      for (const child of Object.values(current)) pending.push(child);
    }
  }
  return true;
}

async function request<T>(path: string, body?: unknown, method?: 'POST' | 'PUT' | 'DELETE', native = false, timeout = REQUEST_TIMEOUT_MS): Promise<Result<T>> {
  if (body && !safeNumbers(body)) return { ok: false, error: 'Request contains an unsafe integer.' };
  try {
    const response = await fetch(path, {
      signal: AbortSignal.timeout(timeout),
      ...(body !== undefined ? { method: method ?? 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) } : method ? { method } : {}),
    });
    if (method === 'DELETE' && response.status === 204) return { ok: true, data: undefined as T };
    const reader = response.body?.getReader();
    if (!reader) return { ok: false, error: `Empty API response (${response.status}).` };
    const decoder = new TextDecoder();
    let text = '', bytes = 0;
    try {
      while (true) {
        const chunk = await reader.read();
        if (chunk.done) break;
        bytes += chunk.value.byteLength;
        if (bytes > MAX_RESPONSE_BYTES) {
          await reader.cancel();
          return { ok: false, error: 'API response exceeds 8 MiB.' };
        }
        text += decoder.decode(chunk.value, { stream: true });
      }
      text += decoder.decode();
    } finally {
      reader.releaseLock();
    }
    let data: unknown;
    try { data = JSON.parse(text); }
    catch { return { ok: false, error: `Invalid API response (${response.status}).` }; }
    if (!safeNumbers(data)) return { ok: false, error: 'API response contains an integer outside JavaScript’s safe range.' };
    if (!response.ok) {
      const error: unknown = data && typeof data === 'object' && 'error' in data
        ? (data as LegacyError | ApiErrorEnvelope).error : undefined;
      const message = typeof error === 'string' ? error :
        error && typeof error === 'object' && 'message' in error && typeof error.message === 'string' ? error.message :
        `API request failed (${response.status}).`;
      return { ok: false, error: message, ...(native ? { status: response.status, ...(error && typeof error === 'object' && 'code' in error && typeof error.code === 'string' ? { code: error.code } : {}) } : {}) };
    }
    return { ok: true, data: data as T };
  } catch (error) {
    return { ok: false, error: error instanceof Error ? error.message : 'API request failed.' };
  }
}

export const loadSeries = () => request<LegacySeries[]>('/api/v1/series');
export const loadEpisodes = (seriesId: number): Promise<Result<LegacyEpisode[]>> =>
  Number.isSafeInteger(seriesId) && seriesId > 0
    ? request<LegacyEpisode[]>(`/api/v1/series/${seriesId}/episodes`)
    : Promise.resolve({ ok: false, error: 'Series ID must be a positive safe integer.' });
export const previewImport = (body: ImportRequest) =>
  Number.isSafeInteger(body.episode_id) && body.episode_id > 0
    ? request<Operation>('/api/v1/imports', body)
    : Promise.resolve<Result<Operation>>({ ok: false, error: 'Episode ID must be a positive safe integer.' });

const collection = (domain: MediaDomain) => domain === 'tv' ? '/api/v1/tv/series' : '/api/v1/movies';
const validId = (id: number) => Number.isSafeInteger(id) && id > 0;
const invalid = <T>(): Promise<Result<T>> => Promise.resolve({ ok: false, error: 'ID must be a positive safe integer.' });
export const listLibrary = (domain: MediaDomain, offset = 0) => request<LibraryPage>(`${collection(domain)}?limit=25&offset=${offset}`, undefined, undefined, true);
export const getLibrary = (domain: MediaDomain, id: number) => validId(id) ? request<LibraryItem>(`${collection(domain)}/${id}`, undefined, undefined, true) : invalid<LibraryItem>();
export const lookupLibrary = (domain: MediaDomain, term: string) => request<LookupResult[]>(`${collection(domain)}/lookup?${new URLSearchParams({term})}`, undefined, undefined, true, 35_000);
export const addLibrary = (domain: MediaDomain, id: number, path: string, settings: LibraryPatch = {}) => validId(id) ? request<LibraryItem>(`${collection(domain)}/lookup`, { [domain === 'tv' ? 'tvdb_id' : 'tmdb_id']: id, path, settings }, 'POST', true, 35_000) : invalid<LibraryItem>();
export const findLibraryByExternalId = (domain: MediaDomain, externalId: number) => validId(externalId) ? request<LibraryPage>(`${collection(domain)}?limit=1&${domain === 'tv' ? 'tvdb_id' : 'tmdb_id'}=${externalId}`, undefined, undefined, true) : invalid<LibraryPage>();
export const updateLibrary = (domain: MediaDomain, id: number, patch: LibraryPatch) => validId(id) ? request<LibraryItem>(`${collection(domain)}/${id}`, patch, 'PUT', true) : invalid<LibraryItem>();
export const listEpisodes = (seriesId: number, offset = 0) => validId(seriesId) ? request<ApiPage<Episode>>(`/api/v1/episodes?series_id=${seriesId}&limit=25&offset=${offset}`, undefined, undefined, true) : invalid<ApiPage<Episode>>();
export const listMovieCredits = (movieId: number) => validId(movieId) ? request<ApiPage<MovieCredit>>(`/api/v1/movies/credits?movie_id=${movieId}&limit=500`, undefined, undefined, true) : invalid<ApiPage<MovieCredit>>();
export const monitorEpisode = (id: number, monitored: boolean) => validId(id) ? request<Episode>(`/api/v1/episodes/${id}`, {monitored}, 'PUT', true) : invalid<Episode>();
export const previewManualImport = (body: ManualImportRequest) => validId(body.target.id) ? request<Operation>('/api/v1/imports', body, 'POST', true) : invalid<Operation>();
const validOperation = (id: string) => /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(id);
export const getImport = (id: string) => validOperation(id) ? request<Operation>(`/api/v1/imports/${id}`, undefined, undefined, true) : Promise.resolve<Result<Operation>>({ok:false,error:'Invalid operation ID.'});
export const executeImport = (id: string) => validOperation(id) ? request<Operation>(`/api/v1/imports/${id}/execute`, undefined, 'POST', true) : Promise.resolve<Result<Operation>>({ok:false,error:'Invalid operation ID.'});

const validManualImportIds = (ids: string[]) => Array.isArray(ids) && ids.length >= 1 && ids.length <= 100
  && ids.every(id => typeof id === 'string' && validOperation(id))
  && new Set(ids.map(id => id.toLowerCase())).size === ids.length;
export const createManualImportCommands = (input: ManualImportBatchInput) =>
  validManualImportIds(input.operation_ids) && ['normal', 'high'].includes(input.priority)
    ? request<ManualImportCommand[]>('/api/v1/manual-import/commands', input, 'POST', true)
    : Promise.resolve<Result<ManualImportCommand[]>>({ok: false, error: 'Choose 1–100 distinct operation IDs and a valid priority.'});
export const getManualImportCommand = (id: string) => validOperation(id)
  ? request<ManualImportCommand>(`/api/v1/manual-import/commands/${id}`, undefined, undefined, true)
  : Promise.resolve<Result<ManualImportCommand>>({ok: false, error: 'Invalid command ID.'});
export const cancelManualImportCommand = (id: string) => validOperation(id)
  ? request<ManualImportCommand>(`/api/v1/manual-import/commands/${id}/cancel`, undefined, 'POST', true)
  : Promise.resolve<Result<ManualImportCommand>>({ok: false, error: 'Invalid command ID.'});
export const reconcileManualImports = (operationIds: string[]) => validManualImportIds(operationIds)
  ? request<ManualImportReconciliation[]>('/api/v1/manual-import/commands/reconcile', {operation_ids: operationIds}, 'POST', true)
  : Promise.resolve<Result<ManualImportReconciliation[]>>({ok: false, error: 'Choose 1–100 distinct operation IDs.'});

export const listProviders = (offset = 0) => request<ApiPage<Provider>>(`/api/v1/providers?limit=25&offset=${offset}`, undefined, undefined, true);
export const getProviderSchema = (media: MediaDomain, kind: ProviderKind) => request<ProviderSchema>(`/api/v1/providers/schema?${new URLSearchParams({media_type: media, kind})}`, undefined, undefined, true);
// Downloaded-folder scan (phase A discovery): one POST authorizes bounded discovery of exactly one
// named file or folder. Nothing here retries; a lost response is reconciled by GET with the SAME id.
const scanId = (id: unknown): id is string => typeof id === 'string' && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(id);
const scanPath = (path: unknown): path is string => typeof path === 'string'
  && path.startsWith('/') && path !== '/' && !path.endsWith('/') && !path.includes('\0')
  && new TextEncoder().encode(path).length <= 4096
  && path.slice(1).split('/').every(part => part !== '' && part !== '.' && part !== '..');
const scanRemote = (remote: unknown): remote is string => typeof remote === 'string' && /^(?:[0-9a-f]{40}|[0-9a-f]{64})$/.test(remote);
const scanDomain = (domain: unknown): domain is MediaDomain => domain === 'tv' || domain === 'movies';
const scanRoot = (domain: MediaDomain) => `/api/v1/${domain}/downloaded-scan-commands`;
const scanError = <T>(error: string) => Promise.resolve<Result<T>>({ ok: false, error });
const scanPage = (query: { limit?: number; offset?: number } | undefined, extra: Record<string, string | undefined>) => {
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(extra)) if (value !== undefined) params.set(key, value);
  if (query?.limit !== undefined) params.set('limit', String(query.limit));
  if (query?.offset !== undefined) params.set('offset', String(query.offset));
  const text = params.toString();
  return text ? `?${text}` : '';
};
const validScanPage = (query: { limit?: number; offset?: number } | undefined) => !query
  || ((query.limit === undefined || (Number.isInteger(query.limit) && query.limit >= 1 && query.limit <= 100))
    && (query.offset === undefined || (Number.isInteger(query.offset) && query.offset >= 0 && query.offset <= 100_000)));
export const createDownloadedScanCommand = (domain: MediaDomain, input: DownloadedScanInput) => {
  if (!scanDomain(domain)) return scanError<DownloadedScanCommand>('Choose TV or movies.');
  if (!scanId(input.id)) return scanError<DownloadedScanCommand>('Invalid command ID.');
  if (!scanPath(input.path)) return scanError<DownloadedScanCommand>('Enter an absolute, normalized file or folder path of at most 4096 bytes.');
  if (!['auto', 'move', 'copy'].includes(input.mode) || !['normal', 'high'].includes(input.priority)) return scanError<DownloadedScanCommand>('Choose a valid mode and priority.');
  if (input.tracked !== undefined && (!scanId(input.tracked.provider_id) || !scanRemote(input.tracked.remote_id))) return scanError<DownloadedScanCommand>('Tracked downloads need a provider ID and a lowercase 40- or 64-character hash.');
  return request<DownloadedScanCommand>(scanRoot(domain), input, 'POST', true);
};
export const listDownloadedScanCommands = (domain: MediaDomain, query?: DownloadedScanQuery) => scanDomain(domain) && validScanPage(query)
  ? request<ApiPage<DownloadedScanCommand>>(`${scanRoot(domain)}${scanPage(query, { status: query?.status })}`, undefined, undefined, true)
  : scanError<ApiPage<DownloadedScanCommand>>('Invalid scan list request.');
export const getDownloadedScanCommand = (domain: MediaDomain, id: string) => scanDomain(domain) && scanId(id)
  ? request<DownloadedScanCommand>(`${scanRoot(domain)}/${id}`, undefined, undefined, true)
  : scanError<DownloadedScanCommand>('Invalid command ID.');
export const listDownloadedScanChildren = (domain: MediaDomain, id: string, query?: DownloadedScanChildQuery) => scanDomain(domain) && scanId(id) && validScanPage(query)
  ? request<ApiPage<ScanChild>>(`${scanRoot(domain)}/${id}/children${scanPage(query, { disposition: query?.disposition })}`, undefined, undefined, true)
  : scanError<ApiPage<ScanChild>>('Invalid scan children request.');
export const cancelDownloadedScanCommand = (domain: MediaDomain, id: string) => scanDomain(domain) && scanId(id)
  ? request<DownloadedScanCommand>(`${scanRoot(domain)}/${id}/cancel`, undefined, 'POST', true)
  : scanError<DownloadedScanCommand>('Invalid command ID.');
// Explicit recovery of one failed child; the body names the exact operation and the generation
// last read. A lost response is reconciled by re-reading or repeating the identical body.
export const resumeDownloadedScanChild = (domain: MediaDomain, id: string, childId: string, input: ScanChildResumeInput) =>
  scanDomain(domain) && scanId(id) && scanId(childId) && scanId(input.operation_id)
    && (input.inventory_revision === 0 || input.inventory_revision === 1)
    && Number.isSafeInteger(input.expected_resume_generation) && input.expected_resume_generation >= 0
    ? request<ScanChild>(`${scanRoot(domain)}/${id}/children/${childId}/resume`, input, 'POST', true)
    : scanError<ScanChild>('Invalid scan child resume request.');
// Complete paginated TV membership of one child's physical file (movie children have none).
export const listDownloadedScanMembers = (domain: MediaDomain, id: string, childId: string, query?: ScanMemberQuery) =>
  scanDomain(domain) && scanId(id) && scanId(childId) && validScanPage(query)
    ? request<ApiPage<ScanChildMember>>(`${scanRoot(domain)}/${id}/children/${childId}/members${scanPage(query, {})}`, undefined, undefined, true)
    : scanError<ApiPage<ScanChildMember>>('Invalid scan membership request.');
// Explicit recovery of an uncertain source-folder cleanup; the body names the generation last
// read. A lost response is reconciled by re-reading or repeating the identical body.
export const resumeDownloadedScanCleanup = (domain: MediaDomain, id: string, input: ScanCleanupResumeInput) =>
  scanDomain(domain) && scanId(id) && (input.inventory_revision === 0 || input.inventory_revision === 1)
    && Number.isSafeInteger(input.expected_cleanup_resume_generation) && input.expected_cleanup_resume_generation >= 0
    ? request<DownloadedScanCommand>(`${scanRoot(domain)}/${id}/resume-cleanup`, input, 'POST', true)
    : scanError<DownloadedScanCommand>('Invalid scan cleanup resume request.');

const invalidProvider = <T>(): Promise<Result<T>> => Promise.resolve({ok:false,error:'Invalid provider ID or revision.'});
export const getProvider = (id: string) => validOperation(id) ? request<Provider>(`/api/v1/providers/${id}`, undefined, undefined, true) : invalidProvider<Provider>();
export const createProvider = (input: ProviderInput) => request<Provider>('/api/v1/providers', input, 'POST', true);
export const updateProvider = (id: string, input: ProviderUpdate) => validOperation(id) && validId(input.revision) ? request<Provider>(`/api/v1/providers/${id}`, input, 'PUT', true) : invalidProvider<Provider>();
export const deleteProvider = (id: string, revision: number) => validOperation(id) && validId(revision) ? request<void>(`/api/v1/providers/${id}?revision=${revision}`, undefined, 'DELETE', true) : invalidProvider<void>();
export const testProvider = (id: string) => validOperation(id) ? request<ProviderTestResult>(`/api/v1/providers/${id}/test`, undefined, 'POST', true, 35_000) : invalidProvider<ProviderTestResult>();

export const listCommands = (query: CommandQuery = {}) => {
  const params = new URLSearchParams({limit:String(query.limit ?? 25),offset:String(query.offset ?? 0)});
  if (query.media_type) params.set('media_type', query.media_type);
  if (query.status) params.set('status', query.status);
  return request<ApiPage<Command>>(`/api/v1/commands?${params}`, undefined, undefined, true);
};
export const getCommand = (id: string) => validOperation(id) ? request<Command>(`/api/v1/commands/${id}`, undefined, undefined, true) : invalidProvider<Command>();
export const createCommand = (input: CommandInput) => validOperation(input.target.provider_id) && validId(input.provider_revision) ? request<Command>('/api/v1/commands', input, 'POST', true) : invalidProvider<Command>();
export const cancelCommand = (id: string) => validOperation(id) ? request<Command>(`/api/v1/commands/${id}/cancel`, undefined, 'POST', true) : invalidProvider<Command>();
export const deleteCommand = (id: string) => validOperation(id) ? request<void>(`/api/v1/commands/${id}`, undefined, 'DELETE', true) : invalidProvider<void>();
export const listRefreshSchedules = () => request<RefreshSchedule[]>('/api/v1/download-refresh/schedules', undefined, undefined, true);
export const saveRefreshSchedule = (input: RefreshScheduleInput) => validOperation(input.target.provider_id) && validId(input.provider_revision) && (input.revision === null || validId(input.revision)) ? request<RefreshSchedule>('/api/v1/download-refresh/schedules', input, 'PUT', true) : invalidProvider<RefreshSchedule>();
export const deleteRefreshSchedule = (input: RefreshScheduleDelete) => validOperation(input.target.provider_id) && validId(input.revision) ? request<void>('/api/v1/download-refresh/schedules', input, 'DELETE', true) : invalidProvider<void>();
export const getQueueSnapshot = (target: RefreshTarget, offset = 0) => validOperation(target.provider_id) ? request<QueueSnapshot>(`/api/v1/queue?${new URLSearchParams({provider_id:target.provider_id,media_type:target.media_type,limit:'25',offset:String(offset)})}`, undefined, undefined, true) : invalidProvider<QueueSnapshot>();

const validMetadataTarget = (target: MetadataRefreshTarget) => validId(target.media_type === 'tv' ? target.series_id : target.movie_id);
export const listMetadataCommands = (query: MetadataCommandQuery = {}) => {
  const params = new URLSearchParams({limit:String(query.limit ?? 25),offset:String(query.offset ?? 0)});
  if (query.media_type) params.set('media_type',query.media_type);
  if (query.series_id !== undefined) {if (!validId(query.series_id)) return invalidProvider<ApiPage<MetadataCommand>>(); params.set('series_id',String(query.series_id));}
  if (query.movie_id !== undefined) {if (!validId(query.movie_id)) return invalidProvider<ApiPage<MetadataCommand>>(); params.set('movie_id',String(query.movie_id));}
  if (query.status) params.set('status',query.status);
  return request<ApiPage<MetadataCommand>>(`/api/v1/metadata-refresh/commands?${params}`,undefined,undefined,true);
};
export const createMetadataCommand = (input: MetadataCommandInput) => validMetadataTarget(input.target) ? request<MetadataCommand>('/api/v1/metadata-refresh/commands',input,'POST',true) : invalidProvider<MetadataCommand>();
export const getMetadataCommand = (id: string) => validOperation(id) ? request<MetadataCommand>(`/api/v1/metadata-refresh/commands/${id}`,undefined,undefined,true) : invalidProvider<MetadataCommand>();
export const cancelMetadataCommand = (id: string) => validOperation(id) ? request<MetadataCommand>(`/api/v1/metadata-refresh/commands/${id}/cancel`,undefined,'POST',true) : invalidProvider<MetadataCommand>();
export const deleteMetadataCommand = (id: string) => validOperation(id) ? request<void>(`/api/v1/metadata-refresh/commands/${id}`,undefined,'DELETE',true) : invalidProvider<void>();

const validBlocklistIdentity = (id:BlocklistIdentity) => ['sonarr','radarr'].includes(id.application) && /^[a-f0-9]{64}$/.test(id.fingerprint) && validId(id.source_id);
const invalidBlocklist = <T>(): Promise<Result<T>> => Promise.resolve({ok:false,error:'Invalid blocklist identity or selection.'});
export const listBlocklist = (query:BlocklistQuery = {}) => {
  const params=new URLSearchParams({limit:String(query.limit??25),offset:String(query.offset??0)});
  for(const field of ['media_type','series_ids','movie_ids','protocols','sort','sort_direction'] as const) if(query[field]!==undefined) params.set(field,String(query[field]));
  return request<ApiPage<BlocklistEntry>>(`/api/v1/blocklist?${params}`,undefined,undefined,true);
};
export const deleteBlocklistEntry = (id:BlocklistIdentity) => validBlocklistIdentity(id) ? request<void>(`/api/v1/blocklist/${id.application}/${id.fingerprint}/${id.source_id}`,undefined,'DELETE',true) : invalidBlocklist<void>();
export const deleteBlocklistEntries = (input:BlocklistRemoval) => input.ids.length>0 && input.ids.length<=100 && input.ids.every(validBlocklistIdentity) && new Set(input.ids.map(id=>`${id.application}:${id.fingerprint}:${id.source_id}`)).size===input.ids.length ? request<void>('/api/v1/blocklist/bulk',input,'DELETE',true) : invalidBlocklist<void>();

export const listBlocklistClearCommands = (query:CommandQuery = {}) => {
  const params=new URLSearchParams({limit:String(query.limit??25),offset:String(query.offset??0)});
  if(query.media_type)params.set('media_type',query.media_type);
  if(query.status)params.set('status',query.status);
  return request<ApiPage<BlocklistClearCommand>>(`/api/v1/blocklist/clear-commands?${params}`,undefined,undefined,true);
};
export const createBlocklistClearCommand = (input:BlocklistClearInput) => request<BlocklistClearCommand>('/api/v1/blocklist/clear-commands',input,'POST',true);
export const getBlocklistClearCommand = (id:string) => validOperation(id)?request<BlocklistClearCommand>(`/api/v1/blocklist/clear-commands/${id}`,undefined,undefined,true):invalidBlocklist<BlocklistClearCommand>();
export const cancelBlocklistClearCommand = (id:string) => validOperation(id)?request<BlocklistClearCommand>(`/api/v1/blocklist/clear-commands/${id}/cancel`,undefined,'POST',true):invalidBlocklist<BlocklistClearCommand>();
export const deleteBlocklistClearCommand = (id:string) => validOperation(id)?request<void>(`/api/v1/blocklist/clear-commands/${id}`,undefined,'DELETE',true):invalidBlocklist<void>();

export const listQualityProfiles = (domain: MediaDomain, offset = 0) => request<QualityProfilePage>(`/api/v1/${domain}/quality-profiles?limit=50&offset=${offset}`, undefined, undefined, true);

export const getReleasePolicy = (domain: MediaDomain) => request<ReleasePolicy | null>(`/api/v1/release-policies/${domain}`, undefined, undefined, true);
export const saveReleasePolicy = (domain: MediaDomain, input: ReleasePolicy) => request<ReleasePolicy>(`/api/v1/release-policies/${domain}`, input, 'PUT', true);
export const searchReleases = (input: ReleaseSearchInput) => validId(input.target.id) && validOperation(input.provider_id) && validId(input.provider_revision) ? request<ReleaseSearchPage>('/api/v1/release-search', input, 'POST', true, 45_000) : invalidProvider<ReleaseSearchPage>();

function validClientSelection(input:Pick<RssTargetInput,'client_selection'|'client_id'|'client_revision'>):boolean {
  const selection=input.client_selection;
  if(selection===undefined)return validOperation(input.client_id??'')&&validId(input.client_revision??0);
  if(!selection||input.client_id!==undefined||input.client_revision!==undefined)return false;
  return selection.kind==='default'||(selection.kind==='explicit'&&validOperation(selection.provider_id)&&validId(selection.provider_revision));
}
const validRssTarget = (target:RssTargetInput) => validOperation(target.indexer_id) && validId(target.indexer_revision) && validClientSelection(target);
export const listRssCommands = (domain:MediaDomain, offset=0) => request<ApiPage<RssCommand>>(`/api/v1/rss/commands?media_type=${domain}&limit=25&offset=${offset}`,undefined,undefined,true);
export const createRssCommand = (input:RssInput) => validRssTarget(input.target)?request<RssCommand>('/api/v1/rss/commands',input,'POST',true):invalidProvider<RssCommand>();
export const cancelRssCommand = (id:string) => validOperation(id)?request<RssCommand>(`/api/v1/rss/commands/${id}/cancel`,undefined,'POST',true):invalidProvider<RssCommand>();
export const listRssCandidates = (domain:MediaDomain,offset=0) => request<ApiPage<RssCandidate>>(`/api/v1/rss/candidates?media_type=${domain}&limit=25&offset=${offset}`,undefined,undefined,true);
export const listRssSchedules = () => request<RssSchedule[]>('/api/v1/rss/schedules',undefined,undefined,true);
export const saveRssSchedule = (input:RssScheduleInput) => validRssTarget(input.target)?request<RssSchedule>('/api/v1/rss/schedules',input,'POST',true):invalidProvider<RssSchedule>();

export const getProcessingPolicy = (provider:string,domain:MediaDomain) => validOperation(provider)?request<ProcessingPolicy>(`/api/v1/download-processing/policies/${provider}/${domain}`,undefined,undefined,true):invalidProvider<ProcessingPolicy>();
export const saveProcessingPolicy = (provider:string,domain:MediaDomain,input:ProcessingPolicyInput) => validOperation(provider)&&validId(input.provider_revision)?request<ProcessingPolicy>(`/api/v1/download-processing/policies/${provider}/${domain}`,input,'PUT',true):invalidProvider<ProcessingPolicy>();
export const listDownloadProcessing = (provider:string,domain:MediaDomain,offset=0) => validOperation(provider)?request<ApiPage<DownloadProcessing>>(`/api/v1/download-processing?provider_id=${provider}&media_type=${domain}&limit=25&offset=${offset}`,undefined,undefined,true):invalidProvider<ApiPage<DownloadProcessing>>();
export const processDownloads = (input:ProcessingInput) => validOperation(input.provider_id)&&validId(input.provider_revision)&&input.receipt_ids.length>0&&input.receipt_ids.length<=100&&input.receipt_ids.every(validOperation)?request<DownloadProcessing[]>('/api/v1/download-processing',input,'POST',true):invalidProvider<DownloadProcessing[]>();
export const cancelDownloadProcessing = (receipt:string) => validOperation(receipt)?request<DownloadProcessing>(`/api/v1/download-processing/${receipt}/cancel`,undefined,'POST',true):invalidProvider<DownloadProcessing>();

export const getTvNaming = () => request<TvNamingConfig>('/api/v1/tv/config/naming', undefined, undefined, true);
export const updateTvNaming = (input: TvNamingUpdate) => validId(input.revision) ? request<TvNamingConfig>('/api/v1/tv/config/naming', input, 'PUT', true) : invalidProvider<TvNamingConfig>();
export const getTvNamingExamples = (query: TvNamingExamplesQuery = {}) => {
  const params = new URLSearchParams();
  for (const field of ['rename_enabled','replace_illegal_characters','colon_replacement','custom_colon_replacement','standard_episode_format','daily_episode_format','anime_episode_format','series_folder_format','season_folder_format','specials_folder_format','multi_episode_style','multi_episode_draft'] as const) {
    if (query[field] != null) params.set(field, String(query[field]));
  }
  const qs = params.toString();
  return request<TvNamingExamples>(`/api/v1/tv/config/naming/examples${qs ? `?${qs}` : ''}`, undefined, undefined, true);
};
export const getMovieNaming = () => request<MovieNamingConfig>('/api/v1/movies/config/naming', undefined, undefined, true);
export const updateMovieNaming = (input: MovieNamingUpdate) => validId(input.revision) ? request<MovieNamingConfig>('/api/v1/movies/config/naming', input, 'PUT', true) : invalidProvider<MovieNamingConfig>();
export const getMovieNamingExamples = (query: MovieNamingExamplesQuery = {}) => {
  const params = new URLSearchParams();
  for (const field of ['rename_enabled','replace_illegal_characters','colon_replacement','custom_colon_replacement','standard_movie_format','movie_folder_format'] as const) {
    if (query[field] != null) params.set(field, String(query[field]));
  }
  const qs = params.toString();
  return request<MovieNamingExamples>(`/api/v1/movies/config/naming/examples${qs ? `?${qs}` : ''}`, undefined, undefined, true);
};

export const listSearchCommands = (target:MediaTarget,offset=0) => validId(target.id) ? request<ApiPage<SearchCommand>>(`/api/v1/search/commands?target_type=${target.media_type}&target_id=${target.id}&limit=25&offset=${offset}`,undefined,undefined,true) : invalid<ApiPage<SearchCommand>>();
export const getSearchCommand = (id:string) => validOperation(id) ? request<SearchCommand>(`/api/v1/search/commands/${id}`,undefined,undefined,true) : invalidProvider<SearchCommand>();
export const createSearchCommand = (input:SearchCommandInput) => validOperation(input.request_id)&&validId(input.target.id)&&validOperation(input.indexer_id)&&validId(input.indexer_revision)&&validClientSelection(input) ? request<SearchCommand>('/api/v1/search/commands',input,'POST',true) : invalidProvider<SearchCommand>();
export const cancelSearchCommand = (id:string) => validOperation(id) ? request<SearchCommand>(`/api/v1/search/commands/${id}/cancel`,{},'POST',true) : invalidProvider<SearchCommand>();
export const listSearchResults = (id:string,offset=0) => validOperation(id) ? request<ApiPage<SearchResult>>(`/api/v1/search/commands/${id}/results?limit=25&offset=${offset}`,undefined,undefined,true) : invalidProvider<ApiPage<SearchResult>>();
export const grabSearchResult = (id:string) => validOperation(id) ? request<RssCandidate>(`/api/v1/search/results/${id}/grab`,{},'POST',true) : invalidProvider<RssCandidate>();
export const deleteSearchCommand = (id:string) => validOperation(id) ? request<void>(`/api/v1/search/commands/${id}`,undefined,'DELETE',true) : invalidProvider<void>();

// Existing-library import wizard: point at a root folder, list its unmapped subfolders, match
// each to a metadata lookup result and create it (there is no bulk-create endpoint; each folder
// is one addLibrary call), then adopt on-disk files via a rescan command.
export const listRootFolders = (domain: MediaDomain, offset = 0) => request<ApiPage<RootFolder>>(`/api/v1/${domain}/root-folders?limit=50&offset=${offset}`, undefined, undefined, true);
export const getRootFolder = (domain: MediaDomain, id: number) => validId(id) ? request<RootFolder>(`/api/v1/${domain}/root-folders/${id}`, undefined, undefined, true) : invalid<RootFolder>();
export const createRootFolder = (domain: MediaDomain, path: string) => path.trim().length > 0 && path.length <= 4096 ? request<RootFolder>(`/api/v1/${domain}/root-folders`, { path }, 'POST', true) : Promise.resolve<Result<RootFolder>>({ ok: false, error: 'Root folder path is required.' });
export const createRescanCommand = (domain: MediaDomain, targetId: number, priority: CommandPriority = 'normal') => validId(targetId) ? request<RescanBatch>(`/api/v1/${domain}/rescan-commands`, { target_id: targetId, priority }, 'POST', true) : invalid<RescanBatch>();
export const getRescanCommand = (domain: MediaDomain, id: string) => validOperation(id) ? request<RescanCommand>(`/api/v1/${domain}/rescan-commands/${id}`, undefined, undefined, true) : invalidProvider<RescanCommand>();

// Custom formats remain scoped to the selected library domain.
export const listCustomFormats = (domain: MediaDomain) => request<import('./api.generated').CustomFormat[]>(`/api/v1/${domain}/custom-formats`, undefined, undefined, true);
export const getCustomFormatSchema = (domain: MediaDomain) => request<import('./api.generated').CustomFormatSchema>(`/api/v1/${domain}/custom-formats/schema`, undefined, undefined, true);
export const createCustomFormat = (domain: MediaDomain, input: import('./api.generated').CustomFormatInput) => request<import('./api.generated').CustomFormat>(`/api/v1/${domain}/custom-formats`, input, 'POST', true);
export const updateCustomFormat = (domain: MediaDomain, id: number, input: import('./api.generated').CustomFormatInput) => validId(id) ? request<import('./api.generated').CustomFormat>(`/api/v1/${domain}/custom-formats/${id}`, input, 'PUT', true) : invalid<import('./api.generated').CustomFormat>();
export const deleteCustomFormat = (domain: MediaDomain, id: number) => validId(id) ? request<void>(`/api/v1/${domain}/custom-formats/${id}`, undefined, 'DELETE', true) : invalid<void>();
export const getQualityProfile = (domain: MediaDomain, id: number) => validId(id) ? request<QualityProfile>(`/api/v1/${domain}/quality-profiles/${id}`, undefined, undefined, true) : invalid<QualityProfile>();
export const getQualityProfileSchema = (domain: MediaDomain) => request<QualityProfileInput>(`/api/v1/${domain}/quality-profiles/schema`, undefined, undefined, true);
export const listQualityDefinitions = (domain: MediaDomain) => request<QualityDefinition[]>(`/api/v1/${domain}/quality-definitions`, undefined, undefined, true);
export const createQualityProfile = (domain: MediaDomain, input: QualityProfileInput) => request<QualityProfile>(`/api/v1/${domain}/quality-profiles`, input, 'POST', true);
export const updateQualityProfile = (domain: MediaDomain, id: number, input: QualityProfileInput) => validId(id) ? request<QualityProfile>(`/api/v1/${domain}/quality-profiles/${id}`, input, 'PUT', true) : invalid<QualityProfile>();
export const deleteQualityProfile = (domain: MediaDomain, id: number) => validId(id) ? request<void>(`/api/v1/${domain}/quality-profiles/${id}`, undefined, 'DELETE', true) : invalid<void>();

export const getQualityDefinitionLimits = (domain: MediaDomain) => request<import('./api.generated').QualityDefinitionLimits>(`/api/v1/${domain}/quality-definitions/limits`, undefined, undefined, true);
export const getQualityDefinitionDefaults = (domain: MediaDomain) => request<QualityDefinition[]>(`/api/v1/${domain}/quality-definitions/defaults`, undefined, undefined, true);
export const updateQualityDefinitions = (domain: MediaDomain, input: import('./api.generated').QualityDefinitionUpdate[]) => request<QualityDefinition[]>(`/api/v1/${domain}/quality-definitions/bulk`, input, 'PUT', true);
export const resetQualityDefinitions = (domain: MediaDomain, resetTitles: boolean) => request<QualityDefinition[]>(`/api/v1/${domain}/quality-definitions/reset`, { reset_titles: resetTitles }, 'POST', true);

const tagPath = (domain: MediaDomain) => `/api/v1/${domain}/tags`;
export const listTags = (domain: MediaDomain) => request<Tag[]>(tagPath(domain), undefined, undefined, true);
export const listTagDetails = (domain: MediaDomain) => request<TagDetail[]>(`${tagPath(domain)}/detail`, undefined, undefined, true);
export const createTag = (domain: MediaDomain, label: string) => request<Tag>(tagPath(domain), {label}, 'POST', true);
export const renameTag = (domain: MediaDomain, id: number, label: string) => validId(id) ? request<Tag>(`${tagPath(domain)}/${id}`, {label}, 'PUT', true) : invalid<Tag>();
export const deleteTag = (domain: MediaDomain, id: number) => validId(id) ? request<void>(`${tagPath(domain)}/${id}`, undefined, 'DELETE', true) : invalid<void>();
export const getTagOwners = (domain: MediaDomain, id: number, offset = 0) => validId(id) && Number.isSafeInteger(offset) && offset >= 0 ? request<TagOwners>(`${tagPath(domain)}/${id}/owners?limit=25&offset=${offset}`, undefined, undefined, true) : invalid<TagOwners>();
export const assignLibraryTags = (domain: MediaDomain, ids: number[], tags: TagAssignment) => ids.length > 0 && ids.length <= 100 && ids.every(validId) && tags.ids.length <= 200 && tags.ids.every(validId) ? request<LibraryItem[]>(`${collection(domain)}/editor`, {ids,patch:{tags}}, 'PUT', true) : invalid<LibraryItem[]>();

const delayPath = (domain: MediaDomain) => `/api/v1/${domain}/delay-profiles`;
export const listDelayProfiles = (domain: MediaDomain) => request<import('./api.generated').DelayProfileCatalog>(delayPath(domain), undefined, undefined, true);
export const getDelayProfileSchema = (domain: MediaDomain) => request<import('./api.generated').DelayProfileInput>(`${delayPath(domain)}/schema`, undefined, undefined, true);
export const createDelayProfile = (domain: MediaDomain, input: import('./api.generated').DelayProfileWrite) => request<import('./api.generated').DelayProfileCatalog>(delayPath(domain), input, 'POST', true);
export const updateDelayProfile = (domain: MediaDomain, id: number, input: import('./api.generated').DelayProfileWrite) => validId(id) ? request<import('./api.generated').DelayProfileCatalog>(`${delayPath(domain)}/${id}`, input, 'PUT', true) : invalid<import('./api.generated').DelayProfileCatalog>();
export const deleteDelayProfile = (domain: MediaDomain, id: number, revision: number) => validId(id)&&validId(revision) ? request<import('./api.generated').DelayProfileCatalog>(`${delayPath(domain)}/${id}?revision=${revision}`, undefined, 'DELETE', true) : invalid<import('./api.generated').DelayProfileCatalog>();
export const reorderDelayProfiles = (domain: MediaDomain, input: import('./api.generated').DelayProfileReorder) => request<import('./api.generated').DelayProfileCatalog>(`${delayPath(domain)}/reorder`, input, 'PUT', true);
export const getImportPolicy = (domain: MediaDomain) => request<import('./api.generated').ImportPolicy>(`/api/v1/${domain}/import-policy`, undefined, undefined, true);
export const updateImportPolicy = (domain: MediaDomain, input: import('./api.generated').ImportPolicyUpdate) => request<import('./api.generated').ImportPolicy>(`/api/v1/${domain}/import-policy`, input, 'PUT', true);

export const getRevisionPolicy = (domain: MediaDomain) => request<import('./api.generated').RevisionPolicy>(`/api/v1/${domain}/revision-policy`, undefined, undefined, true);
export const updateRevisionPolicy = (domain: MediaDomain, input: import('./api.generated').RevisionPolicyUpdate) => request<import('./api.generated').RevisionPolicy>(`/api/v1/${domain}/revision-policy`, input, 'PUT', true);

const releaseProfilePath=(domain:MediaDomain)=>`/api/v1/${domain}/release-profiles`;
export const listReleaseProfiles=(domain:MediaDomain)=>request<import('./api.generated').ReleaseProfileCatalog>(releaseProfilePath(domain),undefined,undefined,true);
export const getReleaseProfileSchema=(domain:MediaDomain)=>request<import('./release-profile-draft').Input>(`${releaseProfilePath(domain)}/schema`,undefined,undefined,true);
export const createReleaseProfile=(domain:MediaDomain,input:import('./api.generated').ReleaseProfileWrite<import('./release-profile-draft').Input>)=>validId(input.revision)&&input.profile.indexers.every(r=>r.kind==='provider')?request<import('./api.generated').ReleaseProfileCatalog>(releaseProfilePath(domain),input,'POST',true):invalid<import('./api.generated').ReleaseProfileCatalog>();
export const updateReleaseProfile=(domain:MediaDomain,id:number,input:import('./api.generated').ReleaseProfileWrite<import('./release-profile-draft').Input>)=>validId(id)&&validId(input.revision)?request<import('./api.generated').ReleaseProfileCatalog>(`${releaseProfilePath(domain)}/${id}`,input,'PUT',true):invalid<import('./api.generated').ReleaseProfileCatalog>();
export const deleteReleaseProfile=(domain:MediaDomain,id:number,revision:number)=>validId(id)&&validId(revision)?request<import('./api.generated').ReleaseProfileCatalog>(`${releaseProfilePath(domain)}/${id}?revision=${revision}`,undefined,'DELETE',true):invalid<import('./api.generated').ReleaseProfileCatalog>();

// One readback boundary for settings which reconcile policies and schedules together.
export const getCompletedDownloadHandling = (media: MediaDomain) => request<import('./api.generated').CompletedDownloadHandling>(`/api/v1/${media}/completed-download-handling`,undefined,undefined,true);
export const saveCompletedDownloadHandling = (media: MediaDomain, input: import('./api.generated').CompletedDownloadHandlingUpdate) => validId(input.revision) ? request<import('./api.generated').CompletedDownloadHandling>(`/api/v1/${media}/completed-download-handling`,input,'PUT',true) : invalidProvider<import('./api.generated').CompletedDownloadHandling>();
export const inheritProcessingPolicy = (provider: string, media: MediaDomain, input: import('./api.generated').ProcessingPolicyReset) => validOperation(provider)&&validId(input.provider_revision)&&(input.revision===null||validId(input.revision)) ? request<ProcessingPolicy>(`/api/v1/download-processing/policies/${provider}/${media}/inherit`,input,'PUT',true) : invalidProvider<ProcessingPolicy>();
export const inheritRefreshSchedule = (input: import('./api.generated').RefreshScheduleReset) => validOperation(input.target.provider_id)&&validId(input.provider_revision)&&(input.revision===null||validId(input.revision)) ? request<RefreshSchedule>('/api/v1/download-refresh/schedules/inherit',input,'PUT',true) : invalidProvider<RefreshSchedule>();
export async function readDownloadHandling(providerId?: string): Promise<Result<{settings:Record<MediaDomain,import('./api.generated').CompletedDownloadHandling>;schedules:RefreshSchedule[];provider:Provider|null;policies:Partial<Record<MediaDomain,ProcessingPolicy>>}>> {
  const [tv,movies,schedules] = await Promise.all([getCompletedDownloadHandling('tv'),getCompletedDownloadHandling('movies'),listRefreshSchedules()]);
  if(!tv.ok)return tv;if(!movies.ok)return movies;if(!schedules.ok)return schedules;
  let provider:Provider|null=null;
  const policies:Partial<Record<MediaDomain,ProcessingPolicy>>={};
  if(providerId){
    const saved=await getProvider(providerId);
    if(!saved.ok && saved.status!==404)return saved;
    if(saved.ok){
      provider=saved.data;
      if(provider.settings.implementation==='qbittorrent')for(const media of ['tv','movies'] as const){
        if(!provider.settings[media])continue;
        const policy=await getProcessingPolicy(providerId,media);
        if(!policy.ok)return policy;
        policies[media]=policy.data;
      }
    }
  }
  return {ok:true,data:{settings:{tv:tv.data,movies:movies.data},schedules:schedules.data,provider,policies}};
}

// Native health is scoped by registry identity, never by a synthetic media ID.
import type { HealthScope, HealthSnapshot, HealthAdmission, HealthCommand, HealthTransitions, CommandStatus } from './api.generated';
const healthScope = (scope:HealthScope) => ['all','tv','movies','system'].includes(scope);
const healthOffset = (value:number,max:number) => Number.isSafeInteger(value)&&value>=0&&value<=max;
const invalidHealth = <T>():Promise<Result<T>> => Promise.resolve({ok:false,error:'Invalid health scope, identifier or pagination.'});
export const getHealth = (scope:HealthScope='all',offset=0) => healthScope(scope)&&healthOffset(offset,127)?request<HealthSnapshot>(`/api/v1/health?${new URLSearchParams({scope,limit:'16',offset:String(offset)})}`,undefined,undefined,true):invalidHealth<HealthSnapshot>();
export type HealthRequestObservation = {epoch:string;checks:HealthSnapshot['checks']};
const healthIdentity = (value:HealthSnapshot['coverage']['identities'][number]) => `${value.scope}:${value.check_key}`;
export async function getHealthRequestObservation(first?:HealthSnapshot):Promise<Result<HealthRequestObservation>> {
  const initial=first?{ok:true as const,data:first}:await getHealth();
  if(!initial.ok)return initial;
  const start=initial.data;
  const incomplete=()=>({ok:false as const,error:'Requested check observations are incomplete or changed while reading; refresh again.'});
  if(!Number.isInteger(start.total)||start.total<0||start.total>128||start.offset!==0||start.limit!==16||!Array.isArray(start.checks)||!Array.isArray(start.coverage?.identities)||typeof start.lifecycle?.epoch!=='string')return incomplete();
  const registry=start.coverage.identities.map(healthIdentity);
  if(registry.length!==start.total||new Set(registry).size!==registry.length)return incomplete();
  const pages=await Promise.all(Array.from({length:Math.max(0,Math.ceil(start.total/16)-1)},(_,n)=>getHealth('all',(n+1)*16)));
  const checks=[...start.checks];
  for(const [index,page] of pages.entries()){
    if(!page.ok)return page;
    const value=page.data;
    if(!Array.isArray(value.checks)||!Array.isArray(value.coverage?.identities)||value.lifecycle?.epoch!==start.lifecycle.epoch||value.total!==start.total||value.offset!==(index+1)*16||value.limit!==16||JSON.stringify(value.coverage.identities.map(healthIdentity))!==JSON.stringify(registry))return incomplete();
    checks.push(...value.checks);
  }
  if(checks.length!==registry.length||checks.some((check,index)=>healthIdentity(check.identity)!==registry[index]))return incomplete();
  return {ok:true,data:{epoch:start.lifecycle.epoch,checks}};
}
export function healthRequestObserved(admission:HealthAdmission,observation:HealthRequestObservation|null,epoch:string):boolean {
  return !!observation&&observation.epoch===epoch&&admission.selection.every(token=>observation.checks.some(check=>healthIdentity(check.identity)===healthIdentity(token.identity)&&check.observed_generation!==null&&check.observed_generation>=token.generation&&check.observed_epoch===epoch&&check.evaluation==='current'&&!check.last_error));
}
export const createHealthCommand = (scope:HealthScope) => healthScope(scope)?request<HealthAdmission>('/api/v1/health/commands',{scope,priority:'normal'},'POST',true):invalidHealth<HealthAdmission>();
export const listHealthCommands = (scope:HealthScope='all',offset=0,status:CommandStatus|''='') => {
  if(!healthScope(scope)||!healthOffset(offset,128)||!['','queued','running','retry_wait','succeeded','failed','cancelled'].includes(status))return invalidHealth<ApiPage<HealthCommand>>();
  const params=new URLSearchParams({scope,limit:'20',offset:String(offset)});if(status)params.set('status',status);
  return request<ApiPage<HealthCommand>>(`/api/v1/health/commands?${params}`,undefined,undefined,true);
};
export const getHealthCommand = (id:string) => validOperation(id)?request<HealthCommand>(`/api/v1/health/commands/${id}`,undefined,undefined,true):invalidHealth<HealthCommand>();
export const cancelHealthCommand = (id:string) => validOperation(id)?request<HealthCommand>(`/api/v1/health/commands/${id}/cancel`,undefined,'POST',true):invalidHealth<HealthCommand>();
export const getHealthTransitions = (after=0) => healthOffset(after,Number.MAX_SAFE_INTEGER)?request<HealthTransitions>(`/api/v1/health/transitions?${new URLSearchParams({after_sequence:String(after),limit:'16'})}`,undefined,undefined,true):invalidHealth<HealthTransitions>();

export const getMovieRenamePreview = (movieIds: number[]): Promise<Result<import('./api.generated').MovieRenamePreview>> => {
  if (!Array.isArray(movieIds) || movieIds.length === 0 || movieIds.length > 200 || new Set(movieIds).size !== movieIds.length || movieIds.some(id => !Number.isSafeInteger(id) || id <= 0)) {
    return Promise.resolve({ ok: false, error: 'Select up to 200 distinct movies with positive safe IDs.' });
  }
  const query = new URLSearchParams({ movie_ids: [...movieIds].sort((a, b) => a - b).join(',') });
  return request<import('./api.generated').MovieRenamePreview>(`/api/v1/movies/rename-preview?${query}`, undefined, undefined, true);
};

export const getHistory = (query: import('./api.generated').HistoryQuery = {}): Promise<Result<ApiPage<import('./api.generated').HistoryEvent>>> => {
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(query)) if (value !== undefined) params.set(key, String(value));
  const suffix = params.size ? `?${params}` : '';
  return request<ApiPage<import('./api.generated').HistoryEvent>>(`/api/v1/history${suffix}`, undefined, undefined, true);
};
export const getHostSettings =(): Promise<Result<import('./api.generated').HostSettings>> =>
  request<import('./api.generated').HostSettings>('/api/v1/config/host', undefined, undefined, true);

// File IDs are scoped by domain; owner-qualified readback is required after uncertain writes.
import type { FilePatch, FileResource } from './api.generated';
import { FILE_PAGE_SIZE, validFileIds, validFileResource, validateFileReadback } from './file-metadata-draft.ts';
const fileDomain = (domain: MediaDomain) => domain === 'tv' || domain === 'movies';
const invalidFiles = <T>(error = 'Invalid file request.'): Result<T> => ({ ok: false, error });
const filePageValid = (value: unknown): value is ApiPage<FileResource> => {
  if (!value || typeof value !== 'object') return false;
  const page = value as ApiPage<FileResource>;
  return Array.isArray(page.items) && page.items.length <= FILE_PAGE_SIZE && Number.isSafeInteger(page.total) && page.total >= 0 && page.limit === FILE_PAGE_SIZE && Number.isSafeInteger(page.offset) && page.offset >= 0 && page.offset <= 4294967295 && page.items.length === Math.min(FILE_PAGE_SIZE, Math.max(0, page.total - page.offset));
};
export async function listMediaFiles(domain: MediaDomain, ownerId: number, offset = 0): Promise<Result<ApiPage<FileResource>>> {
  if (!fileDomain(domain) || !validId(ownerId) || !Number.isInteger(offset) || offset < 0 || offset > 4294967295) return invalidFiles();
  const query = new URLSearchParams({ [domain === 'tv' ? 'series_id' : 'movie_ids']: String(ownerId), limit: String(FILE_PAGE_SIZE), offset: String(offset) });
  const result = await request<ApiPage<FileResource>>(`/api/v1/${domain}/files?${query}`, undefined, undefined, true);
  if (!result.ok) return result;
  if (!filePageValid(result.data) || result.data.offset !== offset) return invalidFiles('Invalid file page response.');
  if (result.data.items.length) {
    const checked = validateFileReadback(domain, ownerId, result.data.items.map(file => file?.id), result.data.items);
    if (!checked.ok) return checked;
  }
  return result;
}
export async function getMediaFile(domain: MediaDomain, fileId: number): Promise<Result<FileResource>> {
  if (!fileDomain(domain) || !validId(fileId)) return invalidFiles();
  const result = await request<FileResource>(`/api/v1/${domain}/files/${fileId}`, undefined, undefined, true);
  return !result.ok || (validFileResource(domain, result.data) && result.data.id === fileId) ? result : invalidFiles('Invalid file detail response.');
}
export async function updateMediaFile(domain: MediaDomain, fileId: number, patch: FilePatch): Promise<Result<FileResource>> {
  if (!fileDomain(domain) || !validId(fileId) || !patch || !Object.keys(patch).length) return invalidFiles();
  const result = await request<FileResource>(`/api/v1/${domain}/files/${fileId}`, patch, 'PUT', true);
  return !result.ok || (validFileResource(domain, result.data) && result.data.id === fileId) ? result : invalidFiles('Invalid file update response; read back before another write.');
}
export async function editMediaFiles(domain: MediaDomain, fileIds: number[], patch: FilePatch): Promise<Result<FileResource[]>> {
  if (!fileDomain(domain) || !validFileIds(fileIds) || !patch || !Object.keys(patch).length) return invalidFiles();
  const ids = [...fileIds];
  const result = await request<FileResource[]>(`/api/v1/${domain}/files/editor`, { ...patch, file_ids: ids }, 'PUT', true);
  if (!result.ok) return result;
  if (!Array.isArray(result.data) || result.data.length !== ids.length || new Set(result.data.map(file => file?.id)).size !== ids.length || result.data.some(file => !validFileResource(domain, file) || !ids.includes(file.id))) return invalidFiles('Invalid file editor response; read back every attempted file before another write.');
  return result;
}
export async function readMediaFiles(domain: MediaDomain, ownerId: number, fileIds: number[]): Promise<Result<FileResource[]>> {
  if (!fileDomain(domain) || !validId(ownerId) || !validFileIds(fileIds)) return invalidFiles();
  // Snapshot the selection before awaiting; callers may change their local selection.
  const ids = [...fileIds];
  const query = new URLSearchParams({ file_ids: ids.join(','), limit: String(FILE_PAGE_SIZE), offset: '0' });
  const result = await request<ApiPage<FileResource>>(`/api/v1/${domain}/files?${query}`, undefined, undefined, true);
  if (!result.ok) return result;
  if (!filePageValid(result.data) || result.data.offset !== 0 || result.data.total !== ids.length) return invalidFiles('Readback did not return the complete attempted selection.');
  return validateFileReadback(domain, ownerId, ids, result.data.items);
}
