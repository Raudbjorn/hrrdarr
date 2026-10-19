<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import type { ManualImportRequest, MediaTarget, Operation, Mode, CommandPriority, ManualImportCommand } from './api.generated';
  import { previewManualImport, executeImport, createManualImportCommands, getManualImportCommand, cancelManualImportCommand, reconcileManualImports } from './api';
  import type { SettingsWriteState } from './api';
  import { manualImportAction, pathDiagnostic } from './pathDiagnostics';
  import {
    MANUAL_IMPORT_LIMIT, MANUAL_IMPORT_RECEIPT_BYTES, createManualImportEntry, decodeManualImportReceipt,
    encodeManualImportReceipt, migrateLegacyManualImportReceipt, validateManualImportSelection,
    validateManualImportOperation, validateManualImportCommand, validateManualImportReconciliation,
    validateManualImportAdmission, markAdmissionUncertain, markExecutionUncertain, recordKnownCommand, markCancelUncertain, resolveCancelUncertain,
  } from './manual-import-state';
  import type { ManualImportEntry, ManualImportOwner } from './manual-import-state';

  let { target, label, owner = null, active = true, relatedWrite = 'idle', onwrite, ondirty, oncomplete }: {
    target: MediaTarget | null; label: string; owner?: ManualImportOwner | null; active?: boolean;
    relatedWrite?: SettingsWriteState; onwrite?: (state: SettingsWriteState) => void; ondirty?: (dirty: boolean) => void;
    oncomplete: (event: {operationId:string;target:MediaTarget;owner:ManualImportOwner|null}) => void;
  } = $props();
  type RecoveryOperation = Operation & {path_failure_code?:string|null;recovery_required?:boolean};
  type Observation = {operation:RecoveryOperation|null;fresh:boolean;ownershipFresh:boolean;activeCommand:ManualImportCommand|null;error:string};
  type CommandObservation = {command:ManualImportCommand|null;fresh:boolean;error:string};
  const KEY = 'hrrdarr.import.collection.v2', LEGACY_KEY = 'hrrdarr.import.receipt.v1';
  const POLL_MS = 5000, MAX_BACKOFF_MS = 60000, COMMAND_READ_LIMIT = 100, COMMAND_CONCURRENCY = 4;
  let entries = $state<ManualImportEntry[]>([]), observations = $state<Record<string,Observation>>({});
  let commands = $state<Record<string,CommandObservation>>({}), selected = $state<string[]>([]);
  let draftTarget = $state<MediaTarget|null>(null), draftOwner = $state<ManualImportOwner|null>(null), draftLabel = $state('');
  let source = $state(''), destination = $state(''), mode = $state<Mode>('copy'), priority = $state<CommandPriority>('normal');
  let keptTarget = $state(''), busy = $state(false), checking = $state(false), ready = $state(false);
  let readError = $state('');
  let error = $state(''), notice = $state(''), storageError = $state(''), storageBlocked = $state(false);
  let exportText = $state(''), exportOpen = $state(false);
  const draftDirty = $derived(!!source || !!destination || mode !== 'copy');
  const mutationBlocked = $derived(busy || !ready || relatedWrite !== 'idle' || storageBlocked || !!storageError);
  const incomingKey = $derived(target ? `${target.media_type}:${target.id}` : '');
  const draftKey = $derived(draftTarget ? `${draftTarget.media_type}:${draftTarget.id}` : '');
  const targetChanged = $derived(!!target && !!draftTarget && incomingKey !== draftKey && keptTarget !== incomingKey);
  const selection = $derived(entries.filter(e => selected.includes(e.operationId)));
  let mounted = false, generation = 0, failures = 0, commandCursor = 0;
  let timer: ReturnType<typeof setTimeout>|undefined, lastStored: string|null = null;
  const completed = new Set<string>();

  $effect(() => { onwrite?.(busy ? 'busy' : 'idle'); });
  $effect(() => { ondirty?.(draftDirty); });
  $effect(() => {
    if (ready && !draftTarget && target && !busy) captureTarget();
  });
  $effect(() => {
    if (!ready) return;
    const visible = active;
    untrack(() => {
      invalidate(entries.map(e=>e.operationId));
      if (visible) schedule(0); else clearTimeout(timer);
    });
  });
  function captureTarget() {
    if (!target || busy) return;
    draftTarget = {...target}; draftOwner = owner ? {...owner} : null; draftLabel = label;
    source = ''; destination = ''; mode = 'copy'; keptTarget = '';
  }
  function discardAndChange() {
    if (busy || !target) return;
    if (draftDirty && !window.confirm('Discard this unpreviewed import form and use the selected target?')) return;
    captureTarget();
  }
  function current(id:string) { return entries.find(e => e.operationId === id); }
  function observed(e:ManualImportEntry) { return observations[e.operationId]; }
  function replace(next:ManualImportEntry) { return entries.map(e => e.operationId === next.operationId ? next : e); }
  function save(next:readonly ManualImportEntry[], keepOnFailure = true): boolean {
    const encoded = encodeManualImportReceipt({version:2,entries:next});
    if (!encoded.ok) { storageError = encoded.error; if (keepOnFailure) entries = [...next]; return false; }
    if (storageBlocked) { if (keepOnFailure) entries = [...next]; return false; }
    try {
      if (localStorage.getItem(KEY) !== lastStored) {
        storageBlocked = true; storageError = 'The saved collection changed in another tab. New authorization is blocked. Export this tab’s retained identities and reload to reconcile the saved collection.';
        if (keepOnFailure) entries = [...next]; return false;
      }
      localStorage.setItem(KEY, encoded.data); lastStored = encoded.data; entries = [...next]; storageError = ''; return true;
    } catch {
      storageError = 'Browser storage is unavailable or full. New authorization is blocked. Export the retained collection, keep this tab open, then retry saving.';
      if (keepOnFailure) entries = [...next]; return false;
    }
  }
  function recoveryExport():string {
    const result=encodeManualImportReceipt({version:2,entries});
    if(result.ok)return result.data;
    // Even if the complete receipt has exhausted its byte budget, preserve bounded IDs for recovery.
    return ['Full versioned receipt unavailable: '+result.error,
      'Identity list only; not a restorable receipt. Retain captured paths from this open tab.',
      ...entries.flatMap(e=>[
        `operation ${e.operationId} target ${e.request.target.media_type}:${e.request.target.id} admission_uncertain=${e.admissionUncertain} execution_uncertain=${e.executionUncertain}`,
        ...e.commands.map(c=>`command ${c.id} batch ${c.batchId} cancel_uncertain=${e.cancelUncertainIds.includes(c.id)}`),
      ])].join('\n');
  }
  function exportReceipt() { exportText=recoveryExport();exportOpen=true; }
  function downloadReceipt() {
    const data=recoveryExport();
    const url = URL.createObjectURL(new Blob([data],{type:'text/plain;charset=utf-8'}));
    const link = document.createElement('a'); link.href = url; link.download = 'hrrdarr-manual-imports.txt'; link.click();
    setTimeout(() => URL.revokeObjectURL(url),0);
  }
  function invalidate(ids:readonly string[]) {
    ++generation;
    const next = {...observations};
    for (const id of ids) if (next[id]) next[id] = {...next[id],fresh:false,ownershipFresh:false};
    observations = next;
  }
  function acceptOperation(e:ManualImportEntry, operation:Operation, ownershipFresh:boolean, activeCommand:ManualImportCommand|null) {
    const result = validateManualImportOperation(e,operation);
    if (!result.ok) { observations = {...observations,[e.operationId]:{operation:null,fresh:false,ownershipFresh:false,activeCommand:null,error:result.error}}; return; }
    observations = {...observations,[e.operationId]:{operation:result.data.operation as RecoveryOperation,fresh:result.data.recoveryProjectionKnown,ownershipFresh,activeCommand,error:result.data.recoveryProjectionKnown?'':'Recovery details are unavailable. Refresh before authorizing another action.'}};
    if (operation.status === 'complete' && !completed.has(e.operationId)) {
      completed.add(e.operationId); oncomplete({operationId:e.operationId,target:{...e.request.target},owner:e.owner});
    }
  }
  function schedule(delay = Math.min(POLL_MS * 2 ** failures,MAX_BACKOFF_MS)) {
    clearTimeout(timer);
    if (mounted && active && !document.hidden && entries.some(e=>observations[e.operationId]?.operation?.status!=='complete' || !observations[e.operationId]?.fresh || !observations[e.operationId]?.ownershipFresh || observations[e.operationId]?.activeCommand || e.commands.some(c=>!commands[c.id]?.fresh || (commands[c.id]?.command && ['queued','running','retry_wait'].includes(commands[c.id].command!.status)))) && !busy && !checking)
      timer = setTimeout(() => { void check(); },delay);
  }
  async function check() {
    if (!mounted || checking || busy || !entries.length) return;
    clearTimeout(timer); checking = true; readError='';
    const ticket = ++generation, captured = [...entries];
    observations = Object.fromEntries(Object.entries(observations).map(([id,value]) => [id,{...value,fresh:false,ownershipFresh:false}]));
    const result = await reconcileManualImports(captured.map(e => e.operationId));
    if (!mounted || ticket !== generation) { checking = false; schedule(); return; }
    let failed = false;
    if (!result.ok) { failed = true; readError = result.error; }
    else {
      const valid = validateManualImportReconciliation(captured,result.data);
      if (!valid.ok) { failed = true; readError = valid.error; }
      else {
        let updated = [...entries];
        for (let i=0;i<captured.length;i++) {
          const e = current(captured[i].operationId); if (!e) continue;
          const row = valid.data[i]; let next = e;
          if (row.active_command) {
            const retained = recordKnownCommand(next,row.active_command);
            if (!retained.ok) { failed = true; readError = retained.error; observations = {...observations,[e.operationId]:{operation:null,fresh:false,ownershipFresh:false,activeCommand:null,error:retained.error}}; continue; }
            next = retained.data;
            commands = {...commands,[row.active_command.id]:{command:row.active_command,fresh:true,error:''}};
          }
          updated = updated.map(old => old.operationId === next.operationId ? next : old);
          if (row.operation) acceptOperation(next,row.operation,true,row.active_command);
          else observations = {...observations,[e.operationId]:{operation:null,fresh:false,ownershipFresh:true,activeCommand:null,error:'The saved operation was not found. Keep its identity; do not create a replacement transfer to bypass this conflict.'}};
        }
        if (JSON.stringify(updated) !== JSON.stringify(entries)) save(updated);
      }
    }
    // Bounded round-robin historical readback. Current active wrappers already came from reconciliation.
    const known = entries.flatMap(e => e.commands.map(c => ({entry:e,id:c.id})))
      .filter(item => !commands[item.id]?.fresh || (commands[item.id]?.command && ['queued','running','retry_wait'].includes(commands[item.id].command!.status)) || item.entry.cancelUncertainIds.includes(item.id));
    const pending = known.filter(item => item.entry.cancelUncertainIds.includes(item.id));
    const rotation = known.length ? [...known.slice(commandCursor % known.length),...known.slice(0,commandCursor % known.length)] : [];
    const ids = new Set<string>();
    const work = [...pending,...rotation].filter(item => {if(ids.has(item.id))return false;ids.add(item.id);return true;}).slice(0,COMMAND_READ_LIMIT);
    commandCursor = known.length ? (commandCursor + work.length) % known.length : 0;
    let index = 0;
    await Promise.all(Array.from({length:Math.min(COMMAND_CONCURRENCY,work.length)},async () => {
      while(index < work.length) {
        const item = work[index++];
        if (!mounted || ticket !== generation) return;
        const reply = await getManualImportCommand(item.id);
        if (!mounted || ticket !== generation) return;
        const e = current(item.entry.operationId); if (!e) continue;
        if (!reply.ok) {
          if(reply.status!==404)failed = true;
          commands = {...commands,[item.id]:{command:reply.status===404?null:commands[item.id]?.command??null,fresh:reply.status===404,error:reply.status===404?'Command history is no longer available. This does not prove the import did not run.':reply.error}}; continue;
        }
        const checked = validateManualImportCommand(e,item.id,reply.data);
        if (!checked.ok) { failed = true; commands = {...commands,[item.id]:{command:null,fresh:false,error:checked.error}}; continue; }
        commands = {...commands,[item.id]:{command:checked.data,fresh:true,error:''}};
        if (e.cancelUncertainIds.includes(item.id)) {
          const resolved = resolveCancelUncertain(e,item.id,checked.data);
          if (resolved.ok) save(replace(resolved.data));
        }
      }
    }));
    if (!mounted || ticket !== generation) {checking = false; schedule(); return;}
    checking = false; failures = failed ? Math.min(failures+1,4) : 0; schedule();
  }
  function clean(e:ManualImportEntry):boolean {
    const view = observed(e), op = view?.operation;
    return !!view?.fresh && view.ownershipFresh && !view.activeCommand && !!op && op.status==='preview' && !op.error_code && op.path_failure_code===null;
  }
  function cancelTargetsAbsent(e:ManualImportEntry):boolean {
    // A fresh 404 prevents this immutable wrapper from receiving a late cancellation.
    // Keep its unresolved provenance: absence does not prove cancellation or no import effects.
    return e.cancelUncertainIds.every(id => commands[id]?.fresh===true && commands[id].command===null);
  }
  function queueEligible(e:ManualImportEntry):boolean { return clean(e) && cancelTargetsAbsent(e) && e.commands.length<MANUAL_IMPORT_LIMIT; }
  function directAction(e:ManualImportEntry):'execute'|'resume'|null {
    const view = observed(e), op = view?.operation;
    if (!view?.fresh || !view.ownershipFresh || view.activeCommand || !op || !cancelTargetsAbsent(e)) return null;
    const action = manualImportAction(op,true);
    // A measured/marked abandoned attempt fences late admissions. It may be explicitly recovered
    // even while the provenance of an older HTTP admission remains uncertain.
    if (e.admissionUncertain && !(action==='resume' && op.recovery_required===true && !!(op.path_failure_code || op.error_code))) return null;
    return action;
  }
  function canForget(e:ManualImportEntry):boolean {
    const view = observed(e);
    if (!view?.fresh || !view.ownershipFresh || view.activeCommand) return false;
    // Completion fences late effects without proving the outstanding HTTP requests settled.
    if (view.operation?.status==='complete') return true;
    if (e.admissionUncertain || e.executionUncertain || e.cancelUncertainIds.length || !clean(e)) return false;
    if (!e.authorized) return true;
    return e.commands.length>0 && e.commands.every(c=>commands[c.id]?.fresh && commands[c.id].command?.status==='cancelled');
  }
  function toggle(e:ManualImportEntry, checked:boolean) {
    selected = checked ? [...new Set([...selected,e.operationId])] : selected.filter(id=>id!==e.operationId);
  }
  async function preview() {
    if (mutationBlocked || !draftTarget || entries.length>=MANUAL_IMPORT_LIMIT) return;
    const request:ManualImportRequest = {target:{...draftTarget},source,destination,mode};
    // Local request validation uses a disposable UUID only as a validator; it is never stored or sent.
    const prepared = createManualImportEntry('00000000-0000-4000-8000-000000000000',request,draftLabel,draftOwner);
    if (!prepared.ok) {error=prepared.error;return;}
    const capturedLabel=draftLabel, capturedOwner=draftOwner;
    busy=true;invalidate([]);error='';notice='';
    const response=await previewManualImport(request);
    if (!mounted) return;
    busy=false;
    if (!response.ok) {error=`${response.error} No transfer was authorized. A lost preview response may leave an unexecuted preview on the server.`;schedule();return;}
    const created=createManualImportEntry(response.data.id,request,capturedLabel,capturedOwner);
    if (!created.ok) {error=created.error;schedule();return;}
    const valid=validateManualImportOperation(created.data,response.data);
    if (!valid.ok || response.data.status!=='preview' || entries.some(e=>e.operationId===created.data.operationId)) {error=valid.ok?'Preview response did not identify a new unexecuted operation.':valid.error;schedule();return;}
    save([...entries,created.data]); acceptOperation(created.data,response.data,false,null);
    selected=[...selected,created.data.operationId]; source='';destination='';mode='copy';draftTarget=null;keptTarget='';
    notice='Preview added. Review its captured paths and mode before authorizing execution or queueing.';
    await check();
  }
  async function queue(rows:ManualImportEntry[] = selection) {
    if (mutationBlocked || !rows.length || rows.some(e=>!queueEligible(e))) return;
    const chosen=validateManualImportSelection(rows); if(!chosen.ok){error=chosen.error;return;}
    const chosenPriority=priority, before=new Map(rows.map(e=>[e.operationId,e.admissionUncertain]));
    const ids=chosen.data;
    const next=entries.map(e=>ids.includes(e.operationId)?markAdmissionUncertain(e):e);
    if(!save(next,false))return;
    busy=true;invalidate(ids);error='';notice='';
    const response=await createManualImportCommands({operation_ids:ids,priority:chosenPriority});
    if(!mounted)return;
    let updated=[...entries];
    if(response.ok){
      const valid=validateManualImportAdmission(rows,response.data,chosenPriority);
      if(!valid.ok)error=`${valid.error} Admission outcome remains uncertain; check status before any further authorization.`;
      else{
        for(let i=0;i<rows.length;i++){
          const e=current(rows[i].operationId);if(!e)continue;
          const retained=recordKnownCommand(e,valid.data[i]);
          if(!retained.ok){error=retained.error;continue;}
          updated=updated.map(old=>old.operationId===e.operationId?{...retained.data,admissionUncertain:before.get(e.operationId)===true}:old);
          commands={...commands,[valid.data[i].id]:{command:valid.data[i],fresh:true,error:''}};
        }
        save(updated);selected=selected.filter(id=>!ids.includes(id));
        notice=`Queued ${rows.length} import${rows.length===1?'':'s'}. Each operation completes independently. Older unresolved admission requests remain recorded.`;
      }
    }else{
      // Only explicit admission rejections settle this request; timeouts/storage/server failures do not.
      const rejected = response.status!==undefined && [400,404,409,413,429].includes(response.status);
      if(rejected)save(entries.map(e=>ids.includes(e.operationId)?{...e,admissionUncertain:before.get(e.operationId)===true}:e));
      error=`${response.error} ${rejected?'This admission was rejected. Refresh the selected operations before another explicit authorization.':'Admission outcome is unknown. Keep these operation IDs and check status; no automatic retry was sent.'}`;
    }
    busy=false;await check();
  }
  async function execute(e:ManualImportEntry) {
    if(mutationBlocked || !directAction(e))return;
    if(!save(replace(markExecutionUncertain(e)),false))return;
    busy=true;invalidate([e.operationId]);error='';notice='';
    const response=await executeImport(e.operationId);
    if(!mounted)return;
    const latest=current(e.operationId);
    if(latest && response.ok){
      const valid=validateManualImportOperation(latest,response.data);
      if(valid.ok){acceptOperation(latest,response.data,false,null);}
      else error=`${valid.error} Check the same operation before another action.`;
    }else if(!response.ok)error=`${response.error} Execution may have started. Check this same operation before explicitly resuming; do not create a replacement preview.`;
    busy=false;await check();
  }
  function cancelEligible(e:ManualImportEntry,c:ManualImportCommand):boolean {
    const view=observed(e), observation=commands[c.id];
    return !!view?.fresh && view.ownershipFresh && view.operation?.status==='preview' && view.operation.path_failure_code===null
      && ['queued','retry_wait'].includes(c.status) && !!observation?.fresh && observation.command?.status===c.status && view.activeCommand?.id===c.id && ['queued','retry_wait'].includes(view.activeCommand.status) && !e.cancelUncertainIds.includes(c.id);
  }
  async function cancel(e:ManualImportEntry,c:ManualImportCommand) {
    if(mutationBlocked || !cancelEligible(e,c))return;
    const marked=markCancelUncertain(e,c.id);if(!marked.ok){error=marked.error;return;}
    if(!save(replace(marked.data),false))return;
    busy=true;invalidate([e.operationId]);commands={...commands,[c.id]:{...commands[c.id],fresh:false}};error='';notice='';
    const response=await cancelManualImportCommand(c.id);
    if(!mounted)return;
    const latest=current(e.operationId);
    if(latest && response.ok){
      const valid=validateManualImportCommand(latest,c.id,response.data);
      if(valid.ok && valid.data.status==='cancelled'){
        commands={...commands,[c.id]:{command:valid.data,fresh:true,error:''}};
        const resolved=resolveCancelUncertain(latest,c.id,valid.data);if(resolved.ok)save(replace(resolved.data));
        notice='This command was cancelled. Cancellation does not undo file effects or revoke another admission request still in flight.';
      }else error=valid.ok?'Cancellation response did not confirm a cancelled command. Check status.':valid.error;
    }else if(!response.ok)error=`${response.error} Check this command and operation to resolve cancellation; no cancel retry was sent.`;
    busy=false;await check();
  }
  function dropViews(e:ManualImportEntry) {
    const next={...observations};delete next[e.operationId];observations=next;
    const remaining={...commands};for(const c of e.commands)delete remaining[c.id];commands=remaining;
    completed.delete(e.operationId);
  }
  function forget(e:ManualImportEntry) {
    if(busy || !canForget(e))return;
    const wasComplete=observed(e)?.operation?.status==='complete';
    invalidate([e.operationId]);
    if(save(entries.filter(row=>row.operationId!==e.operationId),false)){
      selected=selected.filter(id=>id!==e.operationId);dropViews(e);
      notice=wasComplete?'Completed operation removed from this browser collection.':'Unexecuted preview removed locally. Its server preview has not been deleted.';
      if(!entries.length && !draftDirty)draftTarget=null;
    }
  }
  function edit(e:ManualImportEntry) {
    if(busy || e.authorized || !canForget(e))return;
    if(draftDirty && !window.confirm('Discard the current unpreviewed form and edit this preview request?'))return;
    if(!save(entries.filter(row=>row.operationId!==e.operationId),false))return;
    invalidate([e.operationId]);dropViews(e);
    selected=selected.filter(id=>id!==e.operationId);draftTarget={...e.request.target};draftOwner=e.owner;draftLabel=e.label;
    source=e.request.source;destination=e.request.destination;mode=e.request.mode;keptTarget='';
    notice='Editing creates a new preview. The old unexecuted preview remains on the server and was not authorized.';
  }
  function legacyLoad():boolean {
    const raw=localStorage.getItem(LEGACY_KEY);if(!raw)return true;
    const migrated=migrateLegacyManualImportReceipt(raw);
    if(!migrated.ok){storageBlocked=true;storageError=`${migrated.error} Recover the saved operation identity before starting new work.`;return false;}
    entries=[...migrated.data.entries];
    if(save(entries)){localStorage.removeItem(LEGACY_KEY);return true;}
    return false;
  }
  onMount(()=>{
    mounted=true;
    try{
      lastStored=localStorage.getItem(KEY);
      if(lastStored!==null){const decoded=decodeManualImportReceipt(lastStored);if(decoded.ok)entries=[...decoded.data.entries];else{storageBlocked=true;storageError=`${decoded.error} Keep the saved data and recover its identities before new authorization.`;}}
      else legacyLoad();
    }catch{storageBlocked=true;storageError='Browser receipts could not be read. Recover saved import identities before authorizing new work.';}
    ready=true;
    const visibility=()=>{if(document.hidden){clearTimeout(timer);invalidate(entries.map(e=>e.operationId));}else schedule(0);};
    const storage=(event:StorageEvent)=>{
      if(event.key!==KEY && event.key!==LEGACY_KEY && event.key!==null)return;
      storageBlocked=true;storageError='Import receipts changed in another tab. Export retained identities and reload before new authorization.';
      invalidate(entries.map(e=>e.operationId));
    };
    const unload=(event:BeforeUnloadEvent)=>{
      if(draftDirty || busy || (storageError && entries.length>0) || entries.some(e=>observed(e)?.operation?.status!=='complete' && (e.admissionUncertain || e.executionUncertain || e.cancelUncertainIds.length))){event.preventDefault();event.returnValue='';}
    };
    document.addEventListener('visibilitychange',visibility);window.addEventListener('storage',storage);window.addEventListener('beforeunload',unload);
    return()=>{mounted=false;++generation;clearTimeout(timer);document.removeEventListener('visibilitychange',visibility);window.removeEventListener('storage',storage);window.removeEventListener('beforeunload',unload);onwrite?.('idle');ondirty?.(false);};
  });
