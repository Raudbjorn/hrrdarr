<script lang="ts">
  import { onMount } from 'svelte';
  import type { ApiPage, DownloadProcessing, MediaDomain, ProcessingMode, ProcessingPolicy, Provider } from './api.generated';
  import { cancelDownloadProcessing, getProcessingPolicy, listDownloadProcessing, processDownloads, saveProcessingPolicy } from './api';
  let { provider, media }: {provider:Provider; media:MediaDomain} = $props();
  let policy:ProcessingPolicy|null = $state(null), rows:ApiPage<DownloadProcessing>|null = $state(null);
  let enabled = $state(false), mode:ProcessingMode = $state('copy'), offset = $state(0), dirty = $state(false);
  let reading = $state(false), saving = $state(false), uncertain = $state(false), error = $state(''), notice = $state('');
  let alive = true, timer:ReturnType<typeof setTimeout>|undefined;
  function poll() { clearTimeout(timer); if(alive && !document.hidden) timer=setTimeout(()=>void read(),5000); }
  async function read(reconcile=false,reset=false) {
    if(!alive || reading || saving || document.hidden) return;
    reading=true; clearTimeout(timer);
    const [saved, records]=await Promise.all([getProcessingPolicy(provider.id,media),listDownloadProcessing(provider.id,media,offset)]);
    if(!alive) return;
    if(saved.ok) {
      if(!dirty || reset || !policy) { policy=saved.data; enabled=policy.enabled; mode=policy.mode; dirty=false; }
    } else error=saved.error;
    if(records.ok) rows=records.data; else error=records.error;
    reading=false;
    if(reconcile && saved.ok && records.ok) { uncertain=false; error=''; notice='Processing status checked. Inspect saved policy and imports before another action.'; }
    poll();
  }
  async function mutate(action:'policy'|'retry'|'cancel',row?:DownloadProcessing) {
    if(reading || saving || uncertain || !policy) return;
    saving=true; clearTimeout(timer); error=''; notice='';
    const result=action==='policy'
      ? await saveProcessingPolicy(provider.id,media,{provider_revision:provider.revision,revision:policy.revision,enabled,mode})
      : action==='retry'
        ? await processDownloads({provider_id:provider.id,provider_revision:row!.operation_id?row!.provider_revision:provider.revision,media_type:media,receipt_ids:[row!.receipt_id]})
        : await cancelDownloadProcessing(row!.receipt_id);
    if(!alive) return;
    saving=false;
    if(result.ok) {
      notice=action==='policy'?'Completed-download policy saved.':action==='retry'?'Processing request accepted.':'Processing cancellation recorded.';
      if(action==='policy') dirty=false;
    } else {
      error=`${result.error}${result.code ? ` (${result.code})` : ''}${result.status===409 ? ' Check processing status to review current state before retrying.' : ''}`;
      uncertain=!result.status || result.status>=500;
    }
    // Preserve mutation errors until explicit readback; never replay a write after uncertainty.
    if(result.ok) await read(false,action==='policy'); else poll();
  }
  onMount(()=>{
    alive=true; void read();
    const visible=()=>{clearTimeout(timer);if(!document.hidden)void read();};
    document.addEventListener('visibilitychange',visible);
    return()=>{alive=false;clearTimeout(timer);document.removeEventListener('visibilitychange',visible);};
  });
</script>

