<script lang="ts">
  import {onMount} from 'svelte';
  import type {MediaDomain,RecycleBinSettings,RecycleBinCleanupCommand} from './api.generated';
  import {getRecycleBin,updateRecycleBin,listRecycleBinCleanups,createRecycleBinCleanup,getRecycleBinCleanup,type SettingsWriteState} from './api';
  import {recycleBinUpdate,describeCleanup,MAX_CLEANUP_DAYS,type RecycleBinDraft} from './recycle-bin-draft';

  const POLL_MS=500,POLL_LIMIT=240;
  let domain=$state<MediaDomain>('tv'),saved=$state<RecycleBinSettings|null>(null);
  let draft=$state<RecycleBinDraft>({path:'',cleanup_days:7});
  let write=$state<SettingsWriteState>('idle'),loading=$state(false),stale=$state(false),dirty=$state(false),error=$state(''),notice=$state('');
  let cleanup=$state<RecycleBinCleanupCommand|null>(null),cleaning=$state(false),cleanupError=$state('');
  const busy=$derived(write==='busy'),uncertain=$derived(write==='uncertain');
  let alive=true,epoch=0,cleanupEpoch=0;

  function fill(settings:RecycleBinSettings) {
    saved=settings;draft={path:settings.path??'',cleanup_days:settings.cleanup_days};
    dirty=false;stale=false;write='idle';
  }
  async function load() {
    if(busy||loading)return;
    loading=true;error='';notice='';
    const version=++epoch,scope=domain;
    const [result,history]=await Promise.all([getRecycleBin(scope),listRecycleBinCleanups(scope,1)]);
    if(!alive||version!==epoch)return;
    loading=false;
    if(!result.ok){error=result.error;return;}
    if(result.data?.media_type!==scope){stale=true;error='The saved recycle bin response does not match this media type. Reload before editing.';return;}
    fill(result.data);notice='Saved recycle bin settings loaded.';
    cleanup=history.ok?history.data.items[0]??null:null;
  }
  async function change(next:MediaDomain) {
    if(next===domain||busy||loading||uncertain||cleaning||dirty&&!window.confirm('Discard unsaved recycle bin changes?'))return;
    domain=next;saved=null;dirty=false;stale=false;notice='';cleanup=null;cleanupError='';await load();
  }
  async function save() {
    if(!saved||busy||loading||uncertain||stale)return;
    const input=recycleBinUpdate(saved.revision,draft);
    if(!input.ok){error=input.error;return;}
    write='busy';error='';notice='';
    const version=++epoch,scope=domain,result=await updateRecycleBin(scope,input.data);
    if(!alive||version!==epoch)return;
    if(!result.ok){write=result.status===undefined||result.status>=500?'uncertain':'idle';stale=result.status===409&&result.code==='recycle_bin_conflict';error=result.error;return;}
    if(result.data?.media_type!==scope){write='uncertain';error='The save response does not match this media type. Reload the saved settings to check the write.';return;}
    fill(result.data);notice=result.data.path===null?'Recycle bin cleared. File deletion now requires confirming permanent deletion.':'Recycle bin saved. Health re-checks that the folder exists and is writable.';
  }
  async function runCleanup() {
    if(!saved||cleaning||busy||loading)return;
    cleaning=true;cleanupError='';
    const token=++cleanupEpoch,scope=domain,created=await createRecycleBinCleanup(scope);
    if(!alive||token!==cleanupEpoch)return;
    if(!created.ok){cleaning=false;cleanupError=created.error;return;}
    cleanup=created.data;
    for(let attempt=0;attempt<POLL_LIMIT&&cleanup&&['queued','running','retry_wait'].includes(cleanup.status);attempt++) {
      await new Promise(resolve=>setTimeout(resolve,POLL_MS));
      if(!alive||token!==cleanupEpoch)return;
      const next=await getRecycleBinCleanup(created.data.id);
      if(!alive||token!==cleanupEpoch)return;
      if(!next.ok){cleanupError=`Cleanup status could not be read: ${next.error}`;break;}
      cleanup=next.data;
    }
    cleaning=false;
  }
  onMount(()=>{void load();return()=>{alive=false;++epoch;++cleanupEpoch;};});
