// Pure hash-route table. No DOM access: callers pass in `location.hash`.
export type ViewId =
  | 'library' | 'health' | 'providers' | 'activity' | 'blocklist' | 'rss' | 'qualities' | 'profiles'
  | 'naming' | 'import-existing' | 'custom-formats' | 'tags' | 'media-management' | 'general' | 'history';

export type Route =
  | { kind: 'view'; view: ViewId }
  | { kind: 'settings-index' }
  | { kind: 'not-found'; path: string };

export const MAX_HASH_LENGTH = 256;
export const MAX_DISPLAY_LENGTH = 80;
export const SETTINGS_INDEX_PATH = '/settings';

/** One unique path per view; the single source of truth for URLs. */
export const VIEW_PATHS: Readonly<Record<ViewId, string>> = {
  library: '/',
  health: '/system/health',
  rss: '/system/rss',
  activity: '/activity/queue',
  blocklist: '/activity/blocklist',
  history: '/activity/history',
  providers: '/settings/providers',
  qualities: '/settings/quality',
  profiles: '/settings/profiles',
  naming: '/settings/naming',
  'media-management': '/settings/mediamanagement',
  'custom-formats': '/settings/customformats',
  tags: '/settings/tags',
  general: '/settings/general',
  'import-existing': '/add/import',
};

export const VIEW_IDS = Object.keys(VIEW_PATHS) as ViewId[];

export type SettingsSection = { view: ViewId; label: string; description: string };

/** Only sections that exist as views in this build. */
export const SETTINGS_SECTIONS: readonly SettingsSection[] = [
  { view: 'providers', label: 'Providers', description: 'Indexers and download clients' },
  { view: 'qualities', label: 'Quality', description: 'Quality definitions' },
  { view: 'profiles', label: 'Profiles', description: 'Quality, delay and release profiles' },
  { view: 'naming', label: 'Naming', description: 'Episode and movie file naming' },
  { view: 'media-management', label: 'Media management', description: 'Revision and upgrade policy' },
  { view: 'custom-formats', label: 'Custom formats', description: 'Release matching formats' },
  { view: 'tags', label: 'Tags', description: 'Tags shared by library items and settings' },
  { view: 'general', label: 'General', description: 'Host and general settings' },
];

/** Listed as text only, never as links. */
export const UNAVAILABLE_SECTIONS: readonly string[] = [
  'Import lists', 'Metadata', 'Connect (notifications)', 'UI settings', 'Backup', 'Updates', 'Logs',
];

const PATH_TO_VIEW = new Map<string, ViewId>(VIEW_IDS.map((v) => [VIEW_PATHS[v], v]));
const UNSAFE_DISPLAY = /[\u0000-\u001f\u007f-\u009f​-‏‪-‮⁦-⁩﻿]/g;

/** Bounded text for display. Callers must still render it as text, never as HTML. */
export function sanitizeDisplay(text: string): string {
  const chars = Array.from(text.replace(UNSAFE_DISPLAY, '?'));
  return chars.length > MAX_DISPLAY_LENGTH ? `${chars.slice(0, MAX_DISPLAY_LENGTH).join('')}…` : chars.join('');
}

function notFound(raw: string): Route {
  return { kind: 'not-found', path: sanitizeDisplay(raw) };
}

/** Decodes each segment separately so an encoded slash cannot forge a different route. */
function decodeSegments(path: string): string | null {
  const parts: string[] = [];
  for (const segment of path.split('/')) {
    let decoded: string;
    try {
      decoded = decodeURIComponent(segment);
    } catch {
      return null;
    }
    if (decoded.includes('/')) return null;
    parts.push(decoded);
  }
  return parts.join('/');
}

export function parseHash(hash: string): Route {
  const input = typeof hash === 'string' ? hash : '';
  if (input.length > MAX_HASH_LENGTH) return notFound(input);
  let path = input.startsWith('#') ? input.slice(1) : input;
  const query = path.indexOf('?');
  if (query >= 0) path = path.slice(0, query);
  if (path === '') return { kind: 'view', view: 'library' };
  if (!path.startsWith('/')) path = `/${path}`;
  if (path.length > 1 && path.endsWith('/')) path = path.slice(0, -1);
  const decoded = decodeSegments(path);
  if (decoded === null) return notFound(path);
  if (decoded === SETTINGS_INDEX_PATH) return { kind: 'settings-index' };
  const view = PATH_TO_VIEW.get(decoded);
  return view ? { kind: 'view', view } : notFound(decoded);
}

export function hashFor(view: ViewId): string {
  return `#${VIEW_PATHS[view]}`;
}

export const SETTINGS_INDEX_HASH = `#${SETTINGS_INDEX_PATH}`;
