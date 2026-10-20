import type {
  ImportList, ImportListImplementation, ImportListInput, ImportListMonitor, ImportListRun, ImportListSettingValue, ImportListTestResult,
  ImportListExclusionInput, MediaDomain, MinimumAvailability, SeriesType,
} from './api.generated';
import type { Result } from './api';

export const MAX_NAME_BYTES = 128;
export const MAX_TAGS = 200;
export const MAX_INTERVAL_MINUTES = 43_200;
export const MAX_SAFE_ID = Number.MAX_SAFE_INTEGER;
export const MAX_TITLE_BYTES = 1024;

export type ImportListDraft = {
  id?: string; revision?: number;
  name: string; implementation: string; media: MediaDomain;
  enabled: boolean; enableAutomaticAdd: boolean; searchOnAdd: boolean;
  monitor: ImportListMonitor;
  /** Select values: '' means not chosen. */
  qualityProfileId: string; rootFolderId: string;
  minimumAvailability: MinimumAvailability; seriesType: SeriesType; seasonFolder: boolean;
  intervalMinutes: number | string;
  tagIds: number[];
  settings: Record<string, ImportListSettingValue>;
  /** Typed secret values; blank means unchanged (edit) or unset (create). Never populated from the server. */
  secrets: Record<string, string>;
  /** Secrets to clear on save. */
  cleared: string[];
};

export const MONITOR_LABELS: Readonly<Record<ImportListMonitor, string>> = {
  all: 'All', none: 'None', first_season: 'First season only', last_season: 'Last season only',
};
export const SERIES_TYPES: readonly { value: SeriesType; label: string }[] = [
  { value: 'standard', label: 'Standard' }, { value: 'daily', label: 'Daily' }, { value: 'anime', label: 'Anime' },
];
export const AVAILABILITIES: readonly { value: MinimumAvailability; label: string }[] = [
  { value: 'tba', label: 'To be announced' }, { value: 'announced', label: 'Announced' },
  { value: 'in_cinemas', label: 'In cinemas' }, { value: 'released', label: 'Released' },
];
export const DOMAIN_LABELS: Readonly<Record<MediaDomain, string>> = { tv: 'TV', movies: 'Movies' };

/** Monitor choices per domain: movies are either monitored (all) or not (none). */
export function monitorOptions(media: MediaDomain): readonly { value: ImportListMonitor; label: string }[] {
  const all: ImportListMonitor[] = media === 'tv' ? ['all', 'none', 'first_season', 'last_season'] : ['all', 'none'];
  return all.map(value => ({ value, label: media === 'movies' && value === 'all' ? 'Monitored' : media === 'movies' ? 'Not monitored' : MONITOR_LABELS[value] }));
}

/** Which domain-specific default controls apply; mirrors the server's domain rules. */
export const domainFields = (media: MediaDomain) => ({
  seriesType: media === 'tv', seasonFolder: media === 'tv', minimumAvailability: media === 'movies',
});

/** Domains a draft may choose for an implementation. */
export const supportedDomains = (impl: ImportListImplementation): MediaDomain[] => impl.domains.filter(d => d === 'tv' || d === 'movies');

export function blankDraft(impl: ImportListImplementation, media: MediaDomain = supportedDomains(impl)[0] ?? 'tv'): ImportListDraft {
  const settings: Record<string, ImportListSettingValue> = {};
  for (const f of impl.fields) if (!f.secret && f.default !== null) settings[f.name] = f.default;
  return {
    name: impl.label, implementation: impl.implementation, media,
    enabled: true, enableAutomaticAdd: true, searchOnAdd: false, monitor: 'all',
    qualityProfileId: '', rootFolderId: '', minimumAvailability: 'released', seriesType: 'standard', seasonFolder: true,
    intervalMinutes: impl.default_interval_minutes, tagIds: [], settings, secrets: {}, cleared: [],
  };
}

