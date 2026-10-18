<script lang="ts">
  import { onMount, untrack, tick } from 'svelte';
  import type { ApiPage, DownloadProcessing, MediaDomain, ProcessingMode, ProcessingPolicy, Provider } from './api.generated';
  import type {SettingsWriteState} from './api';
  import { cancelDownloadProcessing, inheritProcessingPolicy, listDownloadProcessing, processDownloads, saveProcessingPolicy } from './api';
  let { provider, media, currentPolicy, active=true, relatedWrite='idle', onreload, onwrite=()=>{}, ondirty=()=>{} }: {provider:Provider; media:MediaDomain;currentPolicy:ProcessingPolicy|null;active?:boolean;relatedWrite?:SettingsWriteState;onreload:()=>Promise<boolean>;onwrite?:(state:SettingsWriteState)=>void;ondirty?:(dirty:boolean)=>void} = $props();
  let policy:ProcessingPolicy|null = $state(null), rows:ApiPage<DownloadProcessing>|null = $state(null);
  let override=$state<'inherit'|'on'|'off'>('inherit'), mode:ProcessingMode = $state('copy'), offset = $state(0), dirty = $state(false), stale=$state(false);
  let writeReadback=$state(false);
  let reading = $state(false), saving = $state(false), uncertain = $state(false), error = $state(''), notice = $state('');
  let alive = true, timer:ReturnType<typeof setTimeout>|undefined;
  const locked=$derived(saving||writeReadback||uncertain||relatedWrite!=='idle');
  function fill(value:ProcessingPolicy){policy=value;override=value.enabled_override===null?'inherit':value.enabled_override?'on':'off';mode=value.mode;dirty=false;stale=false;}
  $effect(()=>{const value=currentPolicy;if(value){if(dirty||uncertain||stale){if(policy && (policy.revision!==value.revision||policy.provider_revision!==value.provider_revision||provider.revision!==policy.provider_revision))stale=true;}else fill(value);}else if(policy)stale=true;});
  $effect(()=>onwrite(saving||writeReadback?'busy':uncertain?'uncertain':'idle'));
  $effect(()=>ondirty(dirty||stale));
  $effect(()=>{if(active){untrack(()=>void read());}else clearTimeout(timer);});
  function poll() { clearTimeout(timer); if(alive && active && !document.hidden && !writeReadback) timer=setTimeout(()=>void read(),5000); }
  // Polling/visibility reads cannot take the explicit readback slot; its owner still may read.
  async function read(ownedReadback=false) {
    if(!alive || !active || reading || saving || document.hidden || (writeReadback && !ownedReadback)) return false;
    reading=true;clearTimeout(timer);
    const records=await listDownloadProcessing(provider.id,media,offset);
    if(!alive)return false;
    reading=false;if(records.ok)rows=records.data;else error=records.error;poll();return records.ok;
  }
  async function reconcile(){
    if(saving||reading||writeReadback||relatedWrite==='busy'||(dirty&&!window.confirm('Replace the import policy draft with saved settings?')))return;
    // A navigation/settings read owns the same boundary. Do not offer a click that
    // onreload would discard, or let a periodic status read race explicit reconciliation.
    writeReadback=true;clearTimeout(timer);
    if(await onreload() && await read(true)){await tick();if(currentPolicy)fill(currentPolicy);uncertain=false;error='';notice='Saved policy and related settings checked. Review before another action.';}
    else error='Related settings or processing status readback incomplete. Check processing status.';
    writeReadback=false;poll();
  }
  async function mutate(action:'policy'|'retry'|'cancel',row?:DownloadProcessing) {
    if(reading || locked || !policy || action==='policy'&&(stale||!currentPolicy)) return;
    saving=true; clearTimeout(timer); error=''; notice='';
    const input={provider_revision:policy.provider_revision,revision:policy.revision,mode};
    const result=action==='policy'
      ? override==='inherit'?await inheritProcessingPolicy(provider.id,media,input):await saveProcessingPolicy(provider.id,media,{...input,enabled:override==='on'})
      : action==='retry'
        ? await processDownloads({provider_id:provider.id,provider_revision:row!.operation_id?row!.provider_revision:provider.revision,media_type:media,receipt_ids:[row!.receipt_id]})
        : await cancelDownloadProcessing(row!.receipt_id);
    if(!alive) return;
    saving=false;
    if(result.ok) {writeReadback=true;notice=action==='policy'?'Completed-download policy saved.':action==='retry'?'Processing request accepted.':'Processing cancellation recorded.';if(action==='policy'){dirty=false;stale=false;}const settingsChecked=await onreload();const statusChecked=await read(true);if(!settingsChecked||!statusChecked){uncertain=true;error='Write accepted; related settings or processing status readback incomplete. Check processing status.';}writeReadback=false;poll();}
    else {error=`${result.error}${result.code ? ` (${result.code})` : ''}`;uncertain=result.status===undefined||result.status>=500;if(result.status===409)stale=true;poll();}
  }
  onMount(()=>{alive=true;void read();const visible=()=>{clearTimeout(timer);if(!document.hidden)void read();};document.addEventListener('visibilitychange',visible);return()=>{alive=false;clearTimeout(timer);document.removeEventListener('visibilitychange',visible);onwrite('idle');ondirty(false);};});
