// scn.007 connected proof, owned library_ui_fixture only (real HTTP, no installed services).
// Library rows are created through the real lookup-add API against the fixture's metadata mock
// (tvdb 118 / tmdb 118-120, dates relative to the fixture clock); the has-file episode and the
// cutoff movie get their files through the real downloaded-scan API and worker. No fixture SQL
// hook is used. Searches run through the real command worker against a Torznab mock owned by
// this script on 127.0.0.1:0 that returns no releases, so every automatic search settles
// naturally as failed/no_eligible_release; no terminal state is written directly.
// Disclosed control points: the fixture's stop/start-command-worker routes park admitted work
// as genuinely queued, and the Torznab mock can hold search responses (< the 10 s provider
// request timeout) so the running state is observable; page.route is used only for the
// labelled 503 error-state checks.
import assert from 'node:assert/strict';
import {mkdir} from 'node:fs/promises';
import {createServer} from 'node:http';
import {isAbsolute,join} from 'node:path';
import {pathToFileURL} from 'node:url';
import {randomUUID} from 'node:crypto';
const {chromium}=await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE||'/usr/lib/node_modules/playwright/index.mjs').href);
const origin=process.env.UI_URL,fixture=JSON.parse(process.env.UI_FIXTURE_JSON||'{}');
const scratch=process.env.UI_SCRATCH,tvRoot=process.env.UI_TV_ROOT,movieRoot=process.env.UI_MOVIE_ROOT;
assert.equal(new URL(origin).hostname,'127.0.0.1');
assert.equal(new URL(fixture.api).hostname,'127.0.0.1');
assert.equal(new URL(fixture.provider_origin).hostname,'127.0.0.1');
assert.ok(scratch&&isAbsolute(scratch)&&tvRoot&&movieRoot,'Fixture scratch roots are required');
const artifacts=process.env.UI_SCREENSHOT_DIR;
assert.ok(artifacts&&isAbsolute(artifacts)&&/\.do-not-commit\/loop\/iteration118(?:[-/]|$)/.test(artifacts),'UI_SCREENSHOT_DIR must be an owned iteration118 artifact directory');
await mkdir(artifacts,{recursive:true});

// ---- Owned Torznab mock: caps plus empty result pages; search replies can be held. ----
const INDEXER_KEY='scn007-owned-indexer-key';
const HOLD_LIMIT_MS=8000; // below the provider transport's 10 s request timeout
const MAX_RECORDED=512;
const CAPS='<caps><limits max="100" default="100"/><searching><search available="yes" supportedParams="q"/><tv-search available="yes" supportedParams="q,tvdbid,season,ep"/><movie-search available="yes" supportedParams="q,tmdbid,imdbid"/></searching><categories><category id="5030"/><category id="2000"/></categories></caps>';
const EMPTY='<rss xmlns:x="http://www.newznab.com/DTD/2010/feeds/attributes/"><channel><x:response offset="0" total="0"/></channel></rss>';
const torznab={requests:[],held:false,waiting:[],autoReleased:0,rejected:0};
const mock=createServer((request,response)=>{
 const url=new URL(request.url,'http://127.0.0.1');
 const q=Object.fromEntries(url.searchParams);
 const reply=(status,body)=>{if(!response.writableEnded)response.writeHead(status,{'content-type':'application/xml'}).end(body);};
 if(url.pathname!=='/torznab'){torznab.rejected++;return reply(404,'<error code="404"/>');}
 if(torznab.requests.length<MAX_RECORDED)torznab.requests.push({t:q.t,cat:q.cat??null,q:q.q??null,tvdbid:q.tvdbid??null,tmdbid:q.tmdbid??null,season:q.season??null,ep:q.ep??null,key:q.apikey===INDEXER_KEY});
 if(q.t==='caps')return reply(200,CAPS);
 if(q.apikey!==INDEXER_KEY){torznab.rejected++;return reply(200,'<error code="100" description="owned mock rejected key"/>');}
 if(!torznab.held)return reply(200,EMPTY);
 const timer=setTimeout(()=>{torznab.autoReleased++;reply(200,EMPTY);},HOLD_LIMIT_MS);
 torznab.waiting.push(()=>{clearTimeout(timer);reply(200,EMPTY);});
});
await new Promise((resolve,reject)=>{mock.once('error',reject);mock.listen(0,'127.0.0.1',resolve);});
const torznabOrigin=`http://127.0.0.1:${mock.address().port}`;
const releaseHeld=()=>{torznab.held=false;for(const release of torznab.waiting.splice(0))release();};

const ZONE='Atlantic/Reykjavik'; // UTC+0 without DST: viewer-local days equal the UTC dates the server reports
const browser=await chromium.launch({headless:true,...(process.env.PLAYWRIGHT_EXECUTABLE?{executablePath:process.env.PLAYWRIGHT_EXECUTABLE}:{})});
const context=await browser.newContext({timezoneId:ZONE,locale:'en-US',viewport:{width:1280,height:900}});
// Keep interception enabled for the whole run: Playwright drops requests issued while the last
// page-level unroute disables interception, so a context fallback stays installed.
await context.route('**/api/**',route=>route.fallback());
const page=await context.newPage();page.setDefaultTimeout(20000);
const errors=[];page.on('pageerror',error=>errors.push(error.message));
page.on('dialog',dialog=>{errors.push(`Unexpected dialog: ${dialog.message()}`);void dialog.dismiss();});
const searchListReads=[];
page.on('request',request=>{const url=new URL(request.url());if(request.method()==='GET'&&url.pathname==='/api/v1/search/commands'&&!url.searchParams.has('target_type'))searchListReads.push(Date.now());});
let mainNavigations=0;page.on('framenavigated',frame=>{if(frame===page.mainFrame())mainNavigations++;});

