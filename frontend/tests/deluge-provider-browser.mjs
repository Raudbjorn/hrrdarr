// Owned library_ui_fixture + in-process Deluge Web JSON-RPC mock on 127.0.0.1:0 only; never a live Deluge.
// Drives the real ProviderPanel/ActivityPanel against real provider/command endpoints for TV and movies.
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {mkdir,realpath} from 'node:fs/promises';
import {isAbsolute,join,resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
const {chromium}=await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE||'/usr/lib/node_modules/playwright/index.mjs').href);
const origin=process.env.UI_URL;assert.equal(new URL(origin).hostname,'127.0.0.1');
const fixture=JSON.parse(process.env.UI_FIXTURE_JSON);assert.equal(new URL(fixture.api).hostname,'127.0.0.1');
// Evidence stays in an explicitly selected ignored iteration116 directory, independent of cwd.
assert.ok(process.env.UI_SCREENSHOT_DIR&&isAbsolute(process.env.UI_SCREENSHOT_DIR),'Set an absolute UI_SCREENSHOT_DIR');
const artifactPattern=/\/\.do-not-commit\/loop\/iteration116(?:[-/]|$)/;
const screenshotRequested=resolve(process.env.UI_SCREENSHOT_DIR);assert.match(screenshotRequested,artifactPattern,'Screenshots must stay under ignored iteration116 artifacts');
await mkdir(screenshotRequested,{recursive:true});
const screenshotDirectory=await realpath(screenshotRequested);assert.match(screenshotDirectory,artifactPattern,'Screenshot directory must resolve to ignored iteration116 artifacts');
const screenshotRun=`${Date.now()}-${process.pid}`,screenshots=[];

// ---- Owned Deluge Web JSON-RPC mock: read-only subset (login, daemon attach, version, config, labels, status). ----
const URL_BASE='/deluge',RPC_PATH=`${URL_BASE}/json`,MAX_OBSERVATIONS=512,MAX_BODY_BYTES=64*1024,SESSION='dg-browser-session-token';
const PASSWORD_ORIGINAL='dg-browser-pass-original-5e1b',PASSWORD_ROTATED='dg-browser-pass-rotated-a94c';
const READ_ONLY_METHODS=new Set(['auth.login','web.connected','daemon.get_version','core.get_config_value','label.get_labels','core.get_torrents_status']);
const mock={password:PASSWORD_ORIGINAL,labelPlugin:true,observations:[],unexpected:[],torrents:[]};
const names={tv:{active:'Dg.Browser.Show.S01E01.720p.WEB-DL',done:'Dg.Browser.Show.S01E02.1080p.WEB-DL'},movies:{active:'Dg.Browser.Movie.2024.720p.WEB-DL',done:'Dg.Browser.Movie.2023.1080p.BluRay'}};
const torrent=(n,name,label,state,done)=>[String(n).padStart(40,'0'),{name,state,progress:done/1048576*100,total_wanted:1048576,total_done:done,save_path:'/srv/dg',download_payload_rate:1024,upload_payload_rate:0,eta:30,ratio:0,seeding_time:0,label,is_finished:done===1048576}];
function seed(){
 mock.torrents=[torrent(1,names.tv.active,'tv-dg','Downloading',524288),torrent(2,names.tv.done,'tv-dg','Seeding',1048576),torrent(3,names.movies.active,'movies-dg','Downloading',524288),torrent(4,names.movies.done,'movies-dg','Seeding',1048576),torrent(5,'Foreign.Job.2020','other-app','Downloading',524288),torrent(6,'Foreign.Done.2020','other-app','Seeding',1048576)];
}
const cookieClass=value=>value===undefined?'none':value===`_session_id=${SESSION}`?'current':'other';
const server=createServer((request,response)=>{
 const url=new URL(request.url,'http://127.0.0.1');
 const send=(status,body,headers={})=>{response.writeHead(status,{'content-type':typeof body==='string'?'text/html':'application/json',...headers});response.end(typeof body==='string'?body:JSON.stringify(body));};
 const chunks=[];let size=0,overflow=false;
 request.on('data',chunk=>{size+=chunk.length;if(size>MAX_BODY_BYTES){overflow=true;return;}chunks.push(chunk);});
 request.on('end',()=>{
  let body=null;try{body=JSON.parse(Buffer.concat(chunks).toString('utf8'));}catch{}
  // Record only classified credential evidence; the raw password never enters observations.
  const login=body?.method==='auth.login';
  const row={method:request.method,path:url.pathname,rpc:typeof body?.method==='string'?body.method:null,auth:login?(body.params?.[0]===mock.password?'current':[PASSWORD_ORIGINAL,PASSWORD_ROTATED].includes(body.params?.[0])?'stale':'other'):cookieClass(request.headers.cookie),content_type:request.headers['content-type']};
  const reply=(status,payload,headers)=>{row.status=status;if(mock.observations.length<MAX_OBSERVATIONS)mock.observations.push(row);send(status,payload,headers);};
  if(overflow||request.method!=='POST'||url.pathname!==RPC_PATH||!body||typeof body.method!=='string'){mock.unexpected.push(row);return reply(404,'<h1>404: Not Found</h1>');}
  if(!READ_ONLY_METHODS.has(row.rpc)){mock.unexpected.push(row);return reply(200,{result:null,error:{message:'method not allowed by owned mock',code:3},id:body.id});}
  if(login){const good=row.auth==='current';return reply(200,{result:good,error:null,id:body.id},good?{'set-cookie':`_session_id=${SESSION}; Path=/; HttpOnly`}:{});}
  if(row.auth!=='current')return reply(200,{result:null,error:{message:'Not authenticated',code:1},id:body.id});
  if(['label.get_labels'].includes(row.rpc)&&!mock.labelPlugin)return reply(200,{result:null,error:{message:'Unknown method',code:3},id:body.id});
  const result={
   'web.connected':()=>true,
   'daemon.get_version':()=> '2.1.1',
   'core.get_config_value':()=> '/srv/dg',
   'label.get_labels':()=>['tv-dg','movies-dg'],
   'core.get_torrents_status':()=>Object.fromEntries(mock.torrents.filter(([,t])=>t.label===body.params[0].label)),
  }[row.rpc]();
  return reply(200,{result,error:null,id:body.id});
 });
});
await new Promise((ok,fail)=>{server.once('error',fail);server.listen(0,'127.0.0.1',ok);});
const dgPort=server.address().port,endpoint=`http://127.0.0.1:${dgPort}${URL_BASE}`;
assert.ok(![8112,58846,9091,8080,8989,7878,8787,8686,9696,8096,32400,8083,5076,9117].includes(dgPort),'Mock must not occupy a live-service or default Deluge port');
const since=mark=>mock.observations.slice(mark);
const rpcCalls=rows=>rows.filter(row=>row.status===200).map(row=>row.rpc);
function assertReadOnly(){assert.deepEqual(mock.unexpected,[],'Mock received a mutation or unexpected request');assert.ok(mock.observations.every(row=>READ_ONLY_METHODS.has(row.rpc)),'Only read methods may be sent');assert.ok(mock.observations.length<MAX_OBSERVATIONS,'Mock observations truncated');}

