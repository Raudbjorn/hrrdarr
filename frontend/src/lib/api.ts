import type { Provider, ProviderInput, ProviderUpdate, ProviderSchema, ProviderKind, ProviderTestResult, ApiPage, Episode, LibraryItem, LibraryPage, LibraryPatch, LookupResult, ManualImportRequest, MediaDomain, ApiErrorEnvelope, ImportRequest, LegacyEpisode, LegacyError, LegacySeries, Operation } from './api.generated';

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
