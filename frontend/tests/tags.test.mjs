import test from 'node:test';
import assert from 'node:assert/strict';
import {tagLabelError,assignmentError,unknownTagWrite} from '../src/lib/tag-draft.ts';
import {listTags,listTagDetails,createTag,renameTag,deleteTag,getTagOwners,assignLibraryTags,addLibrary} from '../src/lib/api.ts';
test('tag draft validates source label grammar and explicit empty replacement',()=>{
 for(const label of ['TV-1','a','x'.repeat(128)])assert.equal(tagLabelError(label),'');
 for(const label of ['',' a','a b','ä','x'.repeat(129)])assert.ok(tagLabelError(label));
 const catalog=[{id:1,media_type:'tv',label:'a'}];
 assert.equal(assignmentError([],catalog,'replace'),'');assert.ok(assignmentError([],catalog,'add'));assert.ok(assignmentError([],catalog,'remove'));
 assert.equal(assignmentError([1],catalog,'add'),'');for(const ids of [[2],[1,1],[NaN],[Number.MAX_SAFE_INTEGER+1]])assert.ok(assignmentError(ids,catalog,'replace'));
 assert.equal(unknownTagWrite({status:409}),false);assert.equal(unknownTagWrite({}),true);assert.equal(unknownTagWrite({status:503}),true);
});
test('tag endpoints scope catalog and atomic tag-only assignments without overposting',async()=>{
 const original=globalThis.fetch,calls=[];
 globalThis.fetch=async(url,options)=>{calls.push({url,options});return options?.method==='DELETE'?new Response(null,{status:204}):Response.json([]);};
 try{
  for(const domain of ['tv','movies']){
   await listTags(domain);assert.equal(calls.at(-1).url,`/api/v1/${domain}/tags`);
   await listTagDetails(domain);assert.ok(calls.at(-1).url.endsWith('/tags/detail'));
   await createTag(domain,'ABC');assert.deepEqual(JSON.parse(calls.at(-1).options.body),{label:'ABC'});
   await renameTag(domain,1,'new');assert.equal(calls.at(-1).options.method,'PUT');
   await deleteTag(domain,1);assert.equal(calls.at(-1).options.method,'DELETE');
   await getTagOwners(domain,1,25);assert.ok(calls.at(-1).url.endsWith('/tags/1/owners?limit=25&offset=25'));
   await assignLibraryTags(domain,[1,2],{mode:'replace',ids:[]});assert.equal(calls.at(-1).url,domain==='tv'?'/api/v1/tv/series/editor':'/api/v1/movies/editor');assert.deepEqual(JSON.parse(calls.at(-1).options.body),{ids:[1,2],patch:{tags:{mode:'replace',ids:[]}}});
   await addLibrary(domain,9,'/owned',{tags:{mode:'add',ids:[1]}});assert.deepEqual(JSON.parse(calls.at(-1).options.body).settings,{tags:{mode:'add',ids:[1]}});
  }
  const count=calls.length;assert.equal((await assignLibraryTags('tv',[],{mode:'add',ids:[1]})).ok,false);assert.equal((await getTagOwners('tv',1,-1)).ok,false);assert.equal((await renameTag('tv',NaN,'a')).ok,false);assert.equal(calls.length,count);
 }finally{globalThis.fetch=original;}
});
