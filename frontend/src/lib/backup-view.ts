// Pure helpers for System → Backup. No DOM or network access; the server remains the authority.
import type { BackupCommand, BackupFile, BackupSettingsUpdate, BackupType, PendingRestore } from './api.generated';

export const MIN_INTERVAL_DAYS = 1;
export const MAX_INTERVAL_DAYS = 7;
export const MIN_RETENTION_DAYS = 1;
export const MAX_RETENTION_DAYS = 90;
/** Stored ZIP without ZIP64: the server refuses larger archives. */
export const MAX_UPLOAD_BYTES = 4294967295;
export const BACKUP_PAGE_SIZE = 10;
const MAX_PATH_BYTES = 4096;
const ID = /^[0-9a-f]{32}$/;

export type BackupSettingsDraft = { folder: string; interval_days: number | undefined; retention_days: number | undefined };
type Check<T> = { ok: true; data: T } | { ok: false; error: string };

export function validBackupId(id: unknown): id is string {
  return typeof id === 'string' && ID.test(id);
}

/** Mirrors server validation: blank folder selects the default; otherwise absolute, no traversal. */
export function backupSettingsUpdate(revision: number, draft: BackupSettingsDraft): Check<BackupSettingsUpdate> {
  if (!Number.isSafeInteger(revision) || revision < 1) return { ok: false, error: 'Reload the saved backup settings before saving.' };
  const interval = draft.interval_days, retention = draft.retention_days;
  if (interval === undefined || !Number.isInteger(interval) || interval < MIN_INTERVAL_DAYS || interval > MAX_INTERVAL_DAYS)
    return { ok: false, error: `Backup interval must be a whole number of days from ${MIN_INTERVAL_DAYS} to ${MAX_INTERVAL_DAYS}.` };
  if (retention === undefined || !Number.isInteger(retention) || retention < MIN_RETENTION_DAYS || retention > MAX_RETENTION_DAYS)
    return { ok: false, error: `Backup retention must be a whole number of days from ${MIN_RETENTION_DAYS} to ${MAX_RETENTION_DAYS}.` };
  const raw = typeof draft.folder === 'string' ? draft.folder : '';
  if (raw.trim() === '') return { ok: true, data: { revision, folder: null, interval_days: interval, retention_days: retention } };
  if (new TextEncoder().encode(raw).length > MAX_PATH_BYTES || !raw.startsWith('/') || raw.includes('\\') ||
      /[\u0000-\u001f\u007f]/.test(raw) || raw.split('/').some(s => s === '.' || s === '..'))
    return { ok: false, error: 'Backup folder must be an absolute folder path without “.” or “..” segments or control characters.' };
  const folder = `/${raw.split('/').filter(Boolean).join('/')}`;
  if (folder === '/') return { ok: false, error: 'The filesystem root cannot be the backup folder.' };
  return { ok: true, data: { revision, folder, interval_days: interval, retention_days: retention } };
}

export function formatBytes(bytes: number): string {
  if (!Number.isSafeInteger(bytes) || bytes < 0) return 'unknown size';
  if (bytes < 1024) return `${bytes} B`;
  const units = ['KiB', 'MiB', 'GiB', 'TiB'];
  let value = bytes / 1024, unit = 0;
  while (value >= 1024 && unit < units.length - 1) { value /= 1024; unit++; }
  return `${value.toFixed(value < 10 ? 1 : 0)} ${units[unit]}`;
}

export function formatTime(unixSeconds: number | null | undefined): string {
  if (typeof unixSeconds !== 'number' || !Number.isSafeInteger(unixSeconds) || unixSeconds < 0) return 'unknown time';
  return new Date(unixSeconds * 1000).toISOString().replace('T', ' ').replace(/\.\d{3}Z$/, ' UTC');
}

export function typeLabel(type: BackupType): string {
  return type === 'scheduled' ? 'Scheduled' : type === 'manual' ? 'Manual' : type === 'update' ? 'Update (before migration)' : 'Unknown';
}

