import type { QualityDefinition, QualityDefinitionLimits, QualityDefinitionUpdate } from './api.generated';
export type DefinitionDraft = { id: number; title: string; min_size: string; max_size: string; preferred_size: string };
export function definitionDrafts(rows: QualityDefinition[]): DefinitionDraft[] {
  return rows.map(row => ({ id: row.id, title: row.title, min_size: row.min_size == null ? '' : String(row.min_size), max_size: row.max_size == null ? '' : String(row.max_size), preferred_size: row.preferred_size == null ? '' : String(row.preferred_size) }));
}
export function definitionUpdates(drafts: DefinitionDraft[], baseline: QualityDefinition[], limits: QualityDefinitionLimits): { ok: true; data: QualityDefinitionUpdate[] } | { ok: false; error: string } {
  const updates: QualityDefinitionUpdate[] = [];
  const seen = new Set<number>();
  for (const row of drafts) {
    const original = baseline.find(item => item.id === row.id);
    if (!original || seen.has(row.id)) return {ok:false,error:'The quality catalog changed. Reload before saving.'};
    seen.add(row.id);
    if (!row.title.trim() || [...row.title].length > 100 || /[\p{Cc}]/u.test(row.title)) return {ok:false,error:`Quality ${row.id}: enter a title of 1–100 characters without control characters.`};
    const values: (number | null)[] = [];
    for (const key of ['min_size','preferred_size','max_size'] as const) {
      const raw = row[key].trim();
      if (raw === '') {values.push(null); continue;}
      const value = Number(raw);
      if (!/^[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:e[+-]?\d+)?$/i.test(raw) || !Number.isFinite(value) || value < limits.min || value > limits.max) return {ok:false,error:`${row.title}: sizes must be blank or finite numbers from ${limits.min} to ${limits.max} ${limits.unit}.`};
      values.push(value);
    }
    const supplied = values.filter((v): v is number => v !== null);
    if (supplied.some((v,index) => index > 0 && v < supplied[index-1])) return {ok:false,error:`${row.title}: minimum ≤ preferred ≤ maximum is required for supplied values.`};
    const [min_size,preferred_size,max_size] = values;
    if (row.title !== original.title || min_size !== original.min_size || preferred_size !== original.preferred_size || max_size !== original.max_size) updates.push({id:row.id,title:row.title,min_size,preferred_size,max_size});
  }
  return {ok:true,data:updates};
}