</script>
<svelte:window onbeforeunload={event=>{if(dirty||write!=='idle'){event.preventDefault();event.returnValue='';}}}/>
<section aria-label="Recycle bin" aria-busy={loading||busy}>
  <h2>Recycle bin</h2>
  <nav aria-label="Recycle bin media"><button disabled={busy||loading||uncertain||cleaning} aria-pressed={domain==='tv'} onclick={()=>change('tv')}>TV</button><button disabled={busy||loading||uncertain||cleaning} aria-pressed={domain==='movies'} onclick={()=>change('movies')}>Movies</button></nav>
  <p>When set, deleting a {domain==='tv'?'episode':'movie'} file moves it into this folder (inside a folder named after the {domain==='tv'?'series':'movie'}) instead of deleting it. Leave the path empty to delete files permanently; every deletion still asks for confirmation.</p>
  {#if loading}<p role="status">Loading recycle bin settings…</p>{/if}
  {#if busy}<p role="status">Saving recycle bin settings…</p>{/if}
  {#if error}<p role="alert">{error}</p>{/if}
  {#if notice}<p role="status">{notice}</p>{/if}
  {#if uncertain||stale}<p role="alert">{uncertain?'Write outcome unknown.':'Recycle bin settings changed in another request.'} Your draft is retained. Reload saved settings before another write.</p>{/if}
  <button disabled={busy||loading} onclick={()=>{if(!dirty||window.confirm('Replace this draft with the saved recycle bin settings?'))void load();}}>Reload recycle bin settings</button>
  <form onsubmit={event=>{event.preventDefault();void save();}} oninput={()=>dirty=true} onchange={()=>dirty=true}>
    <fieldset disabled={!saved||busy||loading||uncertain||stale}>
      <legend>{domain==='tv'?'TV':'Movie'} recycle bin</legend>
      <label>Recycle bin folder<input bind:value={draft.path} placeholder="/data/recycle" autocomplete="off" spellcheck="false" aria-describedby="recycle-path-hint"/></label>
      <p id="recycle-path-hint">An absolute folder outside every root folder and library folder (TV and movies). It must already exist; Health reports a missing or unwritable folder. TV and movies may share one folder but cannot nest.</p>
      <label>Cleanup after (days)<input type="number" required min="0" max={MAX_CLEANUP_DAYS} step="1" bind:value={draft.cleanup_days} aria-describedby="recycle-days-hint"/></label>
      <p id="recycle-days-hint">Once a day, files in this folder older than this many days are permanently removed, then empty subfolders. Anything already in the folder counts, not only files hrrdarr moved there. 0 disables automatic cleanup. The first scheduled run is one day after choosing a folder.</p>
      <button>Save recycle bin settings</button>
    </fieldset>
  </form>
  <section aria-label="Recycle bin cleanup">
    <h3>Cleanup</h3>
    <button disabled={!saved||saved.path===null||saved.cleanup_days===0||cleaning||busy||loading||dirty} onclick={runCleanup}>Clean up recycle bin now</button>
    {#if saved&&saved.path===null}<p>Configure a recycle bin to enable cleanup.</p>{:else if saved&&saved.cleanup_days===0}<p>Cleanup is disabled (0 days).</p>{:else if dirty}<p>Save or reload your draft before running cleanup.</p>{/if}
    {#if cleaning}<p role="status">Waiting for cleanup to finish…</p>{/if}
    {#if cleanupError}<p role="alert">{cleanupError}</p>{/if}
    {#if cleanup}<p role="status" aria-label="Latest recycle bin cleanup">{describeCleanup(cleanup)}</p>{/if}
  </section>
</section>
<style>
  section{margin-top:1.5rem}fieldset{display:grid;gap:.8rem;margin-top:1rem;border:1px solid #cbd5de;padding:1rem;min-width:0}fieldset p{margin:0;font-size:.9rem}input{width:100%;max-width:40rem;box-sizing:border-box}[role=alert]{border-left:3px solid #a12637;padding-left:.75rem;color:#a12637}button{min-height:2.5rem}
</style>
