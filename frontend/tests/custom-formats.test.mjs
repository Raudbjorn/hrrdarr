import test from 'node:test';
import assert from 'node:assert/strict';
import { listCustomFormats, getCustomFormatSchema, createCustomFormat, updateCustomFormat, deleteCustomFormat } from '../src/lib/api.ts';

test('custom format helpers keep domain, exact names and copied specifications in their wire contract', async () => {
  const original = globalThis.fetch;
  const calls = [];
  const input = {name:'  Exact name  ',include_when_renaming:true,specifications:[{name:'Condition',required:true,negate:true,condition:{kind:'language',value:1,except_language:true}}]};
  globalThis.fetch = async (path, options) => {
    calls.push([path, options.method, options.body ? JSON.parse(options.body) : undefined]);
    return options.method === 'DELETE' ? new Response(null,{status:204}) : Response.json({id:7,...input});
  };
  try {
    await listCustomFormats('tv'); await getCustomFormatSchema('movies');
    assert.equal((await createCustomFormat('tv',input)).ok,true);
    assert.equal((await updateCustomFormat('movies',7,input)).ok,true);
    assert.deepEqual(await deleteCustomFormat('movies',7),{ok:true,data:undefined});
    for(const id of [0,-1,1.5,NaN,Infinity]) {
      assert.equal((await updateCustomFormat('tv',id,input)).ok,false);
      assert.equal((await deleteCustomFormat('movies',id)).ok,false);
    }
    assert.deepEqual(calls,[['/api/v1/tv/custom-formats',undefined,undefined],['/api/v1/movies/custom-formats/schema',undefined,undefined],['/api/v1/tv/custom-formats','POST',input],['/api/v1/movies/custom-formats/7','PUT',input],['/api/v1/movies/custom-formats/7','DELETE',undefined]]);
  } finally { globalThis.fetch=original; }
});

test('custom format helpers retain validation and conflict messages', async () => {
  const original=globalThis.fetch;
  globalThis.fetch=async()=>Response.json({error:{code:'custom_format_name_conflict',message:'Name already exists'}},{status:409});
  try { assert.deepEqual(await createCustomFormat('tv',{name:'Duplicate',specifications:[],include_when_renaming:false}),{ok:false,status:409,code:'custom_format_name_conflict',error:'Name already exists'}); }
  finally {globalThis.fetch=original;}
});

import { parseCustomFormatDraft } from '../src/lib/custom-format-draft.ts';
const schema={max_specifications:64,conditions:[{kind:'release_title',pattern:'.+'},{kind:'size',min_gib:0,max_gib:100},{kind:'language',value:1,except_language:false},{kind:'year',min:1900,max:2099}],choices:{language:[{value:1,label:'English'}]}};
const draft={name:'Exact',include_when_renaming:true,specifications:[{name:'Title',required:true,negate:false,condition:{kind:'release_title',pattern:'pattern'}}]};
test('native JSON draft roundtrips flags and validates structure without writing',()=>{
  assert.deepEqual(parseCustomFormatDraft(JSON.stringify(draft),schema),{ok:true,data:draft});
  for(const value of [{...draft,specifications:[{...draft.specifications[0],condition:{kind:'year',min:0,max:2000}}]},{...draft,id:1},{...draft,specifications:[]},{...draft,specifications:[{...draft.specifications[0],condition:{kind:'edition',pattern:'x'}}]},{...draft,specifications:[{...draft.specifications[0],condition:{kind:'language',value:999,except_language:false}}]},{...draft,specifications:[{...draft.specifications[0],condition:{kind:'size',min_gib:10,max_gib:5}}]},{...draft,specifications:[{...draft.specifications[0],condition:{kind:'release_title',pattern:'x',unknown:true}}]}]) assert.equal(parseCustomFormatDraft(JSON.stringify(value),schema).ok,false);
  assert.equal(parseCustomFormatDraft('[',schema).ok,false);
  assert.equal(parseCustomFormatDraft(' '.repeat(73729),schema).ok,false);
  const long=structuredClone(draft);long.name='界'.repeat(100);long.specifications[0].name='界'.repeat(100);long.specifications[0].condition.pattern='x'.repeat(4096);
  assert.equal(parseCustomFormatDraft(JSON.stringify(long),schema).ok,true);
  long.name+='界';assert.equal(parseCustomFormatDraft(JSON.stringify(long),schema).ok,false);
});
