<script lang="ts">
  import { onMount } from 'svelte';
  import type { ApiPage, MediaTarget, Provider, SearchCommand, SearchCommandInput, SearchMode, SearchResult } from './api.generated';
  import { listProviders, listSearchCommands, getSearchCommand, deleteSearchCommand, createSearchCommand, cancelSearchCommand, listSearchResults, grabSearchResult } from './api';
  let { target }: {target:MediaTarget} = $props();
  const domain=$derived(target.media_type==='episode'?'tv':'movies');
  let providers:ApiPage<Provider>|null=$state(null), indexer:Provider|null=$state(null), client:Provider|null=$state(null);
  let indexerId=$state(''), clientId=$state(''), providerBusy=$state(false);
  let commands:ApiPage<SearchCommand>|null=$state(null), selected:SearchCommand|null=$state(null), results:ApiPage<SearchResult>|null=$state(null);
  let commandOffset=$state(0), resultOffset=$state(0), reading=$state(false), saving=$state(false), uncertain=$state(false);
  let pending:SearchCommandInput|null=$state(null), retryMissing=$state(false), error=$state(''), notice=$state('');
  let alive=true, timer:ReturnType<typeof setTimeout>|undefined;
  const active=(c:SearchCommand)=>['queued','running','retry_wait'].includes(c.status);
  const eligible=(p:Provider)=>p.enabled && p.settings[domain]!==null;
  function poll(){clearTimeout(timer);if(alive&&!document.hidden)timer=setTimeout(()=>void read(),5000);}
  async function loadProviders(offset=0){
    if(providerBusy||saving)return;providerBusy=true;
    const response=await listProviders(offset);if(!alive)return;providerBusy=false;
    if(response.ok)providers=response.data;else error=response.error;
  }
  async function read(reconcile=false){
    if(!alive||reading||saving||document.hidden)return;
    reading=true;clearTimeout(timer);let ok=true;
    const page=await listSearchCommands(target,commandOffset);if(!alive)return;
    if(page.ok)commands=page.data;else{error=page.error;ok=false;}
    const id=pending?.request_id??selected?.id??(page.ok?page.data.items[0]?.id:undefined);
    if(id){
      const detail=await getSearchCommand(id);if(!alive)return;
      if(detail.ok){
        if(selected?.id!==detail.data.id){resultOffset=0;results=null;}
        selected=detail.data;pending=null;retryMissing=false;
        const offers=await listSearchResults(id,resultOffset);if(!alive)return;
        if(offers.ok)results=offers.data;else{error=offers.error;ok=false;}
      }else if(reconcile&&pending&&detail.status===404){retryMissing=true;notice='No command was found. Retry the same saved request explicitly.';}
      else if(reconcile&&!pending&&detail.status===404){selected=null;results=null;notice='The selected search is no longer retained.';}
      else{error=detail.error;ok=false;}
    }
    reading=false;
    if(reconcile&&ok){uncertain=false;error='';if(!retryMissing)notice='Search status checked. Review the recorded command and download before another action.';}
    poll();
  }
  async function start(mode:SearchMode,retry=false){
    if(reading||saving||uncertain||providerBusy)return;
    if(!retry){
      if(!indexer||!client||pending)return;
      pending={request_id:crypto.randomUUID(),mode,target,indexer_id:indexer.id,indexer_revision:indexer.revision,client_id:client.id,client_revision:client.revision,priority:'normal'};
    }
    if(!pending)return;
    saving=true;clearTimeout(timer);error='';notice='';
    const response=await createSearchCommand(pending);if(!alive)return;saving=false;
    if(response.ok){selected=response.data;results=null;resultOffset=0;pending=null;retryMissing=false;notice='Search queued.';await read();}
    else{
      error=`${response.error}${response.code?` (${response.code})`:''}`;
      uncertain=!response.status||response.status>=500||response.code==='command_conflict';
      if(!uncertain)pending=null;
      retryMissing=false;poll();
    }
  }
  async function act(action:'grab'|'cancel'|'delete',id:string){
    if(reading||saving||uncertain||pending)return;
    if(action==='delete'&&!confirm('Remove this search history and its unused offers?'))return;
    saving=true;clearTimeout(timer);error='';notice='';
    const response=action==='grab'?await grabSearchResult(id):action==='delete'?await deleteSearchCommand(id):await cancelSearchCommand(id);
    if(!alive)return;saving=false;
    if(response.ok){if(action==='delete'){selected=null;results=null;}notice=action==='grab'?'Download selection recorded.':action==='delete'?'Unused search history removed.':'Search cancellation recorded.';await read();}
    else{error=`${response.error}${response.code?` (${response.code})`:''}`;uncertain=true;poll();}
  }
  async function selectCommand(command:SearchCommand){if(reading||saving||uncertain||pending)return;selected=command;results=null;resultOffset=0;await read();}
  onMount(()=>{
    alive=true;void loadProviders();void read();
    const visible=()=>{clearTimeout(timer);if(!document.hidden)void read();};document.addEventListener('visibilitychange',visible);
    return()=>{alive=false;clearTimeout(timer);document.removeEventListener('visibilitychange',visible);};
  });
