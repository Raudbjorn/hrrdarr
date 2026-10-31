import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {blankDraft,draftFromList,buildInput,buildExclusion,describeTest,describeRun,describeError,describeSearch,monitorOptions,domainFields,withDomain,supportedDomains,listStatus,formatTime,pollDelay,isRunActive,ERROR_MESSAGES,validHttpUrl,pageCount} from '../src/lib/import-list-draft.ts';
import {listImportLists,getImportListSchema,createImportList,updateImportList,deleteImportList,testImportListDraft,testSavedImportList,syncImportList,syncAllImportLists,listImportListRuns,listImportListItems,listImportListExclusions,addImportListExclusion,deleteImportListExclusion,bulkDeleteImportListExclusions} from '../src/lib/api.ts';
import {parseHash,hashFor,SETTINGS_SECTIONS,UNAVAILABLE_SECTIONS} from '../src/lib/routes.ts';

const field=(over)=>({name:'x',label:'X',kind:'text',required:false,secret:false,help:'',default:null,options:[],min:null,max:null,max_length:512,...over});
const custom={implementation:'custom',label:'Custom list',description:'',domains:['tv','movies'],default_interval_minutes:360,min_interval_minutes:5,fields:[
  field({name:'url',label:'List URL',kind:'url',required:true}),
  field({name:'bearer_token',label:'Bearer token',secret:true}),
]};
const trakt={implementation:'trakt',label:'Trakt',description:'',domains:['tv'],default_interval_minutes:720,min_interval_minutes:60,fields:[
  field({name:'access_token',label:'Access token',secret:true,required:true}),
  field({name:'limit',label:'Limit',kind:'number',min:1,max:100,default:20}),
  field({name:'kind',label:'Kind',kind:'select',options:['watchlist','popular'],default:'watchlist'}),
  field({name:'flag',label:'Flag',kind:'boolean',default:false}),
]};
const ID='11111111-1111-4111-8111-111111111111';
async function withFetch(stub,run){const original=globalThis.fetch;globalThis.fetch=stub;try{await run();}finally{globalThis.fetch=original;}}
const json=(value,status=200)=>new Response(JSON.stringify(value),{status});
function valid(media='tv'){const d=blankDraft(custom,media);d.settings.url='http://feeds.example/list';d.qualityProfileId='3';d.rootFolderId='4';return d;}

test('blank drafts use implementation defaults and the chosen domain',()=>{
  const d=blankDraft(trakt);
  assert.equal(d.media,'tv');assert.equal(d.intervalMinutes,720);assert.equal(d.settings.limit,20);assert.equal(d.settings.kind,'watchlist');
  assert.equal(d.enableAutomaticAdd,true);assert.equal(d.searchOnAdd,false);assert.deepEqual(d.secrets,{});
  assert.equal(blankDraft(custom,'movies').media,'movies');
  assert.deepEqual(supportedDomains(trakt),['tv']);
});

test('monitor options and visible controls are domain scoped',()=>{
  assert.deepEqual(monitorOptions('tv').map(o=>o.value),['all','none','first_season','last_season']);
  assert.deepEqual(monitorOptions('movies').map(o=>o.value),['all','none']);
  assert.deepEqual(domainFields('tv'),{seriesType:true,seasonFolder:true,minimumAvailability:false});
  assert.deepEqual(domainFields('movies'),{seriesType:false,seasonFolder:false,minimumAvailability:true});
  const tv=valid();tv.monitor='first_season';tv.tagIds=[1];
  const m=withDomain(tv,'movies');
  assert.equal(m.monitor,'all');assert.deepEqual(m.tagIds,[]);assert.equal(m.qualityProfileId,'');assert.equal(m.rootFolderId,'');
  assert.equal(withDomain(tv,'tv'),tv);
});

test('buildInput emits domain-specific fields only',()=>{
  const tv=buildInput(valid('tv'),custom);
  assert.equal(tv.ok,true,tv.error);
  assert.equal(tv.data.series_type,'standard');assert.equal(tv.data.season_folder,true);assert.equal('minimum_availability' in tv.data,false);
  assert.equal(tv.data.media_type,'tv');assert.equal(tv.data.quality_profile_id,3);assert.equal(tv.data.root_folder_id,4);assert.equal(tv.data.interval_minutes,360);
  const mv=buildInput(valid('movies'),custom);
  assert.equal(mv.ok,true);
  assert.equal(mv.data.minimum_availability,'released');assert.equal('series_type' in mv.data,false);assert.equal('season_folder' in mv.data,false);
  const bad=valid('movies');bad.monitor='first_season';
  assert.match(buildInput(bad,custom).error,/Movie lists monitor/);
});

