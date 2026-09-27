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

import { parseCommunityCustomFormat, exportCommunityCustomFormat } from '../src/lib/custom-format-draft.ts';
const interchangeCases=[
 ['ReleaseTitleSpecification',{kind:'release_title',pattern:'(?<=WEB[.-])codec'}, {value:'(?<=WEB[.-])codec'}],
 ['ReleaseGroupSpecification',{kind:'release_group',pattern:'group'}, {value:'group'}],
 ['EditionSpecification',{kind:'edition',pattern:'extended'}, {value:'extended'}],
 ['LanguageSpecification',{kind:'language',value:1,except_language:true}, {value:1,exceptLanguage:true}],
 ['SizeSpecification',{kind:'size',min_gib:0.25,max_gib:20}, {min:0.25,max:20}],
 ['SourceSpecification',{kind:'source',value:0}, {value:0}],
 ['ResolutionSpecification',{kind:'resolution',value:1080}, {value:1080}],
 ['QualityModifierSpecification',{kind:'quality_modifier',value:0}, {value:0}],
 ['IndexerFlagSpecification',{kind:'indexer_flag',value:1}, {value:1}],
 ['ReleaseTypeSpecification',{kind:'release_type',value:0}, {value:0}],
 ['YearSpecification',{kind:'year',min:1980,max:2030}, {min:1980,max:2030}],
];
function interchangeSchema(media) {
 const cases=interchangeCases.filter(([,c])=>media==='tv' ? !['edition','quality_modifier','year'].includes(c.kind) : c.kind!=='release_type');
 return {max_specifications:64,media_type:media,conditions:cases.map(([,c])=>c),choices:Object.fromEntries(cases.filter(([,c])=>'value' in c).map(([,c])=>[c.kind,(c.kind==='indexer_flag'?[1,8]:[0,1,1080]).map(value=>({value,label:String(value)}))]))};
}
test('community format roundtrips every supported implementation across explicit domains',()=>{
 for(const media of ['tv','movies']) {
  const contract=interchangeSchema(media);
  const selected=interchangeCases.filter(([,c])=>contract.conditions.some(x=>x.kind===c.kind));
  const native={name:`Exact ${media}`,include_when_renaming:true,specifications:selected.map(([,condition],i)=>({name:` Exact ${i} `,negate:i%2===0,required:i%2===1,condition}))};
  const result=exportCommunityCustomFormat(native,contract);assert.equal(result.ok,true);
  const external=JSON.parse(result.data);
  assert.deepEqual(Object.keys(external),['name','includeCustomFormatWhenRenaming','specifications']);
  external.specifications.forEach((spec,i)=>{assert.equal(spec.implementation,selected[i][0]);assert.deepEqual(spec.fields,selected[i][2]);assert.equal(Array.isArray(spec.fields),false);});
  assert.deepEqual(parseCommunityCustomFormat(result.data,contract),{ok:true,data:native});
  // Identity and presentation metadata never become native IDs, domain choices, or exported data.
  external.id=999;
  for(const spec of external.specifications) Object.assign(spec,{id:999,implementationName:'private-presentation-sentinel',infoLink:'https://private.invalid/key'});
  const imported=parseCommunityCustomFormat(JSON.stringify(external),contract);assert.deepEqual(imported,{ok:true,data:native});
  const roundtrip=exportCommunityCustomFormat(imported.data,contract);assert.equal(roundtrip.data.includes('private'),false);assert.equal(roundtrip.data.includes('999'),false);
 }
});
test('community defaults come from source zero/false fields, never native example values',()=>{
 for(const media of ['tv','movies']) {
  const contract=interchangeSchema(media);
  for(const implementation of ['SourceSpecification','ResolutionSpecification','LanguageSpecification',media==='tv'?'ReleaseTypeSpecification':'QualityModifierSpecification']) {
   const result=parseCommunityCustomFormat(JSON.stringify({name:'Defaults',specifications:[{name:'Condition',implementation}]}),contract);
   assert.equal(result.ok,true,implementation);assert.equal(result.data.include_when_renaming,false);
   assert.equal(result.data.specifications[0].negate,false);assert.equal(result.data.specifications[0].required,false);assert.equal(result.data.specifications[0].condition.value,0);
   if(implementation==='LanguageSpecification') assert.equal(result.data.specifications[0].condition.except_language,false);
  }
  const size=parseCommunityCustomFormat(JSON.stringify({name:'Size',specifications:[{name:'Size',implementation:'SizeSpecification',fields:{max:10}}]}),contract);
  assert.equal(size.ok,true);assert.equal(size.data.specifications[0].condition.min_gib,0);
  for(const implementation of ['ReleaseTitleSpecification','ReleaseGroupSpecification','SizeSpecification','IndexerFlagSpecification',...(media==='movies'?['YearSpecification','EditionSpecification']:[])]) {
   const failed=parseCommunityCustomFormat(JSON.stringify({name:'Incomplete',specifications:[{name:'Incomplete',implementation}]}),contract);
   assert.equal(failed.ok,false,implementation);assert.match(failed.error,/cannot be represented/);
  }
 }
});
test('community import rejects unknown semantics atomically, invalid values and oversized documents',()=>{
 const contract=interchangeSchema('tv');
 const valid={name:'Valid',specifications:[{name:'Title',implementation:'ReleaseTitleSpecification',fields:{value:'ok'}}]};
 const variations=[null,[],{...valid,apiKey:'secret'}, {...valid,media_type:'movies'}, {...valid,specifications:[...valid.specifications,{name:'Future',implementation:'FutureSpecification',fields:{value:1}}]}, {...valid,specifications:[{...valid.specifications[0],future:true}]}, {...valid,specifications:[{...valid.specifications[0],fields:{value:'ok',future:true}}]}, {...valid,specifications:[{...valid.specifications[0],fields:[]}]}, {...valid,specifications:[{...valid.specifications[0],fields:{value:null}}]}, {...valid,specifications:[{name:'Edition',implementation:'EditionSpecification',fields:{value:'extended'}}]}, {...valid,specifications:[{name:'Language',implementation:'LanguageSpecification',fields:{value:57}}]}, {...valid,specifications:Array(65).fill(valid.specifications[0])}];
 for(const value of variations) {const result=parseCommunityCustomFormat(JSON.stringify(value),contract);assert.equal(result.ok,false,JSON.stringify(value));assert.equal('data' in result,false);}
 assert.equal(parseCommunityCustomFormat('{',contract).ok,false);
 assert.equal(parseCommunityCustomFormat('界'.repeat(25000),contract).ok,false);
 assert.equal(parseCommunityCustomFormat(JSON.stringify({...valid,specifications:[{name:'Prototype',implementation:'__proto__'}]}),contract).ok,false);
 assert.equal(parseCommunityCustomFormat(JSON.stringify({name:'Wrong domain',specifications:[{name:'Pack',implementation:'ReleaseTypeSpecification',fields:{value:0}}]}),interchangeSchema('movies')).ok,false);
});

