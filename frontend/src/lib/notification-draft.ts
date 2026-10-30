import type { MovieNotificationTriggers, Notification, NotificationImplementation, NotificationInput, NotificationTestResult, TvNotificationTriggers } from './api.generated';
import type { Result } from './api';

export type TriggerDef<T> = { key: keyof T & string; label: string; requires?: keyof T & string };

/** Sonarr's event options. A TV scope never carries a movie-only trigger. */
export const TV_TRIGGERS: readonly TriggerDef<TvNotificationTriggers>[] = [
  { key: 'on_grab', label: 'On grab' },
  { key: 'on_import', label: 'On import' },
  { key: 'on_upgrade', label: 'On upgrade', requires: 'on_import' },
  { key: 'on_import_complete', label: 'On import complete (stored, not yet emitted)' },
  { key: 'on_rename', label: 'On rename (no rename executor yet)' },
  { key: 'on_series_add', label: 'On series add' },
  { key: 'on_series_delete', label: 'On series delete' },
  { key: 'on_episode_file_delete', label: 'On episode file delete' },
  { key: 'on_episode_file_delete_for_upgrade', label: 'On episode file delete for upgrade', requires: 'on_episode_file_delete' },
  { key: 'on_health_issue', label: 'On health issue' },
  { key: 'include_health_warnings', label: 'Include health warnings' },
  { key: 'on_health_restored', label: 'On health restored' },
  { key: 'on_application_update', label: 'On application update (no updater yet)' },
  { key: 'on_manual_interaction_required', label: 'On manual interaction required' },
];
/** Radarr's event options (no batch import-complete event). */
export const MOVIE_TRIGGERS: readonly TriggerDef<MovieNotificationTriggers>[] = [
  { key: 'on_grab', label: 'On grab' },
  { key: 'on_import', label: 'On import' },
  { key: 'on_upgrade', label: 'On upgrade', requires: 'on_import' },
  { key: 'on_rename', label: 'On rename (no rename executor yet)' },
  { key: 'on_movie_added', label: 'On movie added' },
  { key: 'on_movie_delete', label: 'On movie delete' },
  { key: 'on_movie_file_delete', label: 'On movie file delete' },
  { key: 'on_movie_file_delete_for_upgrade', label: 'On movie file delete for upgrade', requires: 'on_movie_file_delete' },
  { key: 'on_health_issue', label: 'On health issue' },
  { key: 'include_health_warnings', label: 'Include health warnings' },
  { key: 'on_health_restored', label: 'On health restored' },
  { key: 'on_application_update', label: 'On application update (no updater yet)' },
  { key: 'on_manual_interaction_required', label: 'On manual interaction required' },
];

export const MAX_NAME_BYTES = 128;
export const MAX_TAGS = 200;

/** `null` is what Svelte's `bind:value` yields for a cleared number input. */
export type DraftValue = string | number | boolean | null;
export type NotificationDraft = {
  id?: string; revision?: number; name: string; implementation: string; enabled: boolean;
  settings: Record<string, DraftValue>;
  /** Typed secret values; blank means unchanged (edit) or unset (create). */
  secrets: Record<string, string>;
  /** Secrets to clear on save. */
  cleared: string[];
  tv: { on: boolean; triggers: TvNotificationTriggers; tagIds: number[] };
  movies: { on: boolean; triggers: MovieNotificationTriggers; tagIds: number[] };
};

export const blankTv = (): TvNotificationTriggers => Object.fromEntries(TV_TRIGGERS.map(t => [t.key, false])) as TvNotificationTriggers;
export const blankMovies = (): MovieNotificationTriggers => Object.fromEntries(MOVIE_TRIGGERS.map(t => [t.key, false])) as MovieNotificationTriggers;

export function blankDraft(impl: NotificationImplementation): NotificationDraft {
  const settings: Record<string, DraftValue> = {};
  for (const f of impl.fields) if (!f.secret && f.default !== null) settings[f.name] = f.default;
  return {
    name: impl.label, implementation: impl.implementation, enabled: true, settings, secrets: {}, cleared: [],
    tv: { on: impl.domains.includes('tv'), triggers: blankTv(), tagIds: [] },
    movies: { on: !impl.domains.includes('tv') && impl.domains.includes('movies'), triggers: blankMovies(), tagIds: [] },
  };
}

export function draftFromNotification(n: Notification, impl: NotificationImplementation): NotificationDraft {
  const base = blankDraft(impl);
  return {
    ...base, id: n.id, revision: n.revision, name: n.name, enabled: n.enabled,
    settings: { ...base.settings, ...n.settings },
    tv: n.tv ? { on: true, triggers: { ...blankTv(), ...n.tv.triggers }, tagIds: [...n.tv.tag_ids] } : { on: false, triggers: blankTv(), tagIds: [] },
    movies: n.movies ? { on: true, triggers: { ...blankMovies(), ...n.movies.triggers }, tagIds: [...n.movies.tag_ids] } : { on: false, triggers: blankMovies(), tagIds: [] },
  };
}

const CONTROL = /[\u0000-\u001f\u007f-\u009f]/;
const HTTP_URL = /^https?:\/\/[^/\s@]+/i;
const fail = <T>(error: string): Result<T> => ({ ok: false, error });

