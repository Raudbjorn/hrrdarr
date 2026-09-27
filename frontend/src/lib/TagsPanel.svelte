<script lang="ts">
  import { onMount } from 'svelte';
  import type { MediaDomain, TagDetail, TagOwners } from './api.generated';
  import type { SettingsWriteState } from './api';
  import { listTagDetails, createTag, renameTag, deleteTag, getTagOwners } from './api';
  import { tagLabelError, unknownTagWrite } from './tag-draft';
  let { relatedWrites, onwrite, onchange, onowner, ondelay, usageVersions }: {relatedWrites:Record<MediaDomain,SettingsWriteState>;onwrite:(domain:MediaDomain,state:SettingsWriteState)=>void;onchange:(domain:MediaDomain)=>void;onowner:(domain:MediaDomain,id:number)=>void;ondelay:(domain:MediaDomain,id:number)=>void;usageVersions:Record<MediaDomain,number>} = $props();
  let domain = $state<MediaDomain>('tv'), rows = $state<TagDetail[]>([]), loading = $state(false), busy = $state(false), uncertain = $state(false), loaded = $state(false);
  let label = $state(''), editing = $state<number|null>(null), deleting = $state<TagDetail|null>(null), owners = $state<TagOwners|null>(null), ownerTag = $state<TagDetail|null>(null), error = $state(''), notice = $state('');
  let epoch=0, alive=true, usageSeen={tv:0,movies:0};
  let usageStale=$state(false);
  $effect(()=>{const revision=usageVersions[domain];if(revision!==usageSeen[domain]){usageSeen[domain]=revision;usageStale=true;deleting=null;owners=null;ownerTag=null;}});
  const blocked = $derived(busy || loading || uncertain || relatedWrites[domain] !== 'idle');
  async function load() {
    if (busy || relatedWrites[domain] !== 'idle') return;
    const version=++epoch, scope=domain, usageVersion=usageVersions[domain]; loading=true; loaded=false; deleting=null; owners=null; ownerTag=null; error='';
    const result=await listTagDetails(scope); if(!alive||version!==epoch)return; loading=false;
    if(usageVersion!==usageVersions[scope]||relatedWrites[scope]!=='idle'){usageStale=true;error='Tag usage changed during loading. Reload for current references.';return;}
    if(result.ok){rows=result.data;loaded=true;usageStale=false;uncertain=false;onwrite(scope,'idle');onchange(scope);notice='Tags reloaded. Review the retained label before saving.';}else error=result.error;
  }
  function switchDomain(next:MediaDomain){if(blocked||next===domain)return;domain=next;rows=[];label='';editing=null;notice='';void load();}
  async function save(){
    if(blocked||!loaded)return; error=tagLabelError(label);if(error)return;
    busy=true;deleting=null;onwrite(domain,'busy');const scope=domain;
    const result=editing===null?await createTag(scope,label):await renameTag(scope,editing,label);
    if(!alive)return;busy=false;
    if(!result.ok){error=result.error;uncertain=unknownTagWrite(result);onwrite(scope,uncertain?'uncertain':'idle');if(uncertain)onchange(scope);return;}
    onwrite(scope,'idle');onchange(scope);editing=null;label='';await load();notice='Tag saved.';
  }
  async function remove(){
    if(blocked||!deleting)return;const id=deleting.tag.id,scope=domain;busy=true;onwrite(scope,'busy');error='';
    const result=await deleteTag(scope,id);if(!alive)return;busy=false;deleting=null;
    if(!result.ok){error=result.error;uncertain=unknownTagWrite(result);onwrite(scope,uncertain?'uncertain':'idle');if(uncertain)onchange(scope);return;}
    onwrite(scope,'idle');onchange(scope);await load();notice='Tag deleted.';
  }
  async function showOwners(row:TagDetail,offset=0){
    if(blocked||usageStale)return;const version=++epoch;deleting=null;loading=true;error='';ownerTag=row;owners=null;
    const result=await getTagOwners(domain,row.tag.id,offset);if(!alive||version!==epoch)return;loading=false;if(result.ok)owners=result.data;else error=result.error;
  }
  onMount(()=>{void load();return()=>{alive=false;++epoch;};});
