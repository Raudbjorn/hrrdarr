import type { AutoTag, AutoTagCondition, AutoTagInput, AutoTagSchemaEntry, AutoTagSchemaField, Tag } from './api.generated';

// Bounds mirror src/autotagging.rs; the server stays authoritative.
export const MAX_NAME_BYTES = 256;
export const MAX_RULE_TAGS = 64;
export const MAX_SPECIFICATIONS = 64;
export const MAX_VALUES = 64;
const MAX_ID = Number.MAX_SAFE_INTEGER;
const MAX_ROOT_BYTES = 4096;
const RANGE_BOUNDS: Record<string, [number, number]> = { year: [1, 9999], runtime: [0, 10080] };

export type SpecDraft = {
  key: number; name: string; implementation: string; required: boolean; negate: boolean;
  text: Record<string, string>; flag: Record<string, boolean>; pick: Record<string, string[]>;
};
export type RuleDraft = { name: string; remove: boolean; tagIds: number[]; specs: SpecDraft[] };
export type DraftResult = { ok: true; input: AutoTagInput } | { ok: false; error: string };

const bytes = (s: string) => new TextEncoder().encode(s).length;
const hasControl = (s: string) => /[\u0000-\u001f\u007f-\u009f]/.test(s);
let nextKey = 1;

/** Split free text on commas and newlines, trimming and dropping blanks. */
export function parseTextList(text: string): string[] {
  return text.split(/[,\n\r]+/).map(s => s.trim()).filter(Boolean);
}
/** Integer list parser: null when any item is not a plain non-negative safe integer. */
export function parseIntList(text: string): number[] | null {
  const items = parseTextList(text);
  const out: number[] = [];
  for (const item of items) {
    if (!/^\d{1,16}$/.test(item)) return null;
    const n = Number(item);
    if (!Number.isSafeInteger(n) || n > MAX_ID) return null;
    out.push(n);
  }
  return out;
}

export function emptyRule(): RuleDraft { return { name: '', remove: false, tagIds: [], specs: [] }; }

export function newSpec(entry: AutoTagSchemaEntry | undefined): SpecDraft {
  const spec: SpecDraft = { key: nextKey++, name: entry?.label ?? '', implementation: entry?.implementation ?? '', required: false, negate: false, text: {}, flag: {}, pick: {} };
  initFields(spec, entry);
  return spec;
}
/** Reset the per-field state for an implementation (keeps name/flags). */
export function initFields(spec: SpecDraft, entry: AutoTagSchemaEntry | undefined): void {
  spec.text = {}; spec.flag = {}; spec.pick = {};
  for (const f of entry?.fields ?? []) {
    if (f.kind === 'boolean') spec.flag[f.name] = true;
    else if (f.kind === 'enum_list') spec.pick[f.name] = [];
    else spec.text[f.name] = '';
  }
}

export function draftFromRule(rule: AutoTag, schema: AutoTagSchemaEntry[]): RuleDraft {
  return {
    name: rule.name, remove: rule.remove_tags_automatically, tagIds: [...rule.tag_ids],
    specs: rule.specifications.map(s => {
      const entry = schema.find(e => e.implementation === s.condition.implementation);
      const spec = newSpec(entry);
      spec.name = s.name; spec.required = s.required; spec.negate = s.negate;
      if (!entry) { spec.implementation = s.condition.implementation; return spec; }
      const c = s.condition as unknown as Record<string, unknown>;
      for (const f of entry.fields) {
        const v = c[f.name];
        if (f.kind === 'boolean') spec.flag[f.name] = v === true;
        else if (f.kind === 'enum_list') spec.pick[f.name] = Array.isArray(v) ? v.map(String) : [];
        else if (Array.isArray(v)) spec.text[f.name] = v.join(', ');
        else spec.text[f.name] = v === null || v === undefined ? '' : String(v);
      }
      return spec;
    }),
  };
}

