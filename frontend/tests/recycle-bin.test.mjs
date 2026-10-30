import test from 'node:test';
import assert from 'node:assert/strict';
import {deletionOwnerMatches,recycleBinUpdate,deletionPlan,describeDeletionFailure,describeDeletions,describeCleanup,MAX_CLEANUP_DAYS,MAX_DELETE_FILES} from '../src/lib/recycle-bin-draft.ts';
import {getRecycleBin,updateRecycleBin,deleteMediaFile,deleteMediaFiles,createRecycleBinCleanup,listRecycleBinCleanups} from '../src/lib/api.ts';
import {describeEvent} from '../src/lib/history-view.ts';

// Client stubs check request shape and response validation; tests/file_deletion.rs owns server truth.
async function withFetch(stub,run){const original=globalThis.fetch;globalThis.fetch=stub;try{await run();}finally{globalThis.fetch=original;}}
const json=(value,status=200)=>new Response(JSON.stringify(value),{status});
const settings=over=>({media_type:'tv',revision:3,path:'/data/recycle',cleanup_days:7,next_cleanup_at:null,...over});

test('deletion confirmations remain bound to their domain-scoped owner',()=>{
 assert.equal(deletionOwnerMatches({domain:'tv',id:7},{domain:'tv',id:7}),true);
 assert.equal(deletionOwnerMatches({domain:'tv',id:7},{domain:'movies',id:7}),false);
 assert.equal(deletionOwnerMatches({domain:'tv',id:7},null),false);
});
test('settings draft mirrors server validation and maps empty path to null',()=>{
  assert.deepEqual(recycleBinUpdate(2,{path:'  ',cleanup_days:0}),{ok:true,data:{revision:2,path:null,cleanup_days:0}});
  assert.deepEqual(recycleBinUpdate(2,{path:'/r',cleanup_days:MAX_CLEANUP_DAYS}),{ok:true,data:{revision:2,path:'/r',cleanup_days:MAX_CLEANUP_DAYS}});
  for(const days of [undefined,-1,1.5,MAX_CLEANUP_DAYS+1])assert.equal(recycleBinUpdate(1,{path:'/r',cleanup_days:days}).ok,false,String(days));
  for(const path of ['relative','/a/../b','/a/./b','C:\\bin','/a\u0007','/','//','/'+'x'.repeat(4096)])assert.equal(recycleBinUpdate(1,{path,cleanup_days:1}).ok,false,path);
});

test('deletion confirmation states recycle versus permanent exactly',()=>{
  const recycle=deletionPlan(settings(),[7]);
  assert.equal(recycle.mode,'recycle');assert.equal(recycle.confirm,'Move file 7 to recycle bin');
  assert.match(recycle.detail,/moved to \/data\/recycle/);assert.match(recycle.detail,/after 7 days/);
  assert.match(deletionPlan(settings({cleanup_days:0}),[1,2]).detail,/cleanup is disabled/);
  assert.equal(deletionPlan(settings({cleanup_days:1}),[1,2]).title,'Move 2 files to the recycle bin?');
  const permanent=deletionPlan(settings({path:null}),[1,2,3]);
  assert.equal(permanent.mode,'permanent');assert.equal(permanent.confirm,'Permanently delete 3 files');
  assert.match(permanent.detail,/cannot be restored/);
});

test('failure descriptions say nothing was deleted for refusals and flag uncertainty otherwise',()=>{
  for(const code of ['recycle_bin_configured','recycle_bin_not_configured','recycle_bin_unavailable','library_root_missing','library_busy','file_reserved_by_import']){
    const d=describeDeletionFailure({error:'x',code,status:409});assert.match(d.message,/Nothing was deleted|cannot be deleted/,code);assert.equal(d.reload,false);
  }
  assert.equal(describeDeletionFailure({error:'x',code:'file_delete_recovery_pending',status:409}).reload,true);
  const lost=describeDeletionFailure({error:'Network failed'});assert.equal(lost.reload,true);assert.match(lost.message,/uncertain/);
  assert.equal(describeDeletionFailure({error:'Bad',status:400}).reload,false);
});