test('buildInput mirrors server validation',()=>{
  const cases=[
    [d=>{d.name='';},/Name requires/],[d=>{d.name=' x';},/Name requires/],[d=>{d.name='a'.repeat(129);},/Name requires/],[d=>{d.name='a\nb';},/Name requires/],
    [d=>{d.intervalMinutes=4;},/Interval must/],[d=>{d.intervalMinutes=43201;},/Interval must/],[d=>{d.intervalMinutes=5.5;},/Interval must/],[d=>{d.intervalMinutes='';},/Interval must/],
    [d=>{d.qualityProfileId='';},/Automatic add requires/],[d=>{d.rootFolderId='';},/Automatic add requires/],[d=>{d.rootFolderId='-1';},/must be positive/],
    [d=>{d.tagIds=[1,1];},/distinct/],[d=>{d.tagIds=[0];},/distinct/],
    [d=>{d.settings.url='';},/List URL is required/],[d=>{d.settings.url='ftp://x';},/http\(s\) URL/],[d=>{d.settings.url='http://u:p@host/x';},/without credentials/],[d=>{d.settings.url='http://h/x#frag';},/fragment/],[d=>{d.settings.url=' http://h/x';},/surrounding whitespace/],
    [d=>{d.secrets.bearer_token=' tok';},/surrounding whitespace/],
  ];
  for(const [mutate,pattern] of cases){const d=valid();mutate(d);const r=buildInput(d,custom);assert.equal(r.ok,false,String(pattern));assert.match(r.error,pattern);}
  // record-only lists need no profile or root; disabled lists neither
  const record=valid();record.enableAutomaticAdd=false;record.qualityProfileId='';record.rootFolderId='';assert.equal(buildInput(record,custom).ok,true);
  const off=valid();off.enabled=false;off.qualityProfileId='';assert.equal(buildInput(off,custom).ok,true);
  assert.equal(validHttpUrl('https://a.example:8080/x?y=1'),true);
  const unsupported=valid('movies');assert.match(buildInput(unsupported,trakt).error,/does not support the Movies domain/);
});

test('number select and boolean settings are validated and typed',()=>{
  const d=blankDraft(trakt);d.secrets.access_token='t';d.qualityProfileId='1';d.rootFolderId='1';
  let r=buildInput(d,trakt);assert.equal(r.ok,true,r.error);assert.deepEqual(r.data.settings,{limit:20,kind:'watchlist',flag:false});
  d.settings.limit='50';r=buildInput(d,trakt);assert.equal(r.data.settings.limit,50);
  for(const limit of [0,101,1.5]){d.settings.limit=limit;assert.match(buildInput(d,trakt).error,/whole number/);}
  d.settings.limit=20;d.settings.kind='nope';assert.match(buildInput(d,trakt).error,/must be one of/);
  d.settings.kind='popular';d.settings.flag=true;assert.equal(buildInput(d,trakt).data.settings.flag,true);
  d.interval_minutes=undefined;d.intervalMinutes=59;assert.match(buildInput(d,trakt).error,/from 60/);
});

test('secrets are write-only: blank keeps, typed replaces, clear nulls, none are echoed',()=>{
  const d=valid();
  let r=buildInput(d,custom);assert.deepEqual(r.data.secrets,{});assert.equal('bearer_token' in r.data.settings,false);
  d.secrets.bearer_token='tok-123';r=buildInput(d,custom);assert.deepEqual(r.data.secrets,{bearer_token:'tok-123'});assert.equal(JSON.stringify(r.data.settings).includes('tok-123'),false);
  d.secrets={};d.cleared=['bearer_token'];r=buildInput(d,custom,['bearer_token']);assert.deepEqual(r.data.secrets,{bearer_token:null});
  // typed value wins over clear
  d.secrets.bearer_token='new';r=buildInput(d,custom,['bearer_token']);assert.deepEqual(r.data.secrets,{bearer_token:'new'});
  // required secret: stored satisfies a blank edit, clearing does not, create needs a value
  const t=blankDraft(trakt);t.qualityProfileId='1';t.rootFolderId='1';
  assert.match(buildInput(t,trakt).error,/Access token is required/);
  assert.equal(buildInput(t,trakt,['access_token']).ok,true);
  assert.deepEqual(buildInput(t,trakt,['access_token']).data.secrets,{});
  t.cleared=['access_token'];assert.match(buildInput(t,trakt,['access_token']).error,/Access token is required/);
});

