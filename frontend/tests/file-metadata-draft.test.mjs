import test from 'node:test';
import assert from 'node:assert/strict';
import { emptyFileMetadataDraft, fileMetadataCatalogs, buildFileMetadataPatch, validateFileReadback, mixedFileValue, fileValueLabel } from '../src/lib/file-metadata-draft.ts';
const choice = value => ({ value, label: String(value) });
const schema = domain => ({ media_type: domain, choices: { language: [-2,-1,0,1,domain === 'tv' ? 52 : 57].map(choice), indexer_flag: [1,2,4,8,16,32,64,128,256,...(domain === 'tv' ? [] : [512,1024,2048])].map(choice), ...(domain === 'tv' ? { release_type: [0,1,2,3].map(choice) } : {}) } });
const definitions = domain => [{ id: 999, media_type: domain, quality: { id: 7, name: 'Bluray' }, title: 'Custom Bluray' }];
const catalogs = domain => { const result = fileMetadataCatalogs(domain,definitions(domain),schema(domain)); assert.equal(result.ok,true); return result.data; };
const build = (domain, fields) => buildFileMetadataPatch(domain, { ...emptyFileMetadataDraft(), ...fields }, catalogs(domain));
const set = value => ({ action:'set', value });
const clear = { action:'clear' };
export const resource = (domain, id=1, owner=9) => ({id,path:'/file.mkv',relative_path:'file.mkv',quality:null,languages:null,size:null,date_added:null,release_group:null,indexer_flags:null,scene_name:null,media_info:null,custom_formats:null,custom_format_score:null,quality_cutoff_not_met:null,...(domain==='tv'?{series_id:owner,season_number:null,release_type:null}:{movie_id:owner,edition:null,original_file_path:null})});
test('all fields default unchanged; explicit unknown, empty and concrete zero survive both domains',()=>{
 for(const domain of ['tv','movies']) {
  assert.equal(build(domain,{}).ok,false);
  assert.deepEqual(build(domain,{languages:clear,release_group:set(''),indexer_flags:set(0)}),{ok:true,data:{languages:null,release_group:'',indexer_flags:0}});
  assert.deepEqual(build(domain,{languages:set([])}),{ok:true,data:{languages:[]}});
  assert.deepEqual(build(domain,{languages:set([0])}),{ok:true,data:{languages:[0]}});
  assert.deepEqual(build(domain,{quality:clear}),{ok:true,data:{quality:null}});
 }
 assert.equal(build('tv',{edition:clear}).ok,false);
 assert.equal(build('movies',{release_type:clear}).ok,false);
 assert.deepEqual(build('movies',{edition:set('')}),{ok:true,data:{edition:''}});
 assert.deepEqual(build('tv',{release_type:set(0)}),{ok:true,data:{release_type:0}});
});
test('quality uses catalog quality ID and explicitly replaces the entire nested revision',()=>{
 const revision={version:2,real:0,is_repack:false};
 for(const domain of ['tv','movies']) {
  assert.deepEqual(build(domain,{quality:set({quality_id:7,revision})}),{ok:true,data:{quality:{quality_id:7,revision}}});
  assert.deepEqual(build(domain,{quality:set({quality_id:7,revision:null})}),{ok:true,data:{quality:{quality_id:7,revision:null}}});
  for(const value of [{quality_id:999,revision:null},{quality_id:7},{quality_id:7,revision:{...revision,version:0}},{quality_id:7,revision:{...revision,real:2147483648}},{quality_id:7,revision:{...revision,is_repack:1}}]) assert.equal(build(domain,{quality:set(value)}).ok,false);
 }
});
test('catalog membership, domains, language sentinels and masks fail closed',()=>{
 assert.deepEqual(catalogs('movies').languages.map(c=>c.value),[0,1,57]);
 assert.equal(fileMetadataCatalogs('tv',definitions('movies'),schema('tv')).ok,false);
 assert.equal(fileMetadataCatalogs('tv',definitions('tv'),schema('movies')).ok,false);
 assert.equal(fileMetadataCatalogs('tv',definitions('tv'),{media_type:'tv',choices:{}}).ok,false);
 assert.equal(fileMetadataCatalogs('tv',[...definitions('tv'),...definitions('tv')],schema('tv')).ok,false);
 for(const value of [[-2],[-1],[1,1],[53],[2],[NaN]]) assert.equal(build('tv',{languages:set(value)}).ok,false);
 for(const value of [-1,512,1.5,NaN]) assert.equal(build('tv',{indexer_flags:set(value)}).ok,false);
 assert.equal(build('movies',{indexer_flags:set(4095)}).ok,true);
 assert.equal(build('tv',{release_type:set(4)}).ok,false);
});
test('text preserves spaces and empty text and validates UTF8 bytes rather than code units',()=>{
 assert.deepEqual(build('tv',{release_group:set('  group  ')}),{ok:true,data:{release_group:'  group  '}});
 assert.equal(build('tv',{release_group:set('é'.repeat(512))}).ok,true);
 assert.equal(build('movies',{edition:set('😀'.repeat(256))}).ok,true);
 assert.equal(build('tv',{release_group:set('é'.repeat(513))}).ok,false);
 assert.equal(build('movies',{edition:set('bad\0text')}).ok,false);
});
test('readback requires every distinct requested file with exact domain and owner',()=>{
 for(const domain of ['tv','movies']) {
  const a=resource(domain,1),b=resource(domain,2);
  assert.equal(validateFileReadback(domain,9,[1,2],[b,a]).ok,true);
  for(const files of [[a],[a,a],[a,resource(domain,3)],[a,resource(domain,2,10)],[a,resource(domain==='tv'?'movies':'tv',2)],[a,{...b,languages:undefined}],[a,{...b,quality:{quality_id:7,revision:{version:0,real:0,is_repack:false}}}]]) assert.equal(validateFileReadback(domain,9,[1,2],files).ok,false);
  assert.equal(validateFileReadback(domain,9,[1,1],[a,a]).ok,false);
  assert.equal(validateFileReadback(domain,9,Array.from({length:26},(_,i)=>i+1),Array.from({length:26},(_,i)=>resource(domain,i+1))).ok,false);
  // An unfamiliar retained value is readable; editing it still requires a supported catalog choice.
  assert.equal(validateFileReadback(domain,9,[1],[{...a,quality:{quality_id:999,revision:null},languages:[999]}]).ok,true);
 }
});
test('mixed, unknown, empty and zero remain distinct without initializing replacement values',()=>{
 const a=resource('tv'),b=resource('tv',2);
 assert.deepEqual(mixedFileValue([a,b],'languages'),{kind:'value',value:null});
 assert.deepEqual(mixedFileValue([a,{...b,languages:[]}],'languages'),{kind:'mixed'});
 assert.deepEqual(mixedFileValue([{...a,languages:[1,0]},{...b,languages:[0,1]}],'languages'),{kind:'value',value:[1,0]});
 assert.equal(fileValueLabel(null),'Unknown'); assert.equal(fileValueLabel(''),'Empty text'); assert.equal(fileValueLabel([]),'Known empty'); assert.equal(fileValueLabel(0),'0');
});
