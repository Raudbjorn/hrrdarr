import type { ApiPage, HistoryEvent, HistoryQuery, MediaDomain, MediaTarget, FileQuality } from './api.generated';

export const HISTORY_DEFAULT_LIMIT = 50;
export const HISTORY_MAX_LIMIT = 100;
export const HISTORY_MAX_OFFSET = 10_000;
export const NOT_RECORDED = 'not recorded';

/** Raw form fields; every value is the text the user typed. */
export type HistoryForm = {
  mediaType: '' | MediaDomain;
  episodeId: string; movieId: string; seriesId: string; season: string;
  from: string; to: string; limit: string; offset: string;
};
export const emptyHistoryForm = (): HistoryForm => ({
  mediaType: '', episodeId: '', movieId: '', seriesId: '', season: '', from: '', to: '', limit: '', offset: '',
});

export type HistoryQueryResult = { ok: true; query: HistoryQuery; search: string } | { ok: false; errors: string[] };

const SAFE_MAX = Number.MAX_SAFE_INTEGER;
const RFC3339 = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})(?:\.(\d{1,9}))?(Z|[+-]\d{2}:\d{2})$/i;
const DIGITS = /^\d+$/;

function integer(text: string, min: number, max: number): number | null {
  const trimmed = text.trim();
  if (!DIGITS.test(trimmed)) return null;
  const value = Number(trimmed);
  return Number.isSafeInteger(value) && value >= min && value <= max ? value : null;
}

/** UTC instant as [epoch seconds, nanoseconds], or an error string. Leap seconds and timezone-less input are rejected. */
export function parseTimestamp(text: string): { ok: true; seconds: number; nanos: number } | { ok: false; error: string } {
  const match = RFC3339.exec(text.trim());
  if (!match) return { ok: false, error: 'must be RFC3339 with an explicit timezone, for example 2026-01-02T03:04:05Z or 2026-01-02T03:04:05+02:00' };
  const [, y, mo, d, h, mi, s, frac = '', zone] = match;
  const [year, month, day, hour, minute, second] = [y, mo, d, h, mi, s].map(Number);
  if (month < 1 || month > 12 || hour > 23 || minute > 59) return { ok: false, error: 'has an out-of-range date or time field' };
  if (second === 60) return { ok: false, error: 'leap seconds are not supported' };
  if (second > 59) return { ok: false, error: 'has an out-of-range date or time field' };
  const probe = new Date(0);
  probe.setUTCFullYear(year, month - 1, day);
  if (probe.getUTCFullYear() !== year || probe.getUTCMonth() !== month - 1 || probe.getUTCDate() !== day) return { ok: false, error: 'is not a real calendar date' };
  let offsetMinutes = 0;
  if (zone.toUpperCase() !== 'Z') {
    const zh = Number(zone.slice(1, 3)), zm = Number(zone.slice(4, 6));
    if (zh > 23 || zm > 59) return { ok: false, error: 'has an out-of-range timezone offset' };
    offsetMinutes = (zone[0] === '-' ? -1 : 1) * (zh * 60 + zm);
  }
  probe.setUTCHours(hour, minute, second, 0);
  const seconds = probe.getTime() / 1000 - offsetMinutes * 60;
  const utcYear = new Date(seconds * 1000).getUTCFullYear();
  if (utcYear < 1 || utcYear > 9999) return { ok: false, error: 'must normalise to a UTC year between 0001 and 9999' };
  return { ok: true, seconds, nanos: Number(frac.padEnd(9, '0')) };
}

