// Owned library_ui_fixture only: real HTTP, no installed services. The command worker is
// stopped before any search is admitted, so Wanted searches stay queued and never reach a
// provider; assertions read the durable commands back through the production API.
import assert from 'node:assert/strict';
import {mkdir} from 'node:fs/promises';
import {isAbsolute,join} from 'node:path';
import {pathToFileURL} from 'node:url';
const {chromium}=await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE||'/usr/lib/node_modules/playwright/index.mjs').href);
const origin=process.env.UI_URL,fixture=JSON.parse(process.env.UI_FIXTURE_JSON||'{}');
assert.equal(new URL(origin).hostname,'127.0.0.1');
assert.equal(new URL(fixture.api).hostname,'127.0.0.1');
const artifacts=process.env.UI_SCREENSHOT_DIR;
assert.ok(artifacts&&isAbsolute(artifacts)&&/\.do-not-commit\/loop\/iteration108(?:[-/]|$)/.test(artifacts),'UI_SCREENSHOT_DIR must be an owned iteration108 artifact directory');
await mkdir(artifacts,{recursive:true});
const browser=await chromium.launch({headless:true,...(process.env.PLAYWRIGHT_EXECUTABLE?{executablePath:process.env.PLAYWRIGHT_EXECUTABLE}:{})});
const page=await browser.newPage();page.setDefaultTimeout(20000);
const errors=[];page.on('pageerror',error=>errors.push(error.message));
page.on('dialog',dialog=>{errors.push(`Unexpected dialog: ${dialog.message()}`);void dialog.dismiss();});
async function api(method,path,data,status=200){
 const response=await page.request[method](origin+path,{...(data===undefined?{}:{data}),timeout:10000});
 assert.equal(response.status(),status,`${method} ${path}: ${await response.text()}`);return status===204?null:response.json();
}
async function until(read,accept,label,timeoutMs=20000){
 const deadline=Date.now()+timeoutMs;let last;
 while(Date.now()<deadline){last=await read();if(accept(last))return last;await page.waitForTimeout(100);}
 assert.fail(`${label}: ${JSON.stringify(last)}`);
}
const wanted=page.getByRole('region',{name:'Wanted',exact:true});
const search=wanted.getByRole('region',{name:'Wanted search',exact:true});
const table=name=>wanted.getByRole('table',{name,exact:true});
async function rowsOf(name){await table(name).waitFor();await until(()=>table(name).getAttribute('aria-busy'),v=>v==='false','Table stayed busy');return table(name).getByRole('row').allInnerTexts();}
async function chooseProviders(indexer){
 const select=search.getByLabel(/^Wanted search indexer/);
 await until(()=>select.locator('option').allTextContents(),opts=>opts.some(o=>o.trim()===indexer.name),'Indexer option missing');
 await select.selectOption(indexer.id);
 assert.equal(await search.getByLabel(/^Wanted download client/).inputValue(),'');
}
async function admitted(click){
 const response=page.waitForResponse(r=>r.request().method()==='POST'&&/^\/api\/v1\/(tv|movies)\/wanted\/search$/.test(new URL(r.url()).pathname));
 await click();const reply=await response;assert.equal(reply.status(),202,await reply.text());return reply.json();
}
async function commandsFor(type,id){return (await api('get',`/api/v1/search/commands?target_type=${type}&target_id=${id}`)).items;}
try{
 // Stop the shared worker first: admitted searches must stay queued in this owned fixture.
 await api('post','/api/fixture/stop-command-worker',undefined,204);
 const download=category=>({category,imported_category:null,recent_priority:0,older_priority:0,tags:[]});
 await api('post','/api/v1/providers',{name:'Wanted client',enabled:true,priority:1,settings:{implementation:'qbittorrent',endpoint:fixture.provider_origin,tv:download('tv'),movies:download('movies')},credentials:{kind:'username_password',username:'fixture-user',password:'fixture-good'}},201);
 const indexer=await api('post','/api/v1/providers',{name:'Wanted indexer',enabled:true,priority:1,settings:{implementation:'torznab',endpoint:`${fixture.provider_origin}/torznab`,tv:{categories:[5030],anime_categories:[]},movies:{categories:[2000]}},credentials:{kind:'api_key',api_key:'fixture-good'}},201);
 const profile=domain=>api('post',`/api/v1/${domain}/quality-profiles`,{name:`Wanted ${domain}`,items:[{kind:'quality',quality_id:4,allowed:true},{kind:'quality',quality_id:3,allowed:true}],policy:{upgrade_allowed:true,cutoff:{kind:'quality',quality_id:3},min_format_score:0,cutoff_format_score:0,min_upgrade_format_score:1,language_id:domain==='movies'?-1:null,format_items:[]}},201);
 const tvProfile=await profile('tv'),movieProfile=await profile('movies');
 const seeded=await api('post','/api/fixture/wanted-seed',{tv_profile_id:tvProfile.id,movie_profile_id:movieProfile.id});
 const {episodes,movies}=seeded;
 assert.equal(episodes.pilot,movies.alpha,'Fixture keeps colliding numeric episode/movie IDs');

 await page.goto(`${origin}/#/wanted/missing`);
 await wanted.getByRole('heading',{name:'Wanted',exact:true}).waitFor();
 assert.equal(await page.getByRole('navigation',{name:'Workspace'}).getByRole('button',{name:'Wanted',exact:true}).getAttribute('aria-pressed'),'true');
 let rows=await rowsOf('Missing episodes');
 assert.equal(rows.length,3,rows.join('\n'));
 assert.ok(rows[1].includes('Wanted pilot')&&rows[1].includes('S01E01')&&rows[1].includes('2020-01-01'),rows[1]);
 assert.ok(rows[2].includes('Wanted second'));
 assert.ok(!rows.join('\n').includes('Wanted future'),'Future episodes are not missing');
 await wanted.getByText('Showing 1–2 of 2 missing episodes.',{exact:false}).waitFor();
 // Last searched column: native fact in UTC, never-searched explicit; the time is machine-readable.
 assert.ok(rows[0].includes('Last searched'),rows[0]);
 assert.ok(rows[1].includes('2023-11-14 22:13 UTC'),rows[1]);
 assert.ok(rows[2].includes('Never searched'),rows[2]);
 assert.equal(await table('Missing episodes').locator('time[datetime="2023-11-14T22:13:20Z"]').count(),1);
 const sortBy=wanted.getByLabel('Sort by',{exact:true}),direction=wanted.getByLabel('Sort direction',{exact:true});
 await sortBy.selectOption('last_search');
 await until(async()=>(await rowsOf('Missing episodes')).slice(1).map(r=>r.includes('Wanted second')?'second':'pilot').join(),v=>v==='second,pilot','Never searched first ascending');
 const tvSorted=await api('get','/api/v1/tv/wanted/missing?sort=last_search');
 assert.deepEqual(tvSorted.items.map(i=>[i.target.id,i.last_search_time,i.last_search_source]),[[episodes.second,null,null],[episodes.pilot,'2023-11-14T22:13:20Z','native']]);
 await direction.selectOption('desc');
 await until(async()=>(await rowsOf('Missing episodes')).slice(1).map(r=>r.includes('Wanted second')?'second':'pilot').join(),v=>v==='pilot,second','Never searched last descending');
 await direction.selectOption('asc');await sortBy.selectOption('air_date');
 rows=await until(()=>rowsOf('Missing episodes'),r=>r.length===3&&r[1].includes('Wanted pilot'),'Air date sort restored');
 if(artifacts)await page.screenshot({path:join(artifacts,'wanted-tv-missing-desktop.png'),fullPage:true});

 // Keyboard selection, then search selected with default client selection.
 const pilot=table('Missing episodes').getByRole('checkbox',{name:/^Select Wanted Harbor S01E01 Wanted pilot$/});
 await pilot.focus();await page.keyboard.press('Space');assert.equal(await pilot.isChecked(),true);
 await chooseProviders(indexer);
 const selected=await admitted(()=>search.getByRole('button',{name:'Search selected (1)',exact:true}).click());
 assert.deepEqual(selected.commands.map(c=>c.target),[{media_type:'episode',id:episodes.pilot}]);
 await search.getByText('Queued 1 episode for automatic search; 1 matched.',{exact:true}).waitFor();
 let stored=await commandsFor('episode',episodes.pilot);
 assert.equal(stored.length,1);assert.equal(stored[0].id,selected.commands[0].command_id);
 assert.deepEqual([stored[0].mode,stored[0].status,stored[0].source.selection_kind,stored[0].source.indexer_id],['automatic','queued','default',indexer.id]);
 assert.equal((await commandsFor('movie',movies.alpha)).length,0,'Episode search never targets the equal-numbered movie');
 await search.getByRole('list',{name:'Queued searches'}).getByText(`episode ${episodes.pilot}: queued`,{exact:true}).waitFor();

 // Search all: confirmation is explicit; the active pilot search is skipped, not duplicated.
 await search.getByRole('button',{name:'Search all',exact:true}).click();
 const confirm=search.getByRole('alertdialog',{name:'Search all missing episodes?'});
 await confirm.getByText(/every current monitored member of this list \(2 now; at most 1024\)/).waitFor();
 await confirm.getByRole('button',{name:'Cancel search all',exact:true}).click();
 assert.equal(await confirm.count(),0);
 assert.equal((await commandsFor('episode',episodes.second)).length,0,'Cancelling confirmation admits nothing');
 await search.getByRole('button',{name:'Search all',exact:true}).click();
 const all=await admitted(()=>confirm.getByRole('button',{name:'Confirm search all',exact:true}).click());
 assert.deepEqual([all.matched,all.skipped_active,all.admitted],[2,1,1]);
 assert.deepEqual(all.commands.map(c=>c.target),[{media_type:'episode',id:episodes.second}]);
 await search.getByText('Queued 1 episode for automatic search; 2 matched; 1 skipped because a search is already active.',{exact:true}).waitFor();
 assert.equal((await commandsFor('episode',episodes.pilot)).length,1);
 assert.equal((await commandsFor('episode',episodes.second)).length,1);

 // Cutoff unmet TV via the route switch.
 await wanted.getByRole('navigation',{name:'Wanted lists'}).getByRole('button',{name:'Cutoff unmet',exact:true}).click();
 await until(()=>page.evaluate(()=>location.hash),hash=>hash==='#/wanted/cutoffunmet','Cutoff route');
 rows=await rowsOf('Cutoff unmet episodes');
 assert.equal(rows.length,2,rows.join('\n'));
 assert.ok(rows[1].includes('Wanted low quality')&&rows[1].includes('HDTV-720p'),rows[1]);
 assert.ok(rows[1].includes('2024-01-01 00:00 UTC (imported)'),'Imported snapshot value is labelled as imported: '+rows[1]);
 assert.ok(!rows.join('\n').includes('Wanted good quality'));
 await wanted.getByLabel('Monitoring filter',{exact:true}).selectOption('unmonitored');
 await wanted.getByText('No cutoff unmet episodes for this filter.',{exact:false}).waitFor();
 await wanted.getByLabel('Monitoring filter',{exact:true}).selectOption('monitored');
 await rowsOf('Cutoff unmet episodes');

 // Movies cutoff: search all admits the typed movie target only.
 await wanted.getByRole('navigation',{name:'Wanted media type'}).getByRole('button',{name:'Movies',exact:true}).click();
 rows=await rowsOf('Cutoff unmet movies');
 assert.equal(rows.length,2,rows.join('\n'));assert.ok(rows[1].includes('Wanted Charlie')&&rows[1].includes('HDTV-720p'));
 await chooseProviders(indexer);
 await search.getByRole('button',{name:'Search all',exact:true}).click();
 const movieAll=await admitted(()=>search.getByRole('alertdialog',{name:'Search all cutoff unmet movies?'}).getByRole('button',{name:'Confirm search all',exact:true}).click());
 assert.deepEqual(movieAll.commands.map(c=>c.target),[{media_type:'movie',id:movies.charlie}]);
 stored=await commandsFor('movie',movies.charlie);assert.equal(stored.length,1);assert.equal(stored[0].source.media_type,'movies');
 await search.getByText('Queued 1 movie for automatic search; 1 matched.',{exact:true}).waitFor();
 await search.getByRole('button',{name:'Check search status',exact:true}).click();
 await search.getByText(/^Status of 1 searches: 1 queued\./).waitFor();

 // Movies missing: unavailable movies stay listed; the availability filter uses the policy.
 await wanted.getByRole('navigation',{name:'Wanted lists'}).getByRole('button',{name:'Missing',exact:true}).click();
 rows=await rowsOf('Missing movies');
 assert.equal(rows.length,3,rows.join('\n'));
 assert.ok(rows[1].includes('Wanted Alpha')&&rows[1].includes('Available'));
 assert.ok(rows[2].includes('Wanted Bravo')&&rows[2].includes('Not yet available (released)'));
 await wanted.getByLabel('Availability filter',{exact:true}).selectOption('available');
 await until(async()=>(await rowsOf('Missing movies')).length,n=>n===2,'Availability filter');
 await wanted.getByLabel('Availability filter',{exact:true}).selectOption('all');
 await until(async()=>(await rowsOf('Missing movies')).length,n=>n===3,'Availability reset');
 rows=await rowsOf('Missing movies');
 assert.ok(rows[1].includes('2025-06-15 15:06 UTC'),'Movie fact is its own, not the equal-numbered episode\'s: '+rows[1]);
 assert.ok(rows[2].includes('Never searched'),rows[2]);
 await wanted.getByLabel('Sort by',{exact:true}).selectOption('last_search');
 await until(async()=>(await rowsOf('Missing movies')).slice(1).map(r=>r.includes('Wanted Bravo')?'bravo':'alpha').join(),v=>v==='bravo,alpha','Movies: never searched first');
 const sorted=await api('get','/api/v1/movies/wanted/missing?sort=last_search');
 assert.deepEqual(sorted.items.map(i=>[i.target.id,i.last_search_source]),[[movies.bravo,null],[movies.alpha,'native']]);
 await wanted.getByLabel('Sort by',{exact:true}).selectOption('title');
 await until(async()=>(await rowsOf('Missing movies')).slice(1).map(r=>r.includes('Wanted Bravo')?'bravo':'alpha').join(),v=>v==='alpha,bravo','Title sort restored');

 // Selection actions use the existing monitoring writes and refresh membership.
 await table('Missing movies').getByRole('checkbox',{name:'Select Wanted Bravo (2099)',exact:true}).check();
 await wanted.getByRole('button',{name:'Unmonitor selected',exact:true}).click();
 await wanted.getByText('Unmonitored 1 movie.',{exact:true}).waitFor();
 await until(async()=>(await rowsOf('Missing movies')).length,n=>n===2,'Unmonitored movie leaves the monitored list');
 assert.equal((await api('get',`/api/v1/movies/${movies.bravo}`)).monitored,false);
 await wanted.getByLabel('Monitoring filter',{exact:true}).selectOption('unmonitored');
 await until(async()=>(await rowsOf('Missing movies')).join('\n'),t=>t.includes('Wanted Bravo')&&t.includes('Unmonitored'),'Unmonitored partition');
 if(artifacts)await page.screenshot({path:join(artifacts,'wanted-movies-missing-desktop.png'),fullPage:true});

 // Strict server validation is visible as an error state, not an empty list.
 const invalid=await page.request.get(`${origin}/api/v1/tv/wanted/missing?limit=0`);
 assert.equal(invalid.status(),400);assert.equal((await invalid.json()).error.code,'invalid_wanted_query');

 await page.setViewportSize({width:390,height:844});
 await wanted.getByRole('navigation',{name:'Wanted media type'}).getByRole('button',{name:'TV',exact:true}).click();
 await rowsOf('Missing episodes');
 if(artifacts)await page.screenshot({path:join(artifacts,'wanted-tv-missing-mobile.png'),fullPage:true});
 assert.deepEqual(errors,[]);
 console.log(JSON.stringify({wanted_browser:'passed',episode_commands:[selected.commands[0].command_id,all.commands[0].command_id],movie_command:movieAll.commands[0].command_id}));
}finally{await browser.close();}
