// Owned library_ui_fixture only: real HTTP, two independent mock clients, no fabricated receipts.
import assert from 'node:assert/strict';
import {realpath,stat} from 'node:fs/promises';
import {basename,isAbsolute,join} from 'node:path';
import {pathToFileURL} from 'node:url';
const {chromium}=await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE||'/usr/lib/node_modules/playwright/index.mjs').href);
const origin=process.env.UI_URL,primaryOrigin=process.env.UI_PROVIDER_ORIGIN,alternateOrigin=process.env.UI_ALTERNATE_PROVIDER_ORIGIN;
for(const value of [origin,primaryOrigin,alternateOrigin])assert.equal(new URL(value).hostname,'127.0.0.1');
assert.notEqual(primaryOrigin,alternateOrigin,'Client mocks must be independent');
assert.ok(process.env.UI_SCRATCH&&isAbsolute(process.env.UI_SCRATCH),'Owned UI_SCRATCH is required');
const scratch=await realpath(process.env.UI_SCRATCH);assert.ok(basename(scratch).startsWith('hrrdarr-library-ui-'));
for(const domain of ['tv','movies'])assert.ok((await stat(join(scratch,`search-${domain}-202`))).isDirectory());
const browser=await chromium.launch({headless:true,...(process.env.PLAYWRIGHT_EXECUTABLE?{executablePath:process.env.PLAYWRIGHT_EXECUTABLE}:{})});
const page=await browser.newPage();page.setDefaultTimeout(20000);
const errors=[];page.on('pageerror',error=>errors.push(error.message));
page.on('dialog',dialog=>{errors.push(`Unexpected dialog: ${dialog.message()}`);void dialog.dismiss();});
const nav=page.getByRole('navigation',{name:'Workspace'}),editor=page.getByRole('article',{name:'Provider editor'});
const search=page.getByRole('region',{name:'Search and download',exact:true}),rss=page.getByRole('region',{name:'RSS automation',exact:true});
async function request(base,method,path,data,status=200){
 const response=await page.request[method](base+path,{...(data===undefined?{}:{data}),timeout:10000});
 assert.equal(response.status(),status,await response.text());return status===204?null:response.json();
}
const api=(method,path,data,status)=>request(origin,method,path,data,status);
async function until(read,accept,label,timeoutMs=45000){
 const deadline=Date.now()+timeoutMs;let last;
 while(Date.now()<deadline){last=await read();if(accept(last))return last;await page.waitForTimeout(100);}
 assert.fail(`${label}: ${JSON.stringify(last)}`);
}
async function ready(locator){await locator.waitFor();await until(()=>locator.isEnabled(),Boolean,'Control remained disabled');}
async function observations(base){const rows=await request(base,'get','/fixture-observations');assert.ok(rows.length<256,'Fixture observations truncated');return rows;}
const addCount=rows=>rows.filter(row=>row.path==='/api/v2/torrents/add').length;
const libraryPath=domain=>domain==='tv'?'/api/v1/tv/series':'/api/v1/movies';
async function edit(provider){
 await nav.getByRole('button',{name:'Providers',exact:true}).click();
 const list=page.getByRole('complementary',{name:'Saved providers'});
 await list.getByRole('button',{name:'Refresh providers',exact:true}).click();
 await list.getByRole('button',{name:new RegExp(`^${provider.name} `)}).click();
 await editor.getByRole('heading',{name:provider.name,exact:true}).waitFor();await ready(editor.getByLabel('Name',{exact:true}));
}
async function saveProvider(){
 await ready(editor.getByRole('button',{name:'Save provider',exact:true}));
 const saved=page.waitForResponse(r=>r.request().method()==='PUT'&&new URL(r.url()).pathname.startsWith('/api/v1/providers/'));
 await editor.getByRole('button',{name:'Save provider',exact:true}).click();assert.equal((await saved).status(),200);
 await ready(editor.getByLabel('Name',{exact:true}));
}
async function addLibrary(domain,external=202){
 await nav.getByRole('button',{name:'Library',exact:true}).click();
 await page.getByRole('navigation',{name:'Library type'}).getByRole('button',{name:domain==='tv'?'TV':'Movies',exact:true}).click();
 await page.getByRole('button',{name:domain==='tv'?'Add series':'Add movie',exact:true}).click();
 await page.getByLabel('Search catalogue',{exact:true}).fill(`Search ${external}`);await page.getByRole('button',{name:'Search',exact:true}).click();
 const title=`Search ${domain==='tv'?'series':'movie'} ${external}`;
 await page.getByRole('button',{name:new RegExp(`^${title} `)}).click();
 await page.getByLabel('Existing library directory',{exact:true}).fill(join(scratch,`search-${domain}-${external}`));
 await page.getByRole('button',{name:'Add to library',exact:true}).click();
 await page.getByRole('article',{name:'Library detail'}).getByRole('heading',{name:title,exact:true}).waitFor();
 const item=(await api('get',libraryPath(domain))).items.find(row=>row.title===title);assert.ok(item);
 const profile=await api('post',`/api/v1/${domain}/quality-profiles`,{name:`Selection browser ${domain} ${external}`,items:[{kind:'quality',quality_id:5,allowed:true,min_size:0},{kind:'quality',quality_id:3,allowed:true,min_size:0}],policy:{upgrade_allowed:true,cutoff:{kind:'quality',quality_id:3},min_format_score:0,cutoff_format_score:0,min_upgrade_format_score:1,language_id:domain==='movies'?-2:null,format_items:[]}},201);
 await api('put',`${libraryPath(domain)}/${item.id}`,{quality_profile_id:profile.id,...(domain==='tv'?{series_type:'standard',use_scene_numbering:false}:{minimum_availability:'released'})});
}
async function openSearch(domain,indexer,clientId='',external=202){
 await nav.getByRole('button',{name:'Library',exact:true}).click();
 await page.getByRole('navigation',{name:'Library type'}).getByRole('button',{name:domain==='tv'?'TV':'Movies',exact:true}).click();
 await page.getByRole('complementary',{name:'Library collection'}).getByRole('button',{name:new RegExp(`^Search ${domain==='tv'?'series':'movie'} ${external}`)}).click();
 await page.getByRole('button',{name:domain==='tv'?'Search releases for Search pilot':'Search movie releases',exact:true}).click();
 await ready(search.getByLabel(/^Download search indexer/));await search.getByLabel(/^Download search indexer/).selectOption(indexer.id);
 await ready(search.getByLabel(/^Search download client/));await search.getByLabel(/^Search download client/).selectOption(clientId);
 assert.equal(await search.getByLabel(/^Search download client/).inputValue(),clientId);
}
async function automaticSearch(domain,indexer,clientId='',external=202,accept=value=>['observed','rejected','needs_attention'].includes(value.selected_candidate_status)){
 await openSearch(domain,indexer,clientId,external);
 const admitted=page.waitForResponse(r=>r.request().method()==='POST'&&new URL(r.url()).pathname==='/api/v1/search/commands');
 await search.getByRole('button',{name:'Search and download best match',exact:true}).click();
 const response=await admitted;assert.equal(response.status(),202,await response.text());const command=await response.json();
 assert.equal(command.source.selection_kind,clientId?'explicit':'default');assert.equal(command.source.client_id,clientId||null);assert.equal(command.source.media_type,domain);
 return until(()=>api('get',`/api/v1/search/commands/${command.id}`),accept,`Search ${command.id} download did not settle`);
}
async function showAttempts(command,expected){
 await search.getByRole('button',{name:'Check search status',exact:true}).click();
 const attempts=search.getByRole('list',{name:'Selected download client attempts'});
 for(const [id,state,label] of expected){
  const attempt=command.selected_client_attempts.find(value=>value.client_id===id);assert.ok(attempt);assert.equal(attempt.state,state);
  await attempts.getByText(new RegExp(`^Client ${id}: ${label} · Round ${attempt.selection_round}`)).waitFor();
 }
 assert.equal(command.selected_client_attempts.length,expected.length);assert.equal(await attempts.getByRole('listitem').count(),expected.length);
}
try{
 for(const mock of [primaryOrigin,alternateOrigin])await request(mock,'post','/fixture-search',undefined,204);
 const download=category=>({category,imported_category:null,recent_priority:0,older_priority:0,tags:[]});
 const createClient=(name,endpoint,priority)=>api('post','/api/v1/providers',{name,enabled:true,priority,settings:{implementation:'qbittorrent',endpoint,tv:download('tv'),movies:download('movies')},credentials:{kind:'username_password',username:'fixture-user',password:'fixture-good'}},201);
 const primary=await createClient('Selection primary',primaryOrigin,1),alternate=await createClient('Selection alternate',alternateOrigin,2);
 const setEnabled=async(provider,enabled)=>{const saved=await api('get',`/api/v1/providers/${provider.id}`);return api('put',`/api/v1/providers/${provider.id}`,{revision:saved.revision,name:saved.name,enabled,priority:saved.priority,settings:saved.settings});};
 const indexer=await api('post','/api/v1/providers',{name:'Selection indexer',enabled:true,priority:1,settings:{implementation:'torznab',endpoint:`${primaryOrigin}/torznab`,tv:{categories:[5030],anime_categories:[5070]},movies:{categories:[2000]}},credentials:{kind:'api_key',api_key:'fixture-good'}},201);
 const tags={};for(const domain of ['tv','movies'])tags[domain]=await api('post',`/api/v1/${domain}/tags`,{label:`selection-${domain}`},201);
 await page.goto(origin);for(const domain of ['tv','movies'])await addLibrary(domain);
 // Persist separate native tags; display their real reverse references before clearing them.
 await edit(primary);
 for(const [domain,prefix] of [['tv','TV'],['movies','Movie']]){
  const select=editor.getByLabel(new RegExp(`^${prefix} client eligibility tags`));await ready(select);
  await select.selectOption(String(tags[domain].id));
  assert.equal(await select.getByRole('option',{name:tags[domain==='tv'?'movies':'tv'].label,exact:true}).count(),0);
 }
 await saveProvider();await editor.getByRole('button',{name:'Reload saved provider',exact:true}).click();await ready(editor.getByLabel('Name',{exact:true}));
 let stored=await api('get',`/api/v1/providers/${primary.id}`);
 for(const [domain,prefix] of [['tv','TV'],['movies','Movie']]){
  assert.deepEqual(stored.settings[domain].tags,[tags[domain].id]);
  assert.deepEqual(await editor.getByLabel(new RegExp(`^${prefix} client eligibility tags`)).evaluate(el=>Array.from(el.selectedOptions,option=>Number(option.value))),[tags[domain].id]);
  const detail=(await api('get',`/api/v1/${domain}/tags/detail`)).find(row=>row.tag.id===tags[domain].id);assert.deepEqual(detail.download_client_ids,[primary.id]);
  await nav.getByRole('button',{name:'Tags',exact:true}).click();const panel=page.getByRole('region',{name:'Tags settings',exact:true});
  await ready(panel.getByRole('button',{name:domain==='tv'?'TV':'Movies',exact:true}));await panel.getByRole('button',{name:domain==='tv'?'TV':'Movies',exact:true}).click();
  await ready(panel.getByRole('button',{name:'Reload tags',exact:true}));await panel.getByRole('button',{name:'Reload tags',exact:true}).click();
  await panel.getByRole('button',{name:`Usage of ${tags[domain].label}`,exact:true}).click();await panel.getByRole('region',{name:'Tag usage'}).getByText(`Download client ${primary.id}`,{exact:true}).waitFor();
  // Return to the persisted editor before checking the next domain's selected options.
  await edit(primary);
 }
 await editor.getByRole('button',{name:'Reload client tag choices',exact:true}).click();
 for(const prefix of ['TV','Movie']){const select=editor.getByLabel(new RegExp(`^${prefix} client eligibility tags`));await ready(select);await select.selectOption([]);}
 await saveProvider();stored=await api('get',`/api/v1/providers/${primary.id}`);
 for(const domain of ['tv','movies']){
  assert.deepEqual(stored.settings[domain].tags,[]);
  const detail=(await api('get',`/api/v1/${domain}/tags/detail`)).find(row=>row.tag.id===tags[domain].id);assert.deepEqual(detail.download_client_ids,[]);
 }
 // Schedules persist default intent without choosing a client before a release exists.
 for(const domain of ['tv','movies']){
  await nav.getByRole('button',{name:'RSS',exact:true}).click();await ready(rss.getByLabel(/^RSS media/));await rss.getByLabel(/^RSS media/).selectOption(domain);
  await ready(rss.getByLabel(/^RSS indexer/));await rss.getByLabel(/^RSS indexer/).selectOption(indexer.id);await rss.getByLabel(/^RSS download client/).selectOption('');
  const saved=page.waitForResponse(r=>r.request().method()==='POST'&&new URL(r.url()).pathname==='/api/v1/rss/schedules');
  await rss.getByRole('button',{name:'Save RSS schedule',exact:true}).click();const response=await saved;assert.equal(response.status(),200);
  const schedule=await response.json();assert.equal(schedule.target.selection_kind,'default');assert.equal(schedule.target.client_id,null);assert.equal(schedule.target.client_revision,null);assert.equal(schedule.enabled,false);
  const readback=(await api('get','/api/v1/rss/schedules')).find(value=>value.id===schedule.id);assert.deepEqual(readback.target,schedule.target);
  await rss.getByText(/Default client selection/).waitFor();
 }
 await request(primaryOrigin,'post','/fixture-mode?mode=4',undefined,204);
 // With two eligible clients, movies stop at the first definite pre-submission failure.
 const primaryBefore=await observations(primaryOrigin),alternateBefore=await observations(alternateOrigin);
 const movieFailed=await automaticSearch('movies',indexer);
 assert.equal(movieFailed.selected_candidate_status,'rejected');
 await showAttempts(movieFailed,[[primary.id,'definitely_not_submitted','Not submitted']]);
 const primaryAfterMovie=await observations(primaryOrigin),alternateAfterMovie=await observations(alternateOrigin);
 assert.ok(primaryAfterMovie.length>primaryBefore.length);assert.ok(primaryAfterMovie.slice(primaryBefore.length).some(row=>row.path.startsWith('/api/v2/')));
 assert.equal(addCount(primaryAfterMovie),addCount(primaryBefore));assert.equal(addCount(alternateAfterMovie),addCount(alternateBefore));
 assert.deepEqual(await request(alternateOrigin,'get','/fixture-completed'),[]);
 // TV safely advances to the second client and exposes ordered attempt evidence and winner.
 const tv=await automaticSearch('tv',indexer);assert.equal(tv.selected_candidate_status,'observed');assert.equal(tv.selected_client_id,alternate.id);
 assert.deepEqual(tv.selected_client_attempts.map(attempt=>attempt.client_id),[primary.id,alternate.id]);
 await showAttempts(tv,[[primary.id,'definitely_not_submitted','Not submitted'],[alternate.id,'observed','Confirmed in client']]);
 await search.getByText(`Selected client ${alternate.id}`,{exact:true}).waitFor();
 const alternateAfterTv=await observations(alternateOrigin);assert.equal(addCount(alternateAfterTv),addCount(alternateAfterMovie)+1);
 let downloads=await request(alternateOrigin,'get','/fixture-completed');assert.equal(downloads.length,1);assert.equal(downloads[0].category,'tv');
 // Explicit user choice overrides a saved preference, including a currently failing client.
 await edit(indexer);await editor.getByLabel(/^Movie bound download client/).selectOption(primary.id);await saveProvider();
 assert.equal((await api('get',`/api/v1/providers/${indexer.id}`)).settings.movies.download_client_id,primary.id);
 const beforeExplicit=await observations(primaryOrigin);
 const movie=await automaticSearch('movies',indexer,alternate.id);assert.equal(movie.selected_candidate_status,'observed');assert.equal(movie.selected_client_id,alternate.id);
 await showAttempts(movie,[[alternate.id,'observed','Confirmed in client']]);await search.getByText(`Selected client ${alternate.id}`,{exact:true}).waitFor();
 assert.equal(addCount(await observations(primaryOrigin)),addCount(beforeExplicit));
 assert.equal(addCount(await observations(alternateOrigin)),addCount(alternateAfterTv)+1);
 downloads=await request(alternateOrigin,'get','/fixture-completed');assert.deepEqual(downloads.map(value=>value.category).sort(),['movies','tv']);
 // No eligible client remains a real pending receipt with a visible retry deadline.
 await addLibrary('tv',203);await setEnabled(primary,false);await setEnabled(alternate,false);
 const pendingPrimary=addCount(await observations(primaryOrigin)),pendingAlternate=addCount(await observations(alternateOrigin));
 const pending=await automaticSearch('tv',indexer,'',203,value=>value.selected_candidate_status==='pending'&&value.selected_candidate_error_code==='download_client_unavailable');
 assert.equal(pending.selected_client_id,null);assert.deepEqual(pending.selected_client_attempts,[]);
 await search.getByRole('button',{name:'Check search status',exact:true}).click();await search.getByRole('alert').filter({hasText:'download_client_unavailable'}).waitFor();
 await nav.getByRole('button',{name:'RSS',exact:true}).click();await ready(rss.getByLabel(/^RSS media/));await rss.getByLabel(/^RSS media/).selectOption('tv');
 await rss.getByRole('button',{name:'Check RSS status',exact:true}).click();
 const receipt=rss.getByRole('listitem').filter({hasText:pending.selected_candidate_id});
 await receipt.getByText('Awaiting an eligible download client',{exact:true}).waitFor();await receipt.getByText(/^Next check /).waitFor();
 const pendingRows=await api('get','/api/v1/rss/candidates?media_type=tv&limit=25&offset=0');
 const pendingReceipt=pendingRows.items.find(value=>value.id===pending.selected_candidate_id);assert.ok(pendingReceipt);assert.ok(pendingReceipt.not_before>0);
 assert.equal(addCount(await observations(primaryOrigin)),pendingPrimary);assert.equal(addCount(await observations(alternateOrigin)),pendingAlternate);
 // Eligibility recovery can still exhaust safely; retain the same receipt for a later round.
 await request(alternateOrigin,'post','/fixture-mode?mode=4',undefined,204);await setEnabled(alternate,true);
 const exhausted=await until(()=>api('get',`/api/v1/search/commands/${pending.id}`),value=>value.selected_candidate_status==='pending'&&value.selected_candidate_error_code==='download_client_unavailable'&&value.selected_client_attempts.length===1,'Pending receipt did not retain its safely exhausted first round',90000);
 assert.equal(exhausted.selected_candidate_id,pending.selected_candidate_id);assert.equal(exhausted.selected_client_id,null);
 assert.equal(exhausted.selected_client_attempts[0].client_id,alternate.id);assert.equal(exhausted.selected_client_attempts[0].selection_round,1);assert.equal(exhausted.selected_client_attempts[0].state,'definitely_not_submitted');
 await rss.getByRole('button',{name:'Check RSS status',exact:true}).click();await receipt.getByText(new RegExp(`^Client ${alternate.id}: Not submitted · Round 1`)).waitFor();await receipt.getByText(/^Next check /).waitFor();
 assert.equal(addCount(await observations(primaryOrigin)),pendingPrimary);assert.equal(addCount(await observations(alternateOrigin)),pendingAlternate);
 await request(alternateOrigin,'post','/fixture-mode?mode=0',undefined,204);await api('post',`/api/v1/providers/${alternate.id}/test`);
 const retried=await until(()=>api('get',`/api/v1/search/commands/${pending.id}`),value=>value.selected_candidate_status==='observed','Pending receipt did not retry after client became eligible',90000);
 assert.equal(retried.selected_candidate_id,pending.selected_candidate_id);assert.equal(retried.selected_client_id,alternate.id);assert.equal(retried.selected_client_attempts.length,2);
 assert.deepEqual(retried.selected_client_attempts.map(value=>[value.ordinal,value.selection_round,value.client_id,value.state]),[[1,1,alternate.id,'definitely_not_submitted'],[2,2,alternate.id,'observed']]);
 await rss.getByRole('button',{name:'Check RSS status',exact:true}).click();await receipt.getByText(`Selected client ${alternate.id}`,{exact:true}).waitFor();
 await receipt.getByText(new RegExp(`^Client ${alternate.id}: Not submitted · Round 1`)).waitFor();await receipt.getByText(new RegExp(`^Client ${alternate.id}: Confirmed in client · Round 2`)).waitFor();
 assert.equal(addCount(await observations(primaryOrigin)),pendingPrimary);assert.equal(addCount(await observations(alternateOrigin)),pendingAlternate+1);
 // The mock accepts this add but loses its response; unknown stays on the same client.
 await addLibrary('movies',203);await setEnabled(primary,true);await request(primaryOrigin,'post','/fixture-mode?mode=5',undefined,204);
 const unknownPrimary=addCount(await observations(primaryOrigin)),unknownAlternate=addCount(await observations(alternateOrigin));
 const unknown=await automaticSearch('movies',indexer,'',203,value=>value.selected_candidate_status==='reconciling'&&value.selected_client_attempts.some(attempt=>attempt.state==='unknown'));
 assert.equal(unknown.selected_client_id,primary.id);assert.equal(unknown.selected_candidate_error_code,'submission_unknown');
 await showAttempts(unknown,[[primary.id,'unknown','Outcome uncertain']]);
 await search.getByRole('alert').filter({hasText:'submission_unknown'}).waitFor();
 assert.equal(addCount(await observations(primaryOrigin)),unknownPrimary+1);assert.equal(addCount(await observations(alternateOrigin)),unknownAlternate);
 const acceptedUnknown=await request(primaryOrigin,'get','/fixture-completed');assert.equal(acceptedUnknown.length,1);assert.equal(acceptedUnknown[0].category,'movies');
 await nav.getByRole('button',{name:'RSS',exact:true}).click();await ready(rss.getByLabel(/^RSS media/));await rss.getByLabel(/^RSS media/).selectOption('movies');
 await rss.getByRole('button',{name:'Check RSS status',exact:true}).click();
 const uncertainReceipt=rss.getByRole('listitem').filter({hasText:unknown.selected_candidate_id});
 await uncertainReceipt.getByText(new RegExp(`^Client ${primary.id}: Outcome uncertain`)).waitFor();await uncertainReceipt.getByText(/^Next check /).waitFor();
 await request(primaryOrigin,'post','/fixture-mode?mode=0',undefined,204);
 // Remote presence after response loss cannot prove the requested options or ownership.
 const reconciled=await until(()=>api('get',`/api/v1/search/commands/${unknown.id}`),value=>value.selected_candidate_status==='needs_attention','Unknown receipt did not reconcile with its original client',60000);
 assert.equal(reconciled.selected_candidate_error_code,'presence_unconfirmed');
 assert.equal(reconciled.selected_candidate_id,unknown.selected_candidate_id);assert.equal(reconciled.selected_client_id,primary.id);assert.equal(reconciled.selected_client_attempts.length,1);assert.equal(reconciled.selected_client_attempts[0].state,'unknown');
 await rss.getByRole('button',{name:'Check RSS status',exact:true}).click();await uncertainReceipt.getByText(new RegExp(`^Client ${primary.id}: Outcome uncertain`)).waitFor();
 await uncertainReceipt.getByText('presence_unconfirmed',{exact:true}).waitFor();
 await openSearch('movies',indexer,'',203);await showAttempts(reconciled,[[primary.id,'unknown','Outcome uncertain']]);
 await search.getByRole('alert').filter({hasText:'presence_unconfirmed'}).waitFor();
 assert.equal(addCount(await observations(primaryOrigin)),unknownPrimary+1);assert.equal(addCount(await observations(alternateOrigin)),unknownAlternate);
 assert.deepEqual(errors,[]);
 console.log('PASS real scoped client tags/editor/TagDetail usage and explicit clearing; both-domain default schedules; movie default first-failure stop; TV safe fallback with attempts/winner; explicit movie choice overrides bound primary; actual add observations; unavailable receipt UI, safe exhaustion and same-client round2 recovery on real deadlines; ambiguous real add remains visibly uncertain after same-client presence reconciliation, without resubmission.');
}finally{await browser.close();}