/** Switching domain drops everything scoped to the old domain (tags, profile, root, tv-only monitor modes). */
export function withDomain(draft: ImportListDraft, media: MediaDomain): ImportListDraft {
  if (draft.media === media) return draft;
  return {
    ...draft, media, tagIds: [], qualityProfileId: '', rootFolderId: '',
    monitor: media === 'movies' && draft.monitor !== 'all' && draft.monitor !== 'none' ? 'all' : draft.monitor,
  };
}

export function draftFromList(list: ImportList, impl: ImportListImplementation): ImportListDraft {
  const base = blankDraft(impl, list.media_type);
  return {
    ...base, id: list.id, revision: list.revision, name: list.name, enabled: list.enabled,
    enableAutomaticAdd: list.enable_automatic_add, searchOnAdd: list.search_on_add, monitor: list.monitor,
    qualityProfileId: list.quality_profile_id === null ? '' : String(list.quality_profile_id),
    rootFolderId: list.root_folder_id === null ? '' : String(list.root_folder_id),
    minimumAvailability: list.minimum_availability ?? base.minimumAvailability,
    seriesType: list.series_type ?? base.seriesType, seasonFolder: list.season_folder ?? base.seasonFolder,
    intervalMinutes: list.interval_minutes, tagIds: [...list.tag_ids],
    settings: { ...base.settings, ...list.settings }, secrets: {}, cleared: [],
  };
}