test('deletion notices report server effects, including already-missing files',()=>{
  const d=(file_id,effect,recycle_path=null)=>({id:'x',media_type:'tv',file_id,path:'/tv/a.mkv',effect,recycle_path,targets:[]});
  assert.equal(describeDeletions([d(7,'recycled','/r/S/a.mkv')]),'Moved file 7 to the recycle bin: /r/S/a.mkv.');
  assert.equal(describeDeletions([d(1,'recycled','/r/1'),d(2,'recycled','/r/2'),d(3,'already_missing')]),'Moved 2 files to the recycle bin. File 3 was already missing from disk; the library record was removed.');
  assert.equal(describeDeletions([d(4,'deleted')]),'Permanently deleted file 4.');
  assert.equal(describeDeletions([]),'No files were deleted.');
});

test('cleanup summaries cover outcomes and bounds',()=>{
  const base={id:'00000000-0000-4000-8000-000000000001',origin:'manual',status:'succeeded',error_code:null,outcome:'cleaned',files_removed:1,folders_removed:2,entries_skipped:1,entries_failed:0,bounded:false};
  assert.equal(describeCleanup(base),'Manual cleanup removed 1 file and 2 empty folders; kept 1 symlink or special entry.');
  assert.match(describeCleanup({...base,origin:'automatic',bounded:true,entries_failed:2}),/^Scheduled .*2 could not be removed; stopped at its run limit/);
  assert.match(describeCleanup({...base,outcome:'not_configured'}),/no configured recycle bin/);
  assert.match(describeCleanup({...base,status:'failed',outcome:null,error_code:'recycle_bin_unavailable'}),/failed \(recycle_bin_unavailable\)/);
});

test('API wrappers send explicit modes and validate domain-qualified responses',async()=>{
  const seen=[];
  await withFetch(async(path,options)=>{
    seen.push([path,options.method??'GET',options.body??null]);
    if(path==='/api/v1/tv/recycle-bin')return json(settings());
    if(path==='/api/v1/movies/files/9')return json({id:'d',media_type:'movies',file_id:9,path:'/m/a.mkv',effect:'recycled',recycle_path:'/r/a.mkv',targets:[]});
    if(path==='/api/v1/tv/files/9')return json({id:'d',media_type:'movies',file_id:9,path:'/m/a.mkv',effect:'deleted',recycle_path:null,targets:[]});
    if(path==='/api/v1/tv/files/bulk')return json({batch_id:'b',deletions:[{file_id:2,media_type:'tv'},{file_id:1,media_type:'tv'}]});
    if(path.startsWith('/api/v1/recycle-bin/cleanup-commands'))return json(options.method==='POST'?{id:'c'}:{items:[],total:0,offset:0,limit:1});
    return json({error:'unexpected'},500);
  },async()=>{
    assert.equal((await getRecycleBin('tv')).ok,true);
    assert.equal((await updateRecycleBin('tv',{revision:3,path:null,cleanup_days:7})).ok,true);
    assert.equal((await deleteMediaFile('movies',9,'recycle')).ok,true);
    assert.equal((await deleteMediaFile('tv',9,'permanent')).ok,false,'cross-domain response must be rejected');
    assert.equal((await deleteMediaFiles('tv',[2,1],'recycle')).ok,true);
    assert.equal((await createRecycleBinCleanup('movies')).ok,true);
    assert.equal((await listRecycleBinCleanups('movies',1)).ok,true);
  });
  assert.deepEqual(seen.find(([p,m])=>p==='/api/v1/movies/files/9'&&m==='DELETE')[2],'{"mode":"recycle"}');
  assert.deepEqual(seen.find(([p])=>p==='/api/v1/tv/files/bulk')[2],'{"file_ids":[2,1],"mode":"recycle"}');
  assert.equal(seen.find(([p,m])=>p==='/api/v1/tv/recycle-bin'&&m==='PUT')[2],'{"revision":3,"path":null,"cleanup_days":7}');
  assert.equal(seen.find(([p,m])=>p==='/api/v1/recycle-bin/cleanup-commands'&&m==='POST')[2],'{"target":{"media_type":"movies"},"priority":"high"}');
  assert.ok(seen.some(([p])=>p==='/api/v1/recycle-bin/cleanup-commands?media_type=movies&limit=1'));
  const before=seen.length;
  await withFetch(async(...args)=>{seen.push(args);return json({});},async()=>{
    assert.equal((await deleteMediaFiles('tv',Array.from({length:MAX_DELETE_FILES+1},(_,i)=>i+1),'recycle')).ok,false);
    assert.equal((await deleteMediaFile('tv',1,'trash')).ok,false);
    assert.equal((await deleteMediaFile('episodes',1,'recycle')).ok,false);
  });
  assert.equal(seen.length,before,'invalid requests must not reach the network');
});

