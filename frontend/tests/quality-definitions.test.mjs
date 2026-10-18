import test from 'node:test';
import assert from 'node:assert/strict';
import {definitionDrafts,definitionUpdates} from '../src/lib/quality-definition-draft.ts';
import {getQualityDefinitionLimits,getQualityDefinitionDefaults,updateQualityDefinitions,resetQualityDefinitions} from '../src/lib/api.ts';
const rows=[{id:1,title:'One',min_size:1,preferred_size:2,max_size:3},{id:2,title:'Two',min_size:null,preferred_size:null,max_size:null}];
const limits={min:0,max:1000,unit:'MiB/minute'};
test('definition edits send only changed complete updates and preserve null versus zero',()=>{
 const drafts=definitionDrafts(rows);assert.deepEqual(definitionUpdates(drafts,rows,limits),{ok:true,data:[]});
 drafts[0].title='Renamed';drafts[0].min_size='0';drafts[0].preferred_size='';
 assert.deepEqual(definitionUpdates(drafts,rows,limits),{ok:true,data:[{id:1,title:'Renamed',min_size:0,preferred_size:null,max_size:3}]});
 drafts[1].min_size='0';assert.equal(definitionUpdates(drafts,rows,limits).data.length,2);
});
test('definition validation covers finite bounds, nullable pair ordering and catalog identity',()=>{
 for(const value of ['Infinity','NaN','0x10','1001','-1','1e999']) {const d=definitionDrafts(rows);d[0].min_size=value;assert.equal(definitionUpdates(d,rows,limits).ok,false,value);}
 const d=definitionDrafts(rows);d[0].preferred_size='';d[0].min_size='4';assert.equal(definitionUpdates(d,rows,limits).ok,false);
 d[0].min_size='';d[0].max_size='';assert.equal(definitionUpdates(d,rows,limits).ok,true);
 d[0].id=999;assert.equal(definitionUpdates(d,rows,limits).ok,false);
 for(const title of [' ','x\n','x'.repeat(101)]) {const d=definitionDrafts(rows);d[0].title=title;assert.equal(definitionUpdates(d,rows,limits).ok,false);}
});
test('quality API wrappers scope nullable bulk writes and explicit reset-title selection',async()=>{
 const old=globalThis.fetch,seen=[];
 globalThis.fetch=async(path,options)=>{seen.push([path,options]);return new Response('[]');};
 try {
  for(const domain of ['tv','movies']) {await getQualityDefinitionLimits(domain);await getQualityDefinitionDefaults(domain);await updateQualityDefinitions(domain,[{id:0,title:'Unknown',min_size:null,preferred_size:0,max_size:null}]);await resetQualityDefinitions(domain,false);await resetQualityDefinitions(domain,true);}
  assert.equal(seen[0][0],'/api/v1/tv/quality-definitions/limits');assert.equal(seen[6][0],'/api/v1/movies/quality-definitions/defaults');
  assert.equal(seen[2][1].method,'PUT');assert.equal(JSON.parse(seen[2][1].body)[0].min_size,null);
  assert.deepEqual(JSON.parse(seen[3][1].body),{reset_titles:false});assert.deepEqual(JSON.parse(seen[4][1].body),{reset_titles:true});assert.equal(seen[4][1].method,'POST');
 } finally {globalThis.fetch=old;}
});
