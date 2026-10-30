// Owned library_ui_fixture + in-process SABnzbd API mock on 127.0.0.1:0 only; never a live SABnzbd.
// Drives the real ProviderPanel/ActivityPanel against real provider/command endpoints for TV and movies.
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {mkdir,realpath} from 'node:fs/promises';
import {isAbsolute,join,resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
const {chromium}=await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE||'/usr/lib/node_modules/playwright/index.mjs').href);
const origin=process.env.UI_URL;assert.equal(new URL(origin).hostname,'127.0.0.1');
const fixture=JSON.parse(process.env.UI_FIXTURE_JSON);assert.equal(new URL(fixture.api).hostname,'127.0.0.1');
// Evidence stays in an explicitly selected ignored iteration104/106 directory, independent of cwd.
assert.ok(process.env.UI_SCREENSHOT_DIR&&isAbsolute(process.env.UI_SCREENSHOT_DIR),'Set an absolute UI_SCREENSHOT_DIR');
const artifactPattern=/\/\.do-not-commit\/loop\/iteration10[46](?:[-/]|$)/;
const screenshotRequested=resolve(process.env.UI_SCREENSHOT_DIR);assert.match(screenshotRequested,artifactPattern,'Screenshots must stay under ignored iteration104/106 artifacts');
await mkdir(screenshotRequested,{recursive:true});
const screenshotDirectory=await realpath(screenshotRequested);assert.match(screenshotDirectory,artifactPattern,'Screenshot directory must resolve to ignored iteration104/106 artifacts');
const screenshotRun=`${Date.now()}-${process.pid}`,screenshots=[];