</script>
<section aria-label="Search and download" class="grab-search">
  <h3>Search and download</h3>
  <p>Search this {target.media_type==='episode'?'episode':'movie'} and choose a release, or let the quality profile select one. These user-invoked searches bypass background availability and delay rules.</p>
  {#if error}<p role="alert">{error}</p>{/if}
  {#if uncertain}<p role="alert">The request may have committed. Check search status before another action.</p>{/if}
  {#if notice}<p role="status">{notice}</p>{/if}
  {#if reading||saving}<p role="status">{saving?'Saving search action…':'Reading search status…'}</p>{/if}
  <button disabled={reading||saving} onclick={()=>read(true)}>Check search status</button>
  <fieldset disabled={providerBusy||reading||saving||uncertain||pending!==null}>
    <legend>Search providers</legend>
    <label>Download search indexer<select bind:value={indexerId} onchange={()=>{indexer=providers?.items.find(p=>p.id===indexerId)??null;}}><option value="">Choose an indexer</option>{#if indexer&&!providers?.items.some(p=>p.id===indexer!.id)}<option value={indexer.id}>{indexer.name}</option>{/if}{#each providers?.items.filter(p=>eligible(p)&&p.settings.implementation!=='qbittorrent')??[] as provider (provider.id)}<option value={provider.id}>{provider.name}</option>{/each}</select></label>
    <label>Search download client<select bind:value={clientId} onchange={()=>{client=providers?.items.find(p=>p.id===clientId)??null;}}><option value="">Choose a client</option>{#if client&&!providers?.items.some(p=>p.id===client!.id)}<option value={client.id}>{client.name}</option>{/if}{#each providers?.items.filter(p=>eligible(p)&&p.settings.implementation==='qbittorrent')??[] as provider (provider.id)}<option value={provider.id}>{provider.name}</option>{/each}</select></label>
    {#if providers}<div class="actions"><button disabled={providers.offset===0} onclick={()=>loadProviders(Math.max(0,providers!.offset-25))}>Previous download search providers</button><button disabled={providers.offset+providers.limit>=providers.total} onclick={()=>loadProviders(providers!.offset+providers!.limit)}>Next download search providers</button><button onclick={()=>{indexer=null;client=null;indexerId='';clientId='';void loadProviders();}}>Reload download search providers</button></div>{/if}
    <button disabled={!indexer||!client} onclick={()=>start('interactive')}>Search to choose a download</button>
    <button disabled={!indexer||!client} onclick={()=>start('automatic')}>Search and download best match</button>
  </fieldset>
  {#if pending}<p>Request {pending.request_id}</p>{#if retryMissing}<button disabled={reading||saving||uncertain} onclick={()=>start(pending!.mode,true)}>Retry same search request</button>{/if}{/if}
  <h4>Search history</h4>
  {#if commands}<ul>{#each commands.items as command (command.id)}<li><button disabled={reading||saving||uncertain||pending!==null} onclick={()=>selectCommand(command)}>{command.mode} search · {command.status} · {command.id}</button></li>{:else}<li>No saved searches for this target.</li>{/each}</ul><div class="actions"><button disabled={reading||saving||uncertain||pending!==null||commandOffset===0} onclick={()=>{commandOffset=Math.max(0,commandOffset-25);void read();}}>Previous searches</button><button disabled={reading||saving||uncertain||pending!==null||commandOffset+commands.limit>=commands.total} onclick={()=>{commandOffset+=25;void read();}}>Next searches</button></div>{/if}
  {#if selected}
    <p>Selected search {selected.id}: {selected.status}. Attempts {selected.attempts}; {selected.fetched} releases fetched.</p>
    {#if selected.error_code}<p role="alert">{selected.error_code}</p>{/if}
    {#if selected.selected_candidate_error_code||selected.selected_candidate_reasons.length}<p role="alert">{selected.selected_candidate_error_code??''} {selected.selected_candidate_reasons.join(', ')}</p>{/if}
    {#if selected.selected_candidate_id}<p>Download {selected.selected_candidate_id}: {selected.selected_candidate_status??'Awaiting status'}. Inspect Activity for download and import progress.</p>{/if}
    {#if active(selected)}<button disabled={reading||saving||uncertain||pending!==null} onclick={()=>act('cancel',selected!.id)}>Cancel selected search</button>{/if}
    {#if !active(selected)&&!selected.selected_candidate_id}<button disabled={reading||saving||uncertain||pending!==null} onclick={()=>act('delete',selected!.id)}>Remove unused search history</button>{/if}
    {#if results}<ul>{#each results.items as result (result.id)}<li><strong>{result.metadata.title??'Untitled release'}</strong><p>{result.decision.disposition}: {result.decision.reasons.join(', ')||'No rejection reasons'}</p>
      {#if result.selected_candidate_id}<p>Selected download {result.selected_candidate_id}</p>{:else}<p>Offer expires {new Date(result.expires_at*1000).toLocaleString()}.</p>{/if}
      {#if selected.mode==='interactive'&&selected.status==='succeeded'&&result.decision.disposition==='accept'}<button disabled={reading||saving||uncertain||pending!==null||selected.selected_candidate_id!==null||result.expires_at*1000<=Date.now()} onclick={()=>act('grab',result.id)}>Download {result.metadata.title??'release'}</button>{/if}
    </li>{:else}<li>{selected.fetch_complete?selected.fetched===0?'No releases found.':'No retained offers. Unselected offers expire after 30 minutes.':'Results appear after the complete search finishes.'}</li>{/each}</ul><div class="actions"><button disabled={reading||saving||uncertain||resultOffset===0} onclick={()=>{resultOffset=Math.max(0,resultOffset-25);void read();}}>Previous download results</button><button disabled={reading||saving||uncertain||resultOffset+results.limit>=results.total} onclick={()=>{resultOffset+=25;void read();}}>Next download results</button></div>{/if}
  {/if}
</section>
<style>.grab-search{border-top:1px solid #cbd5de;margin-top:1rem;padding-top:1rem}li,p{overflow-wrap:anywhere}fieldset{margin-top:1rem}.actions{margin:.75rem 0}li{margin:.5rem 0}</style>
