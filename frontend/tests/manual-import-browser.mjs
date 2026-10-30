// Opt-in owned-fixture regression. Requires accepted101/102 integration and103 API/UI.
// Start library_ui_fixture with HRRDARR_UI_PAUSE_COMMANDS=1. This script starts its real
// worker once, after deterministic queued/cancel assertions. No later worker pause is used.
// Overlays drop actual accepted responses, return404 for a real cancelled command to
// exercise history pruning, or hold unchanged real GET responses to expose read races.
// File failures/recovery and every admitted command are real.
import assert from 'node:assert/strict';
import {constants} from 'node:fs';
import {copyFile, mkdir, readFile, realpath, rename, stat} from 'node:fs/promises';
import {basename, isAbsolute, join} from 'node:path';
import {pathToFileURL} from 'node:url';

const origin=process.env.UI_URL;
assert.ok(origin,'Set UI_URL to the owned Vite proxy');
assert.equal(new URL(origin).hostname,'127.0.0.1');
assert.ok(process.env.UI_SCRATCH && isAbsolute(process.env.UI_SCRATCH),'Set absolute UI_SCRATCH from the owned fixture');
const scratch=await realpath(process.env.UI_SCRATCH);
assert.ok(basename(scratch).startsWith('hrrdarr-library-ui-'),'Only an owned library_ui_fixture may be modified');
const artifacts=process.env.UI_SCREENSHOT_DIR;
if(artifacts){
  assert.ok(isAbsolute(artifacts) && artifacts.includes('/.do-not-commit/loop/iteration103'),'Use an owned ignored iteration103 screenshot directory');
  await mkdir(artifacts,{recursive:true});
}
const {chromium}=await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE||'/usr/lib/node_modules/playwright/index.mjs').href);
const browser=await chromium.launch({headless:true,...(process.env.PLAYWRIGHT_EXECUTABLE?{executablePath:process.env.PLAYWRIGHT_EXECUTABLE}:{})});
const page=await browser.newPage({viewport:{width:1280,height:900}});
page.setDefaultTimeout(20000);
const errors=[],external=[],requests=[],overlays=[],retainedParents=new Map(),heldResponses=new Set();
let allowUnload=false, expectedConfirm=false;
page.on('pageerror',error=>errors.push(error.message));
page.on('dialog',dialog=>{
  if(dialog.type()==='beforeunload' && allowUnload){void dialog.accept();return;}
  if(dialog.type()==='confirm' && expectedConfirm && /Discard unsaved file metadata changes/.test(dialog.message())){expectedConfirm=false;void dialog.accept();return;}
  errors.push(`Unexpected dialog: ${dialog.type()} ${dialog.message()}`);void dialog.dismiss();
});
page.on('request',request=>requests.push({method:request.method(),path:new URL(request.url()).pathname,body:request.method()==='POST'||request.method()==='PUT'?request.postDataJSON():null}));
await page.route('**/*',route=>{
  const url=new URL(route.request().url());
  if(['http:','https:'].includes(url.protocol) && url.hostname!=='127.0.0.1'){external.push(url.href);return route.abort();}
  return route.continue();
});
const COMMANDS='/api/v1/manual-import/commands', RECONCILE=`${COMMANDS}/reconcile`;
const KEY='hrrdarr.import.collection.v2';
const workspace=page.getByRole('navigation',{name:'Workspace'});
const domains=page.getByRole('navigation',{name:'Library type'});
const manual=page.getByRole('region',{name:'Manual import',exact:true});
const files=page.getByRole('region',{name:'File metadata',exact:true});
const fileForm=files.getByRole('form',{name:'Edit file metadata',exact:true});
const detail=page.getByRole('article',{name:'Library detail',exact:true});
const row=id=>manual.getByRole('article').filter({has:page.getByText(id,{exact:true})});
const receipt=()=>page.evaluate(key=>JSON.parse(localStorage.getItem(key)),KEY);
const writes=()=>requests.filter(r=>r.method==='POST' && (r.path==='/api/v1/imports'||r.path===COMMANDS||/^\/api\/v1\/imports\/[^/]+\/execute$/.test(r.path)||/^\/api\/v1\/manual-import\/commands\/[^/]+\/cancel$/.test(r.path)));
const countWrites=path=>writes().filter(r=>r.path===path).length;
async function holdOnce(match){
  let release;const gate=new Promise(resolve=>release=resolve);
  const state={body:null,delivered:false,release:()=>{release();heldResponses.delete(release);}};
  heldResponses.add(release);let claimed=false;
  const handler=async route=>{
    if(route.request().method()!=='GET'||claimed)return route.continue();
    claimed=true;const response=await route.fetch();assert.equal(response.status(),200,await response.text());
    state.body=await response.json();await gate;await route.fulfill({response});state.delivered=true;
  };
  await page.route(match,handler);
  return {...state,get body(){return state.body;},get delivered(){return state.delivered;},close:async()=>{state.release();await page.unroute(match,handler);}};
}
async function get(path){const response=await page.request.get(origin+path,{timeout:10000});assert.equal(response.status(),200,await response.text());return response.json();}
async function until(read,predicate,label,timeout=30000){
  const deadline=Date.now()+timeout;let last;
  while(Date.now()<deadline){last=await read();if(predicate(last))return last;await page.waitForTimeout(75);}
  assert.fail(`${label}: ${JSON.stringify(last)}`);
}
async function ready(control){await control.waitFor();await until(()=>control.isEnabled(),Boolean,'Control did not become enabled');}
async function check(){const button=manual.getByRole('button',{name:'Check status',exact:true});await ready(button);await button.click();await ready(button);}
async function reload(){allowUnload=true;try{await page.reload();await manual.waitFor();}finally{allowUnload=false;}}
async function choose(domain,title,episodeTitle=null){
  await workspace.getByRole('button',{name:'Library',exact:true}).click();
  await domains.getByRole('button',{name:domain==='tv'?'TV':'Movies',exact:true}).click();
  await page.getByRole('complementary',{name:'Library collection'}).getByRole('button',{name:new RegExp(`^${title}`)}).click();
  await detail.getByRole('heading',{name:title,exact:true}).waitFor();
  if(episodeTitle)await detail.getByRole('button',{name:`Import ${episodeTitle}`,exact:true}).click();
}
async function add(domain,title,directory,query='Fixture'){
  await workspace.getByRole('button',{name:'Library',exact:true}).click();
  await domains.getByRole('button',{name:domain==='tv'?'TV':'Movies',exact:true}).click();
  await page.getByRole('button',{name:domain==='tv'?'Add series':'Add movie',exact:true}).click();
  await page.getByLabel('Search catalogue',{exact:true}).fill(query);
  await page.getByRole('button',{name:'Search',exact:true}).click();
  await page.getByRole('button',{name:new RegExp(`^${title} `)}).click();
  await page.getByLabel('Existing library directory',{exact:true}).fill(directory);
  await page.getByRole('button',{name:'Add to library',exact:true}).click();
  await detail.getByRole('heading',{name:title,exact:true}).waitFor();
  const collection=await get(domain==='tv'?'/api/v1/tv/series':'/api/v1/movies');
  const item=collection.items.find(item=>item.title===title);assert.ok(item);return item;
}
async function useCurrentTarget(label){
  const change=manual.getByRole('button',{name:new RegExp(`^Discard form and use ${label.replace(/[.*+?^${}()|[\]\\]/g,'\\$&')}$`)});
  await until(async()=>({change:await change.count(),text:await manual.locator('form').innerText()}),value=>value.change>0||value.text.includes(`Selected target: ${label} (`),'Current target did not reach import form');
  if(await change.count())await change.click();
  await manual.locator('form').getByText(new RegExp(`^Selected target: ${label.replace(/[.*+?^${}()|[\]\\]/g,'\\$&')} \\(`)).waitFor();
}
async function preview(source,destination,target){
  await manual.getByLabel('Source file',{exact:true}).fill(source);
  await manual.getByLabel('Destination file',{exact:true}).fill(destination);
  const reply=page.waitForResponse(r=>new URL(r.url()).pathname==='/api/v1/imports'&&r.request().method()==='POST');
  await ready(manual.getByRole('button',{name:'Preview import',exact:true}));
  await manual.getByRole('button',{name:'Preview import',exact:true}).click();
  const response=await reply;assert.equal(response.status(),202,await response.text());const operation=await response.json();
  assert.deepEqual(operation.target,target);
  const entry=await until(receipt,value=>value?.entries.some(e=>e.operationId===operation.id),'Preview identity was not persisted').then(value=>value.entries.find(e=>e.operationId===operation.id));
  assert.deepEqual(entry.request,{target,source,destination,mode:'copy'});assert.equal(entry.authorized,false);
  await ready(row(operation.id).getByRole('button',{name:'Execute import',exact:true}));
  return entry;
}
async function queuedSelection(expectedIds,priority='normal'){
  await manual.getByRole('combobox',{name:/^Import priority/}).selectOption(priority);
  const reply=page.waitForResponse(r=>new URL(r.url()).pathname===COMMANDS&&r.request().method()==='POST');
  const button=manual.getByRole('button',{name:'Queue selected imports',exact:true});await ready(button);await button.click();
  const response=await reply;assert.equal(response.status(),202,await response.text());
  assert.deepEqual(response.request().postDataJSON(),{operation_ids:expectedIds,priority});
  const commands=await response.json();assert.deepEqual(commands.map(c=>c.operation_id),expectedIds);
  await until(receipt,value=>commands.every(c=>value?.entries.find(e=>e.operationId===c.operation_id)?.commands.some(known=>known.id===c.id)),'Accepted command references were not persisted');
  assert.equal(new Set(commands.map(c=>c.batch_id)).size,1);assert.ok(commands.every(c=>c.status==='queued'&&c.priority===priority));
  return commands;
}
async function selectOnly(ids){
  for(const e of (await receipt()).entries){
    const checkbox=row(e.operationId).getByRole('checkbox');
    if(await checkbox.isChecked() && !ids.includes(e.operationId)){await ready(checkbox);await checkbox.uncheck();}
    else if(!(await checkbox.isChecked()) && ids.includes(e.operationId)){await ready(checkbox);await checkbox.check();}
  }
}
async function absent(path){await assert.rejects(stat(path),error=>error.code==='ENOENT');}
async function waitComplete(id){return until(()=>get(`/api/v1/imports/${id}`),op=>op.status==='complete',`Operation ${id} did not complete`,90000);}
async function assertMedia(entry,bytes){
  assert.deepEqual(await readFile(entry.request.destination),bytes);assert.deepEqual(await readFile(entry.request.source),bytes);
  const scope=entry.request.target.media_type==='movie'?'movies':'tv';
  const path=scope==='movies'?`/api/v1/movies/files?movie_ids=${entry.request.target.id}`:`/api/v1/tv/files?series_id=${entry.owner.id}`;
  const matching=(await get(path)).items.filter(file=>file.path===entry.request.destination);assert.equal(matching.length,1);
  if(scope==='tv'){const episodes=await get(`/api/v1/episodes?series_id=${entry.owner.id}`);assert.equal(episodes.items.find(e=>e.id===entry.request.target.id).episode_file_id,matching[0].id);}
  return matching[0];
}
async function beginMetadata(fileId,value){
  await ready(files.getByRole('button',{name:`Edit file ${fileId}`,exact:true}));
  await files.getByRole('button',{name:`Edit file ${fileId}`,exact:true}).click();
  await fileForm.getByLabel('Release group action',{exact:true}).selectOption('set');
  await fileForm.getByLabel('Release group',{exact:true}).fill(value);
}
async function closeMetadata(){expectedConfirm=true;await fileForm.getByRole('button',{name:'Close metadata edit',exact:true}).click();assert.equal(expectedConfirm,false);await until(()=>fileForm.count(),n=>n===0,'Metadata draft did not close');}
async function prepareFailure(domain,target,owner,label){
  const parent=join(scratch,'incoming',`manual-batch-${domain}-source`),hidden=parent+'.retained';
  await mkdir(parent);const source=join(parent,'source.mkv');
  await copyFile(join(scratch,'incoming',domain==='tv'?'episode.mkv':'movie.mkv'),source,constants.COPYFILE_EXCL);
  const identity=await stat(source,{bigint:true}),bytes=await readFile(source);
  assert.ok((await realpath(parent)).startsWith(scratch+'/'));
  const destination=join(owner.path,`manual-batch-${domain}-recovered.mkv`);
  await useCurrentTarget(label);const entry=await preview(source,destination,target);await selectOnly([entry.operationId]);
  await rename(parent,hidden);retainedParents.set(hidden,parent);
  const before=countWrites(COMMANDS),executePath=`/api/v1/imports/${entry.operationId}/execute`,beforeExecute=countWrites(executePath);
  let accepted=null;
  const match=url=>url.pathname===COMMANDS;
  await page.route(match,async route=>{
    assert.equal(route.request().method(),'POST');assert.deepEqual(route.request().postDataJSON().operation_ids,[entry.operationId]);
    const response=await route.fetch();assert.equal(response.status(),202,await response.text());accepted=await response.json();await route.abort('failed');
  });
  try{
    await ready(manual.getByRole('button',{name:'Queue selected imports',exact:true}));
    await manual.getByRole('button',{name:'Queue selected imports',exact:true}).click();
    await until(()=>Promise.resolve(accepted),Boolean,'Lost admission was never accepted');
    await manual.getByRole('alert').filter({hasText:'Admission outcome is unknown'}).waitFor();
  }finally{await page.unroute(match);}
  overlays.push(`${domain}: actual202 response dropped; real source-parent loss, no command/operation status fabricated`);
  const failed=await until(()=>get(`/api/v1/imports/${entry.operationId}`),op=>op.recovery_required===true&&op.path_failure_code==='source_missing_precommit',`${domain} real source failure not retained`,90000);
  assert.equal(failed.target.media_type,target.media_type);
  const wrapper=await until(()=>get(`${COMMANDS}/${accepted[0].id}`),c=>c.status==='failed',`${domain} wrapper did not settle failed`,90000);
  assert.equal(wrapper.operation_id,entry.operationId);
  await check();await ready(row(entry.operationId).getByRole('button',{name:'Resume import',exact:true}));
  await row(entry.operationId).getByText(/An approved source was recorded missing/).waitFor();
  const retained=(await receipt()).entries.find(e=>e.operationId===entry.operationId);assert.equal(retained.admissionUncertain,true);
  await absent(destination);assert.equal(countWrites(COMMANDS),before+1);assert.equal(countWrites(executePath),beforeExecute);
  return {entry,parent,hidden,identity,bytes,wrapper,executePath,beforeExecute};
}
async function restoreFailure(failure){
  await rename(failure.hidden,failure.parent);retainedParents.delete(failure.hidden);
  const restored=await stat(failure.entry.request.source,{bigint:true});
  for(const field of ['dev','ino','size','mtimeNs','ctimeNs'])assert.equal(restored[field],failure.identity[field],`Restored source ${field} changed`);
  assert.deepEqual(await readFile(failure.entry.request.source),failure.bytes);
}
async function resumeFailure(failure){
  const control=row(failure.entry.operationId).getByRole('button',{name:'Resume import',exact:true});await ready(control);
  const reply=page.waitForResponse(r=>new URL(r.url()).pathname===failure.executePath&&r.request().method()==='POST');
  await control.click();const response=await reply;assert.equal(response.status(),200,await response.text());
  const complete=await waitComplete(failure.entry.operationId);assert.equal(complete.path_failure_code,null);assert.equal(complete.recovery_required,false);
  await ready(row(failure.entry.operationId).getByRole('button',{name:'Prepare another import',exact:true}));
  assert.equal(countWrites(failure.executePath),failure.beforeExecute+1,'Only explicit Resume executes the failed same UUID');
  const retained=(await receipt()).entries.find(e=>e.operationId===failure.entry.operationId);
  assert.equal(retained.admissionUncertain,true,'Completion must not pretend the lost HTTP admission response was recovered');
  assert.equal(retained.executionUncertain,true,'Direct execution provenance is retained until explicit removal');
  await assertMedia(failure.entry,failure.bytes);
  assert.equal((await get(`${COMMANDS}/${failure.wrapper.id}`)).status,'failed','Explicit recovery must not rewrite historical wrapper failure');
}

