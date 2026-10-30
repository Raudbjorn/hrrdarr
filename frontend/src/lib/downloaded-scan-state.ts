// Pure state rules for the downloaded-folder scan panel. Nothing here performs I/O.
// Persisted state is identity + immutable request only: a saved receipt never grants an action.
import type { Result } from './api';
import { isDownloadClient } from './provider-draft.ts';
import type {
  CommandPriority, CommandStatus, DownloadedScanCommand, DownloadedScanInput, MediaDomain, Provider,
  ScanChild, ScanChildResumeInput, ScanCleanup, ScanCleanupPhase, ScanCleanupResumeInput, ScanCleanupState, ScanCoverage,
  ScanCoverageState, ScanMode, ScanPhase, ScanSource, ScanTracked, ScanTracking,
} from './api.generated';

export const SCAN_PENDING_KEY = 'hrrdarr.downloaded-scan.pending.v1';
/** Unconfirmed admissions retained per browser; a full list blocks new scans. */
export const SCAN_PENDING_LIMIT = 20;
export const SCAN_PENDING_BYTES = 128 * 1024;
export const SCAN_POLL_MS = 5_000;
export const SCAN_MAX_BACKOFF_MS = 60_000;
export const SCAN_PARENT_PAGE = 10;
export const SCAN_CHILD_PAGE = 25;
/** One bounded owner read per completed parent; larger imports refresh conservatively. */
export const SCAN_OWNER_READ_LIMIT = 100;
const PATH_MAX_BYTES = 4096;
const MAX_BACKOFF_STEPS = 4;

const encoder = new TextEncoder();
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
/** Same grammar as the server: a lowercase 40- or 64-character hexadecimal download ID. */
const REMOTE = /^(?:[0-9a-f]{40}|[0-9a-f]{64})$/;
const MODES: readonly ScanMode[] = ['auto', 'move', 'copy'];
const PRIORITIES: readonly CommandPriority[] = ['normal', 'high'];
const STATUSES: readonly CommandStatus[] = ['queued', 'running', 'retry_wait', 'succeeded', 'failed', 'cancelled'];
const PHASES: readonly ScanPhase[] = ['queued', 'inventory', 'waiting_predecessor', 'awaiting_import', 'importing', 'recovery_required', 'cleanup', 'settled'];
const TERMINAL: readonly CommandStatus[] = ['succeeded', 'failed', 'cancelled'];
const TRACKING: readonly ScanTracking[] = ['untracked', 'pending', 'unknown_fallback', 'receipt'];
const SOURCES: readonly ScanSource[] = ['found', 'missing'];
const COVERAGE_STATES: readonly ScanCoverageState[] = ['untracked', 'pending', 'complete', 'incomplete'];
const CLEANUP_STATES: readonly ScanCleanupState[] = ['not_needed', 'pending', 'complete', 'retained'];
const CLEANUP_PHASES: readonly ScanCleanupPhase[] = ['observed', 'remove_intent', 'settled'];

/** `providerId`/`remoteId` are both empty for an untracked scan, or both set for a tracked one. */
export type ScanForm = { path: string; mode: ScanMode; priority: CommandPriority; providerId?: string; remoteId?: string };
type ValidForm = { path: string; mode: ScanMode; priority: CommandPriority; tracked: ScanTracked | null };
export type PendingScan = Readonly<{ domain: MediaDomain; input: Readonly<DownloadedScanInput> }>;
export type AdmissionOutcome = 'admitted' | 'rejected' | 'uncertain' | 'mismatch';
export type ReadbackOutcome = 'admitted' | 'absent' | 'mismatch' | 'unknown';
type Failure = { ok: false; error: string; code?: string; status?: number };

const fail = <T>(error: string): Result<T> => ({ ok: false, error });
const good = <T>(data: T): Result<T> => ({ ok: true, data });
const object = (v: unknown): v is Record<string, unknown> => !!v && typeof v === 'object' && !Array.isArray(v);
const exactKeys = (v: Record<string, unknown>, names: readonly string[]) =>
  Object.keys(v).length === names.length && names.every(name => Object.hasOwn(v, name));
