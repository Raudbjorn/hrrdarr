import test from 'node:test';
import assert from 'node:assert/strict';
import {saveCompletedDownloadHandling,inheritProcessingPolicy,inheritRefreshSchedule,readDownloadHandling} from '../src/lib/api.ts';
const id='aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa';
const json=(data,status=200)=>new Response(JSON.stringify(data),{status});
test('CDH controls preserve domain, explicit CAS/null and transfer mode without retrying failed writes',async()=>{
 const original=globalThis.fetch,calls=[];
 globalThis.fetch=async(path,options)=>{calls.push([path,JSON.parse(options.body),options.method]);return json({error:{code:'cdh_conflict',message:'Changed'}},409);};
 try{
  for(const media of ['tv','movies']){
   assert.equal((await saveCompletedDownloadHandling(media,{revision:3,enabled:false})).status,409);
   assert.equal((await inheritProcessingPolicy(id,media,{provider_revision:7,revision:null,mode:'hardlink'})).status,409);
   assert.equal((await inheritRefreshSchedule({target:{provider_id:id,media_type:media},provider_revision:7,revision:9})).status,409);
  }
  assert.equal(calls.length,6);
  for(let i=0;i<2;i++){
   const media=['tv','movies'][i];
   assert.deepEqual(calls[i*3],[`/api/v1/${media}/completed-download-handling`,{revision:3,enabled:false},'PUT']);
   assert.deepEqual(calls[i*3+1],[`/api/v1/download-processing/policies/${id}/${media}/inherit`,{provider_revision:7,revision:null,mode:'hardlink'},'PUT']);
   assert.deepEqual(calls[i*3+2],[`/api/v1/download-refresh/schedules/inherit`,{target:{provider_id:id,media_type:media},provider_revision:7,revision:9},'PUT']);
  }
  assert.equal((await inheritRefreshSchedule({target:{provider_id:id,media_type:'tv'},provider_revision:7,revision:undefined})).ok,false);
  assert.equal(calls.length,6);
 }finally{globalThis.fetch=original;}
});
test('shared readback requires both domains, schedules and each configured policy; partial failure is not success',async()=>{
 const original=globalThis.fetch;let fail='',seen=[];
 globalThis.fetch=async path=>{
  seen.push(path);if(path===fail)return json({error:{code:'cdh_storage_error',message:'Unavailable'}},503);
  if(path===`/api/v1/providers/${id}`)return json({id,revision:7,settings:{implementation:'qbittorrent',tv:{},movies:{}}});
  if(path==='/api/v1/download-refresh/schedules')return json([]);
  return json({media_type:path.endsWith('/movies')||path.includes('/movies/')?'movies':'tv',enabled:true,enabled_override:null,reconciliation_reason:null});
 };
 try{
  const good=await readDownloadHandling(id);assert.equal(good.ok,true);assert.deepEqual(Object.keys(good.data.policies),['tv','movies']);
  const paths=[...seen];assert.equal(paths.length,6);
  for(const path of paths){fail=path;seen=[];const result=await readDownloadHandling(id);assert.equal(result.ok,false,path);assert.equal(result.status,503);}
  fail='';const noClient=await readDownloadHandling();assert.equal(noClient.ok,true);assert.equal(noClient.data.provider,null);assert.deepEqual(noClient.data.policies,{});
 }finally{globalThis.fetch=original;}
});