/** Client mirror of server validation; the server stays authoritative. */
export function buildInput(draft: NotificationDraft, impl: NotificationImplementation, storedSecrets: readonly string[] = []): Result<NotificationInput> {
  const name = draft.name;
  if (name === '' || name !== name.trim() || CONTROL.test(name) || new TextEncoder().encode(name).length > MAX_NAME_BYTES)
    return fail(`Name requires 1 to ${MAX_NAME_BYTES} bytes without control characters or surrounding whitespace.`);
  if (!draft.tv.on && !draft.movies.on) return fail('Enable at least one media domain (TV or movies).');
  for (const [on, label] of [[draft.tv.on, 'tv'], [draft.movies.on, 'movies']] as const)
    if (on && !impl.domains.includes(label)) return fail(`${impl.label} does not support the ${label} domain.`);
  for (const [on, defs, triggers, label] of [
    [draft.tv.on, TV_TRIGGERS, draft.tv.triggers, 'TV'], [draft.movies.on, MOVIE_TRIGGERS, draft.movies.triggers, 'Movies'],
  ] as const) {
    if (!on) continue;
    for (const def of defs as readonly TriggerDef<Record<string, boolean>>[])
      if (def.requires && (triggers as Record<string, boolean>)[def.key] && !(triggers as Record<string, boolean>)[def.requires])
        return fail(`${label}: "${def.label}" requires its base trigger.`);
  }
  for (const [on, tags] of [[draft.tv.on, draft.tv.tagIds], [draft.movies.on, draft.movies.tagIds]] as const)
    if (on && (tags.length > MAX_TAGS || new Set(tags).size !== tags.length || tags.some(t => !Number.isSafeInteger(t) || t < 1)))
      return fail(`Tags require at most ${MAX_TAGS} distinct positive IDs.`);
  const settings: Record<string, Exclude<DraftValue, null>> = {};
  const secrets: Record<string, string | null> = {};
  for (const f of impl.fields) {
    if (f.secret) {
      const typed = draft.secrets[f.name] ?? '';
      if (typed !== '') {
        if (typed !== typed.trim() || CONTROL.test(typed)) return fail(`${f.label} must not contain control characters or surrounding whitespace.`);
        if (f.kind === 'url' && !HTTP_URL.test(typed)) return fail(`${f.label} must be an http(s) URL without credentials.`);
        secrets[f.name] = typed;
      } else if (draft.cleared.includes(f.name)) secrets[f.name] = null;
      const after = f.name in secrets ? secrets[f.name] !== null : storedSecrets.includes(f.name);
      if (f.required && !after) return fail(`${f.label} is required.`);
      continue;
    }
    const value = draft.settings[f.name];
    if (f.kind === 'boolean') { settings[f.name] = value === true; continue; }
    if (f.kind === 'number') {
      // A cleared input is unset, never Number(null) === 0.
      if (value === undefined || value === null || value === '') { if (f.required) return fail(`${f.label} is required.`); continue; }
      const n = typeof value === 'number' ? value : Number(value);
      if (!Number.isSafeInteger(n) || (f.min !== null && n < f.min) || (f.max !== null && n > f.max))
        return fail(`${f.label} must be a whole number from ${f.min ?? '-∞'} to ${f.max ?? '∞'}.`);
      settings[f.name] = n; continue;
    }
    const text = typeof value === 'string' ? value : '';
    if (text === '') { if (f.required) return fail(`${f.label} is required.`); if (f.kind !== 'select') continue; }
    if (text !== text.trim() || CONTROL.test(text)) return fail(`${f.label} must not contain control characters or surrounding whitespace.`);
    if (f.max_length !== null && new TextEncoder().encode(text).length > f.max_length) return fail(`${f.label} is too long.`);
    if (f.kind === 'select' && !f.options.includes(text)) return fail(`${f.label} must be one of ${f.options.join(', ')}.`);
    if (f.kind === 'url' && text !== '' && !HTTP_URL.test(text)) return fail(`${f.label} must be an http(s) URL without credentials.`);
    if (f.kind === 'path' && text !== '' && (!text.startsWith('/') || text.split('/').some(p => p === '.' || p === '..'))) return fail(`${f.label} must be an absolute path without . or .. segments.`);
    if (text !== '') settings[f.name] = text;
  }
  return {
    ok: true,
    data: {
      name, implementation: draft.implementation, enabled: draft.enabled, settings, secrets,
      ...(draft.tv.on ? { tv: { triggers: draft.tv.triggers, tag_ids: draft.tv.tagIds } } : {}),
      ...(draft.movies.on ? { movies: { triggers: draft.movies.triggers, tag_ids: draft.movies.tagIds } } : {}),
      ...(draft.revision !== undefined ? { revision: draft.revision } : {}),
    },
  };
}

/** Plain-language summary of a test outcome; never includes remote content. */
export function describeTest(result: NotificationTestResult): string {
  return result.results.map(r => {
    const domain = r.media_type === 'tv' ? 'TV' : 'Movies';
    return r.status === 'delivered' ? `${domain}: delivered`
      : r.status === 'skipped' ? `${domain}: nothing to send for a test event`
      : `${domain}: failed (${r.error ?? 'unknown'})`;
  }).join('; ');
}

export const SCOPE_DEFS = [
  { key: 'tv', label: 'TV', defs: TV_TRIGGERS as readonly TriggerDef<Record<string, boolean>>[] },
  { key: 'movies', label: 'Movies', defs: MOVIE_TRIGGERS as readonly TriggerDef<Record<string, boolean>>[] },
] as const;

export const triggersOn = (triggers: Record<string, boolean>): number => Object.values(triggers).filter(Boolean).length;