<section class="surface" aria-labelledby="processing-heading">
  <h2 id="processing-heading">Completed downloads</h2>
  <p>Import confirmed {media==='tv'?'TV':'movie'} downloads from {provider.name}. Enabled processing uses download refreshes to find completed downloads. Configure a remote path mapping before enabling it.</p>
  <p>Copy and hardlink preserve torrent source files. Replacements keep the previous file in a private recovery location; shared TV files stay in place.</p>
  {#if reading}<p role="status">Reading processing status…</p>{/if}
  {#if saving}<p role="status">Saving processing request…</p>{/if}
  {#if error}<p role="alert">{error}</p>{/if}
  {#if uncertain}<p role="alert">The request may have committed. Check processing status before another action.</p>{/if}
  {#if notice}<p role="status">{notice}</p>{/if}
  <button disabled={reading||saving} onclick={()=>read(true,true)}>Check processing status</button>
  {#if policy}
    <form onsubmit={event=>{event.preventDefault();void mutate('policy');}} oninput={()=>dirty=true}>
      <fieldset disabled={reading||saving||uncertain}>
        <legend>{media==='tv'?'TV':'Movie'} import policy</legend>
        <label class="check"><input type="checkbox" bind:checked={enabled}/>Enable completed-download imports</label>
        <label>Import transfer<select aria-label="Import transfer" bind:value={mode}><option value="copy">Copy</option><option value="hardlink">Hardlink</option></select></label>
        <p>Hardlink requires the same filesystem. Disabling processing stops new imports; already-started operations continue recovery.</p>
        <button disabled={enabled && (!provider.enabled||!provider.settings[media])}>Save import policy</button>
      </fieldset>
    </form>
    {#if policy.provider_revision!==provider.revision}<p role="alert">The saved policy uses an older provider revision. Review and save it for the current provider.</p>{/if}
  {/if}
  <h3>Import processing</h3>
  {#if rows}
    <p>{rows.total} processing records for {media}.</p>
    <ul>{#each rows.items as row (row.receipt_id)}
      <li><strong>{row.target.media_type==='tv'?`Episode ${row.target.episode_ids.join(', ')}`:`Movie ${row.target.movie_id}`}: {row.status}</strong>
        <p>Preflight attempts: {row.preflight_attempts} this round, {row.total_preflight_attempts} total. Provider revision {row.provider_revision}.</p>
        {#if row.operation_id}<p>Import {row.operation_id}: {row.import_phase??'Awaiting status'}</p>{/if}
        {#if row.resume_requested}<p role="status">Resume requested; waiting for an import worker. The same operation will continue.</p>{/if}
        {#if row.retirement_state==='quarantined'}<p>Previous file retained in a private recovery location. Recovery bytes are not automatically deleted.</p>
        {:else if row.retirement_state==='shared_retained'}<p>Previous file retained because other episodes still use it.</p>
        {:else if row.retirement_state==='pending'}<p>Previous file retirement is pending. Original or recovery bytes are retained.</p>{/if}
        {#if row.error_code}<p role="alert">{row.error_code}{row.reasons.length?`: ${row.reasons.join(', ')}`:''}</p>{/if}
        {#if row.status==='blocked'||row.status==='cancelled'}<button disabled={reading||saving||uncertain||!policy?.enabled} onclick={()=>mutate('retry',row)}>Retry processing {row.receipt_id}</button>{/if}
        {#if row.status==='importing' && row.error_code && !row.resume_requested}<button disabled={reading||saving||uncertain} onclick={()=>mutate('retry',row)}>Resume import {row.operation_id}</button><p>Resume continues the same recorded import operation.</p>{/if}
        {#if !row.operation_id && (row.status==='queued'||row.status==='checking')}<button disabled={reading||saving||uncertain} onclick={()=>mutate('cancel',row)}>Cancel processing {row.receipt_id}</button>{/if}
      </li>
    {:else}<li>No processing records. Enable the policy and refresh downloads to inspect confirmed completed submissions.</li>{/each}</ul>
    <div class="actions"><button disabled={reading||saving||offset===0} onclick={()=>{offset=Math.max(0,offset-25);void read();}}>Previous imports</button><button disabled={reading||saving||offset+25>=rows.total} onclick={()=>{offset+=25;void read();}}>Next imports</button></div>
  {/if}
</section>

<style>
  .surface{background:white;border:1px solid #cbd5de;padding:1.25rem;margin:1rem 0}.surface h2{margin-top:0}fieldset{display:grid;gap:.8rem;border:1px solid #cbd5de;padding:1rem;min-width:0;margin-top:1rem}.check{display:flex;gap:.6rem;align-items:center}.check input{width:auto}li{margin:1rem 0;overflow-wrap:anywhere}li p{margin:.4rem 0}[role=alert]{color:#a12637}.actions{display:flex;gap:.5rem;flex-wrap:wrap}@media(max-width:700px){.surface{padding:1rem}}
</style>
