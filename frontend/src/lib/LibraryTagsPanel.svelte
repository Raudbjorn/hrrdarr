<script lang="ts">
  import type { MediaDomain, LibraryItem, Tag, TagAssignmentMode } from './api.generated';
  import type { SettingsWriteState } from './api';
  import { listTags, assignLibraryTags, getLibrary } from './api';
  import { assignmentError, unknownTagWrite } from './tag-draft';
  let { domain, items, selected, catalogVersion, relatedWrite, onwrite, onsaved }: {domain:MediaDomain;items:LibraryItem[];selected:LibraryItem|null;catalogVersion:number;relatedWrite:SettingsWriteState;onwrite:(scope:MediaDomain,state:SettingsWriteState)=>void;onsaved:(items:LibraryItem[])=>void}=$props();
  let catalog=$state<Tag[]>([]), targets=$state<number[]>([]), ids=$state<number[]>([]), mode=$state<TagAssignmentMode>('add'), loading=$state(false), busy=$state(false), uncertain=$state(false), loaded=$state(false), error=$state(''), notice=$state(''), readback=$state<LibraryItem[]>([]);
  let scope=$state<MediaDomain>('tv'), version=$state(-1), epoch=0;
  const stale=$derived(loaded&&version!==catalogVersion);
  const blocked=$derived(busy||loading||uncertain||stale||relatedWrite!=='idle');
  $effect(()=>{if(domain!==scope){scope=domain;++epoch;loading=false;loaded=false;catalog=[];targets=[];ids=[];mode='add';readback=[];error='';notice='';}});
  async function reload(){
    if(busy||relatedWrite!=='idle')return;const token=++epoch, media=domain, revision=catalogVersion, ownerIds=[...targets];loading=true;error='';
    const result=await listTags(media);
    // At most one library page (25 targets) is read, preserving drafts until all reads succeed.
    const owners=result.ok?await Promise.all(ownerIds.map(id=>getLibrary(media,id))):[];
    if(token!==epoch||media!==domain)return;loading=false;
    if(!result.ok){error=result.error;return;}const failure=owners.find(result=>!result.ok);if(failure&&!failure.ok){error=failure.error;return;}
    const changed=loaded&&version!==revision;if(changed)ids=[];catalog=result.data;readback=owners.flatMap(result=>result.ok?[result.data]:[]);loaded=true;version=revision;uncertain=false;onwrite(media,'idle');onsaved(readback);notice=changed?'Catalog changed: tag selection cleared. Choose current labels before applying.':'Current assignments loaded below. Your selection is retained; review before applying.';
  }
  async function save(){
    if(blocked||!loaded||!targets.length||targets.length>25)return;error=assignmentError(ids,catalog,mode);if(error)return;
    busy=true;notice='';onwrite(domain,'busy');const media=domain;
    const result=await assignLibraryTags(media,[...targets],{mode,ids:[...ids]});busy=false;
    if(!result.ok){error=result.error;uncertain=unknownTagWrite(result);onwrite(media,uncertain?'uncertain':'idle');return;}
    readback=result.data;onsaved(result.data);onwrite(media,'idle');notice=`Tags saved for ${result.data.length} library items.`;
  }
  const labels=(item:LibraryItem)=>item.tag_ids.map(id=>catalog.find(tag=>tag.id===id)?.label??`Unknown tag ${id}`).join(', ')||'No tags';
</script>
<section aria-label="Library tags">
  <h2>Library tags</h2><p>Choose owners from this library page, then add, remove or replace their tags. Other library settings are preserved.</p>
  {#if error}<p role="alert">{error}</p>{/if}{#if notice}<p role="status">{notice}</p>{/if}
  {#if uncertain}<p role="alert">Assignment outcome unknown. Reload tags and assignments before another write.</p>{/if}
  {#if stale}<p role="alert">Tag catalog changed. Reload tags and review your selection before applying.</p>{/if}
  {#if relatedWrite!=='idle'}<p role="status">Finish or reconcile the tag settings write before assigning tags.</p>{/if}
  <button disabled={busy||loading||relatedWrite!=='idle'} onclick={reload}>Reload tags and assignments</button>
  {#if loading}<p role="status">Loading tags and assignments…</p>{/if}{#if busy}<p role="status">Saving tag assignments…</p>{/if}
  <fieldset disabled={blocked}><legend>Library owners</legend>
    {#if selected}<button type="button" onclick={()=>{targets=[selected!.id];readback=[selected!];}}>Use selected library item</button>{/if}
    {#each items as item(item.id)}<label><input type="checkbox" disabled={targets.length>=25&&!targets.includes(item.id)} value={item.id} bind:group={targets} />{item.title} ({item.id})</label>{/each}
    <p>{targets.length} selected</p><button type="button" onclick={()=>{targets=[];readback=[];}}>Clear owner selection</button>
  </fieldset>
  <form onsubmit={event=>{event.preventDefault();void save();}}><fieldset disabled={blocked||!loaded}><legend>Tag assignment</legend><label>Tag action<select bind:value={mode}><option value="add">Add tags</option><option value="remove">Remove tags</option><option value="replace">Replace all tags</option></select></label>
    {#each catalog as tag(tag.id)}<label><input type="checkbox" value={tag.id} bind:group={ids} />{tag.label}</label>{:else}<p>No tags. Create tags in Tags settings.</p>{/each}
    {#if mode==='replace'&&!ids.length}<p>Applying this empty selection intentionally removes all tags from the selected owners.</p>{/if}
    <button disabled={!targets.length}>Apply tags</button></fieldset></form>
  {#if readback.length}<section aria-label="Current tag assignments"><h3>Current assignments</h3><ul>{#each readback as item(item.id)}<li>{item.title}: {labels(item)}</li>{/each}</ul></section>{/if}
</section>
