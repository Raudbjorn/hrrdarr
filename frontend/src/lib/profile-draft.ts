import type { QualityProfileInput, QualityProfileItemInput, QualityProfileLeafInput } from './api.generated';

export type Root = { key: string; item: QualityProfileItemInput };
export type Draft = { name: string; roots: Root[]; policy: QualityProfileInput['policy']; cutoff: string };
let sequence = 0;
const key = () => `root-${++sequence}`;
export function fromInput(input: QualityProfileInput): Draft {
  const copy = JSON.parse(JSON.stringify(input)) as QualityProfileInput;
  const roots = copy.items.map(item => ({ key: key(), item }));
  const cutoff = copy.policy?.cutoff;
  return { name: copy.name, roots, policy: copy.policy ?? null, cutoff: cutoff ? (cutoff.kind === 'group' ? roots[cutoff.position]?.key : roots.find(r => r.item.kind === 'quality' && r.item.quality_id === cutoff.quality_id)?.key) ?? '' : '' };
}
export function toInput(draft: Draft): QualityProfileInput {
  const position = draft.roots.findIndex(r => r.key === draft.cutoff);
  const item = draft.roots[position]?.item;
  return { name: draft.name, items: draft.roots.map(r => r.item), policy: draft.policy ? { ...draft.policy, cutoff: item?.kind === 'group' ? { kind: 'group', position } : { kind: 'quality', quality_id: item?.kind === 'quality' ? item.quality_id : -1 } } : null };
}
export function reorder<T>(items: T[], index: number, delta: number): T[] {
  const next = index + delta;
  if (next < 0 || next >= items.length) return items;
  const copy = [...items]; [copy[index], copy[next]] = [copy[next], copy[index]]; return copy;
}
export function addGroup(draft: Draft, name: string): void {
  draft.roots.push({ key: key(), item: { kind: 'group', name, allowed: false, items: [] } });
}
export function dissolve(draft: Draft, index: number): void {
  const root = draft.roots[index];
  if (root.item.kind !== 'group') return;
  const allowed = root.item.allowed;
  draft.roots.splice(index, 1, ...root.item.items.map(leaf => ({ key: key(), item: { kind: 'quality' as const, ...leaf, allowed: allowed && leaf.allowed } })));
  if (draft.cutoff === root.key) draft.cutoff = '';
}
export function moveLeaf(draft: Draft, source: number, child: number | null, destination: string): void {
  const root = draft.roots[source];
  let leaf: QualityProfileLeafInput;
  if (root.item.kind === 'quality') {
    const { kind: _, ...value } = root.item; leaf = value;
    draft.roots.splice(source, 1);
    if (draft.cutoff === root.key) draft.cutoff = '';
  } else {
    if (child === null) return;
    root.item.items.forEach(l => l.allowed = root.item.allowed && l.allowed);
    leaf = root.item.items.splice(child, 1)[0];
    root.item.allowed = root.item.items.some(l => l.allowed);
  }
  const target = draft.roots.find(r => r.key === destination);
  if (target?.item.kind === 'group') {
    target.item.items.forEach(l => l.allowed = target.item.allowed && l.allowed);
    target.item.items.push(leaf);
    target.item.allowed = target.item.items.some(l => l.allowed);
  } else draft.roots.push({ key: key(), item: { kind: 'quality', ...leaf } });
}
export function setAllowed(root: Root, allowed: boolean): void {
  root.item.allowed = allowed;
  if (root.item.kind === 'group') root.item.items.forEach(leaf => leaf.allowed = allowed);
}
export function validateDraft(draft: Draft, domain: 'tv' | 'movies'): string | null {
  if (!draft.name.trim()) return 'Enter a profile name.';
  if (!draft.roots.length) return 'Add at least one quality.';
  if (draft.roots.reduce((n,r) => n + 1 + (r.item.kind === 'group' ? r.item.items.length : 0),0) > 64) return 'Profiles support at most 64 quality and group nodes.';
  for (const root of draft.roots) {
    if (root.item.kind === 'group' && (!root.item.name.trim() || !root.item.items.length)) return 'Name each group and move at least one quality into it.';
    for (const leaf of root.item.kind === 'group' ? root.item.items : [root.item]) {
      const values = [leaf.min_size, leaf.preferred_size, leaf.max_size];
      if (values.some(v => v != null && (!Number.isFinite(v) || v < 0 || v > (domain === 'tv' ? 1000 : 2000)))) return 'Size limits must be finite and within the displayed range.';
      const defined = values.filter((v): v is number => v != null);
      if (defined.some((v,i) => i > 0 && v < defined[i-1])) return 'Size limits must satisfy minimum ≤ preferred ≤ maximum.';
    }
  }
  if (draft.policy) {
    const selected = draft.roots.find(r => r.key === draft.cutoff);
    if (!selected?.item.allowed) return 'Select an allowed top-level quality or group as cutoff.';
    const p = draft.policy;
    if ([p.min_format_score,p.cutoff_format_score,p.min_upgrade_format_score,...p.format_items.map(f=>f.score)].some(v => !Number.isInteger(v) || v < -2147483648 || v > 2147483647)) return 'Scores must be signed 32-bit integers.';
    if (p.min_upgrade_format_score < 1) return 'Minimum score improvement must be at least one.';
  }
  return null;
}
