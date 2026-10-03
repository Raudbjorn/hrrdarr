import test from 'node:test';
import assert from 'node:assert/strict';
import {buildHistoryQuery,emptyHistoryForm,parseTimestamp,describeEvent,describeHistoryPage,describeHistoryFailure,pageNavigation,NOT_RECORDED,HISTORY_NOTES} from '../src/lib/history-view.ts';
import {getHistory} from '../src/lib/api.ts';

const form=over=>({...emptyHistoryForm(),...over});
const built=over=>buildHistoryQuery(form(over));
const errors=over=>{const r=built(over);assert.equal(r.ok,false);return r.errors.join(' | ');};

test('empty form uses default limit and offset',()=>{
  const r=built({});assert.equal(r.ok,true);
  assert.deepEqual(r.query,{limit:50,offset:0});assert.equal(r.search,'limit=50&offset=0');
});
test('timezone plus is percent-encoded and fractions are kept verbatim',()=>{
  const r=built({from:'2026-01-02T03:04:05.100+02:00',to:'2026-01-02T03:04:05.1234Z'});
  assert.equal(r.ok,true);
  assert.match(r.search,/from=2026-01-02T03%3A04%3A05\.100%2B02%3A00/);
  assert.ok(!r.search.includes('+0'));
});
test('timezone-less, malformed and leap-second timestamps are rejected',()=>{
  assert.match(errors({from:'2026-01-02T03:04:05'}),/explicit timezone/);
  assert.match(errors({to:'2026-01-02'}),/RFC3339/);
  assert.match(errors({from:'2026-01-02 03:04:05Z'}),/RFC3339/);
  assert.match(errors({from:'2026-01-02T03:04:60Z'}),/leap/);
  assert.match(errors({from:'2026-02-30T03:04:05Z'}),/calendar/);
  assert.match(errors({from:'2026-01-02T03:04:05.1234567891Z'}),/RFC3339/);
  assert.match(errors({from:'0001-01-01T00:00:00+01:00'}),/year/);
});
test('from must be strictly before to, comparing across offsets and fractions',()=>{
  assert.match(errors({from:'2026-01-02T00:00:00Z',to:'2026-01-02T00:00:00Z'}),/earlier/);
  assert.match(errors({from:'2026-01-02T02:00:00+02:00',to:'2026-01-02T00:00:00Z'}),/earlier/);
  assert.match(errors({from:'2026-01-02T00:00:00.5Z',to:'2026-01-02T00:00:00.4999Z'}),/earlier/);
  assert.equal(built({from:'2026-01-02T00:00:00.1Z',to:'2026-01-02T00:00:00.100000001Z'}).ok,true);
  assert.equal(built({from:'2026-01-02T00:00:00Z'}).ok,true);
});
test('parseTimestamp normalises offsets',()=>{
  const a=parseTimestamp('2026-01-02T02:00:00+02:00'),b=parseTimestamp('2026-01-02T00:00:00z');
  assert.equal(a.ok&&b.ok&&a.seconds===b.seconds,true);
});
test('season requires series; zero season is allowed with series',()=>{
  assert.match(errors({season:'1'}),/Season requires a Series ID/);
  const r=built({seriesId:'3',season:'0'});assert.equal(r.ok,true);assert.equal(r.query.season,0);
  assert.match(errors({seriesId:'3',season:'-1'}),/Season/);
});
test('movie and TV selectors cannot be combined',()=>{
  for(const tv of [{episodeId:'1'},{seriesId:'1'},{seriesId:'1',season:'1'}])assert.match(errors({movieId:'2',...tv}),/Movie ID cannot be combined/);
  assert.match(errors({movieId:'2',mediaType:'tv'}),/media type TV/);
  assert.match(errors({episodeId:'2',mediaType:'movies'}),/cannot be combined with media type Movies/);
  assert.match(errors({seriesId:'2',mediaType:'movies'}),/media type Movies/);
  assert.equal(built({movieId:'2',mediaType:'movies'}).ok,true);
  assert.equal(built({episodeId:'2',seriesId:'1',season:'1',mediaType:'tv'}).ok,true);
});
test('a blank-looking invalid selector still counts as a conflicting selector',()=>{
  assert.match(errors({movieId:'5',episodeId:'abc'}),/must be a positive whole number/);
});
test('ids must be positive safe integers',()=>{
  for(const bad of ['0','-1','1.5','abc','9007199254740993','1e3'])assert.match(errors({movieId:bad}),/Movie ID/);
});
test('limit and offset bounds',()=>{
  assert.equal(built({limit:'1',offset:'0'}).ok,true);assert.equal(built({limit:'100',offset:'10000'}).ok,true);
  for(const l of ['0','101','-1','x','1.5'])assert.match(errors({limit:l}),/Limit/);
  for(const o of ['10001','-1','x'])assert.match(errors({offset:o}),/Offset/);
});
test('multiple errors are all reported',()=>{
  const r=built({season:'1',limit:'0',from:'nope'});assert.equal(r.ok,false);assert.equal(r.errors.length,3);
});

