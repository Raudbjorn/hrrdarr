// Owned library_ui_fixture only: real HTTP and real clicks; every claim is read back through the
// production collections and library APIs. The fixture metadata mock serves the graph for tmdb 7000.
import assert from 'node:assert/strict';
import {mkdir} from 'node:fs/promises';
import {isAbsolute,join} from 'node:path';
import {pathToFileURL} from 'node:url';
const {chromium}=await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE||'/usr/lib/node_modules/playwright/index.mjs').href);
const origin=process.env.UI_URL,fixture=JSON.parse(process.env.UI_FIXTURE_JSON||'{}');
assert.equal(new URL(origin).hostname,'127.0.0.1');
assert.equal(new URL(fixture.api).hostname,'127.0.0.1');
const artifacts=process.env.UI_SCREENSHOT_DIR;
assert.ok(artifacts&&isAbsolute(artifacts)&&/\.do-not-commit\/loop\/iteration114(?:[-/]|$)/.test(artifacts),'UI_SCREENSHOT_DIR must be an owned iteration114 artifact directory');
await mkdir(artifacts,{recursive:true});
const browser=await chromium.launch({headless:true,...(process.env.PLAYWRIGHT_EXECUTABLE?{executablePath:process.env.PLAYWRIGHT_EXECUTABLE}:{})});
const context=await browser.newContext({viewport:{width:1280,height:1000}});
const page=await context.newPage();page.setDefaultTimeout(20000);
const errors=[];page.on('pageerror',error=>errors.push(error.message));
page.on('dialog',dialog=>{errors.push(`Unexpected dialog: ${dialog.message()}`);void dialog.dismiss();});
async function api(method,path,data,status=200){
 const response=await page.request[method](origin+path,{...(data===undefined?{}:{data}),timeout:10000});
 assert.equal(response.status(),status,`${method} ${path}: ${await response.text()}`);return status===204?null:response.json();
}
async function until(read,accept,label,ms=20000){
 const deadline=Date.now()+ms;let last;
 while(Date.now()<deadline){last=await read();if(accept(last))return last;await page.waitForTimeout(150);}
 assert.fail(`${label}: ${JSON.stringify(last)}`);
}
try{
 const seed=await api('post','/api/fixture/collections-seed',{});
 const CID=seed.collection_id;
 const tag=await api('post','/api/v1/movies/tags',{label:'saga-added'},200).catch(async()=>api('post','/api/v1/movies/tags',{label:'saga-added'},201));
 const collection=()=>api('get',`/api/v1/movies/collections/${CID}`);
 const libraryMovies=async()=>{const all=[];for(let o=0;;o+=100){const p=await api('get',`/api/v1/movies?limit=100&offset=${o}`);all.push(...p.items);if(all.length>=p.total||p.items.length===0)return all;}};
 const inCollection=async()=>{const c=await collection();const ids=new Set(c.movies.map(m=>m.movie_id).filter(x=>x!==null));return (await libraryMovies()).filter(m=>ids.has(m.id));};

 // The worker may enqueue an automatic refresh right after seeding; let it settle so the manual run is unambiguous.
 await until(async()=>(await api('get','/api/v1/movies/collections/refresh-commands?limit=5')).items.length,n=>n>0,'automatic refresh enqueued',5000).catch(()=>{});
 await until(async()=>(await api('get','/api/v1/movies/collections/refresh-commands?limit=5')).items.some(c=>['queued','running','retry_wait'].includes(c.status)),active=>!active,'automatic refresh settles',30000);
 await page.goto(`${origin}/#/collections`);
 const region=page.getByRole('region',{name:'Collections',exact:true});
 await region.waitFor();
 const row=region.getByRole('list',{name:'Collection list'}).getByRole('listitem').filter({hasText:'Fixture Saga'});
 await row.waitFor();
 assert.match(await row.innerText(),/1 of \d in library/);
 assert.match(await row.innerText(),/No policy configured/);
 await page.screenshot({path:join(artifacts,'collections-list.png'),fullPage:true});

 // Monitoring before the policy is complete is rejected by the server and shown verbatim.
 await row.getByRole('checkbox',{name:'Monitor Fixture Saga',exact:true}).click();
 await region.getByRole('alert').filter({hasText:'requires a quality profile, root folder and minimum availability'}).waitFor();
 assert.equal((await collection()).monitored,false);
 assert.equal(await row.getByRole('checkbox',{name:'Monitor Fixture Saga',exact:true}).isChecked(),false);

 // Configure the policy through the detail editor.
 await row.getByRole('button',{name:/Details for Fixture Saga/}).click();
 const detail=region.getByRole('region',{name:'Collection details'});
 const form=detail.getByRole('form',{name:'Collection settings'});
 await form.waitFor();
 await form.getByLabel('Quality profile').selectOption({label:'Collection profile'});
 await form.getByLabel('Root folder').selectOption({label:seed.movie_root});
 await form.getByLabel('Minimum availability').selectOption('released');
 await form.getByLabel(/Search on add/).check();
 await form.getByLabel('saga-added').check();
 await form.getByRole('button',{name:'Save settings'}).click();
 await region.getByRole('status').filter({hasText:'Collection settings saved.'}).waitFor();
 const configured=await collection();
 assert.equal(configured.quality_profile_id,seed.quality_profile_id);
 assert.equal(configured.root_folder_path,seed.movie_root);
 assert.equal(configured.minimum_availability,'released');
 assert.equal(configured.search_on_add,true);
 assert.deepEqual(configured.tag_ids,[tag.id]);
 assert.equal(configured.monitored,false);
 await page.screenshot({path:join(artifacts,'collections-configured.png'),fullPage:true});

 // Refresh: command runs against the mock metadata service and the collection gains missing members.
 await region.getByRole('button',{name:'Refresh collections',exact:true}).click();
 await region.locator('[data-refresh-status="succeeded"]').waitFor();
 assert.match(await region.locator('[data-refresh-status]').innerText(),/Refresh succeeded: 1 collection checked, 0 failed, 0 movies added/);
 const refreshed=await until(collection,c=>c.movie_count===3,'collection members after refresh');
 assert.equal(refreshed.missing_movie_count,2);
 assert.equal(refreshed.library_movie_count,1);
 assert.ok(refreshed.last_info_sync);
 assert.deepEqual(refreshed.movies.filter(m=>m.movie_id===null).map(m=>m.title).sort(),['Fixture Saga Three','Fixture Saga Two']);
 assert.equal((await inCollection()).length,1,'unmonitored collection adds nothing to the library');
 const missingList=detail.getByRole('list',{name:'Missing movies'});
 await until(()=>missingList.getByRole('listitem').count(),n=>n===2,'two missing movies shown');
 assert.match(await missingList.innerText(),/Fixture Saga Two \(2003\)/);
 assert.doesNotMatch(await row.innerText(),/Never synced/);
 await page.screenshot({path:join(artifacts,'collections-refreshed.png'),fullPage:true});

 // Monitoring the configured collection adopts the missing members with the collection policy.
 await row.getByRole('checkbox',{name:'Monitor Fixture Saga',exact:true}).check();
 await region.getByRole('status').filter({hasText:'Fixture Saga monitored.'}).waitFor();
 const monitored=await until(collection,c=>c.monitored&&c.library_movie_count===3,'collection monitored with 3 library movies');
 assert.equal(monitored.missing_movie_count,0);
 const movies=await inCollection();
 assert.equal(movies.length,3);
 for(const m of movies.filter(m=>m.id!==seed.movie_id)){
  assert.ok(m.path.startsWith(seed.movie_root),`added movie path ${m.path}`);
  assert.equal(m.settings.quality_profile_id,seed.quality_profile_id);
  assert.deepEqual(m.tag_ids,[tag.id]);
  assert.equal(m.monitored,true);
 }
 await until(()=>detail.getByRole('list',{name:'Movies in library'}).getByRole('listitem').count(),n=>n===3,'three library members shown');
 await page.screenshot({path:join(artifacts,'collections-monitored.png'),fullPage:true});

 // One-shot unmonitor of library movies.
 await detail.getByRole('button',{name:'Unmonitor library movies',exact:true}).click();
 await region.getByRole('status').filter({hasText:'Library movies unmonitored.'}).waitFor();
 await until(async()=>(await inCollection()).every(m=>!m.monitored),ok=>ok,'library movies unmonitored');
 assert.equal((await collection()).monitored,true,'collection stays monitored');
 await detail.getByRole('button',{name:'Monitor library movies',exact:true}).click();
 await region.getByRole('status').filter({hasText:'Library movies monitored.'}).waitFor();
 await until(async()=>(await inCollection()).every(m=>m.monitored),ok=>ok,'library movies monitored again');
 await detail.getByRole('button',{name:'Unmonitor library movies',exact:true}).click();
 await until(async()=>(await inCollection()).every(m=>!m.monitored),ok=>ok,'library movies unmonitored (final)');

 // Bulk unmonitor of the collection.
 await region.getByRole('checkbox',{name:'Select Fixture Saga',exact:true}).check();
 await region.getByRole('button',{name:'Unmonitor selected',exact:true}).click();
 await until(async()=>(await collection()).monitored,m=>m===false,'bulk unmonitor');

 // Filter: monitored filter hides it, unmonitored shows it.
 await region.getByLabel('Show').selectOption('monitored');
 await region.getByText('No collections match this filter.').waitFor();
 await region.getByLabel('Show').selectOption('unmonitored');
 await row.waitFor();

 // Persistence across reload.
 await page.reload();
 await page.getByRole('region',{name:'Collections',exact:true}).waitFor();
 const row2=page.getByRole('list',{name:'Collection list'}).getByRole('listitem').filter({hasText:'Fixture Saga'});
 await row2.waitFor();
 assert.match(await row2.innerText(),/1 of 3 in library|3 of 3 in library/);
 await until(()=>row2.innerText(),t=>/Collection profile/.test(t),'profile name resolved after lookups load');
 assert.match(await row2.innerText(),/Released/);
 assert.match(await row2.innerText(),/search on add/);
 assert.equal(await row2.getByRole('checkbox',{name:'Monitor Fixture Saga',exact:true}).isChecked(),false);
 await page.screenshot({path:join(artifacts,'collections-reloaded.png'),fullPage:true});

 assert.deepEqual(errors,[]);
 console.log(JSON.stringify({collections_browser:'passed',collection:CID,library_movies:(await inCollection()).map(m=>m.id)}));
}finally{await browser.close();}
