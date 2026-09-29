<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import DownloadProcessingPanel from './DownloadProcessingPanel.svelte';
  import type { ApiPage, Command, CommandStatus, CompletedDownloadHandling, ProcessingPolicy, MediaDomain, Provider, QueueSnapshot, RefreshSchedule, RefreshTarget } from './api.generated';
  import type {SettingsWriteState} from './api';
  import {readDownloadHandling,saveCompletedDownloadHandling,inheritRefreshSchedule, listCommands, getCommand, createCommand, cancelCommand, deleteCommand, listRefreshSchedules, saveRefreshSchedule, deleteRefreshSchedule, getQueueSnapshot, listProviders, getProvider } from './api';
  let {active=true,relatedWrite='idle',providerVersion=0,onwrite=()=>{}}:{active?:boolean;relatedWrite?:SettingsWriteState;providerVersion?:number;onwrite?:(state:SettingsWriteState)=>void}=$props();
  let settings=$state<Partial<Record<MediaDomain,CompletedDownloadHandling>>>({}),policies=$state<Partial<Record<MediaDomain,ProcessingPolicy>>>({});
  let master=$state<CompletedDownloadHandling|null>(null),masterEnabled=$state(true),masterDirty=$state(false),masterStale=$state(false),scheduleStale=$state(false),scheduleProviderRevision=$state<number|null>(null);
  let childWrite=$state<SettingsWriteState>('idle'),childDirty=$state(false),providerMissing=$state(false);
  $effect(()=>onwrite(mutating||writeReadback?'busy':uncertain?'uncertain':childWrite));
  let seenProviderVersion=-1;
  $effect(()=>{const next=providerVersion,shown=active;untrack(()=>{if(next!==seenProviderVersion){seenProviderVersion=next;++version;}if(shown)void read();else clearTimeout(timer);});});
  function keepDrafts(){return !locked && (!(masterDirty||scheduleDirty||childDirty)||window.confirm('Discard unsaved completed-download and schedule changes for this selection?'));}
  function masterBaseline(){master=settings[media]??null;if(master)masterEnabled=master.enabled;masterDirty=false;masterStale=false;}
  async function reloadSettings(){if(mutating||reading||relatedWrite==='busy'||childWrite==='busy')return false;if((masterDirty||scheduleDirty)&&!window.confirm('Replace unsaved domain and schedule settings with saved values?'))return false;return read(true,true);}
  async function saveMaster(){
    if(locked||reading||!master||masterStale)return;
    mutating=true;clearTimeout(timer);error='';notice='';
    const result=await saveCompletedDownloadHandling(media,{enabled:masterEnabled,revision:master.revision});
    mutating=false;if(!alive)return;
    if(result.ok){master=result.data;settings={...settings,[media]:result.data};masterDirty=false;notice='Completed-download domain setting saved.';writeReadback=true;if(!await read(false,false)){uncertain=true;error='Write accepted; related readback failed. Reload activity.';}writeReadback=false;}
    else{error=`${result.error}${result.code?` (${result.code})`:''}`;uncertain=result.status===undefined||result.status>=500;masterStale=result.status===409;schedulePoll();}
  }
  let commands: ApiPage<Command> | null = $state(null), selectedCommand: Command | null = $state(null), providers: ApiPage<Provider> | null = $state(null), provider: Provider | null = $state(null);
  let mediaFilter = $state(''), statusFilter = $state(''), commandOffset = $state(0), media: MediaDomain = $state('tv'), queueOffset = $state(0);
  let schedules: RefreshSchedule[] = $state([]), snapshot: QueueSnapshot | null = $state(null), snapshotMessage = $state('Select a download client and media scope.');
  let scheduleRevision: number | null = $state(null), scheduleReady = $state(false), scheduleEnabled = $state(false), interval = $state(300), scheduleDirty = $state(false);
  let writeReadback=$state(false);
  let reading = $state(false), mutating = $state(false), uncertain = $state(false), error = $state(''), readError = $state(''), notice = $state(''), confirmDelete = $state<'command' | 'schedule' | null>(null);
  const locked=$derived(mutating||writeReadback||uncertain||relatedWrite!=='idle'||childWrite!=='idle');
  let alive = true, version = 0, timer: ReturnType<typeof setTimeout> | undefined;
  const target: RefreshTarget | null = $derived.by(() => provider ? {provider_id:provider.id,media_type:media} : null);
  const currentSchedule = $derived.by(() => schedules.find(item => item.target.provider_id === provider?.id && item.target.media_type === media));
  const canRefresh = $derived.by(() => !!provider && !providerMissing && provider.enabled && provider.settings.implementation === 'qbittorrent' && !!provider.settings[media]);
  const terminal = (command: Command) => ['succeeded','failed','cancelled'].includes(command.status);
  const date = (seconds: number | null) => seconds === null ? 'Not recorded' : new Date(seconds * 1000).toLocaleString();
  function schedulePoll() {clearTimeout(timer); if (alive && active && !document.hidden) timer = setTimeout(() => void read(), 5000);}
  function editSchedule() {scheduleRevision = currentSchedule?.revision ?? null; scheduleEnabled = currentSchedule?.requested_enabled ?? currentSchedule?.enabled ?? false; interval = currentSchedule?.interval_seconds ?? 300; scheduleDirty = false; scheduleStale=false; scheduleProviderRevision=provider?.revision??null; scheduleReady = true; confirmDelete = null;}
  async function read(reconcile = false, resetSchedule = false) {
    if (!alive || !active || document.hidden || reading || mutating || relatedWrite==='busy') return false;
    const token = version; reading = true; clearTimeout(timer); readError = ''; let failed = false;
    const history = await listCommands({limit:25,offset:commandOffset,...(mediaFilter ? {media_type:mediaFilter as MediaDomain} : {}),...(statusFilter ? {status:statusFilter as CommandStatus} : {})});
    if (!alive || token !== version || document.hidden) {reading = false; schedulePoll(); return false;}
    if (history.ok) commands = history.data; else {readError = history.error; failed = true;}
    const [saved,available] = await Promise.all([readDownloadHandling(provider?.id),listProviders(providers?.offset??0)]);
    if (!alive || token !== version || document.hidden) {reading=false;schedulePoll();return false;}
    if(available.ok)providers=available.data;else{readError+=` ${available.error}`;failed=true;}
    if(saved.ok){
      settings=saved.data.settings;schedules=saved.data.schedules;policies=saved.data.policies;
      if(provider){providerMissing=saved.data.provider===null;if(saved.data.provider)provider=saved.data.provider;}
      if(masterDirty||masterStale||uncertain){if(master && master.revision!==settings[media]?.revision)masterStale=true;}else masterBaseline();
      if(scheduleDirty||scheduleStale||uncertain){if(scheduleRevision!==(currentSchedule?.revision??null)||scheduleProviderRevision!==provider?.revision)scheduleStale=true;}else if(provider)editSchedule();
    }else{readError += ` ${saved.error}`;failed=true;}
    if (selectedCommand) {
      const detail = await getCommand(selectedCommand.id);
      if (!alive || token !== version || document.hidden) {reading = false; schedulePoll(); return false;}
      if (detail.ok) selectedCommand = detail.data; else if (detail.status === 404) selectedCommand = null; else {readError += ` ${detail.error}`; failed = true;}
    }
    if (target) {
      const observed = await getQueueSnapshot(target, queueOffset);
      if (!alive || token !== version || document.hidden) {reading = false; schedulePoll(); return false;}
      if (observed.ok) {snapshot = observed.data; snapshotMessage = '';}
      else {snapshot = null; snapshotMessage = observed.status === 404 ? 'No successful snapshot yet. Download contents are unknown.' : observed.error; if (observed.status !== 404) failed = true;}
    }
    if(resetSchedule && saved.ok){masterBaseline();if(provider)editSchedule();}
    reading = false;
    if (reconcile && !failed) {uncertain = false; error = ''; notice = 'Readback complete. Inspect the recorded state before issuing another request.';}
    schedulePoll();return !failed;
  }
  async function loadProviders(offset = 0) {
    if (reading || mutating) return;
    const token = ++version; reading = true; clearTimeout(timer); const result = await listProviders(offset);
    if (!alive || token !== version || document.hidden) {reading = false; return;}
    reading = false; if (result.ok) providers = result.data; else readError = result.error; schedulePoll();
  }
  async function chooseProvider(id: string, domain: MediaDomain = media) {
    if (reading || !keepDrafts()) return;
    const token = ++version; reading = true; clearTimeout(timer); const result = await getProvider(id);
    if (!alive || token !== version || document.hidden) {reading = false; return;}
    reading = false; error = ''; notice = ''; confirmDelete = null; scheduleReady = false; snapshot = null; queueOffset = 0;
    if (result.ok) {provider = result.data;providerMissing=false; media = domain; masterDirty=false;masterStale=false;scheduleDirty=false;scheduleStale=false;childDirty=false; await read(false, true);} else {readError = result.error; schedulePoll();}
  }
  async function changeScope(next:MediaDomain) {if(!keepDrafts()||reading)return;media=next;masterDirty=false;masterStale=false;scheduleDirty=false;scheduleStale=false;childDirty=false;++version; snapshot = null; queueOffset = 0; scheduleReady = false; confirmDelete = null; await read(false,true);}
  async function filters() {++version; commandOffset = 0; selectedCommand = null; confirmDelete = null; await read();}
  async function inspect(command: Command) {if (reading || mutating) return; selectedCommand = command; confirmDelete = null; await read();}
  async function mutate(action: 'refresh' | 'cancel' | 'deleteCommand' | 'saveSchedule' | 'deleteSchedule' | 'inheritSchedule') {
    if (reading || locked) return;
    if (action === 'refresh' && (!target || !provider || !canRefresh)) return;
    if ((action === 'cancel' || action === 'deleteCommand') && !selectedCommand) return;
    if ((action === 'saveSchedule' || action === 'deleteSchedule' || action === 'inheritSchedule') && (!target || !provider || !scheduleReady || scheduleStale || providerMissing)) return;
    if(action==='inheritSchedule'&&scheduleDirty&&!window.confirm('Replace this schedule draft with the automatic 60-second schedule?'))return;
    const token = version; mutating = true; clearTimeout(timer); error = ''; notice = '';
    const result = action === 'refresh' ? await createCommand({name:'refresh_downloads',target:target!,provider_revision:provider!.revision,priority:'normal'}) : action === 'cancel' ? await cancelCommand(selectedCommand!.id) : action === 'deleteCommand' ? await deleteCommand(selectedCommand!.id) : action === 'saveSchedule' ? await saveRefreshSchedule({target:target!,revision:scheduleRevision,provider_revision:scheduleProviderRevision!,enabled:scheduleEnabled,interval_seconds:interval}) : action==='inheritSchedule'?await inheritRefreshSchedule({target:target!,revision:scheduleRevision,provider_revision:scheduleProviderRevision!}):await deleteRefreshSchedule({target:target!,revision:scheduleRevision!});
    if (!alive || token !== version) {mutating = false; return;}
    mutating = false; confirmDelete = null;
    if (!result.ok) {error = `${result.error}${result.code ? ` (${result.code})` : ''}`; uncertain = result.status===undefined||result.status>=500; if (uncertain) {error += ' The request may have committed. Reload activity to inspect saved state before another action.'; mediaFilter = ''; statusFilter = ''; commandOffset = 0;} if (action === 'saveSchedule' || action === 'deleteSchedule' || action === 'inheritSchedule') scheduleReady = false;if(result.status===409)scheduleStale=true;}
    else {
      notice = action === 'refresh' ? 'Refresh command accepted. Enabled import policies can process confirmed completed downloads.' : action === 'cancel' ? 'Command cancellation recorded. No download was cancelled.' : action === 'deleteCommand' ? 'Terminal command history deleted. Downloads and media were not deleted.' : action === 'saveSchedule' ? 'Refresh schedule saved.' : action==='inheritSchedule'?'Automatic schedule restored.':'Automatic observation suppressed. Existing imports remain.';
      if (action === 'refresh' || action === 'cancel') selectedCommand = result.data as Command;
      if (action === 'deleteCommand') selectedCommand = null;
      if (action === 'saveSchedule' || action==='inheritSchedule') {const updated = result.data as RefreshSchedule; scheduleRevision = updated.revision; scheduleDirty = false;}
      if (action === 'deleteSchedule') {scheduleRevision = null; scheduleReady = false;scheduleDirty=false;}
    }
    if(result.ok){writeReadback=true;if(!await read(false,false)){uncertain=true;error='Write accepted; related settings readback failed. Reload activity.';}writeReadback=false;}else schedulePoll();
  }
  onMount(() => {
    alive = true; void loadProviders().then(() => read());
    const visibility = () => {clearTimeout(timer); if (!document.hidden && active) void read();};
    document.addEventListener('visibilitychange',visibility);
    return () => {alive = false; ++version; clearTimeout(timer); document.removeEventListener('visibilitychange',visibility);};
  });
