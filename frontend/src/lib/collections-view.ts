// Pure helpers for the Movie Collections panel. Policy rules mirror src/collections.rs.
import type { CollectionPatch, CollectionRefreshCommand, CommandStatus, MinimumAvailability, MovieCollection } from './api.generated';

export const PAGE_SIZE = 25;
export const POLL_INTERVAL_MS = 1_000;
export const POLL_MAX_MS = 5 * 60_000;
export const MAX_QUERY_LENGTH = 100;
export const AVAILABILITIES: readonly MinimumAvailability[] = ['tba', 'announced', 'in_cinemas', 'released'];
export const AVAILABILITY_LABELS: Record<MinimumAvailability, string> = {
  tba: 'TBA', announced: 'Announced', in_cinemas: 'In cinemas', released: 'Released',
};
export type MonitorFilter = 'all' | 'monitored' | 'unmonitored';

export type PolicyDraft = {
  qualityProfileId: string; // '' means unset
  rootFolderId: string;
  minimumAvailability: string;
  searchOnAdd: boolean;
  tagIds: number[];
};
type Policy = Pick<MovieCollection, 'quality_profile_id' | 'root_folder_id' | 'minimum_availability'>;

export const isAvailability = (value: unknown): value is MinimumAvailability =>
  typeof value === 'string' && (AVAILABILITIES as readonly string[]).includes(value);

/** Server rule: monitoring needs quality profile, root folder and minimum availability. */
export const policyComplete = (c: Policy): boolean =>
  c.quality_profile_id !== null && c.root_folder_id !== null && c.minimum_availability !== null;

export const draftOf = (c: MovieCollection): PolicyDraft => ({
  qualityProfileId: c.quality_profile_id === null ? '' : String(c.quality_profile_id),
  rootFolderId: c.root_folder_id === null ? '' : String(c.root_folder_id),
  minimumAvailability: c.minimum_availability ?? '',
  searchOnAdd: c.search_on_add,
  tagIds: [...c.tag_ids].sort((a, b) => a - b),
});

const idOrNull = (text: string): number | null => {
  if (text === '') return null;
  const n = Number(text);
  return Number.isSafeInteger(n) && n > 0 ? n : null;
};

/** Complete policy patch (all five fields) built from the draft. */
export function policyPatch(draft: PolicyDraft): CollectionPatch {
  return {
    quality_profile_id: idOrNull(draft.qualityProfileId),
    root_folder_id: idOrNull(draft.rootFolderId),
    minimum_availability: isAvailability(draft.minimumAvailability) ? draft.minimumAvailability : null,
    search_on_add: draft.searchOnAdd,
    tag_ids: [...new Set(draft.tagIds)].sort((a, b) => a - b),
  };
}

export function draftDirty(draft: PolicyDraft, c: MovieCollection): boolean {
  const a = draftOf(c), b = draft;
  return a.qualityProfileId !== b.qualityProfileId || a.rootFolderId !== b.rootFolderId
    || a.minimumAvailability !== b.minimumAvailability || a.searchOnAdd !== b.searchOnAdd
    || JSON.stringify(a.tagIds) !== JSON.stringify([...new Set(b.tagIds)].sort((x, y) => x - y));
}

export const monitorPatch = (monitored: boolean): CollectionPatch => ({ monitored });
export const libraryMonitorPatch = (monitored: boolean): CollectionPatch => ({ monitor_movies: monitored });

export function toggleId(ids: readonly number[], id: number, on: boolean): number[] {
  const rest = ids.filter(x => x !== id);
  return on ? [...rest, id].sort((a, b) => a - b) : rest;
}

export function listQuery(filter: MonitorFilter, text: string, page: number): { limit: number; offset: number; monitored?: boolean; q?: string } {
  const q = text.trim().slice(0, MAX_QUERY_LENGTH);
  return {
    limit: PAGE_SIZE, offset: Math.max(0, page) * PAGE_SIZE,
    ...(filter === 'all' ? {} : { monitored: filter === 'monitored' }),
    ...(q ? { q } : {}),
  };
}

export const pageCount = (total: number): number => Math.max(1, Math.ceil(Math.max(0, total) / PAGE_SIZE));

export const countsLabel = (c: Pick<MovieCollection, 'library_movie_count' | 'movie_count' | 'missing_movie_count'>): string =>
  `${c.library_movie_count} of ${c.movie_count} in library, ${c.missing_movie_count} missing`;

export function lastSyncLabel(value: string | null): string {
  if (!value) return 'Never synced';
  const ms = Date.parse(value);
  return Number.isNaN(ms) ? 'Never synced' : `Synced ${new Date(ms).toISOString().slice(0, 16).replace('T', ' ')} UTC`;
}

export function policySummary(
  c: Policy & Pick<MovieCollection, 'root_folder_path' | 'search_on_add' | 'tag_ids'>,
  profileNames: ReadonlyMap<number, string>,
): string {
  if (c.quality_profile_id === null && c.root_folder_id === null && c.minimum_availability === null) return 'No policy configured';
  const profile = c.quality_profile_id === null ? 'no profile' : (profileNames.get(c.quality_profile_id) ?? `profile ${c.quality_profile_id}`);
  const root = c.root_folder_id === null ? 'no root folder' : (c.root_folder_path ?? `root folder ${c.root_folder_id}`);
  const availability = isAvailability(c.minimum_availability) ? AVAILABILITY_LABELS[c.minimum_availability] : 'no availability';
  const extras = [c.search_on_add ? 'search on add' : '', c.tag_ids.length ? `${c.tag_ids.length} tag${c.tag_ids.length === 1 ? '' : 's'}` : ''].filter(Boolean);
  return [profile, root, availability, ...extras].join(' · ') + (policyComplete(c) ? '' : ' (incomplete)');
}

export const ACTIVE_STATUSES: readonly CommandStatus[] = ['queued', 'running', 'retry_wait'];
export const isActive = (status: CommandStatus): boolean => ACTIVE_STATUSES.includes(status);
export const isTerminal = (status: CommandStatus): boolean => !isActive(status);

const STATUS_TEXT: Record<CommandStatus, string> = {
  queued: 'Queued', running: 'Running', retry_wait: 'Waiting to retry', succeeded: 'Succeeded', failed: 'Failed', cancelled: 'Cancelled',
};
export const statusText = (status: CommandStatus): string => STATUS_TEXT[status] ?? status;

const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

export function refreshSummary(cmd: CollectionRefreshCommand): string {
  const head = `Refresh ${statusText(cmd.status).toLowerCase()}`;
  if (isActive(cmd.status)) return `${head}.`;
  const body = `${plural(cmd.collections_checked, 'collection')} checked, ${cmd.collections_failed} failed, ${plural(cmd.movies_added, 'movie')} added`;
  const searches = cmd.searches_pending > 0
    ? `, ${plural(cmd.searches_pending, 'search', 'searches')} pending (search on add is recorded but searches are not submitted)` : '';
  const error = cmd.error_code ? ` Error: ${cmd.error_code}.` : '';
  return `${head}: ${body}${searches}.${error}`;
}