const member = <T extends string>(v: unknown, values: readonly T[]): v is T => typeof v === 'string' && (values as readonly string[]).includes(v);
const count = (v: unknown) => typeof v === 'number' && Number.isSafeInteger(v) && v >= 0;

export const scanUuid = (v: unknown): v is string => typeof v === 'string' && UUID.test(v);
export const isScanDomain = (v: unknown): v is MediaDomain => v === 'tv' || v === 'movies';
export const isTerminalScan = (status: CommandStatus) => TERMINAL.includes(status);

/** Mirrors the server: absolute, normalized UTF-8, no NUL, no trailing slash, not `/`, ≤4096 bytes. */
export function scanPathProblem(path: unknown): string | null {
  if (typeof path !== 'string' || path === '') return 'Enter the server path of a downloaded file or folder.';
  // With the u flag this matches only unpaired surrogates, which cannot be encoded as UTF-8.
  if (/[\uD800-\uDFFF]/u.test(path)) return 'The path contains invalid text.';
  if (!path.startsWith('/')) return 'Use an absolute path that starts with /.';
  if (path === '/') return 'The filesystem root cannot be scanned.';
  if (path.includes('\0')) return 'The path cannot contain a NUL character.';
  if (path.endsWith('/')) return 'Remove the trailing slash.';
  if (encoder.encode(path).length > PATH_MAX_BYTES) return 'The path exceeds 4096 bytes.';
  if (!path.slice(1).split('/').every(part => part !== '' && part !== '.' && part !== '..'))
    return 'Use a normalized path without empty, . or .. components.';
  return null;
}

/** A tracked identity is optional; when present it is exact: a provider ID and a download ID. */
export function trackedProblem(providerId: unknown, remoteId: unknown): string | null {
  const provider = typeof providerId === 'string' ? providerId : '', remote = typeof remoteId === 'string' ? remoteId : '';
  if (provider === '' && remote === '') return null;
  if (provider === '') return 'Choose the download client that tracks this download, or clear the download ID.';
  if (!scanUuid(provider)) return 'Choose a listed download client.';
  if (remote === '') return 'Enter the download ID (torrent hash) reported by the download client, or choose Not tracked.';
  if (!REMOTE.test(remote)) return 'The download ID must be a lowercase 40- or 64-character hexadecimal hash.';
  return null;
}

export function validateScanForm(form: ScanForm): Result<ValidForm> {
  if (!object(form)) return fail('Invalid scan form.');
  const problem = scanPathProblem(form.path);
  if (problem) return fail(problem);
  if (!member(form.mode, MODES)) return fail('Choose Auto, Move or Copy.');
  if (!member(form.priority, PRIORITIES)) return fail('Choose Normal or High priority.');
  const trackedIssue = trackedProblem(form.providerId ?? '', form.remoteId ?? '');
  if (trackedIssue) return fail(trackedIssue);
  const tracked = form.providerId ? { provider_id: form.providerId, remote_id: form.remoteId as string } : null;
  return good({ path: form.path, mode: form.mode, priority: form.priority, tracked });
}

/**
 * Captures the immutable request under a client-generated identity before any POST. An explicit
 * successor names the settled scan it continues; it always needs a tracked identity.
 */
export function createPendingScan(domain: MediaDomain, id: string, form: ScanForm, supersedes: string | null = null): Result<PendingScan> {
  if (!isScanDomain(domain)) return fail('Choose TV or movies.');
  if (!scanUuid(id)) return fail('Invalid scan request ID.');
  const valid = validateScanForm(form);
  if (!valid.ok) return valid;
  if (supersedes !== null && (!scanUuid(supersedes) || supersedes === id || !valid.data.tracked))
    return fail('A scan of remaining files needs the finished tracked scan it continues.');
  const { tracked, ...plain } = valid.data;
  const input: DownloadedScanInput = { id, ...plain, ...(tracked ? { tracked: Object.freeze({ ...tracked }) } : {}), ...(supersedes ? { supersedes_command_id: supersedes } : {}) };
  return good(Object.freeze({ domain, input: Object.freeze(input) }));
}