try{
  await page.goto(origin);
  assert.equal(await page.evaluate(()=>localStorage.getItem('hrrdarr.import.receipt.v1')),null,'Use a fresh browser context');
  const tv=await add('tv','Fixture series',join(scratch,'tv'));
  const episodes=(await get(`/api/v1/episodes?series_id=${tv.id}`)).items;assert.equal(episodes.length,2,'Use fresh metadata mode0');
  await detail.getByRole('button',{name:'Import Pilot',exact:true}).click();
  await useCurrentTarget('Fixture series: Pilot');
  const tvSource=join(scratch,'incoming/episode.mkv'),tvDestination=join(scratch,'tv/manual-batch-pilot.mkv');
  await manual.getByLabel('Source file',{exact:true}).fill(tvSource);
  await manual.getByLabel('Destination file',{exact:true}).fill(tvDestination);
  const movie=await add('movies','Fixture movie',join(scratch,'movies'));
  await manual.getByText('The selected library target changed. This form still belongs to Fixture series: Pilot.',{exact:true}).waitFor();
  await manual.getByRole('button',{name:'Keep current draft',exact:true}).click();
  assert.equal(await manual.getByLabel('Source file',{exact:true}).inputValue(),tvSource);
  const tvEntry=await preview(tvSource,tvDestination,{media_type:'episode',id:episodes[0].id});
  assert.deepEqual(tvEntry.owner,{domain:'tv',id:tv.id});
  await useCurrentTarget('Fixture movie');
  const movieEntry=await preview(join(scratch,'incoming/movie.mkv'),join(scratch,'movies/manual-batch-movie.mkv'),{media_type:'movie',id:movie.id});
  assert.deepEqual(movieEntry.owner,{domain:'movies',id:movie.id});
  assert.equal(tvEntry.request.target.id,movieEntry.request.target.id,'Fresh typed target IDs intentionally collide');
  const originalTv=await readFile(tvSource),originalMovie=await readFile(movieEntry.request.source);
  await absent(tvDestination);await absent(movieEntry.request.destination);
  const beforeNav=writes().length;
  await workspace.getByRole('button',{name:'Health',exact:true}).click();
  await workspace.getByRole('button',{name:'Library',exact:true}).click();
  assert.equal((await receipt()).entries.length,2);assert.equal(writes().length,beforeNav,'Workspace navigation never authorizes imports');

  const batch=await queuedSelection([tvEntry.operationId,movieEntry.operationId],'high');
  const movieCommand=batch.find(c=>c.operation_id===movieEntry.operationId);
  for(const c of batch)assert.equal((await get(`${COMMANDS}/${c.id}`)).status,'queued','Fixture must start with HRRDARR_UI_PAUSE_COMMANDS=1');
  await absent(tvDestination);await absent(movieEntry.request.destination);
  await check();
  let cancelAccepted=false;
  const cancelMatch=url=>url.pathname===`${COMMANDS}/${movieCommand.id}/cancel`;
  const prunedMatch=url=>url.pathname===`${COMMANDS}/${movieCommand.id}`;
  await page.route(prunedMatch,route=>{
    if(!cancelAccepted)return route.continue();
    assert.equal(route.request().method(),'GET');
    return route.fulfill({status:404,contentType:'application/json',body:JSON.stringify({error:{code:'manual_import_command_not_found',message:'Manual import command not found'}})});
  });
  await page.route(cancelMatch,async route=>{
    const response=await route.fetch();assert.equal(response.status(),200,await response.text());assert.equal((await response.json()).status,'cancelled');cancelAccepted=true;await route.abort('failed');
  });
  try{
    const cancel=row(movieEntry.operationId).getByRole('button',{name:'Cancel queued command',exact:true});await ready(cancel);await cancel.click();
    await until(()=>Promise.resolve(cancelAccepted),Boolean,'Actual cancellation was not accepted');
    assert.equal((await get(`${COMMANDS}/${movieCommand.id}`)).status,'cancelled');
    assert.equal((await get(`/api/v1/imports/${movieEntry.operationId}`)).status,'preview');
    await check();
    await row(movieEntry.operationId).getByText('Command history is no longer available. This does not prove the import did not run.',{exact:true}).waitFor();
    const stored=(await receipt()).entries.find(e=>e.operationId===movieEntry.operationId);
    assert.deepEqual(stored.cancelUncertainIds,[movieCommand.id]);assert.equal(stored.admissionUncertain,false);
    const selected=row(movieEntry.operationId).getByRole('checkbox');await ready(selected);await selected.check();
    const repeated=await queuedSelection([movieEntry.operationId]);
    assert.notEqual(repeated[0].id,movieCommand.id);assert.notEqual(repeated[0].batch_id,movieCommand.batch_id);
    assert.equal(repeated[0].operation_id,movieEntry.operationId,'Re-admission uses the SAME preview UUID');
    assert.deepEqual((await receipt()).entries.find(e=>e.operationId===movieEntry.operationId).cancelUncertainIds,[movieCommand.id]);
    overlays.push('Actual cancel200 response dropped; GET404 overlays only that real cancelled command to model pruned history; real reconciliation and new admission unchanged');
  }finally{await page.unroute(cancelMatch);await page.unroute(prunedMatch);}
  const beforeReload=writes().length;await reload();await check();
  assert.equal((await receipt()).entries.length,2);assert.equal(writes().length,beforeReload,'Reload/readback must not POST admission or execute');
  assert.equal(await manual.getByRole('button',{name:'Resume import',exact:true}).count(),0,'Active wrappers own queued operations');
  await absent(tvDestination);await absent(movieEntry.request.destination);
  // Hold an actual pre-completion selection response; completion arrives while selected is null.
  // Releasing the stale detail must still lead to an owner-scoped post-import statistics refresh.
  const selectionGate=await holdOnce(url=>url.pathname===`/api/v1/tv/series/${tv.id}`);
  try{
    await domains.getByRole('button',{name:'TV',exact:true}).click();
    await page.getByRole('complementary',{name:'Library collection'}).getByRole('button',{name:/^Fixture series/}).click();
    await until(()=>Promise.resolve(selectionGate.body),Boolean,'Selection detail did not enter response gate');
    assert.equal(selectionGate.body.statistics.file_count,0,'Held selection response must predate import');
    const started=await page.request.post(origin+'/api/fixture/start-command-worker',{timeout:10000});
    assert.equal(started.status(),204,'Fixture must provide the opt-in start-command-worker endpoint');
    assert.equal((await page.request.post(origin+'/api/fixture/start-command-worker')).status(),409,'Worker starts only once');
    const beforeHidden=writes().length;
    await workspace.getByRole('button',{name:'Health',exact:true}).click();
    await waitComplete(tvEntry.operationId);await waitComplete(movieEntry.operationId);
    assert.equal(writes().length,beforeHidden,'Hidden completion does not cause browser authorization');
    await workspace.getByRole('button',{name:'Library',exact:true}).click();await check();
    selectionGate.release();
    await detail.getByRole('heading',{name:'Fixture series',exact:true}).waitFor();
    await detail.getByText(/^1 associated files · /).waitFor();
    overlays.push('Actual pre-import TV detail GET held across real completion and then delivered; refreshed owner statistics required');
  }finally{await selectionGate.close();}
  const tvFile=await assertMedia(tvEntry,originalTv);await assertMedia(movieEntry,originalMovie);
  await ready(row(tvEntry.operationId).getByRole('button',{name:'Prepare another import',exact:true}));
  await ready(row(movieEntry.operationId).getByRole('button',{name:'Prepare another import',exact:true}));
  assert.equal(countWrites(`/api/v1/imports/${tvEntry.operationId}/execute`),0,'Worker execution is not a browser direct Execute');

  // Real TV source-parent loss. Reload while failed, then preserve a same-owner file draft on Resume.
  await choose('tv','Fixture series','Second episode');
  const tvFailure=await prepareFailure('tv',{media_type:'episode',id:episodes[1].id},tv,'Fixture series: Second episode');
  const beforeFailedReload=writes().length;await reload();await check();
  assert.equal(writes().length,beforeFailedReload,'Failed receipt reload cannot resume or re-admit automatically');
  await choose('tv','Fixture series');
  await beginMetadata(tvFile.id,'unsaved same-owner import draft');
  const originalGroup=(await get(`/api/v1/tv/files/${tvFile.id}`)).release_group;
  await restoreFailure(tvFailure);await check();
  // Hold a real import-refresh episode list, then perform a newer successful monitor mutation.
  const episodeGate=await holdOnce(url=>url.pathname==='/api/v1/episodes'&&url.searchParams.get('series_id')===String(tv.id));
  try{
    await resumeFailure(tvFailure);
    await until(()=>Promise.resolve(episodeGate.body),Boolean,'Import refresh episode list did not enter response gate');
    const oldMonitor=episodeGate.body.items.find(e=>e.id===episodes[0].id).monitored;
    const control=detail.getByRole('button',{name:`${oldMonitor?'Unmonitor':'Monitor'} Pilot`,exact:true});await ready(control);
    const changed=page.waitForResponse(response=>response.request().method()==='PUT'&&new URL(response.url()).pathname===`/api/v1/episodes/${episodes[0].id}`);
    await control.click();assert.equal((await changed).status(),200);
    const refreshedEpisodes=page.waitForResponse(async response=>{
      const url=new URL(response.url());
      if(response.request().method()!=='GET'||url.pathname!=='/api/v1/episodes'||url.searchParams.get('series_id')!==String(tv.id)||response.status()!==200)return false;
      return (await response.json()).items.find(e=>e.id===episodes[0].id)?.monitored===!oldMonitor;
    });
    episodeGate.release();
    await until(()=>Promise.resolve(episodeGate.delivered),Boolean,'Old import refresh did not leave gate');
    await refreshedEpisodes;
    await ready(detail.getByRole('button',{name:`${oldMonitor?'Monitor':'Unmonitor'} Pilot`,exact:true}));
    assert.equal((await get(`/api/v1/episodes?series_id=${tv.id}`)).items.find(e=>e.id===episodes[0].id).monitored,!oldMonitor);
    await check();
    assert.equal(await detail.getByRole('button',{name:`${oldMonitor?'Monitor':'Unmonitor'} Pilot`,exact:true}).count(),1,'Stale import-refresh episodes must not overwrite the newer monitor result');
    overlays.push('Actual import-refresh episodes GET held across a real successful monitor PUT, then delivered unchanged; newer monitoring must survive');
  }finally{await episodeGate.close();}
  assert.equal(await fileForm.getByLabel('Release group',{exact:true}).inputValue(),'unsaved same-owner import draft');
  await files.getByText('An import completed. Your retained edit is unchanged; the file listing will refresh when the edit and any pending write are resolved.',{exact:true}).waitFor();
  assert.equal((await get(`/api/v1/tv/files/${tvFile.id}`)).release_group,originalGroup);
  await closeMetadata();
  await until(()=>files.getByRole('button',{name:/^Edit file \d+$/}).count(),count=>count===2,'Deferred file listing did not refresh after closing draft');

  // Real movie failure/recovery while an unrelated TV file draft is visible.
  const movieRoot=join(scratch,'search-movies-202');await mkdir(movieRoot,{recursive:true});
  const otherMovie=await add('movies','Search movie 202',movieRoot,'Search 202');
  const movieFailure=await prepareFailure('movies',{media_type:'movie',id:otherMovie.id},otherMovie,'Search movie 202');
  assert.equal(movieFailure.entry.request.target.id,tvFailure.entry.request.target.id,'Failed operation targets also collide across typed domains');
  await restoreFailure(movieFailure);
  await choose('tv','Fixture series');await beginMetadata(tvFile.id,'retained unrelated-owner draft');
  const tvReads=requests.filter(r=>r.method==='GET'&&r.path==='/api/v1/tv/files').length;
  await check();await resumeFailure(movieFailure);
  await detail.getByRole('heading',{name:'Fixture series',exact:true}).waitFor();
  assert.equal(await fileForm.getByLabel('Release group',{exact:true}).inputValue(),'retained unrelated-owner draft');
  assert.equal(requests.filter(r=>r.method==='GET'&&r.path==='/api/v1/tv/files').length,tvReads,'Movie completion must not invalidate unrelated TV file listing');
  assert.equal((await get(`/api/v1/tv/files/${tvFile.id}`)).release_group,originalGroup);
  assert.equal(requests.filter(r=>r.method==='PUT'&&/^\/api\/v1\/(tv|movies)\/files\//.test(r.path)).length,0,'Import completion cannot save an unsaved file draft');
  if(artifacts)await page.screenshot({path:join(artifacts,'manual-import-desktop.png'),fullPage:true});
  if(artifacts){await row(tvEntry.operationId).evaluate(node=>node.scrollIntoView({block:'start'}));await page.screenshot({path:join(artifacts,'manual-import-desktop-viewport.png')});}
  await page.setViewportSize({width:390,height:844});await manual.scrollIntoViewIfNeeded();
  assert.equal(await manual.evaluate(node=>node.scrollWidth<=node.clientWidth+1),true,'Collection overflows mobile width');
  if(artifacts)await page.screenshot({path:join(artifacts,'manual-import-mobile.png'),fullPage:true});
  if(artifacts){await row(tvEntry.operationId).evaluate(node=>node.scrollIntoView({block:'start'}));await page.screenshot({path:join(artifacts,'manual-import-mobile-viewport.png')});}
  await page.setViewportSize({width:1280,height:900});await closeMetadata();

  const beforeFinal=writes().length;await check();
  assert.equal(writes().length,beforeFinal,'Explicit status checks are read-only even though reconciliation uses POST');
  assert.equal((await receipt()).entries.length,4);
  assert.equal(countWrites('/api/v1/imports'),4,'Recovery/re-admission must never create replacement preview UUIDs');
  assert.equal(countWrites(COMMANDS),4,'Exactly initial batch, explicit re-admission, and two explicit lost-response admissions');
  assert.deepEqual(errors,[]);assert.deepEqual(external,[]);
  console.log('PASS real mixed-domain preview/navigation, queued batch/cancel/re-admission, reload without autoPOST, actual worker completion, both-domain source identity failure/restoration and explicit sameUUID recovery, scoped file-draft retention and mobile layout.');
  console.log('Disclosed controlled overlays: '+JSON.stringify(overlays));
  console.log('Not claimed: backend process/database reopen, real history pruning, active-lease projection overlays, storage quota/cross-tab faults, or full100-row browser scaling.');
}finally{
  try{for(const release of heldResponses)release();for(const [hidden,parent]of retainedParents)await rename(hidden,parent);}
  finally{await browser.close();}
}
