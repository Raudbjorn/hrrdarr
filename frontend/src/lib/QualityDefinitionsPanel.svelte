<script lang="ts">
  import { onMount } from 'svelte';
  import type { MediaDomain, QualityDefinition, QualityDefinitionLimits } from './api.generated';
  import type { SettingsWriteState } from './api';
  import { listQualityDefinitions, getQualityDefinitionLimits, getQualityDefinitionDefaults, updateQualityDefinitions, resetQualityDefinitions } from './api';
  import { definitionDrafts, definitionUpdates, type DefinitionDraft } from './quality-definition-draft';
  let { onchange = (_domain: MediaDomain) => {}, relatedWrites = {tv:'idle',movies:'idle'}, onwritestate = (_domain: MediaDomain, _state: SettingsWriteState) => {} } = $props<{ onchange?: (domain: MediaDomain) => void; relatedWrites?: Record<MediaDomain,SettingsWriteState>; onwritestate?: (domain:MediaDomain,state:SettingsWriteState)=>void }>();
  let domain: MediaDomain = $state('tv'), rows: QualityDefinition[] = $state([]), defaults: QualityDefinition[] = $state([]), drafts: DefinitionDraft[] = $state([]), limits: QualityDefinitionLimits | null = $state(null);
  let loading = $state(false), busy = $state(false), ready = $state(false), dirty = $state(false), uncertain = $state(false), confirming = $state(false), resetTitles = $state(false), showDefaults = $state(false);
  let error = $state(''), notice = $state(''), epoch = 0, alive = true;
  $effect(()=>{if(relatedWrites[domain]!=='idle')confirming=false;});
  function fill(data: QualityDefinition[]) {rows=data;drafts=definitionDrafts(data);dirty=false;uncertain=false;ready=true;confirming=false;onwritestate(domain,'idle');}
  async function load() {
    if (busy || loading) return;
    if ((dirty || uncertain) && !window.confirm('Reload saved quality settings and discard this draft? Check saved values before starting another write.')) return;
    const version=++epoch, scope=domain;loading=true;confirming=false;ready=false;error='';notice='';
    const [current,bounds,original]=await Promise.all([listQualityDefinitions(scope),getQualityDefinitionLimits(scope),getQualityDefinitionDefaults(scope)]);
    if (!alive || version!==epoch) return;
    loading=false;
    if (!current.ok || !bounds.ok || !original.ok) {error=[current,bounds,original].filter(r=>!r.ok).map(r=>r.ok?'':r.error).join(' ');return;}
    limits=bounds.data;defaults=original.data;fill(current.data);
  }
  async function changeDomain(next: MediaDomain) {
    if (busy || loading || uncertain || next===domain) return;
    if ((dirty || uncertain) && !window.confirm('Discard this quality draft and change media domain?')) return;
    domain=next;rows=[];drafts=[];limits=null;defaults=[];dirty=false;uncertain=false;confirming=false;resetTitles=false;await load();
  }
  async function save() {
    if (busy || loading || uncertain || relatedWrites[domain]!=='idle' || !ready || !limits) return;
    const input=definitionUpdates(drafts,rows,limits);
    if (!input.ok) {error=input.error;return;}
    if (!input.data.length) {notice='No changes to save.';return;}
    const scope=domain,version=epoch;busy=true;confirming=false;error='';notice='';onwritestate(scope,'busy');
    const result=await updateQualityDefinitions(scope,input.data);
    if (!alive || version!==epoch) return;
    busy=false;
    if (!result.ok) {error=result.error;uncertain=result.status===undefined || result.status>=500;onwritestate(scope,uncertain?'uncertain':'idle');if(uncertain) onchange(scope);return;}
    fill(result.data);notice='Quality settings saved.';onchange(scope);
  }
  function requestReset() {if(!ready || busy || loading || uncertain || relatedWrites[domain]!=='idle')return;confirming=true;resetTitles=false;}
  async function reset() {
    if(!confirming || !ready || busy || loading || uncertain || relatedWrites[domain]!=='idle')return;
    const scope=domain,version=epoch;busy=true;confirming=false;error='';notice='';onwritestate(scope,'busy');
    const result=await resetQualityDefinitions(scope,resetTitles);
    if(!alive || version!==epoch)return;
    busy=false;
    if(!result.ok){error=result.error;uncertain=result.status===undefined || result.status>=500;onwritestate(scope,uncertain?'uncertain':'idle');if(uncertain)onchange(scope);return;}
    fill(result.data);notice='Quality settings reset.';onchange(scope);
  }
  const value = (number: number | null | undefined) => number == null ? 'Unset' : String(number);
  onMount(()=>{void load();return()=>{alive=false;++epoch;};});