const source=over=>({origin:'source_snapshot',id:{application:'sonarr',fingerprint:'f'.repeat(8),source_id:7},event_type:'file_renamed',source_event_type:6,
  target:{media_type:'episode',id:5},occurred_at:'2026-01-02T03:04:05.1Z',source_title:null,download_id:null,quality:null,languages:null,...over});
const native=over=>({origin:'native_import',id:'op-1',event_type:'file_imported',target:{media_type:'movie',id:5},file:{media_type:'movies',id:5},
  source:'/in/a.mkv',destination:'/lib/a.mkv',size_bytes:123,sha256:'ab'.repeat(32),imported_at:'2026-01-02T03:04:05Z',...over});

test('native events render paths, size, hash and domain-qualified ids',()=>{
  const v=describeEvent(native());
  assert.equal(v.origin,'native_import');assert.equal(v.domain,'Movies');assert.equal(v.target,'Movie 5');
  assert.match(v.timeLabel,/Association committed/);
  const f=Object.fromEntries(v.facts.map(x=>[x.label,x.value]));
  assert.equal(f['Size (bytes)'],'123');assert.equal(f['Historical file'],'Movies file 5');assert.equal(f['Destination path (as recorded)'],'/lib/a.mkv');
  assert.equal(v.sourceEventType,null);assert.deepEqual(v.warnings,[]);
});
test('equal numeric ids never merge targets or keys across domains',()=>{
  const tv=describeEvent(native({target:{media_type:'episode',id:5},file:{media_type:'tv',id:5}}));
  const mv=describeEvent(native({id:'op-2'}));
  assert.equal(tv.target,'TV episode 5');assert.equal(mv.target,'Movie 5');
  const a=describeEvent(source()),b=describeEvent(source({id:{application:'radarr',fingerprint:'f'.repeat(8),source_id:7},target:{media_type:'movie',id:5}}));
  assert.notEqual(a.key,b.key);
});
test('mismatched native file domain is flagged',()=>{
  assert.equal(describeEvent(native({file:{media_type:'tv',id:5}})).warnings.length,1);
});
test('source events have no paths and show not recorded for null facts',()=>{
  const v=describeEvent(source());
  assert.equal(v.origin,'source_snapshot');assert.match(v.originLabel,/Sonarr/);
  const labels=v.facts.map(f=>f.label).join('|');
  assert.ok(!/path|SHA|Size \(bytes\)/i.test(labels.replace('Paths, size, hash, local import time','')));
  for(const l of ['Source title','Download ID','Quality','Languages'])assert.equal(v.facts.find(f=>f.label===l).value,NOT_RECORDED);
  assert.match(v.timeLabel,/source/);
});
test('source facts render when present, including empty language list',()=>{
  const v=describeEvent(source({source_title:'T',download_id:'D1',quality:{quality_id:7,revision:{version:2,real:1,is_repack:true}},languages:[1,2]}));
  const f=Object.fromEntries(v.facts.map(x=>[x.label,x.value]));
  assert.equal(f['Source title'],'T');assert.equal(f['Quality'],'Quality ID 7, revision v2, real 1, repack');assert.equal(f['Languages'],'ID 1, ID 2');
  assert.equal(describeEvent(source({languages:[]})).facts.find(x=>x.label==='Languages').value,'none');
  assert.match(describeEvent(source({quality:{quality_id:3,revision:null}})).facts.find(x=>x.label==='Quality').value,/revision not recorded/);
});
test('source code 6 is a rename for TV and a deletion for movies; semantic type and code both shown',()=>{
  const tv=describeEvent(source());
  assert.equal(tv.eventLabel,'File renamed');assert.equal(tv.sourceEventType,'6');assert.match(tv.sourceCodeNote,/rename for TV/);
  const mv=describeEvent(source({id:{application:'radarr',fingerprint:'abcd1234',source_id:8},event_type:'file_deleted',target:{media_type:'movie',id:9}}));
  assert.equal(mv.eventLabel,'File deleted');assert.equal(mv.sourceEventType,'6');assert.match(mv.sourceCodeNote,/deletion for movies/);
  assert.equal(describeEvent(source({source_event_type:1,event_type:'grabbed'})).sourceCodeNote,null);
});
test('page view validates shape and computes range and navigation',()=>{
  const ok=describeHistoryPage({items:[native(),source()],total:120,limit:50,offset:50});
  assert.equal(ok.kind,'ready');assert.equal(ok.view.rangeLabel,'Showing 51–52 of 120');
  assert.equal(ok.view.prevOffset,0);assert.equal(ok.view.nextOffset,100);
  assert.equal(describeHistoryPage({items:[],total:0,limit:50,offset:0}).view.rangeLabel,'No events');
  for(const bad of [null,{},{items:[{origin:'x',target:{media_type:'movie',id:1}}],total:1,limit:1,offset:0},{items:[],total:-1,limit:1,offset:0},
    {items:[{...native(),target:{media_type:'series',id:1}}],total:1,limit:1,offset:0}])
    assert.equal(describeHistoryPage(bad).kind,'error');
});
test('pagination stops at the offset cap',()=>{
  assert.deepEqual(pageNavigation(30000,100,9900),{prevOffset:9800,nextOffset:10000,beyondOffsetCap:false});
  assert.deepEqual(pageNavigation(30000,100,10000),{prevOffset:9900,nextOffset:null,beyondOffsetCap:true});
  assert.deepEqual(pageNavigation(10,50,0),{prevOffset:null,nextOffset:null,beyondOffsetCap:false});
});
test('error codes map to retry advice',()=>{
  const bad=describeHistoryFailure({error:'bad',code:'invalid_history_query',status:400});
  assert.equal(bad.retryable,false);assert.equal(bad.code,'invalid_history_query');assert.match(bad.advice,/Change the filters/);
  const big=describeHistoryFailure({error:'big',code:'history_response_limit',status:413});
  assert.equal(big.retryable,false);assert.match(big.advice,/Narrow/);
  const slow=describeHistoryFailure({error:'slow',code:'history_timeout',status:503});
  assert.equal(slow.retryable,true);assert.match(slow.advice,/five-second/);
  const ise=describeHistoryFailure({error:'x',code:'internal',status:500});
  assert.equal(ise.retryable,true);assert.equal(ise.status,500);
  const net=describeHistoryFailure({error:'timeout'});
  assert.equal(net.code,'unknown');assert.equal(net.retryable,true);
});
test('limits wording states commit semantics, path caveat and non-snapshot paging',()=>{
  const text=HISTORY_NOTES.join(' ');
  assert.match(text,/association was committed/);assert.match(text,/cleanup/);assert.match(text,/currently exists/);assert.match(text,/not a frozen snapshot/);
});
test('getHistory sends a GET with encoded plus and surfaces envelope codes',async()=>{
  const old=globalThis.fetch,calls=[];
  try{
    globalThis.fetch=async(path,options)=>{calls.push({path,options});return new Response(JSON.stringify({error:{code:'history_timeout',message:'slow'}}),{status:503});};
    const q=built({from:'2026-01-02T03:04:05+02:00'}).query;
    const r=await getHistory(q);
    assert.match(calls[0].path,/^\/api\/v1\/history\?/);assert.match(calls[0].path,/%2B02%3A00/);
    assert.equal(calls[0].options.method,undefined);assert.equal(calls[0].options.body,undefined);
    assert.equal(r.ok,false);assert.equal(r.code,'history_timeout');assert.equal(r.status,503);
    await getHistory();assert.equal(calls[1].path,'/api/v1/history');
  }finally{globalThis.fetch=old;}
});
