<script lang="ts">
  import {onMount,tick} from 'svelte';
  import type {BackupFile,BackupSettings,BackupCommand,PendingRestore} from './api.generated';
  import {listBackups,deleteBackup,restoreBackup,uploadRestore,getPendingRestore,cancelPendingRestore,getBackupSettings,updateBackupSettings,createBackupCommand,listBackupCommands,getBackupCommand,type SettingsWriteState} from './api';
  import {backupSettingsUpdate,describeBackupCommand,describePending,restoreErrorMessage,checkUploadFile,formatBytes,formatTime,typeLabel,pageSummary,downloadPath,BACKUP_PAGE_SIZE,MIN_INTERVAL_DAYS,MAX_INTERVAL_DAYS,MIN_RETENTION_DAYS,MAX_RETENTION_DAYS,type BackupSettingsDraft} from './backup-view';

  const POLL_MS=500,POLL_LIMIT=240;
  let backups=$state<BackupFile[]>([]),total=$state(0),offset=$state(0),listing=$state(false),listError=$state('');
  let command=$state<BackupCommand|null>(null),running=$state(false),commandError=$state('');
  let pending=$state<PendingRestore|null>(null),pendingError=$state('');
  type Confirm={kind:'delete';backup:BackupFile}|{kind:'restore';backup:BackupFile}|{kind:'upload';file:File};
  let confirm=$state<Confirm|null>(null);
  let acting=$state(false),actionError=$state(''),notice=$state('');
  let file=$state<File|null>(null),fileInput=$state<HTMLInputElement|null>(null);
  let saved=$state<BackupSettings|null>(null),draft=$state<BackupSettingsDraft>({folder:'',interval_days:7,retention_days:28});
  let write=$state<SettingsWriteState>('idle'),settingsLoading=$state(false),dirty=$state(false),stale=$state(false),settingsError=$state(''),settingsNotice=$state('');
  let confirmButton=$state<HTMLButtonElement|null>(null);
  let alive=true,listEpoch=0,commandEpoch=0;

  async function loadList(nextOffset=offset){
    listing=true;listError='';
    const version=++listEpoch,result=await listBackups(nextOffset,BACKUP_PAGE_SIZE);
    if(!alive||version!==listEpoch)return;
    listing=false;
    if(!result.ok){listError=`Backups could not be listed: ${result.error}`;return;}
    if(result.data.items.length===0&&result.data.total>0&&nextOffset>0){void loadList(Math.max(0,nextOffset-BACKUP_PAGE_SIZE));return;}
    backups=result.data.items;total=result.data.total;offset=nextOffset;
  }
  async function loadPending(){
    const result=await getPendingRestore();
    if(!alive)return;
    if(!result.ok){pendingError=`Pending restore status could not be read: ${result.error}`;return;}
    pendingError='';pending=result.data.pending;
  }
  async function loadLatestCommand(){
    const result=await listBackupCommands(1);
    if(alive&&result.ok&&!running)command=result.data.items[0]??null;
  }
  function fill(settings:BackupSettings){saved=settings;draft={folder:settings.folder??'',interval_days:settings.interval_days,retention_days:settings.retention_days};dirty=false;stale=false;write='idle';}
  async function loadSettings(){
    settingsLoading=true;settingsError='';settingsNotice='';
    const result=await getBackupSettings();
    if(!alive)return;
    settingsLoading=false;
    if(!result.ok){settingsError=result.error;return;}
    fill(result.data);
  }
  async function saveSettings(){
    if(!saved||write!=='idle'||stale)return;
    const input=backupSettingsUpdate(saved.revision,draft);
    if(!input.ok){settingsError=input.error;return;}
    write='busy';settingsError='';settingsNotice='';
    const result=await updateBackupSettings(input.data);
    if(!alive)return;
    if(!result.ok){write=result.status===undefined||result.status>=500?'uncertain':'idle';stale=result.status===409&&result.code==='backup_settings_conflict';settingsError=result.error;return;}
    fill(result.data);settingsNotice=`Backup settings saved. Backups are written to ${result.data.effective_folder??'the default folder'}.`;
  }
  async function backupNow(){
    if(running)return;
    running=true;commandError='';notice='';
    const token=++commandEpoch,created=await createBackupCommand();
    if(!alive||token!==commandEpoch)return;
    if(!created.ok){running=false;commandError=`Backup could not be started: ${created.error}`;return;}
    command=created.data;
    for(let attempt=0;attempt<POLL_LIMIT&&command&&(command.status==='queued'||command.status==='running');attempt++){
      await new Promise(resolve=>setTimeout(resolve,POLL_MS));
      if(!alive||token!==commandEpoch)return;
      const next=await getBackupCommand(created.data.id);
      if(!alive||token!==commandEpoch)return;
      if(!next.ok){commandError=`Backup status could not be read: ${next.error}`;break;}
      command=next.data;
    }
    running=false;
    if(command?.status==='succeeded')await loadList(0);
  }
  async function ask(next:Confirm){actionError='';notice='';confirm=next;await tick();confirmButton?.focus();}
  function closeConfirm(){confirm=null;actionError='';}
  async function confirmAction(){
    const current=confirm;
    if(!current||acting)return;
    acting=true;actionError='';notice='';
    if(current.kind==='delete'){
      const result=await deleteBackup(current.backup.id);
      if(!alive)return;
      acting=false;
      if(!result.ok){actionError=`Backup was not deleted: ${result.error}`;return;}
      confirm=null;notice=`Deleted ${current.backup.name}.`;await loadList();return;
    }
    const result=current.kind==='restore'?await restoreBackup(current.backup.id):await uploadRestore(current.file);
    if(!alive)return;
    acting=false;
    if(!result.ok){actionError=restoreErrorMessage(result.code,`Restore was not staged: ${result.error}`);if(result.code==='restore_already_pending')await loadPending();return;}
    confirm=null;pending=result.data;file=null;if(fileInput)fileInput.value='';
    notice='Restore validated and staged. Restart hrrdarr to apply it.';
  }
  async function cancelRestore(){
    if(acting)return;
    acting=true;pendingError='';notice='';
    const result=await cancelPendingRestore();
    if(!alive)return;
    acting=false;
    if(!result.ok&&result.code!=='restore_not_pending'){pendingError=`Pending restore was not cancelled: ${result.error}`;return;}
    pending=null;notice='Pending restore cancelled. The current database stays in use.';
  }
  function chooseUpload(){
    const check=checkUploadFile(file);
    if(!check.ok){actionError=check.error;return;}
    if(file)ask({kind:'upload',file});
  }
  onMount(()=>{void loadList(0);void loadPending();void loadSettings();void loadLatestCommand();return()=>{alive=false;++listEpoch;++commandEpoch;};});