test('bulk deletion accepts its 50-file bound independently of metadata page size',async()=>{
  const seen=[];
  await withFetch(async(path,options)=>{
    const body=JSON.parse(options.body);
    seen.push([path,body]);
    return json({batch_id:'b',deletions:body.file_ids.map(file_id=>({file_id,media_type:'tv'}))});
  },async()=>{
    // Deletion batches are bounded at 50 even though metadata readback pages contain 25.
    for(const count of [26,MAX_DELETE_FILES]){
      const ids=Array.from({length:count},(_,i)=>i+1);
      assert.equal((await deleteMediaFiles('tv',ids,'recycle')).ok,true);
      assert.deepEqual(seen.at(-1),['/api/v1/tv/files/bulk',{file_ids:ids,mode:'recycle'}]);
    }
    for(const ids of [[],[1,1],[0],[1.5],[Number.MAX_SAFE_INTEGER+1],Array.from({length:MAX_DELETE_FILES+1},(_,i)=>i+1)]){
      assert.equal((await deleteMediaFiles('tv',ids,'recycle')).ok,false);
    }
    assert.equal(seen.length,2,'invalid identities and oversized batches must not reach the network');
  });
});

test('history renders native deletion events with recycle path or permanent note',()=>{
  const event=over=>({origin:'native_deletion',id:'h1',deletion_id:'d1',event_type:'file_deleted',reason:'manual',target:{media_type:'episode',id:4},file:{media_type:'tv',id:4},path:'/tv/S/a.mkv',recycle_path:'/r/S/a.mkv',deleted_at:'2026-10-17T00:00:00Z',...over});
  const recycled=describeEvent(event());
  assert.equal(recycled.origin,'native_deletion');assert.equal(recycled.eventLabel,'File deleted (moved to recycle bin)');assert.deepEqual(recycled.warnings,[]);
  assert.ok(recycled.facts.some(f=>f.label==='Recycle bin path'&&f.value==='/r/S/a.mkv'&&f.recorded));
  const permanent=describeEvent(event({recycle_path:null,target:{media_type:'movie',id:4}}));
  assert.equal(permanent.eventLabel,'File deleted');assert.equal(permanent.domain,'Movies');assert.equal(permanent.warnings.length,1,'tv file on a movie target is flagged');
  // An upgrade recycle shares the deletion origin; only its reason distinguishes it.
  const upgrade=describeEvent(event({reason:'upgrade',deletion_id:'r1'}));
  assert.equal(upgrade.eventLabel,'Replaced file moved to recycle bin');
  assert.ok(upgrade.facts.some(f=>f.label==='Reason'&&f.value==='Upgrade (an import replaced this file)'));
  assert.ok(upgrade.facts.some(f=>f.label==='Recycle bin path'&&f.value==='/r/S/a.mkv'&&f.recorded));
});
