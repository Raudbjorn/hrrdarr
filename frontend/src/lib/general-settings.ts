import type { HostSettings, HostSettingSource } from './api.generated';

export type HostListView = {
  entries: string[];
  source: string;
  envVar: string;
  summary: string;
};
export type GeneralSettingsView = {
  authentication: string;
  bind: { configured: string; bound: string; source: string; envVar: string; note: string };
  allowedHosts: HostListView;
  trustedNetworks: HostListView;
  runtime: string[];
  unavailable: string[];
  limits: string[];
};
export type GeneralSettingsState =
  | { kind: 'ready'; view: GeneralSettingsView }
  | { kind: 'error'; message: string };

export const ENV_BIND = 'HRRDARR_BIND';
export const ENV_ALLOWED_HOSTS = 'HRRDARR_ALLOWED_HOSTS';
export const ENV_TRUSTED_NETWORKS = 'HRRDARR_TRUSTED_NETWORKS';
const MAX_LIST_ENTRIES = 64;
const MAX_ERROR_CHARS = 300;

export const LIMITS_WORDING = [
  'Allowed hosts validate the HTTP authority (Host header); they do not restrict client IP addresses. Anyone who can reach the listener can send an allowed Host value.',
  'Network reachability is controlled by the deployment bind address, firewall and VPN, not by this application. There is no credential authentication.',
  'These settings are deployment environment variables, read once at process start and immutable. Change them and restart the process externally; there is no save operation or restart endpoint.',
] as const;

// Settings that are not implemented by this build regardless of the host contract.
const NEVER_AVAILABLE = ['API key', 'Authentication method and credentials', 'Log level', 'Update and branch settings', 'Backup settings'];

const SOURCES: readonly HostSettingSource[] = ['default', 'environment'];

export function sourceLabel(source: HostSettingSource, envVar: string): string {
  return source === 'environment' ? `Environment variable ${envVar}` : 'Built-in default (no environment override)';
}

const stringList = (value: unknown): value is string[] =>
  Array.isArray(value) && value.length <= MAX_LIST_ENTRIES && value.every(item => typeof item === 'string');

function listView(entries: string[], source: HostSettingSource, envVar: string, enabled: boolean, off: string, on: string): HostListView {
  const summary = !enabled || entries.length === 0
    ? off
    : `${on} (${entries.length} ${entries.length === 1 ? 'entry' : 'entries'}).`;
  return { entries: [...entries], source: sourceLabel(source, envVar), envVar, summary };
}

export function describeHostSettings(raw: unknown): GeneralSettingsState {
  const bad: GeneralSettingsState = { kind: 'error', message: 'The server returned an unrecognised host settings response.' };
  if (!raw || typeof raw !== 'object') return bad;
  const s = raw as Partial<HostSettings>;
  const caps = s.capabilities;
  if (s.authentication !== 'none' || s.mutability !== 'deployment' || s.apply_mode !== 'process_restart'
    || typeof s.configured_bind !== 'string' || typeof s.bound_address !== 'string'
    || !SOURCES.includes(s.bind_source as HostSettingSource)
    || !SOURCES.includes(s.allowed_hosts_source as HostSettingSource)
    || !SOURCES.includes(s.trusted_networks_source as HostSettingSource)
    || !stringList(s.allowed_hosts) || !stringList(s.trusted_networks)
    || typeof s.filtering_enabled !== 'boolean' || typeof s.forwarding_enabled !== 'boolean'
    || !caps || typeof caps !== 'object' || typeof caps.tls !== 'boolean' || typeof caps.url_base !== 'boolean'
    || typeof caps.persisted_edits !== 'boolean' || typeof caps.trusted_forwarding !== 'boolean') return bad;

  const unavailable = [...NEVER_AVAILABLE];
  if (!caps.tls) unavailable.push('SSL / TLS');
  if (!caps.url_base) unavailable.push('URL base');
  if (!caps.persisted_edits) unavailable.push('Saving settings from the UI');

  return {
    kind: 'ready',
    view: {
      authentication: 'None. The process has no credential authentication.',
      bind: {
        configured: s.configured_bind,
        bound: s.bound_address,
        source: sourceLabel(s.bind_source as HostSettingSource, ENV_BIND),
        envVar: ENV_BIND,
        note: 'The bound address comes from the listener that is actually open (it differs from the configured value when port 0 is used). It is not a claim of an externally reachable URL.',
      },
      allowedHosts: listView(s.allowed_hosts, s.allowed_hosts_source as HostSettingSource, ENV_ALLOWED_HOSTS, s.filtering_enabled,
        'Filtering disabled: no allowed hosts are configured, so no application Host validation is applied (unless trusted forwarding is enabled).',
        'Host filtering enabled; requests must present one of these hostnames'),
      trustedNetworks: listView(s.trusted_networks, s.trusted_networks_source as HostSettingSource, ENV_TRUSTED_NETWORKS, s.forwarding_enabled,
        'Forwarding disabled: no proxy networks are trusted, so X-Forwarded-* headers are ignored. No network is implicitly trusted.',
        'Trusted proxy sources may supply X-Forwarded-For, -Host and -Proto'),
      runtime: [
        'Managed by deployment environment variables; immutable for the life of the process.',
        'Applying a change requires an external process restart.',
        caps.trusted_forwarding ? 'Trusted reverse proxy forwarding is supported by this build.' : 'Trusted reverse proxy forwarding is not supported by this build.',
      ],
      unavailable,
      limits: [...LIMITS_WORDING],
    },
  };
}

export function describeHostError(message: string): GeneralSettingsState {
  const text = typeof message === 'string' && message.trim() ? message.trim().slice(0, MAX_ERROR_CHARS) : 'Host settings request failed.';
  return { kind: 'error', message: text };
}

export function describeHostResult(result: { ok: true; data: unknown } | { ok: false; error: string }): GeneralSettingsState {
  return result.ok ? describeHostSettings(result.data) : describeHostError(result.error);
}