test('drafts from saved lists never receive secret values',()=>{
  const list={id:ID,name:'My list',implementation:'custom',media_type:'movies',enabled:true,enable_automatic_add:false,search_on_add:true,monitor:'none',quality_profile_id:7,root_folder_id:null,minimum_availability:'in_cinemas',series_type:null,season_folder:null,interval_minutes:60,tag_ids:[4],revision:5,settings:{url:'http://h/x'},secrets_configured:['bearer_token'],last_sync_at:null,last_success_at:null,last_error:null,created_at:1,updated_at:1};
  const d=draftFromList(list,custom);
  assert.deepEqual(d.secrets,{});assert.deepEqual(d.cleared,[]);
  assert.equal(d.media,'movies');assert.equal(d.revision,5);assert.equal(d.qualityProfileId,'7');assert.equal(d.rootFolderId,'');
  assert.equal(d.minimumAvailability,'in_cinemas');assert.equal(d.enableAutomaticAdd,false);assert.deepEqual(d.tagIds,[4]);
  assert.equal(JSON.stringify(d).includes('bearer'),false);
  const r=buildInput(d,custom,list.secrets_configured);
  assert.equal(r.ok,true,r.error);assert.equal(r.data.revision,5);assert.deepEqual(r.data.secrets,{});
});

test('error classes have plain-language messages and unknown classes stay generic',()=>{
  for(const code of ['authentication','rate_limited','timeout','transport','server_error','rejected','redirect','host_denied','invalid_response','resource_limit','invalid_settings','unsupported','not_configured','credentials_locked'])
    assert.ok(ERROR_MESSAGES[code]&&!ERROR_MESSAGES[code].includes('_'),code);
  assert.match(describeError('invalid_response'),/not a valid list/);
  assert.match(describeError('something_new'),/unrecognised/);
  assert.equal(describeError(null),'');
});

test('test and run outcomes are summarised without remote content',()=>{
  assert.match(describeTest({ok:true,item_count:2,error:null}),/2 items found/);
  assert.match(describeTest({ok:true,item_count:1,error:null}),/1 item found/);
  assert.match(describeTest({ok:false,item_count:null,error:'authentication'}),/Test failed: .*credentials/);
  const run={id:ID,list_id:ID,media_type:'tv',origin:'manual',status:'succeeded',fetched:true,attempts:1,items_fetched:2,added:2,existing:0,excluded:0,unresolved:0,failed:0,pending:0,error_code:null,created_at:1,started_at:1,completed_at:2};
  assert.match(describeRun(run),/Succeeded: fetched 2, added 2/);
  assert.match(describeRun({...run,status:'queued'}),/Queued/);assert.match(describeRun({...run,status:'running',pending:1}),/Running.*pending 1/);
  assert.match(describeRun({...run,status:'failed',error_code:'invalid_response'}),/Failed: .*not a valid list/);
  assert.equal(isRunActive({status:'queued'}),true);assert.equal(isRunActive({status:'running'}),true);assert.equal(isRunActive({status:'failed'}),false);assert.equal(isRunActive(undefined),false);
});

test('list status, time formatting and polling back-off are bounded',()=>{
  assert.equal(listStatus({last_sync_at:null,last_error:null,last_success_at:null}),'Not synced yet');
  assert.match(listStatus({last_sync_at:1700000000,last_error:'timeout',last_success_at:null}),/failed: .*too long/);
  assert.match(listStatus({last_sync_at:1700000000,last_error:null,last_success_at:1700000000}),/2023-11-14 22:13 UTC succeeded/);
  assert.equal(formatTime(0),'never');assert.equal(formatTime(null),'never');
  assert.equal(pollDelay(0),1000);assert.ok(pollDelay(10)>1000);assert.equal(pollDelay(100000),5000);
  assert.equal(pageCount(0,25),1);assert.equal(pageCount(26,25),2);
});

test('exclusion form validation mirrors the server',()=>{
  const ok=buildExclusion('movies','202','Search movie 202','2021');
  assert.deepEqual(ok,{ok:true,data:{media_type:'movies',external_id:202,title:'Search movie 202',year:2021}});
  assert.equal('year' in buildExclusion('tv','5','X','').data,false);
  for(const [id,title,year] of [['0','X',''],['abc','X',''],['1.5','X',''],['9007199254740993','X',''],['1','',''],['1',' X',''],['1','a\tb',''],['1','X','0'],['1','X','10000'],['1','X','20x']])
    assert.equal(buildExclusion('tv',id,title,year).ok,false,`${id}/${title}/${year}`);
});