const CONTROL = /[\u0000-\u001f\u007f-\u009f]/;
const fail = <T>(error: string): Result<T> => ({ ok: false, error });
const bytes = (text: string) => new TextEncoder().encode(text).length;
const positiveId = (text: string): number | null => {
  if (!/^[0-9]+$/.test(text)) return null;
  const n = Number(text);
  return Number.isSafeInteger(n) && n >= 1 ? n : null;
};
/** Same shape the server enforces: http(s), a host, no credentials, no fragment. */
export function validHttpUrl(text: string): boolean {
  const match = /^https?:\/\/([^/?#\s]+)([^#\s]*)$/i.exec(text);
  return !!match && !match[1].includes('@');
}

/** Client mirror of server validation; the server stays authoritative. */
export function buildInput(draft: ImportListDraft, impl: ImportListImplementation, storedSecrets: readonly string[] = []): Result<ImportListInput> {
  const name = draft.name;
  if (name === '' || name !== name.trim() || CONTROL.test(name) || bytes(name) > MAX_NAME_BYTES)
    return fail(`Name requires 1 to ${MAX_NAME_BYTES} bytes without control characters or surrounding whitespace.`);
  if (!supportedDomains(impl).includes(draft.media)) return fail(`${impl.label} does not support the ${DOMAIN_LABELS[draft.media]} domain.`);
  const tv = draft.media === 'tv';
  if (!monitorOptions(draft.media).some(o => o.value === draft.monitor)) return fail('Movie lists monitor either all or none.');
  const interval = typeof draft.intervalMinutes === 'number' ? draft.intervalMinutes : Number(draft.intervalMinutes);
  if (draft.intervalMinutes === '' || !Number.isSafeInteger(interval) || interval < impl.min_interval_minutes || interval > MAX_INTERVAL_MINUTES)
    return fail(`Interval must be a whole number from ${impl.min_interval_minutes} to ${MAX_INTERVAL_MINUTES} minutes.`);
  const profile = draft.qualityProfileId === '' ? null : positiveId(draft.qualityProfileId);
  const root = draft.rootFolderId === '' ? null : positiveId(draft.rootFolderId);
  if ((draft.qualityProfileId !== '' && profile === null) || (draft.rootFolderId !== '' && root === null)) return fail('Profile and root folder IDs must be positive.');
  if (draft.enabled && draft.enableAutomaticAdd && (profile === null || root === null))
    return fail('Automatic add requires a quality profile and a root folder.');
  if (draft.tagIds.length > MAX_TAGS || new Set(draft.tagIds).size !== draft.tagIds.length || draft.tagIds.some(t => !Number.isSafeInteger(t) || t < 1))
    return fail(`Tags require at most ${MAX_TAGS} distinct positive IDs.`);
  const settings: Record<string, ImportListSettingValue> = {};
  const secrets: Record<string, string | null> = {};
  for (const f of impl.fields) {
    if (f.secret) {
      const typed = draft.secrets[f.name] ?? '';
      if (typed !== '') {
        if (typed !== typed.trim() || CONTROL.test(typed)) return fail(`${f.label} must not contain control characters or surrounding whitespace.`);
        if (bytes(typed) > (f.max_length ?? 1024)) return fail(`${f.label} is too long.`);
        secrets[f.name] = typed;
      } else if (draft.cleared.includes(f.name)) secrets[f.name] = null;
      const after = f.name in secrets ? secrets[f.name] !== null : storedSecrets.includes(f.name);
      if (f.required && !after) return fail(`${f.label} is required.`);
      continue;
    }
    const value = draft.settings[f.name];
    if (f.kind === 'boolean') { settings[f.name] = value === true; continue; }
    if (f.kind === 'number') {
      if (value === undefined || (value as unknown) === '' || value === null) { if (f.required) return fail(`${f.label} is required.`); continue; }
      const n = typeof value === 'number' ? value : Number(value);
      if (!Number.isSafeInteger(n) || (f.min !== null && n < f.min) || (f.max !== null && n > f.max))
        return fail(`${f.label} must be a whole number from ${f.min ?? '-∞'} to ${f.max ?? '∞'}.`);
      settings[f.name] = n; continue;
    }
    const text = typeof value === 'string' ? value : '';
    if (text === '') { if (f.required) return fail(`${f.label} is required.`); if (f.kind !== 'select') continue; }
    if (text !== text.trim() || CONTROL.test(text)) return fail(`${f.label} must not contain control characters or surrounding whitespace.`);
    if (bytes(text) > (f.max_length ?? 512)) return fail(`${f.label} is too long.`);
    if (f.kind === 'select' && !f.options.includes(text)) return fail(`${f.label} must be one of ${f.options.join(', ')}.`);
    if (f.kind === 'url' && text !== '' && !validHttpUrl(text)) return fail(`${f.label} must be an http(s) URL without credentials or a fragment.`);
    if (text !== '') settings[f.name] = text;
  }
  return {
    ok: true,
    data: {
      name, implementation: draft.implementation, media_type: draft.media, enabled: draft.enabled,
      enable_automatic_add: draft.enableAutomaticAdd, search_on_add: draft.searchOnAdd, monitor: draft.monitor,
      ...(profile !== null ? { quality_profile_id: profile } : {}),
      ...(root !== null ? { root_folder_id: root } : {}),
      ...(tv ? { series_type: draft.seriesType, season_folder: draft.seasonFolder } : { minimum_availability: draft.minimumAvailability }),
      interval_minutes: interval, tag_ids: [...draft.tagIds], settings, secrets,
      ...(draft.revision !== undefined ? { revision: draft.revision } : {}),
    },
  };
}

/** Plain-language text for the stable error classes the server reports. Unknown codes stay generic. */
export const ERROR_MESSAGES: Readonly<Record<string, string>> = {
  authentication: 'The list service rejected the credentials. Check the token or API key.',
  rate_limited: 'The list service is rate limiting requests. It will be retried later.',
  timeout: 'The list service took too long to answer.',
  transport: 'The list service could not be reached. Check the address and your network.',
  server_error: 'The list service reported an internal error. It will be retried later.',
  rejected: 'The list service refused the request. Check the list address and settings.',
  redirect: 'The list service redirected to another address, which is not followed.',
  host_denied: 'The list address points to a network the server is not allowed to contact.',
  invalid_response: 'The list service answered with data that is not a valid list.',
  resource_limit: 'The list is larger than the allowed maximum, so nothing was imported from it.',
  invalid_settings: 'The list settings are incomplete or invalid. Edit the list and check its fields.',
  unsupported: 'This list type is not available for the chosen media type.',
  not_configured: 'The list needs more configuration before it can sync.',
  credentials_locked: 'Stored credentials could not be unlocked. Check the independently stored provider key.',
  key_unavailable: 'The provider key for stored credentials is unavailable.',
};
export const describeError = (code: string | null | undefined): string =>
  !code ? '' : ERROR_MESSAGES[code] ?? 'The sync failed for an unrecognised reason.';

/** Plain-language summary of a test outcome; never includes remote content. */
export function describeTest(result: ImportListTestResult): string {
  if (result.ok) return `Reachable: ${result.item_count ?? 0} item${result.item_count === 1 ? '' : 's'} found. Nothing was added.`;
  return `Test failed: ${describeError(result.error ?? 'unknown')}`;
}

export const isRunActive = (run: Pick<ImportListRun, 'status'> | null | undefined): boolean => !!run && (run.status === 'queued' || run.status === 'running');
export const isRunTerminal = (run: Pick<ImportListRun, 'status'> | null | undefined): boolean => !!run && !isRunActive(run);

export function describeRun(run: ImportListRun): string {
  const counts = `fetched ${run.items_fetched}, added ${run.added}, already in library ${run.existing}, excluded ${run.excluded}, unresolved ${run.unresolved}, failed ${run.failed}, pending ${run.pending}`;
  switch (run.status) {
    case 'queued': return 'Queued: waiting for a worker.';
    case 'running': return `Running: ${counts}.`;
    case 'succeeded': return `Succeeded: ${counts}.`;
    default: return `Failed: ${describeError(run.error_code ?? 'unknown')} (${counts}).`;
  }
}

/** Sequential polling delay: 1 s, easing to 5 s. */
export const POLL_BASE_MS = 1000;
export const POLL_MAX_MS = 5000;
export const MAX_POLLS = 600;
export const pollDelay = (attempt: number): number => Math.min(POLL_MAX_MS, POLL_BASE_MS + Math.max(0, Math.floor(attempt / 5)) * 500);

/** Seconds since the epoch, as the server stores them. */
export function formatTime(seconds: number | null | undefined): string {
  if (seconds === null || seconds === undefined || !Number.isFinite(seconds) || seconds <= 0) return 'never';
  const date = new Date(seconds * 1000);
  return Number.isNaN(date.getTime()) ? 'never' : date.toISOString().replace('T', ' ').slice(0, 16) + ' UTC';
}

/** One-line status for a configured list. */
export function listStatus(list: Pick<ImportList, 'last_sync_at' | 'last_error' | 'last_success_at'>): string {
  if (list.last_sync_at === null) return 'Not synced yet';
  return list.last_error ? `Last sync ${formatTime(list.last_sync_at)} failed: ${describeError(list.last_error)}`
    : `Last sync ${formatTime(list.last_sync_at)} succeeded`;
}

export const ITEM_STATE_LABELS: Readonly<Record<string, string>> = {
  pending: 'Pending', added: 'Added', existing: 'Already in library', excluded: 'Excluded', unresolved: 'Could not be matched', failed: 'Failed',
};

/** Exclusion form values to the server shape; the external id is the TVDB (tv) or TMDb (movies) id. */
export function buildExclusion(media: MediaDomain, externalId: string, title: string, year: string): Result<ImportListExclusionInput> {
  const id = positiveId(externalId.trim());
  if (id === null) return fail(`${media === 'tv' ? 'TVDB' : 'TMDb'} ID must be a positive whole number.`);
  if (title === '' || title !== title.trim() || CONTROL.test(title) || bytes(title) > MAX_TITLE_BYTES)
    return fail(`Title requires 1 to ${MAX_TITLE_BYTES} bytes without control characters or surrounding whitespace.`);
  let y: number | undefined;
  if (year.trim() !== '') {
    const n = /^[0-9]{1,4}$/.test(year.trim()) ? Number(year.trim()) : NaN;
    if (!(n >= 1 && n <= 9999)) return fail('Year must be 1 to 9999.');
    y = n;
  }
  return { ok: true, data: { media_type: media, external_id: id, title, ...(y !== undefined ? { year: y } : {}) } };
}

export const pageCount = (total: number, size: number): number => Math.max(1, Math.ceil(total / size));