export function buildHistoryQuery(form: HistoryForm): HistoryQueryResult {
  const errors: string[] = [];
  const query: HistoryQuery = {};
  const text = (value: string) => value.trim();
  const id = (label: string, value: string): number | undefined => {
    if (!text(value)) return undefined;
    const parsed = integer(value, 1, SAFE_MAX);
    if (parsed === null) errors.push(`${label} must be a positive whole number.`);
    return parsed ?? undefined;
  };
  const episode = id('Episode ID', form.episodeId), movie = id('Movie ID', form.movieId), series = id('Series ID', form.seriesId);
  let season: number | undefined;
  if (text(form.season)) {
    const parsed = integer(form.season, 0, SAFE_MAX);
    if (parsed === null) errors.push('Season must be a whole number, zero or greater.');
    else season = parsed;
  }
  if (form.mediaType !== '' && form.mediaType !== 'tv' && form.mediaType !== 'movies') errors.push('Media type must be TV, Movies or unset.');
  const tvSelector = text(form.episodeId) !== '' || text(form.seriesId) !== '' || text(form.season) !== '';
  const movieSelector = text(form.movieId) !== '';
  if (text(form.season) !== '' && text(form.seriesId) === '') errors.push('Season requires a Series ID.');
  if (movieSelector && tvSelector) errors.push('Movie ID cannot be combined with Episode ID, Series ID or Season.');
  if (movieSelector && form.mediaType === 'tv') errors.push('Movie ID cannot be combined with media type TV.');
  if (tvSelector && form.mediaType === 'movies') errors.push('Episode ID, Series ID and Season cannot be combined with media type Movies.');

  const bounds: { seconds: number; nanos: number }[] = [];
  for (const [label, key] of [['From', 'from'], ['To', 'to']] as const) {
    const raw = text(form[key]);
    if (!raw) continue;
    const parsed = parseTimestamp(raw);
    if (!parsed.ok) errors.push(`${label} ${parsed.error}.`);
    else { query[key] = raw; bounds.push(parsed); }
  }
  if (bounds.length === 2) {
    const [a, b] = bounds;
    if (a.seconds > b.seconds || (a.seconds === b.seconds && a.nanos >= b.nanos)) errors.push('From (inclusive) must be earlier than To (exclusive).');
  }
  const limit = text(form.limit) ? integer(form.limit, 1, HISTORY_MAX_LIMIT) : HISTORY_DEFAULT_LIMIT;
  if (limit === null) errors.push(`Limit must be a whole number from 1 to ${HISTORY_MAX_LIMIT}.`);
  const offset = text(form.offset) ? integer(form.offset, 0, HISTORY_MAX_OFFSET) : 0;
  if (offset === null) errors.push(`Offset must be a whole number from 0 to ${HISTORY_MAX_OFFSET}.`);
  if (errors.length) return { ok: false, errors };

  if (form.mediaType) query.media_type = form.mediaType;
  if (episode !== undefined) query.episode_id = episode;
  if (movie !== undefined) query.movie_id = movie;
  if (series !== undefined) query.series_id = series;
  if (season !== undefined) query.season = season;
  query.limit = limit as number;
  query.offset = offset as number;
  return { ok: true, query, search: historySearch(query) };
}

/** URLSearchParams encodes `+` in timezone offsets as %2B. */
export function historySearch(query: HistoryQuery): string {
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(query)) if (value !== undefined) params.set(key, String(value));
  return params.toString();
}

// ---- event view-model ----

export type HistoryFact = { label: string; value: string; recorded: boolean };
export type HistoryEventView = {
  key: string;
  origin: 'native_import' | 'source_snapshot';
  originLabel: string;
  domain: 'TV' | 'Movies';
  target: string;
  eventLabel: string;
  sourceEventType: string | null;
  sourceCodeNote: string | null;
  time: string;
  timeLabel: string;
  facts: HistoryFact[];
  warnings: string[];
};

export const SOURCE_EVENT_LABELS: Record<string, string> = {
  grabbed: 'Release grabbed',
  series_folder_imported: 'Series folder imported',
  download_folder_imported: 'Download folder imported',
  download_failed: 'Download failed',
  file_deleted: 'File deleted',
  file_renamed: 'File renamed',
  download_ignored: 'Download ignored',
  movie_folder_imported: 'Movie folder imported',
};
const APPLICATION_LABELS = { sonarr: 'Sonarr', radarr: 'Radarr' } as const;

const fact = (label: string, value: string | number | null | undefined): HistoryFact =>
  value === null || value === undefined || value === '' ? { label, value: NOT_RECORDED, recorded: false } : { label, value: String(value), recorded: true };

export function describeTarget(target: MediaTarget): { domain: 'TV' | 'Movies'; label: string } {
  return target.media_type === 'episode'
    ? { domain: 'TV', label: `TV episode ${target.id}` }
    : { domain: 'Movies', label: `Movie ${target.id}` };
}
export function describeQuality(quality: FileQuality | null): string | null {
  if (!quality) return null;
  const rev = quality.revision;
  return `Quality ID ${quality.quality_id}` + (rev ? `, revision v${rev.version}${rev.real ? `, real ${rev.real}` : ''}${rev.is_repack ? ', repack' : ''}` : ', revision not recorded');
}
export const describeLanguages = (languages: number[] | null): string | null =>
  languages === null ? null : languages.length ? languages.map(id => `ID ${id}`).join(', ') : 'none';

/** Source code 6 means different things per domain; the server's semantic type is authoritative. */
export function sourceCodeNote(code: number, target: MediaTarget): string | null {
  if (code !== 6) return null;
  return target.media_type === 'episode'
    ? 'Source code 6 is a file rename for TV.'
    : 'Source code 6 is a file deletion for movies, not a rename.';
}

