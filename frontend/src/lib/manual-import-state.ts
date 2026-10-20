import type { Result } from './api';
import type { ManualImportRequest, MediaDomain, Operation, ManualImportCommand, ManualImportReconciliation, CommandPriority } from './api.generated';

export const MANUAL_IMPORT_LIMIT = 100;
export const MANUAL_IMPORT_RECEIPT_BYTES = 2 * 1024 * 1024;
const encoder = new TextEncoder();
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const phases = ['preview', 'staging', 'staged', 'published', 'committed', 'complete'];
const activeStatuses = ['queued', 'running', 'retry_wait'];
export type ManualImportOwner = Readonly<{ domain: MediaDomain; id: number }>;
type CapturedRequest = Readonly<Omit<ManualImportRequest, 'target'>> & { readonly target: Readonly<ManualImportRequest['target']> };
export type ManualImportEntry = Readonly<{
  operationId: string; request: CapturedRequest; owner: ManualImportOwner | null; label: string;
  authorized: boolean; commands: readonly Readonly<{ id: string; batchId: string }>[];
  admissionUncertain: boolean; executionUncertain: boolean; cancelUncertainIds: readonly string[];
}>;
/** Persisted identities and intent only. No stored observation grants action freshness. */
export type ManualImportReceipt = Readonly<{ version: 2; entries: readonly ManualImportEntry[] }>;
export type CheckedManualImportOperation = { operation: Operation; recoveryProjectionKnown: boolean };
const fail = <T>(error: string): Result<T> => ({ ok: false, error });
const good = <T>(data: T): Result<T> => ({ ok: true, data });
const object = (v: unknown): v is Record<string, unknown> => !!v && typeof v === 'object' && !Array.isArray(v);
const keys = (v: Record<string, unknown>, names: string[]) => Object.keys(v).length === names.length && names.every(k => Object.hasOwn(v, k));
const uuid = (v: unknown): v is string => typeof v === 'string' && UUID.test(v);
const integer = (v: unknown): v is number => typeof v === 'number' && Number.isSafeInteger(v) && v >= 0;
const positive = (v: unknown): v is number => integer(v) && v > 0;
const text = (v: unknown, max: number): v is string => typeof v === 'string' && !/[\uD800-\uDFFF]/u.test(v) && encoder.encode(v).length <= max && !/[\u0000-\u001f\u007f]/u.test(v);
const code = (v: unknown) => v === null || (typeof v === 'string' && /^[a-z0-9_]{1,64}$/.test(v));
const member = (v: unknown, values: readonly string[]): v is string => typeof v === 'string' && values.includes(v);
const sameId = (a: string, b: string) => a.toLowerCase() === b.toLowerCase();
function path(v: unknown): v is string {
  return typeof v === 'string' && !/[\uD800-\uDFFF]/u.test(v) && encoder.encode(v).length <= 4096 && !v.includes('\0') && v.startsWith('/') && v !== '/' && v.slice(1).split('/').every(p => p !== '' && p !== '.' && p !== '..');
}
function request(v: unknown): v is ManualImportRequest {
  return object(v) && keys(v, ['target', 'source', 'destination', 'mode']) && object(v.target)
    && keys(v.target, ['media_type', 'id']) && member(v.target.media_type, ['episode', 'movie'])
    && positive(v.target.id) && path(v.source) && path(v.destination)
    && member(v.mode, ['copy', 'move', 'hardlink']);
}
function owner(v: unknown, req: ManualImportRequest): v is ManualImportOwner | null {
  if (v === null) return true; // Legacy receipts do not know the series owner.
  return object(v) && keys(v, ['domain', 'id']) && positive(v.id)
    && v.domain === (req.target.media_type === 'movie' ? 'movies' : 'tv')
    && (req.target.media_type !== 'movie' || v.id === req.target.id);
}
function capture(req: ManualImportRequest): CapturedRequest {
  return Object.freeze({ ...req, target: Object.freeze({ ...req.target }) });
}
export function createManualImportEntry(operationId: string, input: ManualImportRequest, label: string, libraryOwner: ManualImportOwner | null): Result<ManualImportEntry> {
  if (!uuid(operationId) || !request(input) || !owner(libraryOwner, input) || !text(label, 1024) || [...label].length > 256)
    return fail('Invalid captured import request, target, owner, label or operation ID.');
  return good({ operationId: operationId.toLowerCase(), request: capture(input), label,
    owner: libraryOwner === null ? null : Object.freeze({ ...libraryOwner }), authorized: false,
    commands: [], admissionUncertain: false, executionUncertain: false, cancelUncertainIds: [] });
}
function entry(v: unknown): Result<ManualImportEntry> {
  if (!object(v) || !keys(v, ['operationId', 'request', 'owner', 'label', 'authorized', 'commands', 'admissionUncertain', 'executionUncertain', 'cancelUncertainIds'])
    || typeof v.authorized !== 'boolean' || typeof v.admissionUncertain !== 'boolean' || typeof v.executionUncertain !== 'boolean'
    || !Array.isArray(v.commands) || v.commands.length > MANUAL_IMPORT_LIMIT
    || !Array.isArray(v.cancelUncertainIds) || v.cancelUncertainIds.length > MANUAL_IMPORT_LIMIT)
    return fail('Invalid import receipt entry.');
  const created = createManualImportEntry(v.operationId as string, v.request as ManualImportRequest, v.label as string, v.owner as ManualImportOwner | null);
  if (!created.ok) return created;
  const commands: { id: string; batchId: string }[] = [];
  for (const c of v.commands) {
    if (!object(c) || !keys(c, ['id', 'batchId']) || !uuid(c.id) || !uuid(c.batchId) || commands.some(previous => sameId(previous.id, c.id as string)))
      return fail('Invalid or duplicate saved command identity.');
    commands.push({ id: c.id.toLowerCase(), batchId: c.batchId.toLowerCase() });
  }
  const pending: string[] = [];
  for (const id of v.cancelUncertainIds) {
    if (!uuid(id) || pending.includes(id.toLowerCase()) || !commands.some(c => sameId(c.id, id))) return fail('Cancellation uncertainty must identify a known command.');
    pending.push(id.toLowerCase());
  }
  if (!v.authorized && (v.admissionUncertain || v.executionUncertain || commands.length || pending.length)) return fail('Command intent requires a retained authorization.');
  return good({ ...created.data, authorized: v.authorized, commands, admissionUncertain: v.admissionUncertain, executionUncertain: v.executionUncertain, cancelUncertainIds: pending });
}
function receipt(v: unknown): Result<ManualImportReceipt> {
  if (!object(v) || !keys(v, ['version', 'entries']) || v.version !== 2 || !Array.isArray(v.entries) || v.entries.length > MANUAL_IMPORT_LIMIT)
    return fail('Invalid import receipt version or collection size.');
  const entries: ManualImportEntry[] = [], ids = new Set<string>(), commands = new Set<string>();
  for (const raw of v.entries) {
    const parsed = entry(raw); if (!parsed.ok) return parsed;
    if (ids.has(parsed.data.operationId)) return fail('Duplicate saved operation ID.');
    ids.add(parsed.data.operationId);
    for (const c of parsed.data.commands) {
      if (commands.has(c.id)) return fail('A saved command cannot belong to multiple operations.');
      commands.add(c.id);
    }
    entries.push(parsed.data);
  }
  return good({ version: 2, entries });
}
export function decodeManualImportReceipt(raw: string): Result<ManualImportReceipt> {
  if (typeof raw !== 'string' || raw.length > MANUAL_IMPORT_RECEIPT_BYTES || encoder.encode(raw).length > MANUAL_IMPORT_RECEIPT_BYTES)
    return fail('Import receipt exceeds 2 MiB.');
  try { return receipt(JSON.parse(raw)); } catch { return fail('Import receipt is not valid JSON.'); }
}
export function encodeManualImportReceipt(value: ManualImportReceipt): Result<string> {
  const checked = receipt(value); if (!checked.ok) return checked;
  const raw = JSON.stringify(checked.data);
  return encoder.encode(raw).length <= MANUAL_IMPORT_RECEIPT_BYTES ? good(raw) : fail('Import receipt exceeds 2 MiB.');
}
export function migrateLegacyManualImportReceipt(raw: string): Result<ManualImportReceipt> {
  if (typeof raw !== 'string' || raw.length > 16384 || encoder.encode(raw).length > 16384) return fail('Legacy import receipt exceeds 16 KiB.');
  let v: unknown; try { v = JSON.parse(raw); } catch { return fail('Legacy import receipt is not valid JSON.'); }
  if (!object(v) || !keys(v, ['request', 'label', 'operation', 'authorized']) || typeof v.authorized !== 'boolean' || !object(v.operation)) return fail('Invalid legacy import receipt.');
  const created = createManualImportEntry(v.operation.id as string, v.request as ManualImportRequest, v.label as string, null);
  if (!created.ok) return created;
  const checked = validateManualImportOperation(created.data, v.operation); if (!checked.ok) return checked;
  // The operation observation is deliberately discarded. Neither old nor new receipts are fresh.
  return good({ version: 2, entries: [{ ...created.data, authorized: v.authorized, executionUncertain: v.authorized }] });
}
export function validateManualImportSelection(entries: readonly ManualImportEntry[]): Result<string[]> {
  if (!Array.isArray(entries) || entries.length === 0 || entries.length > MANUAL_IMPORT_LIMIT) return fail('Select 1–100 imports.');
  const ids = new Set<string>(), targets = new Set<string>(), destinations = new Set<string>(), sources = new Map<string, string>();
  for (const value of entries) {
    const checked = entry(value); if (!checked.ok) return checked;
    const e = checked.data, target = `${e.request.target.media_type}:${e.request.target.id}`;
    if (ids.has(e.operationId) || targets.has(target) || destinations.has(e.request.destination)) return fail('Selected imports repeat an operation, target or destination.');
    const previousMode = sources.get(e.request.source);
    if (previousMode && (previousMode === 'move' || e.request.mode === 'move')) return fail('A moved source cannot be shared by selected imports.');
    ids.add(e.operationId); targets.add(target); destinations.add(e.request.destination); sources.set(e.request.source, e.request.mode);
  }
  return good([...ids]);
}
function pathCode(v: unknown): boolean {
  if (v === null || v === 'unresolved_attempt') return true;
  if (typeof v !== 'string') return false;
  const [role, kind, boundary, extra] = v.split('_');
  return extra === undefined && ['source', 'root', 'destination', 'stage', 'retirement'].includes(role)
    && ['missing', 'denied', 'changed', 'invalid', 'io'].includes(kind)
    && (boundary === 'cleanup' || (boundary === 'precommit' && role !== 'retirement'));
}
export function validateManualImportOperation(e: ManualImportEntry, value: unknown): Result<CheckedManualImportOperation> {
  if (!object(value) || !uuid(value.id) || !sameId(value.id, e.operationId) || !object(value.target)
    || value.target.media_type !== e.request.target.media_type || !positive(value.target.id) || value.target.id !== e.request.target.id
    || !member(value.status, phases) || !text(value.message, 4096) || !code(value.error_code)) return fail('Import operation does not match the captured target or valid status contract.');
  const hasPath = Object.hasOwn(value, 'path_failure_code'), hasRecovery = Object.hasOwn(value, 'recovery_required');
  if (hasPath !== hasRecovery || (hasPath && (!pathCode(value.path_failure_code) || typeof value.recovery_required !== 'boolean'))
    || (value.status === 'complete' && hasPath && (value.path_failure_code !== null || value.recovery_required !== false))
    || (hasPath && value.recovery_required === true && value.error_code === null && value.path_failure_code === null))
    return fail('Invalid import recovery projection.');
  return good({ operation: value as Operation, recoveryProjectionKnown: hasPath && hasRecovery });
}
function command(value: unknown, operationId: string, activeOnly = false): value is ManualImportCommand {
  if (!object(value) || !uuid(value.id) || !uuid(value.batch_id) || !uuid(value.operation_id) || !sameId(value.operation_id, operationId)
    || !member(value.priority, ['normal', 'high']) || !member(value.status, [...activeStatuses, 'succeeded', 'failed', 'cancelled'])
    || (activeOnly && !member(value.status, activeStatuses)) || !integer(value.attempts) || value.attempts > 3
    || !integer(value.next_attempt_at) || !integer(value.created_at) || !code(value.error_code)) return false;
  if (!['started_at', 'completed_at'].every(k => value[k] === null || integer(value[k]))) return false;
  // Migration0030's CHECK constraints define the wire states, including pre-attempt failure/cancel.
  if (member(value.status, activeStatuses) ? value.completed_at !== null : value.completed_at === null) return false;
  if (value.status === 'queued' && (value.attempts !== 0 || value.started_at !== null)) return false;
  if (member(value.status, ['running', 'retry_wait', 'succeeded']) && (value.attempts === 0 || value.started_at === null)) return false;
  if (value.status === 'retry_wait' && value.attempts >= 3) return false;
  return member(value.status, ['failed', 'retry_wait']) ? value.error_code !== null : value.error_code === null;
}
export function validateManualImportCommand(e: ManualImportEntry, commandId: string, value: unknown): Result<ManualImportCommand> {
  if (!uuid(commandId) || !command(value, e.operationId) || !sameId(value.id, commandId)) return fail('Command does not match the captured operation and command ID.');
  const known = e.commands.find(c => sameId(c.id, value.id));
  return known && !sameId(known.batchId, value.batch_id) ? fail('Known command batch identity changed.') : good(value);
}
function validEntries(entries: readonly ManualImportEntry[]): boolean {
  return entries.length > 0 && entries.length <= MANUAL_IMPORT_LIMIT && receipt({version:2,entries}).ok;
}
export function validateManualImportReconciliation(entries: readonly ManualImportEntry[], value: unknown): Result<ManualImportReconciliation[]> {
  if (!validEntries(entries) || !Array.isArray(value) || value.length !== entries.length) return fail('Reconciliation must return every requested operation exactly once.');
  const commandIds = new Set<string>();
  for (let i = 0; i < entries.length; i++) {
    const row: unknown = value[i], e = entries[i];
    if (!object(row) || !uuid(row.operation_id) || !sameId(row.operation_id, e.operationId)) return fail('Reconciliation membership or order does not match.');
    if (row.operation === null) { if (row.active_command !== null) return fail('Missing operation has a command.'); continue; }
    if (!validateManualImportOperation(e, row.operation).ok) return fail('Reconciliation operation target or status is invalid.');
    if (row.active_command !== null) {
      if (!command(row.active_command, e.operationId, true) || !validateManualImportCommand(e, row.active_command.id, row.active_command).ok || commandIds.has(row.active_command.id.toLowerCase())) return fail('Reconciliation active command is invalid.');
      commandIds.add(row.active_command.id.toLowerCase());
    }
  }
  return good(value as ManualImportReconciliation[]);
}
export function validateManualImportAdmission(entries: readonly ManualImportEntry[], value: unknown, priority: CommandPriority): Result<ManualImportCommand[]> {
  if (!member(priority, ['normal', 'high']) || !validEntries(entries) || !Array.isArray(value) || value.length !== entries.length) return fail('Admission response membership or priority is invalid.');
  const ids = new Set<string>(); let batch: string | null = null;
  for (let i = 0; i < entries.length; i++) {
    const c: unknown = value[i];
    if (!command(c, entries[i].operationId) || c.status !== 'queued' || c.attempts !== 0 || c.started_at !== null || c.completed_at !== null || c.error_code !== null || c.priority !== priority || !validateManualImportCommand(entries[i], c.id, c).ok || ids.has(c.id.toLowerCase()) || (batch !== null && !sameId(batch, c.batch_id))) return fail('Admission response identities do not match the selected operations.');
    ids.add(c.id.toLowerCase()); batch = c.batch_id;
  }
  return good(value as ManualImportCommand[]);
}
export function markExecutionUncertain(e: ManualImportEntry): ManualImportEntry {
  return { ...e, authorized: true, executionUncertain: true };
}
export function markAdmissionUncertain(e: ManualImportEntry): ManualImportEntry {
  return { ...e, authorized: true, admissionUncertain: true };
}
export function recordKnownCommand(e: ManualImportEntry, value: unknown): Result<ManualImportEntry> {
  if (!command(value, e.operationId)) return fail('Invalid command for captured operation.');
  const checked = validateManualImportCommand(e, value.id, value); if (!checked.ok) return checked;
  const known = e.commands.some(c => sameId(c.id, value.id));
  if (!known && e.commands.length >= MANUAL_IMPORT_LIMIT) return fail('Saved command identity limit reached.');
  return good({ ...e, authorized: true, commands: known ? e.commands : [...e.commands, {id:value.id.toLowerCase(), batchId:value.batch_id.toLowerCase()}] });
}
export function markCancelUncertain(e: ManualImportEntry, commandId: string): Result<ManualImportEntry> {
  if (!uuid(commandId) || !e.commands.some(c => sameId(c.id, commandId))) return fail('Cancellation requires a known command.');
  return good({ ...e, cancelUncertainIds: [...new Set([...e.cancelUncertainIds, commandId.toLowerCase()])] });
}
/** Readback settles only this command's cancellation; unresolved admission is preserved. */
export function resolveCancelUncertain(e: ManualImportEntry, commandId: string, value: unknown): Result<ManualImportEntry> {
  const checked = validateManualImportCommand(e, commandId, value); if (!checked.ok) return checked;
  if (!['succeeded', 'failed', 'cancelled'].includes(checked.data.status)) return fail('Cancellation remains unresolved while the command is active.');
  return good({ ...e, cancelUncertainIds: e.cancelUncertainIds.filter(id => !sameId(id, commandId)) });
}