</script>
<section aria-label="Tags settings">
  <h1>Tags</h1><nav aria-label="Tags media type"><button disabled={blocked} aria-pressed={domain==='tv'} onclick={()=>switchDomain('tv')}>TV</button><button disabled={blocked} aria-pressed={domain==='movies'} onclick={()=>switchDomain('movies')}>Movies</button></nav>
  <p>Labels use lowercase letters, digits and hyphens. Creating an existing label returns that tag. TV and movie tags are separate.</p>
  {#if error}<p role="alert">{error}</p>{/if}{#if notice}<p role="status">{notice}</p>{/if}
  {#if uncertain}<p role="alert">Write outcome unknown. Reload tags and review before any further write.</p>{/if}
  {#if relatedWrites[domain]!=='idle'}<p role="status">Finish or reconcile the library or delay-profile write before changing tags.</p>{/if}
  {#if usageStale}<p role="status">Delay profile usage changed. Reload tags for current references.</p>{/if}
  <button disabled={busy||loading||relatedWrites[domain]!=='idle'} onclick={load}>Reload tags</button>
  {#if loading}<p role="status">Loading tags…</p>{/if}{#if busy}<p role="status">Saving tag change…</p>{/if}
  <form onsubmit={event=>{event.preventDefault();void save();}}><fieldset disabled={blocked||!loaded}><legend>{editing===null?'Create tag':`Rename tag ${editing}`}</legend><label>Tag label<input bind:value={label} required maxlength="128" /></label><button>{editing===null?'Create tag':'Save tag'}</button>{#if editing!==null}<button type="button" onclick={()=>{editing=null;label='';}}>Cancel rename</button>{/if}</fieldset></form>
  {#if loaded}<ul>{#each rows as row(row.tag.id)}<li><strong>{row.tag.label}</strong> · {row.owner_count} library owners · {row.delay_profile_ids.length} delay profiles <button disabled={blocked} onclick={()=>{deleting=null;editing=row.tag.id;label=row.tag.label;}}>Rename {row.tag.label}</button> <button disabled={blocked||usageStale} onclick={()=>showOwners(row)}>Usage of {row.tag.label}</button> <button disabled={blocked||usageStale} onclick={()=>deleting=row}>Delete {row.tag.label}</button></li>{:else}<li>No tags in this media type.</li>{/each}</ul>{/if}
  {#if deleting}<div role="group" aria-label="Confirm tag deletion"><p>Delete tag “{deleting.tag.label}”? Assigned tags cannot be deleted; remove their library and delay-profile references first.</p><button disabled={blocked} onclick={remove}>Confirm delete tag</button><button disabled={busy} onclick={()=>deleting=null}>Cancel tag deletion</button></div>{/if}
  {#if ownerTag&&owners}<section aria-label="Tag usage"><h2>{ownerTag.tag.label}: {owners.total} library owners</h2><ul>{#each ownerTag.delay_profile_ids as id}<li><button disabled={blocked||usageStale} onclick={()=>ondelay(domain,id)}>Open delay profile {id}</button></li>{/each}</ul><ul>{#each owners.ids as id}<li><button disabled={blocked} onclick={()=>onowner(domain,id)}>Open {domain==='tv'?'series':'movie'} {id}</button></li>{:else}<li>No library assignments.</li>{/each}</ul><button disabled={blocked||owners.offset===0} onclick={()=>showOwners(ownerTag!,Math.max(0,owners!.offset-25))}>Previous owners</button><button disabled={blocked||owners.offset+owners.limit>=owners.total} onclick={()=>showOwners(ownerTag!,owners!.offset+owners!.limit)}>Next owners</button></section>{/if}
</section>
