<script lang="ts">
  import {onMount} from 'svelte';
  import type {ImportPolicy,MediaDomain} from './api.generated';
  import {getImportPolicy,updateImportPolicy,type SettingsWriteState} from './api';
  import {importPolicyUpdate,MIN_FREE_SPACE_MIB,MAX_FREE_SPACE_MIB,type ImportPolicyDraft} from './import-policy-draft';

  let domain=$state<MediaDomain>('tv'),saved=$state<ImportPolicy|null>(null);
  let draft=$state<ImportPolicyDraft>({skip_free_space_check_when_importing:false,minimum_free_space_mib:MIN_FREE_SPACE_MIB,download_client_working_folders:''});
  let write=$state<SettingsWriteState>('idle'),loading=$state(false),stale=$state(false),dirty=$state(false),error=$state(''),notice=$state('');
  const busy=$derived(write==='busy'),uncertain=$derived(write==='uncertain');
  let alive=true,epoch=0;

  function fill(policy:ImportPolicy) {
    saved=policy;
    draft={skip_free_space_check_when_importing:policy.skip_free_space_check_when_importing,minimum_free_space_mib:policy.minimum_free_space_mib,download_client_working_folders:policy.download_client_working_folders};
    dirty=false;stale=false;write='idle';
  }
  async function load() {
    if(busy||loading)return;
    loading=true;error='';notice='';
    const version=++epoch,scope=domain,result=await getImportPolicy(scope);
    if(!alive||version!==epoch)return;
    loading=false;
    if(!result.ok){error=result.error;return;}
    if(result.data?.media_type!==scope){stale=true;error='The saved policy response does not match this media type. Reload before editing.';return;}
    fill(result.data);notice='Saved import policy loaded.';
  }
  async function change(next:MediaDomain) {
    if(next===domain||busy||loading||uncertain||dirty&&!window.confirm('Discard unsaved import policy changes?'))return;
    domain=next;saved=null;dirty=false;stale=false;notice='';await load();
  }
  async function save() {
    if(!saved||busy||loading||uncertain||stale)return;
    const input=importPolicyUpdate(saved.revision,draft);
    if(!input.ok){error=input.error;return;}
    write='busy';error='';notice='';
    const version=++epoch,scope=domain,result=await updateImportPolicy(scope,input.data);
    if(!alive||version!==epoch)return;
    if(!result.ok){write=result.status===undefined||result.status>=500?'uncertain':'idle';stale=result.status===409;error=result.error;return;}
    if(result.data?.media_type!==scope){write='uncertain';error='The save response does not match this media type. Reload the saved policy to check the write.';return;}
    fill(result.data);notice='Import policy saved.';
  }
  onMount(()=>{void load();return()=>{alive=false;++epoch;};});
</script>
<svelte:window onbeforeunload={event=>{if(dirty||write!=='idle'){event.preventDefault();event.returnValue='';}}}/>
<section aria-label="Import policy">
  <h2>Import policy</h2>
  <nav aria-label="Import policy media"><button disabled={busy||loading||uncertain} aria-pressed={domain==='tv'} onclick={()=>change('tv')}>TV</button><button disabled={busy||loading||uncertain} aria-pressed={domain==='movies'} onclick={()=>change('movies')}>Movies</button></nav>
  <p>Save separate TV and movie settings for import free-space checks and download-client working folders. Import workers do not use these settings yet; saving them does not change current import behavior.</p>
  {#if loading}<p role="status">Loading import policy…</p>{/if}
  {#if busy}<p role="status">Saving import policy…</p>{/if}
  {#if error}<p role="alert">{error}</p>{/if}
  {#if notice}<p role="status">{notice}</p>{/if}
  {#if uncertain||stale}<p role="alert">{uncertain?'Write outcome unknown.':'Import policy changed in another request.'} Your draft is retained. Reload saved policy before another write.</p>{/if}
  <button disabled={busy||loading} onclick={()=>{if(!dirty||window.confirm('Replace this draft with the saved import policy?'))void load();}}>Reload import policy</button>
  <form onsubmit={event=>{event.preventDefault();void save();}} oninput={()=>dirty=true} onchange={()=>dirty=true}>
    <fieldset disabled={!saved||busy||loading||uncertain||stale}>
      <legend>{domain==='tv'?'TV':'Movie'} import settings</legend>
      <label class="check"><input type="checkbox" bind:checked={draft.skip_free_space_check_when_importing}/>Skip free-space check when importing</label>
      <label>Minimum free space (MiB)<input type="number" required min={MIN_FREE_SPACE_MIB} max={MAX_FREE_SPACE_MIB} step="1" bind:value={draft.minimum_free_space_mib} aria-describedby="import-space-hint"/></label>
      <p id="import-space-hint">One MiB is 1,048,576 bytes. The minimum is retained when skipping the check.</p>
      <label>Download-client working folder prefixes<textarea rows="2" bind:value={draft.download_client_working_folders} aria-describedby="import-folders-hint"></textarea></label>
      <p id="import-folders-hint">Separate case-sensitive literal folder-name prefixes with |. Whitespace and empty segments between separators are preserved. Leaving the entire field empty restores _UNPACK_|_FAILED_. Maximum 4096 UTF-8 bytes.</p>
      <button>Save import policy</button>
    </fieldset>
  </form>
</section>
<style>
  section{margin-top:1.5rem}fieldset{display:grid;gap:.8rem;margin-top:1rem;border:1px solid #cbd5de;padding:1rem;min-width:0}.check{display:flex;align-items:center;gap:.6rem}.check input{width:auto}fieldset p{margin:0;font-size:.9rem}textarea{width:100%;box-sizing:border-box}
</style>
