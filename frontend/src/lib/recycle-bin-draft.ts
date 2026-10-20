import type { RecycleBinSettings, RecycleBinSettingsUpdate, FileDeletionMode, RecycleBinCleanupCommand, FileDeletion } from './api.generated';
import type { Result } from './api';

export const MAX_CLEANUP_DAYS = 36500;
export const MAX_RECYCLE_PATH_BYTES = 4096;
/** Matches the server's bulk deletion bound. */
export const MAX_DELETE_FILES = 50;
export type RecycleBinDraft = { path: string; cleanup_days: number | undefined };

const CONTROL = /[\u0000-\u001f\u007f-\u009f]/;

/** Client-side mirror of server validation; the server normalizes and remains authoritative. */
export function recycleBinUpdate(revision: number, draft: RecycleBinDraft): Result<RecycleBinSettingsUpdate> {
  const days = draft.cleanup_days;
  if (days === undefined || !Number.isSafeInteger(days) || days < 0 || days > MAX_CLEANUP_DAYS)
    return { ok: false, error: `Cleanup days must be a whole number from 0 to ${MAX_CLEANUP_DAYS}. Use 0 to disable automatic cleanup.` };
  const raw = draft.path;
  if (raw.trim() === '') return { ok: true, data: { revision, path: null, cleanup_days: days } };
  if (new TextEncoder().encode(raw).length > MAX_RECYCLE_PATH_BYTES) return { ok: false, error: `The recycle bin path must fit within ${MAX_RECYCLE_PATH_BYTES} UTF-8 bytes.` };
  if (!raw.startsWith('/') || raw.includes('\\') || CONTROL.test(raw) || raw.split('/').some(part => part === '.' || part === '..'))
    return { ok: false, error: 'The recycle bin must be an absolute folder path (starting with /) without . or .. segments, backslashes or control characters.' };
  if (raw.split('/').every(part => part === '')) return { ok: false, error: 'The filesystem root cannot be a recycle bin.' };
  return { ok: true, data: { revision, path: raw, cleanup_days: days } };
}

export type DeletionPlan = { mode: FileDeletionMode; title: string; detail: string; confirm: string };
/** The confirmation states exactly what the confirmed request will do under current settings. */
export function deletionPlan(settings: RecycleBinSettings, fileIds: number[]): DeletionPlan {
  const subject = fileIds.length === 1 ? `file ${fileIds[0]}` : `${fileIds.length} files`;
  if (settings.path !== null) {
    const retention = settings.cleanup_days === 0
      ? 'Automatic cleanup is disabled, so recycled files stay until you remove them.'
      : `Automatic cleanup permanently removes recycled files after ${settings.cleanup_days} day${settings.cleanup_days === 1 ? '' : 's'}.`;
    return {
      mode: 'recycle',
      title: `Move ${subject} to the recycle bin?`,
      detail: `The ${fileIds.length === 1 ? 'file is' : 'files are'} moved to ${settings.path}, inside a folder named after the series or movie, and removed from the library. Episodes or the movie lose their file. ${retention}`,
      confirm: `Move ${subject} to recycle bin`,
    };
  }
  return {
    mode: 'permanent',
    title: `Permanently delete ${subject}?`,
    detail: `No recycle bin is configured for this media type. The ${fileIds.length === 1 ? 'file is' : 'files are'} deleted from disk and cannot be restored. Configure a recycle bin under Media management to keep deleted files recoverable.`,
    confirm: `Permanently delete ${subject}`,
  };
}

/** Reports what the server says happened to each file, not what the confirmation planned. */
export function describeDeletions(deletions: FileDeletion[]): string {
  const files = (items: FileDeletion[]) => items.length === 1 ? `file ${items[0].file_id}` : `${items.length} files`;
  const recycled = deletions.filter(d => d.effect === 'recycled'), deleted = deletions.filter(d => d.effect === 'deleted'), missing = deletions.filter(d => d.effect === 'already_missing');
  const parts: string[] = [];
  if (recycled.length) parts.push(`Moved ${files(recycled)} to the recycle bin${recycled.length === 1 && recycled[0].recycle_path ? `: ${recycled[0].recycle_path}` : ''}.`);
  if (deleted.length) parts.push(`Permanently deleted ${files(deleted)}.`);
  if (missing.length) parts.push(`${missing.length === 1 ? `File ${missing[0].file_id} was` : `${missing.length} files were`} already missing from disk; the library record was removed.`);
  return parts.join(' ') || 'No files were deleted.';
}

export function describeDeletionFailure(failure: { error: string; code?: string; status?: number }): { message: string; reload: boolean } {
  switch (failure.code) {
    case 'recycle_bin_configured':
    case 'recycle_bin_not_configured':
    case 'recycle_bin_changed':
      return { message: 'Recycle bin settings changed since you confirmed. Nothing was deleted. Review the new confirmation before deleting.', reload: false };
    case 'recycle_bin_unavailable':
      return { message: 'The configured recycle bin folder does not exist. Nothing was deleted. Create the folder or change the setting; Health reports this check.', reload: false };
    case 'library_root_missing':
    case 'library_root_empty':
      return { message: 'The library root folder is missing or empty (possibly unmounted). Nothing was deleted.', reload: false };
    case 'file_reserved_by_import':
      return { message: 'An import or scan journal still references this file, so it cannot be deleted.', reload: false };
    case 'library_busy':
    case 'import_busy':
      return { message: 'An import, scan or other file operation is running. Nothing was deleted; retry when it finishes.', reload: false };
    case 'file_deletion_pending':
    case 'file_delete_recovery_pending':
    case 'recycle_source_cleanup_pending':
      return { message: `${failure.error} Reload the file list; recovery finishes or undoes the interrupted deletion on the next attempt or restart.`, reload: true };
    default:
      return { message: `${failure.error}${failure.status === undefined || failure.status >= 500 ? ' The outcome is uncertain; reload the file list to see what was deleted.' : ''}`, reload: failure.status === undefined || failure.status >= 500 };
  }
}

export function describeCleanup(command: RecycleBinCleanupCommand): string {
  const origin = command.origin === 'automatic' ? 'Scheduled' : 'Manual';
  if (command.status === 'queued' || command.status === 'retry_wait') return `${origin} cleanup is waiting to run.`;
  if (command.status === 'running') return `${origin} cleanup is running.`;
  if (command.status === 'cancelled') return `${origin} cleanup was cancelled.`;
  if (command.status === 'failed') return `${origin} cleanup failed (${command.error_code ?? 'unknown error'}). Nothing outside the recycle bin was touched.`;
  if (command.outcome === 'not_configured') return `${origin} cleanup found no configured recycle bin; nothing was removed.`;
  if (command.outcome === 'disabled') return `${origin} cleanup is disabled (0 days); nothing was removed.`;
  return `${origin} cleanup removed ${command.files_removed} file${command.files_removed === 1 ? '' : 's'} and ${command.folders_removed} empty folder${command.folders_removed === 1 ? '' : 's'}; kept ${command.entries_skipped} symlink or special entr${command.entries_skipped === 1 ? 'y' : 'ies'}${command.entries_failed ? `; ${command.entries_failed} could not be removed` : ''}${command.bounded ? '; stopped at its run limit, the rest is kept for the next run' : ''}.`;
}
