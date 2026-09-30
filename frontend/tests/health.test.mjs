import test from 'node:test';
import assert from 'node:assert/strict';
import {getHealth,listHealthCommands,getHealthCommand,createHealthCommand,cancelHealthCommand,getHealthTransitions} from '../src/lib/api.ts';
const id='aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa';
test('native health requests preserve scopes, strict admission, bounded pages, membership status and empty cancel body',async()=>{
 const old=globalThis.fetch,calls=[];
 globalThis.fetch=async(path,options)=>{calls.push({path,options});return new Response('{}');};
 try{
  for(const scope of ['all','tv','movies','system']){await getHealth(scope,127);await createHealthCommand(scope);await listHealthCommands(scope,128,'retry_wait');}
  for(let n=0;n<4;n++){
   const scope=['all','tv','movies','system'][n];assert.equal(calls[n*3].path,`/api/v1/health?scope=${scope}&limit=16&offset=127`);assert.equal(calls[n*3].options.method,undefined);
   assert.deepEqual(JSON.parse(calls[n*3+1].options.body),{scope,priority:'normal'});assert.equal(calls[n*3+1].options.method,'POST');
   assert.equal(calls[n*3+2].path,`/api/v1/health/commands?scope=${scope}&limit=20&offset=128&status=retry_wait`);
  }
  await getHealthCommand(id);await cancelHealthCommand(id);assert.equal(calls.at(-1).options.method,'POST');assert.equal(calls.at(-1).options.body,undefined);assert.equal(calls.at(-1).path,`/api/v1/health/commands/${id}/cancel`);
  await getHealthTransitions(9007199254740991);assert.equal(calls.at(-1).path,'/api/v1/health/transitions?after_sequence=9007199254740991&limit=16');
  const count=calls.length;
  for(const attempt of [getHealth('bad'),getHealth('all',128),getHealth('tv',-1),getHealth('movies',1.5),listHealthCommands('all',129),listHealthCommands('all',0,'bad'),createHealthCommand('bad'),getHealthCommand('../other'),cancelHealthCommand('bad'),getHealthTransitions(-1),getHealthTransitions(9007199254740992)])assert.equal((await attempt).ok,false);
  assert.equal(calls.length,count);
 }finally{globalThis.fetch=old;}
});
test('health failures remain Result errors and never automatically retry mutations',async()=>{
 const old=globalThis.fetch;let calls=0;
 try{
  for(const status of [409,429,413,503]){
   globalThis.fetch=async()=>{calls++;return new Response(JSON.stringify({error:{code:'health_fixture',message:'Fixture error'}}),{status});};
   const before=calls,result=await createHealthCommand('all');assert.deepEqual(result,{ok:false,error:'Fixture error',code:'health_fixture',status});assert.equal(calls,before+1);
  }
  globalThis.fetch=async()=>{calls++;return new Response('invalid');};let before=calls;assert.equal((await cancelHealthCommand(id)).ok,false);assert.equal(calls,before+1);
  globalThis.fetch=async()=>{calls++;throw new Error('network unavailable');};before=calls;assert.deepEqual(await createHealthCommand('tv'),{ok:false,error:'network unavailable'});assert.equal(calls,before+1);
 }finally{globalThis.fetch=old;}
});
