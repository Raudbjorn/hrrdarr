import type { MetadataCommand, MetadataCommandInput, MetadataCommandQuery, MetadataRefreshTarget, Command, CommandInput, CommandQuery, RefreshTarget, RefreshSchedule, RefreshScheduleInput, RefreshScheduleDelete, QueueSnapshot, Provider, ProviderInput, ProviderUpdate, ProviderSchema, ProviderKind, ProviderTestResult, ApiPage, Episode, LibraryItem, LibraryPage, LibraryPatch, LookupResult, ManualImportRequest, MediaDomain, ApiErrorEnvelope, ImportRequest, LegacyEpisode, LegacyError, LegacySeries, Operation } from './api.generated';

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
export const updateLibrary = (domain: MediaDomain, id: number, patch: LibraryPatch) => validId(id) ? request<LibraryItem>(`${collection(domain)}/${id}`, patch, 'PUT', true) : invalid<LibraryItem>();
export const listEpisodes = (seriesId: number, offset = 0) => validId(seriesId) ? request<ApiPage<Episode>>(`/api/v1/episodes?series_id=${seriesId}&limit=25&offset=${offset}`, undefined, undefined, true) : invalid<ApiPage<Episode>>();
export const monitorEpisode = (id: number, monitored: boolean) => validId(id) ? request<Episode>(`/api/v1/episodes/${id}`, {monitored}, 'PUT', true) : invalid<Episode>();
export const previewManualImport = (body: ManualImportRequest) => validId(body.target.id) ? request<Operation>('/api/v1/imports', body, 'POST', true) : invalid<Operation>();
const validOperation = (id: string) => /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(id);
export const getImport = (id: string) => validOperation(id) ? request<Operation>(`/api/v1/imports/${id}`, undefined, undefined, true) : Promise.resolve<Result<Operation>>({ok:false,error:'Invalid operation ID.'});
export const executeImport = (id: string) => validOperation(id) ? request<Operation>(`/api/v1/imports/${id}/execute`, undefined, 'POST', true) : Promise.resolve<Result<Operation>>({ok:false,error:'Invalid operation ID.'});

export const listProviders = (offset = 0) => request<ApiPage<Provider>>(`/api/v1/providers?limit=25&offset=${offset}`, undefined, undefined, true);
export const getProviderSchema = (media: MediaDomain, kind: ProviderKind) => request<ProviderSchema>(`/api/v1/providers/schema?${new URLSearchParams({media_type: media, kind})}`, undefined, undefined, true);
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
