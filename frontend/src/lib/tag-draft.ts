import type { Tag, TagAssignmentMode } from './api.generated';
export function tagLabelError(label: string): string {
  return /^[a-zA-Z0-9-]{1,128}$/.test(label) ? '' : 'Use 1–128 ASCII letters, digits or hyphens; labels are saved lowercase.';
}
export function assignmentError(ids: number[], catalog: Tag[], mode: TagAssignmentMode): string {
  if (!['add','remove','replace'].includes(mode)) return 'Choose an assignment action.';
  if (ids.length > 200 || new Set(ids).size !== ids.length || ids.some(id => !Number.isSafeInteger(id) || id <= 0 || !catalog.some(tag => tag.id === id))) return 'Reload tags and choose up to 200 distinct current tags.';
  if (!ids.length && mode !== 'replace') return 'Choose at least one tag to add or remove.';
  return '';
}
export const unknownTagWrite = (result: {status?:number}) => result.status === undefined || result.status >= 500;