</script>
<svelte:window onbeforeunload={event=>{if(dirty||saving||uncertain){event.preventDefault();event.returnValue='';}}} />

<section class="surface" aria-labelledby="processing-heading">
  <h2 id="processing-heading">Completed downloads</h2>
  <p>Import confirmed {media==='tv'?'TV':'movie'} downloads from {provider.name}. Enabled processing uses download refreshes to find completed downloads. Configure a remote path mapping when client paths differ from library host paths.</p>
  <p>Copy and hardlink preserve torrent source files. Replacements keep the previous file in a private recovery location; shared TV files stay in place.</p>
  {#if reading}<p role="status">Reading processing status…</p>{/if}
  {#if saving}<p role="status">Saving processing request…</p>{/if}
  {#if error}<p role="alert">{error}</p>{/if}
  {#if uncertain}<p role="alert">The request may have committed. Check processing status before another action.</p>{/if}
  {#if notice}<p role="status">{notice}</p>{/if}
  <button disabled={reading||saving||writeReadback||relatedWrite==='busy'} onclick={reconcile}>Check processing status</button>
  {#if !currentPolicy}<p role="alert">No current policy is available for this provider scope. Retained drafts cannot grant import authority.</p>{/if}
  {#if stale}<p role="alert">Saved settings changed. Your draft is retained; check processing status before saving.</p>{/if}
  {#if currentPolicy}<p>Import choice: {currentPolicy.enabled_override===null?'Use domain setting':currentPolicy.enabled_override?'Explicitly on':'Explicitly off'}. Desired imports: {currentPolicy.desired_enabled?'on':'off'}. Effective imports: {currentPolicy.enabled?'on':'off'}.</p>
    {#if currentPolicy.observation_suppressed}<p role="status">Automatic observation suppressed by schedule deletion. Use automatic schedule to restore it.</p>{:else if !currentPolicy.observation_enabled}<p role="status">Automatic observation disabled. {currentPolicy.enabled?'Manual refresh can process eligible confirmed downloads.':''}</p>{/if}
  {/if}
  {#if policy}
    <form onsubmit={event=>{event.preventDefault();void mutate('policy');}} oninput={()=>dirty=true}>
      <fieldset disabled={reading||locked}>
        <legend>{media==='tv'?'TV':'Movie'} import policy</legend>
        <label>Completed-download imports<select aria-label="Completed-download imports" bind:value={override}><option value="inherit">Use domain setting</option><option value="on">Explicitly on</option><option value="off">Explicitly off</option></select></label>
        <label>Import transfer<select aria-label="Import transfer" bind:value={mode}><option value="copy">Copy</option><option value="hardlink">Hardlink</option></select></label>
        <p>Hardlink requires the same filesystem. Disabling processing stops new imports; already-started operations continue recovery.</p>
        <button disabled={stale || override==='on' && (!provider.enabled||!provider.settings[media])}>Save import policy</button><p>{dirty?'Unsaved changes. ':''}Choose “Use domain setting” and save to reset this override, retaining the transfer mode.</p>
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
        {#if row.status==='blocked'||row.status==='cancelled'}<button disabled={reading||locked||!currentPolicy?.enabled} onclick={()=>mutate('retry',row)}>Retry processing {row.receipt_id}</button>{/if}
        {#if row.status==='importing' && row.error_code && !row.resume_requested}<button disabled={reading||locked} onclick={()=>mutate('retry',row)}>Resume import {row.operation_id}</button><p>Resume continues the same recorded import operation.</p>{/if}
        {#if !row.operation_id && (row.status==='queued'||row.status==='checking')}<button disabled={reading||locked} onclick={()=>mutate('cancel',row)}>Cancel processing {row.receipt_id}</button>{/if}
      </li>
    {:else}<li>No processing records. Eligible confirmed completed submissions appear here after observation; inherited defaults need no per-client opt-in.</li>{/each}</ul>
    <div class="actions"><button disabled={reading||saving||offset===0} onclick={()=>{offset=Math.max(0,offset-25);void read();}}>Previous imports</button><button disabled={reading||saving||offset+25>=rows.total} onclick={()=>{offset+=25;void read();}}>Next imports</button></div>
  {/if}
</section>

<style>
  .surface{background:white;border:1px solid #cbd5de;padding:1.25rem;margin:1rem 0}.surface h2{margin-top:0}fieldset{display:grid;gap:.8rem;border:1px solid #cbd5de;padding:1rem;min-width:0;margin-top:1rem}li{margin:1rem 0;overflow-wrap:anywhere}li p{margin:.4rem 0}[role=alert]{color:#a12637}.actions{display:flex;gap:.5rem;flex-wrap:wrap}@media(max-width:700px){.surface{padding:1rem}}
</style>
