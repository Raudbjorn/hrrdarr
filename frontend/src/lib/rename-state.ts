// Pure rename selection/state helpers shared by the series and movie panels.
import type { RenameCommand, RenameCommandDetail, RenameInput, RenameReason } from './api.generated';
import { cancelRenameCommand, createRenameCommand, getRenameCommand } from './api.ts';

export const reasonText: Record<RenameReason, string> = {
  invalid_path: 'The stored path is invalid.',
  outside_owner_folder: 'The file is outside the item folder.',
  missing_extension: 'The stored file has no usable extension.',
  invalid_file_facts: 'Stored file facts required by the naming pattern are invalid.',
  naming_render_failed: 'The naming pattern cannot produce a safe name for this file.',
  multi_episode_style_required: 'Choose a multi-episode style in naming settings.',
  no_linked_episodes: 'No episode is linked to this file.',
  invalid_episode_set: 'The linked episodes cannot share one file name.',
  folder_format_unavailable: 'No usable folder format is configured.',
  destination_in_use: 'Another tracked file already uses the new path.',
  duplicate_destination: 'Another selected file would get the same new path.',
  destination_exists: 'A file or folder already exists at the new path.',
  source_missing: 'The file is missing on disk.',
  path_unsafe: 'A path component is a link or outside the allowed folder.',
};
export const isTerminal = (c: Pick<RenameCommand, 'status'>) => c.status === 'succeeded' || c.status === 'failed' || c.status === 'cancelled';
/** Selected file IDs that can still be renamed, in display order. */
export const selectedChangeIds = (items: { id: number; status: string }[], selected: ReadonlySet<number>): number[] =>
  items.filter(i => i.status === 'change' && selected.has(i.id)).map(i => i.id);
export const changeIds = (items: { id: number; status: string }[]): number[] => items.filter(i => i.status === 'change').map(i => i.id);
export function summarize(detail: RenameCommandDetail): string {
  const c = detail.status;
  const counts = `${detail.items_renamed} renamed, ${detail.items_unchanged} unchanged, ${detail.items_failed} failed${detail.bounded ? '; more changes remain, run the command again' : ''}`;
  if (c === 'queued' || c === 'retry_wait') return c === 'queued' ? 'Rename queued.' : `Rename waiting to retry (${detail.error_code ?? 'busy'}).`;
  if (c === 'running') return `Renaming… ${counts}`;
  if (c === 'cancelled') return `Rename cancelled. ${counts}`;
  if (c === 'failed') return `Rename failed (${detail.error_code ?? 'error'}). ${counts}`;
  return `Rename finished. ${counts}`;
}

export type RunOutcome = { ok: true; detail: RenameCommandDetail } | { ok: false; error: string; commandId?: string };
export const POLL_MS = 750;
export const POLL_LIMIT = 800; // about ten minutes; the command itself keeps running if exceeded
/**
 * Submits one rename command and polls it to a terminal state. A lost POST response is not
 * retried (the caller must read command history before trying again); polling errors are
 * surfaced with the command ID so the user can check the result.
 */
export async function runRename(
  input: RenameInput,
  onUpdate: (c: RenameCommandDetail | RenameCommand) => void,
  alive: () => boolean,
  sleep: (ms: number) => Promise<void> = ms => new Promise(r => setTimeout(r, ms)),
): Promise<RunOutcome> {
  const created = await createRenameCommand(input);
  if (!created.ok) return { ok: false, error: created.error };
  onUpdate(created.data);
  const id = created.data.id;
  for (let i = 0; i < POLL_LIMIT && alive(); i++) {
    const read = await getRenameCommand(id);
    if (!alive()) break;
    if (!read.ok) return { ok: false, error: read.error, commandId: id };
    onUpdate(read.data);
    if (isTerminal(read.data)) return { ok: true, detail: read.data };
    await sleep(POLL_MS);
  }
  return { ok: false, error: alive() ? 'Rename is still running; check its status later.' : 'Stopped waiting for the rename.', commandId: id };
}
export const cancelRename = cancelRenameCommand;
