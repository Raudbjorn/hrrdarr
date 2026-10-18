// Owned library_ui_fixture + in-process Transmission RPC mock on 127.0.0.1:0 only; never a live Transmission.
// Drives the real ProviderPanel/ActivityPanel against real provider/command endpoints for TV and movies.
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {mkdir,realpath} from 'node:fs/promises';
import {isAbsolute,join,resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
const {chromium}=await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE||'/usr/lib/node_modules/playwright/index.mjs').href);
const origin=process.env.UI_URL;assert.equal(new URL(origin).hostname,'127.0.0.1');
const fixture=JSON.parse(process.env.UI_FIXTURE_JSON);assert.equal(new URL(fixture.api).hostname,'127.0.0.1');
// Evidence stays in an explicitly selected ignored iteration111 directory, independent of cwd.
assert.ok(process.env.UI_SCREENSHOT_DIR&&isAbsolute(process.env.UI_SCREENSHOT_DIR),'Set an absolute UI_SCREENSHOT_DIR');
const artifactPattern=/\/\.do-not-commit\/loop\/iteration111(?:[-/]|$)/;
const screenshotRequested=resolve(process.env.UI_SCREENSHOT_DIR);assert.match(screenshotRequested,artifactPattern,'Screenshots must stay under ignored iteration111 artifacts');
await mkdir(screenshotRequested,{recursive:true});
const screenshotDirectory=await realpath(screenshotRequested);assert.match(screenshotDirectory,artifactPattern,'Screenshot directory must resolve to ignored iteration111 artifacts');
const screenshotRun=`${Date.now()}-${process.pid}`,screenshots=[];