</script>
<section aria-label="Global quality settings" class="quality-settings">
  <h2>Global quality settings</h2>
  <nav aria-label="Quality settings media"><button disabled={loading || busy || uncertain} aria-pressed={domain==='tv'} onclick={()=>changeDomain('tv')}>TV</button><button disabled={loading || busy || uncertain} aria-pressed={domain==='movies'} onclick={()=>changeDomain('movies')}>Movies</button></nav>
  <p>Edit display titles and global size limits. Quality identity and ordering stay fixed. Blank means unset, not zero. Changes are saved together; concurrent edits by other users are not detected.</p>
  {#if domain==='tv'}<p>Saving a changed TV definition with any size set copies all three sizes, including blanks, into every matching TV profile item. An all-blank size edit leaves profile overrides unchanged. Title-only edits also copy sizes when any is set.</p>{:else}<p>Movie settings do not change profile size overrides.</p>{/if}
  {#if limits}<p>Sizes: {limits.min}–{limits.max} {limits.unit}. Minimum ≤ preferred ≤ maximum for supplied values.</p>{/if}
  {#if loading}<p role="status">Loading quality settings…</p>{/if}{#if busy}<p role="status">Saving quality settings…</p>{/if}
  {#if error}<p role="alert">{error}</p>{/if}{#if notice}<p role="status">{notice}</p>{/if}
  {#if relatedWrites[domain]!=='idle'}<p role="status">Quality profile write {relatedWrites[domain]==='busy'?'in progress':'outcome unknown'}. Global quality writes are blocked until it completes or is reconciled in Quality profiles.</p>{/if}
  {#if uncertain}<p role="alert">The write outcome is unknown. Your draft is retained and retries are blocked. Reload saved settings to reconcile before making another write.</p>{/if}
  <button disabled={loading || busy} onclick={load}>Reload saved settings</button>
  <label><input type="checkbox" bind:checked={showDefaults} /> Show catalog defaults</label>
  {#if ready && !drafts.length}<p>No quality definitions in this media domain.</p>{/if}
  <form onsubmit={event=>{event.preventDefault();void save();}} oninput={()=>{dirty=true;confirming=false;}}>
    <fieldset disabled={!ready || loading || busy || uncertain || relatedWrites[domain]!=='idle'}><legend>{domain==='tv'?'TV':'Movie'} quality definitions</legend>
      {#each drafts as draft (draft.id)}
        {@const metadata=rows.find(row=>row.id===draft.id)}
        {@const original=defaults.find(row=>row.id===draft.id)}
        <fieldset class="definition"><legend>{metadata?.quality.name ?? `Quality ${draft.id}`}</legend>
          <label>Display title <input aria-label={`${metadata?.quality.name} display title`} maxlength="100" required bind:value={draft.title} /></label>
          <label>Minimum <input aria-label={`${metadata?.quality.name} minimum size`} inputmode="decimal" bind:value={draft.min_size} /></label>
          <label>Preferred <input aria-label={`${metadata?.quality.name} preferred size`} inputmode="decimal" bind:value={draft.preferred_size} /></label>
          <label>Maximum <input aria-label={`${metadata?.quality.name} maximum size`} inputmode="decimal" bind:value={draft.max_size} /></label>
          {#if showDefaults && original}<p>Defaults: {original.title}; minimum {value(original.min_size)}, preferred {value(original.preferred_size)}, maximum {value(original.max_size)} {limits?.unit}.</p>{/if}
        </fieldset>
      {/each}
      <button type="submit" disabled={!drafts.length}>Save quality settings</button><button type="button" disabled={!drafts.length} onclick={requestReset}>Reset quality settings</button>
    </fieldset>
  </form>
  {#if confirming}<div role="group" aria-label="Confirm quality reset">
    <p>{domain==='tv'?'TV reset preserves all sizes. Only selected title resets change saved settings.':'Movie reset restores all default size limits.'} Unsaved changes will be discarded.</p>
    <label><input type="checkbox" bind:checked={resetTitles} disabled={loading || busy || uncertain} /> Also reset display titles</label>
    <button disabled={loading || busy || uncertain || relatedWrites[domain]!=='idle'} onclick={reset}>Confirm quality reset</button><button disabled={busy} onclick={()=>confirming=false}>Cancel quality reset</button>
  </div>{/if}
  {#if dirty}<p role="status">Unsaved quality changes</p>{/if}
</section>
<style>.quality-settings{max-width:1100px;margin:1rem auto}fieldset{border:1px solid #777;margin:1rem 0;padding:1rem}.definition{display:flex;flex-wrap:wrap;gap:.75rem}.definition p{flex-basis:100%}label{display:block;margin:.4rem 0}input{display:block;max-width:16rem}input[type=checkbox]{display:inline}button{margin:.3rem}[role=alert]{color:#b42318}</style>
