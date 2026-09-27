<script lang="ts">
  import { onMount } from 'svelte';
  import SearchGrabPanel from './SearchGrabPanel.svelte';
  import type { ApiPage, Provider, MediaTarget, ReleaseSearchPage, IndexerContinuation, MediaDomain } from './api.generated';
  import { listProviders, searchReleases } from './api';
  let {target,onsettings}: {target:MediaTarget;onsettings:(domain:MediaDomain)=>void} = $props();
  const domain = $derived(target.media_type === 'episode' ? 'tv' : 'movies');
  let providers: ApiPage<Provider> | null = $state(null), chosen: Provider | null = $state(null), providerId = $state('');
  let result: ReleaseSearchPage | null = $state(null), busy = $state(false), loading = $state(false), error = $state('');
  let alive = true;
  const eligible = (provider:Provider) => provider.enabled && provider.settings.implementation !== 'qbittorrent' && provider.settings[domain] !== null;
  async function loadProviders(offset=0) {
    loading=true; error=''; chosen=null; providerId=''; result=null;
    const response=await listProviders(offset);
    if(!alive)return;
    loading=false;
    if(response.ok)providers=response.data;else error=response.error;
  }
  async function search(next:IndexerContinuation|null=null) {
    if(!chosen||busy)return;
    busy=true;error='';result=null;
    const response=await searchReleases({provider_id:chosen.id,provider_revision:chosen.revision,target,offset:next?.offset??0,query_index:next?.query_index??0,limit:25});
    if(!alive)return;
    busy=false;if(response.ok)result=response.data;else error=response.error;
  }
  onMount(()=>{alive=true;void loadProviders();return()=>{alive=false;};});
</script>
<section aria-label="Release search" class="release-search">
  <h2>Release search</h2><p>Review releases for {target.media_type} {target.id}. Find releases only reviews results. Use Search and download below to choose or automatically download a release. User searches bypass background availability and delay rules.</p>
  <label>Search indexer<select bind:value={providerId} disabled={busy||loading} onchange={()=>{chosen=providers?.items.find(p=>p.id===providerId)??null;result=null;}}><option value="">Choose an indexer</option>{#if chosen&&!providers?.items.some(p=>p.id===chosen!.id)}<option value={chosen.id}>{chosen.name}</option>{/if}{#each providers?.items.filter(eligible)??[] as provider (provider.id)}<option value={provider.id}>{provider.name} · revision {provider.revision}</option>{/each}</select></label>
  {#if loading}<p role="status">Loading indexers…</p>{/if}
  {#if providers}<div class="actions"><button disabled={loading||busy||providers.offset===0} onclick={()=>loadProviders(Math.max(0,providers!.offset-25))}>Previous search providers</button><button disabled={loading||busy||providers.offset+providers.limit>=providers.total} onclick={()=>loadProviders(providers!.offset+providers!.limit)}>Next search providers</button><button disabled={loading||busy} onclick={()=>loadProviders(providers!.offset)}>Reload search providers</button></div>{/if}
  <button disabled={!chosen||busy||loading} onclick={()=>search()}>Find releases</button>
  {#if busy}<p role="status">Searching releases…</p>{/if}{#if error}<p role="alert" class="error">{error}</p>{/if}
  {#if result}<p>{result.items.length} releases on this page.</p><ul>{#each result.items as item}<li><strong>{item.metadata.title??'Untitled release'}</strong><p>{item.decision.disposition}: {item.decision.reasons.join(', ')||'No rejection reasons'}</p>{#if item.decision.not_before}<p>Eligible after {new Date(item.decision.not_before*1000).toLocaleString()}</p>{/if}<p>Matched {item.decision.target ? item.decision.target.media_type==='tv' ? `series ${item.decision.target.series_id}, episodes ${item.decision.target.episode_ids.join(', ')}` : `movie ${item.decision.target.movie_id}` : 'no library target'}</p></li>{/each}</ul>{#if result.next_query}<button disabled={busy} onclick={()=>search(result!.next_query)}>Next release results</button>{/if}{/if}
  <button onclick={()=>onsettings(domain)}>Open background delay settings</button>
  <SearchGrabPanel {target}/>
</section>
<style>.release-search{border-top:2px solid #245ba8;margin-top:1.5rem;padding-top:1rem}ul{padding-left:1.25rem}li{border-bottom:1px solid #cbd5de;padding:.5rem 0;overflow-wrap:anywhere}.actions{margin:.8rem 0}</style>
