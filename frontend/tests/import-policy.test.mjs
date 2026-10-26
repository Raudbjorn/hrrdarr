import test from 'node:test';
import assert from 'node:assert/strict';
import {importPolicyUpdate} from '../src/lib/import-policy-draft.ts';
import {getImportPolicy,updateImportPolicy} from '../src/lib/api.ts';

const draft={skip_free_space_check_when_importing:false,minimum_free_space_mib:100,download_client_working_folders:'_UNPACK_|_FAILED_'};
test('import policy validates integral MiB bounds and UTF-8 bytes without rewriting literal folder prefixes',()=>{
  for(const minimum of [undefined,NaN,Infinity,99,100.5,2147483648]) assert.equal(importPolicyUpdate(7,{...draft,minimum_free_space_mib:minimum}).ok,false,String(minimum));
  for(const minimum of [100,2147483647]) assert.equal(importPolicyUpdate(7,{...draft,minimum_free_space_mib:minimum}).data.minimum_free_space_mib,minimum);
  for(const prefixes of ['', ' ', ' _UNPACK_||_failed_| ', 'tab\t|line\n||end ', '|', 'é'.repeat(2048)]) {
    assert.deepEqual(importPolicyUpdate(7,{...draft,download_client_working_folders:prefixes}),{ok:true,data:{revision:7,...draft,download_client_working_folders:prefixes}});
  }
  assert.equal(importPolicyUpdate(7,{...draft,download_client_working_folders:'é'.repeat(2048)+'a'}).ok,false);
  assert.equal(importPolicyUpdate(7,{...draft,skip_free_space_check_when_importing:true,minimum_free_space_mib:99}).ok,false,'Skipping the check does not erase or invalidate the saved minimum.');
});

test('import policy transport retains domain, revision, false and exact folder bytes',async()=>{
  const original=globalThis.fetch,seen=[];
  globalThis.fetch=async(path,options)=>{seen.push({path,options});return new Response(JSON.stringify({media_type:path.includes('/tv/')?'tv':'movies',revision:8,...draft}));};
  try {
    for(const domain of ['tv','movies']) {
      const loaded=await getImportPolicy(domain);assert.equal(loaded.data.media_type,domain);
      const input={revision:7,...draft,download_client_working_folders:domain==='tv'?'':' _UNPACK_||_FAILED_| '};
      assert.equal((await updateImportPolicy(domain,input)).ok,true);
      const sent=seen.at(-1);assert.equal(sent.path,`/api/v1/${domain}/import-policy`);assert.equal(sent.options.method,'PUT');assert.deepEqual(JSON.parse(sent.options.body),input);
    }
    assert.equal(seen.length,4);
  } finally {globalThis.fetch=original;}
});

test('failed or unreadable policy writes retain uncertain/conflict evidence and never retry automatically',async()=>{
  const original=globalThis.fetch;let calls=0;
  try {
    for(const status of [409,503]) {
      globalThis.fetch=async()=>{calls++;return new Response(JSON.stringify({error:{code:'policy_failure',message:'Read the saved policy'}}),{status});};
      const result=await updateImportPolicy('movies',{revision:7,...draft});assert.equal(result.ok,false);assert.equal(result.status,status);assert.equal(result.code,'policy_failure');
    }
    globalThis.fetch=async()=>{calls++;return new Response('{lost response');};
    const result=await updateImportPolicy('tv',{revision:7,...draft});assert.equal(result.ok,false);assert.equal(result.status,undefined);
    assert.equal(calls,3);
  } finally {globalThis.fetch=original;}
});