const INPUT_KEYS = ['id', 'path', 'mode', 'priority'] as const;
function pendingEntry(v: unknown): Result<PendingScan> {
  if (!object(v) || !exactKeys(v, ['domain', 'input']) || !object(v.input)) return fail('Invalid saved scan request.');
  const input = v.input as Record<string, unknown>;
  const keys = [...INPUT_KEYS, ...(Object.hasOwn(input, 'tracked') ? ['tracked'] : []), ...(Object.hasOwn(input, 'supersedes_command_id') ? ['supersedes_command_id'] : [])];
  if (!exactKeys(input, keys)) return fail('Invalid saved scan request.');
  const tracked = input.tracked;
  if (tracked !== undefined && (!object(tracked) || !exactKeys(tracked, ['provider_id', 'remote_id']))) return fail('Invalid saved scan request.');
  const supersedes = input.supersedes_command_id;
  if (supersedes !== undefined && typeof supersedes !== 'string') return fail('Invalid saved scan request.');
  return createPendingScan(v.domain as MediaDomain, input.id as string, {
    path: input.path as string, mode: input.mode as ScanMode, priority: input.priority as CommandPriority,
    providerId: tracked ? (tracked as Record<string, unknown>).provider_id as string : '',
    remoteId: tracked ? (tracked as Record<string, unknown>).remote_id as string : '',
  }, (supersedes as string | undefined) ?? null);
}

function pendingList(v: unknown): Result<PendingScan[]> {
  if (!object(v) || !exactKeys(v, ['version', 'entries']) || v.version !== 1 || !Array.isArray(v.entries) || v.entries.length > SCAN_PENDING_LIMIT)
    return fail('Invalid saved scan request list or size.');
  const entries: PendingScan[] = [];
  for (const raw of v.entries) {
    const parsed = pendingEntry(raw);
    if (!parsed.ok) return parsed;
    if (entries.some(e => e.input.id === parsed.data.input.id)) return fail('Duplicate saved scan request ID.');
    entries.push(parsed.data);
  }
  return good(entries);
}

export function decodePendingScans(raw: string | null): Result<PendingScan[]> {
  if (raw === null) return good([]);
  if (typeof raw !== 'string' || raw.length > SCAN_PENDING_BYTES || encoder.encode(raw).length > SCAN_PENDING_BYTES)
    return fail('Saved scan requests exceed 128 KiB.');
  try { return pendingList(JSON.parse(raw)); } catch { return fail('Saved scan requests are not valid JSON.'); }
}

export function encodePendingScans(entries: readonly PendingScan[]): Result<string> {
  const checked = pendingList({ version: 1, entries });
  if (!checked.ok) return checked;
  const raw = JSON.stringify({ version: 1, entries: checked.data });
  return encoder.encode(raw).length <= SCAN_PENDING_BYTES ? good(raw) : fail('Saved scan requests exceed 128 KiB.');
}

export function addPendingScan(entries: readonly PendingScan[], entry: PendingScan): Result<PendingScan[]> {
  if (entries.length >= SCAN_PENDING_LIMIT)
    return fail(`${SCAN_PENDING_LIMIT} scan requests still await confirmation. Check or discard them before starting another scan.`);
  if (entries.some(e => e.input.id === entry.input.id)) return fail('This scan request ID is already retained.');
  return good([...entries, entry]);
}

export const removePendingScan = (entries: readonly PendingScan[], id: string) => entries.filter(e => e.input.id !== id);