// ---- Owned Transmission RPC mock: read-only subset (session-get, torrent-get) used by Test and refresh. ----
const URL_BASE='/transmission',RPC_PATH=`${URL_BASE}/rpc`,MAX_OBSERVATIONS=512,MAX_BODY_BYTES=64*1024;
const USER='tx-browser-user',PASSWORD_ORIGINAL='tx-browser-pass-original-5e1b',PASSWORD_ROTATED='tx-browser-pass-rotated-a94c';
const READ_ONLY_METHODS=new Set(['session-get','torrent-get']);
const DOWNLOAD_DIR='/srv/tx/downloads';
const basic=password=>`Basic ${Buffer.from(`${USER}:${password}`).toString('base64')}`;
const mock={password:PASSWORD_ORIGINAL,session:'tx-session-1',sessions:1,queueEnabled:true,torrents:[],observations:[],unexpected:[]};
const hash=(c)=>c.repeat(40);
const names={tv:{active:'Tx.Browser.Show.S01E01.720p.WEB-DL',done:'Tx.Browser.Show.S01E02.1080p.WEB-DL'},movies:{active:'Tx.Browser.Movie.2024.720p.WEB-DL',done:'Tx.Browser.Movie.2023.1080p.BluRay'}};
const torrent=(id,hashString,name,downloadDir,labels,done)=>({id,hashString,name,downloadDir,labels,status:done?6:4,totalSize:104857600,sizeWhenDone:104857600,leftUntilDone:done?0:26214400,percentDone:done?1:0.75,metadataPercentComplete:1,isFinished:false,isStalled:false,eta:done?-1:100,error:0,errorString:'',rateDownload:done?0:262144,rateUpload:1024,uploadRatio:done?0.5:0.1,secondsSeeding:done?60:0,queuePosition:id,seedRatioLimit:2,seedRatioMode:0,seedIdleLimit:30,seedIdleMode:0});
function seedTorrents(tvLabel,movieDirectory){
 mock.torrents=[
  torrent(1,hash('a'),names.tv.active,`${DOWNLOAD_DIR}/${tvLabel}`,[tvLabel],false),
  torrent(2,hash('b'),names.tv.done,`${DOWNLOAD_DIR}/${tvLabel}`,[tvLabel],true),
  torrent(3,hash('c'),names.movies.active,movieDirectory,[],false),
  torrent(4,hash('d'),names.movies.done,movieDirectory,[],true),
  // Another application's torrent: neither its label nor its folder belongs to either domain.
  torrent(5,hash('e'),'Foreign.Torrent.2020',`/srv/other`,['other-app'],true),
 ];
}
const authClass=value=>value===undefined?'none':value===basic(mock.password)?'current':[basic(PASSWORD_ORIGINAL),basic(PASSWORD_ROTATED)].includes(value)?'stale':'other';
const server=createServer((request,response)=>{
 const url=new URL(request.url,'http://127.0.0.1');
 const send=(status,body,headers={})=>{response.writeHead(status,{'content-type':typeof body==='string'?'text/html':'application/json',...headers});response.end(typeof body==='string'?body:JSON.stringify(body));};
 const chunks=[];let size=0,overflow=false;
 request.on('data',chunk=>{size+=chunk.length;if(size>MAX_BODY_BYTES){overflow=true;return;}chunks.push(chunk);});
 request.on('end',()=>{
  let body=null;try{body=JSON.parse(Buffer.concat(chunks).toString('utf8'));}catch{}
  const sessionHeader=request.headers['x-transmission-session-id'];
  // Record only classified credential/session evidence; the raw password never enters observations.
  const row={method:request.method,path:url.pathname,rpc:typeof body?.method==='string'?body.method:null,auth:authClass(request.headers.authorization),session:sessionHeader===undefined?'none':sessionHeader===mock.session?'current':'stale',ids:Array.isArray(body?.arguments?.ids)?body.arguments.ids.length:null,fields:Array.isArray(body?.arguments?.fields)?[...body.arguments.fields]:null,content_type:request.headers['content-type']??null,status:null};
  const reply=(status,payload,headers)=>{row.status=status;if(mock.observations.length<MAX_OBSERVATIONS)mock.observations.push(row);send(status,payload,headers);};
  if(overflow||request.method!=='POST'||url.pathname!==RPC_PATH||!body||typeof body.method!=='string'){mock.unexpected.push(row);return reply(404,'<h1>404: Not Found</h1>');}
  // Transmission authenticates before the CSRF handshake.
  if(row.auth!=='current')return reply(401,'<h1>401: Unauthorized</h1>');
  if(row.session!=='current')return reply(409,'<h1>409: Conflict</h1>',{'X-Transmission-Session-Id':mock.session});
  if(!READ_ONLY_METHODS.has(row.rpc)){mock.unexpected.push(row);return reply(200,{result:'method not allowed by owned mock',arguments:{},tag:body.tag});}
  if(row.rpc==='session-get')return reply(200,{result:'success',arguments:{'rpc-version':17,'rpc-version-minimum':14,version:'4.0.6 (38c164933e)','download-dir':DOWNLOAD_DIR,'dht-enabled':true,'download-queue-enabled':mock.queueEnabled},tag:body.tag});
  const fields=Array.isArray(body.arguments?.fields)?body.arguments.fields:[];
  const selected=Array.isArray(body.arguments?.ids)?mock.torrents.filter(t=>body.arguments.ids.includes(t.hashString)):mock.torrents;
  return reply(200,{result:'success',arguments:{torrents:selected.map(t=>Object.fromEntries(fields.filter(f=>f in t).map(f=>[f,t[f]])))},tag:body.tag});
 });
});
await new Promise((ok,fail)=>{server.once('error',fail);server.listen(0,'127.0.0.1',ok);});
const txPort=server.address().port,endpoint=`http://127.0.0.1:${txPort}${URL_BASE}`;
assert.ok(![9091,8080,8989,7878,8787,8686,9696,8096,32400,8083,5076,9117].includes(txPort),'Mock must not occupy a live-service or default Transmission port');
const since=mark=>mock.observations.slice(mark);
const rpcCalls=rows=>rows.filter(row=>row.status===200).map(row=>[row.rpc,row.ids===null?'all':'ids']);
function assertReadOnly(){assert.deepEqual(mock.unexpected,[],'Mock received a mutation or unexpected request');assert.ok(mock.observations.every(row=>READ_ONLY_METHODS.has(row.rpc)),'Only session-get/torrent-get may be sent');assert.ok(mock.observations.length<MAX_OBSERVATIONS,'Mock observations truncated');}

