// Owned browser regression: real source-parent loss/recovery and Health in both domains.
// Explicit read-only reconciliation/GET overlays exercise lease projections and malformed completion rejection.
// Run against a fresh owned library_ui_fixture, separate from library-browser.mjs.
import assert from 'node:assert/strict';
import {copyFile,mkdir,readFile,realpath,rename,stat} from 'node:fs/promises';
import {basename,isAbsolute,join,resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
const {chromium}=await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE||'/usr/lib/node_modules/playwright/index.mjs').href);
const origin=process.env.UI_URL,providerOrigin=process.env.UI_PROVIDER_ORIGIN;
for(const value of [origin,providerOrigin])assert.equal(new URL(value).hostname,'127.0.0.1');
assert.ok(process.env.UI_SCRATCH&&isAbsolute(process.env.UI_SCRATCH));
const scratch=await realpath(process.env.UI_SCRATCH);
assert.ok(basename(scratch).startsWith('hrrdarr-library-ui-'),'Only the owned library fixture may be modified');
// Evidence must stay in an explicitly selected ignored 103 directory, independent of cwd.
assert.ok(process.env.UI_SCREENSHOT_DIR&&isAbsolute(process.env.UI_SCREENSHOT_DIR),'Set an absolute UI_SCREENSHOT_DIR');
const screenshotRequested=resolve(process.env.UI_SCREENSHOT_DIR);
assert.match(screenshotRequested,/\/\.do-not-commit\/loop\/iteration103(?:[-/]|$)/,'Screenshots must stay under ignored iteration103 artifacts');
await mkdir(screenshotRequested,{recursive:true});
const screenshotDirectory=await realpath(screenshotRequested);
assert.match(screenshotDirectory,/\/\.do-not-commit\/loop\/iteration103(?:[-/]|$)/,'Screenshot directory must resolve to ignored iteration103 artifacts');
const screenshotRun=`${Date.now()}-${process.pid}`,screenshots=[];
async function screenshot(name){const path=join(screenshotDirectory,`iteration103-${name}-${screenshotRun}.png`);await page.screenshot({path});screenshots.push(path);}
const browser=await chromium.launch({headless:true,...(process.env.PLAYWRIGHT_EXECUTABLE?{executablePath:process.env.PLAYWRIGHT_EXECUTABLE}:{})});
const page=await browser.newPage({viewport:{width:1280,height:900}});
page.setDefaultTimeout(20000);
const errors=[],external=[],writes=[],reconciliations=[],overlays=[];
const reconcilePath='/api/v1/manual-import/commands/reconcile';
const retainedParents=new Map();
page.on('pageerror',error=>errors.push(error.message));
let expectedLegacyBeforeUnload=false;
page.on('dialog',dialog=>{
 if(expectedLegacyBeforeUnload&&dialog.type()==='beforeunload'){expectedLegacyBeforeUnload=false;void dialog.accept();return;}
 errors.push(`Unexpected ${dialog.type()} dialog: ${dialog.message()}`);void dialog.dismiss();
});
page.on('request',request=>{
 const path=new URL(request.url()).pathname;
 // This POST is a read-only snapshot, never an execution/admission authorization.
 if(path===reconcilePath && request.method()==='POST'){reconciliations.push(request.postDataJSON());return;}
 if(!['GET','HEAD'].includes(request.method()))writes.push([request.method(),path]);
});
await page.route('**/*',route=>{
 const url=new URL(route.request().url());
 if(['http:','https:'].includes(url.protocol)&&url.hostname!=='127.0.0.1'){external.push(url.href);return route.abort();}
 return route.continue();
});
const nav=page.getByRole('navigation',{name:'Workspace'});
const manual=page.getByRole('region',{name:'Manual import',exact:true});
const health=page.getByRole('region',{name:'Health',exact:true});
const activity=page.getByRole('region',{name:'Activity',exact:true});
const processing=page.getByRole('region',{name:'Completed downloads',exact:true});
const manualRow=id=>manual.getByRole('article').filter({has:page.getByText(id,{exact:true})});
async function receipt(id){
 await page.waitForFunction(id=>{const saved=JSON.parse(localStorage.getItem('hrrdarr.import.collection.v2')||'null');return saved?.version===2&&saved.entries.some(e=>!id||e.operationId===id);},id);
 const saved=await page.evaluate(()=>JSON.parse(localStorage.getItem('hrrdarr.import.collection.v2')));
 assert.equal(saved.version,2);if(!id)assert.equal(saved.entries.length,1);
 const entry=id?saved.entries.find(e=>e.operationId===id):saved.entries[0];assert.ok(entry);
 assert.equal(Object.hasOwn(entry,'operation'),false,'Stored entries must never substitute for real operation observations');return entry;
}
async function reconciliationResponse(route,id,projection){
 assert.equal(route.request().method(),'POST');assert.deepEqual(route.request().postDataJSON(),{operation_ids:[id]});
 const response=await route.fetch();assert.equal(response.status(),200);const rows=await response.json();
 assert.equal(rows.length,1);assert.equal(rows[0].operation_id,id);assert.equal(rows[0].operation.id,id);assert.equal(rows[0].active_command,null);
 return route.fulfill({response,body:JSON.stringify([{...rows[0],operation:projection}])});
}
async function get(path){const response=await page.request.get(origin+path,{timeout:10000});assert.equal(response.status(),200,await response.text());return response.json();}
async function until(read,accept,label){const deadline=Date.now()+30000;let last;while(Date.now()<deadline){last=await read();if(accept(last))return last;await page.waitForTimeout(75);}assert.fail(`${label}: ${JSON.stringify(last)}`);}
async function ready(button){await button.waitFor();await until(()=>button.isEnabled(),Boolean,'Control did not become enabled');}
async function manualRead(){const button=manual.getByRole('button',{name:'Check status',exact:true});await ready(button);await button.click();await until(()=>button.isEnabled(),Boolean,'Manual import read did not settle');}
async function add(domain){
 const title=domain==='tv'?'Fixture series':'Fixture movie';
 await page.getByRole('navigation',{name:'Library type'}).getByRole('button',{name:domain==='tv'?'TV':'Movies',exact:true}).click();
 await page.getByRole('button',{name:domain==='tv'?'Add series':'Add movie',exact:true}).click();
 await page.getByLabel('Search catalogue').fill('Fixture');await page.getByRole('button',{name:'Search',exact:true}).click();
 await page.getByRole('button',{name:new RegExp(title+' .*')}).click();
 await page.getByLabel('Existing library directory').fill(join(scratch,domain));
 await page.getByRole('button',{name:'Add to library',exact:true}).click();
 await page.getByRole('article').getByRole('heading',{name:title,exact:true}).waitFor();
 const items=await get(domain==='tv'?'/api/v1/tv/series':'/api/v1/movies');
 const item=items.items.find(value=>(domain==='tv'?value.tvdb_id:value.tmdb_id)===101);assert.ok(item);return item;
}
async function enableCdh(domain){
 const path=`/api/v1/${domain}/completed-download-handling`,settings=await get(path);
 const response=await page.request.put(origin+path,{data:{revision:settings.revision,enabled:true},timeout:10000});assert.equal(response.status(),200,await response.text());
}
async function healthRun(domain,expectedReason){
 await nav.getByRole('button',{name:'Health',exact:true}).click();
 await until(()=>health.getAttribute('aria-busy'),value=>value==='false','Health panel did not settle');
 await health.getByLabel('Health scope').selectOption(domain);
 await until(()=>health.getAttribute('aria-busy'),value=>value==='false','Scoped health read did not settle');
 const button=health.getByRole('button',{name:`Run ${domain} health checks`,exact:true});await ready(button);
 const reply=page.waitForResponse(response=>new URL(response.url()).pathname==='/api/v1/health/commands'&&response.request().method()==='POST');
 await button.focus();await page.keyboard.press('Enter');
 const response=await reply;assert.equal(response.status(),202);const admission=await response.json();
 const token=admission.selection.find(value=>value.identity.scope===domain&&value.identity.check_key==='remote_path_mapping');assert.ok(token,'Admission must include the real path check');
 const snapshot=await until(()=>get(`/api/v1/health?scope=${domain}`),value=>{
  const check=value.checks.find(check=>check.identity.check_key==='remote_path_mapping');
  const issue=value.issues.find(issue=>issue.identity.check_key==='remote_path_mapping');
  return check?.evaluation==='current'&&check.observed_generation>=token.generation&&(expectedReason===null?!issue:issue?.reason===expectedReason);
 },'Requested path observation did not become current');
 const refresh=health.getByRole('button',{name:'Refresh health',exact:true});await ready(refresh);await refresh.click();
 await until(()=>health.getAttribute('aria-busy'),value=>value==='false','Health readback did not finish');
 const row=health.getByRole('article',{name:`${domain}:remote_path_mapping`,exact:true});
 const issue=snapshot.issues.find(issue=>issue.identity.check_key==='remote_path_mapping');
 if(issue){assert.equal(issue.severity,'error');await row.getByText(`error: ${issue.message} (${issue.reason})`,{exact:true}).waitFor();assert.ok(!issue.message.includes(scratch));}
 else await row.getByText('No cached issue on this check; evaluation state determines freshness.',{exact:true}).waitFor();
 return snapshot;
}
async function manualProjectionOverlays(operation){
 const match=url=>url.pathname===reconcilePath,row=manualRow(operation.id);
 let projection={...operation,status:'staging',error_code:null,path_failure_code:'unresolved_attempt',recovery_required:false};
 const count=writes.length,reads=reconciliations.length;
 await page.route(match,route=>reconciliationResponse(route,operation.id,projection));
 await manualRead();
 await row.getByText('This import attempt has unresolved evidence. Check status before another action; this does not establish a specific filesystem failure.',{exact:true}).waitFor();
 assert.equal(await row.getByRole('button',{name:'Resume import',exact:true}).count(),0,'Active projection must not offer a future resume');
 assert.equal(await row.getByRole('button',{name:'Execute import',exact:true}).count(),0);
 projection={...projection,recovery_required:true};await manualRead();
 await row.getByText('The previous import attempt has no confirmed outcome. Check retained files and explicitly resume the same operation when ready.',{exact:true}).waitFor();
 await ready(row.getByRole('button',{name:'Resume import',exact:true}));
 assert.ok(reconciliations.length>reads,'Projection was observed through read-only reconciliation');
 assert.equal(writes.length,count,'Rendering recovery projections must not send any mutation or authorization');
 await page.unroute(match);await manualRead();
 assert.equal((await get(`/api/v1/imports/${operation.id}`)).path_failure_code,operation.path_failure_code,'Actual persisted cause must return after overlay removal');
 assert.equal((await receipt(operation.id)).operationId,operation.id);
 overlays.push('Manual read-only reconciliation POST: active and abandoned sentinel with NULL control error; no authorization sent');
}
async function oldReceiptRefresh(operation){
 const match=url=>url.pathname===reconcilePath,row=manualRow(operation.id),entry=await receipt(operation.id);
 let entered;const held=new Promise(resolve=>entered=resolve);let release;const gate=new Promise(resolve=>release=resolve);
 // Explicitly exercise legacy migration: observations are present only in this v1 specimen.
 // Removing v2 is required; otherwise the panel correctly prefers its existing collection.
 await page.evaluate(({entry,operation})=>{
  const oldOperation={...operation};delete oldOperation.path_failure_code;delete oldOperation.recovery_required;
  localStorage.removeItem('hrrdarr.import.collection.v2');
  localStorage.setItem('hrrdarr.import.receipt.v1',JSON.stringify({request:entry.request,label:entry.label,operation:oldOperation,authorized:entry.authorized}));
 },{entry,operation});
 const count=writes.length;
 await page.route(match,async route=>{
  assert.equal(route.request().method(),'POST');assert.deepEqual(route.request().postDataJSON(),{operation_ids:[operation.id]});
  const response=await route.fetch();assert.equal(response.status(),200);entered();await gate;await route.fulfill({response});
 });
 let timeout;
 try{
  // The retained authorized entry deliberately warns before this explicit migration reload.
  expectedLegacyBeforeUnload=true;
  try{await page.reload();}finally{expectedLegacyBeforeUnload=false;}
  await Promise.race([held,new Promise((_,reject)=>{timeout=setTimeout(()=>reject(new Error('Saved receipt reconciliation did not enter gate')),10000);})]);
  await row.getByText('Check status before authorizing this operation. Saved receipts never restore action freshness.',{exact:true}).waitFor();
  assert.equal(await row.getByRole('button',{name:'Resume import',exact:true}).count(),0);
  assert.equal(await row.getByRole('button',{name:'Execute import',exact:true}).count(),0);
 }finally{clearTimeout(timeout);release();}
 await ready(row.getByRole('button',{name:'Resume import',exact:true}));await page.unroute(match);
 const migrated=await receipt(operation.id);assert.deepEqual(migrated.request,entry.request);assert.equal(migrated.authorized,true);assert.equal(migrated.executionUncertain,true);assert.equal(migrated.owner,null);
 assert.equal(await page.evaluate(()=>localStorage.getItem('hrrdarr.import.receipt.v1')),null,'Migration removes v1 only after retaining v2');
 assert.equal((await get(`/api/v1/imports/${operation.id}`)).recovery_required,true);
 assert.equal(writes.length,count,'Migrating and reconciling an old receipt must not authorize effects');
 overlays.push('Delayed actual read-only reconciliation: explicitly seeded legacy receipt requires fresh recovery fields; no execution/admission');
}
async function completedOverlay(operation){
 const match=url=>url.pathname===reconcilePath,row=manualRow(operation.id);
 const count=writes.length;
 await page.route(match,route=>reconciliationResponse(route,operation.id,{...operation,status:'complete',error_code:null,path_failure_code:'source_denied_precommit',recovery_required:true}));
 await manualRead();
 // 103 validates the full operation contract: complete with a retained failure/recovery flag
 // is malformed. Preserve the identity, invalidate actions and reject the observation.
 await manual.getByRole('alert').filter({hasText:'Reconciliation operation target or status is invalid.'}).waitFor();
 await row.getByText('Check status before authorizing this operation. Saved receipts never restore action freshness.',{exact:true}).waitFor();
 assert.equal(await row.getByRole('button',{name:'Prepare another import',exact:true}).count(),0,'An invalid projection cannot authorize forgetting recovery identity');
 assert.equal(await row.getByRole('button',{name:'Resume import',exact:true}).count(),0);
 assert.equal(await row.getByRole('button',{name:'Execute import',exact:true}).count(),0);
 assert.equal(await row.getByText(/denied access|unresolved evidence|previous import attempt/).count(),0);
 assert.equal(writes.length,count,'Malformed completion must not authorize any mutation');
 assert.equal((await receipt(operation.id)).operationId,operation.id);
 const actual=await get(`/api/v1/imports/${operation.id}`);assert.equal(actual.status,'complete');assert.equal(actual.path_failure_code,null);assert.equal(actual.recovery_required,false);
 await page.unroute(match);await manualRead();await ready(row.getByRole('button',{name:'Prepare another import',exact:true}));
 overlays.push('Manual read-only reconciliation: contradictory completed recovery projection rejected; identity retained and every action fenced, no effects');
}
async function processingProjectionOverlays(owners,operations){
 const scope=category=>({category,imported_category:null,recent_priority:0,older_priority:0});
 const response=await page.request.post(origin+'/api/v1/providers',{data:{name:'Path projection fixture',enabled:false,priority:1,settings:{implementation:'qbittorrent',endpoint:providerOrigin,tv:scope('tv'),movies:scope('movies')}},timeout:10000});
 assert.equal(response.status(),201,await response.text());const provider=await response.json();
 const rows=new Map();
 for(const domain of ['tv','movies'])rows.set(domain,{receipt_id:domain==='tv'?'11111111-1111-4111-8111-111111111111':'22222222-2222-4222-8222-222222222222',target:domain==='tv'?{media_type:'tv',series_id:owners.tv.id,episode_ids:[operations.tv.target.id]}:{media_type:'movies',movie_id:owners.movies.id},provider_id:provider.id,provider_revision:provider.revision,remote_id:'a'.repeat(40),policy_revision:1,status:'importing',preflight_attempts:1,total_preflight_attempts:1,next_attempt_at:1,operation_id:operations[domain].id,import_phase:'staging',resume_requested:false,retirement_state:null,recovery_bytes_retained:false,error_code:null,reasons:['hc021_unresolved_attempt'],created_at:1,updated_at:1,path_failure_code:'unresolved_attempt',recovery_required:false});
 const match=url=>url.pathname==='/api/v1/download-processing'&&url.searchParams.get('provider_id')===provider.id;
 await page.route(match,route=>{assert.equal(route.request().method(),'GET');const domain=new URL(route.request().url()).searchParams.get('media_type');return route.fulfill({status:200,contentType:'application/json',body:JSON.stringify({items:[rows.get(domain)],total:1,limit:25,offset:0})});});
 await nav.getByRole('button',{name:'Activity',exact:true}).click();
 await ready(activity.getByLabel('Download client'));await activity.getByLabel('Download client').selectOption(provider.id);
 const count=writes.length;
 async function read(){const button=processing.getByRole('button',{name:'Check processing status',exact:true});await ready(button);await button.click();await until(()=>button.isEnabled(),Boolean,'Processing projection readback did not settle');}
 for(const domain of ['tv','movies']){
  await ready(activity.getByLabel('Refresh media'));await activity.getByLabel('Refresh media').selectOption(domain);
  await processing.getByText('This import attempt has unresolved evidence. Check status before another action; this does not establish a specific filesystem failure.',{exact:true}).waitFor();
  assert.equal(await processing.getByRole('button',{name:/^Resume import /}).count(),0);
  rows.set(domain,{...rows.get(domain),recovery_required:true});await read();
  await ready(processing.getByRole('button',{name:`Resume import ${operations[domain].id}`,exact:true}));
  await processing.getByText('The previous import attempt has no confirmed outcome. Check retained files and explicitly resume the same operation when ready.',{exact:true}).waitFor();
  rows.set(domain,{...rows.get(domain),resume_requested:true});await read();
  await processing.getByText('Resume requested; waiting for an import worker. The same operation will continue.',{exact:true}).waitFor();
  assert.equal(await processing.getByRole('button',{name:/^Resume import /}).count(),0);
  rows.set(domain,{...rows.get(domain),status:'imported',import_phase:'complete',resume_requested:false,path_failure_code:'source_denied_precommit'});await read();
  assert.equal(await processing.getByRole('button',{name:/^Resume import /}).count(),0);
  assert.equal(await processing.getByText(/denied access|unresolved evidence|previous import attempt/).count(),0);
 }
 assert.equal(writes.length,count,'Synthetic processing projections must never trigger an import/recovery write');
 await page.unroute(match);await read();
 assert.equal((await get(`/api/v1/download-processing?provider_id=${provider.id}&media_type=movies`)).total,0,'Projection overlays must not create durable processing receipts');
 overlays.push('Processing GET pages for both domains: active/abandoned NULL-error sentinel, pending resume, completed suppression; synthetic receipts never submitted');
}
try{
 await page.goto(origin);
 const owners={},operations={};
 for(const domain of ['tv','movies']){
  await nav.getByRole('button',{name:'Library',exact:true}).click();
  if(domain==='movies'){await manualRow(operations.tv.id).getByRole('button',{name:'Prepare another import',exact:true}).click();assert.equal(await manualRow(operations.tv.id).count(),0);}
  owners[domain]=await add(domain);await enableCdh(domain);
  if(domain==='tv')await page.getByRole('button',{name:'Import Pilot',exact:true}).click();
  const parent=join(scratch,`path-diagnostic-${domain}`),hidden=join(scratch,`path-diagnostic-${domain}-retained`);
  await mkdir(parent);
  const source=join(parent,'source.mkv'),destination=join(scratch,domain,'path-diagnostic.mkv');
  await copyFile(join(scratch,'incoming',domain==='tv'?'episode.mkv':'movie.mkv'),source);
  const original=await readFile(source),identity=await stat(source,{bigint:true});
  await manual.getByLabel('Source file',{exact:true}).fill(source);await manual.getByLabel('Destination file',{exact:true}).fill(destination);
  await manual.getByRole('button',{name:'Preview import',exact:true}).click();
  const saved=await receipt(),id=saved.operationId,row=manualRow(id);await ready(row.getByRole('button',{name:'Execute import',exact:true}));
  const preview=await get(`/api/v1/imports/${id}`);assert.deepEqual(preview.target,saved.request.target);assert.equal(preview.target.media_type,domain==='tv'?'episode':'movie');
  if(domain==='movies')assert.equal(preview.target.id,operations.tv.target.id,'Both domains exercise colliding numeric target IDs');
  const executePath=`/api/v1/imports/${id}/execute`;
  const before=writes.filter(([,path])=>path===executePath).length;
  // Rename only the source parent; source inode/content/ctime remain identical for recovery.
  await rename(parent,hidden);retainedParents.set(hidden,parent);
  const failedReply=page.waitForResponse(response=>new URL(response.url()).pathname===executePath&&response.request().method()==='POST');
  await row.getByRole('button',{name:'Execute import',exact:true}).click();assert.equal((await failedReply).status(),409);
  const failed=await until(()=>get(`/api/v1/imports/${id}`),value=>value.path_failure_code==='source_missing_precommit'&&value.recovery_required,'Failure did not retain a recoverable source-loss diagnostic');
  await manualRead();await ready(row.getByRole('button',{name:'Resume import',exact:true}));
  await row.getByText('An approved source was recorded missing before import completed. Restore or review the source before explicitly resuming the same operation.',{exact:true}).waitFor();
  assert.equal(await stat(destination).then(()=>true,error=>{assert.equal(error.code,'ENOENT');return false;}),false);
  await manualProjectionOverlays(failed);
  if(domain==='tv')await oldReceiptRefresh(failed);
  await healthRun(domain,'path_source_removed');
  if(domain==='tv')await screenshot('path-health-desktop');
  await rename(hidden,parent);retainedParents.delete(hidden);
  const restored=await stat(source,{bigint:true});for(const field of ['dev','ino','size','mtimeNs','ctimeNs'])assert.equal(restored[field],identity[field],`Source ${field} changed during parent restoration`);
  assert.deepEqual(await readFile(source),original);
  await healthRun(domain,'path_source_removed'); // Restored accessibility is not operation completion.
  await nav.getByRole('button',{name:'Library',exact:true}).click();await ready(row.getByRole('button',{name:'Resume import',exact:true}));
  if(domain==='tv'){
   await page.setViewportSize({width:390,height:844});await manual.scrollIntoViewIfNeeded();
   assert.equal(await manual.evaluate(node=>node.scrollWidth<=node.clientWidth+1),true,'Manual recovery panel overflows mobile width');
   await screenshot('path-import-mobile');await page.setViewportSize({width:1280,height:900});
  }
  const completedReply=page.waitForResponse(response=>new URL(response.url()).pathname===executePath&&response.request().method()==='POST');
  await row.getByRole('button',{name:'Resume import',exact:true}).click();assert.equal((await completedReply).status(),200);
  await ready(row.getByRole('button',{name:'Prepare another import',exact:true}));
  const complete=await get(`/api/v1/imports/${id}`);assert.equal(complete.id,id);assert.equal(complete.status,'complete');assert.equal(complete.path_failure_code,null);assert.equal(complete.recovery_required,false);
  assert.equal((await receipt(id)).operationId,id);assert.deepEqual(await readFile(destination),original);assert.deepEqual(await readFile(source),original);
  assert.equal(writes.filter(([,path])=>path===executePath).length-before,2,'Only explicit failure and explicit recovery may execute this operation');
  await completedOverlay(complete);operations[domain]=complete;
  await healthRun(domain,null);
 }
 // Retire the final completed browser identity before unrelated processing projections.
 await nav.getByRole('button',{name:'Library',exact:true}).click();
 await ready(manualRow(operations.movies.id).getByRole('button',{name:'Prepare another import',exact:true}));
 await manualRow(operations.movies.id).getByRole('button',{name:'Prepare another import',exact:true}).click();
 assert.equal(await manualRow(operations.movies.id).count(),0);
 await processingProjectionOverlays(owners,operations);
 assert.deepEqual(errors,[]);assert.deepEqual(external,[]);
 console.log('PASS actual TV/movie source-parent loss, retained manual and Health diagnosis, unchanged source identity/bytes, explicit same-operation recovery, malformed completed projection rejection, scoped Health recovery, mobile layout.');
 console.log('Screenshots: '+JSON.stringify(screenshots));
 console.log('Disclosed presentation overlays (not backend lease/recovery evidence): '+JSON.stringify(overlays));
}finally{
 try{for(const [hidden,parent] of retainedParents)await rename(hidden,parent);}
 finally{await browser.close();}
}
