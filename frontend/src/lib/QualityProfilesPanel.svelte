<script lang="ts">
  import { onMount } from 'svelte';
  import type { MediaDomain, QualityProfilePage, QualityProfileInput, QualityDefinition, CustomFormat, CustomFormatChoice, QualityProfileLeafInput } from './api.generated';
  import type { SettingsWriteState } from './api';
  import { listQualityProfiles, getQualityProfile, getQualityProfileSchema, listQualityDefinitions, createQualityProfile, updateQualityProfile, deleteQualityProfile, listCustomFormats, getCustomFormatSchema } from './api';
  import { fromInput, toInput, reorder, addGroup, dissolve, moveLeaf, setAllowed, validateDraft, type Draft } from './profile-draft';
  let { onchange = (_domain: MediaDomain) => {}, catalogVersions = {tv:0,movies:0}, relatedWrites = {tv:'idle',movies:'idle'}, onwritestate = (_domain:MediaDomain,_state:SettingsWriteState)=>{} } = $props<{ onchange?: (domain: MediaDomain) => void; catalogVersions?: Record<MediaDomain,number>; relatedWrites?: Record<MediaDomain,SettingsWriteState>; onwritestate?: (domain:MediaDomain,state:SettingsWriteState)=>void }>();
  let domain: MediaDomain = $state('tv'), page: QualityProfilePage | null = $state(null), draft: Draft | null = $state(null), id: number | null = $state(null);
  let catalog: QualityDefinition[] = $state([]), formats: CustomFormat[] = $state([]), languages: CustomFormatChoice[] = $state([]), template: QualityProfileInput | null = $state(null);
  let loading = $state(false), busy = $state(false), dirty = $state(false), uncertain = $state(false), error = $state(''), notice = $state(''), deleting = $state(false);
  let stale = $state(false);
  const seenCatalogVersions = {tv:0,movies:0};
  $effect(() => {
    const revision = catalogVersions[domain];
    if (revision !== seenCatalogVersions[domain]) {
      seenCatalogVersions[domain] = revision;
      deleting = false;
      if (draft) stale = true;
    }
  });
  $effect(()=>{if(relatedWrites[domain]!=='idle')deleting=false;});
  let groupName = $state(''), addQuality = $state('');
  let alive = true, epoch = 0;
  const title = (quality: number) => catalog.find(d => d.quality.id === quality)?.title ?? `Unknown quality ${quality}`;
  const used = $derived.by(() => new Set(draft?.roots.flatMap(r => r.item.kind === 'group' ? r.item.items.map(l => l.quality_id) : [r.item.quality_id]) ?? []));
  function discard(): boolean { return !dirty || window.confirm('Discard unsaved profile changes?'); }
  async function load(offset = 0) {
    if (busy || loading) return;
    deleting = false;
    const version = ++epoch, scope = domain; loading = true; error = '';
    const [p,c,f,s,t] = await Promise.all([listQualityProfiles(scope,offset),listQualityDefinitions(scope),listCustomFormats(scope),getCustomFormatSchema(scope),getQualityProfileSchema(scope)]);
    if (!alive || version !== epoch) return;
    loading = false;
    if (!p.ok || !c.ok || !f.ok || !s.ok || !t.ok) { error = [p,c,f,s,t].filter(r => !r.ok).map(r => r.ok ? '' : r.error).join(' '); return; }
    page = p.data; catalog = c.data; formats = f.data; languages = s.data.choices.language ?? []; template = t.data;
  }
  async function changeDomain(next: MediaDomain) {
    if (next === domain || busy || loading || uncertain || !discard()) return;
    domain = next; draft = null; id = null; stale = false; dirty = false; uncertain = false; page = null; template = null; await load();
  }
  function fill(input: QualityProfileInput, profileId: number | null) {
    draft = fromInput(input); id = profileId; stale = false; dirty = false; uncertain = false; deleting = false; error = ''; notice = ''; onwritestate(domain,'idle');
    if (draft.policy) {
      const scores = new Map(draft.policy.format_items.map(f => [f.format_id,f.score]));
      // Preserve unknown references so stale/deleted formats cannot silently lose a configured score.
      for (const format of formats) if (!scores.has(format.id)) draft.policy.format_items.push({format_id:format.id,score:0});
    }
  }
  async function edit(profileId: number, duplicate = false) {
    if (busy || loading || !discard()) return;
    deleting = false;
    const version = ++epoch, catalogVersion = catalogVersions[domain]; loading = true; error = '';
    const [result, definitions] = await Promise.all([getQualityProfile(domain,profileId), listQualityDefinitions(domain)]);
    if (!alive || version !== epoch) return;
    loading = false;
    if (!result.ok || !definitions.ok) { error = !result.ok ? result.error : !definitions.ok ? definitions.error : ''; return; }
    catalog = definitions.data;
    fill({...result.data,name:duplicate ? `${result.data.name} copy` : result.data.name},duplicate ? null : profileId); dirty = duplicate; stale = catalogVersion !== catalogVersions[domain] || relatedWrites[domain]!=='idle';
  }
  function create() { if (!busy && !loading && !uncertain && template && discard()) { fill(template,null); dirty = true; } }
  function configure() {
    if (!draft || !template?.policy) return;
    draft.policy = JSON.parse(JSON.stringify(template.policy)); draft.cutoff = ''; dirty = true;
  }
  async function save() {
    if (!draft || busy || loading || uncertain || stale || relatedWrites[domain]!=='idle') return;
    const problem = validateDraft(draft,domain);
    if (problem) {error = problem; return;}
    const input = toInput(draft), scope = domain, version = epoch, catalogVersion = catalogVersions[domain];
    deleting = false; busy = true; error = ''; notice = ''; onwritestate(scope,'busy');
    const result = id === null ? await createQualityProfile(scope,input) : await updateQualityProfile(scope,id,input);
    if (!alive || version !== epoch) return;
    busy = false;
    if (!result.ok) { error = result.error; uncertain = result.status === undefined || result.status >= 500; onwritestate(scope,uncertain?'uncertain':'idle'); return; }
    fill(result.data,result.data.id); stale = catalogVersion !== catalogVersions[domain] || relatedWrites[domain]!=='idle'; notice = 'Profile saved.'; onchange(scope); await load(page?.offset ?? 0);
  }
  async function remove() {
    if (id === null || busy || loading || uncertain || stale || relatedWrites[domain]!=='idle' || !deleting) return;
    const scope = domain, version = epoch; busy = true; error = ''; onwritestate(scope,'busy');
    const result = await deleteQualityProfile(scope,id);
    if (!alive || version !== epoch) return;
    busy = false; deleting = false;
    if (!result.ok) { error = result.error; uncertain = result.status === undefined || result.status >= 500; onwritestate(scope,uncertain?'uncertain':'idle'); return; }
    draft = null; id = null; stale = false; dirty = false; notice = 'Profile deleted.'; onwritestate(scope,'idle'); onchange(scope); await load();
  }
  function insertQuality() {
    if (!draft || addQuality === '') return;
    const qualityId = Number(addQuality);
    const leaf = template?.items.flatMap(i => i.kind === 'group' ? i.items : [i]).find(l => l.quality_id === qualityId);
    if (leaf && !used.has(qualityId)) { const single = fromInput({name:'',items:[{...leaf,kind:'quality'}]}); draft.roots.push(single.roots[0]); dirty = true; addQuality = ''; }
  }
  function childAllowed(rootIndex: number) {
    if (!draft) return; const item = draft.roots[rootIndex].item;
    if (item.kind === 'group') item.allowed = item.items.some(l => l.allowed);
    dirty = true;
  }
  onMount(() => { void load(); return () => { alive = false; ++epoch; }; });