const FAILURES: Record<string, string> = {
  interrupted: 'the backup was interrupted (for example by a restart); nothing was published',
  storage_error: 'database storage was unavailable',
  backup_local_only: 'backups need a locally owned database',
  backup_folder_unavailable: 'the backup folder could not be created or written',
  backup_folder_unsafe: 'the backup folder overlaps a library folder or recycle bin',
  backup_too_large: 'the database exceeds the supported backup size',
  backup_timeout: 'the backup exceeded its time budget',
  snapshot_failed: 'the database snapshot could not be taken',
  snapshot_invalid: 'the snapshot failed its integrity check and was discarded',
  archive_failed: 'the archive could not be written or verified',
  publish_failed: 'the archive could not be published',
};

export function describeBackupCommand(command: BackupCommand): string {
  const kind = command.backup_type === 'scheduled' ? 'Scheduled backup' : 'Manual backup';
  switch (command.status) {
    case 'queued': return `${kind} is queued.`;
    case 'running': return `${kind} is running.`;
    case 'succeeded': {
      const pruned = command.pruned > 0 ? ` Removed ${command.pruned} expired ${command.pruned === 1 ? 'backup' : 'backups'} of the same type.` : '';
      return `${kind} completed: ${command.backup_name ?? 'archive'} (${formatBytes(command.size_bytes ?? -1)}).${pruned}`;
    }
    case 'failed': return `${kind} failed: ${FAILURES[command.error_code ?? ''] ?? 'an unknown error occurred'}. Existing backups were not pruned.`;
    case 'cancelled': return `${kind} was cancelled.`;
    default: return `${kind} status is unknown.`;
  }
}

export const RESTORE_ERRORS: Record<string, string> = {
  backup_archive_invalid: 'The file is not a valid hrrdarr backup archive. Nothing was changed.',
  backup_checksum_mismatch: 'The backup database does not match its manifest checksum (corrupted or altered). Nothing was changed.',
  backup_schema_unsupported: 'The backup comes from a newer hrrdarr version than this one. Upgrade first. Nothing was changed.',
  backup_history_mismatch: 'The backup migration history does not match this version. Nothing was changed.',
  backup_integrity_failed: 'The backup database failed its integrity check. Nothing was changed.',
  backup_too_large: 'The backup archive is too large. Nothing was changed.',
  restore_already_pending: 'A restore is already pending. Cancel it before staging another.',
  backup_upload_unsupported: 'Upload a .zip backup archive.',
};

export function restoreErrorMessage(code: string | undefined, fallback: string): string {
  return (code && RESTORE_ERRORS[code]) || fallback;
}

/** Client-side pre-check only; the server validates the archive completely. */
export function checkUploadFile(file: { name: string; size: number } | null | undefined): Check<true> {
  if (!file) return { ok: false, error: 'Choose a backup archive (.zip) to restore.' };
  if (!/\.zip$/i.test(file.name)) return { ok: false, error: 'Backup archives are .zip files.' };
  if (!Number.isSafeInteger(file.size) || file.size <= 0) return { ok: false, error: 'The chosen file is empty.' };
  if (file.size > MAX_UPLOAD_BYTES) return { ok: false, error: 'The chosen file exceeds the 4 GiB backup limit.' };
  return { ok: true, data: true };
}

export function describePending(pending: PendingRestore): string {
  const from = pending.source === 'upload' ? 'an uploaded archive' : `backup ${pending.backup_name ?? ''}`.trim();
  return `Restore of ${from} (${typeLabel(pending.backup_type).toLowerCase()}, created ${formatTime(pending.backup_created_at)}, schema ${pending.schema_version}) is validated and staged. It replaces the database when hrrdarr next restarts; the current database is kept beside it as a recoverable original.`;
}

export function pageSummary(offset: number, count: number, total: number): string {
  if (total === 0 || count === 0) return 'No backups yet.';
  return `Showing ${offset + 1}–${offset + count} of ${total} ${total === 1 ? 'backup' : 'backups'}.`;
}

export function downloadPath(backup: BackupFile): string | null {
  return validBackupId(backup.id) ? `/api/v1/system/backups/${backup.id}/download` : null;
}
