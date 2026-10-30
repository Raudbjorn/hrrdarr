import test from 'node:test';
import assert from 'node:assert/strict';
import {policyComplete,draftOf,policyPatch,draftDirty,monitorPatch,libraryMonitorPatch,toggleId,listQuery,pageCount,countsLabel,lastSyncLabel,policySummary,isActive,isTerminal,statusText,refreshSummary,PAGE_SIZE} from '../src/lib/collections-view.ts';

const base={id:1,tmdb_id:7,title:'Saga',overview:null,monitored:false,search_on_add:false,quality_profile_id:null,root_folder_id:null,root_folder_path:null,minimum_availability:null,tag_ids:[],last_info_sync:null,movie_count:3,library_movie_count:1,missing_movie_count:2,movies:[]};
const full={...base,quality_profile_id:4,root_folder_id:2,root_folder_path:'/m',minimum_availability:'released',tag_ids:[5,3],search_on_add:true};

test('policy completeness mirrors the server rule',()=>{
  assert.equal(policyComplete(base),false);
  assert.equal(policyComplete({...full,minimum_availability:null}),false);
  assert.equal(policyComplete({...full,root_folder_id:null}),false);
  assert.equal(policyComplete(full),true);
});
test('draft round trip and patch building',()=>{
  const d=draftOf(full);
  assert.deepEqual(d,{qualityProfileId:'4',rootFolderId:'2',minimumAvailability:'released',searchOnAdd:true,tagIds:[3,5]});
  assert.deepEqual(policyPatch(d),{quality_profile_id:4,root_folder_id:2,minimum_availability:'released',search_on_add:true,tag_ids:[3,5]});
  assert.deepEqual(policyPatch(draftOf(base)),{quality_profile_id:null,root_folder_id:null,minimum_availability:null,search_on_add:false,tag_ids:[]});
  assert.equal(policyPatch({...d,minimumAvailability:'bogus',qualityProfileId:'x'}).minimum_availability,null);
  assert.equal(policyPatch({...d,qualityProfileId:'x'}).quality_profile_id,null);
  assert.deepEqual(policyPatch({...d,tagIds:[5,5,3]}).tag_ids,[3,5]);
});
test('dirty detection ignores tag order and duplicates',()=>{
  const d=draftOf(full);
  assert.equal(draftDirty(d,full),false);
  assert.equal(draftDirty({...d,tagIds:[5,3,3]},full),false);
  assert.equal(draftDirty({...d,tagIds:[3]},full),true);
  assert.equal(draftDirty({...d,searchOnAdd:false},full),true);
  assert.equal(draftDirty({...d,rootFolderId:''},full),true);
});
test('one-shot patches',()=>{
  assert.deepEqual(monitorPatch(true),{monitored:true});
  assert.deepEqual(libraryMonitorPatch(false),{monitor_movies:false});
});
test('toggleId adds sorted and removes',()=>{
  assert.deepEqual(toggleId([3,1],2,true),[1,2,3]);
  assert.deepEqual(toggleId([1,2],1,false),[2]);
  assert.deepEqual(toggleId([1,2],2,true),[1,2]);
});
test('list query and paging',()=>{
  assert.deepEqual(listQuery('all','  ',0),{limit:PAGE_SIZE,offset:0});
  assert.deepEqual(listQuery('monitored',' saga ',2),{limit:PAGE_SIZE,offset:2*PAGE_SIZE,monitored:true,q:'saga'});
  assert.equal(listQuery('unmonitored','',-3).offset,0);
  assert.equal(listQuery('unmonitored','',0).monitored,false);
  assert.equal(listQuery('all','x'.repeat(500),0).q.length,100);
  assert.equal(pageCount(0),1);assert.equal(pageCount(26),2);assert.equal(pageCount(25),1);
});
test('labels',()=>{
  assert.equal(countsLabel(base),'1 of 3 in library, 2 missing');
  assert.equal(lastSyncLabel(null),'Never synced');
  assert.equal(lastSyncLabel('garbage'),'Never synced');
  assert.equal(lastSyncLabel('2026-10-18T12:34:56Z'),'Synced 2026-10-18 12:34 UTC');
  const names=new Map([[4,'HD']]);
  assert.equal(policySummary(base,names),'No policy configured');
  assert.equal(policySummary(full,names),'HD · /m · Released · search on add · 2 tags');
  assert.match(policySummary({...full,root_folder_id:null,root_folder_path:null,tag_ids:[]},new Map()),/profile 4 · no root folder .*\(incomplete\)$/);
});
test('refresh command status text',()=>{
  const cmd={id:'a',name:'refresh_collections',origin:'api',attempts:1,created_at:1,started_at:null,completed_at:null,error_code:null,collections_checked:1,collections_failed:0,movies_added:2,searches_pending:0};
  for(const s of ['queued','running','retry_wait'])assert.equal(isActive(s),true,s);
  for(const s of ['succeeded','failed','cancelled'])assert.equal(isTerminal(s),true,s);
  assert.equal(statusText('retry_wait'),'Waiting to retry');
  assert.equal(refreshSummary({...cmd,status:'running'}),'Refresh running.');
  assert.equal(refreshSummary({...cmd,status:'succeeded'}),'Refresh succeeded: 1 collection checked, 0 failed, 2 movies added.');
  assert.match(refreshSummary({...cmd,status:'succeeded',searches_pending:1}),/1 search pending \(search on add is recorded but searches are not submitted\)\.$/);
  assert.match(refreshSummary({...cmd,status:'failed',error_code:'metadata_unavailable'}),/Refresh failed: .* Error: metadata_unavailable\.$/);
});