const browser=await chromium.launch({headless:true,...(process.env.PLAYWRIGHT_EXECUTABLE?{executablePath:process.env.PLAYWRIGHT_EXECUTABLE}:{})});
const page=await browser.newPage({viewport:{width:1280,height:900}});page.setDefaultTimeout(20000);
const errors=[];page.on('pageerror',error=>errors.push(error.message));
page.on('dialog',dialog=>{errors.push(`Unexpected dialog: ${dialog.message()}`);void dialog.dismiss();});
// Secrets typed into the UI must never appear in an API response body.
const secrets=[PASSWORD_ORIGINAL,PASSWORD_ROTATED];
const leaks=[];page.on('response',async response=>{if(!new URL(response.url()).pathname.startsWith('/api/'))return;try{const body=await response.text();if(secrets.some(s=>body.includes(s)))leaks.push(new URL(response.url()).pathname);}catch{}});
const nav=page.getByRole('navigation',{name:'Workspace'}),editor=page.getByRole('article',{name:'Provider editor'}),activity=page.getByRole('region',{name:'Activity',exact:true});
async function screenshot(name){const path=join(screenshotDirectory,`iteration111-transmission-${name}-${screenshotRun}.png`);await page.screenshot({path,fullPage:true});screenshots.push(path);}
async function api(method,path,data,status=200){const r=await page.request[method](origin+path,{...(data===undefined?{}:{data}),timeout:40000});const text=await r.text();assert.equal(r.status(),status,text);assert.ok(!secrets.some(s=>text.includes(s)),`Secret echoed by ${path}`);return status===204?null:JSON.parse(text);}
async function until(read,accept,label,timeoutMs=30000){const end=Date.now()+timeoutMs;let last;while(Date.now()<end){last=await read();if(accept(last))return last;await page.waitForTimeout(100);}assert.fail(`${label}: ${JSON.stringify(last)}`);}
async function ready(locator){await locator.waitFor();await until(()=>locator.isEnabled(),Boolean,'Control remained disabled');}
const idle=region=>until(()=>region.getAttribute('aria-busy'),v=>v==='false','Panel readback incomplete');
// Wrapping <label>s include option text in a select's accessible name, so selects match by prefix.
const SELECTS=new Set(['Provider type','Credential action','TV placement','Movie placement','TV recent priority','TV older priority','Movie recent priority','Movie older priority']);
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
// Every request is basic-authenticated with exactly the expected classified credential and posts JSON.
function assertAuth(calls,expected){assert.ok(calls.length>0,'Expected RPC traffic');for(const row of calls){assert.equal(row.auth,expected,JSON.stringify(row));assert.equal(row.path,RPC_PATH,'URL base and /rpc must be preserved');assert.match(row.content_type??'',/^application\/json/);}}
async function expectEditor(values){
 assert.equal(await field('Provider type').inputValue(),'transmission');assert.equal(await field('Provider type').isDisabled(),true);
 assert.equal(await field('Host').inputValue(),'127.0.0.1');assert.equal(await field('Port').inputValue(),String(txPort));assert.equal(await field('Use SSL').isChecked(),false);assert.equal(await field('URL base').inputValue(),URL_BASE);
 await editor.getByText(`RPC URL: ${endpoint}/rpc`,{exact:true}).waitFor();
 assert.equal(await field('TV scope').isChecked(),true);assert.equal(await field('Movie scope').isChecked(),true);
 for(const [label,value] of Object.entries(values))assert.equal(typeof value==='boolean'?await field(label).isChecked():await field(label).inputValue(),value,label);
 assert.equal(await field('Credential action').inputValue(),'preserve');assert.equal(await editor.getByLabel('Password',{exact:true}).count(),0,'Password input must not be prefilled after reload');
 await editor.getByText(/Credentials: stored \(write-only\)\./).waitFor();
 const html=await page.content();assert.ok(!secrets.some(s=>html.includes(s)),'Secret rendered into the DOM');
}
const scopes=provider=>({tv:provider.settings.tv,movies:provider.settings.movies});