</script>
<svelte:window onbeforeunload={event=>{if(dirty||write!=='idle'){event.preventDefault();event.returnValue='';}}}/>
<section aria-label="Backup" class="backup">
  <h2>Backup</h2>
  <p>Backups contain the database only: library, settings, history and provider configuration for TV and movies. Media files are never included. Stored credentials stay encrypted; the key is not in the backup, so a restored instance needs the same <code>HRRDARR_PROVIDER_KEY</code>.</p>
  {#if notice}<p role="status">{notice}</p>{/if}

  <section aria-label="Pending restore" aria-live="polite">
    {#if pending}
      <div class="pending">
        <h3>Restore pending — applies on restart</h3>
        <p>{describePending(pending)}</p>
        <p>Changes made before the restart are not kept in the restored database.</p>
        <button disabled={acting} onclick={cancelRestore}>Cancel pending restore</button>
      </div>
    {/if}
    {#if pendingError}<p role="alert">{pendingError}</p>{/if}
  </section>

  <section aria-label="Create backup">
    <h3>Create</h3>
    <button class="primary" disabled={running} onclick={backupNow}>Back up now</button>
    {#if running}<p role="status">Backing up the database…</p>{/if}
    {#if commandError}<p role="alert">{commandError}</p>{/if}
    {#if command}<p role="status" aria-label="Latest backup">{describeBackupCommand(command)}</p>{/if}
  </section>

  <section aria-label="Backups list">
    <h3>Backups</h3>
    <button disabled={listing} onclick={()=>loadList()}>Refresh backups</button>
    {#if listError}<p role="alert">{listError}</p>{/if}
    {#if listing&&backups.length===0}<p role="status">Loading backups…</p>{/if}
    <div class="scroll"><table aria-label="Backups" aria-busy={listing}>
      <thead><tr><th scope="col">Name</th><th scope="col">Type</th><th scope="col">Created</th><th scope="col">Size</th><th scope="col">Actions</th></tr></thead>
      <tbody>
        {#each backups as backup (backup.id)}
          <tr>
            <td class="name">{backup.name}</td><td>{typeLabel(backup.backup_type)}</td><td>{formatTime(backup.created_at)}</td><td>{formatBytes(backup.size_bytes)}</td>
            <td class="actions">
              {#if downloadPath(backup)}<a href={downloadPath(backup)} download={backup.name} aria-label={`Download ${backup.name}`}>Download</a>{/if}
              <button disabled={acting||!!pending} aria-label={`Restore ${backup.name}`} onclick={()=>ask({kind:'restore',backup})}>Restore</button>
              <button disabled={acting} aria-label={`Delete ${backup.name}`} onclick={()=>ask({kind:'delete',backup})}>Delete</button>
            </td>
          </tr>
        {/each}
      </tbody>
    </table></div>
    {#if !listing&&!listError}<p>{pageSummary(offset,backups.length,total)}</p>{/if}
    <nav aria-label="Backup pages">
      <button disabled={listing||offset===0} onclick={()=>loadList(Math.max(0,offset-BACKUP_PAGE_SIZE))}>Previous page</button>
      <button disabled={listing||offset+backups.length>=total} onclick={()=>loadList(offset+BACKUP_PAGE_SIZE)}>Next page</button>
    </nav>
  </section>

  <section aria-label="Restore from file">
    <h3>Restore from file</h3>
    <label>Backup archive<input bind:this={fileInput} type="file" accept=".zip,application/zip" disabled={acting||!!pending} onchange={event=>{file=(event.currentTarget as HTMLInputElement).files?.[0]??null;actionError='';}}/></label>
    <button disabled={acting||!!pending||!file} onclick={chooseUpload}>Restore from file</button>
    {#if pending}<p>Cancel the pending restore before staging another.</p>{/if}
  </section>

  {#if confirm}
    <div role="alertdialog" aria-modal="false" aria-labelledby="backup-confirm-title" aria-describedby="backup-confirm-body" class="confirm" tabindex="-1" onkeydown={event=>{if(event.key==='Escape'&&!acting)closeConfirm();}}>
      {#if confirm.kind==='delete'}
        <h3 id="backup-confirm-title">Delete backup {confirm.backup.name}?</h3>
        <p id="backup-confirm-body">This permanently removes this archive from the backup folder. Other backups and the live database are not affected.</p>
        <button bind:this={confirmButton} class="danger" disabled={acting} onclick={confirmAction}>Confirm delete</button>
      {:else}
        <h3 id="backup-confirm-title">Restore {confirm.kind==='restore'?`backup ${confirm.backup.name}`:`from ${confirm.file.name}`}?</h3>
        <p id="backup-confirm-body">The archive is validated now (checksums, schema version and database integrity) and staged. It replaces the current database the next time hrrdarr restarts; nothing changes before then. Everything recorded after this backup was taken is lost on restart, but the current database is kept beside it as a recoverable original. Media files are not changed.</p>
        <button bind:this={confirmButton} class="danger" disabled={acting} onclick={confirmAction}>Confirm restore</button>
      {/if}
      <button disabled={acting} onclick={closeConfirm}>Cancel</button>
      {#if acting}<p role="status">{confirm.kind==='delete'?'Deleting backup…':'Validating and staging the restore…'}</p>{/if}
    </div>
  {/if}
  {#if actionError}<p role="alert">{actionError}</p>{/if}

  <section aria-label="Backup settings" aria-busy={settingsLoading||write==='busy'}>
    <h3>Settings</h3>
    {#if settingsLoading}<p role="status">Loading backup settings…</p>{/if}
    {#if settingsError}<p role="alert">{settingsError}</p>{/if}
    {#if settingsNotice}<p role="status">{settingsNotice}</p>{/if}
    {#if write==='uncertain'||stale}<p role="alert">{write==='uncertain'?'Write outcome unknown.':'Backup settings changed in another request.'} Your draft is retained. Reload saved settings before another write.</p>{/if}
    <button disabled={settingsLoading||write==='busy'} onclick={()=>{if(!dirty||window.confirm('Replace this draft with the saved backup settings?'))void loadSettings();}}>Reload backup settings</button>
    <form onsubmit={event=>{event.preventDefault();void saveSettings();}} oninput={()=>dirty=true} onchange={()=>dirty=true}>
      <fieldset disabled={!saved||write!=='idle'||stale}>
        <legend>Backup policy</legend>
        <label>Backup folder<input bind:value={draft.folder} placeholder={saved?.effective_folder??'Default folder'} autocomplete="off" spellcheck="false" aria-describedby="backup-folder-hint"/></label>
        <p id="backup-folder-hint">An absolute folder outside every root folder, library folder and recycle bin. Leave empty for the default{saved?.effective_folder&&!saved.folder?` (${saved.effective_folder})`:''}. It is created if missing and must be writable.</p>
        <label>Scheduled backup interval (days)<input type="number" required min={MIN_INTERVAL_DAYS} max={MAX_INTERVAL_DAYS} step="1" bind:value={draft.interval_days}/></label>
        <label>Keep scheduled and update backups for (days)<input type="number" required min={MIN_RETENTION_DAYS} max={MAX_RETENTION_DAYS} step="1" bind:value={draft.retention_days} aria-describedby="backup-retention-hint"/></label>
        <p id="backup-retention-hint">Older scheduled and update backups are removed only after a newer backup of the same type succeeds. Manual backups are kept until you delete them.</p>
        {#if saved?.next_backup_at}<p>Next scheduled backup: {formatTime(saved.next_backup_at)}.</p>{/if}
        <button>Save backup settings</button>
      </fieldset>
    </form>
  </section>
</section>
<style>
  .backup section{margin-top:1.5rem}table{border-collapse:collapse;width:100%;margin-top:.5rem}th,td{text-align:left;padding:.35rem .5rem;border-bottom:1px solid #cbd5de;white-space:nowrap}td.name{white-space:normal;overflow-wrap:anywhere;min-width:12rem}.scroll{overflow-x:auto;max-width:100%}.actions{display:flex;gap:.5rem;align-items:center}
  fieldset{display:grid;gap:.8rem;margin-top:1rem;border:1px solid #cbd5de;padding:1rem;min-width:0}fieldset p{margin:0;font-size:.9rem}input{width:100%;max-width:40rem;box-sizing:border-box}
  [role=alert]{border-left:3px solid #a12637;padding-left:.75rem;color:#a12637}button,a{min-height:2.5rem}.pending{border:2px solid #8a5a00;padding:.75rem 1rem;background:#fff8e6}
  .confirm{border:2px solid #a12637;padding:1rem;margin-top:1rem;background:#fff}.danger{background:#a12637;color:#fff}.primary{font-weight:600}
</style>
