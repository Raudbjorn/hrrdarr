<script lang="ts">
  import type { MediaDomain, Tag } from './api.generated';
  import { listTags } from './api';
  let {domain,catalogVersion,disabled,onchange}:{domain:MediaDomain;catalogVersion:number;disabled:boolean;onchange:(ids:number[],ready:boolean)=>void}=$props();
  let tags=$state<Tag[]>([]),ids=$state<number[]>([]),loading=$state(false),loaded=$state(false),error=$state(''),version=$state(-1),epoch=0;
  const stale=$derived(loaded&&version!==catalogVersion);
  $effect(()=>{onchange([...ids],!loading&&!stale&&!error);});
  async function load(){if(disabled)return;const token=++epoch,scope=domain,revision=catalogVersion;loading=true;error='';const result=await listTags(scope);if(token!==epoch||scope!==domain)return;loading=false;if(result.ok){if(loaded&&version!==revision)ids=[];tags=result.data;loaded=true;version=revision;}else error=result.error;}
</script>
<fieldset disabled={disabled}><legend>Tags for new library item</legend><button type="button" disabled={loading} onclick={load}>Load creation tags</button>
{#if error}<p role="alert">{error}</p>{/if}{#if stale}<p role="alert">Tag catalog changed. Reload and review creation tags.</p>{/if}
{#if !loaded}<p>Tags are optional. An empty selection creates the item without tags.</p>{/if}
{#each tags as tag(tag.id)}<label><input disabled={loading||stale} type="checkbox" value={tag.id} bind:group={ids}/>{tag.label}</label>{/each}
</fieldset>