const sameTracked = (a: unknown, b: ScanTracked | undefined) => b === undefined
  ? a === null
  : object(a) && a.provider_id === b.provider_id && a.remote_id === b.remote_id;
/** A server row satisfies a retained request only when every immutable input field matches. */
export function commandMatches(entry: PendingScan, command: unknown): boolean {
  return object(command) && command.id === entry.input.id && command.media_type === entry.domain
    && command.path === entry.input.path && command.mode === entry.input.mode
    && command.priority === entry.input.priority && sameTracked(command.tracked, entry.input.tracked)
    && (command.supersedes_command_id ?? null) === (entry.input.supersedes_command_id ?? null);
}

/** 409 codes that reject the request inside the admission transaction: nothing was created. */
const REJECTING_CONFLICTS = new Set([
  'downloaded_scan_provider_unknown', 'downloaded_scan_provider_not_client', 'downloaded_scan_successor_invalid', 'downloaded_scan_successor_not_settled',
  'downloaded_scan_successor_not_incomplete', 'downloaded_scan_successor_conflict', 'downloaded_scan_successor_receipt_unavailable',
]);
/** Only explicit request rejections settle an admission; anything else may have committed. */
export function classifyAdmission(entry: PendingScan, result: Result<DownloadedScanCommand>): AdmissionOutcome {
  if (result.ok) return commandMatches(entry, result.data) && validScanCommand(result.data, entry.domain) ? 'admitted' : 'mismatch';
  const failure = result as Failure;
  if (failure.status === 400 || failure.status === 413 || failure.status === 429) return 'rejected';
  if (failure.status === 409 && REJECTING_CONFLICTS.has(failure.code ?? '')) return 'rejected';
  // 409 downloaded_scan_conflict: this ID already names a different request. Keep the identity.
  if (failure.status === 409) return 'mismatch';
  return 'uncertain';
}

/** GET of the SAME id: 404 is current absence only, never proof the POST will not land. */
export function classifyReadback(entry: PendingScan, result: Result<DownloadedScanCommand>): ReadbackOutcome {
  if (result.ok) return commandMatches(entry, result.data) && validScanCommand(result.data, entry.domain) ? 'admitted' : 'mismatch';
  return (result as Failure).status === 404 ? 'absent' : 'unknown';
}

function validCounts(v: unknown): boolean {
  return object(v) && ['entries', 'ready', 'importing', 'imported', 'already_imported', 'rejected', 'failed', 'cancelled', 'recovery_required']
    .every(name => count(v[name]));
}

/** The coverage the panel renders; a partial row must not be accepted as a parent. */
function validCoverage(v: unknown): boolean {
  return object(v) && member(v.state, COVERAGE_STATES) && count(v.expected) && count(v.covered) && count(v.missing);
}
/** The cleanup state the panel renders; every consumed field is checked before the row is used. */
function validCleanup(v: unknown): boolean {
  return object(v) && member(v.state, CLEANUP_STATES) && (v.phase === null || member(v.phase, CLEANUP_PHASES))
    && (v.reason === null || typeof v.reason === 'string') && count(v.removed)
    && (v.error_code === null || typeof v.error_code === 'string')
    && count(v.resume_generation) && typeof v.resume_requested === 'boolean' && typeof v.recovery_required === 'boolean';
}
/** Structural check of a parent for the requested domain; rejects cross-domain rows. */
export function validScanCommand(v: unknown, domain: MediaDomain): v is DownloadedScanCommand {
  return object(v) && scanUuid(v.id) && v.media_type === domain && typeof v.path === 'string'
    && member(v.mode, MODES) && member(v.priority, PRIORITIES) && member(v.status, STATUSES) && member(v.phase, PHASES)
    && (v.outcome === null || v.outcome === 'success' || v.outcome === 'unsuccessful')
    && typeof v.cancel_requested === 'boolean' && count(v.inventory_revision) && count(v.attempts) && validCounts(v.counts)
    && (v.phase === 'settled') === isTerminalScan(v.status as CommandStatus)
    && typeof v.successor_allowed === 'boolean' && (v.supersedes_command_id === null || scanUuid(v.supersedes_command_id))
    && (v.successor_command_id === null || scanUuid(v.successor_command_id))
    && member(v.tracking, TRACKING) && (v.source === null || member(v.source, SOURCES))
    && (v.error_code === null || typeof v.error_code === 'string')
    && (v.completed_at === null || count(v.completed_at)) && count(v.created_at) && count(v.next_attempt_at)
    && (v.receipt_id === null || scanUuid(v.receipt_id))
    && (v.predecessor_operation_id === null || scanUuid(v.predecessor_operation_id))
    && validCoverage(v.coverage) && validCleanup(v.cleanup)
    && (v.tracked === null || (object(v.tracked) && scanUuid(v.tracked.provider_id) && typeof v.tracked.remote_id === 'string'));
}

