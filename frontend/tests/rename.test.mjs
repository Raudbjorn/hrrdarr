import test from 'node:test';
import assert from 'node:assert/strict';
import {createRenameCommand,getTvRenamePreview,getFolderRenamePreview,getRenameCommand,cancelRenameCommand} from '../src/lib/api.ts';
import {runRename,selectedChangeIds,changeIds,summarize,isTerminal} from '../src/lib/rename-state.ts';

const id='11111111-2222-4333-8444-555555555555';
const command=(status,extra={})=>({id,name:'rename_files',target:{media_type:'tv',series_ids:[1],movie_ids:[],file_ids:[4]},expected_naming_revision:2,priority:'normal',status,attempts:1,next_attempt_at:0,created_at:0,started_at:0,completed_at:null,error_code:null,cancel_requested:false,items_renamed:0,items_unchanged:0,items_failed:0,bounded:false,...extra});
const withFetch=async(handler,run)=>{const original=globalThis.fetch,calls=[];globalThis.fetch=async(path,options)=>{calls.push([path,options?.method,options?.body]);return handler(path,options,calls);};try{return await run(calls);}finally{globalThis.fetch=original;}};

test('TV preview and folder preview build exact typed queries and reject unsafe selections before fetch',async()=>{
 await withFetch(()=>Response.json({}),async calls=>{
  await getTvRenamePreview(7);await getTvRenamePreview(7,0);await getFolderRenamePreview('tv',[3,1]);await getFolderRenamePreview('movies',[2]);
  assert.deepEqual(calls.map(c=>c[0]),['/api/v1/tv/rename-preview?series_id=7','/api/v1/tv/rename-preview?series_id=7&season=0','/api/v1/tv/rename-folder-preview?series_ids=1%2C3','/api/v1/movies/rename-folder-preview?movie_ids=2']);
  for(const bad of [()=>getTvRenamePreview(0),()=>getTvRenamePreview(1.5),()=>getTvRenamePreview(1,-1),()=>getFolderRenamePreview('tv',[]),()=>getFolderRenamePreview('tv',[1,1]),()=>getFolderRenamePreview('movies',Array.from({length:51},(_,i)=>i+1))])assert.equal((await bad()).ok,false);
  assert.equal(calls.length,4);
 });
});
test('rename command POST validates domain-typed targets and never retries',async()=>{
 await withFetch(()=>Response.json(command('queued'),{status:202}),async calls=>{
  const ok={name:'rename_files',target:{media_type:'tv',series_ids:[1],movie_ids:[],file_ids:[4,5]},expected_naming_revision:2,priority:'normal'};
  assert.equal((await createRenameCommand(ok)).ok,true);
  assert.deepEqual(JSON.parse(calls[0][2]),ok);assert.equal(calls[0][1],'POST');
  const bad=[
   {...ok,target:{...ok.target,series_ids:[1,2]}},
   {...ok,target:{...ok.target,file_ids:[]}},
   {...ok,target:{...ok.target,file_ids:[4,4]}},
   {...ok,target:{...ok.target,movie_ids:[1]}},
   {name:'rename_series',target:{media_type:'tv',series_ids:[1],movie_ids:[],file_ids:[1]},priority:'normal'},
   {name:'rename_movie',target:{media_type:'movies',series_ids:[],movie_ids:[],file_ids:[]},priority:'normal'},
  ];
  for(const input of bad)assert.equal((await createRenameCommand(input)).ok,false,JSON.stringify(input));
  assert.equal(calls.length,1);
  assert.equal((await getRenameCommand('nope')).ok,false);assert.equal((await cancelRenameCommand('nope')).ok,false);
 });
});
test('runRename polls to a terminal state, reports updates and stops when no longer alive',async()=>{
 const sequence=['running','running','succeeded'];let reads=0;
 await withFetch(path=>path.endsWith('/commands')?Response.json(command('queued'),{status:202}):Response.json({...command(sequence[reads++]),items:[]}),async()=>{
  const seen=[];const out=await runRename({name:'rename_series',target:{media_type:'tv',series_ids:[1],movie_ids:[],file_ids:[]},priority:'normal'},c=>seen.push(c.status),()=>true,async()=>{});
  assert.equal(out.ok,true);assert.deepEqual(seen,['queued','running','running','succeeded']);
 });
 await withFetch(path=>path.endsWith('/commands')?Response.json(command('queued'),{status:202}):Response.json({...command('running'),items:[]}),async calls=>{
  let alive=true;const out=await runRename({name:'rename_series',target:{media_type:'tv',series_ids:[1],movie_ids:[],file_ids:[]},priority:'normal'},()=>{},()=>alive,async()=>{alive=false;});
  assert.equal(out.ok,false);assert.equal(out.commandId,id);assert.equal(calls.filter(c=>c[1]==='POST').length,1);
 });
 await withFetch(path=>path.endsWith('/commands')?Response.json(command('queued'),{status:202}):Response.json({error:{code:'x',message:'poll failed'}},{status:500}),async()=>{
  const out=await runRename({name:'rename_series',target:{media_type:'tv',series_ids:[1],movie_ids:[],file_ids:[]},priority:'normal'},()=>{},()=>true,async()=>{});
  assert.deepEqual([out.ok,out.commandId],[false,id]);
 });
});
test('selection helpers only select renamable files and summaries distinguish outcomes',()=>{
 const items=[{id:1,status:'change'},{id:2,status:'blocked'},{id:3,status:'change'},{id:4,status:'unavailable'}];
 assert.deepEqual(changeIds(items),[1,3]);
 assert.deepEqual(selectedChangeIds(items,new Set([1,2,4,99])),[1]);
 assert.equal(isTerminal(command('running')),false);assert.equal(isTerminal(command('failed')),true);
 assert.match(summarize({...command('succeeded',{items_renamed:2,items_unchanged:1}),items:[]}),/2 renamed, 1 unchanged, 0 failed/);
 assert.match(summarize({...command('failed',{error_code:'naming_revision_changed'}),items:[]}),/naming_revision_changed/);
 assert.match(summarize({...command('succeeded',{bounded:true}),items:[]}),/run the command again/);
});