// ---- Owned SABnzbd mock: read-only API subset the backend uses for Test and refresh. ----
const URL_BASE='/sabnzbd',MAX_OBSERVATIONS=512;
const KEY_ORIGINAL='sab-browser-key-original-0f3a',KEY_ROTATED='sab-browser-key-rotated-7c21';
const READ_ONLY_MODES=new Set(['version','get_config','queue','history','fullstatus','get_cats']);
const category=name=>({name,order:1,pp:'',script:'Default',dir:name,priority:-100});
const queueSlot=(nzo,filename,cat)=>({status:'Downloading',index:0,password:'',avg_age:'1d',script:'None',direct_unpack:null,mb:'100.00',mbleft:'25.00',mbmissing:'0.0',size:'100 MB',sizeleft:'25 MB',filename,labels:[],priority:'Normal',cat,timeleft:'0:01:40',percentage:'75',nzo_id:nzo,unpackopts:'3'});
const historySlot=(nzo,name,cat)=>({action_line:'',fail_message:'',loaded:false,size:'100 MB',category:cat,pp:'D',retry:0,script:'None',nzb_name:`${name}.nzb`,download_time:10,storage:`/srv/sab/complete/${cat}/${name}`,status:'Completed',completed:1700000000,nzo_id:nzo,downloaded:104857600,report:'',password:'',path:`/srv/sab/incomplete/${name}`,postproc_time:1,name,url:'',md5sum:'',archive:false,bytes:104857600,url_info:'',stage_log:[]});
const mock={key:KEY_ORIGINAL,categories:[{name:'*',order:0,pp:'3',script:'None',dir:'',priority:0},category('tv-sab'),category('movies-sab')],queue:[],history:[],observations:[],unexpected:[]};
const names={tv:{queue:'Sab.Browser.Show.S01E01.720p.WEB-DL',history:'Sab.Browser.Show.S01E02.1080p.WEB-DL'},movies:{queue:'Sab.Browser.Movie.2024.720p.WEB-DL',history:'Sab.Browser.Movie.2023.1080p.BluRay'}};
function seedJobs(domain,cat){
 mock.queue=mock.queue.filter(slot=>slot.cat!==cat).concat(queueSlot(`SABnzbd_nzo_${domain}_q`,names[domain].queue,cat));
 mock.history=mock.history.filter(slot=>slot.category!==cat).concat(historySlot(`SABnzbd_nzo_${domain}_h`,names[domain].history,cat));
}
const sameCategory=(a,b)=>a.toLowerCase()===b.toLowerCase();
const keyClass=value=>value===undefined?'none':value===mock.key?'current':[KEY_ORIGINAL,KEY_ROTATED].includes(value)?'stale':'other';
const server=createServer((request,response)=>{
 const url=new URL(request.url,'http://127.0.0.1'),q=Object.fromEntries(url.searchParams);
 const send=(status,body,type='application/json')=>{response.writeHead(status,{'content-type':type});response.end(typeof body==='string'?body:JSON.stringify(body));};
 // Record only classified key evidence; the raw key never enters observations or logs.
 const row={method:request.method,path:url.pathname,mode:q.mode??null,cat:q.cat??null,start:q.start??null,limit:q.limit??null,output:q.output??null,key:keyClass(q.apikey),params:[...url.searchParams.keys()].sort()};
 if(mock.observations.length<MAX_OBSERVATIONS)mock.observations.push(row);
 if(request.method!=='GET'||url.pathname!==`${URL_BASE}/api`){mock.unexpected.push(row);return send(404,'unexpected path','text/plain');}
 if(!READ_ONLY_MODES.has(row.mode)){mock.unexpected.push(row);return send(400,{status:false,error:'mode not supported by owned mock'});}
 if(row.mode==='version')return send(200,{version:'4.5.1'});
 if(row.key==='none')return send(200,'error: API Key Required\n','text/plain');
 if(row.key!=='current')return send(200,'error: API Key Incorrect\n','text/plain');
 const window=items=>items.slice(Number(q.start??0),Number(q.start??0)+Number(q.limit??items.length));
 if(row.mode==='get_cats')return send(200,{categories:mock.categories.map(c=>c.name)});
 if(row.mode==='get_config')return send(200,{config:{misc:{complete_dir:'/srv/sab/complete',download_dir:'/srv/sab/incomplete',history_retention_option:'all',history_retention_number:1,enable_tv_sorting:0,tv_categories:[],enable_movie_sorting:0,movie_categories:[],enable_date_sorting:0,date_categories:[]},categories:mock.categories,sorters:[]}});
 if(row.mode==='fullstatus')return send(200,{status:{completedir:'/srv/sab/complete'}});
 if(row.mode==='queue'){const slots=window(mock.queue.filter(s=>!row.cat||sameCategory(s.cat,row.cat)));return send(200,{queue:{status:'Downloading',paused:false,noofslots:slots.length,noofslots_total:slots.length,start:0,limit:0,slots,version:'4.5.1'}});}
 const slots=window(mock.history.filter(s=>!row.cat||sameCategory(s.category,row.cat)));return send(200,{history:{noofslots:slots.length,ppslots:0,slots}});
});
await new Promise((ok,fail)=>{server.once('error',fail);server.listen(0,'127.0.0.1',ok);});
const sabOrigin=`http://127.0.0.1:${server.address().port}`,endpoint=`${sabOrigin}${URL_BASE}`;
assert.ok(![8080,8989,7878,8787,8686,9696,8096,32400,8083,5076,9117].includes(server.address().port),'Mock must not occupy a live-service port');
const since=mark=>mock.observations.slice(mark);
// Test probes use version/get_config and one-slot queue reads; automatic refresh reads use 500-slot pages and are excluded.
const testCalls=rows=>rows.filter(row=>row.mode==='version'||row.mode==='get_config'||(row.mode==='queue'&&row.limit==='1'));
function assertReadOnly(){assert.deepEqual(mock.unexpected,[],'Mock received a non-read-only or unexpected request');assert.ok(mock.observations.every(row=>row.mode!=='addurl'),'No addurl may be sent');assert.ok(mock.observations.length<MAX_OBSERVATIONS,'Mock observations truncated');}

