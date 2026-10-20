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
test('manual health tracking reads bounded pages and verifies every requested generation in one epoch',async()=>{
 const {getHealthRequestObservation,healthRequestObserved}=await import('../src/lib/api.ts');
 const epoch='aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa';
 const checks=Array.from({length:18},(_,n)=>({identity:{scope:n<9?'movies':'tv',check_key:`check_${String(n).padStart(2,'0')}`},generation:3,observed_generation:3,observed_epoch:epoch,evaluation:'current',last_error:null}));
 const identities=checks.map(check=>check.identity);
 const page=offset=>({total:18,offset,limit:16,lifecycle:{epoch},coverage:{identities},checks:checks.slice(offset,offset+16)});
 const admission={selection:checks.map(check=>({identity:check.identity,generation:3}))};
 const old=globalThis.fetch,calls=[];let failure='',reads=0;
 globalThis.fetch=async url=>{
  calls.push(url);reads++;const value=page(Number(new URL(url,'http://fixture.invalid').searchParams.get('offset')));
  if(failure==='http')return new Response('{}',{status:503});
  if(failure==='epoch')value.lifecycle.epoch='bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb';
  if(failure==='registry')value.coverage.identities=identities.slice(1);
  if(failure==='missing')value.checks=value.checks.slice(1);
  return Response.json(value);
 };
 try{
  let result=await getHealthRequestObservation(page(0));assert.equal(result.ok,true);assert.equal(reads,1);assert.equal(new URL(calls[0],'http://fixture.invalid').searchParams.get('offset'),'16');
  assert.equal(result.data.checks.length,18);assert.equal(healthRequestObserved(admission,result.data,epoch),true);
  // The hidden eighteenth identity must independently satisfy the generation, epoch and current-state requirements.
  for(const patch of [{observed_generation:2},{observed_generation:null},{observed_epoch:'old'},{evaluation:'stale'},{last_error:'check_failed'}]){
   const changed=structuredClone(result.data);Object.assign(changed.checks[17],patch);assert.equal(healthRequestObserved(admission,changed,epoch),false);
  }
  assert.equal(healthRequestObserved(admission,result.data,'new'),false);
  for(failure of ['http','epoch','registry','missing'])assert.equal((await getHealthRequestObservation(page(0))).ok,false,failure);
  failure='';reads=0;assert.equal((await getHealthRequestObservation()).ok,true);assert.equal(reads,2);
  reads=0;assert.equal((await getHealthRequestObservation({...page(0),total:129})).ok,false);assert.equal(reads,0);
  const maximum=Array.from({length:128},(_,n)=>({...checks[0],identity:{scope:'tv',check_key:`bounded_${n}`}}));
  globalThis.fetch=async url=>{reads++;const offset=Number(new URL(url,'http://fixture.invalid').searchParams.get('offset'));return Response.json({...page(offset),total:128,coverage:{identities:maximum.map(check=>check.identity)},checks:maximum.slice(offset,offset+16)});};
  assert.equal((await getHealthRequestObservation()).data.checks.length,128);assert.equal(reads,8);
 }finally{globalThis.fetch=old;}
});
