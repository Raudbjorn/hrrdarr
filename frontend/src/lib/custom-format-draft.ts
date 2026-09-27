import type { CustomFormatInput, CustomFormatSchema } from './api.generated';
import type { Result } from './api';
const MAX_JSON_BYTES = 73728;
const object = (value: unknown): value is Record<string, unknown> => !!value && typeof value === 'object' && !Array.isArray(value);
const keys = (value: Record<string, unknown>, expected: string[]) => Object.keys(value).length === expected.length && expected.every(key => Object.hasOwn(value, key));
const name = (value: unknown, condition = false) => typeof value === 'string' && value.trim().length > 0 && [...value].length <= 100 && !(condition ? /\u0000/ : /[\u0000-\u001f\u007f]/).test(value);
// Only accept the native editable DTO; IDs/domain are chosen outside the document.
// Server-side regex and policy validation still run on Save, before persistence.
export function parseCustomFormatDraft(text: string, schema: CustomFormatSchema): Result<CustomFormatInput> {
  const invalid = (error: string): Result<CustomFormatInput> => ({ok:false,error});
  if (new TextEncoder().encode(text).length > MAX_JSON_BYTES) return invalid('JSON exceeds 72 KiB.');
  let value: unknown;
  try { value = JSON.parse(text); } catch { return invalid('Invalid JSON.'); }
  if (!object(value) || !keys(value,['name','include_when_renaming','specifications']) || !name(value.name) || typeof value.include_when_renaming !== 'boolean' || !Array.isArray(value.specifications) || value.specifications.length < 1 || value.specifications.length > schema.max_specifications) return invalid('Expected a native format name, renaming flag and 1–64 conditions.');
  for (const specification of value.specifications) {
    if (!object(specification) || !keys(specification,['name','negate','required','condition']) || !name(specification.name, true) || typeof specification.negate !== 'boolean' || typeof specification.required !== 'boolean' || !object(specification.condition)) return invalid('Invalid condition name, flags or fields.');
    const condition=specification.condition;
    const template=schema.conditions.find(c=>c.kind===condition.kind);
    if (!template || !keys(condition,Object.keys(template))) return invalid('Condition kind or fields are unsupported in this library.');
    for (const [key, example] of Object.entries(template)) {
      const field=condition[key];
      if (typeof field !== typeof example || (typeof field==='number' && (!Number.isFinite(field) || (Number.isInteger(example) && !['min_gib','max_gib'].includes(key) && !Number.isSafeInteger(field))))) return invalid('Condition parameters have invalid types.');
    }
    if ('value' in condition && !schema.choices[String(condition.kind)]?.some(choice=>choice.value===condition.value)) return invalid('Condition value is outside this library’s choices.');
    if ('pattern' in condition && (typeof condition.pattern !== 'string' || condition.pattern.length===0 || condition.pattern.includes('\0') || new TextEncoder().encode(condition.pattern).length>4096)) return invalid('Pattern must contain 1–4096 bytes.');
    if (condition.kind==='size' && (!(Number(condition.min_gib)>=0) || !(Number(condition.max_gib)>Number(condition.min_gib)))) return invalid('Maximum size must exceed the nonnegative minimum.');
    if (condition.kind==='year' && (Number(condition.min)<=0 || Number(condition.max)>2147483647 || Number(condition.max)<Number(condition.min))) return invalid('Year range is invalid.');
  }
  return {ok:true,data:value as unknown as CustomFormatInput};
}