export function validScanChild(v: unknown): v is ScanChild {
  return object(v) && scanUuid(v.id) && typeof v.relative_path === 'string' && typeof v.disposition === 'string'
    && Array.isArray(v.reasons) && v.reasons.every(r => typeof r === 'string')
    && typeof v.recovery_required === 'boolean' && typeof v.resume_requested === 'boolean' && count(v.resume_generation)
    && (v.operation_id === null || scanUuid(v.operation_id));
}

export function pollDelay(failures: number): number {
  const steps = Math.min(Math.max(0, Math.trunc(failures) || 0), MAX_BACKOFF_STEPS);
  return Math.min(SCAN_POLL_MS * 2 ** steps, SCAN_MAX_BACKOFF_MS);
}

export type PollInput = {
  mounted: boolean; active: boolean; hidden: boolean;
  /** Every parent currently rendered (list page and selected detail). */
  visible: readonly Pick<DownloadedScanCommand, 'status'>[];
  /** No successful read yet (initial load or after an error). */
  unread: boolean;
};
/** Polls only while something visible can still change. Hidden or inactive panels never poll. */
export function shouldPoll(input: PollInput): boolean {
  if (!input.mounted || !input.active || input.hidden) return false;
  return input.unread || input.visible.some(parent => !isTerminalScan(parent.status));
}

/** Cancellation is idempotent; it is offered while the parent can still change. */
export const canCancelScan = (parent: DownloadedScanCommand) => !isTerminalScan(parent.status) && !parent.cancel_requested;

/** Builds the exact explicit-recovery body, or null when the API does not require recovery. */
export function resumeRequest(parent: DownloadedScanCommand, child: ScanChild): ScanChildResumeInput | null {
  if (parent.status !== 'running' || parent.inventory_revision !== 1) return null;
  if (child.disposition !== 'failed' || !child.recovery_required || child.resume_requested || !scanUuid(child.operation_id)) return null;
  if (!Number.isSafeInteger(child.resume_generation) || child.resume_generation < 0) return null;
  return { operation_id: child.operation_id, inventory_revision: parent.inventory_revision, expected_resume_generation: child.resume_generation };
}

export function resumeFailureMessage(result: Failure): string {
  switch (result.code) {
    case 'downloaded_scan_resume_generation':
      return 'This file changed since it was read; another recovery may already have been accepted. Refresh and review it before trying again.';
    case 'downloaded_scan_resume_not_required': return 'This file no longer needs recovery. Refresh to read its current state.';
    case 'downloaded_scan_resume_conflict': return 'The scan or operation no longer matches this recovery request. Refresh before trying again.';
    case 'downloaded_scan_cancelled': return 'The scan was cancelled before this file changed anything, so it cannot be resumed.';
    case 'import_busy': return 'Another import is running. Try recovery again after it finishes.';
    default: return result.error;
  }
}