</script>

<section aria-labelledby="import-heading" class="import-panel">
  <h2 id="import-heading">Manual import</h2>
  <p>Preview files for empty episode or movie targets, then explicitly execute one or queue selected imports. Use existing library and destination directories. Replacement, automatic matching and directory creation are not supported.</p>
  <p>{entries.length} / {MANUAL_IMPORT_LIMIT} retained imports. Reviewed rows keep their captured targets when you navigate.</p>
  {#if targetChanged}
    <div class="target-notice" role="status"><p>The selected library target changed. This form still belongs to {draftLabel}.</p><div class="actions"><button disabled={busy} onclick={()=>keptTarget=incomingKey}>Keep current draft</button><button disabled={busy} onclick={discardAndChange}>Discard form and use {label}</button></div></div>
  {/if}
  <form onsubmit={(event)=>{event.preventDefault();void preview();}}>
    <h3>Prepare an import</h3><p>{draftTarget?`Selected target: ${draftLabel} (${draftTarget.media_type} ${draftTarget.id})`:'Select a movie or an episode without a file.'}</p>
    <label>Source file<textarea bind:value={source} required maxlength="4096" rows="2" disabled={busy}></textarea></label>
    <label>Destination file<textarea bind:value={destination} required maxlength="4096" rows="2" disabled={busy}></textarea></label>
    <label>Transfer mode<select bind:value={mode} disabled={busy}><option value="copy">Copy (keep source)</option><option value="move">Move (retire source after import)</option><option value="hardlink">Hardlink (same filesystem)</option></select></label>
    <button class="primary" disabled={!draftTarget || mutationBlocked || entries.length>=MANUAL_IMPORT_LIMIT}>Preview import</button>
  </form>
  {#if entries.length}
    <div class="batch-review">
      <h3>Reviewed collection</h3>
      <p>{selection.length} selected. Queueing authorizes the captured source, destination and mode for every selected row. Admission is atomic; file transfers can finish with different outcomes.</p>
      <label>Import priority<select bind:value={priority} disabled={busy}><option value="normal">Normal</option><option value="high">High</option></select></label>
      <div class="actions"><button disabled={mutationBlocked || !selection.length || selection.some(e=>!queueEligible(e))} onclick={()=>queue()}>{selection.some(e=>e.admissionUncertain)?'Authorize selected queue again':'Queue selected imports'}</button><button disabled={checking || busy} onclick={()=>check()}>Check status</button><button disabled={busy} onclick={exportReceipt}>Export recovery receipt</button></div>
      {#if selection.some(e=>e.admissionUncertain)}<p>Some selected rows have an unresolved admission. Authorizing the selected queue again is a new explicit authorization of these same operations after fresh reconciliation; the old request remains uncertain.</p>{/if}
    </div>
    <ol class="import-rows">
      {#each entries as entry (entry.operationId)}
        {@const view=observed(entry)}{@const op=view?.operation}{@const action=directAction(entry)}
        {@const diagnostic=op?pathDiagnostic(op.path_failure_code,op.recovery_required===true,op.status==='complete'):null}
        <li><article aria-label={`Import ${entry.label}`}>
          <h3>Confirmed import: {entry.label}</h3>
          <label class="select-row"><input type="checkbox" checked={selected.includes(entry.operationId)} disabled={busy || (!queueEligible(entry) && !selected.includes(entry.operationId))} onchange={event=>toggle(entry,event.currentTarget.checked)} />Select {entry.label} for queued import</label>
          <dl><dt>Target</dt><dd>{entry.request.target.media_type} {entry.request.target.id}</dd><dt>Source</dt><dd>{entry.request.source}</dd><dt>Destination</dt><dd>{entry.request.destination}</dd><dt>Transfer mode</dt><dd>{entry.request.mode}</dd><dt>Operation ID</dt><dd>{entry.operationId}</dd><dt>Status</dt><dd>{op?.status??'Not yet verified'}{op?.error_code?` (${op.error_code})`:''}{view?.fresh?'':' — refresh required'}</dd></dl>
          {#if view?.error}<p role="alert">{view.error}</p>{/if}
          {#if diagnostic}<p role={op?.recovery_required?'alert':'status'}>{diagnostic}</p>{/if}
          {#if entry.executionUncertain && op?.status!=='complete'}<p role="status">A direct execution was authorized for this operation. Its request outcome remains conservatively retained until completion. Use fresh status and explicit same-operation recovery; cancellation cannot clear this uncertainty.</p>{/if}
          {#if entry.admissionUncertain}<p role="status">An earlier queue admission has no confirmed response. Current command observations do not identify that original batch. Cancelling a known command does not revoke a request still in flight.</p>{/if}
          {#if op?.status==='complete' && (entry.executionUncertain || entry.admissionUncertain)}<p>The actual operation is complete and fences further effects for this UUID. Earlier HTTP request provenance can remain unknown; you may remove this completed receipt once no command remains active.</p>{/if}
          {#if !view?.fresh || !view.ownershipFresh}<p>Check status before authorizing this operation. Saved receipts never restore action freshness.</p>{:else if view.activeCommand}<p>Current command: {view.activeCommand.status}. Execution and recovery remain under that command’s authority.</p>{:else if !action && op?.status!=='complete'}<p>No direct execution or recovery action is currently available.</p>{/if}
          <div class="actions">
            {#if !entry.authorized && canForget(entry)}<button disabled={busy} onclick={()=>edit(entry)}>Edit unexecuted preview</button>{/if}
            {#if action}<button class="primary" disabled={mutationBlocked} onclick={()=>execute(entry)}>{action==='execute' && !entry.authorized?'Execute import':'Resume import'}</button>{/if}
            {#if entry.admissionUncertain && queueEligible(entry)}<button disabled={mutationBlocked} onclick={()=>queue([entry])}>Authorize queue again</button>{/if}
            {#if canForget(entry)}<button disabled={busy} onclick={()=>forget(entry)}>{op?.status==='complete'?'Prepare another import':entry.authorized?'Remove cancelled preview':'Remove unexecuted preview'}</button>{/if}
          </div>
          {#if entry.admissionUncertain && action==='resume'}<p>Resume explicitly continues this same marked operation. Its retained failure prevents a late queue admission from authorizing another attempt; the older HTTP request remains recorded as uncertain.</p>{/if}
          {#if !entry.authorized}<p>No transfer has been authorized from this confirmation. Execute import or Queue selected imports authorizes the captured {entry.request.mode} operation.</p>{/if}
          {#if entry.commands.length>=MANUAL_IMPORT_LIMIT}<p role="status">The retained command-reference limit has been reached. No new queue authorization is available for this operation; preserve its receipt and use current operation status for any explicit recovery.</p>{/if}
          {#if entry.commands.length}
            <h4>Known commands</h4><ul class="command-rows">
              {#each entry.commands as known (known.id)}
                {@const saved=commands[known.id]}{@const command=saved?.command}
                <li><p>Command <code>{known.id}</code><br/>Batch <code>{known.batchId}</code></p>
                  <p>{command?.status??(saved?.fresh?'History unavailable':'History not yet checked')}{saved?.fresh?'':' — refresh required'}{command?` · attempts ${command.attempts}/3`:''}{command?.error_code?` · ${command.error_code}`:''}</p>
                  {#if command?.status==='retry_wait'}<p>Next attempt: {new Date(command.next_attempt_at*1000).toLocaleString()}</p>{/if}
                  {#if saved?.error}<p role="status">{saved.error}</p>{/if}
                  {#if entry.cancelUncertainIds.includes(known.id)}<p role="status">Cancellation outcome is unknown for this command. Check status; cancellation was not retried.</p>{/if}
                  {#if command && cancelEligible(entry,command)}<button disabled={mutationBlocked} onclick={()=>cancel(entry,command)}>Cancel queued command</button>{/if}
                  {#if command?.status==='failed' && op?.status==='complete'}<p>This command failed historically; the actual operation subsequently completed.</p>{/if}
                </li>
              {/each}
            </ul>
          {/if}
        </article></li>
      {/each}
    </ol>
  {/if}
  {#if busy}<p role="status">Saving import authorization or checking its immediate outcome…</p>{/if}
  {#if checking}<p role="status">Checking import operations and command history…</p>{/if}
  {#if relatedWrite!=='idle'}<p role="status">Another library write must settle before new import authorization. Status checks remain available.</p>{/if}
  {#if readError}<p role="alert" class="error">{readError}</p>{/if}
  {#if notice}<p role="status">{notice}</p>{/if}{#if error}<p role="alert" class="error">{error}</p>{/if}
  {#if storageError}<p role="alert" class="error">{storageError}</p><div class="actions"><button onclick={exportReceipt}>Show retained recovery receipt</button>{#if !storageBlocked}<button disabled={busy} onclick={()=>save(entries)}>Retry saving receipt</button>{/if}</div>{/if}
  {#if exportOpen}<div class="receipt-export"><h3>Recovery receipt</h3><p>This contains local file paths and retained operation identities. Keep it private. Export contains only the validated identities retained in this tab; unreadable or newer data from another tab is not included. Export does not execute, cancel or reconcile anything; a full receipt is bounded to {MANUAL_IMPORT_RECEIPT_BYTES/1024/1024} MiB. If the full receipt is too large, the export is explicitly an identity-only recovery list.</p><label>Retained import receipt<textarea readonly value={exportText} rows="8"></textarea></label><div class="actions"><button onclick={downloadReceipt}>Download recovery receipt</button><button onclick={()=>exportOpen=false}>Close receipt</button></div></div>{/if}
</section>

<style>
  .import-panel { min-width:0; }
  form, .batch-review, .target-notice, .receipt-export { display:grid;gap:.75rem;margin-block:1rem; }
  label { display:grid;gap:.35rem; }
  textarea { width:100%;min-width:0;box-sizing:border-box;font:inherit; }
  .actions { display:flex;flex-wrap:wrap;gap:.5rem; }
  .import-rows { padding:0;list-style:none;display:grid;gap:1rem; }
  article { border:1px solid var(--border,#606675);border-radius:.4rem;padding:1rem;min-width:0; }
  dl { display:grid;grid-template-columns:minmax(6rem,auto) minmax(0,1fr);gap:.4rem 1rem; }
  dd { margin:0;white-space:pre-wrap;overflow-wrap:anywhere; }
  dt { font-weight:600; }
  code { overflow-wrap:anywhere; }
  .select-row { display:flex;align-items:center;gap:.5rem; }
  .command-rows { padding-left:1.2rem; }
  .command-rows li { margin-block:.75rem; }
  @media(max-width:600px) { dl{grid-template-columns:1fr;}dd{margin-bottom:.5rem;}article{padding:.75rem;} }
</style>