function fieldValue(f: AutoTagSchemaField, spec: SpecDraft, bound: [number, number] | undefined, label: string): { value: unknown } | { error: string } {
  switch (f.kind) {
    case 'boolean': return { value: spec.flag[f.name] === true };
    case 'enum_list': {
      const picked = spec.pick[f.name] ?? [];
      if (picked.length < 1 || picked.length > MAX_VALUES) return { error: `${label}: choose 1 to ${MAX_VALUES} ${f.label.toLowerCase()}.` };
      return { value: [...picked] };
    }
    case 'text_list': {
      const items = parseTextList(spec.text[f.name] ?? '');
      if (items.length < 1 || items.length > MAX_VALUES) return { error: `${label}: enter 1 to ${MAX_VALUES} values for ${f.label}.` };
      if (items.some(i => bytes(i) > MAX_NAME_BYTES || hasControl(i))) return { error: `${label}: each value must be at most ${MAX_NAME_BYTES} bytes without control characters.` };
      return { value: items };
    }
    case 'integer_list': {
      const items = parseIntList(spec.text[f.name] ?? '');
      if (items === null) return { error: `${label}: ${f.label} must be non-negative whole numbers.` };
      if (items.length < 1 || items.length > MAX_VALUES) return { error: `${label}: enter 1 to ${MAX_VALUES} values for ${f.label}.` };
      return { value: items };
    }
    case 'integer': {
      const raw = (spec.text[f.name] ?? '').trim();
      if (!raw) return f.required ? { error: `${label}: ${f.label} is required.` } : { value: null };
      const n = Number(raw);
      if (!/^-?\d+$/.test(raw) || !Number.isSafeInteger(n)) return { error: `${label}: ${f.label} must be a whole number.` };
      if (bound && (n < bound[0] || n > bound[1])) return { error: `${label}: ${f.label} must be between ${bound[0]} and ${bound[1]}.` };
      return { value: n };
    }
    default: {
      const raw = (spec.text[f.name] ?? '').trim();
      if (!raw || hasControl(raw)) return { error: `${label}: ${f.label} is required.` };
      if (bytes(raw) > MAX_ROOT_BYTES) return { error: `${label}: ${f.label} is too long.` };
      if (spec.implementation === 'root_folder' && !raw.startsWith('/')) return { error: `${label}: ${f.label} must be an absolute path.` };
      return { value: raw };
    }
  }
}

/** Validate a draft against the domain schema and convert it to an API payload. */
export function toInput(draft: RuleDraft, schema: AutoTagSchemaEntry[], catalog: Tag[]): DraftResult {
  const name = draft.name.trim();
  if (!name || bytes(name) > MAX_NAME_BYTES || hasControl(name)) return { ok: false, error: `Rule name must be 1 to ${MAX_NAME_BYTES} bytes without control characters.` };
  const ids = [...new Set(draft.tagIds)];
  if (ids.length < 1 || ids.length > MAX_RULE_TAGS) return { ok: false, error: `Choose 1 to ${MAX_RULE_TAGS} tags.` };
  if (ids.some(id => !catalog.some(t => t.id === id))) return { ok: false, error: 'A selected tag no longer exists. Reload and choose current tags.' };
  if (draft.specs.length < 1 || draft.specs.length > MAX_SPECIFICATIONS) return { ok: false, error: `Add 1 to ${MAX_SPECIFICATIONS} specifications.` };
  const specifications: AutoTagInput['specifications'] = [];
  for (const [index, spec] of draft.specs.entries()) {
    const label = `Specification ${index + 1}`;
    const specName = spec.name.trim();
    if (!specName || bytes(specName) > MAX_NAME_BYTES || hasControl(specName)) return { ok: false, error: `${label}: name must be 1 to ${MAX_NAME_BYTES} bytes without control characters.` };
    const entry = schema.find(e => e.implementation === spec.implementation);
    if (!entry) return { ok: false, error: `${label}: choose an implementation.` };
    const condition: Record<string, unknown> = { implementation: entry.implementation };
    for (const f of entry.fields) {
      const r = fieldValue(f, spec, RANGE_BOUNDS[entry.implementation], label);
      if ('error' in r) return { ok: false, error: r.error };
      condition[f.name] = r.value;
    }
    if (entry.fields.some(f => f.name === 'min') && entry.fields.some(f => f.name === 'max')) {
      const { min, max } = condition as { min: number | null; max: number | null };
      if (min === null && max === null) return { ok: false, error: `${label}: enter a minimum, a maximum or both.` };
      if (min !== null && max !== null && min > max) return { ok: false, error: `${label}: minimum must not exceed maximum.` };
    }
    specifications.push({ name: specName, negate: spec.negate, required: spec.required, condition: condition as unknown as AutoTagCondition });
  }
  return { ok: true, input: { name, remove_tags_automatically: draft.remove, tag_ids: ids, specifications } };
}

/** Human summary of a condition, e.g. "genre is Drama, Anime" or "year 2000–2010". */
export function summarizeCondition(condition: AutoTagCondition): string {
  const c = condition as unknown as Record<string, unknown>;
  const kind = condition.implementation.replace(/_/g, ' ');
  if (Array.isArray(c.values)) return `${kind} is ${c.values.join(', ')}`;
  if (typeof c.path === 'string') return `${kind} is ${c.path}`;
  if (typeof c.value === 'boolean') return `${kind} is ${c.value ? 'yes' : 'no'}`;
  const min = typeof c.min === 'number' ? c.min : null, max = typeof c.max === 'number' ? c.max : null;
  if (min !== null && max !== null) return `${kind} ${min}–${max}`;
  if (min !== null) return `${kind} at least ${min}`;
  if (max !== null) return `${kind} at most ${max}`;
  return kind;
}
export function summarizeSpecification(s: { name: string; negate: boolean; required: boolean; condition: AutoTagCondition }): string {
  return `${s.name}: ${s.negate ? 'not ' : ''}${summarizeCondition(s.condition)}${s.required ? ' (required)' : ''}`;
}
