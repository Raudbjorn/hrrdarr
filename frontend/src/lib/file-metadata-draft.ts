import type { CustomFormatChoice, CustomFormatSchema, FilePatch, FileResource, FileRevision, MediaDomain, QualityDefinition } from './api.generated';
import type { Result } from './api';

export type EditIntent<T> = { action: 'keep' } | { action: 'clear' } | { action: 'set'; value: T };
export type FileMetadataDraft = {
  quality: EditIntent<{ quality_id: number; revision: FileRevision | null }>;
  languages: EditIntent<number[]>;
  release_group: EditIntent<string>;
  edition: EditIntent<string>;
  indexer_flags: EditIntent<number>;
  release_type: EditIntent<number>;
};
export type FileMetadataCatalogs = {
  domain: MediaDomain;
  qualities: CustomFormatChoice[];
  languages: CustomFormatChoice[];
  indexer_flags: CustomFormatChoice[];
  release_types: CustomFormatChoice[];
};
export const FILE_PAGE_SIZE = 25;
const fail = <T>(error: string): Result<T> => ({ ok: false, error });
const object = (v: unknown): v is Record<string, unknown> => !!v && typeof v === 'object' && !Array.isArray(v);
const integer = (v: unknown, min: number, max = Number.MAX_SAFE_INTEGER): v is number => typeof v === 'number' && Number.isSafeInteger(v) && v >= min && v <= max;
const domainValid = (d: unknown): d is MediaDomain => d === 'tv' || d === 'movies';
const revisionValid = (v: unknown): v is FileRevision => object(v) && integer(v.version, 0, 2147483647) && integer(v.real, 0, 2147483647) && typeof v.is_repack === 'boolean';
const textValid = (v: unknown): v is string => typeof v === 'string' && !v.includes('\0') && new TextEncoder().encode(v).length <= 1024;

export function emptyFileMetadataDraft(): FileMetadataDraft {
  return { quality: { action: 'keep' }, languages: { action: 'keep' }, release_group: { action: 'keep' }, edition: { action: 'keep' }, indexer_flags: { action: 'keep' }, release_type: { action: 'keep' } };
}

export function fileMetadataCatalogs(domain: MediaDomain, definitions: QualityDefinition[], schema: CustomFormatSchema): Result<FileMetadataCatalogs> {
  if (!domainValid(domain) || !object(schema) || schema.media_type !== domain || !object(schema.choices) || !Array.isArray(definitions) || !definitions.length || definitions.length > 200) return fail('File metadata catalogs are invalid or belong to another domain.');
  const qualities: CustomFormatChoice[] = [];
  for (const definition of definitions) {
    if (!object(definition) || definition.media_type !== domain || !object(definition.quality) || !integer(definition.quality.id, 0) || typeof definition.title !== 'string' || typeof definition.quality.name !== 'string' || qualities.some(q => q.value === definition.quality.id)) return fail('Quality catalog is invalid.');
    qualities.push({ value: definition.quality.id, label: definition.title || definition.quality.name });
  }
  const choices = (key: string, accept: (n: number) => boolean): CustomFormatChoice[] | null => {
    const source = schema.choices[key];
    if (!Array.isArray(source) || source.length > 128) return null;
    const result: CustomFormatChoice[] = [];
    const seen = new Set<number>();
    for (const entry of source) {
      if (!object(entry) || !integer(entry.value, -2) || typeof entry.label !== 'string' || seen.has(entry.value)) return null;
      seen.add(entry.value);
      if (accept(entry.value)) result.push({ value: entry.value, label: entry.label });
      else if (key !== 'language' || entry.value >= 0) return null;
    }
    return result.length ? result : null;
  };
  const languages = choices('language', n => n >= 0 && n <= (domain === 'tv' ? 52 : 57));
  const indexer_flags = choices('indexer_flag', n => n > 0 && n <= (domain === 'tv' ? 256 : 2048) && (n & (n - 1)) === 0);
  const release_types = domain === 'tv' ? choices('release_type', n => n >= 0 && n <= 3) : [];
  if (!languages || !indexer_flags || !release_types) return fail('Language, flag or release type catalog is invalid.');
  return { ok: true, data: { domain, qualities, languages, indexer_flags, release_types } };
}

export function buildFileMetadataPatch(domain: MediaDomain, draft: FileMetadataDraft, catalogs: FileMetadataCatalogs): Result<FilePatch> {
  if (!domainValid(domain) || catalogs.domain !== domain) return fail('Catalog domain does not match the file.');
  const patch: FilePatch = {};
  for (const field of Object.keys(emptyFileMetadataDraft()) as (keyof FileMetadataDraft)[]) {
    const intent = draft[field];
    if (!intent || !['keep', 'clear', 'set'].includes(intent.action)) return fail(`Choose an action for ${field}.`);
    if (intent.action === 'keep') continue;
    if ((field === 'edition' && domain !== 'movies') || (field === 'release_type' && domain !== 'tv')) return fail(`${field} is unavailable for this domain.`);
    if (intent.action === 'clear') { Object.assign(patch, { [field]: null }); continue; }
    const value = intent.value;
    switch (field) {
      case 'quality':
        if (!object(value) || !integer(value.quality_id, 0) || !catalogs.qualities.some(q => q.value === value.quality_id) || (value.revision !== null && !revisionValid(value.revision))) return fail('Select a catalog quality and explicitly choose unknown or a complete revision.');
        patch.quality = { quality_id: value.quality_id, revision: value.revision === null ? null : { version: value.revision.version, real: value.revision.real, is_repack: value.revision.is_repack } };
        break;
      case 'languages':
        if (!Array.isArray(value) || value.length > 64 || new Set(value).size !== value.length || value.some(n => !integer(n, 0, domain === 'tv' ? 52 : 57) || !catalogs.languages.some(c => c.value === n))) return fail('Languages must be distinct concrete catalog values.');
        patch.languages = [...value];
        break;
      case 'release_group': case 'edition':
        if (!textValid(value)) return fail(`${field} must contain at most 1024 UTF-8 bytes and no NUL.`);
        patch[field] = value;
        break;
      case 'indexer_flags': {
        const mask = catalogs.indexer_flags.reduce((bits, c) => bits | c.value, 0);
        if (!integer(value, 0, domain === 'tv' ? 511 : 4095) || (value & ~mask) !== 0) return fail('Flags must use this domain’s catalog bits.');
        patch.indexer_flags = value;
        break;
      }
      case 'release_type':
        if (!integer(value, 0, 3) || !catalogs.release_types.some(c => c.value === value)) return fail('Select a TV release type from the catalog.');
        patch.release_type = value;
    }
  }
  return Object.keys(patch).length ? { ok: true, data: patch } : fail('Choose at least one metadata change.');
}