</script>
<svelte:window onbeforeunload={event=>{if(masterDirty||scheduleDirty||childDirty||mutating||uncertain||childWrite!=='idle'){event.preventDefault();event.returnValue='';}}} />
<section aria-labelledby="activity-heading" aria-busy={reading||mutating||writeReadback||childWrite==='busy'||relatedWrite==='busy'}>
  {#if relatedWrite!=='idle'}<p role="alert">Provider changes have a pending or unknown outcome. Resolve them in Providers before another settings write.</p>{/if}
  {#if uncertain}<p role="alert">The request may have committed. Related writes remain blocked until a complete readback. Reload activity to inspect saved settings.</p>{/if}
  {#if providerMissing}<p role="alert">Selected provider was removed. Retained drafts cannot be saved; select another provider after reviewing them.</p>{/if}
  <div class="heading"><div><h1 id="activity-heading">Activity</h1><p>Download observations, refresh schedules and completed-download imports. Only confirmed submissions with an enabled import policy can be processed automatically.</p></div><button disabled={reading || mutating} onclick={reloadSettings}>Reload activity</button></div>
  {#if reading}<p role="status">Reading activity…</p>{/if}{#if mutating}<p role="status">Saving request…</p>{/if}{#if error}<p class="error" role="alert">{error}</p>{/if}{#if readError}<p class="error" role="alert">{readError}</p>{/if}{#if notice}<p role="status">{notice}</p>{/if}
  <section class="surface" aria-labelledby="cdh-heading"><h2 id="cdh-heading">Completed-download handling</h2>
    <label>Completed-download media<select value={media} disabled={reading||locked} onchange={event=>void changeScope(event.currentTarget.value as MediaDomain)}><option value="tv">TV</option><option value="movies">Movies</option></select></label>
    {#if settings[media]}{@const current=settings[media]!}<p>{current.defined?'Explicitly defined':'Default setting (not explicitly defined)'}. {current.locally_edited?'Native choice saved.':'No native edit recorded.'} Desired domain imports: {current.enabled?'on':'off'}.</p><p>{current.effective_scopes} effective import scopes; {current.observation_disabled_scopes} with automatic observation disabled.</p>
      {#if current.reconciliation_pending}<p role="alert">Automatic coverage incomplete: {current.reconciliation_reason==='schedule_limit'?'refresh schedule capacity reached.':'reconciliation pending.'} Existing valid explicit work may continue.</p>{/if}
    {/if}
    {#if masterStale}<p role="alert">Domain setting changed. Your draft is retained; reload activity before saving.</p>{/if}
    <form onsubmit={event=>{event.preventDefault();void saveMaster();}}><fieldset disabled={!master||reading||locked||masterStale}><legend>{media==='tv'?'TV':'Movie'} default import handling</legend><label class="check"><input type="checkbox" bind:checked={masterEnabled} onchange={()=>masterDirty=true}/>Enable completed-download handling</label><p>Applies to confirmed owned submissions. Disabling stops new imports; already-linked operations retain recovery. Saving also records an explicit native choice.</p><button>Save completed-download handling</button>{#if masterDirty}<p>Unsaved domain changes.</p>{/if}</fieldset></form>
  </section>
  <section class="surface" aria-labelledby="refresh-heading"><h2 id="refresh-heading">Download refresh</h2>
    <div class="controls"><label>Download client<select value={provider?.id ?? ''} disabled={reading || locked} onchange={(event) => {if(event.currentTarget.value) void chooseProvider(event.currentTarget.value);}}><option value="">Select a qBittorrent provider</option>{#if provider && !providers?.items.some(item => item.id === provider?.id)}<option value={provider.id}>{provider.name}</option>{/if}{#each providers?.items.filter(item => item.settings.implementation === 'qbittorrent') ?? [] as item (item.id)}<option value={item.id}>{item.name}{item.enabled ? '' : ' (disabled)'}</option>{/each}</select></label><label>Refresh media<select value={media} disabled={reading || locked} onchange={(event) => {void changeScope(event.currentTarget.value as MediaDomain);}}><option value="tv">TV</option><option value="movies">Movies</option></select></label></div>
    {#if providers}<div class="actions"><button disabled={reading || mutating || providers.offset === 0} onclick={() => loadProviders(Math.max(0,providers!.offset-25))}>Previous clients</button><button disabled={reading || mutating || providers.offset + providers.limit >= providers.total} onclick={() => loadProviders(providers!.offset+providers!.limit)}>Next clients</button><span>Providers {providers.offset + 1}–{Math.min(providers.total,providers.offset+providers.limit)} of {providers.total}; only qBittorrent shown.</span></div>{/if}
    {#if provider}<p>Provider revision {provider.revision}; {provider.enabled ? 'enabled' : 'disabled'}. {provider.settings[media] ? `${media} scope configured.` : `${media} scope is not configured.`}</p>{/if}
    <button class="primary" disabled={reading || mutating || uncertain || !canRefresh} onclick={() => mutate('refresh')}>Refresh downloads</button>
    <h3>Latest successful snapshot</h3>{#if snapshot}<p>Observed {date(snapshot.observed_at)}, provider revision {snapshot.provider_revision}. {snapshot.total} downloads. {snapshot.command_id ? `Command ${snapshot.command_id}` : 'Originating command history is unavailable.'}</p><p>This dated observation may be stale. Unassociated downloads have no known library target.</p><div class="scroll"><table><thead><tr><th>Name</th><th>Scope / category</th><th>Status</th><th>Client progress</th><th>Library association</th></tr></thead><tbody>{#each snapshot.items as item (item.download.hash)}<tr><td>{item.download.name}<small>{item.download.hash}</small></td><td>{item.download.domain} / {item.download.category}</td><td>{item.download.status}{item.download.diagnostic ? ` (${item.download.diagnostic})` : ''}</td><td>{Math.round(item.download.progress * 100)}%</td><td>{item.association ? `${item.association.media_type} ${item.association.id}` : 'Unknown / unassociated'}</td></tr>{:else}<tr><td colspan="5">Successful observation contains no downloads on this page.</td></tr>{/each}</tbody></table></div><div class="actions"><button disabled={reading || mutating || queueOffset === 0} onclick={() => {queueOffset = Math.max(0,queueOffset-25); void read();}}>Previous downloads</button><button disabled={reading || mutating || queueOffset+25>=snapshot.total} onclick={() => {queueOffset+=25; void read();}}>Next downloads</button></div>{:else}<p>{snapshotMessage}</p>{/if}
  </section>
  {#if provider}{#key `${provider.id}:${media}`}<DownloadProcessingPanel {provider} {media} {active} currentPolicy={policies[media]??null} relatedWrite={reading||mutating||writeReadback?'busy':uncertain?'uncertain':relatedWrite} onreload={()=>read(true,false)} onwrite={state=>childWrite=state} ondirty={value=>childDirty=value} />{/key}{/if}
  <section class="surface" aria-labelledby="schedule-heading"><h2 id="schedule-heading">Refresh schedules</h2><p>Enabled schedules become due immediately. Last accepted run records command admission, not download-refresh success.</p>
    <ul>{#each schedules as item (`${item.target.provider_id}:${item.target.media_type}`)}<li><button disabled={reading || locked} onclick={() => chooseProvider(item.target.provider_id,item.target.media_type)}>{item.target.media_type}: {item.target.provider_id}</button> {item.intent==='inherited'?'Automatic (inherits import policy)':`Explicitly ${item.requested_enabled?'on':'off'}`}; effective observation {item.enabled ? 'enabled' : 'disabled'}, revision {item.revision}, provider revision {item.provider_revision}; next due {date(item.next_run_at)}; last accepted run {date(item.last_run_at)}{item.error_code ? `; error ${item.error_code}` : ''}</li>{:else}<li>No saved schedules.</li>{/each}</ul>
    {#if provider}<button disabled={reading || locked} onclick={reloadSettings}>Reload schedule settings</button><form onsubmit={(event) => {event.preventDefault(); void mutate('saveSchedule');}} oninput={() => scheduleDirty = true}><fieldset disabled={reading || locked || !scheduleReady || scheduleStale || providerMissing}><legend>{media} schedule for {provider.name}</legend><p>{scheduleRevision === null ? 'New schedule (no saved revision).' : `Editing schedule revision ${scheduleRevision}.`}{scheduleDirty ? ' Unsaved changes.' : ''}</p><label class="check"><input type="checkbox" bind:checked={scheduleEnabled} />Request automatic observation</label><label>Interval seconds<input type="number" bind:value={interval} required min="60" max="86400" step="1" /></label><button disabled={!provider.settings[media] || (scheduleEnabled && !provider.enabled)}>Save refresh schedule</button></fieldset></form>{#if scheduleStale}<p role="alert">Schedule or provider changed. Your draft is retained; reload settings before saving.</p>{/if}{#if policies[media]?.observation_suppressed}<p>Automatic observation suppressed by schedule deletion.</p>{/if}<button disabled={reading||locked||!scheduleReady||scheduleStale||providerMissing||!provider.settings[media]} onclick={()=>mutate('inheritSchedule')}>Use automatic schedule (60 seconds)</button>{#if scheduleRevision !== null}<button disabled={reading || locked || !scheduleReady || scheduleStale || providerMissing} onclick={() => confirmDelete = 'schedule'}>Delete schedule</button>{/if}{/if}
    {#if confirmDelete === 'schedule'}<div class="confirmation"><p>Delete the {media} refresh schedule for {provider?.name}? This suppresses future automatic observation until you explicitly save or restore an automatic schedule. Existing commands, snapshots and import recovery remain.</p><button disabled={reading || locked} onclick={() => mutate('deleteSchedule')}>Confirm delete schedule</button><button onclick={() => confirmDelete = null}>Keep schedule</button></div>{/if}
  </section>
  <section class="surface" aria-labelledby="commands-heading"><h2 id="commands-heading">Commands</h2><div class="controls"><label>Command media<select bind:value={mediaFilter} disabled={reading || locked} onchange={(event) => {mediaFilter = event.currentTarget.value; void filters();}}><option value="">All media</option><option value="tv">TV</option><option value="movies">Movies</option></select></label><label>Command status<select bind:value={statusFilter} disabled={reading || locked} onchange={(event) => {statusFilter = event.currentTarget.value; void filters();}}><option value="">All statuses</option>{#each ['queued','running','retry_wait','succeeded','failed','cancelled'] as status}<option value={status}>{status}</option>{/each}</select></label></div>
    {#if commands}<p>{commands.total} matching commands</p><div class="scroll"><table><thead><tr><th>Command</th><th>Target</th><th>State</th><th>Attempts / error</th><th>Created</th></tr></thead><tbody>{#each commands.items as command (command.id)}<tr><td><button disabled={reading || mutating} onclick={() => inspect(command)}>Inspect command {command.id}</button></td><td>{command.target.media_type}<small>{command.target.provider_id}</small></td><td>{command.status}</td><td>{command.attempts} / {command.error_code ?? 'No recorded error'}</td><td>{date(command.created_at)}</td></tr>{:else}<tr><td colspan="5">No matching commands.</td></tr>{/each}</tbody></table></div><div class="actions"><button disabled={reading || mutating || commandOffset===0} onclick={() => {commandOffset=Math.max(0,commandOffset-25); void read();}}>Previous commands</button><button disabled={reading || mutating || commandOffset+25>=commands.total} onclick={() => {commandOffset+=25; void read();}}>Next commands</button></div>{/if}
    {#if selectedCommand}<section aria-label="Command detail"><h3>{selectedCommand.name}: {selectedCommand.status}</h3><dl><dt>ID</dt><dd>{selectedCommand.id}</dd><dt>Target</dt><dd>{selectedCommand.target.media_type} / {selectedCommand.target.provider_id}</dd><dt>Provider revision</dt><dd>{selectedCommand.provider_revision}</dd><dt>Priority / attempts</dt><dd>{selectedCommand.priority} / {selectedCommand.attempts}</dd><dt>Created</dt><dd>{date(selectedCommand.created_at)}</dd><dt>Started</dt><dd>{date(selectedCommand.started_at)}</dd><dt>Completed</dt><dd>{date(selectedCommand.completed_at)}</dd><dt>Next attempt</dt><dd>{terminal(selectedCommand) ? 'No further attempt' : date(selectedCommand.next_attempt_at)}</dd><dt>Error</dt><dd>{selectedCommand.error_code ?? 'No recorded error'}</dd><dt>Items observed</dt><dd>{selectedCommand.status === 'succeeded' ? selectedCommand.items_observed : 'No successful result from this command'}</dd></dl>{#if terminal(selectedCommand)}<button disabled={reading || locked} onclick={() => confirmDelete = 'command'}>Delete command history</button>{:else}<button disabled={reading || locked} onclick={() => mutate('cancel')}>Cancel command</button><p>Cancellation stops this refresh command, not client downloads.</p>{/if}</section>{/if}
    {#if confirmDelete === 'command'}<div class="confirmation"><p>Delete only terminal command {selectedCommand?.id}? Downloads and media remain intact.</p><button disabled={reading || locked} onclick={() => mutate('deleteCommand')}>Confirm delete history</button><button onclick={() => confirmDelete = null}>Keep history</button></div>{/if}
  </section>
</section>
<style>
  .heading{display:flex;justify-content:space-between;align-items:start;gap:1rem}.surface{background:white;border:1px solid #cbd5de;padding:1.25rem;margin:1rem 0}.surface h2{margin-top:0}.controls{display:flex;flex-wrap:wrap;gap:1rem;margin-bottom:1rem}.controls label{min-width:180px;flex:1}.scroll{overflow-x:auto}table{width:100%;border-collapse:collapse;text-align:left}th,td{padding:.7rem;border-bottom:1px solid #dce3e9;vertical-align:top}small{display:block;overflow-wrap:anywhere;color:#596b7b}fieldset{display:grid;gap:.8rem;border:1px solid #cbd5de;padding:1rem;min-width:0}.check{display:flex;gap:.6rem;align-items:center}.check input{width:auto}li{margin:.7rem 0;overflow-wrap:anywhere}.confirmation{padding:1rem;border-left:3px solid #a12637;background:#fff0f2;margin-top:1rem}.actions{margin-top:1rem}.confirmation button{margin-right:.5rem}@media(max-width:700px){.heading{flex-wrap:wrap}.surface{padding:1rem}.controls{display:block}.controls label{margin-bottom:.7rem}}
</style>
