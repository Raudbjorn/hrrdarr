// Pure Wanted view helpers: query building, sort choices, labels and request bodies.
// No DOM or network access; every value here is rendered as text by the caller.
import type { ClientSelection, MediaDomain, MovieWantedItem, TvWantedItem, WantedAvailability, WantedDirection, WantedKind, WantedMonitored, WantedSearchInput, WantedSearchReceipt, WantedSelection, WantedSort } from './api.generated';

export const WANTED_PAGE_SIZES = [25, 50, 100] as const;
export const MAX_WANTED_SEARCH_TARGETS = 1024;

export type WantedOptions = {
  limit: number; offset: number; monitored: WantedMonitored; sort: WantedSort; direction: WantedDirection;
  availability: WantedAvailability;
};

export function defaultOptions(domain: MediaDomain): WantedOptions {
  return { limit: 50, offset: 0, monitored: 'monitored', sort: domain === 'tv' ? 'air_date' : 'title', direction: 'asc', availability: 'all' };
}

export function sortChoices(domain: MediaDomain, kind: WantedKind): { value: WantedSort; label: string }[] {
  const base: { value: WantedSort; label: string }[] = domain === 'tv'
    ? [{ value: 'air_date', label: 'Air date' }, { value: 'series_title', label: 'Series title' }]
    : [{ value: 'title', label: 'Title' }, { value: 'year', label: 'Year' }, { value: 'in_cinemas', label: 'In cinemas' },
       { value: 'digital_release', label: 'Digital release' }, { value: 'physical_release', label: 'Physical release' }];
  return kind === 'cutoff' ? [...base, { value: 'quality', label: 'Current quality' }] : base;
}

/** Only keys the server accepts for this list; defaults are sent explicitly for clarity. */
export function wantedQuery(domain: MediaDomain, kind: WantedKind, options: WantedOptions): string {
  const sorts = sortChoices(domain, kind).map((choice) => choice.value);
  const params = new URLSearchParams();
  params.set('limit', String(Math.min(100, Math.max(1, Math.trunc(options.limit)))));
  params.set('offset', String(Math.max(0, Math.trunc(options.offset))));
  params.set('monitored', options.monitored);
  params.set('sort', sorts.includes(options.sort) ? options.sort : sorts[0]);
  params.set('direction', options.direction);
  if (domain === 'movies' && kind === 'missing' && options.availability !== 'all') params.set('availability', options.availability);
  return params.toString();
}

const pad = (value: number) => String(value).padStart(2, '0');

export function episodeLabel(item: TvWantedItem): string {
  const parts = [item.season === 0 ? `Special ${item.number}` : `S${pad(item.season)}E${pad(item.number)}`];
  if (item.series_type === 'anime' && item.absolute_episode_number !== null) parts.push(`absolute ${item.absolute_episode_number}`);
  if (item.scene_season_number !== null && item.scene_episode_number !== null) parts.push(`scene S${pad(item.scene_season_number)}E${pad(item.scene_episode_number)}`);
  return parts.join(' · ');
}

export function formatDate(value: string | null): string {
  if (!value) return 'Unknown';
  const parsed = Date.parse(value.includes('T') ? value : `${value.replace(' ', 'T')}Z`);
  return Number.isFinite(parsed) ? new Date(parsed).toISOString().slice(0, 10) : 'Unknown';
}

export function availabilityLabel(item: MovieWantedItem): string {
  return item.availability === 'available' ? 'Available' : item.availability === 'unavailable'
    ? `Not yet available (${item.minimum_availability ?? 'no minimum availability'})` : 'No availability policy';
}

export function monitoredLabel(item: TvWantedItem | MovieWantedItem): string {
  if ('series_monitored' in item) {
    if (item.effective_monitored) return 'Monitored';
    return item.monitored ? 'Series unmonitored' : 'Episode unmonitored';
  }
  return item.monitored ? 'Monitored' : 'Unmonitored';
}

export function cutoffLabel(item: TvWantedItem | MovieWantedItem): string {
  const quality = item.file?.quality_name ?? (item.file?.quality_id !== null && item.file?.quality_id !== undefined ? `Quality ${item.file.quality_id}` : 'Unknown quality');
  if (!item.cutoff) return quality;
  return `${quality} (rank ${item.cutoff.quality_rank} below ${item.cutoff.threshold_rank}${item.cutoff.basis === 'first_allowed' ? ', upgrades disabled' : ''})`;
}

export function searchBody(
  requestId: string, kind: WantedKind, selection: WantedSelection,
  indexer: { id: string; revision: number }, client: ClientSelection,
): WantedSearchInput {
  return { request_id: requestId, kind, selection, indexer_id: indexer.id, indexer_revision: indexer.revision, client_selection: client, priority: 'normal' };
}

export function selectedIds(selection: ReadonlySet<number>): number[] {
  return [...selection].filter((id) => Number.isSafeInteger(id) && id > 0).sort((a, b) => a - b);
}

export function receiptSummary(receipt: WantedSearchReceipt): string {
  const noun = receipt.media_type === 'tv' ? 'episode' : 'movie';
  const plural = (count: number) => `${count} ${noun}${count === 1 ? '' : 's'}`;
  if (receipt.replayed) return `Earlier identical request read back: ${plural(receipt.admitted)} already queued for search.`;
  const parts = [`Queued ${plural(receipt.admitted)} for automatic search`];
  if (receipt.matched !== null) parts.push(`${receipt.matched} matched`);
  if (receipt.skipped_active) parts.push(`${receipt.skipped_active} skipped because a search is already active`);
  return `${parts.join('; ')}.`;
}

/** Keeps only selections still visible on the current page; scope changes clear selection. */
export function retainVisible(selection: ReadonlySet<number>, visible: readonly number[]): Set<number> {
  const keep = new Set(visible);
  return new Set([...selection].filter((id) => keep.has(id)));
}