export function describeEvent(event: HistoryEvent): HistoryEventView {
  const { domain, label } = describeTarget(event.target);
  if (event.origin === 'native_import') {
    const warnings: string[] = [];
    const fileDomain = event.file.media_type === 'tv' ? 'TV' : 'Movies';
    if (fileDomain !== domain) warnings.push(`Historical file domain (${fileDomain}) does not match the target domain (${domain}).`);
    return {
      key: `native:${event.id}`, origin: 'native_import', originLabel: 'Native import', domain, target: label,
      eventLabel: 'File imported',
      sourceEventType: null, sourceCodeNote: null, time: event.imported_at, timeLabel: 'Association committed (UTC)',
      facts: [
        fact('Operation', event.id), fact('Historical file', `${fileDomain} file ${event.file.id}`),
        fact('Source path (as recorded)', event.source), fact('Destination path (as recorded)', event.destination),
        fact('Size (bytes)', event.size_bytes), fact('SHA-256', event.sha256),
      ],
      warnings,
    };
  }
  const id = event.id;
  const semantic = SOURCE_EVENT_LABELS[event.event_type] ?? String(event.event_type);
  return {
    key: `source:${id.application}:${id.fingerprint}:${id.source_id}`, origin: 'source_snapshot',
    originLabel: `Source snapshot (${APPLICATION_LABELS[id.application] ?? id.application})`, domain, target: label,
    eventLabel: semantic, sourceEventType: `${event.source_event_type}`, sourceCodeNote: sourceCodeNote(event.source_event_type, event.target),
    time: event.occurred_at, timeLabel: 'Occurred at source (UTC)',
    facts: [
      fact('Source event ID', id.source_id), fact('Snapshot fingerprint', id.fingerprint),
      fact('Source title', event.source_title), fact('Download ID', event.download_id),
      fact('Quality', describeQuality(event.quality)), fact('Languages', describeLanguages(event.languages)),
      { label: 'Paths, size, hash, local import time', value: 'not part of source events', recorded: false },
    ],
    warnings: [],
  };
}

// ---- page and pagination ----

export type HistoryPageView = {
  events: HistoryEventView[]; total: number; limit: number; offset: number;
  rangeLabel: string; prevOffset: number | null; nextOffset: number | null; beyondOffsetCap: boolean;
};
export type HistoryPageState = { kind: 'ready'; view: HistoryPageView } | { kind: 'error'; message: string };

export function pageNavigation(total: number, limit: number, offset: number) {
  const prevOffset = offset > 0 ? Math.max(0, offset - limit) : null;
  const next = offset + limit;
  const hasNext = next < total;
  return { prevOffset, nextOffset: hasNext && next <= HISTORY_MAX_OFFSET ? next : null, beyondOffsetCap: hasNext && next > HISTORY_MAX_OFFSET };
}

const isEvent = (value: unknown): value is HistoryEvent => {
  if (!value || typeof value !== 'object') return false;
  const e = value as Record<string, any>;
  const target = e.target;
  if (!target || (target.media_type !== 'episode' && target.media_type !== 'movie') || !Number.isSafeInteger(target.id)) return false;
  if (e.origin === 'native_import') return typeof e.id === 'string' && !!e.file && typeof e.imported_at === 'string';
  if (e.origin === 'source_snapshot') return !!e.id && typeof e.id === 'object' && Number.isSafeInteger(e.source_event_type) && typeof e.occurred_at === 'string';
  return false;
};

export function describeHistoryPage(raw: unknown): HistoryPageState {
  const bad: HistoryPageState = { kind: 'error', message: 'The server returned an unrecognised history response.' };
  if (!raw || typeof raw !== 'object') return bad;
  const page = raw as Partial<ApiPage<unknown>>;
  if (!Array.isArray(page.items) || ![page.total, page.limit, page.offset].every(n => Number.isSafeInteger(n) && (n as number) >= 0)) return bad;
  if (!page.items.every(isEvent)) return bad;
  const { total, limit, offset } = page as { total: number; limit: number; offset: number };
  const events = (page.items as HistoryEvent[]).map(describeEvent);
  const rangeLabel = total === 0 ? 'No events' : `Showing ${offset + 1}–${offset + events.length} of ${total}`;
  return { kind: 'ready', view: { events, total, limit, offset, rangeLabel, ...pageNavigation(total, limit, offset) } };
}

// ---- errors ----

export type HistoryErrorView = { code: string; status: number | null; title: string; advice: string; retryable: boolean; message: string };
export function describeHistoryFailure(failure: { error: string; code?: string; status?: number }): HistoryErrorView {
  const code = failure.code ?? 'unknown', status = failure.status ?? null;
  const base = { code, status, message: failure.error };
  if (code === 'invalid_history_query') return { ...base, title: 'The server rejected the history filters', advice: 'Change the filters and apply them again. Retrying the same query will fail the same way.', retryable: false };
  if (code === 'history_response_limit') return { ...base, title: 'The response was too large', advice: 'Narrow the media type, library selectors or date range, or lower the page size.', retryable: false };
  if (code === 'history_timeout') return { ...base, title: 'The history read timed out', advice: 'The server stopped after its five-second deadline. Retry, or narrow the filters.', retryable: true };
  if (status === 500) return { ...base, title: 'The server could not read history', advice: 'This is a server-side storage error. Retry; the response carries no further detail.', retryable: true };
  return { ...base, title: 'History could not be loaded', advice: 'The request failed before a usable response arrived. Retry when ready.', retryable: true };
}

export const HISTORY_NOTES = [
  'Native timestamps mark the moment the file association was committed. They do not show that cleanup of a moved source file finished.',
  'Paths describe what was committed at the time. They do not assert that a file currently exists at that location.',
  'Offset pages are not a frozen snapshot: new events arriving between requests can shift items across pages.',
  'Series and season filters use each episode\'s current relationship and numbering, not what it was at event time.',
  'This view is read-only and is not V3 wire-compatible; it is not full History parity.',
] as const;