export function cleanupResumeFailureMessage(result: Failure): string {
  switch (result.code) {
    case 'downloaded_scan_cleanup_resume_generation':
      return 'The cleanup changed since it was read; another recovery may already have been accepted. Refresh before trying again.';
    case 'downloaded_scan_cleanup_resume_not_required': return 'The source-folder cleanup no longer needs recovery. Refresh to read its state.';
    case 'downloaded_scan_cleanup_resume_conflict': return 'The scan no longer matches this cleanup request. Refresh before trying again.';
    case 'import_busy': return 'An import is running. Try the cleanup recovery again after it finishes.';
    default: return result.error;
  }
}

/** Library owners whose files changed; null means the bounded read could not prove the set. */
export function completionOwners(domain: MediaDomain, imported: readonly ScanChild[], total: number): number[] | null {
  if (total > imported.length) return null;
  const owners = new Set<number>();
  for (const child of imported) {
    const owner = domain === 'tv' ? child.series_id : child.movie_id;
    if (child.disposition !== 'imported' || owner === null || !Number.isSafeInteger(owner)) return null;
    owners.add(owner);
  }
  return [...owners].sort((a, b) => a - b);
}

const PHASE_TEXT: Record<ScanPhase, string> = {
  queued: 'Queued', inventory: 'Discovering files', waiting_predecessor: 'Waiting for the tracked download\'s own import',
  awaiting_import: 'Waiting to import', importing: 'Importing', recovery_required: 'Needs explicit recovery',
  cleanup: 'Cleaning up emptied source folders', settled: 'Finished',
};

const CLEANUP_REASON: Record<string, string> = {
  unimported_media: 'media files that were not imported remain', unknown_leftovers: 'other files or folders remain',
  archive_leftovers: 'archive files remain', identity_changed: 'the folder changed or was replaced',
  path_denied: 'access was denied', path_invalid: 'the path is no longer a folder', cleanup_io: 'a file system error occurred',
  cancelled: 'the scan was cancelled',
};
/** Plain-language source-folder cleanup state; static reason codes only. */
export function describeCleanup(cleanup: ScanCleanup): string {
  const removed = cleanup.removed === 1 ? '1 emptied folder' : `${cleanup.removed} emptied folders`;
  switch (cleanup.state) {
    case 'not_needed': return 'Not needed (nothing was moved out of a folder).';
    case 'complete': return `Removed the emptied source folder (${removed}).`;
    case 'retained': return `Kept the source folder: ${CLEANUP_REASON[cleanup.reason ?? ''] ?? 'leftovers remain'} (removed ${removed}).`;
    case 'pending':
      if (cleanup.recovery_required) return `Removal was interrupted${cleanup.error_code ? ` (${cleanup.error_code})` : ''}; it needs an explicit cleanup resume.`;
      return cleanup.phase === 'remove_intent' ? 'Removing emptied folders.' : 'Checking the source folder before removing emptied folders.';
  }
}

/** Plain-language typed coverage of a tracked receipt. */
export function describeCoverage(coverage: ScanCoverage): string | null {
  if (coverage.state === 'untracked') return null;
  const tally = `${coverage.covered} of ${coverage.expected} covered`;
  if (coverage.state === 'pending') return coverage.expected ? `In progress: ${tally}.` : 'Not yet published.';
  return coverage.state === 'complete' ? `Complete: ${tally}.` : `Incomplete: ${tally}; the download stays blocked.`;
}

/** Builds the exact explicit cleanup-resume body, or null when cleanup is not awaiting recovery. */
export function cleanupResumeRequest(parent: DownloadedScanCommand): ScanCleanupResumeInput | null {
  if (parent.status !== 'running' || parent.inventory_revision !== 1 || !parent.cleanup.recovery_required) return null;
  if (!Number.isSafeInteger(parent.cleanup.resume_generation) || parent.cleanup.resume_generation < 0) return null;
  return { inventory_revision: 1, expected_cleanup_resume_generation: parent.cleanup.resume_generation };
}
export const scanPhaseText = (phase: ScanPhase) => PHASE_TEXT[phase] ?? phase;

