// Owned fresh library_ui_fixture + Vite proxy only. Every scan runs through the real HTTP API and
// worker against the fixture's isolated scratch tree. One disclosed overlay: the 409 check replays
// an earlier REAL children response so the UI holds a stale resume generation; the 409 itself is
// produced by the real backend.
import assert from 'node:assert/strict';
import {existsSync, readFileSync} from 'node:fs';
import {join} from 'node:path';
import {pathToFileURL} from 'node:url';
const {chromium}=await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE||'/usr/lib/node_modules/playwright/index.mjs').href);
const origin=process.env.UI_URL,scratch=process.env.UI_SCRATCH,tvRoot=process.env.UI_TV_ROOT,movieRoot=process.env.UI_MOVIE_ROOT;
assert.ok(origin&&scratch&&tvRoot&&movieRoot);assert.equal(new URL(origin).hostname,'127.0.0.1');
const sources=join(scratch,'downloaded-scan');assert.ok(existsSync(sources),'Fixture must provide the isolated downloaded-scan tree');
const KEY='hrrdarr.downloaded-scan.pending.v1';
const browser=await chromium.launch({headless:true,...(process.env.PLAYWRIGHT_EXECUTABLE?{executablePath:process.env.PLAYWRIGHT_EXECUTABLE}:{})}),page=await browser.newPage();page.setDefaultTimeout(20000);
page.on('dialog',dialog=>dialog.accept());
const errors=[];page.on('pageerror',error=>errors.push(error.message));
const scanPosts=[],scanReads=[],resumeBodies=[];
page.on('request',request=>{
 const url=new URL(request.url());if(!url.pathname.includes('/downloaded-scan-commands'))return;
 if(request.method()==='POST'&&/\/downloaded-scan-commands$/.test(url.pathname))scanPosts.push({path:url.pathname,body:request.postDataJSON()});
 else if(request.method()==='POST'&&url.pathname.endsWith('/resume'))resumeBodies.push(request.postDataJSON());
 else if(request.method()==='GET')scanReads.push(url.pathname);
});
const nav=page.getByRole('navigation',{name:'Workspace'}),panel=page.getByRole('region',{name:'Scan downloaded files',exact:true});
const detail=panel.getByRole('article',{name:'Scan detail',exact:true}),pathInput=panel.getByLabel('Downloaded file or folder',{exact:true});
const scanButton=panel.getByRole('button',{name:'Scan and import',exact:true}),libraryDetail=page.getByRole('article',{name:'Library detail'});
async function api(method,path,data,status=200){const response=await page.request[method](`${origin}${path}`,data===undefined?{}:{data});assert.equal(response.status(),status,`${method} ${path}: ${await response.text()}`);return status===204?null:response.json();}
async function until(read,accept,label,timeoutMs=30000){const end=Date.now()+timeoutMs;let last;while(Date.now()<end){last=await read();if(accept(last))return last;await page.waitForTimeout(150);}assert.fail(`${label}: ${JSON.stringify(last)}`);}
const libraryPath=domain=>domain==='tv'?'/api/v1/tv/series':'/api/v1/movies';
async function libraryType(domain){await nav.getByRole('button',{name:'Library',exact:true}).click();await page.getByRole('navigation',{name:'Library type'}).getByRole('button',{name:domain==='tv'?'TV':'Movies',exact:true}).click();}
async function addLibrary(domain){
 await libraryType(domain);await page.getByRole('button',{name:domain==='tv'?'Add series':'Add movie',exact:true}).click();
 await page.getByLabel('Search catalogue',{exact:true}).fill('Fixture');await page.getByRole('button',{name:'Search',exact:true}).click();
 const title=domain==='tv'?'Fixture series':'Fixture movie';
 await page.getByRole('button',{name:new RegExp(`^${title} `)}).click();
 await page.getByLabel('Existing library directory',{exact:true}).fill(domain==='tv'?tvRoot:movieRoot);
 await page.getByRole('button',{name:'Add to library',exact:true}).click();await libraryDetail.getByRole('heading',{name:title,exact:true}).waitFor();
 const item=(await api('get',libraryPath(domain))).items.find(row=>row.title===title);assert.ok(item);
 // Scan decisions require a configured profile; WEBDL-1080p (catalog quality 3) is allowed.
 const profile=await api('post',`/api/v1/${domain}/quality-profiles`,{name:`Scan browser ${domain}`,items:[{kind:'quality',quality_id:3,allowed:true,min_size:0}],policy:{upgrade_allowed:false,cutoff:{kind:'quality',quality_id:3},min_format_score:0,cutoff_format_score:0,min_upgrade_format_score:1,language_id:domain==='movies'?-2:null,format_items:[]}},201);
 await api('put',`${libraryPath(domain)}/${item.id}`,{quality_profile_id:profile.id,...(domain==='tv'?{series_type:'standard',use_scene_numbering:false}:{minimum_availability:'released'})});
 return item;
}
async function setForm(path,mode='copy',priority='normal'){await pathInput.fill(path);await panel.getByLabel('Scan transfer mode',{exact:true}).selectOption(mode);await panel.getByLabel('Scan priority',{exact:true}).selectOption(priority);}
const summary=text=>detail.getByText(text,{exact:false}).first().waitFor();
const entry=name=>detail.getByRole('article',{name:`Entry ${name}`,exact:true});
const escape=text=>text.replace(/[.*+?^${}()|[\]\\]/g,'\\$&');
const pendingStored=()=>page.evaluate(key=>localStorage.getItem(key),KEY);
try {
 await page.goto(origin);
 // Loading and navigating never authorizes a scan.
 await panel.getByText('No TV scans yet.',{exact:true}).waitFor();
 await nav.getByRole('button',{name:'Activity',exact:true}).click();await nav.getByRole('button',{name:'Library',exact:true}).click();
 assert.equal(scanPosts.length,0);
 const movie=await addLibrary('movies');const series=await addLibrary('tv');

 // 1. TV success with in-place library refresh: copy keeps the source and the selected
 // series' episode row picks up the new file without a reload.
 await page.getByRole('complementary',{name:'Library collection'}).getByRole('button',{name:/^Fixture series/}).click();
 await libraryDetail.getByRole('button',{name:'Import Pilot',exact:true}).waitFor();
 assert.match(await panel.getByText('Copy leaves every downloaded file in place.',{exact:true}).textContent(),/Copy/);
 await panel.getByLabel('Scan transfer mode',{exact:true}).selectOption('auto');
 await panel.getByText('Auto moves files for downloads that are not tracked',{exact:false}).waitFor();
 const tvFolder=join(sources,'tv','Fixture.series.S01E01.1080p.WEB-DL'),tvFile='Fixture.series.S01E01.1080p.WEB-DL.flv';
 await setForm(tvFolder,'copy');await scanButton.click();
 await summary('Finished: 1 imported, 1 rejected, 0 already imported, 0 failed.');
 assert.equal(scanPosts.length,1);const tvScan=scanPosts[0];assert.equal(tvScan.path,'/api/v1/tv/downloaded-scan-commands');
 assert.deepEqual(Object.keys(tvScan.body).sort(),['id','mode','path','priority']);assert.match(tvScan.body.id,/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
 assert.deepEqual({...tvScan.body,id:undefined},{id:undefined,path:tvFolder,mode:'copy',priority:'normal'});
 const tvDestination=join(tvRoot,tvFile);
 await entry(tvFile).getByText(`${tvDestination} (copy)`,{exact:true}).waitFor();
 await entry(tvFile).getByText('complete',{exact:true}).waitFor();
 await entry('readme.nfo').getByText('unsupported_file_type',{exact:true}).waitFor();
 assert.ok(existsSync(tvDestination)&&existsSync(join(tvFolder,tvFile)),'Copy imports the file and keeps the source');
 assert.deepEqual(readFileSync(tvDestination),readFileSync(join(tvFolder,tvFile)));
 await libraryDetail.getByText(tvDestination,{exact:true}).waitFor();
 const tvParent=await api('get',`/api/v1/tv/downloaded-scan-commands/${tvScan.body.id}`);
 assert.deepEqual([tvParent.status,tvParent.outcome,tvParent.counts.imported],['succeeded','success',1]);
 assert.equal((await api('get',`/api/v1/movies/downloaded-scan-commands/${tvScan.body.id}`,undefined,404)).error.code,'downloaded_scan_not_found');
 assert.equal(await pendingStored(),null,'A confirmed admission leaves no pending identity');

 // 2. Domain change keeps an edited form until the user chooses explicitly.
 await pathInput.fill('/unsent/draft');await libraryType('movies');
 await panel.getByText('This scan form still targets TV.',{exact:false}).waitFor();assert.equal(await pathInput.inputValue(),'/unsent/draft');
 await panel.getByRole('button',{name:'Use Movies scans',exact:true}).click();
 await panel.getByRole('heading',{name:'New Movies scan',exact:true}).waitFor();assert.equal(await pathInput.inputValue(),'');

 // 3. Movies: the 202 is committed on the server but lost in the browser. The identity was
 // saved before sending; reload reconciles it by GET of the same ID and never POSTs again.
 const movieFolder=join(sources,'movies','Fixture.movie.2021.1080p.WEB-DL.English'),movieFile='Fixture.movie.2021.1080p.WEB-DL.English.flv';
 let capture;const held=new Promise(resolve=>capture=resolve);const pattern='**/api/v1/movies/downloaded-scan-commands';
 await page.route(pattern,route=>route.request().method()==='POST'?capture(route):route.continue());
 await setForm(movieFolder,'copy');await scanButton.click();
 const lost=await held;const lostBody=lost.request().postDataJSON();
 const saved=JSON.parse(await pendingStored());assert.deepEqual(saved,{version:1,entries:[{domain:'movies',input:lostBody}]},'Identity persisted before the POST left the browser');
 const committed=await lost.fetch();assert.equal(committed.status(),202);await lost.abort('failed');
 await panel.getByText('Admission outcome unknown.',{exact:false}).first().waitFor();
 await panel.getByRole('article',{name:`Unconfirmed scan ${movieFolder}`,exact:true}).waitFor();
 await page.unroute(pattern);
 const postsBeforeReload=scanPosts.length;assert.equal(postsBeforeReload,2);
 await page.reload();
 await until(pendingStored,value=>value===null,'Reload did not reconcile the retained identity');
 await panel.getByText('A retained scan request was found on the server.',{exact:true}).waitFor();
 assert.ok(scanReads.includes(`/api/v1/movies/downloaded-scan-commands/${lostBody.id}`),'Readback used the same ID');
 assert.equal(scanPosts.length,postsBeforeReload,'Reconciliation never re-sends the POST');
 await libraryType('movies');
 const movieRow=panel.getByRole('button',{name:new RegExp(`^${escape(movieFolder)}`)});
 await movieRow.focus();await page.keyboard.press('Enter');
 await summary('Finished: 1 imported, 0 rejected, 0 already imported, 0 failed.');
 await entry(movieFile).getByText(`${join(movieRoot,movieFile)} (copy)`,{exact:true}).waitFor();
 assert.ok(existsSync(join(movieRoot,movieFile)));
 await until(async()=>(await api('get',`/api/v1/movies/${movie.id}`)).statistics.file_count,count=>count===1,'Movie file was not associated');
 await page.getByRole('complementary',{name:'Library collection'}).getByText('1 files',{exact:true}).waitFor();

 // 4. Zero-import outcomes stay successful commands with visible reasons; keyboard submission.
 await libraryType('tv');await panel.getByRole('heading',{name:'New TV scan',exact:true}).waitFor();
 await pathInput.fill(join(sources,'unmatched'));await pathInput.press('Enter');
 await summary('Finished without importing anything (0 imported, 2 rejected');
 await entry('Unknown.Show.S01E01.1080p.WEB-DL.flv').getByText('no_library_match',{exact:true}).waitFor();
 await entry('notes.txt').getByText('unsupported_file_type',{exact:true}).waitFor();
 await pathInput.fill(join(sources,'does-not-exist'));await scanButton.click();
 await summary('Finished: the path does not exist. Nothing was imported.');
 await detail.getByText('The path was not found, so there are no entries.',{exact:true}).waitFor();
 await pathInput.fill(join(sources,'empty'));await scanButton.click();
 await summary('Finished without importing anything (0 imported, 0 rejected');
 await detail.getByText('This scan found no entries.',{exact:true}).waitFor();
 // Every visible parent is terminal: no further poll cycle is scheduled (5s interval).
 const readsWhenSettled=scanReads.length;await page.waitForTimeout(6500);
 assert.equal(scanReads.length,readsWhenSettled,'Polling continued after every visible scan settled');
 await pathInput.fill('relative/path');await scanButton.click();
 await panel.getByText('Use an absolute path that starts with /.',{exact:true}).waitFor();
 assert.equal(scanPosts.length,5,'Invalid input never reaches the network');

 // 5. Cancel a genuinely queued scan behind the fixture's stopped-worker barrier.
 await api('post','/api/fixture/stop-command-worker',undefined,204);
 const cancelPath=join(sources,'cancel');
 await setForm(cancelPath,'auto','high');await scanButton.click();
 await detail.getByText('queued · Queued',{exact:true}).waitFor();
 const cancelId=scanPosts.at(-1).body.id;
 await detail.getByRole('button',{name:'Stop scan after current file',exact:true}).click();
 await panel.getByText('Scan cancelled. Files already imported stay imported.',{exact:true}).waitFor();
 await summary('Cancelled: 0 imported');await detail.getByText('No entries were discovered.',{exact:true}).waitFor();
 assert.equal(await detail.getByRole('button',{name:'Stop scan after current file',exact:true}).count(),0);
 await api('post','/api/fixture/start-command-worker',undefined,204);
 await page.waitForTimeout(1500);
 const cancelled=await api('get',`/api/v1/tv/downloaded-scan-commands/${cancelId}`);
 assert.deepEqual([cancelled.status,cancelled.cancel_requested,cancelled.attempts,cancelled.counts.entries],['cancelled',true,0,0]);
 assert.ok(existsSync(join(cancelPath,'Fixture.series.S01E02.1080p.WEB-DL.flv')),'Auto never moved a cancelled scan source');
 assert.equal((await api('post',`/api/v1/tv/downloaded-scan-commands/${cancelId}/cancel`)).status,'cancelled','Cancel is idempotent');
 await panel.getByLabel('Scan priority',{exact:true}).selectOption('normal');await panel.getByLabel('Scan transfer mode',{exact:true}).selectOption('copy');

 // 6. Explicit same-child recovery. The fixture trigger fails the import-history write after
 // the transfer started, so the real engine leaves an unresolved operation.
 await api('post','/api/fixture/fail-import-history?enabled=1',undefined,204);
 const recoveryFolder=join(sources,'tv-recovery','Fixture.series.S01E02.1080p.WEB-DL'),recoveryFile='Fixture.series.S01E02.1080p.WEB-DL.flv';
 await setForm(recoveryFolder,'copy');await scanButton.click();
 await detail.getByText('An import was interrupted after it started changing files.',{exact:false}).waitFor();
 const recoveryId=scanPosts.at(-1).body.id,childrenPath=`/api/v1/tv/downloaded-scan-commands/${recoveryId}/children`;
 const resumeButton=entry(recoveryFile).getByRole('button',{name:`Resume import of ${recoveryFile}`,exact:true});
 await resumeButton.waitFor();
 const stale=await api('get',`${childrenPath}?limit=25&offset=0`);const failed=stale.items[0];
 assert.deepEqual([failed.disposition,failed.recovery_required,failed.resume_generation],['failed',true,0]);
 // Hold the UI on that earlier real read while another client consumes generation 0.
 const childPattern=url=>url.pathname===childrenPath;
 await page.route(childPattern,route=>route.fulfill({status:200,contentType:'application/json',body:JSON.stringify(stale)}));
 await api('post',`${childrenPath}/${failed.id}/resume`,{operation_id:failed.operation_id,inventory_revision:1,expected_resume_generation:0});
 await until(async()=>(await api('get',`${childrenPath}`)).items[0],c=>c.resume_generation===1&&c.recovery_required&&!c.resume_requested,'Second failure did not settle');
 await panel.getByRole('button',{name:'Refresh scans',exact:true}).click();
 await until(async()=>resumeButton.isEnabled(),Boolean,'Stale resume did not unlock');
 await resumeButton.click();
 await panel.getByText('This file changed since it was read; another recovery may already have been accepted.',{exact:false}).waitFor();
 assert.deepEqual(resumeBodies.at(-1),{operation_id:failed.operation_id,inventory_revision:1,expected_resume_generation:0});
 await page.unroute(childPattern);
 await api('post','/api/fixture/fail-import-history?enabled=0',undefined,204);
 await panel.getByRole('button',{name:'Refresh scans',exact:true}).click();
 await entry(recoveryFile).getByText('Required',{exact:true}).waitFor();
 await until(async()=>resumeButton.isEnabled(),Boolean,'Resume did not unlock after a fresh read');
 await resumeButton.click();
 await panel.getByText(`Recovery requested for ${recoveryFile}.`,{exact:false}).waitFor();
 assert.deepEqual(resumeBodies.at(-1),{operation_id:failed.operation_id,inventory_revision:1,expected_resume_generation:1});
 await summary('Finished: 1 imported, 0 rejected, 0 already imported, 0 failed.');
 await entry(recoveryFile).getByText('complete',{exact:true}).waitFor();
 assert.ok(existsSync(join(tvRoot,recoveryFile)));
 const episodes=await api('get',`/api/v1/episodes?series_id=${series.id}&limit=25&offset=0`);
 assert.deepEqual(episodes.items.filter(e=>e.has_file).map(e=>e.number).sort(),[1,2]);
 // page.request calls are not page traffic: only the two UI resumes were sent by the browser.
 assert.equal(resumeBodies.length,2,'Exactly the explicit UI resumes were sent');

 // 7. List rediscovery and mobile layout without storage: the parent list comes from the API.
 await page.evaluate(key=>localStorage.removeItem(key),KEY);await page.setViewportSize({width:390,height:844});await page.reload();
 await nav.getByRole('button',{name:'Library',exact:true}).click();
 await until(()=>panel.getByRole('listitem').filter({has:page.getByRole('button',{name:new RegExp(`^${escape(recoveryFolder)}`)})}).count(),n=>n===1,'Parent not rediscovered from the list');
 assert.equal(await panel.evaluate(el=>el.scrollWidth<=el.clientWidth),true,'Scan panel overflows on mobile');
 assert.equal(scanPosts.length,7,'One POST per explicit Scan and import click');
 assert.deepEqual(errors,[]);
 console.log('Downloaded-scan browser passed TV/movie real imports with library refresh, lost-202 same-ID readback without re-POST, zero-import/missing/empty outcomes, queued cancel, real 409 stale-generation and explicit same-child resume, list rediscovery and mobile layout.');
} finally {await browser.close();}
