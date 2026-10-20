// Public diagnostics contain closed codes, never filesystem paths or provider endpoints.
type ImportState = {
  status: string;
  path_failure_code?: string | null;
  recovery_required?: boolean;
  error_code?: string | null;
};
export function hasRecoveryState(value: ImportState): boolean {
  return typeof value.recovery_required === 'boolean'
    && (value.path_failure_code === null || typeof value.path_failure_code === 'string');
}
export function manualImportAction(value: ImportState, fresh: boolean): 'execute' | 'resume' | null {
  if (!fresh || !hasRecoveryState(value) || value.status === 'complete') return null;
  if (value.recovery_required === true) return 'resume';
  // A lost execute request can leave the same confirmed operation at its untouched preview.
  if (value.status === 'preview' && value.path_failure_code === null && !value.error_code) return 'execute';
  return null;
}
export function canResumeProcessing(value: {
  status: string; import_phase: string | null; operation_id: string | null;
  recovery_required?: boolean; resume_requested: boolean;
}): boolean {
  return value.status === 'importing' && !!value.operation_id && value.import_phase !== 'complete'
    && value.recovery_required === true && !value.resume_requested;
}
const roles: Record<string, string> = {
  source: 'source', root: 'library root', destination: 'destination',
  stage: 'staging location', retirement: 'replaced-file recovery location',
};
const kinds: Record<string, string> = {
  missing: 'was missing', denied: 'denied access to the service', changed: 'changed identity or content',
  invalid: 'did not meet import path requirements', io: 'could not be accessed because of an I/O error',
};
const unknown = 'An import has an unrecognized diagnostic. Check its current status and saved settings before retrying; no specific filesystem cause has been established.';
export function pathDiagnostic(code: unknown, recoveryRequired: boolean, complete = false): string | null {
  if (complete || code === null || code === undefined) return null;
  if (typeof code !== 'string') return unknown;
  const receipt = code.startsWith('hc021_');
  const cause = receipt ? code.slice(6) : code;
  if (cause === 'unresolved_attempt') return recoveryRequired
    ? 'The previous import attempt has no confirmed outcome. Check retained files and explicitly resume the same operation when ready.'
    : 'This import attempt has unresolved evidence. Check status before another action; this does not establish a specific filesystem failure.';
  if (receipt && ['mapping_missing', 'mapping_changed'].includes(cause)) return 'A download import recorded a missing or changed path mapping. Review the mapping before retrying the same receipt.';
  if (receipt && ['output_missing', 'output_invalid'].includes(cause)) return 'A download import recorded absent or unusable client output. Check the client category, output directory and path mapping before retrying.';
  const parts = cause.split('_');
  if (parts.length !== 3) return unknown;
  const [role, kind, boundary] = parts;
  if (!Object.hasOwn(roles, role) || !Object.hasOwn(kinds, kind)
    || (receipt ? boundary !== 'prepare' || !['source', 'root', 'destination'].includes(role) : !['precommit', 'cleanup'].includes(boundary))
    || (role === 'retirement' && boundary !== 'cleanup')) return unknown;
  if (role === 'source' && kind === 'missing' && boundary === 'precommit') return 'An approved source was recorded missing before import completed. Restore or review the source before explicitly resuming the same operation.';
  const detail = `An import recorded that its ${roles[role]} ${kinds[kind]}.`;
  return boundary === 'cleanup'
    ? `${detail} Cleanup remains unresolved. Preserve retained files and resume the same operation after correcting the issue; this is not evidence of source loss before import.`
    : `${detail} Check the affected location and service access before retrying. This finding remains until the operation completes.`;
}
export function processingDiagnostic(value: {
  status: string; import_phase: string | null; path_failure_code?: string | null;
  recovery_required?: boolean; reasons: string[];
}): string | null {
  if (value.status === 'imported' || value.import_phase === 'complete') return null;
  if (value.path_failure_code != null) return pathDiagnostic(value.path_failure_code, value.recovery_required === true);
  const markers = value.reasons.filter(reason => reason.startsWith('hc021_'));
  if (markers.length > 1 || (markers.length === 1 && value.reasons[0] !== markers[0])) return unknown;
  return pathDiagnostic(markers[0], value.recovery_required === true);
}
