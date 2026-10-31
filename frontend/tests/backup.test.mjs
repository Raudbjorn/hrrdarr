import test from 'node:test';
import assert from 'node:assert/strict';
import {backupSettingsUpdate,validBackupId,formatBytes,formatTime,typeLabel,describeBackupCommand,restoreErrorMessage,checkUploadFile,describePending,pageSummary,downloadPath,MAX_UPLOAD_BYTES,MAX_INTERVAL_DAYS,MAX_RETENTION_DAYS} from '../src/lib/backup-view.ts';
import {listBackups,deleteBackup,restoreBackup,uploadRestore,getBackup,cancelPendingRestore,createBackupCommand,updateBackupSettings} from '../src/lib/api.ts';

// Client stubs check request shape and validation only; tests/backup_restore.rs owns server truth.
async function withFetch(stub,run){const original=globalThis.fetch;globalThis.fetch=stub;try{await run();}finally{globalThis.fetch=original;}}
const json=(value,status=200)=>new Response(JSON.stringify(value),{status});
const ID='0123456789abcdef0123456789abcdef';
const command=over=>({id:'e3442408-a805-4365-966c-02a3c5bf60f4',name:'backup',backup_type:'manual',status:'succeeded',attempts:1,created_at:1,started_at:1,completed_at:2,error_code:null,backup_id:ID,backup_name:'hrrdarr_backup_20261019T201919Z_a1b02eeb.zip',size_bytes:2879328,pruned:0,...over});

test('settings draft mirrors server validation; blank folder selects the default',()=>{
  assert.deepEqual(backupSettingsUpdate(3,{folder:'  ',interval_days:7,retention_days:28}),{ok:true,data:{revision:3,folder:null,interval_days:7,retention_days:28}});
  assert.deepEqual(backupSettingsUpdate(3,{folder:'/srv//backups/',interval_days:1,retention_days:MAX_RETENTION_DAYS}).data.folder,'/srv/backups');
  for(const folder of ['relative','/a/../b','/a/./b','C:\\b','/a\u0007','/','//','/'+'x'.repeat(4096)])assert.equal(backupSettingsUpdate(1,{folder,interval_days:1,retention_days:1}).ok,false,folder);
  for(const [i,r] of [[0,1],[MAX_INTERVAL_DAYS+1,1],[1.5,1],[1,0],[1,MAX_RETENTION_DAYS+1],[undefined,1],[1,undefined]])assert.equal(backupSettingsUpdate(1,{folder:'',interval_days:i,retention_days:r}).ok,false,`${i}/${r}`);
  assert.equal(backupSettingsUpdate(0,{folder:'',interval_days:1,retention_days:1}).ok,false,'revision required');
});

test('ids, sizes, times and labels are bounded and explicit',()=>{
  assert.equal(validBackupId(ID),true);
  for(const bad of ['', '../x', ID.toUpperCase(), ID.slice(1), `${ID}0`, null, 7])assert.equal(validBackupId(bad),false,String(bad));
  assert.equal(formatBytes(512),'512 B');assert.equal(formatBytes(2879328),'2.7 MiB');assert.equal(formatBytes(-1),'unknown size');
  assert.equal(formatTime(0),'1970-01-01 00:00:00 UTC');assert.equal(formatTime(null),'unknown time');
  assert.equal(typeLabel('update'),'Update (before migration)');
  assert.equal(downloadPath({id:ID,name:'n',backup_type:'manual',size_bytes:1,created_at:0}),`/api/v1/system/backups/${ID}/download`);
  assert.equal(downloadPath({id:'../../etc',name:'n',backup_type:'manual',size_bytes:1,created_at:0}),null);
  assert.equal(pageSummary(0,0,0),'No backups yet.');assert.equal(pageSummary(10,2,12),'Showing 11–12 of 12 backups.');
});

test('command and restore outcomes say what happened, including no pruning on failure',()=>{
  assert.equal(describeBackupCommand(command({pruned:2,backup_type:'scheduled'})),'Scheduled backup completed: hrrdarr_backup_20261019T201919Z_a1b02eeb.zip (2.7 MiB). Removed 2 expired backups of the same type.');
  assert.match(describeBackupCommand(command({status:'failed',error_code:'backup_folder_unsafe',backup_id:null})),/overlaps a library folder.*not pruned/);
  assert.equal(describeBackupCommand(command({status:'queued'})),'Manual backup is queued.');
  assert.match(restoreErrorMessage('backup_schema_unsupported','x'),/newer hrrdarr version/);
  assert.equal(restoreErrorMessage('unknown_code','fallback'),'fallback');
  assert.match(describePending({source:'upload',backup_id:null,backup_name:null,backup_type:'manual',backup_created_at:0,schema_version:70,application_version:'0.1.0',staged_at:1,database_sha256:'a'.repeat(64),database_bytes:1,restart_required:true}),/uploaded archive.*next restarts.*recoverable original/);
  assert.equal(checkUploadFile(null).ok,false);
  assert.equal(checkUploadFile({name:'b.db',size:5}).ok,false);
  assert.equal(checkUploadFile({name:'b.zip',size:0}).ok,false);
  assert.equal(checkUploadFile({name:'b.zip',size:MAX_UPLOAD_BYTES+1}).ok,false);
  assert.equal(checkUploadFile({name:'B.ZIP',size:10}).ok,true);
});

test('client sends ids, never paths, and uploads the raw archive body',async()=>{
  const seen=[];
  await withFetch(async(url,init={})=>{seen.push({url:String(url),method:init.method??'GET',type:init.headers?.['content-type'],body:init.body});
    if(init.method==='DELETE')return new Response(null,{status:204});
    if(String(url).endsWith('/restore/upload'))return json({source:'upload',backup_id:null,backup_name:null,backup_type:'manual',backup_created_at:0,schema_version:70,application_version:'0.1.0',staged_at:1,database_sha256:'a'.repeat(64),database_bytes:1,restart_required:true},202);
    return json({items:[],total:0,limit:10,offset:0});},async()=>{
    assert.equal((await listBackups(20,10)).ok,true);
    assert.equal((await deleteBackup(ID)).ok,true);
    await restoreBackup(ID);
    await createBackupCommand();
    await updateBackupSettings({revision:1,folder:null,interval_days:7,retention_days:28});
    const blob=new Blob([new Uint8Array([80,75,5,6])]);
    const staged=await uploadRestore(blob);
    assert.equal(staged.ok,true);assert.equal(staged.data.restart_required,true);
    assert.equal((await cancelPendingRestore()).ok,true);
    for(const bad of ['../../library.db','%2e%2e',ID.toUpperCase()]){
      assert.equal((await getBackup(bad)).ok,false);assert.equal((await deleteBackup(bad)).ok,false);assert.equal((await restoreBackup(bad)).ok,false);
    }
    assert.equal((await uploadRestore(new Blob([]))).ok,false,'empty upload refused client-side');
  });
  assert.deepEqual(seen.map(s=>`${s.method} ${s.url}`),[
    'GET /api/v1/system/backups?limit=10&offset=20',
    `DELETE /api/v1/system/backups/${ID}`,
    `POST /api/v1/system/backups/${ID}/restore`,
    'POST /api/v1/system/backup-commands',
    'PUT /api/v1/system/backup-settings',
    'POST /api/v1/system/restore/upload',
    'DELETE /api/v1/system/restore',
  ]);
  const upload=seen[5];
  assert.equal(upload.type,'application/zip');
  assert.ok(upload.body instanceof Blob,'raw archive body, not JSON or multipart');
  assert.equal(seen[3].body,'{}');
});