async function api(method,path,data,status=200){
 const response=await page.request[method](origin+path,{...(data===undefined?{}:{data}),timeout:10000});
 const expected=Array.isArray(status)?status:[status];
 assert.ok(expected.includes(response.status()),`${method} ${path}: ${response.status()} ${await response.text()}`);
 return response.status()===204?null:response.json();
}
async function until(read,accept,label,timeoutMs=20000,stepMs=100){
 const deadline=Date.now()+timeoutMs;let last;
 while(Date.now()<deadline){last=await read();if(accept(last))return last;await page.waitForTimeout(stepMs);}
 assert.fail(`${label}: ${JSON.stringify(last)}`);
}
const shot=name=>page.screenshot({path:join(artifacts,`${name}.png`),fullPage:true});
const addDays=(d,n)=>new Date(Date.parse(`${d}T00:00:00Z`)+n*86400000).toISOString().slice(0,10);
const MONTHS=['January','February','March','April','May','June','July','August','September','October','November','December'];
const monthName=d=>`${MONTHS[Number(d.slice(5,7))-1]} ${Number(d.slice(0,4))}`;
const nav=page.getByRole('navigation',{name:'Workspace'});

// ---- Calendar helpers ----
const cal=page.getByRole('region',{name:'Calendar',exact:true});
const calBusy=()=>until(async()=>(await cal.getByRole('status').filter({hasText:'Loading calendar'}).count()),n=>n===0,'Calendar stayed loading');
const dayButton=d=>cal.getByRole('grid').locator(`button[data-date="${d}"]`);
const cellText=async d=>{await calBusy();return dayButton(d).locator('xpath=..').innerText();};
async function showMonthOf(date){
 const want=monthName(date);
 for(let i=0;i<24;i++){
  await calBusy();const title=(await cal.locator('#calendar-range').innerText()).trim();
  if(title===want)return;
  const [name,year]=title.split(' '),month=MONTHS.indexOf(name)+1;
  assert.ok(month>0&&Number(year)>0,`Unexpected month title ${title}`);
  const forward=Number(date.slice(0,4))*12+Number(date.slice(5,7))>Number(year)*12+month;
  await cal.getByRole('button',{name:forward?'Next month':'Previous month',exact:true}).click();
 }
 assert.fail(`Could not navigate to ${want}`);
}
async function feed(link){
 const url=new URL(link);assert.equal(url.origin,origin,'Feed links use the UI origin');
 const response=await page.request.get(origin+url.pathname+url.search,{timeout:10000});
 assert.equal(response.status(),200,`${url.pathname}: ${await response.text()}`);
 assert.equal(response.headers()['content-type'],'text/calendar; charset=utf-8');
 assert.equal(response.headers()['cache-control'],'private, no-store');
 const text=await response.text();
 assert.ok(text.startsWith('BEGIN:VCALENDAR\r\n')&&text.endsWith('END:VCALENDAR\r\n'));
 return text.replace(/\r\n[ \t]/g,'');
}
const uids=text=>[...text.matchAll(/^UID:(.+)$/gm)].map(m=>m[1]);
const events=text=>Object.fromEntries(text.split('BEGIN:VEVENT\r\n').slice(1).map(block=>[/^UID:(.+)$/m.exec(block)[1],block.split('END:VEVENT')[0]]));

// ---- Wanted helpers ----
const wanted=page.getByRole('region',{name:'Wanted',exact:true});
const wantedSearch=wanted.getByRole('region',{name:'Wanted search',exact:true});
const table=name=>wanted.getByRole('table',{name,exact:true});
async function rowsOf(name){await table(name).waitFor();await until(()=>table(name).getAttribute('aria-busy'),v=>v==='false','Wanted table stayed busy');return (await table(name).getByRole('row').allInnerTexts()).slice(1);}
async function wantedScope(list,domain){
 await wanted.getByRole('navigation',{name:'Wanted lists'}).getByRole('button',{name:list,exact:true}).click();
 await wanted.getByRole('navigation',{name:'Wanted media type'}).getByRole('button',{name:domain,exact:true}).click();
}
async function chooseIndexer(name){
 const select=wantedSearch.getByLabel(/^Wanted search indexer/);
 await until(()=>select.locator('option').allTextContents(),opts=>opts.some(o=>o.trim()===name),'Indexer option missing');
 await select.selectOption({label:name});
}
async function admitted(click){
 const response=page.waitForResponse(r=>r.request().method()==='POST'&&/^\/api\/v1\/(tv|movies)\/wanted\/search$/.test(new URL(r.url()).pathname));
 await click();const reply=await response;assert.equal(reply.status(),202,await reply.text());return reply.json();
}
const commandsFor=async(type,id)=>(await api('get',`/api/v1/search/commands?target_type=${type}&target_id=${id}`)).items;