</script>

<section aria-label="Quality profiles" class="profiles">
  <h2>Quality profiles</h2>
  <p>Configure TV and movie quality preferences separately. Higher entries in this editor are lower priority; move qualities down to prefer them.</p>
  <nav aria-label="Profile media type"><button disabled={busy || loading || uncertain} aria-pressed={domain === 'tv'} onclick={() => changeDomain('tv')}>TV</button><button disabled={busy || loading || uncertain} aria-pressed={domain === 'movies'} onclick={() => changeDomain('movies')}>Movies</button></nav>
  {#if error}<p role="alert">{error}</p>{/if}
  {#if notice}<p role="status">{notice}</p>{/if}
  {#if loading}<p role="status">Loading profiles…</p>{/if}
  {#if relatedWrites[domain]!=='idle'}<p role="status">Global quality write {relatedWrites[domain]==='busy'?'in progress':'outcome unknown'}. Profile writes are blocked until it completes or is reconciled in Quality settings.</p>{/if}
  {#if stale}<p role="alert">Global quality settings changed. This profile draft is retained but cannot be saved or deleted. Reopen the profile to read its current sizes, or explicitly create a new profile.</p>{/if}
  {#if uncertain}<p role="alert">The write outcome is unknown. Your draft is retained and retries are blocked. Refresh the list and reopen the saved profile to reconcile before making another write.</p><button disabled={busy || loading} onclick={() => {if(window.confirm('Have you checked the saved profiles? Discard this uncertain draft and start a new action?')) {draft=null;id=null;dirty=false;uncertain=false;stale=false;onwritestate(domain,'idle');}}}>Discard uncertain draft after checking saved profiles</button>{/if}
  <button disabled={busy || loading} onclick={() => load(page?.offset ?? 0)}>Refresh list and catalogs</button>
  <button disabled={busy || loading || !template || uncertain} onclick={create}>New profile</button>
  {#if page}
    <ul>{#each page.items as profile (profile.id)}<li><strong>{profile.name}</strong> <button disabled={busy || loading} onclick={() => edit(profile.id)}>Edit {profile.name}</button> <button disabled={busy || loading || uncertain} onclick={() => edit(profile.id,true)}>Duplicate {profile.name}</button></li>{/each}</ul>
    {#if page.total === 0}<p>No profiles in this media domain. Create a profile to configure quality preferences.</p>{/if}
    <p>{page.total} profiles</p><button disabled={busy || loading || page.offset === 0} onclick={() => load(Math.max(0,page!.offset-page!.limit))}>Previous profiles</button><button disabled={busy || loading || page.offset+page.limit >= page.total} onclick={() => load(page!.offset+page!.limit)}>Next profiles</button>
  {/if}
  {#if draft}
    <form onsubmit={event => {event.preventDefault(); void save();}} oninput={() => dirty = true}>
      <fieldset disabled={busy || loading || uncertain || stale || relatedWrites[domain]!=='idle'}>
        <legend>{id === null ? 'Create profile' : 'Edit profile'}</legend>
        <label>Profile name <input required maxlength="100" bind:value={draft.name} /></label>
        <p>Changes are saved together. Concurrent edits by other users are not detected by this API.</p>
        <h3>Qualities and groups</h3>
        <label>Add missing quality <select bind:value={addQuality}><option value="">Choose quality</option>{#each catalog.filter(c => !used.has(c.quality.id)) as definition}<option value={String(definition.quality.id)}>{definition.title}</option>{/each}</select></label><button type="button" onclick={insertQuality}>Add quality</button>
        <label>New group name <input bind:value={groupName} /></label><button type="button" disabled={!groupName.trim()} onclick={() => {addGroup(draft!,groupName); groupName=''; dirty=true;}}>Create group</button>
        <ol>
          {#each draft.roots as root,index (root.key)}
            <li class="quality-row">
              <label><input type="checkbox" checked={root.item.allowed} onchange={event => {setAllowed(root,event.currentTarget.checked); dirty=true;}} /> Allow {root.item.kind === 'quality' ? title(root.item.quality_id) : root.item.name}</label>
              <button type="button" disabled={index === 0} aria-label={`Move ${root.item.kind === 'quality' ? title(root.item.quality_id) : root.item.name} up`} onclick={() => {draft!.roots=reorder(draft!.roots,index,-1);dirty=true;}}>↑</button>
              <button type="button" disabled={index === draft.roots.length-1} aria-label={`Move ${root.item.kind === 'quality' ? title(root.item.quality_id) : root.item.name} down`} onclick={() => {draft!.roots=reorder(draft!.roots,index,1);dirty=true;}}>↓</button>
              {#if root.item.kind === 'group'}
                <label>Group name <input bind:value={root.item.name} /></label><button type="button" onclick={() => {dissolve(draft!,index);dirty=true;}}>Dissolve {root.item.name}</button>
                <ol>{#each root.item.items as leaf,child (leaf.quality_id)}<li>
                  <label><input type="checkbox" checked={leaf.allowed} onchange={event => {leaf.allowed=event.currentTarget.checked;childAllowed(index);}} /> Allow {title(leaf.quality_id)}</label>
                  <button type="button" disabled={child === 0} aria-label={`Move ${title(leaf.quality_id)} up in group`} onclick={() => {if(root.item.kind==='group') root.item.items=reorder(root.item.items,child,-1);dirty=true;}}>↑</button>
                  <button type="button" disabled={child === root.item.items.length-1} aria-label={`Move ${title(leaf.quality_id)} down in group`} onclick={() => {if(root.item.kind==='group') root.item.items=reorder(root.item.items,child,1);dirty=true;}}>↓</button>
                  {@render sizes(leaf)}{@render move(index,child,leaf.quality_id)}
                </li>{/each}</ol>
              {:else}{@render sizes(root.item)}{@render move(index,null,root.item.quality_id)}{/if}
            </li>
          {/each}
        </ol>
        {#if draft.policy}
          <h3>Upgrade policy</h3>
          <label><input type="checkbox" bind:checked={draft.policy.upgrade_allowed} /> Allow upgrades</label>
          <label>Quality cutoff <select aria-label="Quality cutoff" bind:value={draft.cutoff}><option value="">Select allowed quality or group</option>{#each draft.roots.filter(r => r.item.allowed) as root}<option value={root.key}>{root.item.kind==='group' ? root.item.name : title(root.item.quality_id)}</option>{/each}</select></label>
          {#if domain === 'movies'}<label>Movie language <select bind:value={draft.policy.language_id}>{#each languages as language}<option value={language.value}>{language.label}</option>{/each}</select></label>{/if}
          <label>Minimum custom format score <input type="number" step="1" bind:value={draft.policy.min_format_score} /></label>
          <label>Custom format upgrade cutoff <input type="number" step="1" bind:value={draft.policy.cutoff_format_score} /></label>
          <label>Minimum score improvement <input type="number" min="1" step="1" bind:value={draft.policy.min_upgrade_format_score} /></label>
          <h3>Custom format scores</h3>
          {#if draft.policy.format_items.length === 0}<p>No custom formats in this domain.</p>{/if}
          {#each draft.policy.format_items as score (score.format_id)}<label>{formats.find(f=>f.id===score.format_id)?.name ?? `Unavailable format ${score.format_id}`} score <input type="number" step="1" bind:value={score.score} /></label>{#if !formats.some(f=>f.id===score.format_id)}<button type="button" onclick={() => {draft!.policy!.format_items=draft!.policy!.format_items.filter(f=>f.format_id!==score.format_id);dirty=true;}}>Remove unavailable format {score.format_id}</button>{/if}{/each}
        {:else}<p>This legacy profile has no upgrade policy. Saving quality edits preserves that state.</p><button type="button" onclick={configure}>Configure upgrade policy</button>{/if}
        <button type="submit">{busy ? 'Saving…' : 'Save profile'}</button>
        {#if id !== null}<button type="button" onclick={() => deleting=true}>Delete profile</button>{/if}
      </fieldset>
    </form>
    {#if deleting}<div role="group" aria-label="Confirm profile deletion"><p>Delete this profile? Assigned profiles cannot be deleted.</p><button disabled={busy || loading || uncertain || stale || relatedWrites[domain]!=='idle'} onclick={remove}>Confirm delete profile</button><button disabled={busy || loading} onclick={() => deleting=false}>Cancel deletion</button></div>{/if}
    {#if dirty}<p role="status">Unsaved changes</p>{/if}
  {/if}
</section>

{#snippet sizes(leaf: QualityProfileLeafInput)}
  <fieldset class="sizes"><legend>{title(leaf.quality_id)} size limits (MB/min, 0–{domain==='tv' ? 1000 : 2000}; blank uses global limits)</legend>
    <label>Minimum <input aria-label={`${title(leaf.quality_id)} minimum size`} type="number" min="0" max={domain==='tv'?1000:2000} step="any" value={leaf.min_size ?? ''} oninput={e=>leaf.min_size=e.currentTarget.value===''?null:Number(e.currentTarget.value)} /></label>
    <label>Preferred <input aria-label={`${title(leaf.quality_id)} preferred size`} type="number" min="0" max={domain==='tv'?1000:2000} step="any" value={leaf.preferred_size ?? ''} oninput={e=>leaf.preferred_size=e.currentTarget.value===''?null:Number(e.currentTarget.value)} /></label>
    <label>Maximum <input aria-label={`${title(leaf.quality_id)} maximum size`} type="number" min="0" max={domain==='tv'?1000:2000} step="any" value={leaf.max_size ?? ''} oninput={e=>leaf.max_size=e.currentTarget.value===''?null:Number(e.currentTarget.value)} /></label>
  </fieldset>
{/snippet}
{#snippet move(index: number, child: number | null, quality: number)}
  <label>Move {title(quality)} to <select aria-label={`Move ${title(quality)} to`} value="" onchange={event=>{if(event.currentTarget.value){moveLeaf(draft!,index,child,event.currentTarget.value);dirty=true;}}}><option value="">Choose destination</option>{#if child !== null}<option value="root">Top level (end)</option>{/if}{#each draft?.roots ?? [] as target,targetIndex}{#if target.item.kind==='group' && targetIndex!==index}<option value={target.key}>{target.item.name}</option>{/if}{/each}</select></label>
{/snippet}
<style>
  .profiles{max-width:1100px;margin:1rem auto}label{display:block;margin:.5rem 0}button{margin:.2rem}input,select{margin-left:.4rem}fieldset{border:1px solid #777;padding:1rem;margin:.7rem 0}.quality-row{padding:.6rem;border-bottom:1px solid #777}.sizes{display:flex;flex-wrap:wrap;gap:.6rem}.sizes label{display:inline-block}[role=alert]{color:#b42318}
</style>