const browser=await chromium.launch({headless:true,...(process.env.PLAYWRIGHT_EXECUTABLE?{executablePath:process.env.PLAYWRIGHT_EXECUTABLE}:{})});
const page=await browser.newPage({viewport:{width:1280,height:900}});page.setDefaultTimeout(20000);
const errors=[];page.on('pageerror',error=>errors.push(error.message));
page.on('dialog',dialog=>{errors.push(`Unexpected dialog: ${dialog.message()}`);void dialog.dismiss();});
// Secrets typed into the UI must never appear in an API response body.
const secrets=[PASSWORD_ORIGINAL,PASSWORD_ROTATED,SESSION];
const leaks=[];page.on('response',async response=>{if(!new URL(response.url()).pathname.startsWith('/api/'))return;try{const body=await response.text();if(secrets.some(s=>body.includes(s)))leaks.push(new URL(response.url()).pathname);}catch{}});
const nav=page.getByRole('navigation',{name:'Workspace'}),editor=page.getByRole('article',{name:'Provider editor'}),activity=page.getByRole('region',{name:'Activity',exact:true});
async function screenshot(name){const path=join(screenshotDirectory,`iteration116-deluge-${name}-${screenshotRun}.png`);await page.screenshot({path,fullPage:true});screenshots.push(path);}
async function api(method,path,data,status=200){const r=await page.request[method](origin+path,{...(data===undefined?{}:{data}),timeout:40000});const text=await r.text();assert.equal(r.status(),status,text);assert.ok(!secrets.some(s=>text.includes(s)),`Secret echoed by ${path}`);return status===204?null:JSON.parse(text);}
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
  await ready(button);return {body,calls:since(mark)};
 }
 assert.fail('Saved Test stayed busy');
}
const disableSchedules=async provider=>{for(const media_type of ['tv','movies']){const prior=(await api('get','/api/v1/download-refresh/schedules')).find(s=>s.target.provider_id===provider.id&&s.target.media_type===media_type);await api('put','/api/v1/download-refresh/schedules',{target:{provider_id:provider.id,media_type},provider_revision:provider.revision,revision:prior?.revision??null,enabled:false,interval_seconds:60});}};
function assertAuth(calls,expected){assert.ok(calls.length>0,'Expected RPC traffic');for(const row of calls){assert.equal(row.auth,expected,JSON.stringify(row));assert.equal(row.path,RPC_PATH,'URL base and /json must be preserved');assert.match(row.content_type??'',/^application\/json/);}}

