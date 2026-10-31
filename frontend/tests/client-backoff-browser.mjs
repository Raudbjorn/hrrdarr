// Real owned client failures/recovery. Clock aging and pending automatic commands are explicit fixture seeds; no UI overlays.
import assert from 'node:assert/strict';
import {pathToFileURL} from 'node:url';
const {chromium}=await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE||'/usr/lib/node_modules/playwright/index.mjs').href);
// Reasoning: 0080 registers root_folder and mount_read_only per domain plus movies collection_root_folder, so scoped registries differ (tv 14, movies 15).
const scopedChecks=domain=>domain==='tv'?14:15;
const origin=process.env.UI_URL,remote=process.env.UI_PROVIDER_ORIGIN;
for(const value of [origin,remote])assert.equal(new URL(value).hostname,'127.0.0.1');
const browser=await chromium.launch({headless:true,...(process.env.PLAYWRIGHT_EXECUTABLE?{executablePath:process.env.PLAYWRIGHT_EXECUTABLE}:{})});
const page=await browser.newPage();page.setDefaultTimeout(20000);
const errors=[];page.on('pageerror',error=>errors.push(error.message));page.on('dialog',dialog=>{errors.push(`Unexpected dialog: ${dialog.message()}`);void dialog.dismiss();});
const nav=page.getByRole('navigation',{name:'Workspace'}),health=page.getByRole('region',{name:'Health',exact:true}),activity=page.getByRole('region',{name:'Activity',exact:true}),editor=page.getByRole('article',{name:'Provider editor'});
async function api(method,path,data,status=200){const r=await page.request[method](origin+path,{...(data===undefined?{}:{data}),timeout:10000});assert.equal(r.status(),status,await r.text());return status===204?null:r.json();}
const get=path=>api('get',path);
async function until(read,accept,label){const end=Date.now()+25000;while(Date.now()<end){const value=await read();if(accept(value))return value;await page.waitForTimeout(100);}assert.fail(label);}
const idle=region=>until(()=>region.getAttribute('aria-busy'),v=>v==='false','Panel readback incomplete');
const mode=async value=>assert.equal((await page.request.post(`${remote}/fixture-mode?mode=${value}`,{timeout:10000})).status(),204);
async function testSaved(provider,success){
 await nav.getByRole('button',{name:'Providers',exact:true}).click();const list=page.getByRole('complementary',{name:'Saved providers'});
 await list.getByRole('button',{name:'Refresh providers',exact:true}).click();await list.getByRole('button',{name:new RegExp(`^${provider.name} `)}).click();
 await editor.getByRole('heading',{name:provider.name,exact:true}).waitFor();
 const response=page.waitForResponse(r=>new URL(r.url()).pathname===`/api/v1/providers/${provider.id}/test`&&r.request().method()==='POST');
 await editor.getByRole('button',{name:'Test saved provider',exact:true}).click();const reply=await response;assert.equal(reply.status(),success?200:502,await reply.text());
 await until(()=>editor.getByRole('button',{name:'Test saved provider',exact:true}).isEnabled(),Boolean,'Test readback incomplete');
}
async function healthFor(domain){
 await nav.getByRole('button',{name:'Health',exact:true}).click();await idle(health);await health.getByLabel('Health scope').selectOption(domain);await idle(health);
 const response=page.waitForResponse(r=>new URL(r.url()).pathname==='/api/v1/health/commands'&&r.request().method()==='POST');
 await health.getByRole('button',{name:`Run ${domain} health checks`,exact:true}).click();const reply=await response;assert.equal(reply.status(),202);const admission=await reply.json();// Reasoning: migration 0053 added remote_path_mapping, so each domain registers 11 checks.
assert.equal(admission.selection.length,scopedChecks(domain)); // Reasoning: 0080 adds root_folder and mount_read_only per domain and movies-only collection_root_folder (tv 12 -> 14, movies 12 -> 15).
 // Reasoning: 0059 adds recycle_bin per domain (11 -> 12).
 const snapshot=await until(()=>get(`/api/v1/health?scope=${domain}`),s=>s.summary.current===scopedChecks(domain)&&!s.active_command&&admission.selection.every(token=>s.checks.some(c=>c.identity.check_key===token.identity.check_key&&c.observed_generation>=token.generation)),'Scoped health did not settle');
 await health.getByRole('button',{name:'Refresh health',exact:true}).click();await idle(health);return snapshot;
}
async function schedules(provider,enabled){
 for(const media_type of ['tv','movies']){const prior=(await get('/api/v1/download-refresh/schedules')).find(s=>s.target.provider_id===provider.id&&s.target.media_type===media_type);await api('put','/api/v1/download-refresh/schedules',{target:{provider_id:provider.id,media_type},provider_revision:provider.revision,revision:prior?.revision??null,enabled,interval_seconds:60});}
}
const command=id=>get(`/api/v1/commands/${id}`);
const finished=id=>until(()=>command(id),c=>['succeeded','failed','cancelled'].includes(c.status),'Refresh command did not finish');
const queue=(id,domain)=>get(`/api/v1/queue?provider_id=${id}&media_type=${domain}`);
async function observations(){const r=await page.request.get(`${remote}/fixture-observations`,{timeout:10000});assert.equal(r.status(),200);return r.json();}
try{
 await mode(0);const scope=category=>({category,imported_category:null,recent_priority:0,older_priority:0});
 const provider=await api('post','/api/v1/providers',{name:'Backoff browser client',enabled:true,priority:1,settings:{implementation:'qbittorrent',endpoint:remote,tv:scope('tv'),movies:scope('movies')},credentials:{kind:'username_password',username:'fixture-user',password:'fixture-good'}},201);
 await schedules(provider,false);
 for(const domain of ['tv','movies']){const c=await api('post','/api/v1/commands',{name:'refresh_downloads',target:{provider_id:provider.id,media_type:domain},provider_revision:provider.revision,priority:'normal'},202);assert.equal((await finished(c.id)).status,'succeeded');}
 const baseline={tv:await queue(provider.id,'tv'),movies:await queue(provider.id,'movies')};
 await page.goto(origin);await mode(4);await testSaved(provider,false);
 const state=()=>get(`/api/fixture/client-backoff/${provider.id}`);
 const first=await state();assert.equal(first.length,2);assert.ok(first.every(s=>s.escalation_level===1&&s.disabled_until===null));
 await api('post','/api/fixture/client-backoff/age',{provider_id:provider.id},204);
 console.log('FIXTURE seed: aged only existing real first-failure timestamps by301seconds; no wall-clock five-minute wait.');
 await testSaved(provider,false);const blocked=await state();assert.ok(blocked.every(s=>s.disabled_until>s.last_failure_at));await mode(0);
 for(const domain of ['tv','movies']){
  const snapshot=await healthFor(domain),issue=snapshot.issues.find(i=>i.identity.check_key==='download_client_backoff');assert.ok(issue);assert.equal(issue.severity,'error');assert.equal(issue.reason,'download_clients_backoff');assert.equal(issue.compatibility_type,'DownloadClientStatusCheck');
  const row=health.getByRole('article',{name:`${domain}:download_client_backoff`,exact:true});await row.getByText(`error: ${issue.message} (${issue.reason})`,{exact:true}).waitFor();assert.ok(!JSON.stringify(snapshot).includes('PRIVATE_FIXTURE_CLIENT_FAILURE'));
 }
 await schedules(provider,true);
 const pending=await api('post','/api/fixture/client-backoff/pending',{provider_id:provider.id});console.log('FIXTURE seed: two pending automatic commands under an already-real block; real schedules are enabled and due.');
 const before={tv:await command(pending.tv),movies:await command(pending.movies),schedules:await get('/api/v1/download-refresh/schedules'),observations:await observations(),state:await state()};
 assert.ok(before.observations.length<256,'Fixture observation capacity must remain unsaturated');
 for(const domain of ['tv','movies']){assert.equal(before[domain].origin,'automatic');assert.equal(before[domain].manual_bypass,false);assert.equal(before[domain].attempts,0);assert.equal(before[domain].suppression_reason,'download_client_backoff');}
 await nav.getByRole('button',{name:'Activity',exact:true}).click();await idle(activity);
 await activity.getByLabel(/^Download client/).selectOption(provider.id);await idle(activity);
 for(const domain of ['tv','movies']){
  await activity.getByLabel(/^Refresh media/).selectOption(domain);await idle(activity);
  await activity.getByRole('button',{name:`Inspect command ${pending[domain]}`,exact:true}).click();await idle(activity);
  await activity.getByRole('region',{name:'Command detail'}).getByText(/Automatic polling delayed until .*manual refresh or provider Test/).waitFor();
 }
 // Observe more than two one-second worker ticks; deterministic scheduler/claim barriers are covered by Rust tests.
 await page.waitForTimeout(2200);
 for(const domain of ['tv','movies']){assert.deepEqual(await command(pending[domain]),before[domain]);assert.deepEqual(await queue(provider.id,domain),baseline[domain]);}
 assert.deepEqual(await get('/api/v1/download-refresh/schedules'),before.schedules);assert.deepEqual(await observations(),before.observations);assert.deepEqual(await state(),before.state);
 // Stop future scheduling to isolate each pending command's recovery; existing queued commands remain intact.
 await schedules(provider,false);
 assert.ok((await state()).every(s=>s.disabled_until>Date.now()/1000+5),'Recovery must precede expiry');
 await activity.getByLabel(/^Refresh media/).selectOption('tv');await idle(activity);
 const coalesced=page.waitForResponse(r=>new URL(r.url()).pathname==='/api/v1/commands'&&r.request().method()==='POST');
 await activity.getByRole('button',{name:'Refresh downloads',exact:true}).click();const reply=await coalesced;assert.equal(reply.status(),202);const admitted=await reply.json();assert.equal(admitted.id,pending.tv);assert.equal(admitted.origin,'automatic');assert.equal(admitted.manual_bypass,true);
 const recovered=await finished(pending.tv);assert.equal(recovered.status,'succeeded');assert.equal(recovered.attempts,1);assert.equal((await queue(provider.id,'tv')).command_id,pending.tv);
 const afterManual=await state();assert.equal(afterManual.find(s=>s.media_type==='tv').disabled_until,null);assert.deepEqual(afterManual.find(s=>s.media_type==='movies'),before.state.find(s=>s.media_type==='movies'));assert.deepEqual(await command(pending.movies),before.movies);assert.deepEqual(await queue(provider.id,'movies'),baseline.movies);
 assert.equal((await healthFor('tv')).issues.some(i=>i.identity.check_key==='download_client_backoff'),false);assert.equal((await healthFor('movies')).issues.some(i=>i.identity.check_key==='download_client_backoff'),true);
 await testSaved(provider,true);assert.ok((await state()).every(s=>s.disabled_until===null));assert.equal((await finished(pending.movies)).status,'succeeded');
 assert.equal((await healthFor('movies')).issues.some(i=>i.identity.check_key==='download_client_backoff'),false);
 const transitions=health.getByRole('region',{name:'Health diagnostic transitions'}),restored=transitions.getByText(/restored — movies:download_client_backoff/);
 for(let pageNumber=0;pageNumber<8&&await restored.count()===0;pageNumber++){
  const next=transitions.getByRole('button',{name:'Newer events',exact:true});assert.equal(await next.isEnabled(),true,'Restoration must exist in retained real diagnostic events');await next.click();await idle(health);
 }
 await restored.first().waitFor();
 assert.deepEqual(errors,[]);console.log('PASS actual failures block both scoped automatic polls; Activity/Health show delay; blocked ticks preserve network/attempts/snapshots/schedules/status; real manual coalescing recoversTV only; saved Test recoversMovies beforeexpiry. Clock aging and pending-command admission were explicit fixture seeds.');
}catch(error){console.error('PAGESTATE',(await page.locator('body').innerText()).slice(0,10000));throw error;}
finally{try{await mode(0);}finally{await browser.close();}}
