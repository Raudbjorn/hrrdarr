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

export function exportNativeCustomFormat(draft: CustomFormatInput, schema: CustomFormatSchema): Result<string> {
  const text=JSON.stringify(draft,null,2);
  // Validate the actual exported bytes, including pretty-print expansion, for roundtrip safety.
  const checked=parseCustomFormatDraft(text,schema);
  return checked.ok ? {ok:true,data:text} : checked;
}

const communityKinds = {
  ReleaseTitleSpecification: 'release_title', ReleaseGroupSpecification: 'release_group',
  EditionSpecification: 'edition', LanguageSpecification: 'language', SizeSpecification: 'size',
  SourceSpecification: 'source', ResolutionSpecification: 'resolution', QualityModifierSpecification: 'quality_modifier',
  IndexerFlagSpecification: 'indexer_flag', ReleaseTypeSpecification: 'release_type', YearSpecification: 'year',
} as const;
const allowedKeys = (value: Record<string, unknown>, allowed: string[]) => Object.keys(value).every(key=>allowed.includes(key));

// Source specifications default to numeric zero, boolean false and null regex strings.
// Native editor examples are not interchange defaults. No partially converted graph escapes.
export function parseCommunityCustomFormat(text: string, schema: CustomFormatSchema): Result<CustomFormatInput> {
  const invalid = (error: string): Result<CustomFormatInput> => ({ok:false,error});
  if (new TextEncoder().encode(text).length > MAX_JSON_BYTES) return invalid('JSON exceeds 72 KiB.');
  let document: unknown;
  try { document=JSON.parse(text); } catch { return invalid('Invalid JSON.'); }
  if (!object(document)) return invalid('Invalid community format document.');
  // Both source import paths project the editable contract; community metadata is not model state.
  // Discard outer/spec presentation and identity keys, but validate every fields-object option.
  const rows=document.specifications ?? [];
  if (!Array.isArray(rows) || rows.length>schema.max_specifications) return invalid('Invalid community specifications.');
  const specifications: unknown[]=[];
  // Specification flags differ: Sonarr defaults null; Radarr's nonnullable bools reject it.
  // The root renaming flag is nullable in Radarr too and independently defaults to false.
  const flag=(value:Record<string,unknown>,key:string)=>schema.media_type==='tv' ? value[key] ?? false : Object.hasOwn(value,key) ? value[key] : false;
  for(const row of rows) {
    if (!object(row) || typeof row.implementation!=='string' || !Object.hasOwn(communityKinds,row.implementation)) return invalid('Unknown community condition or condition fields.');
    const kind=communityKinds[row.implementation as keyof typeof communityKinds];
    if (!schema.conditions.some(condition=>condition.kind===kind)) return invalid('Community condition is unsupported in this library.');
    const fields=schema.media_type==='tv' ? row.fields ?? {} : Object.hasOwn(row,'fields') ? row.fields : {};
    const options=kind==='size'||kind==='year' ? ['min','max'] : kind==='language' ? ['value','exceptLanguage'] : ['value'];
    if (!object(fields) || !allowedKeys(fields,options)) return invalid('Unknown community condition option.');
    const field=(key:string, fallback:unknown)=>Object.hasOwn(fields,key) ? fields[key] : fallback;
    const condition=['release_title','release_group','edition'].includes(kind) ? {kind,pattern:field('value',null)}
      : kind==='size' ? {kind,min_gib:field('min',0),max_gib:field('max',0)}
      : kind==='year' ? {kind,min:field('min',0),max:field('max',0)}
      : kind==='language' ? {kind,value:field('value',0),except_language:field('exceptLanguage',false)}
      : {kind,value:field('value',0)};
    specifications.push({name:row.name,negate:flag(row,'negate'),required:flag(row,'required'),condition});
  }
  const result=parseCustomFormatDraft(JSON.stringify({name:document.name ?? '',include_when_renaming:document.includeCustomFormatWhenRenaming ?? false,specifications}),schema);
  return result.ok ? result : invalid(`Community definition cannot be represented without changes: ${result.error} No conditions were imported. Complete omitted fields rather than relying on native example values.`);
}

export function exportCommunityCustomFormat(draft: CustomFormatInput, schema: CustomFormatSchema): Result<string> {
  const checked=parseCustomFormatDraft(JSON.stringify(draft),schema);
  if (!checked.ok) return checked;
  const specifications=[];
  for (const specification of checked.data.specifications) {
    const condition=specification.condition;
    const mapping=Object.entries(communityKinds).find(([,kind])=>kind===condition.kind);
    if (!mapping) return {ok:false,error:'Condition has no supported community representation.'};
    const implementation=mapping[0];
    const fields='pattern' in condition ? {value:condition.pattern}
      : condition.kind==='size' ? {min:condition.min_gib,max:condition.max_gib}
      : condition.kind==='year' ? {min:condition.min,max:condition.max}
      : condition.kind==='language' ? {value:condition.value,exceptLanguage:condition.except_language}
      : {value:condition.value};
    specifications.push({name:specification.name,implementation,negate:specification.negate,required:specification.required,fields});
  }
  const text=JSON.stringify({name:checked.data.name,includeCustomFormatWhenRenaming:checked.data.include_when_renaming,specifications},null,2);
  if(new TextEncoder().encode(text).length>MAX_JSON_BYTES) return {ok:false,error:'Export exceeds the 72 KiB interchange limit.'};
  return {ok:true,data:text};
}
