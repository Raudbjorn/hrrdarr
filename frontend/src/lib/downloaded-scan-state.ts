// Pure state rules for the downloaded-folder scan panel. Nothing here performs I/O.
// Persisted state is identity + immutable request only: a saved receipt never grants an action.
import type { Result } from './api';
import type {
  CommandPriority, CommandStatus, DownloadedScanCommand, DownloadedScanInput, MediaDomain,
  ScanChild, ScanChildResumeInput, ScanMode, ScanPhase,
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
const MODES: readonly ScanMode[] = ['auto', 'move', 'copy'];
const PRIORITIES: readonly CommandPriority[] = ['normal', 'high'];
const STATUSES: readonly CommandStatus[] = ['queued', 'running', 'retry_wait', 'succeeded', 'failed', 'cancelled'];
const PHASES: readonly ScanPhase[] = ['queued', 'inventory', 'awaiting_import', 'importing', 'recovery_required', 'settled'];
const TERMINAL: readonly CommandStatus[] = ['succeeded', 'failed', 'cancelled'];

export type ScanForm = { path: string; mode: ScanMode; priority: CommandPriority };
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

export function validateScanForm(form: ScanForm): Result<ScanForm> {
  if (!object(form)) return fail('Invalid scan form.');
  const problem = scanPathProblem(form.path);
  if (problem) return fail(problem);
  if (!member(form.mode, MODES)) return fail('Choose Auto, Move or Copy.');
  if (!member(form.priority, PRIORITIES)) return fail('Choose Normal or High priority.');
  return good({ path: form.path, mode: form.mode, priority: form.priority });
}

/** Captures the immutable request under a client-generated identity before any POST. */
export function createPendingScan(domain: MediaDomain, id: string, form: ScanForm): Result<PendingScan> {
  if (!isScanDomain(domain)) return fail('Choose TV or movies.');
  if (!scanUuid(id)) return fail('Invalid scan request ID.');
  const valid = validateScanForm(form);
  if (!valid.ok) return valid;
  return good(Object.freeze({ domain, input: Object.freeze({ id, ...valid.data }) }));
}

function pendingEntry(v: unknown): Result<PendingScan> {
  if (!object(v) || !exactKeys(v, ['domain', 'input']) || !object(v.input) || !exactKeys(v.input, ['id', 'path', 'mode', 'priority']))
    return fail('Invalid saved scan request.');
  const input = v.input as Record<string, unknown>;
  return createPendingScan(v.domain as MediaDomain, input.id as string,
    { path: input.path as string, mode: input.mode as ScanMode, priority: input.priority as CommandPriority });
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

/** A server row satisfies a retained request only when every immutable input field matches. */
export function commandMatches(entry: PendingScan, command: unknown): boolean {
  return object(command) && command.id === entry.input.id && command.media_type === entry.domain
    && command.path === entry.input.path && command.mode === entry.input.mode
    && command.priority === entry.input.priority && command.tracked === null;
}

/** Only explicit request rejections settle an admission; anything else may have committed. */
export function classifyAdmission(entry: PendingScan, result: Result<DownloadedScanCommand>): AdmissionOutcome {
  if (result.ok) return commandMatches(entry, result.data) && validScanCommand(result.data, entry.domain) ? 'admitted' : 'mismatch';
  const failure = result as Failure;
  if (failure.status === 400 || failure.status === 413 || failure.status === 429) return 'rejected';
  if (failure.status === 409 && failure.code === 'downloaded_scan_provider_unknown') return 'rejected';
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

/** Structural check of a parent for the requested domain; rejects cross-domain rows. */
export function validScanCommand(v: unknown, domain: MediaDomain): v is DownloadedScanCommand {
  return object(v) && scanUuid(v.id) && v.media_type === domain && typeof v.path === 'string'
    && member(v.mode, MODES) && member(v.priority, PRIORITIES) && member(v.status, STATUSES) && member(v.phase, PHASES)
    && (v.outcome === null || v.outcome === 'success' || v.outcome === 'unsuccessful')
    && typeof v.cancel_requested === 'boolean' && count(v.inventory_revision) && count(v.attempts) && validCounts(v.counts)
    && (v.phase === 'settled') === isTerminalScan(v.status as CommandStatus);
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
  queued: 'Queued', inventory: 'Discovering files', awaiting_import: 'Waiting to import', importing: 'Importing',
  recovery_required: 'Needs explicit recovery', settled: 'Finished',
};
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
  if (parent.phase === 'recovery_required') return `${c.recovery_required} file${c.recovery_required === 1 ? '' : 's'} need explicit recovery.${stopping}`;
  return `${PHASE_TEXT[parent.phase] ?? parent.phase}: ${tally}.${stopping}`;
}
