import type { ApiErrorEnvelope, ImportRequest, LegacyEpisode, LegacyError, LegacySeries, Operation } from './api.generated';

type Result<T> = { ok: true; data: T } | { ok: false; error: string };
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

async function request<T>(path: string, body?: ImportRequest): Promise<Result<T>> {
  if (body && !safeNumbers(body)) return { ok: false, error: 'Request contains an unsafe integer.' };
  try {
    const response = await fetch(path, {
      signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
      ...(body ? { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) } : {}),
    });
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
      return { ok: false, error: message };
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