const browser=await chromium.launch({headless:true,...(process.env.PLAYWRIGHT_EXECUTABLE?{executablePath:process.env.PLAYWRIGHT_EXECUTABLE}:{})});
const page=await browser.newPage({viewport:{width:1280,height:900}});page.setDefaultTimeout(20000);
const errors=[];page.on('pageerror',error=>errors.push(error.message));
page.on('dialog',dialog=>{errors.push(`Unexpected dialog: ${dialog.message()}`);void dialog.dismiss();});
// Secrets typed into the UI must never appear in an API response body.
const leaks=[];page.on('response',async response=>{if(!new URL(response.url()).pathname.startsWith('/api/'))return;try{const body=await response.text();if(body.includes(KEY_ORIGINAL)||body.includes(KEY_ROTATED))leaks.push(new URL(response.url()).pathname);}catch{}});
const nav=page.getByRole('navigation',{name:'Workspace'}),editor=page.getByRole('article',{name:'Provider editor'}),activity=page.getByRole('region',{name:'Activity',exact:true});
async function screenshot(name){const path=join(screenshotDirectory,`iteration106-sabnzbd-${name}-${screenshotRun}.png`);await page.screenshot({path,fullPage:true});screenshots.push(path);}
async function api(method,path,data,status=200){const r=await page.request[method](origin+path,{...(data===undefined?{}:{data}),timeout:40000});const text=await r.text();assert.equal(r.status(),status,text);assert.ok(!text.includes(KEY_ORIGINAL)&&!text.includes(KEY_ROTATED),`Secret echoed by ${path}`);return status===204?null:JSON.parse(text);}
async function until(read,accept,label,timeoutMs=30000){const end=Date.now()+timeoutMs;let last;while(Date.now()<end){last=await read();if(accept(last))return last;await page.waitForTimeout(100);}assert.fail(`${label}: ${JSON.stringify(last)}`);}
async function ready(locator){await locator.waitFor();await until(()=>locator.isEnabled(),Boolean,'Control remained disabled');}
const idle=region=>until(()=>region.getAttribute('aria-busy'),v=>v==='false','Panel readback incomplete');
// Wrapping <label>s include option text in a select's accessible name, so selects match by prefix.
const SELECTS=new Set(['Provider type','Credential action','TV recent priority','TV older priority','Movie recent priority','Movie older priority']);
const field=label=>SELECTS.has(label)?editor.getByLabel(new RegExp(`^${label}`)):editor.getByLabel(label,{exact:true});
async function openProvider(provider){
 await nav.getByRole('button',{name:'Providers',exact:true}).click();
 const list=page.getByRole('complementary',{name:'Saved providers'});
 await ready(list.getByRole('button',{name:'Refresh providers',exact:true}));await list.getByRole('button',{name:'Refresh providers',exact:true}).click();
 await list.getByRole('button',{name:new RegExp(`^${provider.name} `)}).click();
 await editor.getByRole('heading',{name:provider.name,exact:true}).waitFor();await ready(field('Name'));
}
async function save(method){
 const button=editor.getByRole('button',{name:'Save provider',exact:true});await ready(button);
 const saved=page.waitForResponse(r=>r.request().method()===method&&new URL(r.url()).pathname.startsWith('/api/v1/providers'));
 await button.click();const response=await saved;assert.equal(response.status(),method==='POST'?201:200,await response.text());
 await ready(field('Name'));return response.json();
}
// Automatic refreshes share the per-provider operation lane; a concurrent one yields 429 provider_busy,
// which the UI surfaces as an alert. Retry the user action a bounded number of times.
const BUSY_RETRIES=40,BUSY_WAIT_MS=250;let busyRetries=0;
async function testSaved(provider,status){
 const button=editor.getByRole('button',{name:'Test saved provider',exact:true});
 for(let attempt=0;attempt<=BUSY_RETRIES;attempt++){
  await ready(button);const mark=mock.observations.length;
  const reply=page.waitForResponse(r=>new URL(r.url()).pathname===`/api/v1/providers/${provider.id}/test`&&r.request().method()==='POST');
  await button.click();const response=await reply;const body=await response.json();
  if(response.status()===429&&body.error?.code==='provider_busy'){await page.getByRole('alert').filter({hasText:'Provider operation capacity is busy'}).waitFor();busyRetries++;await page.waitForTimeout(BUSY_WAIT_MS);continue;}
  assert.equal(response.status(),status,JSON.stringify(body));
  await ready(button);return {body,calls:testCalls(since(mark))};
 }
 assert.fail('Saved Test stayed busy');
}
const disableSchedules=async provider=>{for(const media_type of ['tv','movies']){const prior=(await api('get','/api/v1/download-refresh/schedules')).find(s=>s.target.provider_id===provider.id&&s.target.media_type===media_type);await api('put','/api/v1/download-refresh/schedules',{target:{provider_id:provider.id,media_type},provider_revision:provider.revision,revision:prior?.revision??null,enabled:false,interval_seconds:60});}};
// Version is unauthenticated; every other call carries exactly the expected classified key.
function assertKeyed(calls,expected){for(const row of calls){assert.equal(row.output,'json');assert.equal(row.key,row.mode==='version'?'none':expected,JSON.stringify(row));}}
async function expectEditor(values){
 assert.equal(await field('Provider type').inputValue(),'sabnzbd');assert.equal(await field('Provider type').isDisabled(),true);
 assert.equal(await field('Endpoint').inputValue(),endpoint);assert.equal(await field('TV scope').isChecked(),true);assert.equal(await field('Movie scope').isChecked(),true);
 for(const [label,value] of Object.entries(values))assert.equal(await field(label).inputValue(),value,label);
 assert.equal(await field('Credential action').inputValue(),'preserve');assert.equal(await editor.getByLabel('API key',{exact:true}).count(),0,'API key input must not be prefilled after reload');
 await editor.getByText(/Credentials: stored \(write-only\)\./).waitFor();
 const html=await page.content();assert.ok(!html.includes(KEY_ORIGINAL)&&!html.includes(KEY_ROTATED),'Secret rendered into the DOM');
}
const scopes=provider=>({tv:provider.settings.tv,movies:provider.settings.movies});