// ---- Activity helpers ----
const activity=page.getByRole('region',{name:'Activity',exact:true});
const searches=activity.getByRole('region',{name:'Search commands',exact:true});
const searchRow=id=>searches.getByRole('table',{name:'Search command history',exact:true}).getByRole('row').filter({hasText:id});
async function searchState(id){const row=searchRow(id);if(!(await row.count()))return null;return {status:await row.getAttribute('data-status'),text:await row.innerText()};}
const refreshRow=id=>activity.getByRole('region',{name:'Commands',exact:true}).getByRole('row').filter({has:page.getByRole('button',{name:`Inspect command ${id}`,exact:true})});

const STATUS_TEXT={queued:'Queued',running:'Running',retry_wait:'Waiting to retry',succeeded:'Succeeded',failed:'Failed',cancelled:'Cancelled'};
let stoppedWorker=false;
try{
 // ---- 0. Accessible empty states against the real, still-empty library. ----
 await page.goto(`${origin}/#/calendar`);
 await cal.getByRole('heading',{name:'Calendar',exact:true}).waitFor();
 await calBusy();
 const emptyTitle=(await cal.locator('#calendar-range').innerText()).trim();
 await cal.getByRole('status').filter({hasText:`Nothing scheduled in ${emptyTitle} for these filters.`}).waitFor();
 await nav.getByRole('button',{name:'Wanted',exact:true}).click();
 await wanted.getByText('No missing episodes for this filter.',{exact:false}).waitFor();
 assert.deepEqual(await rowsOf('Missing episodes'),['Nothing missing here.']);
 await nav.getByRole('button',{name:'Activity',exact:true}).click();
 await searches.getByRole('cell',{name:'No search commands yet. Searches started from Wanted or library detail appear here.'}).waitFor();
 await searches.getByText('No search commands.',{exact:true}).waitFor();

 // ---- 1. Seed through real APIs. ----
 const download=category=>({category,imported_category:null,recent_priority:0,older_priority:0,tags:[]});
 const client=await api('post','/api/v1/providers',{name:'SCN007 client',enabled:true,priority:1,settings:{implementation:'qbittorrent',endpoint:fixture.provider_origin,tv:download('tv'),movies:download('movies')},credentials:{kind:'username_password',username:'fixture-user',password:'fixture-good'}},201);
 const indexer=await api('post','/api/v1/providers',{name:'SCN007 indexer',enabled:true,priority:1,settings:{implementation:'torznab',endpoint:`${torznabOrigin}/torznab`,tv:{categories:[5030],anime_categories:[],enable_rss:false},movies:{categories:[2000],enable_rss:false}},credentials:{kind:'api_key',api_key:INDEXER_KEY}},201);
 const profile=domain=>api('post',`/api/v1/${domain}/quality-profiles`,{name:`SCN007 ${domain}`,items:[{kind:'quality',quality_id:4,allowed:true,min_size:0},{kind:'quality',quality_id:3,allowed:true,min_size:0}],policy:{upgrade_allowed:true,cutoff:{kind:'quality',quality_id:3},min_format_score:0,cutoff_format_score:0,min_upgrade_format_score:1,language_id:domain==='movies'?-1:null,format_items:[]}},201);
 const tvProfile=await profile('tv'),movieProfile=await profile('movies');
 // An explicit movie release policy makes availability a real gate (not "ungated").
 await api('put','/api/v1/release-policies/movies',{torrent_delay_minutes:0,usenet_delay_minutes:0,availability_delay_days:0});
 const paths={series:join(tvRoot,'harbor-watch'),premiere:join(movieRoot,'harbor-premiere'),archive:join(movieRoot,'harbor-archive'),rerun:join(movieRoot,'harbor-rerun')};
 for(const path of Object.values(paths))await mkdir(path,{recursive:true});
 const series=await api('post','/api/v1/tv/series/lookup',{tvdb_id:118,path:paths.series,settings:{monitored:true,quality_profile_id:tvProfile.id,series_type:'standard'}},201);
 const movieSettings={monitored:true,quality_profile_id:movieProfile.id,minimum_availability:'released'};
 const premiere=await api('post','/api/v1/movies/lookup',{tmdb_id:118,path:paths.premiere,settings:movieSettings},201);
 const archive=await api('post','/api/v1/movies/lookup',{tmdb_id:119,path:paths.archive,settings:movieSettings},201);
 const rerun=await api('post','/api/v1/movies/lookup',{tmdb_id:120,path:paths.rerun,settings:movieSettings},201);
 assert.deepEqual([series.title,premiere.title,archive.title,rerun.title],['Harbor Watch','Harbor Premiere','Harbor Archive','Harbor Rerun']);
 let episodeList=(await api('get',`/api/v1/episodes?series_id=${series.id}&limit=25&offset=0`)).items.sort((a,b)=>a.number-b.number);
 assert.deepEqual(episodeList.map(e=>[e.season,e.number,e.title]),[[1,1,'Harbor arrival'],[1,2,'Harbor storm'],[1,3,'Harbor repairs'],[1,4,'Harbor regatta']]);
 const unmonitoredAtAdd=episodeList.filter(e=>!e.monitored).map(e=>e.id);
 if(unmonitoredAtAdd.length)await api('put','/api/v1/episodes/monitor',{episode_ids:unmonitoredAtAdd,monitored:true});
 const [arrival,storm,repairs,regatta]=episodeList.map(e=>e.id);
 // Equal numeric IDs across domains: every later assertion keeps the typed target.
 assert.equal(arrival,premiere.id,'Episode and movie IDs collide numerically (fresh fixture database)');
 const today=new Date().toISOString().slice(0,10);
 const airDay=Object.fromEntries(episodeList.map(e=>[e.id,e.air_date_utc.slice(0,10)]));
 assert.deepEqual([airDay[arrival],airDay[storm],airDay[repairs],airDay[regatta]],[addDays(today,-3),addDays(today,-2),addDays(today,-1),addDays(today,2)]);

 // Real downloaded-scan imports give the has-file episode and the cutoff movie a parsed quality.
 for(const [domain,folder] of [['tv','tv/Harbor.Watch.S01E03.720p.HDTV.x264'],['movies','movies/Harbor.Rerun.2018.720p.HDTV.x264.English']]){
  const id=randomUUID();
  await api('post',`/api/v1/${domain}/downloaded-scan-commands`,{id,path:join(scratch,'scn007-scan',folder),mode:'copy',priority:'normal'},202);
  const scan=await until(()=>api('get',`/api/v1/${domain}/downloaded-scan-commands/${id}`),s=>['succeeded','failed','cancelled'].includes(s.status),`${domain} scan`,60000,250);
  assert.deepEqual([scan.status,scan.outcome,scan.counts.imported],['succeeded','success',1],JSON.stringify(scan));
 }
 assert.equal((await api('get',`/api/v1/episodes/${repairs}`)).has_file,true);
 assert.equal((await api('get',`/api/v1/movies/${rerun.id}`)).statistics.file_count,1);

 // ---- 2. Calendar: API, UI and iCal feeds for both domains. ----
 const tvCal=await api('get',`/api/v1/tv/calendar?start=${addDays(today,-7)}&end=${addDays(today,14)}`);
 assert.deepEqual(tvCal.items.map(i=>[i.target.media_type,i.target.id,i.has_file,i.effective_monitored]),[['episode',arrival,false,true],['episode',storm,false,true],['episode',repairs,true,true],['episode',regatta,false,true]]);
 assert.ok(tvCal.items.every(i=>i.starts_at.endsWith('T20:00:00Z')&&i.runtime_minutes===30&&i.network==='Harbor Net'));
 const movieCal=await api('get',`/api/v1/movies/calendar?start=${addDays(today,-7)}&end=${addDays(today,14)}`);
 const releaseDays={in_cinemas:addDays(today,-5),digital:addDays(today,3),physical:addDays(today,12)};
 assert.deepEqual(movieCal.items.map(i=>[i.target.media_type,i.target.id,i.release_type,i.date]),Object.entries(releaseDays).map(([type,date])=>['movie',premiere.id,type,date]));
 assert.ok(movieCal.items.every(i=>i.studio==='Harbor Studio'&&!i.has_file&&i.monitored));

 await nav.getByRole('button',{name:'Calendar',exact:true}).click();
 await cal.getByRole('heading',{name:'Calendar',exact:true}).waitFor();
 const tvExpect=[[arrival,'S01E01 · Harbor arrival','Missing'],[storm,'S01E02 · Harbor storm','Missing'],[repairs,'S01E03 · Harbor repairs','Downloaded'],[regatta,'S01E04 · Harbor regatta','Not aired yet']];
 for(const [id,detail,status] of tvExpect){
  await showMonthOf(airDay[id]);
  const text=await cellText(airDay[id]);
  assert.ok(text.includes('20:00')&&text.includes('Harbor Watch')&&text.includes(detail)&&text.includes(status),`${airDay[id]}: ${text}`);
 }
 const movieExpect={in_cinemas:['In cinemas','Released, no file'],digital:['Digital release','Not released yet'],physical:['Physical release','Not released yet']};
 for(const [type,[label,status]] of Object.entries(movieExpect)){
  await showMonthOf(releaseDays[type]);
  const text=await cellText(releaseDays[type]);
  assert.equal((text.match(/Harbor Premiere \(\d{4}\)/g)||[]).length,1,text);
  assert.ok(text.includes('All day')&&text.includes(label)&&text.includes(status),`${releaseDays[type]}: ${text}`);
  for(const other of Object.values(movieExpect).map(v=>v[0]).filter(v=>v!==label))assert.ok(!text.includes(other),`${type} cell only shows its own release type: ${text}`);
 }
 await showMonthOf(today);
 await shot('scn007-calendar-month-desktop');
 // Feeds: the UI's own links, fetched on the same origin/router as the JSON API.
 const tvFeed=await feed(await cal.getByLabel('TV feed link',{exact:true}).inputValue());
 assert.deepEqual(uids(tvFeed),[arrival,storm,repairs,regatta].map(id=>`episode-${id}@hrrdarr`));
 assert.ok(tvFeed.includes(`DTSTART:${airDay[repairs].replaceAll('-','')}T200000Z`)&&tvFeed.includes('SUMMARY:Harbor Watch - 1x03 - Harbor repairs')&&tvFeed.includes('DURATION:PT30M'));
 const tvEvents=events(tvFeed);
 assert.ok(tvEvents[`episode-${repairs}@hrrdarr`].includes('STATUS:CONFIRMED')&&tvEvents[`episode-${arrival}@hrrdarr`].includes('STATUS:TENTATIVE'),'File presence sets the event status');
 const movieFeed=await feed(await cal.getByLabel('Movies feed link',{exact:true}).inputValue());
 assert.deepEqual(uids(movieFeed),['in_cinemas','digital','physical'].map(type=>`movie-${premiere.id}-${type}@hrrdarr`));
 for(const [type,date] of Object.entries(releaseDays))assert.ok(movieFeed.includes(`DTSTART;VALUE=DATE:${date.replaceAll('-','')}`),type);
 assert.ok(movieFeed.includes('SUMMARY:Harbor Premiere - Physical release')&&movieFeed.includes('CATEGORIES:Harbor Studio'));
 assert.ok(!movieFeed.includes('Harbor Archive')&&!movieFeed.includes('Harbor Rerun'),'Out-of-window movies are not emitted');
 // Empty window after seeding, an error state with retry, and strict query validation.
 await showMonthOf(addDays(today,75));
 const farTitle=(await cal.locator('#calendar-range').innerText()).trim();
 await cal.getByRole('status').filter({hasText:`Nothing scheduled in ${farTitle} for these filters.`}).waitFor();
 const failTv=url=>url.pathname==='/api/v1/tv/calendar';
 await page.route(failTv,route=>route.fulfill({status:503,contentType:'application/json',body:JSON.stringify({error:{code:'calendar_unavailable',message:'Calendar storage is unavailable'}})}));
 await cal.getByRole('button',{name:'Refresh calendar',exact:true}).click();
 await cal.getByRole('alert').filter({hasText:'TV calendar could not be loaded: Calendar storage is unavailable'}).waitFor();
 assert.equal(await cal.getByRole('status').filter({hasText:'Nothing scheduled'}).count(),0,'An error is not presented as an empty calendar');
 await page.unroute(failTv);
 await cal.getByRole('button',{name:'Retry loading calendar',exact:true}).click();
 await until(()=>cal.getByRole('alert').count(),n=>n===0,'Calendar alert cleared after retry');
 const invalid=await page.request.get(`${origin}/api/v1/movies/calendar?start=${today}&end=${today}`);
 assert.equal(invalid.status(),400);assert.equal((await invalid.json()).error.code,'invalid_calendar_query');

 // ---- 3. Wanted: lists, filters, selection and bulk search. ----
 await nav.getByRole('button',{name:'Wanted',exact:true}).click();
 await wantedScope('Missing','TV');
 let rows=await rowsOf('Missing episodes');
 assert.equal(rows.length,2,rows.join('\n'));
 assert.ok(rows[0].includes('Harbor Watch')&&rows[0].includes('S01E01')&&rows[0].includes('Harbor arrival'),rows[0]);
 assert.ok(rows[1].includes('S01E02')&&rows[1].includes('Harbor storm'),rows[1]);
 const tvMissing=await api('get','/api/v1/tv/wanted/missing');
 assert.deepEqual(tvMissing.items.map(i=>i.target),[{media_type:'episode',id:arrival},{media_type:'episode',id:storm}]);
 await wanted.getByLabel('Monitoring filter',{exact:true}).selectOption('unmonitored');
 await wanted.getByText('No missing episodes for this filter.',{exact:false}).waitFor();
 await wanted.getByLabel('Monitoring filter',{exact:true}).selectOption('monitored');
 await until(async()=>(await rowsOf('Missing episodes')).length,n=>n===2,'Monitored filter restored');
 await wantedScope('Cutoff unmet','TV');
 rows=await rowsOf('Cutoff unmet episodes');
 assert.equal(rows.length,1,rows.join('\n'));assert.ok(rows[0].includes('Harbor repairs')&&rows[0].includes('HDTV-720p'),rows[0]);
 await wantedScope('Cutoff unmet','Movies');
 rows=await rowsOf('Cutoff unmet movies');
 assert.equal(rows.length,1,rows.join('\n'));assert.ok(rows[0].includes('Harbor Rerun')&&rows[0].includes('HDTV-720p'),rows[0]);
 await wantedScope('Missing','Movies');
 rows=await rowsOf('Missing movies');
 assert.equal(rows.length,2,rows.join('\n'));
 assert.ok(rows.some(r=>r.includes('Harbor Archive')&&r.includes('Available')));
 assert.ok(rows.some(r=>r.includes('Harbor Premiere')&&r.includes('Not yet available (released)')));
 assert.ok(!rows.join('\n').includes('Harbor Rerun'),'A movie with a file is not missing');
 const availability=wanted.getByLabel('Availability filter',{exact:true});
 await availability.selectOption('available');
 await until(async()=>(await rowsOf('Missing movies')).join('\n'),t=>t.includes('Harbor Archive')&&!t.includes('Harbor Premiere'),'Available filter');
 assert.deepEqual((await api('get','/api/v1/movies/wanted/missing?availability=available')).items.map(i=>i.target),[{media_type:'movie',id:archive.id}]);
 await availability.selectOption('unavailable');
 await until(async()=>(await rowsOf('Missing movies')).join('\n'),t=>t.includes('Harbor Premiere')&&!t.includes('Harbor Archive'),'Unavailable filter');
 await availability.selectOption('all');
 await until(async()=>(await rowsOf('Missing movies')).length,n=>n===2,'Availability reset');

 // Park the worker so admitted searches stay genuinely queued until Activity is watching.
 await api('post','/api/fixture/stop-command-worker',undefined,204);stoppedWorker=true;
 await chooseIndexer(indexer.name);
 for(const title of ['Harbor Premiere','Harbor Archive'])await table('Missing movies').getByRole('checkbox',{name:new RegExp(`^Select ${title} \\(\\d{4}\\)$`)}).check();
 const movieReceipt=await admitted(()=>wantedSearch.getByRole('button',{name:'Search selected (2)',exact:true}).click());
 assert.deepEqual(movieReceipt.commands.map(c=>c.target).sort((a,b)=>a.id-b.id),[{media_type:'movie',id:premiere.id},{media_type:'movie',id:archive.id}].sort((a,b)=>a.id-b.id));
 await wantedSearch.getByText('Queued 2 movies for automatic search; 2 matched.',{exact:true}).waitFor();
 await wantedScope('Missing','TV');
 await chooseIndexer(indexer.name);
 const arrivalBox=table('Missing episodes').getByRole('checkbox',{name:'Select Harbor Watch S01E01 Harbor arrival',exact:true});
 await arrivalBox.focus();await page.keyboard.press('Space');assert.equal(await arrivalBox.isChecked(),true);
 await table('Missing episodes').getByRole('checkbox',{name:'Select Harbor Watch S01E02 Harbor storm',exact:true}).check();
 const tvReceipt=await admitted(()=>wantedSearch.getByRole('button',{name:'Search selected (2)',exact:true}).click());
 assert.deepEqual(tvReceipt.commands.map(c=>c.target),[{media_type:'episode',id:arrival},{media_type:'episode',id:storm}]);
 await wantedSearch.getByText('Queued 2 episodes for automatic search; 2 matched.',{exact:true}).waitFor();
 await shot('scn007-wanted-tv-searched-desktop');
 // Server state: typed commands per target, one per selection, none crossing domains.
 const commandIds=[...tvReceipt.commands,...movieReceipt.commands].map(c=>c.command_id);
 for(const [type,id] of [['episode',arrival],['episode',storm],['movie',premiere.id],['movie',archive.id]]){
  const stored=await commandsFor(type,id);
  assert.equal(stored.length,1,`${type} ${id}`);
  assert.deepEqual([stored[0].mode,stored[0].status,stored[0].source.indexer_id,stored[0].source.selection_kind,stored[0].source.media_type],['automatic','queued',indexer.id,'default',type==='episode'?'tv':'movies']);
  assert.ok(commandIds.includes(stored[0].id));
 }
 assert.equal((await commandsFor('episode',regatta)).length,0,'Unselected future episode was not searched');
 assert.equal((await commandsFor('movie',rerun.id)).length,0,'Unselected cutoff movie was not searched');

 // ---- 4. Activity: polled transitions queued -> running -> failed(no_eligible_release). ----
 await nav.getByRole('button',{name:'Activity',exact:true}).click();
 for(const id of commandIds)await until(()=>searchState(id),s=>s?.status==='queued'&&s.text.includes('Queued'),`Activity shows queued ${id}`);
 assert.ok((await searchState(tvReceipt.commands[0].command_id)).text.includes(`Episode ${arrival}`));
 assert.ok((await searchState(movieReceipt.commands.find(c=>c.target.id===premiere.id).command_id)).text.includes(`Movie ${premiere.id}`),'Equal numeric IDs keep their typed target labels');
 await searches.getByText('4 search commands: 4 queued.',{exact:true}).waitFor();
 // A manual download refresh admitted while the worker is parked.
 await activity.getByLabel(/^Download client/).selectOption({label:client.name});
 await until(()=>activity.getByRole('button',{name:'Refresh downloads',exact:true}).isEnabled(),v=>v,'Refresh downloads enabled');
 const refreshReply=page.waitForResponse(r=>r.request().method()==='POST'&&new URL(r.url()).pathname==='/api/v1/commands');
 await activity.getByRole('button',{name:'Refresh downloads',exact:true}).click();
 const refreshResponse=await refreshReply;assert.equal(refreshResponse.status(),202,await refreshResponse.text());
 const refresh=await refreshResponse.json();
 assert.equal(refresh.status,'queued');
 await until(async()=>(await refreshRow(refresh.id).count())?refreshRow(refresh.id).innerText():'',t=>/\bqueued\b/.test(t),'Refresh row queued');
 await shot('scn007-activity-queued-desktop');

 const navigationsBefore=mainNavigations,readsBefore=searchListReads.length;
 torznab.held=true;
 await api('post','/api/fixture/start-command-worker',undefined,204);stoppedWorker=false;
 const running=await until(async()=>{for(const id of commandIds){const c=await api('get',`/api/v1/search/commands/${id}`);if(c.status==='running')return c;}return null;},c=>c!==null,'A search was claimed by the worker',15000,100);
 await until(()=>searchState(running.id),s=>s?.status==='running'&&s.text.includes('Running'),'Activity shows the running search without a reload',HOLD_LIMIT_MS-500);
 const others=await Promise.all(commandIds.filter(id=>id!==running.id).map(searchState));
 assert.ok(others.every(s=>['queued','running'].includes(s?.status)),JSON.stringify(others));
 await shot('scn007-activity-running-desktop');
 assert.equal(torznab.autoReleased,0,'The running state was observed before the hold limit');
 releaseHeld();
 for(const id of commandIds)await until(()=>searchState(id),s=>s?.status==='failed'&&s.text.includes('Failed')&&s.text.includes('No eligible release found (no_eligible_release)'),`Activity shows the settled search ${id}`,30000,200);
 await searches.getByText('4 search commands: 4 failed.',{exact:true}).waitFor();
 await until(async()=>(await refreshRow(refresh.id).count())?refreshRow(refresh.id).innerText():'',t=>/\bsucceeded\b/.test(t),'Refresh row succeeded via polling',30000,200);
 assert.equal(mainNavigations,navigationsBefore,'No page reload or navigation while observing transitions');
 assert.ok(searchListReads.length-readsBefore>=3,`Activity polled the search list (${searchListReads.length-readsBefore} reads)`);
 for(const id of commandIds){
  const c=await api('get',`/api/v1/search/commands/${id}`);
  assert.deepEqual([c.status,c.error_code,c.attempts,c.fetched,c.fetch_complete,c.selected_candidate_id],['failed','no_eligible_release',1,0,true,null],id);
  assert.ok(c.started_at!==null&&c.completed_at!==null);
 }
 assert.equal((await api('get',`/api/v1/commands/${refresh.id}`)).status,'succeeded');
 assert.equal(torznab.autoReleased,0);
 // The searches reached the owned indexer with each domain's own identity and category.
 const searchesSeen=torznab.requests.filter(r=>r.t!=='caps');
 assert.ok(searchesSeen.length>=4&&searchesSeen.every(r=>r.key),JSON.stringify(searchesSeen));
 assert.ok(searchesSeen.some(r=>r.cat==='5030'&&r.tvdbid==='118'&&r.season==='1'&&r.ep==='1'),JSON.stringify(searchesSeen));
 assert.ok(searchesSeen.some(r=>r.cat==='5030'&&r.tvdbid==='118'&&r.season==='1'&&r.ep==='2'));
 assert.ok(searchesSeen.some(r=>r.cat==='2000'&&r.tmdbid==='118')&&searchesSeen.some(r=>r.cat==='2000'&&r.tmdbid==='119'));
 assert.ok(!searchesSeen.some(r=>r.tmdbid==='120'||r.ep==='3'||r.ep==='4'),'Only selected targets were searched');
 assert.ok(searchesSeen.every(r=>r.cat==='5030'?r.tmdbid===null:r.cat==='2000'&&r.tvdbid===null&&r.season===null&&r.ep===null),`Each domain searches with its own identity only: ${JSON.stringify(searchesSeen)}`);
 await shot('scn007-activity-settled-desktop');
 // Status filter and its empty state; then a polled read error and its recovery.
 const statusFilter=searches.getByLabel(/^Search status/);
 await statusFilter.selectOption('running');
 await searches.getByRole('cell',{name:'No running search commands.',exact:true}).waitFor();
 await statusFilter.selectOption('failed');
 for(const id of commandIds)await until(()=>searchState(id),s=>s?.status==='failed','Failed filter rows');
 await statusFilter.selectOption('');
 const failSearchList=url=>url.pathname==='/api/v1/search/commands'&&!url.searchParams.has('target_type');
 await page.route(failSearchList,route=>route.fulfill({status:503,contentType:'application/json',body:JSON.stringify({error:{code:'command_storage_error',message:'Command storage is unavailable'}})}));
 await searches.getByRole('alert').filter({hasText:'Search commands could not be loaded: Command storage is unavailable Showing the last successful read.'}).waitFor({timeout:15000});
 await page.unroute(failSearchList);
 await until(()=>searches.getByRole('alert').count(),n=>n===0,'Polling recovers from the read error without a reload',15000,200);

 // ---- 5. Bulk unmonitor from Wanted is reflected in Calendar (both domains). ----
 await nav.getByRole('button',{name:'Wanted',exact:true}).click();
 await wantedScope('Missing','TV');
 await until(async()=>(await rowsOf('Missing episodes')).length,n=>n===2,'TV missing rows');
 await table('Missing episodes').getByRole('checkbox',{name:'Select Harbor Watch S01E02 Harbor storm',exact:true}).check();
 await wanted.getByRole('button',{name:'Unmonitor selected',exact:true}).click();
 await wanted.getByText('Unmonitored 1 episode.',{exact:true}).waitFor();
 await until(async()=>(await rowsOf('Missing episodes')).join('\n'),t=>t.includes('Harbor arrival')&&!t.includes('Harbor storm'),'Unmonitored episode leaves the monitored list');
 assert.equal((await api('get',`/api/v1/episodes/${storm}`)).monitored,false);
 await wanted.getByLabel('Monitoring filter',{exact:true}).selectOption('unmonitored');
 await until(async()=>(await rowsOf('Missing episodes')).join('\n'),t=>t.includes('Harbor storm')&&t.includes('Episode unmonitored'),'Unmonitored episode partition');
 await wantedScope('Missing','Movies');
 await until(async()=>(await rowsOf('Missing movies')).length,n=>n===2,'Movie missing rows');
 await table('Missing movies').getByRole('checkbox',{name:new RegExp('^Select Harbor Premiere \\(\\d{4}\\)$')}).check();
 await wanted.getByRole('button',{name:'Unmonitor selected',exact:true}).click();
 await wanted.getByText('Unmonitored 1 movie.',{exact:true}).waitFor();
 await until(async()=>(await rowsOf('Missing movies')).join('\n'),t=>t.includes('Harbor Archive')&&!t.includes('Harbor Premiere'),'Unmonitored movie leaves the monitored list');
 assert.equal((await api('get',`/api/v1/movies/${premiere.id}`)).monitored,false);
 assert.equal((await api('get',`/api/v1/episodes/${arrival}`)).monitored,true,'Same numeric ID in the other domain is untouched');

 await nav.getByRole('button',{name:'Calendar',exact:true}).click();
 await cal.getByRole('heading',{name:'Calendar',exact:true}).waitFor();
 await showMonthOf(airDay[storm]);
 let text=await cellText(airDay[storm]);
 assert.ok(!text.includes('Harbor storm'),`Unmonitored episode hidden by default: ${text}`);
 for(const date of Object.values(releaseDays)){await showMonthOf(date);assert.ok(!(await cellText(date)).includes('Harbor Premiere'),`Unmonitored movie hidden on ${date}`);}
 assert.deepEqual(uids(await feed(await cal.getByLabel('TV feed link',{exact:true}).inputValue())),[arrival,repairs,regatta].map(id=>`episode-${id}@hrrdarr`));
 assert.deepEqual(uids(await feed(await cal.getByLabel('Movies feed link',{exact:true}).inputValue())),[]);
 await cal.getByRole('checkbox',{name:'Include unmonitored'}).check();
 await showMonthOf(airDay[storm]);
 await until(()=>cellText(airDay[storm]),t=>t.includes('Harbor storm')&&t.includes('Unmonitored'),'Unmonitored episode shown when requested');
 for(const date of Object.values(releaseDays)){await showMonthOf(date);await until(()=>cellText(date),t=>t.includes('Harbor Premiere')&&t.includes('Unmonitored'),`Unmonitored movie shown on ${date}`);}
 const withHidden=await api('get',`/api/v1/tv/calendar?start=${addDays(today,-7)}&end=${addDays(today,14)}&unmonitored=true`);
 assert.deepEqual(withHidden.items.filter(i=>!i.effective_monitored).map(i=>i.target),[{media_type:'episode',id:storm}]);
 const moviesHidden=await api('get',`/api/v1/movies/calendar?start=${addDays(today,-7)}&end=${addDays(today,14)}&unmonitored=true`);
 assert.ok(moviesHidden.items.length===3&&moviesHidden.items.every(i=>i.target.id===premiere.id&&i.monitored===false));
 const tvLink=await cal.getByLabel('TV feed link',{exact:true}).inputValue();
 assert.equal(new URL(tvLink).searchParams.get('unmonitored'),'true');
 assert.deepEqual(uids(await feed(tvLink)),[arrival,storm,repairs,regatta].map(id=>`episode-${id}@hrrdarr`));
 assert.deepEqual(uids(await feed(await cal.getByLabel('Movies feed link',{exact:true}).inputValue())),['in_cinemas','digital','physical'].map(type=>`movie-${premiere.id}-${type}@hrrdarr`));
 await showMonthOf(airDay[storm]);
 await shot('scn007-calendar-unmonitored-desktop');

 await page.setViewportSize({width:390,height:844});
 await nav.getByRole('button',{name:'Activity',exact:true}).click();
 await searches.getByText('4 search commands: 4 failed.',{exact:true}).waitFor();
 await shot('scn007-activity-mobile');
 assert.equal(torznab.rejected,0,'The owned indexer saw only valid keyed requests');
 assert.deepEqual(errors,[]);
 console.log(JSON.stringify({scn007_browser:'passed',zone:ZONE,collision:{episode:arrival,movie:premiere.id},search_commands:commandIds,refresh_command:refresh.id,indexer_requests:torznab.requests.map(r=>[r.t,r.cat,r.tvdbid??r.tmdbid??r.q,r.season,r.ep].filter(v=>v!==null).join(' ')),activity_search_reads:searchListReads.length}));
}finally{
 releaseHeld();
 if(stoppedWorker)await page.request.post(`${origin}/api/fixture/start-command-worker`).catch(()=>{});
 await browser.close();
 mock.closeAllConnections();await new Promise(resolve=>mock.close(resolve));
}