export function validFileIds(ids: unknown): ids is number[] {
  return Array.isArray(ids) && ids.length > 0 && ids.length <= FILE_PAGE_SIZE && ids.every(id => integer(id, 1)) && new Set(ids).size === ids.length;
}

// Validate the resource boundary without rejecting unknown retained catalog values.
export function validFileResource(domain: MediaDomain, value: unknown): value is FileResource {
  if (!domainValid(domain) || !object(value) || !integer(value.id, 1) || typeof value.path !== 'string') return false;
  const nullableString = (key: string) => value[key] === null || typeof value[key] === 'string';
  const nullableInteger = (key: string) => value[key] === null || integer(value[key], 0);
  if (!['relative_path', 'date_added', 'release_group'].every(nullableString) || !['size', 'indexer_flags'].every(nullableInteger)) return false;
  if (value.quality !== null && (!object(value.quality) || !integer(value.quality.quality_id, 0) || (value.quality.revision !== null && !revisionValid(value.quality.revision)))) return false;
  if (value.languages !== null && (!Array.isArray(value.languages) || value.languages.length > 64 || !value.languages.every(n => integer(n, -2)))) return false;
  if (!['scene_name', 'custom_formats', 'custom_format_score', 'quality_cutoff_not_met'].every(key => value[key] === null)) return false;
  if (value.media_info !== null && !validMediaInfo(value.media_info)) return false;
  return domain === 'tv'
    ? integer(value.series_id, 1) && !('movie_id' in value) && nullableInteger('season_number') && nullableInteger('release_type')
    : integer(value.movie_id, 1) && !('series_id' in value) && nullableString('edition') && nullableString('original_file_path');
}
function validMediaInfo(value: unknown): boolean {
  if (!object(value)) return false;
  const strings = 'container_format audio_codec audio_languages video_codec video_dynamic_range video_dynamic_range_type resolution run_time scan_type subtitles video_format video_codec_id video_profile audio_format audio_codec_id audio_profile audio_channel_positions'.split(' ');
  const numbers = 'schema_revision audio_bitrate audio_channels audio_stream_count video_bit_depth video_bitrate video_fps audio_channel_count width height runtime_ticks'.split(' ');
  if (!strings.every(k => value[k] === null || typeof value[k] === 'string') || !numbers.every(k => value[k] === null || (typeof value[k] === 'number' && Number.isFinite(value[k])))) return false;
  const streams = (key: string, text: string[], numeric: string[], bools: string[]) => value[key] === null || (Array.isArray(value[key]) && value[key].length <= 256 && value[key].every(s => object(s) && text.every(k => s[k] === null || typeof s[k] === 'string') && numeric.every(k => s[k] === null || (typeof s[k] === 'number' && Number.isFinite(s[k]))) && bools.every(k => s[k] === null || typeof s[k] === 'boolean')));
  return streams('audio_streams', ['language', 'format', 'codec_id', 'profile', 'channel_positions'], ['bitrate', 'channels'], []) && streams('subtitle_streams', ['language', 'format'], [], ['forced', 'hearing_impaired']);
}

export function validateFileReadback(domain: MediaDomain, ownerId: number, ids: number[], value: unknown): Result<FileResource[]> {
  if (!domainValid(domain) || !integer(ownerId, 1) || !validFileIds(ids) || !Array.isArray(value) || value.length !== ids.length) return fail('Readback must contain every attempted file. Repair the selection if a file is missing.');
  const seen = new Set<number>();
  for (const file of value) {
    if (!validFileResource(domain, file) || !ids.includes(file.id) || seen.has(file.id) || (domain === 'tv' ? ('series_id' in file ? file.series_id : null) : ('movie_id' in file ? file.movie_id : null)) !== ownerId) return fail('Readback contains an invalid, duplicate or differently owned file.');
    seen.add(file.id);
  }
  return { ok: true, data: value };
}

export function mixedFileValue(files: FileResource[], field: keyof FileMetadataDraft): { kind: 'mixed' } | { kind: 'value'; value: unknown } {
  const values = files.map(file => field in file ? (file as unknown as Record<string, unknown>)[field] : undefined);
  const key = (value: unknown) => JSON.stringify(value, (_k, v) => Array.isArray(v) ? [...v].sort((a, b) => typeof a === 'number' && typeof b === 'number' ? a - b : 0) : v);
  return values.some(v => key(v) !== key(values[0])) ? { kind: 'mixed' } : { kind: 'value', value: values[0] };
}
export function fileValueLabel(value: unknown): string {
  if (value === null || value === undefined) return 'Unknown';
  if (value === '') return 'Empty text';
  if (Array.isArray(value) && !value.length) return 'Known empty';
  return typeof value === 'string' ? value : JSON.stringify(value);
}