try{
 await page.goto(origin);
 const before=(await api('get','/api/v1/providers')).total;
 // 1. Create both scopes through the real editor: TV by category/label, movies by directory.
 await nav.getByRole('button',{name:'Providers',exact:true}).click();
 await ready(page.getByRole('button',{name:'New provider',exact:true}));await page.getByRole('button',{name:'New provider',exact:true}).click();
 await field('Provider type').selectOption('transmission');
 // The endpoint is composed from separate fields; the template starts at the conventional URL base.
 assert.equal(await field('URL base').inputValue(),URL_BASE);assert.equal(await field('Port').inputValue(),'9091');
 assert.equal(await editor.getByLabel('Endpoint',{exact:true}).count(),0,'Transmission uses host/port/SSL/URL base fields');
 assert.equal(await editor.getByLabel('API key',{exact:true}).count(),0,'Transmission has no API key credential');
 // The Transmission template starts disabled; enabling is an explicit choice.
 assert.equal(await field('Enabled').isChecked(),false);await field('Enabled').check();
 await field('Name').fill('Transmission browser client');
 await field('Host').fill('127.0.0.1');await field('Port').fill(String(txPort));
 await editor.getByText(`RPC URL: ${endpoint}/rpc`,{exact:true}).waitFor();
 await field('Movie scope').check();
 await field('TV download category').fill('tv-tx');
 await field('TV recent priority').selectOption('first');await field('TV older priority').selectOption('last');
 await field('Movie recent priority').selectOption('last');await field('Movie older priority').selectOption('first');
 await field('Movie add paused').check();
 await field('Credential action').selectOption('replace');await field('Username').fill(USER);await field('Password').fill(PASSWORD_ORIGINAL);
 // The UI rejects a shared label before sending anything.
 await field('Movie download category').fill('TV-TX');
 await ready(editor.getByRole('button',{name:'Save provider',exact:true}));await editor.getByRole('button',{name:'Save provider',exact:true}).click();
 await page.getByRole('alert').filter({hasText:'TV and movie categories must differ so each domain only sees its own torrents.'}).waitFor();
 // ...and an unusable directory, with a domain-qualified explanation.
 await field('Movie placement').selectOption('directory');await field('Movie download directory').fill('relative/movies');
 await editor.getByRole('button',{name:'Save provider',exact:true}).click();
 await page.getByRole('alert').filter({hasText:'Movie placement: Download directories must be absolute paths on the Transmission host'}).waitFor();
 assert.equal((await api('get','/api/v1/providers')).total,before,'Rejected drafts must not be saved');
 assert.equal(mock.observations.length,0,'Client-side rejections must not contact the client');
 await field('Movie download directory').fill('/srv/tx/movies');
 await screenshot('editor-draft');
 const created=await save('POST');
 // Explicit schedule suppression keeps automatic reads out of the Test traces (reapplied before Activity).
 await disableSchedules(created);
 assert.equal(created.settings.implementation,'transmission');assert.equal(created.settings.endpoint,endpoint);assert.equal(created.has_credentials,true);assert.equal(created.enabled,true);assert.equal(created.test_status,'never_tested');
 let stored=await api('get',`/api/v1/providers/${created.id}`);
 assert.deepEqual(scopes(stored),{
  tv:{placement:{strategy:'category',category:'tv-tx'},recent_priority:'first',older_priority:'last',add_paused:false,tags:[]},
  movies:{placement:{strategy:'directory',directory:'/srv/tx/movies'},recent_priority:'last',older_priority:'first',add_paused:true,tags:[]},
 });
 assert.equal(stored.has_credentials,true);assert.equal(stored.revision,created.revision);
 // Reload the page: values round-trip from the server; the password is write-only.
 await page.reload();await openProvider(created);
 const savedValues={'TV placement':'category','TV download category':'tv-tx','TV recent priority':'first','TV older priority':'last','TV add paused':false,'Movie placement':'directory','Movie download directory':'/srv/tx/movies','Movie recent priority':'last','Movie older priority':'first','Movie add paused':true};
 await expectEditor(savedValues);
 // 2a. Saved Test: one 409 session handshake, then session-get and a bounded torrent listing.
 // The daemon's session id rotates between operations; the handshake picks up the new one.
 mock.session='tx-session-2';mock.sessions++;
 let tested=await testSaved(created,200);
 assert.deepEqual(tested.body.result,{transmission_version:'4.0.6 (38c164933e)',rpc_version:17,domains:['tv','movies'],labels_supported:true,warnings:[]});assert.equal(tested.body.revision,created.revision);
 assert.deepEqual(tested.calls.map(c=>[c.rpc,c.session,c.status]),[['session-get','none',409],['session-get','current',200],['torrent-get','current',200]],'Exactly one handshake (each operation starts without a session id), then reads with the issued id');
 assert.deepEqual(tested.calls[2].fields,['id','hashString','downloadDir','labels'],'Test lists identity and placement only');
 assertAuth(tested.calls,'current');
 const result=page.getByRole('status').filter({hasText:'Connection test result'});
 await result.getByText(`Tested revision ${created.revision}: tv, movies.`,{exact:true}).waitFor();
 await result.getByText('Transmission 4.0.6 (38c164933e); RPC 17. Labels identify each domain’s category torrents.',{exact:true}).waitFor();
 stored=await api('get',`/api/v1/providers/${created.id}`);assert.equal(stored.test_status,'success');assert.equal(stored.last_test.revision,created.revision);
 await editor.getByText(/^Saved test: success at .*\(revision \d+\)\.$/).waitFor();
 await screenshot('test-success');
 // 2b. A disabled download queue makes "First" ineffective: success with a visible warning.
 mock.queueEnabled=false;
 tested=await testSaved(created,200);assert.deepEqual(tested.body.result.warnings,['queue_disabled']);
 await page.getByRole('alert').filter({hasText:'the Transmission download queue is disabled'}).waitFor();
 mock.queueEnabled=true;
 // 2c. The RPC password rotates: the saved Test sends the stale credential and surfaces an authentication alert.
 mock.password=PASSWORD_ROTATED;
 tested=await testSaved(created,502);assert.equal(tested.body.error.code,'authentication');assertAuth(tested.calls,'stale');
 assert.deepEqual(tested.calls.map(c=>c.status),[401],'Authentication failure ends the probe');
 await page.getByRole('alert').filter({hasText:'Provider rejected authentication'}).waitFor();
 stored=await api('get',`/api/v1/providers/${created.id}`);assert.equal(stored.test_status,'failure');assert.equal(stored.last_test.error_code,'authentication');
 await editor.getByText(/^Saved test: failure at .*: authentication\.$/).waitFor();
 await screenshot('test-authentication-failure');
 // Replace only the credential bundle; endpoint and scopes must not change.
 const beforeReplace=scopes(stored);
 await field('Credential action').selectOption('replace');await field('Username').fill(USER);await field('Password').fill(PASSWORD_ROTATED);
 const replaced=await save('PUT');assert.equal(replaced.revision,created.revision+1);assert.equal(replaced.has_credentials,true);assert.deepEqual(scopes(replaced),beforeReplace);assert.equal(replaced.settings.endpoint,endpoint);
 assert.equal(await editor.getByLabel('Password',{exact:true}).count(),0,'Secret field must clear after save');
 tested=await testSaved(replaced,200);assertAuth(tested.calls,'current');assert.deepEqual(tested.body.result.domains,['tv','movies']);
 // 3. Edit only the movie scope; TV scope, endpoint and stored credentials are preserved.
 const beforeEdit=await api('get',`/api/v1/providers/${created.id}`);
 await field('Movie download directory').fill('/srv/tx/films');await field('Movie add paused').uncheck();
 const edited=await save('PUT');assert.equal(edited.revision,replaced.revision+1);
 stored=await api('get',`/api/v1/providers/${created.id}`);
 assert.deepEqual(stored.settings.tv,beforeEdit.settings.tv,'TV scope must be preserved');
 assert.deepEqual(stored.settings.movies,{...beforeEdit.settings.movies,placement:{strategy:'directory',directory:'/srv/tx/films'},add_paused:false},'Only the movie placement and add-paused may change');
 assert.equal(stored.has_credentials,true);assert.equal(stored.settings.endpoint,endpoint);
 await page.reload();await openProvider(created);await expectEditor({...savedValues,'Movie download directory':'/srv/tx/films','Movie add paused':false});
 // Draft Test (API only: ProviderPanel has no draft-test control) writes no configuration or observation.
 // A movie directory inside TV's effective category folder is a Test-time overlap problem.
 const config=extra=>({name:stored.name,enabled:stored.enabled,priority:stored.priority,settings:{...stored.settings,...extra}});
 let mark=mock.observations.length;
 await until(async()=>{const r=await page.request.post(`${origin}/api/v1/providers/test-draft`,{data:{source:{id:stored.id,revision:stored.revision},config:config({movies:{...stored.settings.movies,placement:{strategy:'directory',directory:`${DOWNLOAD_DIR}/tv-tx/films`}}})},timeout:40000});return [r.status(),await r.json()];},([status,body])=>status===422&&body.error.code==='transmission_domains_overlap','Source-bound draft did not report the overlap');
 assertAuth(since(mark),'current');
 assert.deepEqual(await api('get',`/api/v1/providers/${created.id}`),stored,'Draft must not change the saved provider or observation');
 mark=mock.observations.length;
 const draft=await api('post','/api/v1/providers/test-draft',{source:null,config:{...config({}),name:'Unsaved Transmission draft',credentials:{kind:'username_password',username:USER,password:PASSWORD_ROTATED}}});
 assert.equal(draft.source,null);assert.deepEqual(draft.result.domains,['tv','movies']);assertAuth(since(mark),'current');
 assert.equal((await api('get','/api/v1/providers')).total,before+1,'Draft must not create a provider');
 // 4. Activity refresh for each domain lists placements, reads full state for that domain only and renders the snapshot.
 await disableSchedules(stored);
 const ambient=mock.observations.filter(o=>o.rpc==='torrent-get'&&o.ids!==null).length;
 seedTorrents('tv-tx','/srv/tx/films');
 await nav.getByRole('button',{name:'Activity',exact:true}).click();await idle(activity);
 const clientSelect=activity.getByLabel(/^Download client/);await ready(clientSelect);
 assert.equal(await clientSelect.getByRole('option',{name:'Transmission browser client',exact:true}).count(),1,'Transmission client must be selectable in Activity');
 await clientSelect.selectOption(created.id);await idle(activity);
 for(const [domain,category,own,other] of [['tv','tv-tx',[hash('a'),hash('b')],[hash('c'),hash('d'),hash('e')]],['movies','',[hash('c'),hash('d')],[hash('a'),hash('b'),hash('e')]]]){
  await activity.getByLabel(/^Refresh media/).selectOption(domain);await idle(activity);
  const refresh=activity.getByRole('button',{name:'Refresh downloads',exact:true});await ready(refresh);
  mark=mock.observations.length;
  const admitted=page.waitForResponse(r=>new URL(r.url()).pathname==='/api/v1/commands'&&r.request().method()==='POST');
  await refresh.click();const reply=await admitted;assert.equal(reply.status(),202,await reply.text());const command=await reply.json();
  assert.equal(command.name,'refresh_downloads');assert.deepEqual(command.target,{provider_id:created.id,media_type:domain});assert.equal(command.provider_revision,stored.revision);
  const finished=await until(()=>api('get',`/api/v1/commands/${command.id}`),c=>['succeeded','failed','cancelled'].includes(c.status),`Refresh ${domain} did not finish`);
  assert.equal(finished.status,'succeeded',JSON.stringify(finished));assert.equal(finished.items_observed,2);
  const calls=since(mark).filter(c=>c.status===200);assertAuth(calls,'current');
  assert.deepEqual(rpcCalls(calls),[['session-get','all'],['torrent-get','all'],['torrent-get','ids']],`Refresh ${domain} must list then read its own torrents`);
  assert.equal(calls[2].ids,own.length,'Full fields are requested only for this domain');
  assert.ok(calls[1].fields.length<=4&&calls[2].fields.length===25,'Bounded field sets');
  const snapshot=await until(()=>api('get',`/api/v1/queue?provider_id=${created.id}&media_type=${domain}&limit=25&offset=0`),s=>s.command_id===command.id,`Snapshot ${domain} not from this command`);
  assert.equal(snapshot.total,2);
  assert.deepEqual(snapshot.items.map(i=>[i.download.hash,i.download.domain,i.download.category,i.download.name,i.download.status,i.association]).sort(),[[own[0],domain,category,names[domain].active,'downloading',null],[own[1],domain,category,names[domain].done,'completed',null]].sort());
  assert.ok(snapshot.items.every(i=>!other.includes(i.download.hash)),'Snapshot must stay within its domain placement');
  await activity.getByRole('button',{name:'Reload activity',exact:true}).click();await idle(activity);
  const table=activity.getByRole('table');
  for(const name of [names[domain].active,names[domain].done]){const row=table.getByRole('row').filter({hasText:name});await row.waitFor();await row.getByText(new RegExp(`^${domain} / ${category}\\s*$`)).waitFor();}
  assert.equal(await table.getByRole('row').filter({hasText:'Foreign.Torrent.2020'}).count(),0,'Foreign torrent must not render');
  await activity.getByText(new RegExp(`Command ${command.id}`)).waitFor();
  await screenshot(`activity-${domain}`);
 }
 assert.equal(await activity.locator('p.error').count(),0,'Activity must render without errors');
 assertReadOnly();assert.deepEqual(leaks,[],'Secret leaked into an API response');assert.deepEqual(errors,[]);
 console.log(JSON.stringify({screenshots,mock_requests:mock.observations.length,session_rotations:mock.sessions,automatic_full_reads_before_activity:ambient,provider_busy_retries:busyRetries,methods:[...new Set(mock.observations.map(o=>o.rpc))].sort()}));
 console.log('PASS Transmission ProviderPanel: UI create from host/port/SSL/URL base with TV category-label and movie directory placements, priorities and add-paused (shared label and relative directory rejected client-side without contacting the client), server readback and page-reload round trip with write-only password; saved Test with one 409 session handshake, queue-disabled warning, rotated-password authentication alert, credential-only replace, movie-only edit preserving TV scope and credentials; API-only draft tests (overlap problem, unsaved draft) without persistence; Activity refresh for both domains listed placements then read only that domain on the owned mock and rendered real snapshots excluding a foreign torrent. Only session-get/torrent-get reached the mock.');
}catch(error){console.error('PAGESTATE',(await page.locator('body').innerText().catch(()=>'')).slice(0,8000));console.error('MOCK',JSON.stringify(mock.observations.slice(-40)));throw error;}
finally{try{await browser.close();}finally{const closed=new Promise(done=>server.close(()=>done()));server.closeAllConnections();await closed;}}