test('client stubs use the documented routes and methods',async()=>{
  const calls=[];
  await withFetch(async(url,init)=>{calls.push([String(url),init?.method??'GET',init?.body?JSON.parse(init.body):undefined]);return init?.method==='DELETE'?new Response(null,{status:204}):json({items:[],runs:[],skipped:0});},async()=>{
    await listImportLists();await getImportListSchema();await createImportList({name:'a'});await updateImportList(ID,{name:'a'});await deleteImportList(ID);
    await testImportListDraft({draft:{name:'a'},existing_id:ID});await testSavedImportList(ID);await syncImportList(ID);await syncAllImportLists();
    await listImportListRuns(ID,3);await listImportListItems(ID,{state:'added',limit:10,offset:20});await listImportListItems(ID);
    await listImportListExclusions({media_type:'movies',limit:5,offset:10});await listImportListExclusions();
    await addImportListExclusion({media_type:'tv',external_id:5,title:'X'});await deleteImportListExclusion(9);await bulkDeleteImportListExclusions([1,2]);
  });
  assert.deepEqual(calls.map(c=>c.slice(0,2)),[
    ['/api/v1/importlist','GET'],['/api/v1/importlist/schema','GET'],['/api/v1/importlist','POST'],[`/api/v1/importlist/${ID}`,'PUT'],[`/api/v1/importlist/${ID}`,'DELETE'],
    ['/api/v1/importlist/test','POST'],[`/api/v1/importlist/${ID}/test`,'POST'],[`/api/v1/importlist/${ID}/sync`,'POST'],['/api/v1/importlist/sync','POST'],
    [`/api/v1/importlist/${ID}/runs?limit=3`,'GET'],[`/api/v1/importlist/${ID}/items?state=added&limit=10&offset=20`,'GET'],[`/api/v1/importlist/${ID}/items?limit=50&offset=0`,'GET'],
    ['/api/v1/importlist/exclusions?media_type=movies&limit=5&offset=10','GET'],['/api/v1/importlist/exclusions?limit=50&offset=0','GET'],
    ['/api/v1/importlist/exclusions','POST'],['/api/v1/importlist/exclusions/9','DELETE'],['/api/v1/importlist/exclusions/bulk','DELETE']]);
  assert.deepEqual(calls[5][2],{draft:{name:'a'},existing_id:ID});
  assert.deepEqual(calls[16][2],{ids:[1,2]});
});

test('client stubs reject malformed ids and arguments without a request',async()=>{
  let n=0;
  await withFetch(async()=>{n++;return json({});},async()=>{
    for(const r of [
      await updateImportList('nope',{}),await deleteImportList('../x'),await testSavedImportList('1'),await syncImportList('x'),
      await testImportListDraft({draft:{},existing_id:'bad'}),await listImportListRuns('x'),await listImportListRuns(ID,0),await listImportListRuns(ID,33),
      await listImportListItems('x'),await listImportListItems(ID,{state:'weird'}),await listImportListItems(ID,{limit:501}),await listImportListItems(ID,{offset:-1}),
      await listImportListExclusions({media_type:'music'}),await listImportListExclusions({limit:0}),
      await deleteImportListExclusion(0),await deleteImportListExclusion(1.5),await bulkDeleteImportListExclusions([]),await bulkDeleteImportListExclusions([1,1]),await bulkDeleteImportListExclusions([1,-2]),
      await bulkDeleteImportListExclusions(Array.from({length:501},(_,i)=>i+1)),
    ]) assert.equal(r.ok,false);
  });
  assert.equal(n,0);
});

test('server errors surface as results with the stable code',async()=>{
  await withFetch(async()=>json({error:{code:'import_list_not_found',message:'Import list does not exist'}},404),async()=>{
    const r=await syncImportList(ID);
    assert.equal(r.ok,false);assert.equal(r.error,'Import list does not exist');assert.equal(r.status,404);assert.equal(r.code,'import_list_not_found');
  });
});

test('the import lists route is listed under settings and no longer unavailable',()=>{
  assert.deepEqual(parseHash(hashFor('import-lists')),{kind:'view',view:'import-lists'});
  assert.equal(hashFor('import-lists'),'#/settings/lists');
  assert.ok(SETTINGS_SECTIONS.some(s=>s.view==='import-lists'&&s.label==='Import lists'));
  assert.equal(UNAVAILABLE_SECTIONS.includes('Import lists'),false);
});

test('the panel and app wire the route without innerHTML',()=>{
  const panel=readFileSync(new URL('../src/lib/ImportListsPanel.svelte',import.meta.url),'utf8');
  const app=readFileSync(new URL('../src/App.svelte',import.meta.url),'utf8');
  assert.equal(/innerHTML|\{@html/.test(panel),false);
  assert.match(app,/importListsVisited/);assert.match(app,/<ImportListsPanel \/>/);
  assert.match(panel,/Configured \(leave blank to keep\)/);
});


test('search admission distinguishes blocked requests from submitted work without claiming a download',()=>{
  assert.equal(describeSearch(null,null),'');
  assert.match(describeSearch('pending','no_search_indexer'),/Enable an automatic-search indexer/);
  assert.match(describeSearch('pending','command_history_full'),/retry when capacity/);
  assert.equal(describeSearch('admitted',null),'Search submitted. Check Activity for results.');
});