test('community export rejects output expansion beyond the bounded interchange size',()=>{
 const many={name:'Bounded',include_when_renaming:false,specifications:Array.from({length:64},(_,i)=>({name:`Condition ${i}`,negate:false,required:false,condition:{kind:'release_title',pattern:'x'.repeat(1000)}}))};
 const exported=exportCommunityCustomFormat(many,interchangeSchema('tv'));
 assert.equal(exported.ok,false);assert.match(exported.error,/72 KiB/);
});

test('future schema conditions cannot silently export without a community mapping',()=>{
 const future={max_specifications:64,conditions:[{kind:'future',value:0}],choices:{future:[{value:0,label:'Future'}]}};
 const input={...draft,specifications:[{...draft.specifications[0],condition:{kind:'future',value:0}}]};
 assert.deepEqual(exportCommunityCustomFormat(input,future),{ok:false,error:'Condition has no supported community representation.'});
});

import { exportNativeCustomFormat } from '../src/lib/custom-format-draft.ts';
test('native export validates the actual pretty bytes and preserves its import roundtrip',()=>{
 const contract=interchangeSchema('tv');
 const many={name:'Bounded',include_when_renaming:false,specifications:Array.from({length:64},(_,i)=>({name:`Condition ${i}`,negate:false,required:false,condition:{kind:'release_title',pattern:'界'.repeat(333)}}))};
 assert.equal(parseCustomFormatDraft(JSON.stringify(many),contract).ok,true);
 const exported=exportNativeCustomFormat(many,contract);
 assert.equal(exported.ok,false);assert.match(exported.error,/72 KiB/);
 const valid=exportNativeCustomFormat(draft,contract);assert.equal(valid.ok,true);
 assert.deepEqual(parseCustomFormatDraft(valid.data,contract),{ok:true,data:draft});
 assert.equal(exportNativeCustomFormat({...draft,specifications:[]},contract).ok,false);
});
test('community explicit null follows each pinned importer instead of assuming omission',()=>{
 const valid={name:'Null flags',specifications:[{name:'Source',implementation:'SourceSpecification',fields:{value:0}}]};
 for(const field of ['negate','required','includeCustomFormatWhenRenaming','fields']) {
  const input=structuredClone(valid);
  if(field==='includeCustomFormatWhenRenaming') input[field]=null;
  else input.specifications[0][field]=null;
  assert.equal(parseCommunityCustomFormat(JSON.stringify(input),interchangeSchema('tv')).ok,true,field);
  assert.equal(parseCommunityCustomFormat(JSON.stringify(input),interchangeSchema('movies')).ok,false,field);
 }
});
