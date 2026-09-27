import test from 'node:test';
import assert from 'node:assert/strict';
import {getQualityProfile,getQualityProfileSchema,listQualityDefinitions,createQualityProfile,updateQualityProfile,deleteQualityProfile} from '../src/lib/api.ts';
test('profile wrappers scope reads/writes, preserve explicit null policy and surface in-use failures',async()=>{
 const original=globalThis.fetch; const seen=[];
 globalThis.fetch=async(path,options)=>{seen.push([path,options]);return options.method==='DELETE'?new Response(JSON.stringify({error:{code:'profile_in_use',message:'Assigned'}}),{status:409}):new Response('{}');};
 try {
  for(const domain of ['tv','movies']) {
   await getQualityProfileSchema(domain);await listQualityDefinitions(domain);await getQualityProfile(domain,7);
   const input={name:'Legacy',items:[],policy:null};await createQualityProfile(domain,input);await updateQualityProfile(domain,7,input);
   const deleted=await deleteQualityProfile(domain,7);assert.equal(deleted.code,'profile_in_use');assert.equal(deleted.status,409);
  }
  assert.equal(seen[0][0],'/api/v1/tv/quality-profiles/schema');assert.equal(seen[6][0],'/api/v1/movies/quality-profiles/schema');
  assert.equal(seen[3][1].method,'POST');assert.equal(seen[4][1].method,'PUT');assert.equal(seen[5][1].method,'DELETE');
  assert.equal(JSON.parse(seen[4][1].body).policy,null);assert.ok(seen.every(([,o])=>o.signal instanceof AbortSignal));
 } finally {globalThis.fetch=original;}
});