export function describeScan(parent: DownloadedScanCommand): string {
  const c = parent.counts;
  const tally = `${c.imported} imported, ${c.rejected} rejected, ${c.already_imported} already imported, ${c.failed} failed`;
  if (parent.status === 'succeeded') {
    if (parent.source === 'missing') return 'Finished: the path does not exist. Nothing was imported.';
    return parent.outcome === 'success' ? `Finished: ${tally}.` : `Finished without importing anything (${tally}).`;
  }
  if (parent.status === 'failed') return `Failed${parent.error_code ? ` (${parent.error_code})` : ''}: ${tally}.`;
  if (parent.status === 'cancelled') return `Cancelled: ${tally}.`;
  if (parent.status === 'retry_wait') return `Waiting to retry${parent.error_code ? ` after ${parent.error_code}` : ''} (attempt ${parent.attempts} of 3).`;
  const stopping = parent.cancel_requested ? ' Stopping after the current file.' : '';
  if (parent.phase === 'recovery_required' && parent.cleanup?.recovery_required) return `Source-folder cleanup was interrupted and needs an explicit cleanup resume.${stopping}`;
  if (parent.phase === 'recovery_required') return `${c.recovery_required} file${c.recovery_required === 1 ? '' : 's'} need explicit recovery.${stopping}`;
  return `${PHASE_TEXT[parent.phase] ?? parent.phase}: ${tally}.${stopping}`;
}

/**
 * The reviewed "Scan remaining files" request for a settled tracked scan with incomplete
 * coverage: a NEW identity with the same path, mode, priority and tracked receipt, explicitly
 * naming the settled scan. Never reopens the original inventory. Null when not offered.
 */
export function successorRequest(parent: DownloadedScanCommand, id: string): Result<PendingScan> | null {
  if (!parent.successor_allowed || !isTerminalScan(parent.status) || parent.coverage.state !== 'incomplete' || !parent.tracked) return null;
  return createPendingScan(parent.media_type, id, {
    path: parent.path, mode: parent.mode, priority: parent.priority,
    providerId: parent.tracked.provider_id, remoteId: parent.tracked.remote_id,
  }, parent.id);
}

/** Plain-language confirmation shown before a successor is sent. */
export function successorConfirmation(parent: DownloadedScanCommand): string {
  return `Start a new scan of ${parent.path} for the files this tracked download is still missing (${parent.coverage.missing} of ${parent.coverage.expected})? `
    + 'The finished scan stays unchanged. Files it already imported are not imported again, and the download stays blocked unless the new scan covers everything.';
}

export function admissionFailureMessage(result: Failure): string {
  switch (result.code) {
    case 'downloaded_scan_successor_invalid': return 'The finished scan does not match this request (library type, path or tracked download). Refresh before trying again.';
    case 'downloaded_scan_successor_not_settled': return 'The previous scan has not finished, or still has an unresolved import or cleanup. Recover it first.';
    case 'downloaded_scan_successor_not_incomplete': return 'The previous scan is not missing any tracked files, so there is nothing remaining to scan.';
    case 'downloaded_scan_successor_conflict': return 'Another scan already continues this tracked download. Open the newest scan instead.';
    case 'downloaded_scan_successor_receipt_unavailable': return 'The tracked download is no longer available from an enabled download client for this library type.';
    case 'downloaded_scan_provider_unknown': return 'The chosen download client is not configured for this library type.';
    case 'downloaded_scan_provider_not_client': return 'A tracked scan needs a download client; the chosen provider is an indexer.';
    default: return result.error;
  }
}

/** Download clients (not indexers) that are configured for the given library type. */
export function trackingProviders(providers: readonly Provider[], domain: MediaDomain): Provider[] {
  return providers.filter(p => isDownloadClient(p) && p.settings[domain] !== null && scanUuid(p.id));
}