try{
 await page.goto(origin);
 const before=(await api('get','/api/v1/providers')).total;
 // 1. Create both scopes through the real editor.
 await nav.getByRole('button',{name:'Providers',exact:true}).click();
 await ready(page.getByRole('button',{name:'New provider',exact:true}));await page.getByRole('button',{name:'New provider',exact:true}).click();
 await field('Provider type').selectOption('sabnzbd');
 await editor.getByText('Include any URL base (for example /sabnzbd); the API path is added automatically.').waitFor();
 // The SABnzbd template starts disabled; enabling is an explicit choice.
 assert.equal(await field('Enabled').isChecked(),false);await field('Enabled').check();
 await field('Name').fill('SABnzbd browser client');await field('Endpoint').fill(endpoint);
 await field('Movie scope').check();
 await field('TV download category').fill('tv-sab');await field('Movie download category').fill('movies-sab');
 await field('TV recent priority').selectOption('high');await field('TV older priority').selectOption('low');
 await field('Movie recent priority').selectOption('force');await field('Movie older priority').selectOption('paused');
 // The UI rejects a shared category before sending anything.
 await field('Movie download category').fill('TV-SAB');await field('Credential action').selectOption('replace');await field('API key').fill(KEY_ORIGINAL);
 await ready(editor.getByRole('button',{name:'Save provider',exact:true}));await editor.getByRole('button',{name:'Save provider',exact:true}).click();
 await page.getByRole('alert').filter({hasText:'TV and movie categories must differ so each domain only sees its own downloads.'}).waitFor();
 assert.equal((await api('get','/api/v1/providers')).total,before,'Rejected draft must not be saved');
 await field('Movie download category').fill('movies-sab');
 const created=await save('POST');
 // Explicit schedule suppression keeps automatic reads out of the Test traces (reapplied before Activity).
 await disableSchedules(created);
 assert.equal(created.settings.implementation,'sabnzbd');assert.equal(created.settings.endpoint,endpoint);assert.equal(created.has_credentials,true);assert.equal(created.enabled,true);assert.equal(created.test_status,'never_tested');
 // Saving sends no probe; any 500-slot reads here come from the automatic schedule an enabled client inherits.
 assert.deepEqual(testCalls(mock.observations),[],'Saving must not probe the client');
 let stored=await api('get',`/api/v1/providers/${created.id}`);
 assert.deepEqual({tv:{category:stored.settings.tv.category,recent:stored.settings.tv.recent_priority,older:stored.settings.tv.older_priority},movies:{category:stored.settings.movies.category,recent:stored.settings.movies.recent_priority,older:stored.settings.movies.older_priority}},{tv:{category:'tv-sab',recent:'high',older:'low'},movies:{category:'movies-sab',recent:'force',older:'paused'}});
 assert.equal(stored.has_credentials,true);assert.equal(stored.revision,created.revision);
 const listed=(await api('get','/api/v1/providers')).items.find(p=>p.id===created.id);assert.ok(listed);assert.equal(listed.has_credentials,true);
 // Reload the page: values round-trip from the server; the key is write-only.
 await page.reload();await openProvider(created);
 const savedValues={'TV download category':'tv-sab','Movie download category':'movies-sab','TV recent priority':'high','TV older priority':'low','Movie recent priority':'force','Movie older priority':'paused'};
 await expectEditor(savedValues);
 // 2a. Saved Test succeeds against the owned mock for both scopes.
 let tested=await testSaved(created,200);
 assert.deepEqual(tested.body.result.domains,['tv','movies']);assert.equal(tested.body.result.sabnzbd_version,'4.5.1');assert.deepEqual(tested.body.result.warnings,[]);assert.equal(tested.body.revision,created.revision);
 assert.deepEqual(tested.calls.map(c=>[c.mode,c.cat]),[['version',null],['get_config',null],['queue','tv-sab'],['queue','movies-sab']]);
 assert.ok(tested.calls.every(c=>c.path===`${URL_BASE}/api`),'URL base must be preserved');assertKeyed(tested.calls,'current');
 const result=page.getByRole('status').filter({hasText:'Connection test result'});
 await result.getByText(`Tested revision ${created.revision}: tv, movies.`,{exact:true}).waitFor();await result.getByText('SABnzbd 4.5.1. Configured categories exist with job folders and no sorting.',{exact:true}).waitFor();
 stored=await api('get',`/api/v1/providers/${created.id}`);assert.equal(stored.test_status,'success');assert.equal(stored.last_test.status,'success');assert.equal(stored.last_test.revision,created.revision);
 await editor.getByText(/^Saved test: success at .*\(revision \d+\)\.$/).waitFor();
 await screenshot('test-success');
 // 2b. The client's key rotates: the saved Test sends the stale key and surfaces an authentication alert.
 mock.key=KEY_ROTATED;
 tested=await testSaved(created,502);assert.equal(tested.body.error.code,'authentication');assertKeyed(tested.calls,'stale');
 await page.getByRole('alert').filter({hasText:'Provider rejected authentication'}).waitFor();
 stored=await api('get',`/api/v1/providers/${created.id}`);assert.equal(stored.test_status,'failure');assert.equal(stored.last_test.error_code,'authentication');
 await editor.getByText(/^Saved test: failure at .*: authentication\.$/).waitFor();
 await screenshot('test-authentication-failure');
 // Replace only the credential bundle; scopes must not change.
 const beforeReplace=scopes(stored);
 await field('Credential action').selectOption('replace');await field('API key').fill(KEY_ROTATED);
 const replaced=await save('PUT');assert.equal(replaced.revision,created.revision+1);assert.equal(replaced.has_credentials,true);assert.deepEqual(scopes(replaced),beforeReplace);
 assert.equal(await editor.getByLabel('API key',{exact:true}).count(),0,'Secret field must clear after save');
 tested=await testSaved(replaced,200);assertKeyed(tested.calls,'current');assert.deepEqual(tested.body.result.domains,['tv','movies']);
 // 3. Edit only the movie scope; TV scope and stored credentials are preserved.
 const beforeEdit=await api('get',`/api/v1/providers/${created.id}`);
 await field('Movie download category').fill('movies-sab-new');
 const edited=await save('PUT');assert.equal(edited.revision,replaced.revision+1);
 stored=await api('get',`/api/v1/providers/${created.id}`);
 assert.deepEqual(stored.settings.tv,beforeEdit.settings.tv,'TV scope must be preserved');
 assert.deepEqual(stored.settings.movies,{...beforeEdit.settings.movies,category:'movies-sab-new'},'Only the movie category may change');
 assert.equal(stored.has_credentials,true);assert.equal(stored.settings.endpoint,endpoint);
 await page.reload();await openProvider(created);await expectEditor({...savedValues,'Movie download category':'movies-sab-new'});
 // 2c. Missing category: the configured movie category does not exist in the client.
 tested=await testSaved(edited,422);assert.equal(tested.body.error.code,'sabnzbd_category_missing');assertKeyed(tested.calls,'current');
 assert.ok(!tested.calls.some(c=>c.cat==='movies-sab-new'),'A missing category must fail before job reads');
 await page.getByRole('alert').filter({hasText:'A configured SABnzbd category does not exist; create it in SABnzbd'}).waitFor();
 stored=await api('get',`/api/v1/providers/${created.id}`);assert.equal(stored.test_status,'failure');assert.equal(stored.last_test.revision,edited.revision);
 await screenshot('test-category-missing');
 mock.categories.push(category('movies-sab-new'));
 tested=await testSaved(edited,200);assert.deepEqual(tested.calls.map(c=>[c.mode,c.cat]),[['version',null],['get_config',null],['queue','tv-sab'],['queue','movies-sab-new']]);
 stored=await api('get',`/api/v1/providers/${created.id}`);assert.equal(stored.test_status,'success');
 // Draft Test (API only: ProviderPanel has no draft-test control) writes no configuration or observation.
 const config=extra=>({name:stored.name,enabled:stored.enabled,priority:stored.priority,settings:{...stored.settings,...extra}});
 let mark=mock.observations.length;
 await until(async()=>{const r=await page.request.post(`${origin}/api/v1/providers/test-draft`,{data:{source:{id:stored.id,revision:stored.revision},config:config({movies:{...stored.settings.movies,category:'movies-absent'}})},timeout:40000});return [r.status(),await r.json()];},([status,body])=>status===422&&body.error.code==='sabnzbd_category_missing','Source-bound draft did not report the missing category');
 assertKeyed(testCalls(since(mark)),'current');
 assert.deepEqual(await api('get',`/api/v1/providers/${created.id}`),stored,'Draft must not change the saved provider or observation');
 mark=mock.observations.length;
 const draft=await api('post','/api/v1/providers/test-draft',{source:null,config:{...config({}),name:'Unsaved SAB draft',credentials:{kind:'api_key',api_key:KEY_ROTATED}}});
 assert.equal(draft.source,null);assert.deepEqual(draft.result.domains,['tv','movies']);assertKeyed(testCalls(since(mark)),'current');
 assert.equal((await api('get','/api/v1/providers')).total,before+1,'Draft must not create a provider');
 // 4. Activity refresh for each domain reads the scoped queue and history and renders the snapshot.
 // Explicitly switch off automatic observation for this client so each snapshot is attributable to the manual refresh.
 await disableSchedules(stored);
 const ambient=mock.observations.filter(o=>o.limit==='500').length;
 seedJobs('tv','tv-sab');seedJobs('movies','movies-sab-new');
 await nav.getByRole('button',{name:'Activity',exact:true}).click();await idle(activity);
 const clientSelect=activity.getByLabel(/^Download client/);await ready(clientSelect);
 assert.equal(await clientSelect.getByRole('option',{name:'SABnzbd browser client',exact:true}).count(),1,'SABnzbd client must be selectable in Activity');
 await clientSelect.selectOption(created.id);await idle(activity);
 for(const [domain,cat,other] of [['tv','tv-sab','movies-sab-new'],['movies','movies-sab-new','tv-sab']]){
  await activity.getByLabel(/^Refresh media/).selectOption(domain);await idle(activity);
  const refresh=activity.getByRole('button',{name:'Refresh downloads',exact:true});await ready(refresh);
  mark=mock.observations.length;
  const admitted=page.waitForResponse(r=>new URL(r.url()).pathname==='/api/v1/commands'&&r.request().method()==='POST');
  await refresh.click();const reply=await admitted;assert.equal(reply.status(),202,await reply.text());const command=await reply.json();
  assert.equal(command.name,'refresh_downloads');assert.deepEqual(command.target,{provider_id:created.id,media_type:domain});assert.equal(command.provider_revision,stored.revision);
  const finished=await until(()=>api('get',`/api/v1/commands/${command.id}`),c=>['succeeded','failed','cancelled'].includes(c.status),`Refresh ${domain} did not finish`);
  assert.equal(finished.status,'succeeded',JSON.stringify(finished));assert.equal(finished.items_observed,2);
  const calls=since(mark).filter(c=>c.mode==='queue'||c.mode==='history');
  assert.ok(calls.some(c=>c.mode==='queue'&&c.cat===cat&&c.key==='current')&&calls.some(c=>c.mode==='history'&&c.cat===cat&&c.key==='current'),`Refresh ${domain} did not read scoped queue and history`);
  const snapshot=await until(()=>api('get',`/api/v1/queue?provider_id=${created.id}&media_type=${domain}&limit=25&offset=0`),s=>s.command_id===command.id,`Snapshot ${domain} not from this command`);
  assert.equal(snapshot.total,2);
  assert.deepEqual(snapshot.items.map(i=>[i.download.domain,i.download.category,i.download.name,i.download.status,i.association]).sort(),[[domain,cat,names[domain].history,'completed',null],[domain,cat,names[domain].queue,'downloading',null]].sort());
  assert.ok(snapshot.items.every(i=>i.download.category!==other),'Snapshot must stay within its domain category');
  await activity.getByRole('button',{name:'Reload activity',exact:true}).click();await idle(activity);
  const table=activity.getByRole('table');
  for(const name of [names[domain].queue,names[domain].history]){const row=table.getByRole('row').filter({hasText:name});await row.waitFor();await row.getByText(`${domain} / ${cat}`,{exact:true}).waitFor();}
  await activity.getByText(new RegExp(`Command ${command.id}`)).waitFor();
  await screenshot(`activity-${domain}`);
 }
 assert.equal(await activity.locator('p.error').count(),0,'Activity must render without errors');
 assertReadOnly();assert.deepEqual(leaks,[],'Secret leaked into an API response');assert.deepEqual(errors,[]);
 console.log(JSON.stringify({screenshots,mock_requests:mock.observations.length,automatic_refresh_reads_before_activity:ambient,provider_busy_retries:busyRetries,modes:[...new Set(mock.observations.map(o=>o.mode))].sort()}));
 console.log('PASS SABnzbd ProviderPanel: UI create with distinct TV/movie categories and priorities (shared category rejected client-side), server readback and page-reload round trip with write-only key; saved Test success, stale-key authentication alert, credential-only replace, movie-only edit preserving TV scope and credentials, missing-category alert and recovery; API-only draft tests without persistence; Activity refresh for both domains read scoped queue/history on the owned mock and rendered real snapshots. No addurl or other mutation reached the mock.');
}catch(error){console.error('PAGESTATE',(await page.locator('body').innerText().catch(()=>'')).slice(0,8000));console.error('MOCK',JSON.stringify(mock.observations.slice(-40)));throw error;}
finally{try{await browser.close();}finally{const closed=new Promise(done=>server.close(()=>done()));server.closeAllConnections();await closed;}}
