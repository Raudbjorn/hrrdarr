// Pure helpers for the Activity "Search commands" list. No DOM or network access.
import type { CommandStatus, MediaTarget, SearchCommand } from './api.generated';

export const SEARCH_ACTIVITY_PAGE_SIZE = 25;
/** Idle cadence matches the other Activity lists. */
export const SEARCH_ACTIVITY_IDLE_POLL_MS = 5_000;
/** While any listed search is queued, running or waiting to retry, transitions are read sooner. */
export const SEARCH_ACTIVITY_ACTIVE_POLL_MS = 2_000;
export const SEARCH_STATUSES: readonly CommandStatus[] = ['queued', 'running', 'retry_wait', 'succeeded', 'failed', 'cancelled'];
const ACTIVE: ReadonlySet<CommandStatus> = new Set(['queued', 'running', 'retry_wait']);
const MAX_OFFSET = 4_294_967_295;

export const isActiveSearch = (command: Pick<SearchCommand, 'status'>) => ACTIVE.has(command.status);

/** Query string for `GET /api/v1/search/commands` (all targets, newest first). */
export function searchActivityQuery(status: '' | CommandStatus, offset: number): string {
  const safeOffset = Number.isSafeInteger(offset) && offset >= 0 && offset <= MAX_OFFSET ? offset : 0;
  const params = new URLSearchParams({ limit: String(SEARCH_ACTIVITY_PAGE_SIZE), offset: String(safeOffset) });
  if (status && SEARCH_STATUSES.includes(status)) params.set('status', status);
  return params.toString();
}

export function nextPollMs(items: readonly Pick<SearchCommand, 'status'>[]): number {
  return items.some(isActiveSearch) ? SEARCH_ACTIVITY_ACTIVE_POLL_MS : SEARCH_ACTIVITY_IDLE_POLL_MS;
}

/** Typed target text: an episode and a movie with the same numeric ID never read alike. */
export function targetLabel(target: MediaTarget): string {
  return `${target.media_type === 'episode' ? 'Episode' : 'Movie'} ${target.id}`;
}

export const STATUS_TEXT: Readonly<Record<CommandStatus, string>> = {
  queued: 'Queued', running: 'Running', retry_wait: 'Waiting to retry', succeeded: 'Succeeded', failed: 'Failed', cancelled: 'Cancelled',
};

const OUTCOME_TEXT: Readonly<Record<string, string>> = {
  no_eligible_release: 'No eligible release found',
};

/** Recorded failure reason, keeping the stable code visible next to any readable text. */
export function outcomeLabel(command: Pick<SearchCommand, 'error_code' | 'selected_candidate_status'>): string {
  if (command.error_code) {
    const text = OUTCOME_TEXT[command.error_code];
    return text ? `${text} (${command.error_code})` : command.error_code;
  }
  return command.selected_candidate_status ? `Selected release: ${command.selected_candidate_status}` : 'No recorded error';
}

/** One sentence for the live region, e.g. "3 search commands: 1 running, 2 queued." */
export function statusSummary(total: number, items: readonly Pick<SearchCommand, 'status'>[]): string {
  if (total === 0) return 'No search commands.';
  const counts = new Map<CommandStatus, number>();
  for (const item of items) counts.set(item.status, (counts.get(item.status) ?? 0) + 1);
  const parts = SEARCH_STATUSES.filter((status) => counts.has(status)).map((status) => `${counts.get(status)} ${STATUS_TEXT[status].toLowerCase()}`);
  const noun = total === 1 ? 'search command' : 'search commands';
  const shown = items.length < total ? ` (${items.length} on this page)` : '';
  return `${total} ${noun}${shown}${parts.length ? `: ${parts.join(', ')}` : ''}.`;
}