async function expectEditor(values){
 assert.equal(await field('Provider type').inputValue(),'deluge');assert.equal(await field('Provider type').isDisabled(),true);
 assert.equal(await field('Endpoint').inputValue(),endpoint);
 assert.equal(await field('TV scope').isChecked(),true);assert.equal(await field('Movie scope').isChecked(),true);
 for(const [label,value] of Object.entries(values))assert.equal(typeof value==='boolean'?await field(label).isChecked():await field(label).inputValue(),value,label);
 assert.equal(await field('Credential action').inputValue(),'preserve');assert.equal(await editor.getByLabel('API key',{exact:true}).count(),0,'Password input must not be prefilled after reload');
 await editor.getByText(/Credentials: stored \(write-only\)\./).waitFor();
 const html=await page.content();assert.ok(!secrets.some(s=>html.includes(s)),'Secret rendered into the DOM');
}
const scopes=provider=>({tv:provider.settings.tv,movies:provider.settings.movies});

try{
 await page.goto(origin);
 const before=(await api('get','/api/v1/providers')).total;
 // 1. Create both scopes through the real editor.
 await nav.getByRole('button',{name:'Providers',exact:true}).click();
 await ready(page.getByRole('button',{name:'New provider',exact:true}));await page.getByRole('button',{name:'New provider',exact:true}).click();
 await field('Provider type').selectOption('deluge');
 assert.equal(await editor.getByLabel('Username',{exact:true}).count(),0,'Deluge has no username');
 assert.equal(await field('Enabled').isChecked(),false);await field('Enabled').check();
 await field('Name').fill('Deluge browser client');await field('Endpoint').fill(endpoint);
 await field('Movie scope').check();
 await field('TV download category').fill('tv-dg');
 await field('TV recent priority').selectOption('first');await field('TV older priority').selectOption('last');
 await field('Movie recent priority').selectOption('last');await field('Movie older priority').selectOption('first');
 await field('Movie add paused').check();
 await field('Credential action').selectOption('replace');await field('API key').fill(PASSWORD_ORIGINAL);
 // The UI rejects an invalid or shared label before sending anything.
 await field('Movie download category').fill('Movies');
 await ready(editor.getByRole('button',{name:'Save provider',exact:true}));await editor.getByRole('button',{name:'Save provider',exact:true}).click();
 await page.getByRole('alert').filter({hasText:'Movie label: use 1 to 64 lowercase letters'}).waitFor();
 await field('Movie download category').fill('tv-dg');
 await editor.getByRole('button',{name:'Save provider',exact:true}).click();
 await page.getByRole('alert').filter({hasText:'TV and movie labels must differ so each domain only sees its own downloads.'}).waitFor();
 assert.equal((await api('get','/api/v1/providers')).total,before,'Rejected drafts must not be saved');
 assert.equal(mock.observations.length,0,'Client-side rejections must not contact the client');
 await field('Movie download category').fill('movies-dg');
 await screenshot('editor-draft');
 const created=await save('POST');
 await disableSchedules(created);
 assert.equal(created.settings.implementation,'deluge');assert.equal(created.settings.endpoint,endpoint);assert.equal(created.has_credentials,true);assert.equal(created.enabled,true);assert.equal(created.test_status,'never_tested');
 let stored=await api('get',`/api/v1/providers/${created.id}`);
 assert.deepEqual(scopes(stored),{
  tv:{category:'tv-dg',recent_priority:'first',older_priority:'last',add_paused:false,tags:[]},
  movies:{category:'movies-dg',recent_priority:'last',older_priority:'first',add_paused:true,tags:[]},
 });
 assert.equal(stored.has_credentials,true);assert.equal(stored.revision,created.revision);
 await page.reload();await openProvider(created);
 const savedValues={'TV download category':'tv-dg','TV recent priority':'first','TV older priority':'last','TV add paused':false,'Movie download category':'movies-dg','Movie recent priority':'last','Movie older priority':'first','Movie add paused':true};
 await expectEditor(savedValues);
 // 2a. Saved Test: login, daemon attach check, version, config, labels, then each domain's torrent list.
 seed();
 let tested=await testSaved(created,200);
 assert.deepEqual(tested.body.result,{client_version:'2.1.1',domains:['tv','movies'],warnings:[]});assert.equal(tested.body.revision,created.revision);
 assert.deepEqual(rpcCalls(tested.calls),['auth.login','web.connected','daemon.get_version','core.get_config_value','label.get_labels','core.get_torrents_status','core.get_torrents_status']);
 assertAuth(tested.calls,'current');
 const result=page.getByRole('status').filter({hasText:'Connection test result'});
 await result.getByText(`Tested revision ${created.revision}: tv, movies.`,{exact:true}).waitFor();
 await result.getByText('Deluge 2.1.1. The Label plugin is enabled and torrent lists are readable for each configured label.',{exact:true}).waitFor();
 stored=await api('get',`/api/v1/providers/${created.id}`);assert.equal(stored.test_status,'success');assert.equal(stored.last_test.revision,created.revision);
 await editor.getByText(/^Saved test: success at .*\(revision \d+\)\.$/).waitFor();
 await screenshot('test-success');
 // 2b. A disabled Label plugin is a distinct, actionable failure.
 mock.labelPlugin=false;
 tested=await testSaved(created,422);assert.equal(tested.body.error.code,'label_plugin_unavailable');
 await page.getByRole('alert').filter({hasText:'The client cannot hold labels'}).waitFor();
 mock.labelPlugin=true;
 // 2c. The Web UI password rotates: the saved Test logs in with the stale credential.
 mock.password=PASSWORD_ROTATED;
 tested=await testSaved(created,502);assert.equal(tested.body.error.code,'authentication');assertAuth(tested.calls,'stale');
 assert.deepEqual(tested.calls.map(c=>c.rpc),['auth.login'],'Authentication failure ends the probe');
 await page.getByRole('alert').filter({hasText:'Provider rejected authentication'}).waitFor();
 stored=await api('get',`/api/v1/providers/${created.id}`);assert.equal(stored.test_status,'failure');assert.equal(stored.last_test.error_code,'authentication');
 await screenshot('test-authentication-failure');
 const beforeReplace=scopes(stored);
 await field('Credential action').selectOption('replace');await field('API key').fill(PASSWORD_ROTATED);
 const replaced=await save('PUT');assert.equal(replaced.revision,created.revision+1);assert.equal(replaced.has_credentials,true);assert.deepEqual(scopes(replaced),beforeReplace);assert.equal(replaced.settings.endpoint,endpoint);
 assert.equal(await editor.getByLabel('API key',{exact:true}).count(),0,'Secret field must clear after save');
 tested=await testSaved(replaced,200);assertAuth(tested.calls,'current');
 // 3. Edit only the movie scope; TV scope, endpoint and credentials are preserved.
 const beforeEdit=await api('get',`/api/v1/providers/${created.id}`);
 await field('Movie recent priority').selectOption('first');await field('Movie add paused').uncheck();
 const edited=await save('PUT');assert.equal(edited.revision,replaced.revision+1);
 stored=await api('get',`/api/v1/providers/${created.id}`);
 assert.deepEqual(stored.settings.tv,beforeEdit.settings.tv,'TV scope must be preserved');
 assert.deepEqual(stored.settings.movies,{...beforeEdit.settings.movies,recent_priority:'first',add_paused:false});
 assert.equal(stored.has_credentials,true);assert.equal(stored.settings.endpoint,endpoint);
 await page.reload();await openProvider(created);await expectEditor({...savedValues,'Movie recent priority':'first','Movie add paused':false});
 // Draft Test (API only) writes nothing.
 const mark0=mock.observations.length;
 const draft=await api('post','/api/v1/providers/test-draft',{source:null,config:{name:'Unsaved Deluge draft',enabled:stored.enabled,priority:stored.priority,settings:stored.settings,credentials:{kind:'api_key',api_key:PASSWORD_ROTATED}}});
 assert.equal(draft.source,null);assert.deepEqual(draft.result.domains,['tv','movies']);assertAuth(since(mark0),'current');
 assert.equal((await api('get','/api/v1/providers')).total,before+1,'Draft must not create a provider');
 // 4. Activity refresh for each domain renders only that category's queue and history.
 await disableSchedules(stored);
 await nav.getByRole('button',{name:'Activity',exact:true}).click();await idle(activity);
 const clientSelect=activity.getByLabel(/^Download client/);await ready(clientSelect);
 assert.equal(await clientSelect.getByRole('option',{name:'Deluge browser client',exact:true}).count(),1,'Deluge client must be selectable in Activity');
 await clientSelect.selectOption(created.id);await idle(activity);
 for(const [domain,category,own,other] of [['tv','tv-dg',[1,2],[3,4,5,6]],['movies','movies-dg',[3,4],[1,2,5,6]]]){
  await activity.getByLabel(/^Refresh media/).selectOption(domain);await idle(activity);
  const refresh=activity.getByRole('button',{name:'Refresh downloads',exact:true});await ready(refresh);
  const mark=mock.observations.length;
  const admitted=page.waitForResponse(r=>new URL(r.url()).pathname==='/api/v1/commands'&&r.request().method()==='POST');
  await refresh.click();const reply=await admitted;assert.equal(reply.status(),202,await reply.text());const command=await reply.json();
  assert.equal(command.name,'refresh_downloads');assert.deepEqual(command.target,{provider_id:created.id,media_type:domain});assert.equal(command.provider_revision,stored.revision);
  const finished=await until(()=>api('get',`/api/v1/commands/${command.id}`),c=>['succeeded','failed','cancelled'].includes(c.status),`Refresh ${domain} did not finish`);
  assert.equal(finished.status,'succeeded',JSON.stringify(finished));assert.equal(finished.items_observed,2);
  const calls=since(mark).filter(c=>c.status===200);assertAuth(calls,'current');
  assert.deepEqual(rpcCalls(calls),['auth.login','web.connected','daemon.get_version','core.get_config_value','label.get_labels','core.get_torrents_status'],`Refresh ${domain} connects then reads that label's torrents`);
  const snapshot=await until(()=>api('get',`/api/v1/queue?provider_id=${created.id}&media_type=${domain}&limit=25&offset=0`),s=>s.command_id===command.id,`Snapshot ${domain} not from this command`);
  assert.equal(snapshot.total,2);
  assert.deepEqual(snapshot.items.map(i=>[i.download.domain,i.download.category,i.download.name,i.download.status,i.association]).sort(),[[domain,category,names[domain].active,'downloading',null],[domain,category,names[domain].done,'completed',null]].sort());
  await activity.getByRole('button',{name:'Reload activity',exact:true}).click();await idle(activity);
  const table=activity.getByRole('table');
  for(const name of [names[domain].active,names[domain].done]){const row=table.getByRole('row').filter({hasText:name});await row.waitFor();await row.getByText(new RegExp(`^${domain} / ${category}\\s*$`)).waitFor();}
  for(const foreign of ['Foreign.Job.2020','Foreign.Done.2020'])assert.equal(await table.getByRole('row').filter({hasText:foreign}).count(),0,'Foreign torrents must not render');
  await activity.getByText(new RegExp(`Command ${command.id}`)).waitFor();
  await screenshot(`activity-${domain}`);
 }
 assert.equal(await activity.locator('p.error').count(),0,'Activity must render without errors');
 assertReadOnly();assert.deepEqual(leaks,[],'Secret leaked into an API response');assert.deepEqual(errors,[]);
 console.log(JSON.stringify({screenshots,mock_requests:mock.observations.length,provider_busy_retries:busyRetries,methods:[...new Set(mock.observations.map(o=>o.rpc))].sort()}));
 console.log('PASS Deluge ProviderPanel: UI create with per-domain labels, queue placement and add-paused (invalid and shared labels rejected client-side without contacting the client), password credential, server readback and reload round trip, saved Test success / Label plugin disabled / authentication failure with credential replacement, movie-only edit preserving TV and credentials, draft Test without persistence, and Activity refresh listing only each domain label. Owned 127.0.0.1 mock only; no live Deluge.');
}catch(error){console.error('PAGESTATE',(await page.locator('body').innerText().catch(()=>'')).slice(0,8000));console.error('MOCK',JSON.stringify(mock.observations.slice(-40)));throw error;}
finally{try{await browser.close();}finally{const closed=new Promise(done=>server.close(()=>done()));server.closeAllConnections();await closed;}}
